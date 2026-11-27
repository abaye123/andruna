//! `futex` on `WaitOnAddress` / `WakeByAddress*`.
//!
//! Supported: `FUTEX_WAIT`, `FUTEX_WAKE`, and `FUTEX_WAIT_BITSET` /
//! `FUTEX_WAKE_BITSET` with `FUTEX_BITSET_MATCH_ANY` (what bionic uses for
//! absolute timeouts), private or not, with `FUTEX_CLOCK_REALTIME`.
//!
//! `WaitOnAddress` compares and sleeps atomically with respect to
//! `WakeByAddress*`, which gives the Linux guarantee that a waiter that saw
//! the expected value cannot miss a wake that follows a store. Windows does
//! not report how many threads a wake reached, so lx counts waiters per
//! address and returns `min(val, waiters)`; a waiter that is just leaving
//! (timed out or woken) can still be counted.
//!
//! # Deviations
//!
//! - Non-private futexes work within the process only. Shared memory
//!   mappings do not exist yet; cross-process futexes on them need a
//!   different mechanism (TODO with shared mappings).
//! - An unmapped `uaddr` faults instead of returning `EFAULT`.
//! - Waits time out with millisecond granularity (rounded up). A queued
//!   signal makes a wait return `EINTR` ([`crate::signal`]), also with
//!   `SA_RESTART` (callers in bionic retry).
//! - Bitsets other than `FUTEX_BITSET_MATCH_ANY`, requeue and PI operations
//!   return `ENOSYS`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::errno::{Errno, SysResult};
use crate::linux::futex::*;
use crate::linux::time::{CLOCK_MONOTONIC, CLOCK_REALTIME, Timespec};
use crate::sys::sync::{self as os, Wait};
use crate::time::clock_gettime;
use crate::util::lock;

const SHARDS: usize = 64;

/// Number of threads waiting per address, sharded by address.
static WAITERS: [Mutex<Vec<(usize, u32)>>; SHARDS] = [const { Mutex::new(Vec::new()) }; SHARDS];

fn shard(addr: usize) -> &'static Mutex<Vec<(usize, u32)>> {
    &WAITERS[(addr >> 2) % SHARDS]
}

fn add_waiter(addr: usize) {
    let mut v = lock(shard(addr));
    match v.iter_mut().find(|(a, _)| *a == addr) {
        Some((_, n)) => *n += 1,
        None => v.push((addr, 1)),
    }
}

fn remove_waiter(addr: usize) {
    let mut v = lock(shard(addr));
    if let Some(i) = v.iter().position(|(a, _)| *a == addr) {
        v[i].1 -= 1;
        if v[i].1 == 0 {
            v.swap_remove(i);
        }
    }
}

fn waiters(addr: usize) -> u32 {
    lock(shard(addr))
        .iter()
        .find(|(a, _)| *a == addr)
        .map_or(0, |(_, n)| *n)
}

/// A wait deadline: an instant on a clock, in nanoseconds.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    clock: i32,
    ns: i128,
}

impl Deadline {
    /// `timeout` from now on `CLOCK_MONOTONIC` (`FUTEX_WAIT`).
    pub fn relative(timeout: &Timespec) -> SysResult<Self> {
        if !timeout.is_valid_duration() {
            return Err(Errno::EINVAL);
        }
        let now = clock_gettime(CLOCK_MONOTONIC)?.as_nanos();
        Ok(Self {
            clock: CLOCK_MONOTONIC,
            ns: now + timeout.as_nanos(),
        })
    }

    /// `at` on the given clock (`FUTEX_WAIT_BITSET`).
    pub fn absolute(at: &Timespec, realtime: bool) -> SysResult<Self> {
        if !at.is_valid_duration() {
            return Err(Errno::EINVAL);
        }
        Ok(Self {
            clock: if realtime {
                CLOCK_REALTIME
            } else {
                CLOCK_MONOTONIC
            },
            ns: at.as_nanos(),
        })
    }

    /// Milliseconds left, rounded up; `None` once expired.
    fn remaining_ms(&self) -> SysResult<Option<u32>> {
        let left = self.ns - clock_gettime(self.clock)?.as_nanos();
        if left <= 0 {
            return Ok(None);
        }
        let ms = (left + 999_999) / 1_000_000;
        // u32::MAX means INFINITE to WaitOnAddress.
        Ok(Some(ms.min(i128::from(u32::MAX - 1)) as u32))
    }
}

/// `FUTEX_WAIT`: sleeps while `*word == val`. `Ok` on a wake-up (which may
/// be spurious, as on Linux), `EAGAIN` if the value differed, `ETIMEDOUT`.
pub fn wait(word: &AtomicU32, val: u32, deadline: Option<Deadline>) -> SysResult<()> {
    if word.load(Ordering::SeqCst) != val {
        return Err(Errno::EAGAIN);
    }
    let addr = word.as_ptr() as usize;
    add_waiter(addr);
    // A thread with signal state can be interrupted: senders wake it on
    // `addr`. A wake that comes between the check and the sleep is lost,
    // so such a thread sleeps at most a second at a time.
    let interruptible = crate::signal::sleeping_on(addr);
    let r = loop {
        if interruptible.is_some() && crate::signal::interrupted() {
            break Err(Errno::EINTR);
        }
        let ms = match deadline {
            None => None,
            Some(d) => match d.remaining_ms() {
                Ok(Some(ms)) => Some(ms),
                Ok(None) => break Err(Errno::ETIMEDOUT),
                Err(e) => break Err(e),
            },
        };
        let ms = if interruptible.is_some() {
            Some(ms.unwrap_or(1000).min(1000))
        } else {
            ms
        };
        match os::wait_on_address(word, val, ms) {
            Wait::Woken if interruptible.is_some() && crate::signal::interrupted() => {
                break Err(Errno::EINTR);
            }
            Wait::Woken => break Ok(()),
            // Re-check the deadline: a Windows timeout can end slightly
            // before it on the Linux clock.
            Wait::TimedOut => continue,
        }
    };
    remove_waiter(addr);
    r
}

/// `FUTEX_WAKE`: wakes up to `n` waiters on `addr`; returns how many were
/// waiting and were woken.
pub fn wake(addr: usize, n: i32) -> u32 {
    let waiting = waiters(addr);
    if waiting == 0 {
        return 0;
    }
    // Linux wakes at least one waiter even for n <= 0.
    let n = n.max(1) as u32;
    if n >= waiting {
        os::wake_all(addr);
        waiting
    } else {
        for _ in 0..n {
            os::wake_one(addr);
        }
        n
    }
}

/// The `futex` system call. `word` is `uaddr` as a reference; the caller
/// checks it for NULL and alignment ([`check_uaddr`]).
pub fn futex(
    word: &AtomicU32,
    op: i32,
    val: u32,
    timeout: Option<&Timespec>,
    val3: u32,
) -> SysResult<i64> {
    let realtime = op & FUTEX_CLOCK_REALTIME != 0;
    match op & FUTEX_CMD_MASK {
        FUTEX_WAIT => {
            let d = timeout.map(Deadline::relative).transpose()?;
            wait(word, val, d).map(|()| 0)
        }
        FUTEX_WAIT_BITSET => {
            match val3 {
                0 => return Err(Errno::EINVAL),
                FUTEX_BITSET_MATCH_ANY => {}
                _ => return Err(Errno::ENOSYS),
            }
            let d = timeout
                .map(|t| Deadline::absolute(t, realtime))
                .transpose()?;
            wait(word, val, d).map(|()| 0)
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            if realtime {
                return Err(Errno::ENOSYS);
            }
            if op & FUTEX_CMD_MASK == FUTEX_WAKE_BITSET {
                match val3 {
                    0 => return Err(Errno::EINVAL),
                    FUTEX_BITSET_MATCH_ANY => {}
                    _ => return Err(Errno::ENOSYS),
                }
            }
            Ok(i64::from(wake(word.as_ptr() as usize, val as i32)))
        }
        _ => Err(Errno::ENOSYS),
    }
}

/// Validates `uaddr` as Linux does before touching it.
pub fn check_uaddr(uaddr: usize) -> SysResult<()> {
    if uaddr == 0 {
        Err(Errno::EFAULT)
    } else if !uaddr.is_multiple_of(4) {
        Err(Errno::EINVAL)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const PRIV: i32 = FUTEX_PRIVATE_FLAG;

    #[test]
    fn wait_value_mismatch() {
        let w = AtomicU32::new(1);
        assert_eq!(futex(&w, FUTEX_WAIT | PRIV, 0, None, 0), Err(Errno::EAGAIN));
    }

    #[test]
    fn wait_times_out() {
        let w = AtomicU32::new(0);
        let t = Timespec {
            tv_sec: 0,
            tv_nsec: 30_000_000,
        };
        let start = Instant::now();
        assert_eq!(
            futex(&w, FUTEX_WAIT | PRIV, 0, Some(&t), 0),
            Err(Errno::ETIMEDOUT)
        );
        assert!(start.elapsed() >= Duration::from_millis(30));
        // Absolute deadline in the past, realtime clock.
        assert_eq!(
            futex(
                &w,
                FUTEX_WAIT_BITSET | PRIV | FUTEX_CLOCK_REALTIME,
                0,
                Some(&Timespec::default()),
                FUTEX_BITSET_MATCH_ANY
            ),
            Err(Errno::ETIMEDOUT)
        );
    }

    #[test]
    fn wake_without_waiters() {
        let w = AtomicU32::new(0);
        assert_eq!(futex(&w, FUTEX_WAKE | PRIV, 10, None, 0), Ok(0));
    }

    #[test]
    fn wake_counts_and_wakes() {
        let w = Arc::new(AtomicU32::new(0));
        let n = 4;
        let threads: Vec<_> = (0..n)
            .map(|_| {
                let w = w.clone();
                std::thread::spawn(move || {
                    while w.load(Ordering::SeqCst) == 0 {
                        let _ = futex(&w, FUTEX_WAIT | PRIV, 0, None, 0);
                    }
                })
            })
            .collect();
        // Wait until all four are registered.
        let addr = w.as_ptr() as usize;
        let start = Instant::now();
        while waiters(addr) < n {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::yield_now();
        }
        // Give them time to enter WaitOnAddress.
        std::thread::sleep(Duration::from_millis(20));
        // One wake without changing the value: exactly one returns and goes
        // back to sleep.
        assert_eq!(futex(&w, FUTEX_WAKE | PRIV, 1, None, 0), Ok(1));
        w.store(1, Ordering::SeqCst);
        let woken = futex(&w, FUTEX_WAKE | PRIV, i32::MAX as u32, None, 0).unwrap();
        assert!((1..=4).contains(&woken), "{woken}");
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(waiters(addr), 0);
    }

    #[test]
    fn bitset_rules() {
        let w = AtomicU32::new(0);
        assert_eq!(futex(&w, FUTEX_WAIT_BITSET, 0, None, 0), Err(Errno::EINVAL));
        assert_eq!(futex(&w, FUTEX_WAIT_BITSET, 0, None, 1), Err(Errno::ENOSYS));
        assert_eq!(futex(&w, FUTEX_WAKE_BITSET, 1, None, 0), Err(Errno::EINVAL));
        assert_eq!(
            futex(&w, FUTEX_WAKE_BITSET, 1, None, FUTEX_BITSET_MATCH_ANY),
            Ok(0)
        );
        assert_eq!(
            futex(&w, FUTEX_WAKE | FUTEX_CLOCK_REALTIME, 1, None, 0),
            Err(Errno::ENOSYS)
        );
        assert_eq!(futex(&w, 6 /* LOCK_PI */, 0, None, 0), Err(Errno::ENOSYS));
        let bad = Timespec {
            tv_sec: 0,
            tv_nsec: -1,
        };
        assert_eq!(futex(&w, FUTEX_WAIT, 0, Some(&bad), 0), Err(Errno::EINVAL));
        assert_eq!(check_uaddr(0), Err(Errno::EFAULT));
        assert_eq!(check_uaddr(2), Err(Errno::EINVAL));
    }
}

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
//! - `FUTEX_REQUEUE` and `FUTEX_CMP_REQUEUE` wake the waiters to be moved
//!   instead of moving them ([`requeue`]).
//! - PI futexes (`FUTEX_LOCK_PI`, `FUTEX_LOCK_PI2`, `FUTEX_TRYLOCK_PI`,
//!   `FUTEX_UNLOCK_PI`) keep Linux's word protocol (owner tid,
//!   `FUTEX_WAITERS`, `FUTEX_OWNER_DIED`) and mutual exclusion, but no
//!   priority is inherited: Windows has no way to lend a thread's priority
//!   to another. Unlocking frees the word and wakes one waiter instead of
//!   handing the lock to the highest-priority waiter, so a thread that
//!   arrives then can take it first. A lock wait is not ended by signals
//!   (Linux restarts it after the handler); `FUTEX_WAIT_REQUEUE_PI` and
//!   `FUTEX_CMP_REQUEUE_PI` return `ENOSYS`.
//! - Bitsets other than `FUTEX_BITSET_MATCH_ANY` return `ENOSYS`.

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

/// `FUTEX_LOCK_PI` / `FUTEX_TRYLOCK_PI` (`try_only`): takes the PI futex
/// `word` for the calling thread.
pub fn lock_pi(word: &AtomicU32, deadline: Option<Deadline>, try_only: bool) -> SysResult<i64> {
    let me = crate::process::gettid() as u32;
    let addr = word.as_ptr() as usize;
    let mut registered = false;
    let r = loop {
        let v = word.load(Ordering::SeqCst);
        let owner = v & FUTEX_TID_MASK;
        if owner == 0 {
            let others = waiters(addr) - u32::from(registered);
            let mut new = me | (v & FUTEX_OWNER_DIED);
            if others > 0 {
                new |= FUTEX_WAITERS;
            }
            if word
                .compare_exchange(v, new, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break Ok(0);
            }
            continue;
        }
        if owner == me {
            break Err(Errno::EDEADLK);
        }
        if try_only {
            break Err(Errno::EAGAIN);
        }
        if !crate::sys::thread::is_own_thread(owner) {
            // The owner is gone and nobody marked the word.
            break Err(Errno::ESRCH);
        }
        if !registered {
            add_waiter(addr);
            registered = true;
        }
        let want = v | FUTEX_WAITERS;
        if v != want
            && word
                .compare_exchange(v, want, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            continue;
        }
        let ms = match deadline {
            None => 1000,
            Some(d) => match d.remaining_ms() {
                Ok(Some(ms)) => ms.min(1000),
                Ok(None) => break Err(Errno::ETIMEDOUT),
                Err(e) => break Err(e),
            },
        };
        // Bounded, so that an owner that ends without unlocking is noticed.
        os::wait_on_address(word, want, Some(ms));
    };
    if registered {
        remove_waiter(addr);
    }
    r
}

/// `FUTEX_UNLOCK_PI`: releases `word`, which the calling thread owns, and
/// wakes a waiter.
pub fn unlock_pi(word: &AtomicU32) -> SysResult<i64> {
    let me = crate::process::gettid() as u32;
    let v = word.load(Ordering::SeqCst);
    if v & FUTEX_TID_MASK != me {
        return Err(Errno::EPERM);
    }
    word.store(0, Ordering::SeqCst);
    let addr = word.as_ptr() as usize;
    if waiters(addr) > 0 {
        os::wake_one(addr);
    }
    Ok(0)
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
        // FUTEX_LOCK_PI's timeout is on CLOCK_REALTIME; FUTEX_LOCK_PI2's on
        // CLOCK_MONOTONIC unless FUTEX_CLOCK_REALTIME is given.
        FUTEX_LOCK_PI | FUTEX_LOCK_PI2 => {
            let rt = realtime || op & FUTEX_CMD_MASK == FUTEX_LOCK_PI;
            let d = timeout.map(|t| Deadline::absolute(t, rt)).transpose()?;
            lock_pi(word, d, false)
        }
        FUTEX_TRYLOCK_PI => lock_pi(word, None, true),
        FUTEX_UNLOCK_PI => unlock_pi(word),
        _ => Err(Errno::ENOSYS),
    }
}

/// Wakes up to `n` waiters on `addr` (none for `n == 0`).
fn wake_n(addr: usize, n: u32) -> u32 {
    if n == 0 {
        0
    } else {
        wake(addr, n.min(i32::MAX as u32) as i32)
    }
}

/// `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` (with `expected`): wakes up to
/// `n_wake` waiters on `word` and moves up to `n_requeue` others to
/// `word2`. `WaitOnAddress` cannot move a waiter to another address, so
/// the ones to move are woken instead: a futex waiter treats that as a
/// spurious wake-up, checks its condition and waits again (on `word2`, if
/// that is where its condition now lives). Returns the number woken plus
/// the number "moved", as Linux does.
pub fn requeue(
    word: &AtomicU32,
    _word2: &AtomicU32,
    n_wake: u32,
    n_requeue: u32,
    expected: Option<u32>,
) -> SysResult<i64> {
    if n_wake > i32::MAX as u32 || n_requeue > i32::MAX as u32 {
        return Err(Errno::EINVAL);
    }
    if let Some(v) = expected
        && word.load(Ordering::SeqCst) != v
    {
        return Err(Errno::EAGAIN);
    }
    let addr = word.as_ptr() as usize;
    let waiting = waiters(addr);
    let woken = wake_n(addr, n_wake.min(waiting));
    let moved = (waiting - woken).min(n_requeue);
    if moved > 0 {
        os::wake_all(addr);
    }
    Ok(i64::from(woken + moved))
}

/// `FUTEX_WAKE_OP`: applies the operation encoded in `val3` to `word2`
/// atomically, wakes up to `n` waiters on `word`, and up to `n2` on
/// `word2` if the old value of `word2` passes the encoded comparison.
pub fn wake_op(word: &AtomicU32, word2: &AtomicU32, n: u32, n2: u32, val3: u32) -> SysResult<i64> {
    let sext12 = |v: u32| (((v & 0xfff) << 20) as i32) >> 20;
    let op = (val3 >> 28) & 0xf;
    let cmp = (val3 >> 24) & 0xf;
    let mut oparg = sext12(val3 >> 12) as u32;
    let cmparg = sext12(val3);
    if op & 8 != 0 {
        // FUTEX_OP_OPARG_SHIFT
        if oparg > 31 {
            return Err(Errno::EINVAL);
        }
        oparg = 1 << oparg;
    }
    let f = |old: u32| -> Option<u32> {
        match op & 7 {
            0 => Some(oparg),
            1 => Some(old.wrapping_add(oparg)),
            2 => Some(old | oparg),
            3 => Some(old & !oparg),
            4 => Some(old ^ oparg),
            _ => None,
        }
    };
    if f(0).is_none() || cmp > 5 {
        return Err(Errno::ENOSYS);
    }
    let old = word2
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, f)
        .expect("operation checked above") as i32;
    let mut r = wake_n(word.as_ptr() as usize, n);
    let pass = match cmp {
        0 => old == cmparg,
        1 => old != cmparg,
        2 => old < cmparg,
        3 => old <= cmparg,
        4 => old > cmparg,
        _ => old >= cmparg,
    };
    if pass {
        r += wake_n(word2.as_ptr() as usize, n2);
    }
    Ok(i64::from(r))
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
    fn pi_lock_protocol() {
        let word = Arc::new(AtomicU32::new(0));
        let me = crate::process::gettid() as u32;
        let op = |o: i32| o | PRIV;
        assert_eq!(futex(&word, op(FUTEX_LOCK_PI), 0, None, 0), Ok(0));
        assert_eq!(word.load(Ordering::SeqCst), me);
        assert_eq!(
            futex(&word, op(FUTEX_LOCK_PI), 0, None, 0),
            Err(Errno::EDEADLK)
        );
        // Another thread waits; its timeout passes first.
        let w = word.clone();
        let t = std::thread::spawn(move || {
            let past = Timespec::default();
            let r = futex(&w, op(FUTEX_LOCK_PI), 0, Some(&past), 0);
            assert_eq!(r, Err(Errno::ETIMEDOUT));
            assert_eq!(
                futex(&w, op(FUTEX_TRYLOCK_PI), 0, None, 0),
                Err(Errno::EAGAIN)
            );
            let r = futex(&w, op(FUTEX_LOCK_PI), 0, None, 0);
            let tid = crate::process::gettid() as u32;
            (r, w.load(Ordering::SeqCst) & FUTEX_TID_MASK == tid)
        });
        // Wait until it marks the word.
        let start = Instant::now();
        while word.load(Ordering::SeqCst) & FUTEX_WAITERS == 0 {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(word.load(Ordering::SeqCst) & FUTEX_TID_MASK, me);
        assert_eq!(futex(&word, op(FUTEX_UNLOCK_PI), 0, None, 0), Ok(0));
        let (r, owns) = t.join().unwrap();
        assert_eq!(r, Ok(0));
        assert!(owns);
        // Not the owner: EPERM; the owner ended: ESRCH.
        assert_eq!(
            futex(&word, op(FUTEX_UNLOCK_PI), 0, None, 0),
            Err(Errno::EPERM)
        );
        assert_eq!(
            futex(&word, op(FUTEX_LOCK_PI), 0, None, 0),
            Err(Errno::ESRCH)
        );
    }

    #[test]
    fn requeue_and_wake_op() {
        let w = Arc::new(AtomicU32::new(0));
        let w2 = AtomicU32::new(5);
        let mut threads = Vec::new();
        for _ in 0..3 {
            let w = w.clone();
            threads.push(std::thread::spawn(move || wait(&w, 0, None)));
        }
        while waiters(w.as_ptr() as usize) < 3 {
            std::thread::yield_now();
        }
        assert_eq!(requeue(&w, &w2, 1, 1, Some(7)), Err(Errno::EAGAIN));
        // One woken, two "moved" (woken as well).
        assert_eq!(requeue(&w, &w2, 1, 5, Some(0)), Ok(3));
        for t in threads {
            assert_eq!(t.join().unwrap(), Ok(()));
        }
        assert_eq!(requeue(&w, &w2, 1, 1, None), Ok(0));

        // FUTEX_OP(ADD, 3, CMP_EQ, 5): w2 += 3, wake on w2 if it was 5.
        let val3 = (1 << 28) | (3 << 12) | 5;
        assert_eq!(wake_op(&w, &w2, 1, 1, val3), Ok(0));
        assert_eq!(w2.load(Ordering::SeqCst), 8);
        // OPARG_SHIFT: w2 |= 1 << 4.
        let val3 = ((8 | 2) << 28) | (4 << 12);
        wake_op(&w, &w2, 1, 1, val3).unwrap();
        assert_eq!(w2.load(Ordering::SeqCst), 24);
        assert_eq!(wake_op(&w, &w2, 1, 1, 7 << 28), Err(Errno::ENOSYS));
    }

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
        assert_eq!(
            futex(&w, 11 /* WAIT_REQUEUE_PI */, 0, None, 0),
            Err(Errno::ENOSYS)
        );
        let bad = Timespec {
            tv_sec: 0,
            tv_nsec: -1,
        };
        assert_eq!(futex(&w, FUTEX_WAIT, 0, Some(&bad), 0), Err(Errno::EINVAL));
        assert_eq!(check_uaddr(0), Err(Errno::EFAULT));
        assert_eq!(check_uaddr(2), Err(Errno::EINVAL));
    }
}

//! Scheduling policy and CPU affinity: `sched_getscheduler`,
//! `sched_setscheduler`, `sched_getparam`, `sched_setparam`,
//! `sched_get_priority_max/min`, `sched_getaffinity`,
//! `sched_setaffinity`.
//!
//! Threads run under Windows' scheduler, which corresponds to Linux's
//! normal class. `SCHED_OTHER`, `SCHED_BATCH` and `SCHED_IDLE` (priority
//! 0) are accepted and reported back; `SCHED_BATCH` and `SCHED_IDLE` are
//! scheduling hints that Windows has no equivalent for. The real-time
//! classes fail with `EPERM`, as for a Linux process without
//! `CAP_SYS_NICE`. Affinity is the Windows thread affinity mask (machines
//! with more than 64 logical processors are seen through the calling
//! thread's processor group).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::errno::{Errno, SysResult};
use crate::util::lock;

pub const SCHED_OTHER: i32 = 0;
pub const SCHED_FIFO: i32 = 1;
pub const SCHED_RR: i32 = 2;
pub const SCHED_BATCH: i32 = 3;
pub const SCHED_IDLE: i32 = 5;
pub const SCHED_DEADLINE: i32 = 6;
/// Flag a caller may add to the policy.
pub const SCHED_RESET_ON_FORK: i32 = 0x4000_0000;

/// Policies set per thread id (threads not listed use `SCHED_OTHER`).
static POLICIES: Mutex<Option<HashMap<i32, i32>>> = Mutex::new(None);

/// The thread id a `pid` argument names: 0 is the caller; otherwise a
/// thread of this process (or the process id, its main thread's group).
fn target(pid: i32) -> SysResult<i32> {
    if pid < 0 {
        return Err(Errno::EINVAL);
    }
    let me = crate::process::gettid();
    if pid == 0 || pid == me || pid == crate::process::getpid() {
        return Ok(if pid == 0 { me } else { pid });
    }
    if crate::sys::thread::is_own_thread(pid as u32) {
        Ok(pid)
    } else {
        Err(Errno::ESRCH)
    }
}

pub fn getscheduler(pid: i32) -> SysResult<i32> {
    let t = target(pid)?;
    Ok(lock(&POLICIES)
        .as_ref()
        .and_then(|m| m.get(&t).copied())
        .unwrap_or(SCHED_OTHER))
}

pub fn setscheduler(pid: i32, policy: i32, priority: i32) -> SysResult<()> {
    let t = target(pid)?;
    match policy & !SCHED_RESET_ON_FORK {
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => {
            if priority != 0 {
                return Err(Errno::EINVAL);
            }
            lock(&POLICIES)
                .get_or_insert_with(HashMap::new)
                .insert(t, policy & !SCHED_RESET_ON_FORK);
            Ok(())
        }
        SCHED_FIFO | SCHED_RR => {
            if !(1..=99).contains(&priority) {
                return Err(Errno::EINVAL);
            }
            Err(Errno::EPERM)
        }
        SCHED_DEADLINE => Err(Errno::EINVAL),
        _ => Err(Errno::EINVAL),
    }
}

/// `sched_getparam`: the priority, 0 in the normal classes.
pub fn getparam(pid: i32) -> SysResult<i32> {
    target(pid).map(|_| 0)
}

/// `sched_setparam`: only priority 0 exists in the normal classes.
pub fn setparam(pid: i32, priority: i32) -> SysResult<()> {
    target(pid)?;
    if priority == 0 {
        Ok(())
    } else {
        Err(Errno::EINVAL)
    }
}

pub fn priority_max(policy: i32) -> SysResult<i32> {
    match policy {
        SCHED_FIFO | SCHED_RR => Ok(99),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        _ => Err(Errno::EINVAL),
    }
}

pub fn priority_min(policy: i32) -> SysResult<i32> {
    match policy {
        SCHED_FIFO | SCHED_RR => Ok(1),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        _ => Err(Errno::EINVAL),
    }
}

/// The size of the kernel's CPU mask in bytes (whole `unsigned long`s).
fn mask_bytes() -> usize {
    crate::sys::thread::processor_count().div_ceil(64).max(1) * 8
}

/// `sched_getaffinity`: the mask, `mask_bytes()` long. `EINVAL` when the
/// caller's buffer is smaller.
pub fn getaffinity(pid: i32, len: usize) -> SysResult<Vec<u8>> {
    let t = target(pid)?;
    let n = mask_bytes();
    if len < n || !len.is_multiple_of(8) {
        return Err(Errno::EINVAL);
    }
    let m = crate::sys::thread::affinity(t as u32)?;
    let mut out = vec![0u8; n];
    out[..8].copy_from_slice(&m.to_le_bytes());
    Ok(out)
}

/// `sched_setaffinity`: CPUs outside the machine are ignored; `EINVAL` if
/// none is left.
pub fn setaffinity(pid: i32, mask: &[u8]) -> SysResult<()> {
    let t = target(pid)?;
    let mut m = [0u8; 8];
    let k = mask.len().min(8);
    m[..k].copy_from_slice(&mask[..k]);
    let n = crate::sys::thread::processor_count().min(64);
    let all = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
    let m = u64::from_le_bytes(m) & all;
    if m == 0 {
        return Err(Errno::EINVAL);
    }
    crate::sys::thread::set_affinity(t as u32, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_and_affinity() {
        std::thread::spawn(|| {
            assert_eq!(getscheduler(0), Ok(SCHED_OTHER));
            assert_eq!(setscheduler(0, SCHED_FIFO, 10), Err(Errno::EPERM));
            assert_eq!(setscheduler(0, SCHED_FIFO, 0), Err(Errno::EINVAL));
            assert_eq!(setscheduler(0, SCHED_OTHER, 1), Err(Errno::EINVAL));
            assert_eq!(setscheduler(0, SCHED_BATCH, 0), Ok(()));
            assert_eq!(getscheduler(0), Ok(SCHED_BATCH));
            assert_eq!(setscheduler(0, SCHED_OTHER, 0), Ok(()));
            assert_eq!(getparam(0), Ok(0));
            assert_eq!(setparam(0, 1), Err(Errno::EINVAL));
            assert_eq!(getscheduler(-1), Err(Errno::EINVAL));
            assert_eq!(priority_max(SCHED_RR), Ok(99));
            assert_eq!(priority_min(SCHED_OTHER), Ok(0));
            assert_eq!(priority_max(42), Err(Errno::EINVAL));

            let m = getaffinity(0, 128).unwrap();
            assert_eq!(m.len() % 8, 0);
            assert_ne!(m[0], 0);
            assert_eq!(getaffinity(0, 1), Err(Errno::EINVAL));
            // Pin to the first CPU in the mask, then restore.
            let first = m[0].trailing_zeros();
            setaffinity(0, &(1u64 << first).to_le_bytes()).unwrap();
            assert_eq!(getaffinity(0, 8).unwrap()[0], 1 << first);
            setaffinity(0, &m).unwrap();
            assert_eq!(setaffinity(0, &[0u8; 8]), Err(Errno::EINVAL));
        })
        .join()
        .unwrap();
    }
}

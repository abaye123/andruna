//! Blocking waits on lx objects (pipes, sockets, eventfd, timerfd) and
//! readiness notification for `poll`, `select` and `epoll`.
//!
//! An object that changes state (data arrives, space frees up, a peer
//! closes) calls [`notify`]. A thread that blocks re-checks its condition
//! after every notification: [`wait_until`] sleeps on one process-wide
//! sequence number, so any change anywhere wakes every waiter, which then
//! re-checks and sleeps again. That keeps the model simple and free of lost
//! wake-ups (the number is read before the condition is checked); the cost
//! is spurious wake-ups when many threads block on different objects at
//! once.
//!
//! Waits are interruptible as on Linux: a signal the thread can receive
//! ends the wait with `EINTR` (the sender wakes the thread through
//! [`crate::signal::sleeping_on`]).

use std::sync::atomic::{AtomicU32, Ordering};

use crate::errno::{Errno, SysResult};
use crate::sys::sync as os;

/// Changed and woken on every state change of a waitable object.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// Reports a state change: wakes every thread blocked in [`wait_until`].
pub fn notify() {
    SEQ.fetch_add(1, Ordering::SeqCst);
    os::wake_all(SEQ.as_ptr() as usize);
}

/// A deadline in `CLOCK_MONOTONIC` nanoseconds.
pub type Deadline = Option<i128>;

/// The deadline `timeout_ns` from now (`None` for no timeout).
pub fn deadline_in(timeout_ns: Option<i128>) -> Deadline {
    timeout_ns.map(|t| crate::time::monotonic_ns() + t.max(0))
}

/// Blocks until `ready()` returns `Some`, the deadline passes
/// (`ETIMEDOUT`), or a signal arrives (`EINTR`, checked only after
/// `ready()` failed once, so a ready object never gives `EINTR`).
pub fn wait_until<T>(
    deadline: Deadline,
    mut ready: impl FnMut() -> SysResult<Option<T>>,
) -> SysResult<T> {
    let addr = SEQ.as_ptr() as usize;
    let guard = crate::signal::sleeping_on(addr);
    loop {
        let seen = SEQ.load(Ordering::SeqCst);
        if let Some(v) = ready()? {
            return Ok(v);
        }
        if guard.is_some() && crate::signal::interrupted() {
            return Err(Errno::EINTR);
        }
        let ms = match deadline {
            None => None,
            Some(d) => {
                let left = d - crate::time::monotonic_ns();
                if left <= 0 {
                    return Err(Errno::ETIMEDOUT);
                }
                Some(((left + 999_999) / 1_000_000).min(i128::from(u32::MAX - 1)) as u32)
            }
        };
        // A signal sent between the check above and the sleep wakes the
        // sequence address too (the guard), but bound the sleep anyway.
        let ms = if guard.is_some() {
            Some(ms.unwrap_or(1000).min(1000))
        } else {
            ms
        };
        os::wait_on_address(&SEQ, seen, ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn wakes_on_notify_and_times_out() {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let t = std::thread::spawn(move || {
            wait_until(None, || Ok(f.load(Ordering::SeqCst).then_some(7)))
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        flag.store(true, Ordering::SeqCst);
        notify();
        assert_eq!(t.join().unwrap(), Ok(7));

        let start = std::time::Instant::now();
        let r: SysResult<()> = wait_until(deadline_in(Some(20_000_000)), || Ok(None));
        assert_eq!(r, Err(Errno::ETIMEDOUT));
        assert!(start.elapsed() >= std::time::Duration::from_millis(19));
    }
}

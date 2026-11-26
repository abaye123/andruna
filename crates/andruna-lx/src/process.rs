//! Process and thread identity: `getpid`, `gettid`, `sched_yield`, and the
//! user and group ids lx reports.
//!
//! Deviation: Linux gives the main thread `tid == pid`. Windows process and
//! thread ids come from one namespace and never coincide, so `gettid()` on
//! the main thread differs from `getpid()`. Code that compares them to find
//! the main thread needs lx to designate a main thread (TODO with the
//! thread layer).

use std::sync::atomic::{AtomicU32, Ordering};

use crate::sys::thread;

static UID: AtomicU32 = AtomicU32::new(0);
static GID: AtomicU32 = AtomicU32::new(0);

pub fn getpid() -> i32 {
    thread::current_process_id() as i32
}

pub fn gettid() -> i32 {
    thread::current_thread_id() as i32
}

pub fn sched_yield() {
    thread::yield_now();
}

/// Sets the user and group id that lx reports for this process and its
/// files. The host process sets the package's ids before running app code;
/// until then both are 0.
pub fn set_ids(uid: u32, gid: u32) {
    UID.store(uid, Ordering::Relaxed);
    GID.store(gid, Ordering::Relaxed);
}

pub fn uid() -> u32 {
    UID.load(Ordering::Relaxed)
}

pub fn gid() -> u32 {
    GID.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert_eq!(getpid() as u32, std::process::id());
        let main_tid = gettid();
        let other = std::thread::spawn(gettid).join().unwrap();
        assert_ne!(main_tid, other);
        assert!(main_tid > 0 && other > 0);
        sched_yield();
    }
}

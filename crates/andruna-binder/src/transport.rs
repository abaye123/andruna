//! The transport between a guest process and the broker (ADR 0025,
//! spike S3).
//!
//! - **Control pipe.** A local, message-mode named pipe; each request and
//!   reply is one [`Msg`]. The guest uses it for everything except
//!   `BINDER_WRITE_READ`: registration, its threads, the receive buffer,
//!   thread limits, the context manager, thread exit, `poll`. The broker
//!   learns the guest's process id from the pipe (never from the guest)
//!   and duplicates every handle it gives into the guest process itself.
//! - **Lanes.** Each guest thread that uses binder has a lane: a section
//!   shared with its broker thread, holding a [`LaneHeader`], the packed
//!   write stream and the read stream, and two auto-reset events. The
//!   guest writes the request, then wakes the broker thread and sleeps in
//!   one `SignalObjectAndWait`; the broker thread answers the same way.

use core::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The pipe name a guest connects to, from the environment variable
/// [`PIPE_ENV`] (set by sysd for the processes it starts).
pub const PIPE_ENV: &str = "ANDRUNA_BINDER_PIPE";

/// Control requests.
pub mod op {
    /// `a`: protocol version. Reply: `x` the broker's process handle
    /// (`SYNCHRONIZE`), `y` the poll event.
    pub const HELLO: u32 = 1;
    /// `a`: thread id. Reply: `x` lane section, `y` request event, `z`
    /// response event.
    pub const THREAD: u32 = 2;
    /// `a`: length. Reply: `x` the section of the receive buffer.
    pub const MMAP: u32 = 3;
    /// `a`: the address of the guest's view, `b`: its length.
    pub const MAPPED: u32 = 4;
    /// `a`: maximum number of looper threads.
    pub const MAX_THREADS: u32 = 5;
    /// `flags`: object flags, `a`: binder, `b`: cookie, `c`: 1 for the
    /// `_EXT` form.
    pub const CONTEXT_MGR: u32 = 6;
    /// `a`: thread id.
    pub const THREAD_EXIT: u32 = 7;
    /// `a`: thread id. Reply `x`: 1 if the thread has work.
    pub const HAS_WORK: u32 = 8;
}

/// The control protocol version.
pub const VERSION: u64 = 1;

/// A control request or reply. In a reply, `op` is the status: 0 or a
/// negative errno.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Msg {
    pub op: i32,
    pub flags: u32,
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

const _: () = assert!(size_of::<Msg>() == 32);

impl Msg {
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0..4].copy_from_slice(&self.op.to_le_bytes());
        b[4..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8..16].copy_from_slice(&self.a.to_le_bytes());
        b[16..24].copy_from_slice(&self.b.to_le_bytes());
        b[24..32].copy_from_slice(&self.c.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != 32 {
            return None;
        }
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        Some(Self {
            op: i32::from_le_bytes(b[0..4].try_into().unwrap()),
            flags: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            a: u64_at(8),
            b: u64_at(16),
            c: u64_at(24),
        })
    }
}

/// The header at the start of a lane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LaneHeader {
    /// Request: bytes of the packed write stream.
    pub write_len: u64,
    /// Request: room for the read stream (0: no read).
    pub read_cap: u64,
    /// Request: [`LANE_FIRST_READ`], [`LANE_NONBLOCK`].
    pub flags: u32,
    /// Reply: 0 or a negative errno.
    pub status: i32,
    /// Reply: bytes of the write stream consumed.
    pub write_consumed: u64,
    /// Reply: bytes of the read stream.
    pub read_len: u64,
}

const _: () = assert!(size_of::<LaneHeader>() == 40);

pub const LANE_FIRST_READ: u32 = 1;
pub const LANE_NONBLOCK: u32 = 2;

/// Offset of the write stream in a lane.
pub const LANE_WRITE: usize = 64;
/// Room for the write stream: a transaction as large as the largest
/// receive buffer libbinder maps (1 MiB) plus the commands around it.
pub const LANE_WRITE_MAX: usize = (1 << 20) + (64 << 10);
/// Offset of the read stream.
pub const LANE_READ: usize = LANE_WRITE + LANE_WRITE_MAX;
/// Room for the read stream.
pub const LANE_READ_MAX: usize = 64 << 10;
/// Size of a lane section.
pub const LANE_SIZE: usize = LANE_READ + LANE_READ_MAX;

/// Offsets of the wake words in a lane (after the [`LaneHeader`]).
const REQ_SEQ: usize = 40;
const REQ_WAITING: usize = 44;
const RESP_SEQ: usize = 48;
const RESP_WAITING: usize = 52;

/// One direction of a lane: a sequence number and a "sleeping" flag in
/// shared memory, and an auto-reset event (S3's wake mechanism). The
/// producer publishes the sequence and sets the event only when the
/// consumer announced that it sleeps; the consumer spins for a bounded
/// time, then announces, re-checks and sleeps. A stale event causes at
/// most one spurious wake-up, after which the sequence is checked again.
#[derive(Clone, Copy, Debug)]
pub struct Wake {
    seq: *const AtomicU32,
    waiting: *const AtomicU32,
    event: windows::Win32::Foundation::HANDLE,
}

// SAFETY: the words live in a lane mapping that outlives every `Wake`
// (the owner keeps the view mapped while its threads use it).
unsafe impl Send for Wake {}

impl Wake {
    /// The request (`req`) or response direction of the lane at `base`.
    ///
    /// # Safety
    ///
    /// `base` is a mapped lane of [`LANE_SIZE`] bytes that stays mapped
    /// while the value is used; `event` stays open as long.
    pub unsafe fn new(base: *mut u8, req: bool, event: windows::Win32::Foundation::HANDLE) -> Self {
        let (s, w) = if req {
            (REQ_SEQ, REQ_WAITING)
        } else {
            (RESP_SEQ, RESP_WAITING)
        };
        // SAFETY: inside the lane header area (contract).
        unsafe {
            Self {
                seq: base.add(s).cast(),
                waiting: base.add(w).cast(),
                event,
            }
        }
    }

    fn seq(&self) -> &AtomicU32 {
        // SAFETY: see `new`; 4-byte aligned in a page-aligned mapping.
        unsafe { &*self.seq }
    }

    fn waiting(&self) -> &AtomicU32 {
        // SAFETY: as above.
        unsafe { &*self.waiting }
    }

    /// The current sequence number (read before sending, to wait for the
    /// next one).
    pub fn current(&self) -> u32 {
        self.seq().load(Ordering::SeqCst)
    }

    /// Publishes a message and wakes the consumer if it sleeps.
    pub fn post(&self) {
        self.seq().fetch_add(1, Ordering::SeqCst);
        if self.waiting().swap(0, Ordering::SeqCst) != 0 {
            crate::sys::set_event(self.event);
        }
    }

    /// Publishes a message on this direction and waits for the next one
    /// on `reply` (whose number was `seen`). Without spinning, waking the
    /// peer and sleeping is one `SignalObjectAndWait`.
    pub fn post_then_wait(
        &self,
        reply: &Wake,
        seen: u32,
        spin: Duration,
        slice_ms: u32,
        alive: &dyn Fn() -> bool,
    ) -> bool {
        if !spin.is_zero() {
            self.post();
            return reply.wait(seen, spin, slice_ms, alive);
        }
        reply.waiting().store(1, Ordering::SeqCst);
        self.seq().fetch_add(1, Ordering::SeqCst);
        if self.waiting().swap(0, Ordering::SeqCst) != 0 {
            crate::sys::signal_and_wait(self.event, reply.event, slice_ms);
        } else if reply.current() == seen {
            crate::sys::wait(reply.event, Some(slice_ms));
        }
        reply.current() != seen || reply.wait(seen, Duration::ZERO, slice_ms, alive)
    }

    /// Waits until the sequence number differs from `seen`: spins for up
    /// to `spin`, then sleeps on the event in slices of `slice_ms`
    /// (calling `alive` after each empty slice; `false` ends the wait).
    pub fn wait(&self, seen: u32, spin: Duration, slice_ms: u32, alive: &dyn Fn() -> bool) -> bool {
        if !spin.is_zero() {
            let start = Instant::now();
            loop {
                for _ in 0..64 {
                    if self.current() != seen {
                        return true;
                    }
                    core::hint::spin_loop();
                }
                if start.elapsed() >= spin {
                    break;
                }
            }
        }
        loop {
            self.waiting().store(1, Ordering::SeqCst);
            if self.current() != seen {
                return true;
            }
            crate::sys::wait(self.event, Some(slice_ms));
            if self.current() != seen {
                return true;
            }
            if !alive() {
                return false;
            }
        }
    }
}

/// How long a waiting side spins before it sleeps: bounded, only while the
/// lane is busy (a message within the last 2 ms), and never on machines
/// with fewer than four logical processors, where a spinning thread takes
/// the processor the thread it waits for needs (S3, ARM64 runner).
/// `ANDRUNA_BINDER_SPIN_US` overrides the budget (0 disables spinning).
pub fn spin_budget(hot: &Hot) -> Duration {
    if hot.recent() {
        spin_budget_when_hot()
    } else {
        Duration::ZERO
    }
}

/// The spin budget of a busy side (see [`spin_budget`]).
pub fn spin_budget_when_hot() -> Duration {
    static CFG: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CFG.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        match std::env::var("ANDRUNA_BINDER_SPIN_US")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            Some(us) => Duration::from_micros(us),
            None if cpus >= 4 => SPIN,
            None => Duration::ZERO,
        }
    })
}

/// When a side last got a message.
#[derive(Debug, Default)]
pub struct Hot(std::sync::atomic::AtomicU64);

fn epoch() -> Instant {
    static E: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *E.get_or_init(Instant::now)
}

impl Hot {
    pub fn mark(&self) {
        let us = epoch().elapsed().as_micros() as u64 + 1;
        self.0.store(us, Ordering::Relaxed);
    }

    /// A message within the last 2 ms.
    pub fn recent(&self) -> bool {
        let t = self.0.load(Ordering::Relaxed);
        t != 0 && (epoch().elapsed().as_micros() as u64 + 1).saturating_sub(t) < 2000
    }
}

/// The default spin budget per wait.
pub const SPIN: Duration = Duration::from_micros(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_round_trip() {
        let m = Msg {
            op: -22,
            flags: 7,
            a: u64::MAX,
            b: 2,
            c: 3,
        };
        assert_eq!(Msg::from_bytes(&m.to_bytes()), Some(m));
        assert_eq!(Msg::from_bytes(&[0; 31]), None);
    }
}

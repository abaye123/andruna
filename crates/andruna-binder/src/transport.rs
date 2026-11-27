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

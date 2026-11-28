//! Thin wrappers around the Windows APIs lx uses.
//!
//! This is where the `unsafe` calls into Windows live. Each wrapper states
//! the conditions it relies on and converts failures to [`Errno`]. The rest
//! of the crate calls only these wrappers.
//!
//! [`Errno`]: crate::errno::Errno

pub mod alarm;
pub mod ea;
pub mod file;
pub mod mem;
pub mod procinfo;
pub mod random;
pub mod sync;
pub mod sysinfo;
pub mod thread;
pub mod time;

use windows::Win32::Foundation::{self as wf, CloseHandle, HANDLE};

/// `RtlNtStatusToDosError`.
pub fn nt_status_to_win32(status: wf::NTSTATUS) -> u32 {
    // SAFETY: pure conversion function without pointer arguments.
    unsafe { wf::RtlNtStatusToDosError(status) }
}

/// An owned Windows kernel handle, closed on drop.
#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

// SAFETY: a kernel handle is a process-wide value; Windows handle operations
// are safe to call from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above; `&OwnedHandle` only gives out the raw value.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// Takes ownership of `h`, which must be a valid handle that nothing
    /// else closes.
    pub(crate) fn from_raw(h: HANDLE) -> Self {
        Self(h)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle owned by this value (see
        // `from_raw`) and is closed exactly once. A failure cannot be
        // reported from `drop` and leaves nothing to clean up.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

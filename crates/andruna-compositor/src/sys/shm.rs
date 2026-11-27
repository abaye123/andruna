//! Shared memory sections and events, the primitives of the input
//! channel. All `unsafe` code of the channel is here.

use std::io;
use std::sync::atomic::{AtomicU8, AtomicU64};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, SetEvent, WaitForSingleObject,
};

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

/// An owned kernel handle.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: kernel handles are process-wide and usable from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; the operations used on it are thread safe.
unsafe impl Sync for Handle {}

impl Handle {
    /// The raw value, for passing to another process after duplication.
    pub fn raw(&self) -> usize {
        self.0.0 as usize
    }

    /// Takes ownership of a raw handle value.
    ///
    /// # Safety
    /// `raw` must be a valid handle owned by the caller and not closed
    /// elsewhere.
    pub unsafe fn from_raw(raw: usize) -> Self {
        Self(HANDLE(raw as *mut _))
    }

    /// Duplicates the handle into `process` (a process handle with
    /// `PROCESS_DUP_HANDLE`), or into this process when `None`. Returns
    /// the value valid in the target process.
    pub fn duplicate_into(&self, process: Option<usize>) -> io::Result<usize> {
        let mut out = HANDLE::default();
        // SAFETY: both process handles are valid (the pseudo handle of the
        // current process, or the caller's); `out` is a valid out pointer.
        unsafe {
            let target = process.map_or(GetCurrentProcess(), |p| HANDLE(p as *mut _));
            DuplicateHandle(
                GetCurrentProcess(),
                self.0,
                target,
                &mut out,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .map_err(to_io)?;
        }
        Ok(out.0 as usize)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A pagefile-backed section mapped into this process.
#[derive(Debug)]
pub struct Section {
    handle: Handle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

// SAFETY: the view is plain shared memory, accessed only through atomics.
unsafe impl Send for Section {}
// SAFETY: as above.
unsafe impl Sync for Section {}

impl Section {
    /// Creates and maps a zeroed section of `len` bytes.
    pub fn create(len: usize) -> io::Result<Self> {
        // SAFETY: an anonymous, unnamed section; no pointers are passed.
        let h = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                (len as u64 >> 32) as u32,
                len as u32,
                None,
            )
        }
        .map_err(to_io)?;
        Self::map(Handle(h), len)
    }

    /// Maps a section handle received from another process.
    pub fn map(handle: Handle, len: usize) -> io::Result<Self> {
        // SAFETY: `handle` is a section handle; a failed map returns null.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, view, len })
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The section's bytes, as atomics (another process writes them
    /// concurrently).
    pub fn bytes(&self) -> &[AtomicU8] {
        // SAFETY: the view is `len` bytes, mapped for the life of `self`,
        // and `AtomicU8` has the layout of `u8`. Every access goes through
        // atomics, so concurrent writers in other processes are not data
        // races.
        unsafe { std::slice::from_raw_parts(self.view.Value as *const AtomicU8, self.len) }
    }

    /// The 8-byte aligned word at `offset`.
    pub fn word(&self, offset: usize) -> &AtomicU64 {
        assert!(offset.is_multiple_of(8) && offset + 8 <= self.len);
        // SAFETY: in bounds and aligned (the view is page aligned and
        // `offset` a multiple of 8); `AtomicU64` has the layout of `u64`.
        unsafe { &*((self.view.Value as *const u8).add(offset) as *const AtomicU64) }
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: the view was mapped by `map` and is unmapped once; no
        // references outlive `self`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

/// An auto-reset event.
#[derive(Debug)]
pub struct Event(Handle);

impl Event {
    pub fn create() -> io::Result<Self> {
        // SAFETY: an unnamed event; no pointers are passed.
        let h = unsafe { CreateEventW(None, false, false, None) }.map_err(to_io)?;
        Ok(Self(Handle(h)))
    }

    pub fn from_handle(h: Handle) -> Self {
        Self(h)
    }

    pub fn handle(&self) -> &Handle {
        &self.0
    }

    pub fn set(&self) {
        // SAFETY: a valid event handle.
        unsafe {
            let _ = SetEvent(self.0.0);
        }
    }

    /// Waits until set or `timeout` passes; true when set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: a valid event handle.
        unsafe { WaitForSingleObject(self.0.0, ms) == WAIT_OBJECT_0 }
    }
}

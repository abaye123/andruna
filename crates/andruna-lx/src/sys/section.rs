//! File sections and their views in placeholders (`CreateFileMappingW`,
//! `MapViewOfFile3`, `UnmapViewOfFile2`), for lx's file mappings
//! ([`crate::mem`]).
//!
//! Measured on Windows 11 build 26300 (the tests in `crate::mem` check the
//! behavior lx relies on):
//!
//! - A view must start on a 64 KiB boundary of the address space and of
//!   the file, and must replace a placeholder of exactly its size (pages).
//!   Its size need not be a multiple of 64 KiB: the rest of the last block
//!   can be another placeholder or allocation.
//! - A view may end at the end of the file inside a page: that page reads
//!   as zero past the end. A view past the end of the section fails.
//! - Copy-on-write views (`PAGE_WRITECOPY`) take `PAGE_WRITECOPY` for
//!   writing; a written page becomes private (`PAGE_READWRITE`). A view
//!   mapped without execute cannot be made executable.
//! - Writes through `WriteFile` are seen by shared views and by unwritten
//!   pages of copy-on-write views, and `ReadFile` sees writes through a
//!   shared view: the cache and the views share pages.
//! - Unmapping with `MEM_PRESERVE_PLACEHOLDER` leaves one placeholder of
//!   the view's size. Adjacent placeholders can be coalesced.
//! - A file with a mapped view cannot be truncated below the view
//!   (`ERROR_USER_MAPPED_FILE`); it can grow, be renamed and be deleted.

use core::ffi::c_void;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FlushViewOfFile, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile3, PAGE_PROTECTION_FLAGS, UnmapViewOfFile2,
    VIRTUAL_FREE_TYPE, VirtualFree,
};
use windows::Win32::System::Threading::GetCurrentProcess;

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

/// `MEM_COALESCE_PLACEHOLDERS` for `VirtualFree`.
const MEM_COALESCE_PLACEHOLDERS: u32 = 0x1;
/// `MEM_RELEASE`.
const MEM_RELEASE: u32 = 0x8000;

fn last_error() -> Errno {
    Errno::from(windows::core::Error::from_thread())
}

/// A section of the whole file `h` (its current size) with maximum page
/// protection `protect`.
pub fn create(h: HANDLE, protect: PAGE_PROTECTION_FLAGS) -> SysResult<OwnedHandle> {
    // SAFETY: `h` is a live file handle of the caller; an unnamed section
    // with default security. Size 0 is the file's size.
    let s = unsafe { CreateFileMappingW(h, None, protect, 0, 0, None) }.map_err(Errno::from)?;
    Ok(OwnedHandle::from_raw(s))
}

/// Maps `size` bytes of `section` from `offset` into the placeholder at
/// `addr`, which spans `size` rounded up to pages.
pub fn map(
    section: &OwnedHandle,
    addr: usize,
    offset: u64,
    size: usize,
    protect: PAGE_PROTECTION_FLAGS,
) -> SysResult<()> {
    // SAFETY: the placeholder at `addr` is owned by lx's address-space
    // table; replacing it does not affect other memory.
    let v = unsafe {
        MapViewOfFile3(
            section.raw(),
            Some(GetCurrentProcess()),
            Some(addr as *const c_void),
            offset,
            size,
            MEM_REPLACE_PLACEHOLDER,
            protect.0,
            None,
        )
    };
    if v.Value.is_null() {
        return Err(last_error());
    }
    debug_assert_eq!(v.Value as usize, addr);
    Ok(())
}

/// Unmaps the view at `addr`, leaving a placeholder of its size.
pub fn unmap(addr: usize) -> SysResult<()> {
    // SAFETY: `addr` is the base of a view lx mapped; nothing else refers
    // to the view's pages once lx unmaps it.
    unsafe {
        UnmapViewOfFile2(
            GetCurrentProcess(),
            MEMORY_MAPPED_VIEW_ADDRESS {
                Value: addr as *mut c_void,
            },
            MEM_PRESERVE_PLACEHOLDER,
        )
    }
    .map_err(Errno::from)
}

/// Starts writing the dirty pages of `[addr, addr+len)` of a view to the
/// file.
pub fn flush(addr: usize, len: usize) -> SysResult<()> {
    // SAFETY: the range lies in a view lx mapped.
    unsafe { FlushViewOfFile(addr as *const c_void, len) }.map_err(Errno::from)
}

/// Coalesces the adjacent placeholders that make up exactly
/// `[addr, addr+len)` into one.
pub fn coalesce(addr: usize, len: usize) -> SysResult<()> {
    // SAFETY: the range consists of whole placeholders owned by lx.
    unsafe {
        VirtualFree(
            addr as *mut c_void,
            len,
            VIRTUAL_FREE_TYPE(MEM_RELEASE | MEM_COALESCE_PLACEHOLDERS),
        )
    }
    .map_err(Errno::from)
}

//! Memory exports.
//!
//! # Safety
//!
//! These functions change the address space. As on Linux, unmapping or
//! protecting memory that other code still uses is the caller's
//! responsibility; lx refuses to replace or unmap memory it does not own
//! (see [`crate::mem`]).

use core::ffi::c_void;

use super::guard;
use crate::errno::SysResult;
use crate::mem;

pub(crate) fn sys_mmap(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: i64,
) -> SysResult<i64> {
    mem::mmap(addr, len, prot, flags, fd, offset).map(|a| a as i64)
}

/// `void* mmap(void*, size_t, int, int, int, off_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> *mut c_void {
    guard(|| {
        sys_mmap(addr as usize, len, prot as u32, flags as u32, fd, offset)
            .map(|a| core::ptr::with_exposed_provenance_mut(a as usize))
    })
}

/// `mmap64`, the same as `mmap` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap64(
    addr: *mut c_void,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> *mut c_void {
    // SAFETY: same contract.
    unsafe { mmap(addr, len, prot, flags, fd, offset) }
}

/// `int munmap(void*, size_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn munmap(addr: *mut c_void, len: usize) -> i32 {
    guard(|| mem::munmap(addr as usize, len).map(|()| 0))
}

/// `int mprotect(void*, size_t, int)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32 {
    guard(|| mem::mprotect(addr as usize, len, prot as u32).map(|()| 0))
}

/// `int madvise(void*, size_t, int)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32 {
    guard(|| mem::madvise(addr as usize, len, advice).map(|()| 0))
}

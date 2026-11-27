//! The C exports, with bionic names and Linux semantics.
//!
//! Every export is `extern "C"`, runs its body inside [`guard`] (which
//! catches panics so none crosses the boundary) and reports failure the way
//! the bionic function does: usually `-1` with `errno` set.
//!
//! Pointers from callers are used as the Linux interface defines them. A
//! NULL pointer where the kernel would report `EFAULT` gives `EFAULT`; a
//! non-NULL pointer to inaccessible memory gives `EFAULT` for buffers handed
//! to Windows I/O and an access violation (SIGSEGV once signals exist) for
//! structures lx reads or writes itself.
//!
//! # Calling convention of variadic functions
//!
//! `open`, `openat`, `fcntl` and `syscall` are variadic in C. Stable Rust
//! cannot define variadic functions, so they are defined with a fixed
//! number of integer parameters. This is compatible with variadic callers
//! for integer and pointer arguments: on Windows x64, variadic and fixed
//! integer arguments use the same registers and stack slots; on Windows
//! ARM64, a variadic call passes the first eight in `x0`-`x7` as a fixed
//! call does. Parameters the caller did not pass hold unspecified values and
//! are ignored by the operation.

mod errno;
mod fs;
mod host;
mod mem;
mod syscall;
mod syscall_ext;
mod time;

use core::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::errno::{Errno, SysResult, set_errno};

/// The failure value of an export's return type.
pub(crate) trait Failure: Copy {
    const FAILED: Self;
}

impl Failure for i32 {
    const FAILED: Self = -1;
}

impl Failure for i64 {
    const FAILED: Self = -1;
}

impl Failure for isize {
    const FAILED: Self = -1;
}

impl Failure for *mut c_void {
    /// `MAP_FAILED`.
    const FAILED: Self = core::ptr::without_provenance_mut(usize::MAX);
}

/// Runs an export body: catches panics, stores the error in `errno` and
/// returns the failure value.
pub(crate) fn guard<T: Failure>(f: impl FnOnce() -> SysResult<T>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_errno(e);
            T::FAILED
        }
        Err(_) => {
            // A panic is an lx bug; the caller gets a generic I/O error
            // rather than an unwind through C frames.
            set_errno(Errno::EIO);
            T::FAILED
        }
    }
}

/// Like [`guard`] for functions that return the error number instead of
/// setting `errno` (`clock_nanosleep`, `pthread_*` style): 0 or the error.
pub(crate) fn guard_errno(f: impl FnOnce() -> SysResult<()>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => e.0,
        Err(_) => Errno::EIO.0,
    }
}

/// Borrows a NUL-terminated path argument.
///
/// # Safety
///
/// `p` is NULL or points to a NUL-terminated string that stays valid and
/// unchanged during the call.
pub(crate) unsafe fn path_arg<'a>(p: *const c_char) -> SysResult<&'a [u8]> {
    if p.is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: guaranteed by the caller.
    Ok(unsafe { CStr::from_ptr(p) }.to_bytes())
}

/// Reads a structure argument.
///
/// # Safety
///
/// `p` is NULL or valid for reading a `T`.
pub(crate) unsafe fn read_arg<T: Copy>(p: *const T) -> SysResult<T> {
    if p.is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: guaranteed by the caller; unaligned reads are allowed because
    // C callers may pass packed buffers.
    Ok(unsafe { p.read_unaligned() })
}

/// Writes a structure result.
///
/// # Safety
///
/// `p` is NULL or valid for writing a `T`.
pub(crate) unsafe fn write_arg<T>(p: *mut T, v: T) -> SysResult<()> {
    if p.is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { p.write_unaligned(v) };
    Ok(())
}

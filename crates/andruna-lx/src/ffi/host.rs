//! Exports for the host process that sets lx up (not part of bionic).

use core::ffi::c_char;
use std::path::Path;

use super::{guard, path_arg};
use crate::errno::Errno;
use crate::fs::mount;

/// `int andruna_lx_mount(const char* linux_dir, const char* windows_dir)`:
/// maps the absolute Linux directory `linux_dir` to the absolute Windows
/// directory `windows_dir` (both UTF-8). Returns 0, or -1 with `errno`
/// (`EINVAL` for a relative or non-UTF-8 path, `EFAULT` for NULL).
///
/// # Safety
///
/// Both arguments are NULL or NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn andruna_lx_mount(
    linux_dir: *const c_char,
    windows_dir: *const c_char,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let l = unsafe { path_arg(linux_dir) }?;
        // SAFETY: forwarded from this function's contract.
        let w = unsafe { path_arg(windows_dir) }?;
        let l = core::str::from_utf8(l).map_err(|_| Errno::EINVAL)?;
        let w = core::str::from_utf8(w).map_err(|_| Errno::EINVAL)?;
        mount::mount(l, Path::new(w)).map(|()| 0)
    })
}

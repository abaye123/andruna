//! File and descriptor exports.
//!
//! # Safety
//!
//! For every function here: path arguments are NULL or NUL-terminated
//! strings; `buf`/`count` pairs describe the caller's buffer (Windows
//! validates it, an inaccessible buffer gives `EFAULT`); `struct stat`
//! pointers are NULL or valid for writing one bionic `struct stat` of the
//! build's architecture.

use core::ffi::{c_char, c_void};

use super::{guard, path_arg, write_arg};
use crate::errno::SysResult;
use crate::fd::FDS;
use crate::fs;
use crate::linux::fcntl::{AT_FDCWD, AT_SYMLINK_NOFOLLOW, O_CREAT, O_TRUNC, O_WRONLY};
use crate::linux::stat::Stat;
use crate::util::UserBuf;

pub(crate) unsafe fn sys_openat(
    dirfd: i32,
    path: *const c_char,
    flags: u32,
    mode: u32,
) -> SysResult<i64> {
    // SAFETY: forwarded from the export's contract.
    let p = unsafe { path_arg(path) }?;
    fs::openat(dirfd, p, flags, mode).map(i64::from)
}

pub(crate) unsafe fn sys_read(fd: i32, buf: *mut c_void, count: usize) -> SysResult<i64> {
    // SAFETY: the caller's buffer, which lx does not reference otherwise.
    let b = unsafe { UserBuf::new(buf.cast(), count) };
    fs::read(fd, b).map(|n| n as i64)
}

pub(crate) unsafe fn sys_write(fd: i32, buf: *const c_void, count: usize) -> SysResult<i64> {
    // SAFETY: as in `sys_read`; Windows only reads from it.
    let b = unsafe { UserBuf::new(buf as *mut u8, count) };
    fs::write(fd, b).map(|n| n as i64)
}

pub(crate) unsafe fn sys_pread(
    fd: i32,
    buf: *mut c_void,
    count: usize,
    offset: i64,
) -> SysResult<i64> {
    // SAFETY: as in `sys_read`.
    let b = unsafe { UserBuf::new(buf.cast(), count) };
    fs::pread(fd, b, offset).map(|n| n as i64)
}

pub(crate) unsafe fn sys_pwrite(
    fd: i32,
    buf: *const c_void,
    count: usize,
    offset: i64,
) -> SysResult<i64> {
    // SAFETY: as in `sys_write`.
    let b = unsafe { UserBuf::new(buf as *mut u8, count) };
    fs::pwrite(fd, b, offset).map(|n| n as i64)
}

pub(crate) unsafe fn sys_fstat(fd: i32, buf: *mut Stat) -> SysResult<i64> {
    let st = fs::fstat(fd)?;
    // SAFETY: forwarded from the export's contract.
    unsafe { write_arg(buf, Stat::from(&st)) }?;
    Ok(0)
}

pub(crate) unsafe fn sys_fstatat(
    dirfd: i32,
    path: *const c_char,
    buf: *mut Stat,
    flags: u32,
) -> SysResult<i64> {
    // SAFETY: forwarded from the export's contract.
    let p = unsafe { path_arg(path) }?;
    let st = fs::fstatat(dirfd, p, flags)?;
    // SAFETY: forwarded from the export's contract.
    unsafe { write_arg(buf, Stat::from(&st)) }?;
    Ok(0)
}

/// `int open(const char*, int, ...)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: i32, mode: u32) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_openat(AT_FDCWD, path, flags as u32, mode) }.map(|fd| fd as i32))
}

/// `open64`, the same as `open` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open64(path: *const c_char, flags: i32, mode: u32) -> i32 {
    // SAFETY: same contract.
    unsafe { open(path, flags, mode) }
}

/// `int openat(int, const char*, int, ...)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat(dirfd: i32, path: *const c_char, flags: i32, mode: u32) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_openat(dirfd, path, flags as u32, mode) }.map(|fd| fd as i32))
}

/// `openat64`, the same as `openat` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat64(dirfd: i32, path: *const c_char, flags: i32, mode: u32) -> i32 {
    // SAFETY: same contract.
    unsafe { openat(dirfd, path, flags, mode) }
}

/// `int creat(const char*, mode_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn creat(path: *const c_char, mode: u32) -> i32 {
    // SAFETY: same contract.
    unsafe { open(path, (O_CREAT | O_WRONLY | O_TRUNC) as i32, mode) }
}

/// `int close(int)`.
#[unsafe(no_mangle)]
pub extern "C" fn close(fd: i32) -> i32 {
    guard(|| fs::close(fd).map(|()| 0))
}

/// `ssize_t read(int, void*, size_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: i32, buf: *mut c_void, count: usize) -> isize {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_read(fd, buf, count) }.map(|n| n as isize))
}

/// `ssize_t write(int, const void*, size_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: i32, buf: *const c_void, count: usize) -> isize {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_write(fd, buf, count) }.map(|n| n as isize))
}

/// `ssize_t pread64(int, void*, size_t, off64_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pread64(fd: i32, buf: *mut c_void, count: usize, offset: i64) -> isize {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_pread(fd, buf, count, offset) }.map(|n| n as isize))
}

/// `pread`, the same as `pread64` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pread(fd: i32, buf: *mut c_void, count: usize, offset: i64) -> isize {
    // SAFETY: same contract.
    unsafe { pread64(fd, buf, count, offset) }
}

/// `ssize_t pwrite64(int, const void*, size_t, off64_t)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pwrite64(fd: i32, buf: *const c_void, count: usize, offset: i64) -> isize {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_pwrite(fd, buf, count, offset) }.map(|n| n as isize))
}

/// `pwrite`, the same as `pwrite64` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pwrite(fd: i32, buf: *const c_void, count: usize, offset: i64) -> isize {
    // SAFETY: same contract.
    unsafe { pwrite64(fd, buf, count, offset) }
}

/// `off_t lseek(int, off_t, int)`.
#[unsafe(no_mangle)]
pub extern "C" fn lseek(fd: i32, offset: i64, whence: i32) -> i64 {
    guard(|| fs::lseek(fd, offset, whence))
}

/// `lseek64`, the same as `lseek` on LP64.
#[unsafe(no_mangle)]
pub extern "C" fn lseek64(fd: i32, offset: i64, whence: i32) -> i64 {
    lseek(fd, offset, whence)
}

/// `int fstat(int, struct stat*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat(fd: i32, buf: *mut Stat) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_fstat(fd, buf) }.map(|_| 0))
}

/// `fstat64`, the same as `fstat` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat64(fd: i32, buf: *mut Stat) -> i32 {
    // SAFETY: same contract.
    unsafe { fstat(fd, buf) }
}

/// `int fstatat(int, const char*, struct stat*, int)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatat(
    dirfd: i32,
    path: *const c_char,
    buf: *mut Stat,
    flags: i32,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_fstatat(dirfd, path, buf, flags as u32) }.map(|_| 0))
}

/// `fstatat64`, the same as `fstatat` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatat64(
    dirfd: i32,
    path: *const c_char,
    buf: *mut Stat,
    flags: i32,
) -> i32 {
    // SAFETY: same contract.
    unsafe { fstatat(dirfd, path, buf, flags) }
}

/// `int stat(const char*, struct stat*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut Stat) -> i32 {
    // SAFETY: same contract.
    unsafe { fstatat(AT_FDCWD, path, buf, 0) }
}

/// `stat64`, the same as `stat` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat64(path: *const c_char, buf: *mut Stat) -> i32 {
    // SAFETY: same contract.
    unsafe { stat(path, buf) }
}

/// `int lstat(const char*, struct stat*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut Stat) -> i32 {
    // SAFETY: same contract.
    unsafe { fstatat(AT_FDCWD, path, buf, AT_SYMLINK_NOFOLLOW as i32) }
}

/// `lstat64`, the same as `lstat` on LP64.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat64(path: *const c_char, buf: *mut Stat) -> i32 {
    // SAFETY: same contract.
    unsafe { lstat(path, buf) }
}

/// `int dup(int)`.
#[unsafe(no_mangle)]
pub extern "C" fn dup(fd: i32) -> i32 {
    guard(|| FDS.dup_min(fd, 0, false))
}

/// `int dup2(int, int)`.
#[unsafe(no_mangle)]
pub extern "C" fn dup2(old: i32, new: i32) -> i32 {
    guard(|| FDS.dup2(old, new))
}

/// `int dup3(int, int, int)`.
#[unsafe(no_mangle)]
pub extern "C" fn dup3(old: i32, new: i32, flags: i32) -> i32 {
    guard(|| FDS.dup3(old, new, flags as u32))
}

/// `int fcntl(int, int, ...)`. The commands supported this round take an
/// integer argument or none.
#[unsafe(no_mangle)]
pub extern "C" fn fcntl(fd: i32, cmd: i32, arg: usize) -> i32 {
    guard(|| FDS.fcntl(fd, cmd, arg as u64))
}

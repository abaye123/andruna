//! Time, scheduling and identity exports.
//!
//! # Safety
//!
//! `struct timespec` pointers are NULL or valid for reading (requests) or
//! writing (results) one bionic `struct timespec`.

use super::{guard, guard_errno, read_arg, write_arg};
use crate::errno::{Errno, SysResult};
use crate::linux::time::Timespec;
use crate::{process, time};

pub(crate) unsafe fn sys_clock_gettime(clk: i32, tp: *mut Timespec) -> SysResult<i64> {
    let t = time::clock_gettime(clk)?;
    // SAFETY: forwarded from the export's contract.
    unsafe { write_arg(tp, t) }?;
    Ok(0)
}

pub(crate) unsafe fn sys_clock_getres(clk: i32, res: *mut Timespec) -> SysResult<i64> {
    let t = time::clock_getres(clk)?;
    // A NULL result pointer is allowed.
    if !res.is_null() {
        // SAFETY: forwarded from the export's contract.
        unsafe { write_arg(res, t) }?;
    }
    Ok(0)
}

pub(crate) unsafe fn sys_nanosleep(req: *const Timespec, rem: *mut Timespec) -> SysResult<i64> {
    // SAFETY: forwarded from the export's contract.
    let r = unsafe { read_arg(req) }?;
    // SAFETY: as above.
    unsafe { finish_sleep(time::nanosleep(&r), rem, true) }.map(|()| 0)
}

pub(crate) unsafe fn sys_clock_nanosleep(
    clk: i32,
    flags: i32,
    req: *const Timespec,
    rem: *mut Timespec,
) -> SysResult<()> {
    // SAFETY: forwarded from the export's contract.
    let r = unsafe { read_arg(req) }?;
    let relative = flags & crate::linux::time::TIMER_ABSTIME == 0;
    // SAFETY: as above.
    unsafe { finish_sleep(time::clock_nanosleep(clk, flags, &r), rem, relative) }
}

/// The result of a sleep: on `EINTR` a relative sleep writes the time left
/// to `rem` (when it is not NULL).
///
/// # Safety
///
/// `rem` is NULL or valid for writing a `timespec`.
unsafe fn finish_sleep(
    r: Result<(), time::Interrupted>,
    rem: *mut Timespec,
    relative: bool,
) -> SysResult<()> {
    match r {
        Ok(()) => Ok(()),
        Err(time::Interrupted::Signal(left)) => {
            if relative && !rem.is_null() {
                // SAFETY: contract.
                unsafe { write_arg(rem, left) }?;
            }
            Err(Errno::EINTR)
        }
        Err(time::Interrupted::Error(e)) => Err(e),
    }
}

/// `int clock_gettime(clockid_t, struct timespec*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clk: i32, tp: *mut Timespec) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_clock_gettime(clk, tp) }.map(|_| 0))
}

/// `int clock_getres(clockid_t, struct timespec*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_getres(clk: i32, res: *mut Timespec) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_clock_getres(clk, res) }.map(|_| 0))
}

/// `int nanosleep(const struct timespec*, struct timespec*)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { sys_nanosleep(req, rem) }.map(|_| 0))
}

/// `int clock_nanosleep(clockid_t, int, const struct timespec*, struct
/// timespec*)`: returns 0 or the error number (it does not set `errno`).
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_nanosleep(
    clk: i32,
    flags: i32,
    req: *const Timespec,
    rem: *mut Timespec,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    guard_errno(|| unsafe { sys_clock_nanosleep(clk, flags, req, rem) })
}

/// `int sched_yield(void)`.
#[unsafe(no_mangle)]
pub extern "C" fn sched_yield() -> i32 {
    guard(|| {
        process::sched_yield();
        Ok(0)
    })
}

/// `pid_t getpid(void)`.
#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> i32 {
    guard(|| Ok(process::getpid()))
}

/// `pid_t gettid(void)`.
#[unsafe(no_mangle)]
pub extern "C" fn gettid() -> i32 {
    guard(|| Ok(process::gettid()))
}

//! Clocks and sleeping.
//!
//! All clocks are returned in 100 ns units, the resolution Windows offers.

use std::cell::OnceCell;

use windows::Win32::Foundation::{FILETIME, WAIT_OBJECT_0};
use windows::Win32::System::SystemInformation::{
    GetSystemTimeAdjustment, GetSystemTimeAsFileTime, GetSystemTimePreciseAsFileTime,
};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::System::WindowsProgramming::{
    QueryInterruptTimePrecise, QueryUnbiasedInterruptTime, QueryUnbiasedInterruptTimePrecise,
};
use windows::core::PCWSTR;

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

/// 100 ns intervals between 1601-01-01 and 1970-01-01.
pub const UNIX_EPOCH_AS_FILETIME: i64 = 116_444_736_000_000_000;

pub fn filetime_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Wall-clock time in 100 ns units since the Unix epoch.
pub fn realtime(precise: bool) -> i64 {
    // SAFETY: no arguments; returns a value.
    let ft = unsafe {
        if precise {
            GetSystemTimePreciseAsFileTime()
        } else {
            GetSystemTimeAsFileTime()
        }
    };
    filetime_to_u64(ft) as i64 - UNIX_EPOCH_AS_FILETIME
}

/// Time since boot excluding time spent suspended (Linux `CLOCK_MONOTONIC`).
pub fn unbiased_interrupt_time(precise: bool) -> u64 {
    if precise {
        // SAFETY: no arguments; returns a value.
        unsafe { QueryUnbiasedInterruptTimePrecise() }
    } else {
        let mut t = 0u64;
        // SAFETY: `t` is a valid out pointer. The call cannot fail with a
        // valid pointer.
        let _ = unsafe { QueryUnbiasedInterruptTime(&mut t) };
        t
    }
}

/// Time since boot including time spent suspended (Linux `CLOCK_BOOTTIME`).
pub fn interrupt_time() -> u64 {
    // SAFETY: no arguments; returns a value.
    unsafe { QueryInterruptTimePrecise() }
}

/// The interval at which the coarse clocks advance, in 100 ns units.
pub fn clock_tick() -> u32 {
    let (mut adj, mut inc, mut disabled) = (0u32, 0u32, Default::default());
    // SAFETY: all three out pointers are valid locals.
    match unsafe { GetSystemTimeAdjustment(&mut adj, &mut inc, &mut disabled) } {
        Ok(()) if inc != 0 => inc,
        // The default clock interrupt period, 15.625 ms.
        _ => 156_250,
    }
}

thread_local! {
    static TIMER: OnceCell<Option<OwnedHandle>> = const { OnceCell::new() };
}

fn create_timer() -> Option<OwnedHandle> {
    // SAFETY: no security attributes, no name; the returned handle is owned
    // by the caller.
    unsafe {
        CreateWaitableTimerExW(
            None,
            PCWSTR::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS.0,
        )
    }
    .ok()
    .map(OwnedHandle::from_raw)
}

/// Sleeps for `hundred_ns` 100 ns units on a high-resolution waitable timer
/// (one per thread, created on first use). With `wake`, the sleep also ends
/// when that event is set; returns whether it did.
pub fn sleep(hundred_ns: u64, wake: Option<windows::Win32::Foundation::HANDLE>) -> SysResult<bool> {
    use windows::Win32::System::Threading::WaitForMultipleObjects;
    if hundred_ns == 0 {
        return Ok(false);
    }
    TIMER.with(|cell| {
        let timer = cell
            .get_or_init(create_timer)
            .as_ref()
            .ok_or(Errno::ENOMEM)?;
        // Negative due time: relative.
        let due = -(hundred_ns.min(i64::MAX as u64) as i64);
        // SAFETY: `timer` is a live timer handle and `due` a valid pointer;
        // no completion routine.
        unsafe { SetWaitableTimer(timer.raw(), &due, 0, None, None, false) }
            .map_err(Errno::from)?;
        let r = match wake {
            // SAFETY: waits on a live handle.
            None => unsafe { WaitForSingleObject(timer.raw(), INFINITE) },
            // SAFETY: waits on two live handles.
            Some(w) => unsafe { WaitForMultipleObjects(&[timer.raw(), w], false, INFINITE) },
        };
        match r.0.wrapping_sub(WAIT_OBJECT_0.0) {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Errno::EIO),
        }
    })
}

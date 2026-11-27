//! Clock sources.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTimePrecise;

/// Time since boot excluding suspend, in 100 ns units (andruna-lx's
/// `CLOCK_MONOTONIC` source).
pub fn unbiased_interrupt_time_100ns() -> u64 {
    // SAFETY: no arguments; returns a value.
    unsafe { QueryUnbiasedInterruptTimePrecise() }
}

/// The performance counter and its frequency.
pub fn qpc() -> (i64, i64) {
    let (mut c, mut f) = (0i64, 0i64);
    // SAFETY: valid out pointers; both calls cannot fail on Windows XP and
    // later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    (c, f.max(1))
}

/// The millisecond tick count (the base of message times).
pub fn tick_count() -> u32 {
    // SAFETY: no arguments; returns a value.
    unsafe { GetTickCount() }
}

//! The uptime clock of input and vsync timestamps.
//!
//! Android stamps input events and vsync with `CLOCK_MONOTONIC`
//! (`SystemClock.uptimeMillis` in milliseconds). andruna-lx implements
//! `CLOCK_MONOTONIC` with `QueryUnbiasedInterruptTimePrecise`, so this
//! module uses the same source; timestamps compare with what the app reads
//! from `clock_gettime`.
//!
//! Windows timestamps come in two other bases: the performance counter
//! (pointer input, compositor frame statistics) and the millisecond tick
//! count (`GetMessageTime`). They are converted by their distance from
//! now, so the result is on the uptime clock even across sleep.

use crate::sys::time as os;

/// `CLOCK_MONOTONIC` now, in nanoseconds.
pub fn uptime_ns() -> i64 {
    os::unbiased_interrupt_time_100ns() as i64 * 100
}

/// A performance counter value converted to the uptime clock.
pub fn qpc_to_uptime_ns(qpc: i64) -> i64 {
    let (now_qpc, freq) = os::qpc();
    let now = uptime_ns();
    let ago = (now_qpc - qpc) as i128 * 1_000_000_000 / freq as i128;
    now - ago as i64
}

/// Converts a performance counter duration to nanoseconds.
pub fn qpc_duration_ns(ticks: i64) -> i64 {
    let (_, freq) = os::qpc();
    (ticks as i128 * 1_000_000_000 / freq as i128) as i64
}

/// A message time (`GetMessageTime`, milliseconds of the tick count)
/// converted to the uptime clock.
pub fn tick_to_uptime_ns(tick_ms: u32) -> i64 {
    let ago = os::tick_count().wrapping_sub(tick_ms) as i64;
    uptime_ns() - ago * 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_agree_with_now() {
        let (q, _) = os::qpc();
        let a = uptime_ns();
        let b = qpc_to_uptime_ns(q);
        assert!((b - a).abs() < 2_000_000, "{a} {b}");
        let c = tick_to_uptime_ns(os::tick_count());
        // The tick count has a 10-16 ms resolution.
        assert!((c - a).abs() < 40_000_000, "{a} {c}");
        let (_, f) = os::qpc();
        assert_eq!(qpc_duration_ns(f), 1_000_000_000);
        assert!(uptime_ns() >= a);
    }
}

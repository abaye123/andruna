//! Time types and clock ids (`linux/time.h`, bionic `bits/timespec.h`).

use core::mem::{offset_of, size_of};

/// `struct timespec` on LP64 bionic: `time_t tv_sec; long tv_nsec;`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

const _: () = {
    assert!(size_of::<Timespec>() == 16);
    assert!(offset_of!(Timespec, tv_sec) == 0);
    assert!(offset_of!(Timespec, tv_nsec) == 8);
};

pub const NSEC_PER_SEC: i64 = 1_000_000_000;

impl Timespec {
    /// Whether `tv_nsec` is in `[0, 1e9)` and `tv_sec` is not negative, the
    /// check Linux applies to sleep and timeout arguments.
    pub fn is_valid_duration(&self) -> bool {
        self.tv_sec >= 0 && (0..NSEC_PER_SEC).contains(&self.tv_nsec)
    }

    /// Whether `tv_nsec` is in `[0, 1e9)` (absolute times may have any
    /// `tv_sec`).
    pub fn has_valid_nsec(&self) -> bool {
        (0..NSEC_PER_SEC).contains(&self.tv_nsec)
    }

    pub fn from_nanos(ns: i128) -> Self {
        let sec = ns.div_euclid(NSEC_PER_SEC as i128);
        let nsec = ns.rem_euclid(NSEC_PER_SEC as i128);
        Self {
            tv_sec: sec.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
            tv_nsec: nsec as i64,
        }
    }

    pub fn as_nanos(&self) -> i128 {
        self.tv_sec as i128 * NSEC_PER_SEC as i128 + self.tv_nsec as i128
    }
}

pub const CLOCK_REALTIME: i32 = 0;
pub const CLOCK_MONOTONIC: i32 = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: i32 = 2;
pub const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
pub const CLOCK_MONOTONIC_RAW: i32 = 4;
pub const CLOCK_REALTIME_COARSE: i32 = 5;
pub const CLOCK_MONOTONIC_COARSE: i32 = 6;
pub const CLOCK_BOOTTIME: i32 = 7;
pub const CLOCK_REALTIME_ALARM: i32 = 8;
pub const CLOCK_BOOTTIME_ALARM: i32 = 9;

pub const TIMER_ABSTIME: i32 = 1;

/// CPU-time clock ids for a given process or thread, as built by
/// `clock_getcpuclockid` and `pthread_getcpuclockid` (`linux/posix-timers.h`):
/// `(~id << 3) | per_thread << 2 | type`.
pub const CPUCLOCK_PROF: i32 = 0;
pub const CPUCLOCK_VIRT: i32 = 1;
pub const CPUCLOCK_SCHED: i32 = 2;
pub const CPUCLOCK_CLOCK_MASK: i32 = 3;
pub const CPUCLOCK_PERTHREAD_MASK: i32 = 4;

/// Builds the clock id `pthread_getcpuclockid` returns for thread `tid`.
pub const fn make_thread_cpuclock(tid: i32, kind: i32) -> i32 {
    ((!tid) << 3) | CPUCLOCK_PERTHREAD_MASK | kind
}

/// Builds the clock id `clock_getcpuclockid` returns for process `pid`.
pub const fn make_process_cpuclock(pid: i32, kind: i32) -> i32 {
    ((!pid) << 3) | kind
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_round_trip() {
        for ns in [0i128, 1, 999_999_999, 1_000_000_000, -1, -1_000_000_001] {
            assert_eq!(Timespec::from_nanos(ns).as_nanos(), ns);
            assert!(Timespec::from_nanos(ns).has_valid_nsec());
        }
    }

    #[test]
    fn validity() {
        assert!(
            Timespec {
                tv_sec: 0,
                tv_nsec: 0
            }
            .is_valid_duration()
        );
        assert!(
            !Timespec {
                tv_sec: -1,
                tv_nsec: 0
            }
            .is_valid_duration()
        );
        assert!(
            !Timespec {
                tv_sec: 0,
                tv_nsec: NSEC_PER_SEC
            }
            .is_valid_duration()
        );
    }
}

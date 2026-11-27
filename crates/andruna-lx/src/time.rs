//! Clocks and sleeping: `clock_gettime`, `clock_getres`, `nanosleep`,
//! `clock_nanosleep`.
//!
//! | Linux clock | Windows source |
//! | --- | --- |
//! | `CLOCK_REALTIME` (and `_ALARM`) | `GetSystemTimePreciseAsFileTime` |
//! | `CLOCK_REALTIME_COARSE` | `GetSystemTimeAsFileTime` |
//! | `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_RAW` | `QueryUnbiasedInterruptTimePrecise` (stops while suspended, as Linux) |
//! | `CLOCK_MONOTONIC_COARSE` | `QueryUnbiasedInterruptTime` |
//! | `CLOCK_BOOTTIME` (and `_ALARM`) | `QueryInterruptTimePrecise` (counts suspend) |
//! | process / thread CPU time | `GetProcessTimes` / `GetThreadTimes` |
//!
//! Resolution is 100 ns (Linux high-resolution clocks report 1 ns), which
//! `clock_getres` reports honestly. CPU-time clocks advance in scheduler
//! quanta on Windows.
//!
//! Sleeps never end early and never return `EINTR` until signals exist
//! ([`crate::signal`]).

use crate::errno::{Errno, SysResult};
use crate::linux::time::*;
use crate::process;
use crate::sys::{thread, time as os};

const HUNDRED_NS: i128 = 100;

fn from_100ns(v: i128) -> Timespec {
    Timespec::from_nanos(v * HUNDRED_NS)
}

/// The CPU-time clock a negative clock id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CpuClock {
    /// `pid` `None`: this process.
    Process {
        pid: Option<u32>,
        kind: i32,
    },
    Thread {
        tid: Option<u32>,
        kind: i32,
    },
}

fn cpu_clock(clk: i32) -> SysResult<CpuClock> {
    match clk {
        CLOCK_PROCESS_CPUTIME_ID => {
            return Ok(CpuClock::Process {
                pid: None,
                kind: CPUCLOCK_SCHED,
            });
        }
        CLOCK_THREAD_CPUTIME_ID => {
            return Ok(CpuClock::Thread {
                tid: None,
                kind: CPUCLOCK_SCHED,
            });
        }
        c if c >= 0 => return Err(Errno::EINVAL),
        _ => {}
    }
    let kind = clk & CPUCLOCK_CLOCK_MASK;
    if kind == CPUCLOCK_CLOCK_MASK {
        return Err(Errno::EINVAL);
    }
    let id = !(clk >> 3);
    if clk & CPUCLOCK_PERTHREAD_MASK != 0 {
        let tid = if id == process::gettid() {
            None
        } else {
            Some(id as u32)
        };
        Ok(CpuClock::Thread { tid, kind })
    } else if id == 0 || id == process::getpid() {
        Ok(CpuClock::Process { pid: None, kind })
    } else if id > 0 && thread::process_exists(id as u32) {
        // Another process: its CPU time, as Linux allows.
        Ok(CpuClock::Process {
            pid: Some(id as u32),
            kind,
        })
    } else {
        Err(Errno::EINVAL)
    }
}

fn cpu_time(c: CpuClock) -> SysResult<Timespec> {
    let (t, kind) = match c {
        CpuClock::Process { pid: None, kind } => (thread::process_times()?, kind),
        CpuClock::Process { pid: Some(p), kind } => (thread::process_times_of(p)?, kind),
        CpuClock::Thread { tid, kind } => (thread::thread_times(tid)?, kind),
    };
    let v = if kind == CPUCLOCK_VIRT {
        t.user
    } else {
        t.user + t.kernel
    };
    Ok(from_100ns(i128::from(v)))
}

/// `clock_gettime`.
pub fn clock_gettime(clk: i32) -> SysResult<Timespec> {
    Ok(match clk {
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM => from_100ns(os::realtime(true).into()),
        CLOCK_REALTIME_COARSE => from_100ns(os::realtime(false).into()),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW => {
            from_100ns(os::unbiased_interrupt_time(true).into())
        }
        CLOCK_MONOTONIC_COARSE => from_100ns(os::unbiased_interrupt_time(false).into()),
        CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => from_100ns(os::interrupt_time().into()),
        _ => return cpu_time(cpu_clock(clk)?),
    })
}

/// `clock_getres`.
pub fn clock_getres(clk: i32) -> SysResult<Timespec> {
    match clk {
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => Ok(from_100ns(os::clock_tick().into())),
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM | CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW
        | CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => Ok(from_100ns(1)),
        _ => {
            cpu_clock(clk)?;
            Ok(from_100ns(1))
        }
    }
}

/// Monotonic time in nanoseconds.
pub fn monotonic_ns() -> i128 {
    i128::from(os::unbiased_interrupt_time(true)) * HUNDRED_NS
}

/// Nanoseconds left until `deadline` on clock `clk` (realtime, monotonic
/// or boottime).
fn remaining_ns(clk: i32, deadline: i128) -> SysResult<i128> {
    Ok(deadline - clock_gettime(clk)?.as_nanos())
}

/// Sleeps until `deadline` (ns) on clock `clk`, re-checking the clock after
/// each wait so the sleep never ends early.
fn sleep_until(clk: i32, deadline: i128) -> SysResult<()> {
    loop {
        let left = remaining_ns(clk, deadline)?;
        if left <= 0 {
            return Ok(());
        }
        let units = (left + HUNDRED_NS - 1) / HUNDRED_NS;
        os::sleep(units.min(i128::from(u64::MAX)) as u64)?;
    }
}

/// `nanosleep` (relative, on `CLOCK_MONOTONIC`).
pub fn nanosleep(req: &Timespec) -> SysResult<()> {
    clock_nanosleep(CLOCK_MONOTONIC, 0, req)
}

/// `clock_nanosleep`, returning the error instead of setting `errno` (the
/// export converts it as the interface requires).
pub fn clock_nanosleep(clk: i32, flags: i32, req: &Timespec) -> SysResult<()> {
    match clk {
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM | CLOCK_MONOTONIC | CLOCK_BOOTTIME
        | CLOCK_BOOTTIME_ALARM => {}
        CLOCK_THREAD_CPUTIME_ID => return Err(Errno::EINVAL),
        CLOCK_PROCESS_CPUTIME_ID => return Err(Errno::EOPNOTSUPP),
        _ => return Err(Errno::EINVAL),
    }
    if flags & TIMER_ABSTIME != 0 {
        if !req.has_valid_nsec() || req.tv_sec < 0 {
            return Err(Errno::EINVAL);
        }
        sleep_until(clk, req.as_nanos())
    } else {
        if !req.is_valid_duration() {
            return Err(Errno::EINVAL);
        }
        if req.as_nanos() == 0 {
            return Ok(());
        }
        let deadline = clock_gettime(clk)?.as_nanos() + req.as_nanos();
        sleep_until(clk, deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_advance() {
        for clk in [
            CLOCK_REALTIME,
            CLOCK_MONOTONIC,
            CLOCK_BOOTTIME,
            CLOCK_MONOTONIC_RAW,
            CLOCK_REALTIME_COARSE,
            CLOCK_MONOTONIC_COARSE,
        ] {
            let a = clock_gettime(clk).unwrap();
            assert!(a.has_valid_nsec());
            nanosleep(&Timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            })
            .unwrap();
            let b = clock_gettime(clk).unwrap();
            assert!(b.as_nanos() > a.as_nanos(), "clock {clk}");
        }
    }

    #[test]
    fn realtime_matches_system_time() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i128;
        let rt = clock_gettime(CLOCK_REALTIME).unwrap().as_nanos();
        assert!((rt - now).abs() < 1_000_000_000);
        // Boot time includes suspended time, so it is never behind.
        let mono = clock_gettime(CLOCK_MONOTONIC).unwrap().as_nanos();
        let boot = clock_gettime(CLOCK_BOOTTIME).unwrap().as_nanos();
        assert!(boot >= mono);
    }

    #[test]
    fn sleep_is_not_short() {
        let start = monotonic_ns();
        nanosleep(&Timespec {
            tv_sec: 0,
            tv_nsec: 1_500_000,
        })
        .unwrap();
        assert!(monotonic_ns() - start >= 1_500_000);

        let deadline = clock_gettime(CLOCK_MONOTONIC).unwrap().as_nanos() + 3_000_000;
        clock_nanosleep(
            CLOCK_MONOTONIC,
            TIMER_ABSTIME,
            &Timespec::from_nanos(deadline),
        )
        .unwrap();
        assert!(monotonic_ns() >= deadline);
        // A deadline in the past returns at once.
        clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME, &Timespec::default()).unwrap();
    }

    #[test]
    fn invalid_arguments() {
        assert_eq!(clock_gettime(12345), Err(Errno::EINVAL));
        assert_eq!(clock_getres(12345), Err(Errno::EINVAL));
        let bad = Timespec {
            tv_sec: 0,
            tv_nsec: NSEC_PER_SEC,
        };
        assert_eq!(nanosleep(&bad), Err(Errno::EINVAL));
        assert_eq!(
            nanosleep(&Timespec {
                tv_sec: -1,
                tv_nsec: 0
            }),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            clock_nanosleep(CLOCK_THREAD_CPUTIME_ID, 0, &Timespec::default()),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            clock_nanosleep(CLOCK_PROCESS_CPUTIME_ID, 0, &Timespec::default()),
            Err(Errno::EOPNOTSUPP)
        );
    }

    #[test]
    fn cpu_clocks() {
        let busy = |ms: u128| {
            let s = std::time::Instant::now();
            let mut x = 0u64;
            while s.elapsed().as_millis() < ms {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        };
        let t0 = clock_gettime(CLOCK_THREAD_CPUTIME_ID).unwrap().as_nanos();
        busy(60);
        let t1 = clock_gettime(CLOCK_THREAD_CPUTIME_ID).unwrap().as_nanos();
        assert!(t1 > t0, "thread CPU time advances");
        assert!(clock_gettime(CLOCK_PROCESS_CPUTIME_ID).unwrap().as_nanos() >= t1);

        // pthread_getcpuclockid / clock_getcpuclockid encodings.
        let me = make_thread_cpuclock(process::gettid(), CPUCLOCK_SCHED);
        assert!(clock_gettime(me).unwrap().as_nanos() >= t1);
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            busy(60);
            tx.send(process::gettid()).unwrap();
            done_rx.recv().unwrap();
        });
        let tid = rx.recv().unwrap();
        let other = clock_gettime(make_thread_cpuclock(tid, CPUCLOCK_SCHED)).unwrap();
        assert!(other.as_nanos() > 0);
        done_tx.send(()).unwrap();
        h.join().unwrap();
        let p = make_process_cpuclock(0, CPUCLOCK_PROF);
        assert!(clock_gettime(p).is_ok());
        let p = make_process_cpuclock(process::getpid(), CPUCLOCK_VIRT);
        assert!(clock_gettime(p).is_ok());
        assert_eq!(clock_gettime(-1), Err(Errno::EINVAL)); // kind 3
        // A process that does not exist (other processes' clocks are
        // readable when Windows lets this process query them).
        assert_eq!(
            clock_gettime(make_process_cpuclock(0x7fff_fff0, CPUCLOCK_SCHED)),
            Err(Errno::EINVAL)
        );
    }

    #[test]
    fn resolutions() {
        assert_eq!(clock_getres(CLOCK_MONOTONIC).unwrap().tv_nsec, 100);
        let coarse = clock_getres(CLOCK_MONOTONIC_COARSE).unwrap();
        assert!(coarse.tv_nsec >= 100 && coarse.tv_sec == 0);
        assert!(clock_getres(CLOCK_THREAD_CPUTIME_ID).is_ok());
    }
}

//! Timers: POSIX timers (`timer_create` and friends), `timerfd` and the
//! real-time interval timer (`setitimer(ITIMER_REAL)`, which `alarm` uses).
//!
//! Every armed timer is served by one lx thread that sleeps on a
//! high-resolution Windows timer until the earliest expiry (at most a
//! second, so wall-clock changes are noticed for `CLOCK_REALTIME` timers),
//! fires the timers that are due and sleeps again. The thread exists only
//! while a timer is armed.
//!
//! An expiry of a POSIX timer queues its signal (`si_code` `SI_TIMER`,
//! `si_timerid`, `si_overrun`, `si_value`) for the process or, with
//! `SIGEV_THREAD_ID`, for one thread; while that signal is still pending,
//! further expiries count as overruns, as on Linux. bionic's `SIGEV_THREAD`
//! is built on `SIGEV_THREAD_ID` in libc.
//!
//! # Deviations from Linux
//!
//! - Timers on CPU-time clocks (`CLOCK_PROCESS_CPUTIME_ID`,
//!   `CLOCK_THREAD_CPUTIME_ID`, `clock_getcpuclockid` clocks) are refused
//!   with `EINVAL`, as are `ITIMER_VIRTUAL` and `ITIMER_PROF`: Windows
//!   gives no notification when a CPU-time budget is used up.
//! - `CLOCK_REALTIME_ALARM` and `CLOCK_BOOTTIME_ALARM` timers do not wake a
//!   suspended PC; they fire when it resumes.
//! - `TFD_TIMER_CANCEL_ON_SET` is accepted; a wall-clock change does not
//!   cancel the timer (`ECANCELED` is never reported).
//! - Expiries are seen by readers after the timer thread has woken, which
//!   Windows schedules with about 0.5 ms resolution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::{O_NONBLOCK, O_RDWR};
use crate::linux::poll::*;
use crate::linux::signal::{SI_KERNEL, SI_TIMER, SIGALRM, SIGRTMAX, SigInfo};
use crate::linux::stat::StatData;
use crate::linux::time::*;
use crate::pipe::copy_to_user;
use crate::sys::alarm::Alarm;
use crate::util::{UserBuf, lock};
use crate::waitq;

pub const SIGEV_SIGNAL: i32 = 0;
pub const SIGEV_NONE: i32 = 1;
pub const SIGEV_THREAD: i32 = 2;
pub const SIGEV_THREAD_ID: i32 = 4;

pub const ITIMER_REAL: i32 = 0;
pub const ITIMER_VIRTUAL: i32 = 1;
pub const ITIMER_PROF: i32 = 2;

/// The kernel's `struct sigevent` (64 bytes): the fields lx reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SigEvent {
    pub value: u64,
    pub signo: i32,
    pub notify: i32,
    /// `sigev_notify_thread_id` (the union's first word).
    pub tid: i32,
    pub _pad: [i32; 11],
}

const _: () = assert!(size_of::<SigEvent>() == 64);

/// `struct itimerspec`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItimerSpec {
    pub interval: Timespec,
    pub value: Timespec,
}

const _: () = assert!(size_of::<ItimerSpec>() == 32);

#[derive(Debug)]
enum Action {
    None,
    Signal {
        target: Option<i32>,
        signo: i32,
        value: u64,
        id: i32,
        code: i32,
    },
    Fd,
}

#[derive(Debug, Default)]
struct State {
    /// Next expiry on the timer's clock, nanoseconds.
    next: Option<i128>,
    interval: i128,
    /// Overruns since the last queued signal.
    overrun: i32,
    /// Overruns reported for the last queued signal (`timer_getoverrun`).
    last_overrun: i32,
    /// Unread expirations of a timerfd.
    expirations: u64,
}

#[derive(Debug)]
struct Core {
    clock: i32,
    state: Mutex<State>,
    action: Action,
}

fn now(clock: i32) -> i128 {
    crate::time::clock_gettime(clock)
        .map(|t| t.as_nanos())
        .unwrap_or(0)
}

impl Core {
    fn new(clock: i32, action: Action) -> Arc<Self> {
        Arc::new(Self {
            clock,
            state: Mutex::new(State::default()),
            action,
        })
    }

    /// The remaining time and interval.
    fn get(&self) -> ItimerSpec {
        let st = lock(&self.state);
        let value = match st.next {
            Some(n) => Timespec::from_nanos((n - now(self.clock)).max(1)),
            None => Timespec::default(),
        };
        ItimerSpec {
            interval: Timespec::from_nanos(st.interval),
            value,
        }
    }

    /// Arms (or disarms, for a zero value) the timer; returns the old
    /// setting.
    fn set(self: &Arc<Self>, new: &ItimerSpec, absolute: bool) -> SysResult<ItimerSpec> {
        for t in [&new.interval, &new.value] {
            if !t.is_valid_duration() {
                return Err(Errno::EINVAL);
            }
        }
        let old = self.get();
        let value = new.value.as_nanos();
        let armed = {
            let mut st = lock(&self.state);
            st.interval = new.interval.as_nanos();
            st.overrun = 0;
            st.expirations = 0;
            st.next = if value == 0 {
                None
            } else if absolute {
                Some(value)
            } else {
                Some(now(self.clock) + value)
            };
            st.next.is_some()
        };
        if armed {
            service_arm(self);
        }
        waitq::notify();
        Ok(old)
    }

    /// Fires the timer if it is due at `t`; returns the next expiry.
    fn run(&self, t: i128) -> Option<i128> {
        let mut st = lock(&self.state);
        let next = st.next?;
        if t < next {
            return Some(next);
        }
        let mut missed: u64 = 0;
        if st.interval > 0 {
            let late = ((t - next) / st.interval) as u64;
            missed = late;
            st.next = Some(next + (i128::from(late) + 1) * st.interval);
        } else {
            st.next = None;
        }
        match &self.action {
            Action::None => {}
            Action::Fd => {
                st.expirations = st.expirations.saturating_add(missed + 1);
                waitq::notify();
            }
            &Action::Signal {
                target,
                signo,
                value,
                id,
                code,
            } => {
                st.overrun = st
                    .overrun
                    .saturating_add(missed.min(i32::MAX as u64) as i32);
                let mut info = SigInfo {
                    si_signo: signo,
                    si_code: code,
                    ..SigInfo::default()
                };
                info.fields[0] = u64::from(id as u32) | (u64::from(st.overrun as u32) << 32);
                info.fields[1] = value;
                match crate::signal::queue_timer(target, info) {
                    Ok(true) => {
                        st.last_overrun = st.overrun;
                        st.overrun = 0;
                    }
                    Ok(false) => st.overrun = st.overrun.saturating_add(1),
                    // The target thread is gone: nothing to deliver.
                    Err(_) => {}
                }
            }
        }
        st.next
    }
}

/// The timer thread's shared state.
#[derive(Default)]
struct Service {
    running: bool,
    armed: Vec<Weak<Core>>,
    alarm: Option<Arc<Alarm>>,
}

static SERVICE: Mutex<Option<Service>> = Mutex::new(None);

fn service_arm(core: &Arc<Core>) {
    let mut g = lock(&SERVICE);
    let s = g.get_or_insert_with(Service::default);
    if !s.armed.iter().any(|w| w.as_ptr() == Arc::as_ptr(core)) {
        s.armed.push(Arc::downgrade(core));
    }
    if s.running {
        if let Some(a) = &s.alarm {
            a.wake();
        }
        return;
    }
    let alarm = match &s.alarm {
        Some(a) => a.clone(),
        None => match Alarm::new() {
            Ok(a) => {
                let a = Arc::new(a);
                s.alarm = Some(a.clone());
                a
            }
            Err(_) => return,
        },
    };
    s.running = true;
    let spawned = std::thread::Builder::new()
        .name("lx timers".into())
        .spawn(move || service_loop(&alarm));
    if spawned.is_err() {
        s.running = false;
    }
}

fn service_loop(alarm: &Alarm) {
    loop {
        let cores: Vec<Arc<Core>> = {
            let mut g = lock(&SERVICE);
            let s = g.get_or_insert_with(Service::default);
            s.armed
                .retain(|w| w.upgrade().is_some_and(|c| lock(&c.state).next.is_some()));
            if s.armed.is_empty() {
                s.running = false;
                return;
            }
            s.armed.iter().filter_map(Weak::upgrade).collect()
        };
        let mut wait: i128 = 1_000_000_000;
        for c in &cores {
            let t = now(c.clock);
            if let Some(next) = c.run(t) {
                wait = wait.min(next - now(c.clock));
            }
        }
        if wait > 0 {
            let _ = alarm.sleep(((wait + 99) / 100) as u64);
        }
    }
}

fn check_clock(clock: i32) -> SysResult<()> {
    match clock {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM
        | CLOCK_BOOTTIME_ALARM => Ok(()),
        _ => Err(Errno::EINVAL),
    }
}

/// The base clock of a timer clock id (alarm clocks read like their
/// base).
fn base_clock(clock: i32) -> i32 {
    match clock {
        CLOCK_REALTIME_ALARM => CLOCK_REALTIME,
        CLOCK_BOOTTIME_ALARM => CLOCK_BOOTTIME,
        c => c,
    }
}

/// The process's POSIX timers by id.
static POSIX: Mutex<Option<HashMap<i32, Arc<Core>>>> = Mutex::new(None);

/// `timer_create`: returns the new timer id. `ev` is `None` for a NULL
/// `sigevent` (`SIGALRM` to the process with the timer id as value).
pub fn timer_create(clock: i32, ev: Option<SigEvent>) -> SysResult<i32> {
    check_clock(clock)?;
    let mut g = lock(&POSIX);
    let m = g.get_or_insert_with(HashMap::new);
    let id = (0..i32::MAX)
        .find(|i| !m.contains_key(i))
        .ok_or(Errno::EAGAIN)?;
    let action = match ev {
        None => Action::Signal {
            target: None,
            signo: SIGALRM,
            value: id as u64,
            id,
            code: SI_TIMER,
        },
        Some(e) => {
            let signal = |target| -> SysResult<Action> {
                if !(1..=SIGRTMAX).contains(&e.signo) {
                    return Err(Errno::EINVAL);
                }
                Ok(Action::Signal {
                    target,
                    signo: e.signo,
                    value: e.value,
                    id,
                    code: SI_TIMER,
                })
            };
            match e.notify {
                SIGEV_NONE => Action::None,
                SIGEV_SIGNAL => signal(None)?,
                SIGEV_THREAD_ID => {
                    let own = e.tid == crate::process::gettid()
                        || (e.tid > 0 && crate::sys::thread::is_own_thread(e.tid as u32));
                    if !own {
                        return Err(Errno::EINVAL);
                    }
                    signal(Some(e.tid))?
                }
                _ => return Err(Errno::EINVAL),
            }
        }
    };
    m.insert(id, Core::new(base_clock(clock), action));
    Ok(id)
}

fn posix(id: i32) -> SysResult<Arc<Core>> {
    lock(&POSIX)
        .as_ref()
        .and_then(|m| m.get(&id).cloned())
        .ok_or(Errno::EINVAL)
}

/// `timer_settime`: returns the old setting.
pub fn timer_settime(id: i32, flags: i32, new: &ItimerSpec) -> SysResult<ItimerSpec> {
    posix(id)?.set(new, flags & TIMER_ABSTIME != 0)
}

pub fn timer_gettime(id: i32) -> SysResult<ItimerSpec> {
    Ok(posix(id)?.get())
}

pub fn timer_getoverrun(id: i32) -> SysResult<i32> {
    Ok(lock(&posix(id)?.state).last_overrun)
}

pub fn timer_delete(id: i32) -> SysResult<()> {
    let c = lock(&POSIX)
        .as_mut()
        .and_then(|m| m.remove(&id))
        .ok_or(Errno::EINVAL)?;
    lock(&c.state).next = None;
    Ok(())
}

/// `struct itimerval` (microseconds).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItimerVal {
    pub interval: [i64; 2],
    pub value: [i64; 2],
}

fn tv_to_ts(tv: [i64; 2]) -> SysResult<Timespec> {
    if !(0..1_000_000).contains(&tv[1]) || tv[0] < 0 {
        return Err(Errno::EINVAL);
    }
    Ok(Timespec {
        tv_sec: tv[0],
        tv_nsec: tv[1] * 1000,
    })
}

fn ts_to_tv(ts: Timespec) -> [i64; 2] {
    // Round up so an armed timer never reads as disarmed.
    let us = (ts.as_nanos() + 999) / 1000;
    [(us / 1_000_000) as i64, (us % 1_000_000) as i64]
}

static REAL: Mutex<Option<Arc<Core>>> = Mutex::new(None);

fn real() -> Arc<Core> {
    lock(&REAL)
        .get_or_insert_with(|| {
            Core::new(
                CLOCK_MONOTONIC,
                Action::Signal {
                    target: None,
                    signo: SIGALRM,
                    value: 0,
                    id: -1,
                    code: SI_KERNEL,
                },
            )
        })
        .clone()
}

fn itimer_val(s: ItimerSpec) -> ItimerVal {
    ItimerVal {
        interval: ts_to_tv(s.interval),
        value: ts_to_tv(s.value),
    }
}

/// `getitimer`.
pub fn getitimer(which: i32) -> SysResult<ItimerVal> {
    match which {
        ITIMER_REAL => Ok(itimer_val(real().get())),
        ITIMER_VIRTUAL | ITIMER_PROF => Err(Errno::EINVAL),
        _ => Err(Errno::EINVAL),
    }
}

/// `setitimer`: returns the old value.
pub fn setitimer(which: i32, new: &ItimerVal) -> SysResult<ItimerVal> {
    if which != ITIMER_REAL {
        return Err(Errno::EINVAL);
    }
    let spec = ItimerSpec {
        interval: tv_to_ts(new.interval)?,
        value: tv_to_ts(new.value)?,
    };
    real().set(&spec, false).map(itimer_val)
}

/// A timerfd.
#[derive(Debug)]
pub struct TimerFd {
    core: Arc<Core>,
}

impl FileOps for TimerFd {
    fn read(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        let n = waitq::wait_until(None, || {
            let mut st = lock(&self.core.state);
            if st.expirations > 0 {
                return Ok(Some(core::mem::take(&mut st.expirations)));
            }
            if file.status_flags() & O_NONBLOCK != 0 {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })?;
        copy_to_user(buf, 0, &n.to_ne_bytes())?;
        Ok(8)
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(crate::poll::anon_stat())
    }

    fn kind_name(&self) -> &'static str {
        "timerfd"
    }

    fn poll(&self) -> u32 {
        if lock(&self.core.state).expirations > 0 {
            POLLIN | POLLRDNORM
        } else {
            0
        }
    }
}

impl Drop for TimerFd {
    fn drop(&mut self) {
        lock(&self.core.state).next = None;
    }
}

/// `timerfd_create`.
pub fn timerfd_create(clock: i32, flags: u32) -> SysResult<i32> {
    if flags & !(TFD_CLOEXEC | TFD_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    check_clock(clock)?;
    let core = Core::new(base_clock(clock), Action::Fd);
    let f = Arc::new(OpenFile::new(
        Box::new(TimerFd { core }),
        O_RDWR | (flags & TFD_NONBLOCK),
    ));
    FDS.insert(f, flags & TFD_CLOEXEC != 0, 0)
}

fn timerfd(fd: i32) -> SysResult<Arc<Core>> {
    let f = FDS.get(fd)?;
    f.downcast::<TimerFd>()
        .map(|t| t.core.clone())
        .ok_or(Errno::EINVAL)
}

/// `timerfd_settime`: returns the old setting.
pub fn timerfd_settime(fd: i32, flags: u32, new: &ItimerSpec) -> SysResult<ItimerSpec> {
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return Err(Errno::EINVAL);
    }
    timerfd(fd)?.set(new, flags & TFD_TIMER_ABSTIME != 0)
}

pub fn timerfd_gettime(fd: i32) -> SysResult<ItimerSpec> {
    Ok(timerfd(fd)?.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs;

    fn ms(n: i64) -> Timespec {
        Timespec::from_nanos(i128::from(n) * 1_000_000)
    }

    #[test]
    fn timerfd_fires_and_repeats() {
        let fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC).unwrap();
        let start = std::time::Instant::now();
        timerfd_settime(
            fd,
            0,
            &ItimerSpec {
                interval: ms(5),
                value: ms(20),
            },
        )
        .unwrap();
        let g = timerfd_gettime(fd).unwrap();
        assert!(g.value.as_nanos() > 0 && g.interval == ms(5));
        let mut b = [0u8; 8];
        assert_eq!(fs::read(fd, UserBuf::from_mut(&mut b)), Ok(8));
        assert!(u64::from_ne_bytes(b) >= 1);
        assert!(start.elapsed() >= std::time::Duration::from_millis(19));
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(fs::read(fd, UserBuf::from_mut(&mut b)), Ok(8));
        assert!(u64::from_ne_bytes(b) >= 3, "{}", u64::from_ne_bytes(b));
        // Disarm.
        timerfd_settime(fd, 0, &ItimerSpec::default()).unwrap();
        assert_eq!(timerfd_gettime(fd).unwrap(), ItimerSpec::default());
        fs::close(fd).unwrap();
        assert_eq!(
            timerfd_create(CLOCK_PROCESS_CPUTIME_ID, 0),
            Err(Errno::EINVAL)
        );
        assert_eq!(timerfd_create(CLOCK_MONOTONIC, 0x40), Err(Errno::EINVAL));
    }

    #[test]
    fn posix_timer_ids_and_errors() {
        let ev = SigEvent {
            notify: SIGEV_NONE,
            ..SigEvent::default()
        };
        let id = timer_create(CLOCK_REALTIME, Some(ev)).unwrap();
        assert_eq!(timer_create(16, None), Err(Errno::EINVAL));
        let bad = SigEvent {
            notify: SIGEV_SIGNAL,
            signo: 0,
            ..SigEvent::default()
        };
        assert_eq!(timer_create(CLOCK_MONOTONIC, Some(bad)), Err(Errno::EINVAL));
        let s = ItimerSpec {
            interval: Timespec::default(),
            value: Timespec {
                tv_sec: 10,
                tv_nsec: 0,
            },
        };
        timer_settime(id, 0, &s).unwrap();
        let g = timer_gettime(id).unwrap();
        assert!(g.value.tv_sec >= 9 && g.value.tv_sec <= 10);
        let bad = ItimerSpec {
            interval: Timespec::default(),
            value: Timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000_000,
            },
        };
        assert_eq!(timer_settime(id, 0, &bad), Err(Errno::EINVAL));
        assert_eq!(timer_getoverrun(id), Ok(0));
        timer_delete(id).unwrap();
        assert_eq!(timer_delete(id), Err(Errno::EINVAL));
        assert_eq!(timer_gettime(id), Err(Errno::EINVAL));
    }
}

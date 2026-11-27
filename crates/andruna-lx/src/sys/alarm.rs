//! A high-resolution waitable timer paired with a wake-up event, for the
//! thread that runs lx's timers.

use windows::Win32::Foundation::WAIT_FAILED;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE,
    SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForMultipleObjects,
};
use windows::core::PCWSTR;

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

/// A timer to sleep on and an event that ends the sleep early.
#[derive(Debug)]
pub struct Alarm {
    timer: OwnedHandle,
    event: OwnedHandle,
}

impl Alarm {
    pub fn new() -> SysResult<Self> {
        // SAFETY: no security attributes and no name; the handles are owned
        // by the result.
        let timer = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .map_err(|_| Errno::ENOMEM)?;
        let timer = OwnedHandle::from_raw(timer);
        // SAFETY: as above; an auto-reset event, initially not set.
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map_err(|_| Errno::ENOMEM)?;
        Ok(Self {
            timer,
            event: OwnedHandle::from_raw(event),
        })
    }

    /// Ends the current or next [`Alarm::sleep`] early.
    pub fn wake(&self) {
        // SAFETY: a live event handle.
        let _ = unsafe { SetEvent(self.event.raw()) };
    }

    /// Sleeps for `hundred_ns` 100 ns units (at least one) or until
    /// [`Alarm::wake`].
    pub fn sleep(&self, hundred_ns: u64) -> SysResult<()> {
        let due = -(hundred_ns.clamp(1, i64::MAX as u64) as i64);
        // SAFETY: a live timer handle and a valid due-time pointer; no
        // completion routine.
        unsafe { SetWaitableTimer(self.timer.raw(), &due, 0, None, None, false) }
            .map_err(Errno::from)?;
        let handles = [self.timer.raw(), self.event.raw()];
        // SAFETY: both handles are live for the call.
        let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        if r == WAIT_FAILED {
            return Err(Errno::EIO);
        }
        Ok(())
    }
}

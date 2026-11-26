//! Process and thread identity and CPU times.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId,
    GetProcessIdOfThread, GetProcessTimes, GetThreadTimes, OpenThread, SwitchToThread,
    THREAD_QUERY_LIMITED_INFORMATION,
};

use super::OwnedHandle;
use super::time::filetime_to_u64;
use crate::errno::{Errno, SysResult};

pub fn current_process_id() -> u32 {
    // SAFETY: no arguments; returns a value.
    unsafe { GetCurrentProcessId() }
}

pub fn current_thread_id() -> u32 {
    // SAFETY: no arguments; returns a value.
    unsafe { GetCurrentThreadId() }
}

/// Gives up the processor to another ready thread, if any.
pub fn yield_now() {
    // SAFETY: no arguments. The result only says whether a switch happened.
    let _ = unsafe { SwitchToThread() };
}

/// User and kernel CPU time, in 100 ns units.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuTimes {
    pub user: u64,
    pub kernel: u64,
}

pub fn process_times() -> SysResult<CpuTimes> {
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: the pseudo handle is always valid; all out pointers are valid.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }
        .map_err(Errno::from)?;
    Ok(times(k, u))
}

/// CPU times of thread `tid` of this process (`EINVAL` for other threads);
/// the calling thread when `tid` is `None`.
pub fn thread_times(tid: Option<u32>) -> SysResult<CpuTimes> {
    let owned;
    let handle = match tid {
        None => {
            // SAFETY: returns the pseudo handle of the calling thread.
            unsafe { GetCurrentThread() }
        }
        Some(t) => {
            // SAFETY: opens a new handle that `owned` closes.
            let h = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, t) }
                .map_err(|_| Errno::EINVAL)?;
            owned = OwnedHandle::from_raw(h);
            // Thread ids are global in Windows; only threads of this process
            // have clocks here, as on Linux.
            // SAFETY: `owned` is a live thread handle.
            if unsafe { GetProcessIdOfThread(owned.raw()) } != current_process_id() {
                return Err(Errno::EINVAL);
            }
            owned.raw()
        }
    };
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: `handle` is valid for the call; all out pointers are valid.
    unsafe { GetThreadTimes(handle, &mut c, &mut e, &mut k, &mut u) }.map_err(Errno::from)?;
    Ok(times(k, u))
}

fn times(kernel: FILETIME, user: FILETIME) -> CpuTimes {
    CpuTimes {
        user: filetime_to_u64(user),
        kernel: filetime_to_u64(kernel),
    }
}

//! Process and thread identity and CPU times.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId,
    GetExitCodeThread, GetProcessIdOfThread, GetProcessTimes, GetThreadTimes, OpenThread,
    SwitchToThread, THREAD_QUERY_LIMITED_INFORMATION, TerminateProcess,
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

/// Whether `tid` is a running thread of this process.
pub fn is_own_thread(tid: u32) -> bool {
    // SAFETY: opens a new handle that `owned` closes.
    let Ok(h) = (unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) }) else {
        return false;
    };
    let owned = OwnedHandle::from_raw(h);
    let mut code = 0u32;
    // SAFETY: `owned` is a live thread handle; `code` is a valid out pointer.
    let alive = unsafe { GetExitCodeThread(owned.raw(), &mut code) }.is_ok() && code == 259;
    // SAFETY: as above.
    alive && unsafe { GetProcessIdOfThread(owned.raw()) } == current_process_id()
}

/// Ends the process at once with `code` (no exit handlers run, as for a
/// Linux process killed by a signal).
pub fn terminate_process(code: u32) -> ! {
    // SAFETY: the pseudo handle of the current process is always valid.
    let _ = unsafe { TerminateProcess(GetCurrentProcess(), code) };
    std::process::abort()
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

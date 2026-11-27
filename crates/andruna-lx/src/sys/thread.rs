//! Process and thread identity and CPU times.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcess, GetCurrentProcessId, GetCurrentThread,
    GetCurrentThreadId, GetExitCodeThread, GetProcessAffinityMask, GetProcessIdOfThread,
    GetProcessTimes, GetThreadTimes, OpenThread, SetThreadAffinityMask, SwitchToThread,
    THREAD_QUERY_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SET_INFORMATION,
    TerminateProcess,
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

/// Logical processors in the calling thread's processor group.
pub fn processor_count() -> usize {
    // SAFETY: no pointer arguments.
    let n = unsafe { GetActiveProcessorCount(0) };
    (n as usize).max(1)
}

fn open_for_affinity(tid: u32) -> SysResult<OwnedHandle> {
    let tid = crate::process::windows_tid(tid);
    // SAFETY: opens a new handle that the result owns.
    let h = unsafe {
        OpenThread(
            THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION,
            false,
            tid,
        )
    }
    .map_err(|_| Errno::ESRCH)?;
    Ok(OwnedHandle::from_raw(h))
}

/// The affinity mask of thread `tid` of this process.
pub fn affinity(tid: u32) -> SysResult<u64> {
    let h = open_for_affinity(tid)?;
    let (mut process, mut system) = (0usize, 0usize);
    // SAFETY: valid out pointers; the pseudo handle is always valid.
    unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) }
        .map_err(Errno::from)?;
    // Windows has no getter: setting returns the previous mask, which is
    // then put back.
    // SAFETY: `h` is a live thread handle with set access.
    let old = unsafe { SetThreadAffinityMask(h.raw(), process) };
    if old == 0 {
        return Err(Errno::EINVAL);
    }
    // SAFETY: as above.
    unsafe { SetThreadAffinityMask(h.raw(), old) };
    Ok(old as u64)
}

/// Sets the affinity mask of thread `tid` of this process.
pub fn set_affinity(tid: u32, mask: u64) -> SysResult<()> {
    let h = open_for_affinity(tid)?;
    // SAFETY: `h` is a live thread handle with set access.
    if unsafe { SetThreadAffinityMask(h.raw(), mask as usize) } == 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// The thread ids of this process (Linux numbering).
pub fn threads() -> Vec<u32> {
    let mut out: Vec<u32> = match super::procinfo::thread_ids() {
        Ok(v) => v.into_iter().map(crate::process::linux_tid).collect(),
        Err(_) => vec![crate::process::linux_tid(current_thread_id())],
    };
    out.sort_unstable();
    out
}

fn open_named(
    tid: u32,
    access: windows::Win32::System::Threading::THREAD_ACCESS_RIGHTS,
) -> Option<OwnedHandle> {
    let tid = crate::process::windows_tid(tid);
    // SAFETY: opens a new handle that the result owns.
    let h = unsafe { OpenThread(access, false, tid) }.ok()?;
    Some(OwnedHandle::from_raw(h))
}

/// The Windows description (name) of thread `tid`.
pub fn description(tid: u32) -> Option<String> {
    use windows::Win32::System::Threading::GetThreadDescription;
    let h = open_named(tid, THREAD_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: `h` is a live thread handle; the returned string is freed
    // with LocalFree below.
    let p = unsafe { GetThreadDescription(h.raw()) }.ok()?;
    // SAFETY: a NUL-terminated string from Windows.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: the string was allocated by GetThreadDescription.
    unsafe {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(p.0 as _)))
    };
    s
}

/// Sets the Windows description (name) of thread `tid`.
pub fn set_description(tid: u32, name: &str) -> SysResult<()> {
    use windows::Win32::System::Threading::{SetThreadDescription, THREAD_SET_LIMITED_INFORMATION};
    let h = open_named(tid, THREAD_SET_LIMITED_INFORMATION).ok_or(Errno::ESRCH)?;
    let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `h` is a live thread handle; `w` is NUL-terminated.
    unsafe { SetThreadDescription(h.raw(), windows::core::PCWSTR(w.as_ptr())) }
        .map_err(|_| Errno::EINVAL)
}

/// Whether `tid` is a running thread of this process.
pub fn is_own_thread(tid: u32) -> bool {
    let tid = crate::process::windows_tid(tid);
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

/// Suspends every thread of the process, the caller included
/// (`NtSuspendProcess`, which the `windows` crate does not declare).
/// Returns when another process resumes it (`NtResumeProcess`). Without
/// the export, the calling thread waits forever instead.
pub fn suspend_process() {
    use windows::Win32::Foundation::NTSTATUS;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{s, w};
    type NtSuspendProcess =
        unsafe extern "system" fn(windows::Win32::Foundation::HANDLE) -> NTSTATUS;
    // SAFETY: ntdll is loaded in every process; the names are valid
    // NUL-terminated strings.
    let f = unsafe { GetModuleHandleW(w!("ntdll.dll")) }
        .ok()
        .and_then(|m| unsafe { GetProcAddress(m, s!("NtSuspendProcess")) });
    match f {
        Some(f) => {
            // SAFETY: the export has the signature of NtSuspendProcess.
            let f: NtSuspendProcess = unsafe { core::mem::transmute(f) };
            // SAFETY: the pseudo handle of the process is always valid.
            let _ = unsafe { f(GetCurrentProcess()) };
        }
        None => loop {
            std::thread::park();
        },
    }
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

fn open_process(pid: u32) -> Option<OwnedHandle> {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: opens a new handle that the result owns.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    Some(OwnedHandle::from_raw(h))
}

/// Whether process `pid` exists (and may be queried).
pub fn process_exists(pid: u32) -> bool {
    let Some(h) = open_process(pid) else {
        return false;
    };
    let mut code = 0u32;
    // SAFETY: a live process handle and a valid out pointer.
    unsafe { windows::Win32::System::Threading::GetExitCodeProcess(h.raw(), &mut code) }.is_ok()
        && code == 259
}

/// CPU times of another process.
pub fn process_times_of(pid: u32) -> SysResult<CpuTimes> {
    let h = open_process(pid).ok_or(Errno::EINVAL)?;
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: a live process handle; all out pointers are valid.
    unsafe { GetProcessTimes(h.raw(), &mut c, &mut e, &mut k, &mut u) }.map_err(Errno::from)?;
    Ok(times(k, u))
}

fn creation_time(h: &OwnedHandle) -> Option<u64> {
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: a live process handle; all out pointers are valid.
    unsafe { GetProcessTimes(h.raw(), &mut c, &mut e, &mut k, &mut u) }.ok()?;
    Some(filetime_to_u64(c))
}

/// The id of the process that started this one, or 0 when it has ended
/// (its id may then belong to a newer process).
pub fn parent_process_id() -> u32 {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let me = current_process_id();
    // SAFETY: creates a snapshot handle that `owned` closes.
    let Ok(h) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return 0;
    };
    let owned = OwnedHandle::from_raw(h);
    let mut e = PROCESSENTRY32W {
        dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut parent = 0;
    // SAFETY: `e` is a valid PROCESSENTRY32W with its size set.
    let mut ok = unsafe { Process32FirstW(owned.raw(), &mut e) }.is_ok();
    while ok {
        if e.th32ProcessID == me {
            parent = e.th32ParentProcessID;
            break;
        }
        // SAFETY: as above.
        ok = unsafe { Process32NextW(owned.raw(), &mut e) }.is_ok();
    }
    if parent == 0 || !process_exists(parent) {
        return 0;
    }
    // A process started after this one only reuses the parent's id.
    let mine = open_process(me).and_then(|h| creation_time(&h));
    let theirs = open_process(parent).and_then(|h| creation_time(&h));
    match (mine, theirs) {
        (Some(m), Some(t)) if t <= m => parent,
        _ => 0,
    }
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

/// The stack reserve of the executable's image, which Windows gives the
/// process's main thread.
pub fn main_stack_size() -> u64 {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    // SAFETY: NULL names the executable's module, which stays loaded.
    let Ok(m) = (unsafe { GetModuleHandleW(None) }) else {
        return 1 << 20;
    };
    let base = m.0 as *const u8;
    // SAFETY: a loaded image starts with its DOS header, whose `e_lfanew`
    // (offset 0x3c) locates the NT headers inside the image; in a PE32+
    // optional header (after the 4-byte signature and the 20-byte file
    // header) `SizeOfStackReserve` is at offset 72.
    unsafe {
        let nt = base.add(base.add(0x3c).cast::<u32>().read_unaligned() as usize);
        nt.add(4 + 20 + 72).cast::<u64>().read_unaligned()
    }
}

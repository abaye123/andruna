//! The scheduler's view of this process: its threads, each thread's state
//! and times, and the process's memory counters.
//!
//! Everything is asked of this process and its threads only: the threads
//! are enumerated with `NtGetNextThread`, a thread's state and times come
//! from `NtQueryInformationThread` (`ThreadSystemThreadInformation`), and
//! the memory counters from `NtQueryInformationProcess`
//! (`ProcessVmCounters`). A snapshot of every process of the system
//! (`NtQuerySystemInformation`) cost 7 to 20 ms per `/proc/self/stat` open
//! on a loaded desktop; these calls cost microseconds.

use core::ffi::c_void;
use std::sync::OnceLock;

use windows::Wdk::System::SystemServices::VM_COUNTERS;
use windows::Wdk::System::Threading::{
    NtQueryInformationProcess, NtQueryInformationThread, ProcessVmCounters, THREADINFOCLASS,
};
use windows::Win32::Foundation::{HANDLE, NTSTATUS};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeThread, GetProcessIdOfThread, GetThreadId, OpenThread,
    THREAD_QUERY_INFORMATION,
};
use windows::Win32::System::WindowsProgramming::SYSTEM_THREAD_INFORMATION;
use windows::core::{s, w};

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

/// One thread as the Windows scheduler sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadInfo {
    /// Windows thread id.
    pub tid: u32,
    /// `KTHREAD_STATE`.
    pub state: u32,
    /// `KWAIT_REASON`, meaningful when `state` is waiting.
    pub wait_reason: u32,
    /// CPU times and creation time, 100 ns units (creation as a FILETIME).
    pub user: u64,
    pub kernel: u64,
    pub created: u64,
    /// Current priority.
    pub priority: i32,
}

/// The process's memory counters, bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryCounters {
    pub virtual_size: u64,
    pub working_set: u64,
}

// `KTHREAD_STATE` values.
#[cfg(test)]
pub const STATE_RUNNING: u32 = 2;
pub const STATE_TERMINATED: u32 = 4;
pub const STATE_WAITING: u32 = 5;

// `KWAIT_REASON` values.
pub const WAIT_FREE_PAGE: u32 = 1;
pub const WAIT_PAGE_IN: u32 = 2;
pub const WAIT_POOL_ALLOCATION: u32 = 3;
pub const WAIT_SUSPENDED: u32 = 5;
pub const WAIT_WR_SUSPENDED: u32 = 12;
pub const WAIT_WR_PAGE_IN: u32 = 9;

/// `ThreadSystemThreadInformation` (Windows 8 and later).
const THREAD_SYSTEM_THREAD_INFORMATION: THREADINFOCLASS = THREADINFOCLASS(40);
/// `STILL_ACTIVE`, the exit code of a running thread.
const STILL_ACTIVE: u32 = 259;

/// `NtGetNextThread`, which the `windows` crate does not declare: the
/// next thread of a process after `thread` (or the first one).
type NtGetNextThread = unsafe extern "system" fn(
    process: HANDLE,
    thread: HANDLE,
    access: u32,
    attributes: u32,
    flags: u32,
    next: *mut HANDLE,
) -> NTSTATUS;

fn nt_get_next_thread() -> Option<NtGetNextThread> {
    static F: OnceLock<Option<NtGetNextThread>> = OnceLock::new();
    *F.get_or_init(|| {
        // SAFETY: ntdll is loaded in every process; the name is a valid
        // NUL-terminated string.
        let m = unsafe { GetModuleHandleW(w!("ntdll.dll")) }.ok()?;
        // SAFETY: as above; the export has the signature of
        // `NtGetNextThread` (ntdll's documented-by-use contract since
        // Windows Vista), so the transmute gives a callable pointer.
        unsafe { GetProcAddress(m, s!("NtGetNextThread")) }
            .map(|f| unsafe { core::mem::transmute::<_, NtGetNextThread>(f) })
    })
}

/// Handles (query access) to the live threads of this process.
fn thread_handles() -> SysResult<Vec<OwnedHandle>> {
    let next = nt_get_next_thread().ok_or(Errno::ENOSYS)?;
    let mut out: Vec<OwnedHandle> = Vec::new();
    loop {
        let prev = out.last().map_or(HANDLE::default(), OwnedHandle::raw);
        let mut h = HANDLE::default();
        // SAFETY: `prev` is null or a live thread handle of this process;
        // `h` is a valid out pointer. The pseudo handle of the process is
        // always valid.
        let st = unsafe {
            next(
                GetCurrentProcess(),
                prev,
                THREAD_QUERY_INFORMATION.0,
                0,
                0,
                &mut h,
            )
        };
        if st.is_err() {
            // STATUS_NO_MORE_ENTRIES ends the list.
            break;
        }
        out.push(OwnedHandle::from_raw(h));
    }
    out.retain(|h| {
        let mut code = 0u32;
        // SAFETY: `h` is a live thread handle; `code` is a valid out pointer.
        unsafe { GetExitCodeThread(h.raw(), &mut code) }.is_ok() && code == STILL_ACTIVE
    });
    Ok(out)
}

/// The Windows ids of the live threads of this process.
pub fn thread_ids() -> SysResult<Vec<u32>> {
    Ok(thread_handles()?
        .iter()
        // SAFETY: a live thread handle.
        .map(|h| unsafe { GetThreadId(h.raw()) })
        .collect())
}

fn query_thread(h: HANDLE) -> SysResult<ThreadInfo> {
    let mut t = SYSTEM_THREAD_INFORMATION::default();
    let mut len = 0u32;
    // SAFETY: `t` is a writable SYSTEM_THREAD_INFORMATION of the size
    // passed; `h` is a live thread handle with query access.
    let st = unsafe {
        NtQueryInformationThread(
            h,
            THREAD_SYSTEM_THREAD_INFORMATION,
            (&mut t as *mut SYSTEM_THREAD_INFORMATION).cast::<c_void>(),
            size_of::<SYSTEM_THREAD_INFORMATION>() as u32,
            &mut len,
        )
    };
    if st.is_err() {
        return Err(Errno::from_ntstatus(st));
    }
    Ok(ThreadInfo {
        tid: t.ClientId.UniqueThread.0 as usize as u32,
        state: t.ThreadState,
        wait_reason: t.WaitReason,
        // Reserved1 holds KernelTime, UserTime, CreateTime.
        kernel: t.Reserved1[0] as u64,
        user: t.Reserved1[1] as u64,
        created: t.Reserved1[2] as u64,
        priority: t.Priority,
    })
}

/// The scheduler's information about thread `tid` (a Windows id) of this
/// process; `ESRCH` if it is not a live thread of this process.
pub fn thread(tid: u32) -> SysResult<ThreadInfo> {
    // SAFETY: opens a new handle that `h` owns.
    let h = unsafe { OpenThread(THREAD_QUERY_INFORMATION, false, tid) }
        .map(OwnedHandle::from_raw)
        .map_err(|_| Errno::ESRCH)?;
    // SAFETY: `h` is a live thread handle.
    if unsafe { GetProcessIdOfThread(h.raw()) } != super::thread::current_process_id() {
        return Err(Errno::ESRCH);
    }
    let t = query_thread(h.raw())?;
    if t.state == STATE_TERMINATED {
        return Err(Errno::ESRCH);
    }
    Ok(t)
}

/// The scheduler's information about every live thread of this process.
#[cfg(test)]
pub fn threads() -> SysResult<Vec<ThreadInfo>> {
    let mut v: Vec<ThreadInfo> = thread_handles()?
        .iter()
        .filter_map(|h| query_thread(h.raw()).ok())
        .filter(|t| t.state != STATE_TERMINATED)
        .collect();
    v.sort_unstable_by_key(|t| t.tid);
    Ok(v)
}

/// The process's virtual size and working set.
pub fn memory() -> SysResult<MemoryCounters> {
    let mut c = VM_COUNTERS::default();
    let mut len = 0u32;
    // SAFETY: `c` is a writable VM_COUNTERS of the size passed; the pseudo
    // handle of the process is always valid.
    let st = unsafe {
        NtQueryInformationProcess(
            GetCurrentProcess(),
            ProcessVmCounters,
            (&mut c as *mut VM_COUNTERS).cast::<c_void>(),
            size_of::<VM_COUNTERS>() as u32,
            &mut len,
        )
    };
    if st.is_err() {
        return Err(Errno::from_ntstatus(st));
    }
    Ok(MemoryCounters {
        virtual_size: c.VirtualSize as u64,
        working_set: c.WorkingSetSize as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_windows() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<SYSTEM_THREAD_INFORMATION>(), 0x50);
            assert_eq!(size_of::<VM_COUNTERS>(), 0x58);
        }
    }

    #[test]
    fn sees_a_sleeping_thread() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (idtx, idrx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            idtx.send(super::super::thread::current_thread_id())
                .unwrap();
            rx.recv().unwrap();
        });
        let tid = idrx.recv().unwrap();
        let me = super::super::thread::current_thread_id();
        assert!(thread_ids().unwrap().contains(&tid));
        assert!(thread_ids().unwrap().contains(&me));
        let m = memory().unwrap();
        assert!(m.virtual_size > 0 && m.working_set > 0);
        let mine = thread(me).unwrap();
        assert_eq!(mine.state, STATE_RUNNING);
        assert_eq!(mine.tid, me);
        let mut ok = false;
        for _ in 0..1000 {
            if thread(tid).unwrap().state == STATE_WAITING
                && threads()
                    .unwrap()
                    .iter()
                    .any(|t| t.tid == tid && t.state == STATE_WAITING)
            {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(ok);
        tx.send(()).unwrap();
        h.join().unwrap();
        // An ended thread is no longer reported.
        assert_eq!(thread(tid), Err(Errno::ESRCH));
        assert!(!thread_ids().unwrap().contains(&tid));
    }

    #[test]
    fn queries_are_cheap() {
        let t0 = std::time::Instant::now();
        for _ in 0..100 {
            let _ = threads().unwrap();
            let _ = memory().unwrap();
        }
        let each = t0.elapsed() / 100;
        // A system-wide snapshot took milliseconds; these take microseconds
        // (generous bound for loaded CI machines).
        assert!(each < std::time::Duration::from_millis(2), "{each:?}");
    }
}

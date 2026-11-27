//! The scheduler's view of this process: each thread's state and times,
//! and the process's memory counters (`NtQuerySystemInformation`).

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::STATUS_INFO_LENGTH_MISMATCH;
use windows::Win32::System::WindowsProgramming::{
    SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
};

use std::sync::atomic::{AtomicUsize, Ordering};

use super::thread::current_process_id;
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

/// The process and its threads.
#[derive(Clone, Debug, Default)]
pub struct ProcessInfo {
    pub virtual_size: u64,
    pub working_set: u64,
    pub threads: Vec<ThreadInfo>,
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

/// Reads the scheduler's information about this process.
pub fn current_process() -> SysResult<ProcessInfo> {
    // The buffer holds every process and thread of the system (about 1 MB
    // on a desktop). It starts at 2 MiB or the size the last call needed,
    // so a call normally queries once, and grows by an eighth over what
    // Windows asks for instead of to 16 times its size: the retried query
    // and the larger buffer doubled the cost of opening `/proc/self/stat`,
    // which bionic does twice while ART starts.
    static LAST: AtomicUsize = AtomicUsize::new(256 * 1024);
    let pid = current_process_id() as usize;
    let mut buf: Vec<u64> = vec![0; LAST.load(Ordering::Relaxed)];
    loop {
        let bytes = (buf.len() * 8) as u32;
        let mut needed = 0u32;
        // SAFETY: `buf` is writable for `bytes` bytes and 8-byte aligned,
        // as the records require; `needed` is a valid out pointer.
        let st = unsafe {
            NtQuerySystemInformation(
                SystemProcessInformation,
                buf.as_mut_ptr().cast(),
                bytes,
                &mut needed,
            )
        };
        if st == STATUS_INFO_LENGTH_MISMATCH {
            let bytes = (needed as usize).max(buf.len() * 8);
            let want = (bytes + bytes / 8) / 8 + 512;
            LAST.store(want, Ordering::Relaxed);
            buf = vec![0; want];
            continue;
        }
        if st.is_err() {
            return Err(Errno::from_ntstatus(st));
        }
        break;
    }
    let base = buf.as_ptr().cast::<u8>();
    let total = buf.len() * 8;
    let mut off = 0usize;
    loop {
        if off + size_of::<SYSTEM_PROCESS_INFORMATION>() > total {
            return Err(Errno::ESRCH);
        }
        // SAFETY: the record lies inside `buf` (checked above); Windows
        // aligns records to 8 bytes.
        let p = unsafe { &*base.add(off).cast::<SYSTEM_PROCESS_INFORMATION>() };
        if p.UniqueProcessId.0 as usize == pid {
            let first = off + size_of::<SYSTEM_PROCESS_INFORMATION>();
            let n = p.NumberOfThreads as usize;
            if first + n * size_of::<SYSTEM_THREAD_INFORMATION>() > total {
                return Err(Errno::EIO);
            }
            let threads = (0..n)
                .map(|i| {
                    // SAFETY: the thread records follow the process record
                    // inside `buf` (checked above).
                    let t = unsafe {
                        &*base
                            .add(first + i * size_of::<SYSTEM_THREAD_INFORMATION>())
                            .cast::<SYSTEM_THREAD_INFORMATION>()
                    };
                    ThreadInfo {
                        tid: t.ClientId.UniqueThread.0 as usize as u32,
                        state: t.ThreadState,
                        wait_reason: t.WaitReason,
                        // Reserved1 holds KernelTime, UserTime, CreateTime.
                        kernel: t.Reserved1[0] as u64,
                        user: t.Reserved1[1] as u64,
                        created: t.Reserved1[2] as u64,
                        priority: t.Priority,
                    }
                })
                .collect();
            return Ok(ProcessInfo {
                virtual_size: p.VirtualSize as u64,
                working_set: p.WorkingSetSize as u64,
                threads,
            });
        }
        if p.NextEntryOffset == 0 {
            return Err(Errno::ESRCH);
        }
        off += p.NextEntryOffset as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_windows() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<SYSTEM_PROCESS_INFORMATION>(), 0x100);
            assert_eq!(size_of::<SYSTEM_THREAD_INFORMATION>(), 0x50);
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
        let mut ok = false;
        for _ in 0..1000 {
            let info = current_process().unwrap();
            assert!(info.virtual_size > 0 && info.working_set > 0);
            let mine = info.threads.iter().find(|t| t.tid == me).unwrap();
            assert_eq!(mine.state, STATE_RUNNING);
            if info
                .threads
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
    }
}

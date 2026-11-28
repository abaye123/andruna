//! Windows calls behind `uname`, `sysinfo`, `getrusage`, priorities,
//! `getcpu`, `membarrier` and `mincore` ([`crate::sysinfo`]).

use core::ffi::c_void;

use windows::Wdk::System::SystemServices::VM_COUNTERS;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessVmCounters};
use windows::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows::Win32::System::ProcessStatus::{PSAPI_WORKING_SET_EX_INFORMATION, QueryWorkingSetEx};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    FlushProcessWriteBuffers, GetCurrentProcess, GetCurrentProcessorNumberEx, OpenThread,
    SetThreadPriority, THREAD_PRIORITY, THREAD_SET_LIMITED_INFORMATION,
};

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

/// Physical memory and commit (page file) totals, bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStatus {
    pub total_phys: u64,
    pub avail_phys: u64,
    pub total_commit: u64,
    pub avail_commit: u64,
}

pub fn memory_status() -> MemoryStatus {
    let mut m = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: a correctly sized structure with its length set.
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_err() {
        return MemoryStatus::default();
    }
    MemoryStatus {
        total_phys: m.ullTotalPhys,
        avail_phys: m.ullAvailPhys,
        total_commit: m.ullTotalPageFile,
        avail_commit: m.ullAvailPageFile,
    }
}

/// The idle time of all processors since boot (the idle process's time,
/// 100 ns units) and the number of active processors in all groups.
/// `SystemPerformanceInformation` costs microseconds; the per-processor
/// queries (`GetSystemTimes`, `SystemProcessorPerformanceInformation`)
/// took 4 to 25 ms on a loaded desktop.
pub fn idle_time() -> Option<(u64, u32)> {
    use windows::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS,
    };
    use windows::Win32::System::Threading::GetActiveProcessorCount;
    // SYSTEM_PERFORMANCE_INFORMATION starts with IdleProcessTime; the
    // structure is 376 bytes on 64-bit Windows 11.
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for the size passed and 8-byte aligned.
    let st = unsafe {
        NtQuerySystemInformation(
            SYSTEM_INFORMATION_CLASS(2), // SystemPerformanceInformation
            buf.as_mut_ptr().cast::<c_void>(),
            size_of_val(&buf) as u32,
            &mut len,
        )
    };
    if st.is_err() || len < 8 {
        return None;
    }
    // SAFETY: no pointer arguments; ALL_PROCESSOR_GROUPS.
    let cpus = unsafe { GetActiveProcessorCount(0xffff) }.max(1);
    Some((buf[0], cpus))
}

/// The process's peak working set and page fault count.
pub fn process_counters() -> (u64, u64) {
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
        return (0, 0);
    }
    (c.PeakWorkingSetSize as u64, u64::from(c.PageFaultCount))
}

/// The processor the calling thread runs on, numbered across groups of 64.
pub fn current_processor() -> u32 {
    // SAFETY: no arguments.
    let p: PROCESSOR_NUMBER = unsafe { GetCurrentProcessorNumberEx() };
    u32::from(p.Group) * 64 + u32::from(p.Number)
}

/// An inter-processor interrupt to every processor running a thread of the
/// process, with a memory barrier on each.
pub fn flush_process_write_buffers() {
    // SAFETY: no arguments.
    unsafe { FlushProcessWriteBuffers() }
}

/// Sets the Windows priority of thread `tid` (a Windows id).
pub fn set_thread_priority(tid: u32, priority: i32) -> SysResult<()> {
    // SAFETY: opens a new handle that `h` owns.
    let h = unsafe { OpenThread(THREAD_SET_LIMITED_INFORMATION, false, tid) }
        .map(OwnedHandle::from_raw)
        .map_err(|_| Errno::ESRCH)?;
    // SAFETY: a live thread handle with set access.
    unsafe { SetThreadPriority(h.raw(), THREAD_PRIORITY(priority)) }.map_err(Errno::from)
}

/// Whether each page starting at `addr` is resident (in the working set).
pub fn resident(addr: usize, pages: usize) -> SysResult<Vec<bool>> {
    let mut v: Vec<PSAPI_WORKING_SET_EX_INFORMATION> = (0..pages)
        .map(|i| PSAPI_WORKING_SET_EX_INFORMATION {
            VirtualAddress: (addr + i * 4096) as *mut c_void,
            ..Default::default()
        })
        .collect();
    // SAFETY: `v` is a writable array of the size passed; the call only
    // queries the address space.
    unsafe {
        QueryWorkingSetEx(
            GetCurrentProcess(),
            v.as_mut_ptr().cast::<c_void>(),
            (v.len() * size_of::<PSAPI_WORKING_SET_EX_INFORMATION>()) as u32,
        )
    }
    .map_err(Errno::from)?;
    // SAFETY: the Flags view of the attribute union; bit 0 is `Valid`.
    Ok(v.iter()
        .map(|e| unsafe { e.VirtualAttributes.Flags } & 1 != 0)
        .collect())
}

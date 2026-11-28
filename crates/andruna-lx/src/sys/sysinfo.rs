//! Windows calls behind `uname`, `sysinfo`, `getrusage`, priorities,
//! `getcpu`, `membarrier` and `mincore` ([`crate::sysinfo`]).

use core::ffi::c_void;

use windows::Wdk::System::SystemServices::VM_COUNTERS;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessVmCounters};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows::Win32::System::ProcessStatus::{PSAPI_WORKING_SET_EX_INFORMATION, QueryWorkingSetEx};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    FlushProcessWriteBuffers, GetCurrentProcess, GetCurrentProcessorNumberEx, GetSystemTimes,
    OpenThread, SetThreadPriority, THREAD_PRIORITY, THREAD_SET_LIMITED_INFORMATION,
};

use super::OwnedHandle;
use super::time::filetime_to_u64;
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

/// System-wide processor times since boot, 100 ns units: (idle, busy).
/// Windows counts idle time inside kernel time.
pub fn system_times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: three valid out pointers.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    let (i, k, u) = (
        filetime_to_u64(idle),
        filetime_to_u64(kernel),
        filetime_to_u64(user),
    );
    Some((i, (k + u).saturating_sub(i)))
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

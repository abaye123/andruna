//! Small Windows helpers: handles, process creation (optionally suspended
//! and inside an AppContainer), and memory accounting queries.

use std::ffi::c_void;
use std::io;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, SetHandleInformation, DUPLICATE_HANDLE_OPTIONS,
    DUPLICATE_SAME_ACCESS, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{FreeSid, SECURITY_CAPABILITIES};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
use windows::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_FREE};
use windows::Win32::System::ProcessStatus::{
    GetPerformanceInfo, GetProcessMemoryInfo, QueryWorkingSet, QueryWorkingSetEx,
    PERFORMANCE_INFORMATION, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, CREATE_SUSPENDED,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

pub const PAGE: usize = 4096;

pub fn werr(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xffff)
}

pub fn last_err() -> io::Error {
    io::Error::last_os_error()
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// An owned handle, closed on drop.
pub struct Owned(pub HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0 .0.is_null() {
            // SAFETY: we own the handle.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// SAFETY: a kernel handle value may be used from any thread.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

/// Duplicates `h` within this process with `access` (or the same access when
/// `access` is 0) and the inherit flag set.
pub fn dup_inheritable(h: HANDLE, access: u32) -> io::Result<Owned> {
    let mut out = HANDLE::default();
    let opts = if access == 0 {
        DUPLICATE_SAME_ACCESS
    } else {
        DUPLICATE_HANDLE_OPTIONS(0)
    };
    // SAFETY: plain handle duplication within this process.
    unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(me, h, me, &mut out, access, true, opts).map_err(werr)?;
    }
    Ok(Owned(out))
}

pub fn set_inherit(h: HANDLE, on: bool) {
    // SAFETY: changes a flag on a handle we own.
    unsafe {
        let _ = SetHandleInformation(
            h,
            HANDLE_FLAG_INHERIT.0,
            if on {
                HANDLE_FLAG_INHERIT
            } else {
                Default::default()
            },
        );
    }
}

// ------------------------------------------------------------ AppContainer

/// Creates the AppContainer profile `name` if needed.
pub fn appcontainer_profile(name: &str) -> io::Result<()> {
    let w = wide(name);
    // SAFETY: NUL-terminated strings; the SID is freed with FreeSid.
    unsafe {
        let sid = match CreateAppContainerProfile(
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            None,
        ) {
            Ok(s) => s,
            Err(_) => {
                DeriveAppContainerSidFromAppContainerName(PCWSTR(w.as_ptr())).map_err(werr)?
            }
        };
        FreeSid(sid);
    }
    Ok(())
}

pub fn delete_appcontainer_profile(name: &str) {
    let w = wide(name);
    // SAFETY: NUL-terminated name.
    unsafe {
        let _ = DeleteAppContainerProfile(PCWSTR(w.as_ptr()));
    }
}

pub struct Child {
    pub process: Owned,
    pub thread: Owned,
    pub pid: u32,
}

/// Starts `cmdline` with stdout set to `stdout`, stderr inherited, and the
/// extra `inherit` handles (which must be inheritable). Optionally
/// suspended and inside the AppContainer `container` (no capabilities).
pub fn spawn(
    cmdline: &str,
    container: Option<&str>,
    suspended: bool,
    stdout: HANDLE,
    inherit: &[HANDLE],
) -> io::Result<Child> {
    let mut cmd = wide(cmdline);
    // SAFETY: standard extended-startup-info process creation; every buffer
    // passed to the attribute list outlives CreateProcessW.
    unsafe {
        let err = dup_inheritable(GetStdHandle(STD_ERROR_HANDLE).map_err(werr)?, 0)?;
        let mut list: Vec<HANDLE> = vec![stdout, err.0];
        list.extend_from_slice(inherit);

        let sid = match container {
            Some(name) => {
                let w = wide(name);
                Some(DeriveAppContainerSidFromAppContainerName(PCWSTR(w.as_ptr())).map_err(werr)?)
            }
            None => None,
        };
        let caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid.unwrap_or_default(),
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let count = if sid.is_some() { 2 } else { 1 };
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, count, None, &mut size);
        let mut attr_buf = vec![0u64; size.div_ceil(8)];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(attrs), count, None, &mut size).map_err(werr)?;
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(list.as_ptr() as *const c_void),
            std::mem::size_of_val(list.as_slice()),
            None,
            None,
        )
        .map_err(werr)?;
        if sid.is_some() {
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some(&caps as *const _ as *const c_void),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                None,
                None,
            )
            .map_err(werr)?;
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdOutput = stdout;
        si.StartupInfo.hStdError = err.0;
        si.lpAttributeList = attrs;
        let mut flags: PROCESS_CREATION_FLAGS = EXTENDED_STARTUPINFO_PRESENT;
        if suspended {
            flags |= CREATE_SUSPENDED;
        }
        let mut pi = PROCESS_INFORMATION::default();
        let r = CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            None,
            PCWSTR::null(),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attrs);
        if let Some(s) = sid {
            FreeSid(s);
        }
        r.map_err(werr)?;
        Ok(Child {
            process: Owned(pi.hProcess),
            thread: Owned(pi.hThread),
            pid: pi.dwProcessId,
        })
    }
}

// ------------------------------------------------------- memory accounting

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcMem {
    pub working_set: usize,
    pub private_ws: usize,
    pub private_commit: usize,
}

pub fn proc_mem(p: HANDLE) -> ProcMem {
    let mut c = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        ..Default::default()
    };
    // SAFETY: the buffer is a PROCESS_MEMORY_COUNTERS_EX2 with cb set.
    unsafe {
        let _ = GetProcessMemoryInfo(p, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb);
    }
    ProcMem {
        working_set: c.WorkingSetSize,
        private_ws: c.PrivateWorkingSetSize,
        private_commit: c.PrivateUsage,
    }
}

/// Whole-process working set from `QueryWorkingSet`: (total, shared) pages.
pub fn ws_pages(p: HANDLE) -> (usize, usize) {
    let mut n = 1 << 16;
    loop {
        let mut buf = vec![0usize; n + 1];
        // SAFETY: the buffer holds a count plus n entries.
        let ok =
            unsafe { QueryWorkingSet(p, buf.as_mut_ptr() as *mut c_void, (buf.len() * 8) as u32) };
        let count = buf[0];
        if ok.is_ok() {
            let shared = buf[1..=count]
                .iter()
                .filter(|&&e| e & (1 << 8) != 0)
                .count();
            return (count, shared);
        }
        if count > n {
            n = count + 4096;
        } else {
            n *= 2;
        }
        if n > (1 << 26) {
            return (0, 0);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RangeWs {
    /// Pages of the range in the working set.
    pub valid: usize,
    /// Valid pages that are shareable (section pages not copied on write).
    pub shared: usize,
    /// Valid pages that are private to the process (copied on write).
    pub private: usize,
    /// Valid shared pages whose share count is at its maximum (7 or more).
    pub share7: usize,
}

/// Per-page working-set attributes of `[base, base+len)` in process `p`.
pub fn range_ws(p: HANDLE, base: usize, len: usize) -> RangeWs {
    let pages = len / PAGE;
    let mut buf = vec![[0usize; 2]; pages];
    for (i, e) in buf.iter_mut().enumerate() {
        e[0] = base + i * PAGE;
    }
    // SAFETY: PSAPI_WORKING_SET_EX_INFORMATION is (address, flags).
    let ok =
        unsafe { QueryWorkingSetEx(p, buf.as_mut_ptr() as *mut c_void, (buf.len() * 16) as u32) };
    let mut r = RangeWs::default();
    if ok.is_err() {
        return r;
    }
    for e in &buf {
        let f = e[1];
        if f & 1 == 0 {
            continue;
        }
        r.valid += 1;
        let share_count = (f >> 1) & 7;
        if f & (1 << 15) != 0 {
            r.shared += 1;
            if share_count == 7 {
                r.share7 += 1;
            }
        } else {
            r.private += 1;
        }
    }
    r
}

/// System commit total in bytes.
pub fn commit_total() -> usize {
    let mut pi = PERFORMANCE_INFORMATION {
        cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: the struct size is set.
    unsafe {
        let _ = GetPerformanceInfo(&mut pi, pi.cb);
    }
    pi.CommitTotal * pi.PageSize
}

/// True when `[base, base+len)` is entirely free in process `p`.
pub fn range_free(p: HANDLE, base: usize, len: usize) -> bool {
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: plain query into a local struct.
    let n = unsafe {
        VirtualQueryEx(
            p,
            Some(base as *const c_void),
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    n != 0
        && mbi.State == MEM_FREE
        && (mbi.BaseAddress as usize) <= base
        && (mbi.BaseAddress as usize + mbi.RegionSize) >= base + len
}

/// Allocations below 4 GiB in process `p`: (count, bytes, lowest base,
/// list of (base, size) of regions overlapping `[lo, hi)`).
pub fn low_regions(p: HANDLE, lo: usize, hi: usize) -> (usize, usize, Vec<(usize, usize)>) {
    let mut addr = 0x10000usize;
    let mut count = 0;
    let mut bytes = 0;
    let mut hits = Vec::new();
    while addr < 0x1_0000_0000 {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: plain query into a local struct.
        let n = unsafe {
            VirtualQueryEx(
                p,
                Some(addr as *const c_void),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if n == 0 {
            break;
        }
        let b = mbi.BaseAddress as usize;
        let s = mbi.RegionSize;
        if mbi.State != MEM_FREE {
            count += 1;
            bytes += s;
            if b < hi && b + s > lo {
                hits.push((b, s));
            }
        }
        addr = b + s;
    }
    (count, bytes, hits)
}

pub fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    v[i]
}

pub fn mib(b: usize) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

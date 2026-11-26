//! Thin wrappers over the Win32 calls the spike uses.
//!
//! Every `unsafe` block in the spike that calls Windows lives here, except
//! the ring buffer's raw shared-memory access (`ring.rs`) and the ntdll
//! keyed-event probe (`probe.rs`).

use std::ffi::c_void;
use std::io;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, LocalFree, DUPLICATE_HANDLE_OPTIONS, DUPLICATE_SAME_ACCESS,
    FILETIME, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, WAIT_EVENT, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, RevertToSelf, SecurityIdentification, TokenAppContainerSid,
    TokenImpersonationLevel, TokenIntegrityLevel, TokenIsAppContainer, TokenUser,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_IMPERSONATION_LEVEL,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, GetProcessTimes, OpenProcess,
    OpenProcessToken, OpenThreadToken, SetEvent, SetThreadAffinityMask, SignalObjectAndWait,
    WaitForSingleObject, INFINITE, PROCESS_ACCESS_RIGHTS,
};

/// Raw handle value that can cross threads and processes as a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raw(pub usize);

impl Raw {
    pub fn h(self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
    pub fn from(h: HANDLE) -> Raw {
        Raw(h.0 as usize)
    }
}

/// Owned handle, closed on drop.
#[derive(Debug)]
pub struct Owned(pub Raw);

impl Owned {
    pub fn raw(&self) -> Raw {
        self.0
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: we own this handle and close it exactly once.
        unsafe {
            let _ = CloseHandle(self.0.h());
        }
    }
}

fn werr(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xffff)
}

/// Closes a raw handle value owned by this process.
pub fn close(r: Raw) {
    // SAFETY: caller passes a handle it owns; a bad value only yields an error.
    unsafe {
        let _ = CloseHandle(r.h());
    }
}

// ---------------------------------------------------------------- sections

/// A pagefile-backed section mapped into this process.
pub struct Section {
    pub handle: Owned,
    pub base: *mut u8,
    pub size: usize,
}

// SAFETY: the mapping is process-wide; synchronization of its contents is
// done with atomics by the users of `base`.
unsafe impl Send for Section {}
// SAFETY: as above.
unsafe impl Sync for Section {}

impl Section {
    pub fn create(size: usize) -> io::Result<Section> {
        // SAFETY: anonymous (pagefile-backed) unnamed section, default security.
        let h = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                (size as u64 >> 32) as u32,
                size as u32,
                PCWSTR::null(),
            )
        }
        .map_err(werr)?;
        Self::map(Owned(Raw::from(h)), size)
    }

    /// Maps a section handle received from the broker.
    pub fn map(handle: Owned, size: usize) -> io::Result<Section> {
        // SAFETY: `handle` is a section handle; we map `size` bytes from offset 0.
        let v = unsafe { MapViewOfFile(handle.raw().h(), FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if v.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Section {
            handle,
            base: v.Value as *mut u8,
            size,
        })
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: `base` is the view we mapped; nobody uses it after drop.
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.base as *mut c_void,
            });
        }
    }
}

// ------------------------------------------------------------------ events

pub fn event_auto() -> io::Result<Owned> {
    // SAFETY: unnamed auto-reset event, initially not signaled.
    let h = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(werr)?;
    Ok(Owned(Raw::from(h)))
}

pub fn set_event(e: Raw) {
    // SAFETY: `e` is an event handle valid in this process.
    unsafe {
        let _ = SetEvent(e.h());
    }
}

pub fn wait(h: Raw, ms: u32) -> bool {
    // SAFETY: `h` is a waitable handle valid in this process.
    let r: WAIT_EVENT = unsafe { WaitForSingleObject(h.h(), ms) };
    r == WAIT_OBJECT_0
}

pub fn wait_infinite(h: Raw) -> bool {
    wait(h, INFINITE)
}

pub fn signal_and_wait(signal: Raw, wait_on: Raw) -> bool {
    // SAFETY: both are event handles valid in this process.
    let r = unsafe { SignalObjectAndWait(signal.h(), wait_on.h(), INFINITE, false) };
    r == WAIT_OBJECT_0
}

// --------------------------------------------------------------- processes

pub fn current_process() -> Raw {
    // SAFETY: pseudo handle, always valid.
    Raw::from(unsafe { GetCurrentProcess() })
}

pub fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Owned> {
    // SAFETY: plain OpenProcess; the result is checked.
    let h = unsafe { OpenProcess(access, false, pid) }.map_err(werr)?;
    Ok(Owned(Raw::from(h)))
}

/// Duplicates `src` (valid in `src_proc`) into `dst_proc` with the same access.
pub fn dup_into(src_proc: Raw, src: Raw, dst_proc: Raw) -> io::Result<Raw> {
    dup_opts(src_proc, src, dst_proc, DUPLICATE_SAME_ACCESS)
}

pub fn dup_opts(
    src_proc: Raw,
    src: Raw,
    dst_proc: Raw,
    opts: DUPLICATE_HANDLE_OPTIONS,
) -> io::Result<Raw> {
    let mut out = HANDLE::default();
    // SAFETY: process handles carry PROCESS_DUP_HANDLE; `out` is a valid out pointer.
    unsafe {
        DuplicateHandle(
            src_proc.h(),
            src.h(),
            dst_proc.h(),
            &mut out,
            0,
            false,
            opts,
        )
    }
    .map_err(werr)?;
    Ok(Raw::from(out))
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// User plus kernel CPU time of a process, in 100 ns units.
pub fn process_cpu(p: Raw) -> u64 {
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: `p` has PROCESS_QUERY_LIMITED_INFORMATION; out pointers are valid.
    let ok = unsafe { GetProcessTimes(p.h(), &mut c, &mut e, &mut k, &mut u) }.is_ok();
    if ok {
        ft(k) + ft(u)
    } else {
        0
    }
}

/// Pins the calling thread to one logical CPU, or unpins it (`None`).
pub fn pin_current_thread(cpu: Option<u32>) {
    let mask = match cpu {
        Some(c) => 1usize << c,
        None => {
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(64);
            if n >= usize::BITS as usize {
                usize::MAX
            } else {
                (1usize << n) - 1
            }
        }
    };
    // SAFETY: pseudo handle for the current thread; mask within the system's CPUs.
    unsafe {
        SetThreadAffinityMask(GetCurrentThread(), mask);
    }
}

// ------------------------------------------------------------------ tokens

#[derive(Clone, Debug, Default)]
pub struct TokenInfo {
    pub user_sid: String,
    pub is_appcontainer: bool,
    pub appcontainer_sid: Option<String>,
    pub integrity_rid: u32,
}

fn token_buf(tok: Raw, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer.
    let _ = unsafe { GetTokenInformation(tok.h(), class, None, 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: buffer of at least `len` bytes, 8-byte aligned.
    unsafe {
        GetTokenInformation(
            tok.h(),
            class,
            Some(buf.as_mut_ptr() as *mut c_void),
            (buf.len() * 8) as u32,
            &mut len,
        )
    }
    .map_err(werr)?;
    Ok(buf)
}

pub fn sid_string(sid: PSID) -> String {
    if sid.0.is_null() {
        return String::new();
    }
    let mut s = PWSTR::null();
    // SAFETY: `sid` points to a valid SID; `s` is freed with LocalFree below.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s).is_err() {
            return "<bad sid>".into();
        }
        let out = s.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
        out
    }
}

pub fn token_info(tok: Raw) -> io::Result<TokenInfo> {
    let mut ti = TokenInfo::default();
    let b = token_buf(tok, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER written by the system.
    ti.user_sid = sid_string(unsafe { (*(b.as_ptr() as *const TOKEN_USER)).User.Sid });
    let b = token_buf(tok, TokenIsAppContainer)?;
    ti.is_appcontainer = b[0] as u32 != 0;
    if ti.is_appcontainer {
        let b = token_buf(tok, TokenAppContainerSid)?;
        // SAFETY: the buffer holds a TOKEN_APPCONTAINER_INFORMATION.
        let sid =
            unsafe { (*(b.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION)).TokenAppContainer };
        ti.appcontainer_sid = Some(sid_string(sid));
    }
    let b = token_buf(tok, TokenIntegrityLevel)?;
    // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL; the SID's last
    // subauthority is the integrity RID.
    unsafe {
        let sid = (*(b.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid;
        let cnt = *windows::Win32::Security::GetSidSubAuthorityCount(sid);
        ti.integrity_rid = *windows::Win32::Security::GetSidSubAuthority(sid, cnt as u32 - 1);
    }
    Ok(ti)
}

pub fn process_token(proc_: Raw) -> io::Result<Owned> {
    let mut t = HANDLE::default();
    // SAFETY: `proc_` has PROCESS_QUERY_LIMITED_INFORMATION.
    unsafe { OpenProcessToken(proc_.h(), TOKEN_QUERY, &mut t) }.map_err(werr)?;
    Ok(Owned(Raw::from(t)))
}

/// While impersonating a named pipe client, opens the thread token and
/// checks that the client allowed at least identification level.
pub fn impersonation_token_of_pipe_client(pipe: Raw) -> io::Result<(Owned, i32)> {
    // SAFETY: `pipe` is a connected server end that has received data.
    unsafe {
        windows::Win32::System::Pipes::ImpersonateNamedPipeClient(pipe.h()).map_err(werr)?;
        let mut t = HANDLE::default();
        let r = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut t);
        let _ = RevertToSelf();
        r.map_err(werr)?;
        let tok = Owned(Raw::from(t));
        let b = token_buf(tok.raw(), TokenImpersonationLevel)?;
        let lvl = b[0] as i32;
        if lvl < SecurityIdentification.0 {
            return Err(io::Error::other("client denied identification"));
        }
        let _: SECURITY_IMPERSONATION_LEVEL = SECURITY_IMPERSONATION_LEVEL(lvl);
        Ok((tok, lvl))
    }
}

// ------------------------------------------------------- security descriptors

/// Security attributes built from SDDL; the descriptor is freed on drop.
pub struct Sddl {
    pub sa: SECURITY_ATTRIBUTES,
    sd: PSECURITY_DESCRIPTOR,
}

impl Sddl {
    pub fn new(sddl: &str) -> io::Result<Sddl> {
        let w: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: NUL-terminated wide string; `sd` is freed with LocalFree on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(w.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(werr)?;
        Ok(Sddl {
            sa: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            },
            sd,
        })
    }
}

impl Drop for Sddl {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

/// The current process user's SID as a string.
pub fn my_user_sid() -> String {
    process_token(current_process())
        .and_then(|t| token_info(t.raw()))
        .map(|i| i.user_sid)
        .unwrap_or_default()
}

// ------------------------------------------------------------ CPU topology

/// Returns (logical cpu index, efficiency class) for every logical CPU in group 0.
pub fn cpu_classes() -> Vec<(u32, u8)> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    let mut len = 0u32;
    // SAFETY: size query.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of `len` bytes.
    if unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut len,
        )
    }
    .is_err()
    {
        return vec![];
    }
    let mut out = vec![];
    let mut off = 0usize;
    let base = buf.as_ptr() as *const u8;
    while off < len as usize {
        // SAFETY: records are variable-size and chained by `Size` inside the buffer.
        unsafe {
            let rec = &*(base.add(off) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX);
            let p = &rec.Anonymous.Processor;
            let mask = p.GroupMask[0].Mask;
            for i in 0..usize::BITS {
                if mask & (1 << i) != 0 {
                    out.push((i, p.EfficiencyClass));
                }
            }
            off += rec.Size as usize;
        }
    }
    out.sort();
    out
}

//! Windows API wrappers for [`super`]: SIDs, AppContainer profiles,
//! security descriptors on files, and process creation inside an
//! AppContainer. All `unsafe` code of the module is here.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_NOT_FOUND, HANDLE, HLOCAL, LocalFree, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, BCryptHash,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    ACE_FLAGS, ACL, CONTAINER_INHERIT_ACE, CopySid, DACL_SECURITY_INFORMATION,
    DeriveCapabilitySidsFromName, FreeSid, GetLengthSid, GetSecurityDescriptorDacl,
    GetSecurityDescriptorSacl, GetTokenInformation, IsValidSid, LABEL_SECURITY_INFORMATION,
    OBJECT_INHERIT_ACE, OBJECT_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY, TokenAppContainerSid,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};
use windows::core::{PCWSTR, PWSTR};

/// `PROC_THREAD_ATTRIBUTE_JOB_LIST`: the process starts inside these jobs.
const PROC_THREAD_ATTRIBUTE_JOB_LIST: usize = 0x0002_000D;
/// `SE_GROUP_ENABLED`.
const SE_GROUP_ENABLED: u32 = 0x4;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn to_io(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

fn check(e: WIN32_ERROR) -> io::Result<()> {
    if e.0 == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e.0 as i32))
    }
}

/// Frees a `LocalAlloc`'d pointer returned by a Windows API.
fn local_free(p: *mut c_void) {
    if !p.is_null() {
        // SAFETY: callers pass only pointers that a Windows API allocated
        // with LocalAlloc and that nothing uses afterwards.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(p)));
        }
    }
}

/// A SID, owned as bytes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Sid(Vec<u8>);

impl Sid {
    /// Copies the SID `p` points to.
    ///
    /// # Safety
    /// `p` must point to a valid SID.
    unsafe fn copy_from(p: PSID) -> io::Result<Self> {
        // SAFETY: the caller guarantees a valid SID; the buffer has the
        // SID's length.
        unsafe {
            if !IsValidSid(p).as_bool() {
                return Err(io::Error::other("invalid SID"));
            }
            let n = GetLengthSid(p);
            let mut v = vec![0u8; n as usize];
            CopySid(n, PSID(v.as_mut_ptr().cast()), p).map_err(to_io)?;
            Ok(Self(v))
        }
    }

    /// Parses the string form (`S-1-...`).
    pub fn parse(s: &str) -> io::Result<Self> {
        let w = wide(OsStr::new(s));
        let mut p = PSID::default();
        // SAFETY: `w` is NUL-terminated; the SID is LocalAlloc'd, copied
        // and freed.
        unsafe {
            ConvertStringSidToSidW(PCWSTR(w.as_ptr()), &mut p).map_err(to_io)?;
            let r = Self::copy_from(p);
            local_free(p.0);
            r
        }
    }

    pub(super) fn psid(&self) -> PSID {
        PSID(self.0.as_ptr() as *mut c_void)
    }

    /// The string form (`S-1-15-2-...`).
    pub fn to_string_sid(&self) -> String {
        let mut out = PWSTR::null();
        // SAFETY: the SID is valid (checked at construction); the string is
        // LocalAlloc'd, copied and freed.
        unsafe {
            if ConvertSidToStringSidW(self.psid(), &mut out).is_err() {
                return String::new();
            }
            let s = out.to_string().unwrap_or_default();
            local_free(out.0.cast());
            s
        }
    }
}

impl std::fmt::Debug for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_sid())
    }
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> io::Result<[u8; 32]> {
    let mut out = [0u8; 32];
    // SAFETY: the pseudo handle needs no opening; both buffers are valid
    // for the call.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.ok().map_err(to_io)?;
    Ok(out)
}

/// `n` random bytes from the system's preferred generator.
pub fn random(n: usize) -> io::Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    // SAFETY: the buffer is valid; no algorithm handle with this flag.
    let status = unsafe { BCryptGenRandom(None, &mut v, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    status.ok().map_err(to_io)?;
    Ok(v)
}

/// Creates the AppContainer profile; returns its SID and whether it was
/// created now (false: it existed).
pub fn create_profile(name: &str, display: &str, description: &str) -> io::Result<(Sid, bool)> {
    let n = wide(OsStr::new(name));
    let d = wide(OsStr::new(display));
    let s = wide(OsStr::new(description));
    // SAFETY: NUL-terminated strings; the returned SID is copied and freed
    // with FreeSid as the API requires.
    unsafe {
        match CreateAppContainerProfile(
            PCWSTR(n.as_ptr()),
            PCWSTR(d.as_ptr()),
            PCWSTR(s.as_ptr()),
            None,
        ) {
            Ok(p) => {
                let r = Sid::copy_from(p);
                FreeSid(p);
                Ok((r?, true))
            }
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {
                Ok((derive_profile_sid(name)?, false))
            }
            Err(e) => Err(to_io(e)),
        }
    }
}

/// The SID of the AppContainer `name` (whether or not its profile exists).
pub fn derive_profile_sid(name: &str) -> io::Result<Sid> {
    let n = wide(OsStr::new(name));
    // SAFETY: NUL-terminated name; the SID is copied and freed with FreeSid.
    unsafe {
        let p = DeriveAppContainerSidFromAppContainerName(PCWSTR(n.as_ptr())).map_err(to_io)?;
        let r = Sid::copy_from(p);
        FreeSid(p);
        r
    }
}

/// Deletes the profile; a profile that does not exist is not an error.
pub fn delete_profile(name: &str) -> io::Result<()> {
    let n = wide(OsStr::new(name));
    // SAFETY: NUL-terminated name.
    match unsafe { DeleteAppContainerProfile(PCWSTR(n.as_ptr())) } {
        Ok(()) => Ok(()),
        Err(e)
            if e.code() == ERROR_NOT_FOUND.to_hresult()
                || e.code() == ERROR_FILE_NOT_FOUND.to_hresult() =>
        {
            Ok(())
        }
        Err(e) => Err(to_io(e)),
    }
}

/// The capability SID of a capability name (`internetClient`, or a name
/// of our own).
pub fn capability_sid(name: &str) -> io::Result<Sid> {
    let n = wide(OsStr::new(name));
    let mut groups: *mut PSID = std::ptr::null_mut();
    let mut ngroups = 0u32;
    let mut caps: *mut PSID = std::ptr::null_mut();
    let mut ncaps = 0u32;
    // SAFETY: the out pointers are valid; every returned SID and both
    // arrays are LocalAlloc'd and freed here after the first capability SID
    // is copied.
    unsafe {
        DeriveCapabilitySidsFromName(
            PCWSTR(n.as_ptr()),
            &mut groups,
            &mut ngroups,
            &mut caps,
            &mut ncaps,
        )
        .map_err(to_io)?;
        let r = if ncaps > 0 {
            Sid::copy_from(*caps)
        } else {
            Err(io::Error::other(format!("no capability SID for {name}")))
        };
        for i in 0..ngroups as usize {
            local_free((*groups.add(i)).0);
        }
        for i in 0..ncaps as usize {
            local_free((*caps.add(i)).0);
        }
        local_free(groups.cast());
        local_free(caps.cast());
        r
    }
}

/// A security descriptor built from SDDL, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(OsStr::new(sddl));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: NUL-terminated input; the descriptor is owned by `Self`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(w.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(to_io)?;
        }
        Ok(Self(sd))
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = Default::default();
        let mut defaulted = Default::default();
        let mut acl = std::ptr::null_mut();
        // SAFETY: the descriptor is valid; the ACL points into it.
        unsafe {
            GetSecurityDescriptorDacl(self.0, &mut present, &mut acl, &mut defaulted)
                .map_err(to_io)?
        };
        Ok(acl)
    }

    fn sacl(&self) -> io::Result<*mut ACL> {
        let mut present = Default::default();
        let mut defaulted = Default::default();
        let mut acl = std::ptr::null_mut();
        // SAFETY: as in `dacl`.
        unsafe {
            GetSecurityDescriptorSacl(self.0, &mut present, &mut acl, &mut defaulted)
                .map_err(to_io)?
        };
        Ok(acl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        local_free(self.0.0);
    }
}

fn set_named(
    path: &Path,
    info: OBJECT_SECURITY_INFORMATION,
    dacl: Option<*const ACL>,
    sacl: Option<*const ACL>,
) -> io::Result<()> {
    let w = wide(path.as_os_str());
    // SAFETY: NUL-terminated path; the ACLs point into live descriptors
    // owned by the caller. Windows propagates inheritable entries to the
    // existing children.
    unsafe {
        check(SetNamedSecurityInfoW(
            PCWSTR(w.as_ptr()),
            SE_FILE_OBJECT,
            info,
            None,
            None,
            dacl,
            sacl,
        ))
    }
}

/// Replaces the DACL of `path` (protected: nothing inherited from the
/// parent) with the one in `sddl`, and its mandatory label with the one in
/// `sddl` when there is one. The DACL is set first: setting the label
/// needs WRITE_OWNER, which the owner has only through an ACE.
pub fn set_descriptor(path: &Path, sddl: &str) -> io::Result<()> {
    let sd = Descriptor::from_sddl(sddl)?;
    set_named(
        path,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        Some(sd.dacl()?),
        None,
    )?;
    let sacl = sd.sacl()?;
    if !sacl.is_null() {
        set_named(path, LABEL_SECURITY_INFORMATION, None, Some(sacl))?;
    }
    Ok(())
}

/// Creates directory `path` with the security descriptor `sddl`.
pub fn create_dir_with(path: &Path, sddl: &str) -> io::Result<()> {
    let sd = Descriptor::from_sddl(sddl)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    let w = wide(path.as_os_str());
    // SAFETY: NUL-terminated path; `sa` points to a live descriptor.
    unsafe { CreateDirectoryW(PCWSTR(w.as_ptr()), Some(&sa)).map_err(to_io) }
}

/// Adds an inheritable allow ACE for `sid` with `mask` to the DACL of
/// `path`, keeping the existing entries.
pub fn add_ace(path: &Path, sid: &Sid, mask: u32) -> io::Result<()> {
    let w = wide(path.as_os_str());
    // SAFETY: the descriptor from GetNamedSecurityInfo owns `dacl` and is
    // freed after SetEntriesInAcl copied it; the new ACL is freed after
    // SetNamedSecurityInfo.
    unsafe {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        check(GetNamedSecurityInfoW(
            PCWSTR(w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        ))?;
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: ACE_FLAGS(OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0),
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: PWSTR(sid.psid().0.cast()),
                ..Default::default()
            },
        };
        let mut new: *mut ACL = std::ptr::null_mut();
        let r = SetEntriesInAclW(Some(&[ea]), Some(dacl), &mut new);
        local_free(sd.0);
        check(r)?;
        let r = set_named(path, DACL_SECURITY_INFORMATION, Some(new), None);
        local_free(new.cast());
        r
    }
}

/// What to start inside an AppContainer.
pub struct Spawn<'a> {
    pub application: &'a Path,
    /// The full command line, `argv[0]` included, already quoted.
    pub command_line: &'a str,
    pub app_container: &'a Sid,
    pub capabilities: &'a [Sid],
    /// stdin, stdout, stderr; `None` gives the child no handle.
    pub stdio: [Option<BorrowedHandle<'a>>; 3],
    /// Further handles the child inherits.
    pub inherit: &'a [BorrowedHandle<'a>],
    /// A job the process starts in (before it runs any code).
    pub job: Option<BorrowedHandle<'a>>,
    /// The environment block (`NAME=value` entries); `None` inherits sysd's.
    pub environment: Option<&'a [String]>,
    pub current_dir: Option<&'a Path>,
    pub suspended: bool,
}

pub struct Spawned {
    pub process: OwnedHandle,
    pub thread: OwnedHandle,
    pub pid: u32,
    /// The child's value of each handle of [`Spawn::inherit`], in order.
    pub inherited: Vec<usize>,
}

/// An inheritable duplicate of `h` in this process, closed on drop (after
/// CreateProcess copied it into the child).
fn inheritable(h: BorrowedHandle<'_>) -> io::Result<OwnedHandle> {
    let mut out = HANDLE::default();
    // SAFETY: duplicates a handle this process holds into this process.
    unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(
            me,
            HANDLE(h.as_raw_handle()),
            me,
            &mut out,
            0,
            true,
            DUPLICATE_SAME_ACCESS,
        )
        .map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(out.0))
    }
}

/// Starts a process inside an AppContainer. The child inherits exactly
/// the handles given (PROC_THREAD_ATTRIBUTE_HANDLE_LIST), as inheritable
/// duplicates made for this call only.
pub fn spawn(s: &Spawn<'_>) -> io::Result<Spawned> {
    let stdio: Vec<Option<OwnedHandle>> = s
        .stdio
        .iter()
        .map(|h| h.map(inheritable).transpose())
        .collect::<io::Result<_>>()?;
    let extra: Vec<OwnedHandle> = s
        .inherit
        .iter()
        .map(|h| inheritable(*h))
        .collect::<io::Result<_>>()?;
    let raw = |h: &Option<OwnedHandle>| {
        h.as_ref()
            .map(|h| HANDLE(h.as_raw_handle()))
            .unwrap_or_default()
    };
    let mut list: Vec<HANDLE> = stdio
        .iter()
        .flatten()
        .chain(extra.iter())
        .map(|h| HANDLE(h.as_raw_handle()))
        .collect();
    list.dedup();
    let mut caps: Vec<SID_AND_ATTRIBUTES> = s
        .capabilities
        .iter()
        .map(|c| SID_AND_ATTRIBUTES {
            Sid: c.psid(),
            Attributes: SE_GROUP_ENABLED,
        })
        .collect();
    let sc = SECURITY_CAPABILITIES {
        AppContainerSid: s.app_container.psid(),
        Capabilities: if caps.is_empty() {
            std::ptr::null_mut()
        } else {
            caps.as_mut_ptr()
        },
        CapabilityCount: caps.len() as u32,
        Reserved: 0,
    };
    let jobs: Vec<HANDLE> = s.job.iter().map(|j| HANDLE(j.as_raw_handle())).collect();
    let env: Option<Vec<u16>> = s.environment.map(|e| {
        let mut v: Vec<u16> = Vec::new();
        for entry in e {
            v.extend(OsStr::new(entry).encode_wide());
            v.push(0);
        }
        v.push(0);
        if e.is_empty() {
            v.push(0);
        }
        v
    });
    let app = wide(s.application.as_os_str());
    let mut cmd = wide(OsStr::new(s.command_line));
    let cwd = s.current_dir.map(|d| wide(d.as_os_str()));
    let count = 2 + u32::from(!list.is_empty()) + u32::from(!jobs.is_empty());
    let mut size = 0usize;
    // SAFETY: the first call only reports the size the list needs.
    let _ = unsafe { InitializeProcThreadAttributeList(None, count, None, &mut size) };
    let mut buf = vec![0u64; size.div_ceil(8)];
    let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
    // SAFETY: `buf` has the size reported above.
    unsafe {
        InitializeProcThreadAttributeList(Some(attrs), count, None, &mut size).map_err(to_io)?
    };
    // SAFETY: every buffer the attribute list points to (`list`, `sc` and
    // the capabilities it points to, `jobs`) and the strings passed to
    // CreateProcessW live until after the call; the list is deleted on
    // every path; returned handles are owned right after.
    let r = unsafe {
        (|| -> io::Result<Spawned> {
            if !list.is_empty() {
                UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(list.as_ptr().cast()),
                    std::mem::size_of_val(list.as_slice()),
                    None,
                    None,
                )
                .map_err(to_io)?;
            }
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some((&sc as *const SECURITY_CAPABILITIES).cast()),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                None,
                None,
            )
            .map_err(to_io)?;
            if !jobs.is_empty() {
                UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST,
                    Some(jobs.as_ptr().cast()),
                    std::mem::size_of_val(jobs.as_slice()),
                    None,
                    None,
                )
                .map_err(to_io)?;
            }
            let mut si = STARTUPINFOEXW::default();
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = raw(&stdio[0]);
            si.StartupInfo.hStdOutput = raw(&stdio[1]);
            si.StartupInfo.hStdError = raw(&stdio[2]);
            si.lpAttributeList = attrs;
            let mut flags: PROCESS_CREATION_FLAGS =
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NEW_PROCESS_GROUP;
            if s.suspended {
                flags |= CREATE_SUSPENDED;
            }
            if env.is_some() {
                flags |= CREATE_UNICODE_ENVIRONMENT;
            }
            let mut pi = PROCESS_INFORMATION::default();
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                !list.is_empty(),
                flags,
                env.as_ref().map(|e| e.as_ptr() as *const c_void),
                cwd.as_ref()
                    .map(|c| PCWSTR(c.as_ptr()))
                    .unwrap_or(PCWSTR::null()),
                &si.StartupInfo,
                &mut pi,
            )
            .map_err(to_io)?;
            Ok(Spawned {
                process: OwnedHandle::from_raw_handle(pi.hProcess.0),
                thread: OwnedHandle::from_raw_handle(pi.hThread.0),
                pid: pi.dwProcessId,
                inherited: extra.iter().map(|h| h.as_raw_handle() as usize).collect(),
            })
        })()
    };
    // SAFETY: initialized above; deleted once.
    unsafe { DeleteProcThreadAttributeList(attrs) };
    drop(caps);
    r
}

/// Resumes the first thread of a process created suspended.
pub fn resume(thread: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a thread handle this process owns.
    if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The AppContainer SID in the token of `process`, `None` when the
/// process does not run in an AppContainer.
pub fn process_app_container_sid(process: BorrowedHandle<'_>) -> io::Result<Option<Sid>> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a valid handle; the token handle is owned right
    // after.
    let token = unsafe {
        OpenProcessToken(HANDLE(process.as_raw_handle()), TOKEN_QUERY, &mut token)
            .map_err(to_io)?;
        OwnedHandle::from_raw_handle(token.0)
    };
    let mut buf = vec![0u64; 16];
    let mut n = 0u32;
    // SAFETY: the buffer is 128 bytes, enough for the struct and a SID; the
    // SID pointer in the result points into the buffer.
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenAppContainerSid,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut n,
        )
        .map_err(to_io)?;
        let info = &*(buf.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION);
        if info.TokenAppContainer.0.is_null() {
            return Ok(None);
        }
        Sid::copy_from(info.TokenAppContainer).map(Some)
    }
}

/// The user SID of this process.
pub fn current_user_sid() -> io::Result<Sid> {
    use windows::Win32::Security::{TOKEN_USER, TokenUser};
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; the token handle is owned
    // right after.
    let token = unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(to_io)?;
        OwnedHandle::from_raw_handle(token.0)
    };
    let mut buf = vec![0u64; 16];
    let mut n = 0u32;
    // SAFETY: a 128-byte buffer holds TOKEN_USER and its SID; the SID
    // pointer points into the buffer.
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut n,
        )
        .map_err(to_io)?;
        let info = &*(buf.as_ptr() as *const TOKEN_USER);
        Sid::copy_from(info.User.Sid)
    }
}

/// Waits up to `ms` for the process to end; its exit code, or `None` on
/// timeout.
pub fn wait_process(process: &OwnedHandle, ms: u32) -> io::Result<Option<u32>> {
    use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    let h = HANDLE(process.as_raw_handle());
    // SAFETY: a process handle this process owns; `code` is a valid out
    // pointer.
    unsafe {
        match WaitForSingleObject(h, ms) {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                GetExitCodeProcess(h, &mut code).map_err(to_io)?;
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// Ends the process with `code`.
pub fn terminate(process: &OwnedHandle, code: u32) {
    use windows::Win32::System::Threading::TerminateProcess;
    // SAFETY: a process handle this process owns.
    unsafe {
        let _ = TerminateProcess(HANDLE(process.as_raw_handle()), code);
    }
}

/// Opens process `pid` for querying its token.
pub fn open_process_for_query(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: no memory preconditions; the handle is owned right after.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

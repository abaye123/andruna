//! Windows helpers for the spike: SIDs, AppContainer profiles, ACLs and
//! integrity labels, pipes, and process creation inside an AppContainer.

use std::ffi::c_void;
use std::io;
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, ERROR_ALREADY_EXISTS, HANDLE,
    HANDLE_FLAG_INHERIT, HLOCAL, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, REVOKE_ACCESS, SDDL_REVISION_1, SE_FILE_OBJECT,
    TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    CopySid, DeriveCapabilitySidsFromName, FreeSid, GetLengthSid, GetSecurityDescriptorSacl,
    GetTokenInformation, TokenIntegrityLevel, TokenIsAppContainer, ACE_FLAGS, ACL,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, UpdateProcThreadAttribute,
    CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` (less privileged
/// AppContainer when set to opt out).
const ATTR_ALL_APP_PACKAGES_POLICY: usize = 0x0002_000F;
const ALL_APP_PACKAGES_OPT_OUT: u32 = 1;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn wpath(p: &Path) -> Vec<u16> {
    wide(&p.display().to_string())
}

pub fn werr(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

fn w32(e: WIN32_ERROR) -> io::Result<()> {
    if e.0 == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e.0 as i32))
    }
}

/// An owned kernel handle.
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

// SAFETY: a handle value may be used from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above.
unsafe impl Sync for Owned {}

/// A SID held as bytes.
#[derive(Clone)]
pub struct Sid(Vec<u8>);

impl Sid {
    /// Copies a SID from a pointer.
    ///
    /// # Safety
    /// `p` must point to a valid SID.
    unsafe fn copy(p: PSID) -> Sid {
        // SAFETY: caller guarantees a valid SID.
        unsafe {
            let n = GetLengthSid(p);
            let mut v = vec![0u8; n as usize];
            CopySid(n, PSID(v.as_mut_ptr() as *mut c_void), p).expect("CopySid");
            Sid(v)
        }
    }

    pub fn parse(s: &str) -> io::Result<Sid> {
        let w = wide(s);
        let mut p = PSID::default();
        // SAFETY: NUL-terminated input; the result is LocalAlloc'd and freed.
        unsafe {
            ConvertStringSidToSidW(PCWSTR(w.as_ptr()), &mut p).map_err(werr)?;
            let s = Sid::copy(p);
            let _ = LocalFree(Some(HLOCAL(p.0)));
            Ok(s)
        }
    }

    pub fn psid(&self) -> PSID {
        PSID(self.0.as_ptr() as *mut c_void)
    }

    pub fn string(&self) -> String {
        let mut out = PWSTR::null();
        // SAFETY: the SID is valid; the string is LocalAlloc'd and freed.
        unsafe {
            ConvertSidToStringSidW(self.psid(), &mut out).expect("ConvertSidToStringSid");
            let s = out.to_string().unwrap_or_default();
            let _ = LocalFree(Some(HLOCAL(out.0 as *mut c_void)));
            s
        }
    }
}

/// `ALL APPLICATION PACKAGES`.
pub fn all_app_packages() -> Sid {
    Sid::parse("S-1-15-2-1").unwrap()
}

/// `ALL RESTRICTED APPLICATION PACKAGES` (what less privileged
/// AppContainers match instead).
pub fn all_restricted_app_packages() -> Sid {
    Sid::parse("S-1-15-2-2").unwrap()
}

/// Creates the AppContainer profile `name`; returns its SID and whether it
/// was created now.
pub fn create_profile(name: &str) -> io::Result<(Sid, bool)> {
    let w = wide(name);
    // SAFETY: NUL-terminated strings; the SID is freed with FreeSid.
    unsafe {
        match CreateAppContainerProfile(
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            None,
        ) {
            Ok(p) => {
                let s = Sid::copy(p);
                FreeSid(p);
                Ok((s, true))
            }
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {
                let p =
                    DeriveAppContainerSidFromAppContainerName(PCWSTR(w.as_ptr())).map_err(werr)?;
                let s = Sid::copy(p);
                FreeSid(p);
                Ok((s, false))
            }
            Err(e) => Err(werr(e)),
        }
    }
}

pub fn delete_profile(name: &str) -> io::Result<()> {
    let w = wide(name);
    // SAFETY: NUL-terminated name.
    unsafe { DeleteAppContainerProfile(PCWSTR(w.as_ptr())).map_err(werr) }
}

/// The capability SID of a named capability (`internetClient`, or a custom
/// name).
pub fn capability_sid(name: &str) -> io::Result<Sid> {
    let w = wide(name);
    let mut groups: *mut PSID = std::ptr::null_mut();
    let mut ngroups = 0u32;
    let mut caps: *mut PSID = std::ptr::null_mut();
    let mut ncaps = 0u32;
    // SAFETY: out pointers are valid; every returned SID and both arrays are
    // LocalAlloc'd and freed here.
    unsafe {
        DeriveCapabilitySidsFromName(
            PCWSTR(w.as_ptr()),
            &mut groups,
            &mut ngroups,
            &mut caps,
            &mut ncaps,
        )
        .map_err(werr)?;
        let s = if ncaps > 0 {
            Some(Sid::copy(*caps))
        } else {
            None
        };
        for i in 0..ngroups as usize {
            let _ = LocalFree(Some(HLOCAL((*groups.add(i)).0)));
        }
        for i in 0..ncaps as usize {
            let _ = LocalFree(Some(HLOCAL((*caps.add(i)).0)));
        }
        let _ = LocalFree(Some(HLOCAL(groups as *mut c_void)));
        let _ = LocalFree(Some(HLOCAL(caps as *mut c_void)));
        s.ok_or_else(|| io::Error::other("no capability SID"))
    }
}

pub const FILE_GENERIC_READ_EXECUTE: u32 = 0x0012_00A9;
pub const FILE_ALL: u32 = 0x001F_01FF;

/// Adds (or with `revoke`, removes) an inheritable ACE for `sid` on `path`.
/// Windows propagates it to the existing children.
pub fn set_access(path: &Path, sid: &Sid, mask: u32, revoke: bool) -> io::Result<()> {
    let w = wpath(path);
    // SAFETY: the security descriptor returned by GetNamedSecurityInfo owns
    // the DACL pointer and is freed after SetEntriesInAcl copied it; the
    // new ACL is freed after use.
    unsafe {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        w32(GetNamedSecurityInfoW(
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
            grfAccessMode: if revoke { REVOKE_ACCESS } else { GRANT_ACCESS },
            grfInheritance: ACE_FLAGS(OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0),
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: PWSTR(sid.psid().0 as *mut u16),
                ..Default::default()
            },
        };
        let mut new: *mut ACL = std::ptr::null_mut();
        let r = SetEntriesInAclW(Some(&[ea]), Some(dacl), &mut new);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        w32(r)?;
        let r = SetNamedSecurityInfoW(
            PCWSTR(w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new),
            None,
        );
        let _ = LocalFree(Some(HLOCAL(new as *mut c_void)));
        w32(r)
    }
}

/// Gives `path` (and, inherited, its contents) a low mandatory label with
/// no-write-up, so low-integrity processes can write there.
pub fn set_low_label(path: &Path) -> io::Result<()> {
    let w = wpath(path);
    let sddl = wide("S:(ML;OICI;NW;;;LW)");
    // SAFETY: the descriptor is LocalAlloc'd and freed after use; the SACL
    // pointer points into it.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(werr)?;
        let mut present = Default::default();
        let mut defaulted = Default::default();
        let mut sacl: *mut ACL = std::ptr::null_mut();
        let r = GetSecurityDescriptorSacl(sd, &mut present, &mut sacl, &mut defaulted);
        let r = r.map_err(werr).and_then(|_| {
            w32(SetNamedSecurityInfoW(
                PCWSTR(w.as_ptr()),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                None,
                None,
                None,
                Some(sacl),
            ))
        });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// Creates directory `path` with the security descriptor `sddl` (DACL and
/// mandatory label). Setting a label later needs WRITE_OWNER, which the
/// owner does not have implicitly; at creation it does not.
pub fn create_dir_sd(path: &Path, sddl: &str) -> io::Result<()> {
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    let w = wpath(path);
    let s = wide(sddl);
    // SAFETY: the descriptor is LocalAlloc'd, used for the call and freed.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(s.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(werr)?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let r = CreateDirectoryW(PCWSTR(w.as_ptr()), Some(&sa)).map_err(werr);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// An anonymous pipe:(read end, write end). `inherit_write` makes the
/// write end inheritable (child's stdout), otherwise the read end (child's
/// stdin).
pub fn pipe(inherit_write: bool) -> io::Result<(Owned, Owned)> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut r = HANDLE::default();
    let mut w = HANDLE::default();
    // SAFETY: out pointers are valid; the handles are owned right after.
    unsafe {
        CreatePipe(&mut r, &mut w, Some(&sa), 1 << 16).map_err(werr)?;
        let (keep, _) = if inherit_write { (r, w) } else { (w, r) };
        let _ = SetHandleInformation(keep, HANDLE_FLAG_INHERIT.0, Default::default());
    }
    Ok((Owned(r), Owned(w)))
}

pub struct Container<'a> {
    pub sid: &'a Sid,
    pub capabilities: &'a [Sid],
    /// Less privileged AppContainer: `ALL APPLICATION PACKAGES` grants do
    /// not apply.
    pub lpac: bool,
}

pub struct Child {
    pub process: Owned,
    pub thread: Owned,
    pub pid: u32,
}

/// Starts `cmdline` with the given standard handles (inheritable) and extra
/// inherited handles, optionally inside an AppContainer and suspended.
pub fn spawn(
    cmdline: &str,
    container: Option<&Container>,
    suspended: bool,
    stdio: [HANDLE; 3],
    inherit: &[HANDLE],
) -> io::Result<Child> {
    let mut cmd = wide(cmdline);
    let mut list: Vec<HANDLE> = Vec::new();
    for h in stdio.iter().chain(inherit) {
        if !list.contains(h) {
            list.push(*h);
        }
    }
    // SAFETY: extended startup info process creation; every buffer the
    // attribute list points to (handle list, capabilities, policy) lives
    // until after CreateProcessW.
    unsafe {
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        let mut sc = SECURITY_CAPABILITIES::default();
        let policy = ALL_APP_PACKAGES_OPT_OUT;
        let mut count = 1;
        if let Some(c) = container {
            caps = c
                .capabilities
                .iter()
                .map(|s| SID_AND_ATTRIBUTES {
                    Sid: s.psid(),
                    Attributes: 4, // SE_GROUP_ENABLED
                })
                .collect();
            sc = SECURITY_CAPABILITIES {
                AppContainerSid: c.sid.psid(),
                Capabilities: if caps.is_empty() {
                    std::ptr::null_mut()
                } else {
                    caps.as_mut_ptr()
                },
                CapabilityCount: caps.len() as u32,
                Reserved: 0,
            };
            count += 1;
            if c.lpac {
                count += 1;
            }
        }
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, count, None, &mut size);
        let mut buf = vec![0u64; size.div_ceil(8)];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(attrs), count, None, &mut size).map_err(werr)?;
        let r = (|| -> io::Result<Child> {
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
            if let Some(c) = container {
                UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some(&sc as *const _ as *const c_void),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                    None,
                    None,
                )
                .map_err(werr)?;
                if c.lpac {
                    UpdateProcThreadAttribute(
                        attrs,
                        0,
                        ATTR_ALL_APP_PACKAGES_POLICY,
                        Some(&policy as *const u32 as *const c_void),
                        4,
                        None,
                        None,
                    )
                    .map_err(werr)?;
                }
            }
            let mut si = STARTUPINFOEXW::default();
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = stdio[0];
            si.StartupInfo.hStdOutput = stdio[1];
            si.StartupInfo.hStdError = stdio[2];
            si.lpAttributeList = attrs;
            let mut flags: PROCESS_CREATION_FLAGS = EXTENDED_STARTUPINFO_PRESENT;
            if suspended {
                flags |= CREATE_SUSPENDED;
            }
            let mut pi = PROCESS_INFORMATION::default();
            CreateProcessW(
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
            )
            .map_err(werr)?;
            Ok(Child {
                process: Owned(pi.hProcess),
                thread: Owned(pi.hThread),
                pid: pi.dwProcessId,
            })
        })();
        DeleteProcThreadAttributeList(attrs);
        drop(caps);
        r
    }
}

/// (is AppContainer, integrity level RID) of this process's token.
pub fn own_token() -> (bool, u32) {
    // SAFETY: queries into local buffers of the sizes given.
    unsafe {
        let mut t = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t).is_err() {
            return (false, 0);
        }
        let t = Owned(t);
        let mut ac = 0u32;
        let mut n = 0u32;
        let _ = GetTokenInformation(
            t.0,
            TokenIsAppContainer,
            Some(&mut ac as *mut u32 as *mut c_void),
            4,
            &mut n,
        );
        let mut buf = vec![0u64; 16];
        let mut il = 0;
        if GetTokenInformation(
            t.0,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr() as *mut c_void),
            128,
            &mut n,
        )
        .is_ok()
        {
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let sid = Sid::copy(label.Label.Sid).string();
            il = sid
                .rsplit('-')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        }
        (ac != 0, il)
    }
}

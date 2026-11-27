//! Wrappers around the Windows APIs this crate uses. All `unsafe` code of
//! the crate is here.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{PCWSTR, PWSTR};

/// Converts a `windows` error to `io::Error`, keeping Win32 error codes.
pub fn to_io(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn handle(h: &impl AsRawHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// A security identifier, held as its binary form.
#[derive(Clone, PartialEq, Eq)]
pub struct Sid(Vec<u8>);

impl std::fmt::Debug for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sid({})", self.to_string_sid())
    }
}

impl Sid {
    /// The `S-1-...` string form.
    pub fn to_string_sid(&self) -> String {
        let mut out = PWSTR::null();
        // SAFETY: the buffer holds a valid SID (it was copied from a token by
        // `token_user_sid`); `out` receives a LocalAlloc string that is read
        // and freed below.
        unsafe {
            if ConvertSidToStringSidW(PSID(self.0.as_ptr() as *mut c_void), &mut out).is_err() {
                return String::from("S-?");
            }
            let s = out.to_string().unwrap_or_default();
            LocalFree(Some(HLOCAL(out.0 as *mut c_void)));
            s
        }
    }
}

fn token_user_sid(token: &OwnedHandle) -> io::Result<Sid> {
    let mut len = 0u32;
    // SAFETY: a size query with no buffer; `len` receives the needed size.
    let first = unsafe { GetTokenInformation(handle(token), TokenUser, None, 0, &mut len) };
    if let Err(e) = first
        && e.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
    {
        return Err(to_io(e));
    }
    // u64 elements keep the TOKEN_USER header aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `len` bytes and aligned for TOKEN_USER.
    unsafe {
        GetTokenInformation(
            handle(token),
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .map_err(to_io)?;
    }
    // SAFETY: the call above filled `buf` with a TOKEN_USER whose SID
    // pointer points into `buf`; the SID is copied out before `buf` drops.
    unsafe {
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = tu.User.Sid;
        let n = GetLengthSid(sid) as usize;
        let bytes = std::slice::from_raw_parts(sid.0 as *const u8, n).to_vec();
        Ok(Sid(bytes))
    }
}

/// The user SID of this process.
pub fn current_user_sid() -> io::Result<Sid> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process needs no closing;
    // `token` receives a new handle that is owned right after.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(to_io)? };
    // SAFETY: `token` is a valid handle owned by nobody else.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    token_user_sid(&token)
}

/// The user SID of process `pid`.
pub fn process_user_sid(pid: u32) -> io::Result<Sid> {
    // SAFETY: OpenProcess has no memory preconditions; the handle is owned
    // right after.
    let process = unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(to_io)?;
        OwnedHandle::from_raw_handle(h.0)
    };
    let mut token = HANDLE::default();
    // SAFETY: `process` is valid; `token` receives a handle owned right after.
    let token = unsafe {
        OpenProcessToken(handle(&process), TOKEN_QUERY, &mut token).map_err(to_io)?;
        OwnedHandle::from_raw_handle(token.0)
    };
    token_user_sid(&token)
}

/// The Terminal Services session id of this process.
pub fn current_session_id() -> io::Result<u32> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out pointer.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id).map_err(to_io)? };
    Ok(id)
}

/// A self-relative security descriptor built from SDDL.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is an immutable heap block after creation; it is
// only read by the system calls that receive it.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above, shared reads only.
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `w` is NUL-terminated; `sd` receives a LocalAlloc block
        // freed in `drop`.
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

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the block came from ConvertStringSecurityDescriptor... and
        // is freed once.
        unsafe {
            LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

const PIPE_BUFFER: u32 = 64 * 1024;

/// Creates one server instance of a byte-mode pipe that rejects remote
/// clients. `first` fails when any instance of the name already exists,
/// which keeps another process from owning the name.
pub fn create_pipe_instance(
    name: &[u16],
    first: bool,
    sd: &SecurityDescriptor,
) -> io::Result<OwnedHandle> {
    let mut open = PIPE_ACCESS_DUPLEX;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let sa = sd.attributes();
    // SAFETY: `name` is NUL-terminated (from `wide`), `sa` points to a live
    // descriptor; the returned handle is checked and owned.
    unsafe {
        let h = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            Some(&sa),
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

/// Waits until a client connects to the server instance `h`.
pub fn connect_pipe_instance(h: &OwnedHandle) -> io::Result<()> {
    // SAFETY: `h` is a synchronous pipe server handle; no OVERLAPPED.
    match unsafe { ConnectNamedPipe(handle(h), None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(e) => Err(to_io(e)),
    }
}

pub fn pipe_client_pid(h: &OwnedHandle) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: `pid` is a valid out pointer, `h` a pipe handle.
    unsafe { GetNamedPipeClientProcessId(handle(h), &mut pid).map_err(to_io)? };
    Ok(pid)
}

pub fn pipe_server_pid(h: &OwnedHandle) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: `pid` is a valid out pointer, `h` a pipe handle.
    unsafe { GetNamedPipeServerProcessId(handle(h), &mut pid).map_err(to_io)? };
    Ok(pid)
}

/// Opens the client end of a pipe. The server may only identify, not
/// impersonate, the caller.
pub fn open_pipe_client(name: &[u16]) -> io::Result<OwnedHandle> {
    // SAFETY: `name` is NUL-terminated; the handle is owned right after.
    unsafe {
        let h = CreateFileW(
            PCWSTR(name.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(SECURITY_SQOS_PRESENT.0 | SECURITY_IDENTIFICATION.0),
            None,
        )
        .map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

/// Waits up to `ms` for a free instance of an existing pipe. Returns false
/// at once when no pipe of that name exists.
pub fn wait_named_pipe(name: &[u16], ms: u32) -> bool {
    // SAFETY: `name` is NUL-terminated.
    unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), ms).as_bool() }
}

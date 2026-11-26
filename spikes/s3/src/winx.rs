//! Named pipe, anonymous pipe and AppContainer helpers.

use std::ffi::c_void;
use std::io;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{FreeSid, SECURITY_CAPABILITIES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, CreatePipe, GetNamedPipeClientProcessId,
    SetNamedPipeHandleState, TransactNamedPipe, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetProcessHandleCount,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::win::{sid_string, Owned, Raw, Sddl};

fn werr(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xffff)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

// ------------------------------------------------------------- named pipes

/// Creates one message-mode server instance of a local-only named pipe.
pub fn pipe_server(name: &str, first: bool, sa: &Sddl) -> io::Result<Owned> {
    let w = wide(name);
    let mut open = PIPE_ACCESS_DUPLEX;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: NUL-terminated name; `sa` outlives the call.
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(w.as_ptr()),
            open,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            128 * 1024,
            128 * 1024,
            0,
            Some(&sa.sa),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Owned(Raw::from(h)))
}

pub fn pipe_accept(p: Raw) -> io::Result<()> {
    // SAFETY: synchronous connect on a server instance.
    match unsafe { ConnectNamedPipe(p.h(), None) } {
        Ok(()) => Ok(()),
        Err(e) if (e.code().0 & 0xffff) == 535 => Ok(()), // ERROR_PIPE_CONNECTED
        Err(e) => Err(werr(e)),
    }
}

pub fn pipe_client_pid(p: Raw) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: connected server end; valid out pointer.
    unsafe { GetNamedPipeClientProcessId(p.h(), &mut pid) }.map_err(werr)?;
    Ok(pid)
}

/// Opens the pipe as a client, allowing the server identification only
/// (not impersonation), in message read mode.
pub fn pipe_connect(name: &str) -> io::Result<Owned> {
    let w = wide(name);
    // SAFETY: NUL-terminated name; flags request identification-level SQOS.
    let h = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            None,
        )
    }
    .map_err(werr)?;
    let own = Owned(Raw::from(h));
    let mode = PIPE_READMODE_MESSAGE;
    // SAFETY: valid pipe handle and mode pointer.
    unsafe { SetNamedPipeHandleState(h, Some(&mode), None, None) }.map_err(werr)?;
    Ok(own)
}

pub fn pipe_read(p: Raw, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0u32;
    // SAFETY: synchronous read into `buf`.
    unsafe { ReadFile(p.h(), Some(buf), Some(&mut n), None) }.map_err(werr)?;
    Ok(n as usize)
}

pub fn pipe_write(p: Raw, buf: &[u8]) -> io::Result<()> {
    let mut n = 0u32;
    // SAFETY: synchronous write from `buf`.
    unsafe { WriteFile(p.h(), Some(buf), Some(&mut n), None) }.map_err(werr)
}

pub fn pipe_transact(p: Raw, req: &[u8], out: &mut [u8]) -> io::Result<usize> {
    let mut n = 0u32;
    // SAFETY: synchronous transact; buffers outlive the call.
    unsafe {
        TransactNamedPipe(
            p.h(),
            Some(req.as_ptr() as *const c_void),
            req.len() as u32,
            Some(out.as_mut_ptr() as *mut c_void),
            out.len() as u32,
            &mut n,
            None,
        )
    }
    .map_err(werr)?;
    Ok(n as usize)
}

/// Anonymous pipe pair (read end, write end).
pub fn anon_pipe() -> io::Result<(Owned, Owned)> {
    let (mut r, mut w) = (HANDLE::default(), HANDLE::default());
    // SAFETY: valid out pointers.
    unsafe { CreatePipe(&mut r, &mut w, None, 0) }.map_err(werr)?;
    Ok((Owned(Raw::from(r)), Owned(Raw::from(w))))
}

pub fn handle_count() -> u32 {
    let mut n = 0u32;
    // SAFETY: pseudo handle; valid out pointer.
    unsafe {
        let _ = GetProcessHandleCount(GetCurrentProcess(), &mut n);
    }
    n
}

// ------------------------------------------------------------ AppContainer

/// Returns the SID string of the AppContainer profile `name`, creating it if needed.
pub fn appcontainer_profile(name: &str) -> io::Result<String> {
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
        let s = sid_string(sid);
        FreeSid(sid);
        Ok(s)
    }
}

pub fn delete_appcontainer_profile(name: &str) {
    let w = wide(name);
    // SAFETY: NUL-terminated name.
    unsafe {
        let _ = DeleteAppContainerProfile(PCWSTR(w.as_ptr()));
    }
}

/// Starts `cmdline` inside the AppContainer `name` with no capabilities.
/// Only stdout and stderr are inherited. Returns (process handle, pid).
pub fn spawn_in_appcontainer(name: &str, cmdline: &str) -> io::Result<(Owned, u32)> {
    let wname = wide(name);
    let mut cmd = wide(cmdline);
    // SAFETY: standard extended-startup-info process creation; every buffer
    // passed to the attribute list outlives CreateProcessW.
    unsafe {
        let sid =
            DeriveAppContainerSidFromAppContainerName(PCWSTR(wname.as_ptr())).map_err(werr)?;
        let caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid,
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        // Inheritable duplicates of stdout and stderr.
        let me = GetCurrentProcess();
        let mut inh = [HANDLE::default(); 2];
        for (i, which) in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .into_iter()
            .enumerate()
        {
            let s = GetStdHandle(which).map_err(werr)?;
            DuplicateHandle(me, s, me, &mut inh[i], 0, true, DUPLICATE_SAME_ACCESS)
                .map_err(werr)?;
        }
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 2, None, &mut size);
        let mut attr_buf = vec![0u64; size.div_ceil(8)];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(attrs), 2, None, &mut size).map_err(werr)?;
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
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inh.as_ptr() as *const c_void),
            std::mem::size_of_val(&inh),
            None,
            None,
        )
        .map_err(werr)?;
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdOutput = inh[0];
        si.StartupInfo.hStdError = inh[1];
        si.lpAttributeList = attrs;
        let mut pi = PROCESS_INFORMATION::default();
        let r = CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attrs);
        for h in inh {
            let _ = CloseHandle(h);
        }
        FreeSid(sid);
        r.map_err(werr)?;
        let _ = CloseHandle(pi.hThread);
        Ok((Owned(Raw::from(pi.hProcess)), pi.dwProcessId))
    }
}

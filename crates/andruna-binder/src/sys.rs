//! Wrappers around the Windows APIs of the transport. The `unsafe` calls
//! of the host and device sides are here.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{
    DUPLICATE_HANDLE_OPTIONS, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_PIPE_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_EVENT, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    SetNamedPipeHandleState, TransactNamedPipe, WaitNamedPipeW,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcess, PROCESS_ACCESS_RIGHTS, SetEvent,
    SignalObjectAndWait, WaitForSingleObject,
};
use windows::core::PCWSTR;

pub use windows::Win32::System::Threading::PROCESS_DUP_HANDLE;

pub fn to_io(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn raw(h: &impl AsRawHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// Takes ownership of a raw handle value.
///
/// # Safety
///
/// `v` must be a valid handle of this process that nothing else closes.
pub unsafe fn own(v: u64) -> OwnedHandle {
    // SAFETY: contract.
    unsafe { OwnedHandle::from_raw_handle(v as usize as *mut c_void) }
}

/// A pagefile-backed section of `size` bytes.
pub fn create_section(size: usize) -> io::Result<OwnedHandle> {
    let size = size as u64;
    // SAFETY: no name, no security attributes; the handle is owned at once.
    unsafe {
        let h = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            (size >> 32) as u32,
            size as u32,
            PCWSTR::null(),
        )
        .map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

/// A read-write view of `size` bytes of `section`.
pub fn map_view(section: &OwnedHandle, size: usize) -> io::Result<*mut u8> {
    // SAFETY: a section handle; the view is unmapped with `unmap_view`.
    let v = unsafe { MapViewOfFile(raw(section), FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, size) };
    if v.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(v.Value.cast())
}

/// Unmaps a view from [`map_view`].
///
/// # Safety
///
/// Nothing may use the view afterwards.
pub unsafe fn unmap_view(p: *mut u8) {
    // SAFETY: contract; a failure leaves nothing to do.
    let _ = unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: p.cast() }) };
}

/// An auto-reset event, not signaled.
pub fn create_event() -> io::Result<OwnedHandle> {
    // SAFETY: no name or attributes; owned at once.
    unsafe {
        let h = CreateEventW(None, false, false, PCWSTR::null()).map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

pub fn set_event(h: HANDLE) {
    // SAFETY: an event handle owned by the caller.
    let _ = unsafe { SetEvent(h) };
}

/// Signals `signal` and waits for `wait` in one call; whether `wait` was
/// signaled (false: timeout or failure).
pub fn signal_and_wait(signal: HANDLE, wait: HANDLE, ms: u32) -> bool {
    // SAFETY: two event handles owned by the caller.
    let r = unsafe { SignalObjectAndWait(signal, wait, ms, false) };
    r == WAIT_OBJECT_0
}

/// Waits for `h`; whether it was signaled.
pub fn wait(h: HANDLE, ms: Option<u32>) -> bool {
    // SAFETY: a waitable handle owned by the caller.
    let r: WAIT_EVENT = unsafe { WaitForSingleObject(h, ms.unwrap_or(INFINITE)) };
    r == WAIT_OBJECT_0
}

pub fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<OwnedHandle> {
    // SAFETY: plain call; owned at once.
    unsafe {
        let h = OpenProcess(access, false, pid).map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

/// Duplicates `h` of process `src` into process `dst` (same access, or
/// `access` when given); the value in `dst`.
pub fn duplicate(src: HANDLE, h: HANDLE, dst: HANDLE, access: Option<u32>) -> io::Result<u64> {
    let mut out = HANDLE::default();
    let (acc, opts) = match access {
        Some(a) => (a, DUPLICATE_HANDLE_OPTIONS(0)),
        None => (0, DUPLICATE_SAME_ACCESS),
    };
    // SAFETY: process handles with PROCESS_DUP_HANDLE (or the current
    // process pseudo handle); `out` is a valid out pointer.
    unsafe { DuplicateHandle(src, h, dst, &mut out, acc, false, opts).map_err(to_io)? };
    Ok(out.0 as u64)
}

pub fn current_process() -> HANDLE {
    // SAFETY: returns a pseudo handle.
    unsafe { GetCurrentProcess() }
}

/// A server instance of the message-mode pipe `name`.
pub fn create_pipe(name: &[u16]) -> io::Result<OwnedHandle> {
    // SAFETY: `name` is NUL-terminated; default security (the user and
    // SYSTEM); the handle is checked and owned.
    unsafe {
        let h = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle::from_raw_handle(h.0))
    }
}

pub fn connect_pipe(h: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a synchronous pipe server handle; no OVERLAPPED.
    match unsafe { ConnectNamedPipe(raw(h), None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(e) => Err(to_io(e)),
    }
}

pub fn pipe_client_pid(h: &OwnedHandle) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: valid out pointer, a pipe handle.
    unsafe { GetNamedPipeClientProcessId(raw(h), &mut pid).map_err(to_io)? };
    Ok(pid)
}

/// Reads one message (at most `buf.len()` bytes); its length.
pub fn read_msg(h: &OwnedHandle, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0u32;
    // SAFETY: `buf` is writable for its length; synchronous handle.
    unsafe { ReadFile(raw(h), Some(buf), Some(&mut n), None).map_err(to_io)? };
    Ok(n as usize)
}

pub fn write_msg(h: &OwnedHandle, buf: &[u8]) -> io::Result<()> {
    let mut n = 0u32;
    // SAFETY: `buf` is readable; synchronous handle.
    unsafe { WriteFile(raw(h), Some(buf), Some(&mut n), None).map_err(to_io)? };
    Ok(())
}

/// A private channel: the server end, and an inheritable client end in
/// message mode, for a child process.
pub fn create_channel() -> io::Result<(OwnedHandle, OwnedHandle)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
    use windows::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE;
    static N: AtomicU64 = AtomicU64::new(0);
    let name = wide(&format!(
        r"\\.\pipe\andruna-binder-channel-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    // SAFETY: `name` is NUL-terminated; one instance only, so nothing else
    // can take the name; the handle is checked and owned.
    let server = unsafe {
        let h = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(h.0)
    };
    let client = open_pipe(&name)?;
    // SAFETY: a handle owned by `client`.
    unsafe {
        SetHandleInformation(
            raw(&client),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS(HANDLE_FLAG_INHERIT.0),
        )
        .map_err(to_io)?
    };
    Ok((server, client))
}

/// Opens the client end of the pipe `name` in message mode, waiting up to
/// five seconds for a free instance.
pub fn open_pipe(name: &[u16]) -> io::Result<OwnedHandle> {
    // SAFETY: `name` is NUL-terminated.
    unsafe {
        let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), 5000);
    }
    // SAFETY: as above; owned at once.
    let h = unsafe {
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
        OwnedHandle::from_raw_handle(h.0)
    };
    let mode = PIPE_READMODE_MESSAGE;
    // SAFETY: a pipe client handle; valid pointer to the mode.
    unsafe { SetNamedPipeHandleState(raw(&h), Some(&mode), None, None).map_err(to_io)? };
    Ok(h)
}

/// Writes `req` and reads the reply in one call.
pub fn transact(h: &OwnedHandle, req: &[u8], reply: &mut [u8]) -> io::Result<usize> {
    let mut n = 0u32;
    // SAFETY: both buffers valid for their lengths; synchronous handle.
    unsafe {
        TransactNamedPipe(
            raw(h),
            Some(req.as_ptr().cast()),
            req.len() as u32,
            Some(reply.as_mut_ptr().cast()),
            reply.len() as u32,
            &mut n,
            None,
        )
        .map_err(to_io)?
    };
    Ok(n as usize)
}

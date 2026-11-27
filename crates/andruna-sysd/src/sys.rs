//! Wrappers around the Windows APIs sysd uses. All `unsafe` code of the
//! crate is here.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_RTLREADING, DT_WORDBREAK,
    DrawTextW, EndPaint, HBRUSH, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetSystemTime;
use windows::Win32::System::Threading::{CreateMutexW, SetEvent};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW,
    DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClientRect, GetMessageW,
    GetWindowThreadProcessId, IDC_ARROW, IsIconic, IsWindowVisible, LoadCursorW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_PAINT,
    WM_QUERYENDSESSION, WM_SIZE, WNDCLASSEXW, WS_OVERLAPPED, WS_OVERLAPPEDWINDOW,
};
use windows::core::{BOOL, PCWSTR};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn handle(h: &impl AsRawHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

fn to_io(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn utc_timestamp() -> String {
    // SAFETY: no preconditions.
    let t = unsafe { GetSystemTime() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// The user's `%LOCALAPPDATA%` (the known folder, not the environment).
pub fn local_app_data() -> io::Result<PathBuf> {
    // SAFETY: the GUID is a static; the returned string is CoTaskMemAlloc'd,
    // copied, then freed.
    unsafe {
        let p =
            SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).map_err(to_io)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(PathBuf::from(s.map_err(io::Error::other)?))
    }
}

/// Stops host processes from inheriting sysd's standard handles (which
/// may be a pipe of whoever started sysd). Hosts get null stdio, but
/// Windows passes every inheritable handle on.
pub fn make_std_handles_private() {
    use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
    let handles = [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ];
    for h in handles {
        if h.is_null() {
            continue;
        }
        // SAFETY: changes a flag of a handle this process holds; an invalid
        // value makes the call fail harmlessly.
        unsafe {
            let _ = SetHandleInformation(HANDLE(h), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
        }
    }
}

/// The primary language id of the user's UI language (`LANG_*`).
pub fn ui_primary_language() -> u16 {
    // SAFETY: no preconditions.
    unsafe { GetUserDefaultUILanguage() & 0x3FF }
}

/// Creates the named mutex. `Ok(None)` when it already exists, that is,
/// another instance holds the name. The mutex is not acquired; existence
/// alone marks the instance, and the handle keeps it alive.
pub fn create_single_instance_mutex(name: &str) -> io::Result<Option<OwnedHandle>> {
    let w = wide(name);
    // SAFETY: `w` is NUL-terminated; the handle is owned right after and
    // GetLastError is read before any other call.
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(w.as_ptr())).map_err(to_io)?;
        let existed = GetLastError() == ERROR_ALREADY_EXISTS;
        let h = OwnedHandle::from_raw_handle(h.0);
        Ok(if existed { None } else { Some(h) })
    }
}

/// Signals and closes an event handle inherited from the launcher, whose
/// value was passed on the command line.
pub fn signal_inherited_event(value: usize) -> io::Result<()> {
    let h = HANDLE(value as *mut c_void);
    // SAFETY: the value names an inherited event handle owned by this
    // process; a wrong value makes SetEvent fail, and closing an unrelated
    // handle cannot happen because SetEvent succeeded only on an event.
    unsafe {
        SetEvent(h).map_err(to_io)?;
        let _ = CloseHandle(h);
    }
    Ok(())
}

/// A job object that kills its processes when sysd's handle closes, so app
/// processes never outlive sysd.
pub struct Job(OwnedHandle);

impl Job {
    pub fn new() -> io::Result<Self> {
        // SAFETY: an unnamed job; the handle is owned right after, and the
        // info struct is a valid, fully initialized value.
        unsafe {
            let h = CreateJobObjectW(None, PCWSTR::null()).map_err(to_io)?;
            let job = OwnedHandle::from_raw_handle(h.0);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle(&job),
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of_val(&info) as u32,
            )
            .map_err(to_io)?;
            Ok(Self(job))
        }
    }

    pub fn assign(&self, process: &impl AsRawHandle) -> io::Result<()> {
        // SAFETY: both handles are valid for the call.
        unsafe { AssignProcessToJobObject(handle(&self.0), handle(process)).map_err(to_io) }
    }

    pub fn terminate(&self, code: u32) {
        // SAFETY: the job handle is valid.
        unsafe {
            let _ = TerminateJobObject(handle(&self.0), code);
        }
    }
}

/// Lets process `pid` take the foreground, if this process may.
pub fn allow_foreground(pid: u32) {
    // SAFETY: no memory preconditions.
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

struct FindWindow {
    pid: u32,
    found: HWND,
}

unsafe extern "system" fn find_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `FindWindow` passed by `focus_process_window`,
    // alive for the whole EnumWindows call.
    let fw = unsafe { &mut *(lparam.0 as *mut FindWindow) };
    let mut pid = 0;
    // SAFETY: `hwnd` comes from EnumWindows; `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: as above.
    if pid == fw.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        fw.found = hwnd;
        return false.into();
    }
    true.into()
}

/// Restores and brings forward the first visible top-level window of
/// process `pid`. False when it has none.
pub fn focus_process_window(pid: u32) -> bool {
    let mut fw = FindWindow {
        pid,
        found: HWND::default(),
    };
    // SAFETY: the callback only reads `fw` through the LPARAM while
    // EnumWindows runs; `fw` outlives the call. EnumWindows reports an
    // error when the callback stops early, which is expected.
    unsafe {
        let _ = EnumWindows(Some(find_window_proc), LPARAM(&mut fw as *mut _ as isize));
    }
    if fw.found.is_invalid() {
        return false;
    }
    // SAFETY: `found` is a window handle; a stale one makes these fail
    // harmlessly.
    unsafe {
        if IsIconic(fw.found).as_bool() {
            let _ = ShowWindow(fw.found, SW_RESTORE);
        }
        let _ = SetForegroundWindow(fw.found);
    }
    true
}

/// A window handle that can cross threads; only used to post messages.
#[derive(Clone, Copy, Debug)]
pub struct WindowRef(isize);

impl WindowRef {
    /// Asks the window to close; its thread then leaves the message loop.
    pub fn post_close(self) {
        // SAFETY: PostMessage to a destroyed window fails harmlessly.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.0 as *mut c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

unsafe extern "system" fn control_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // The session ends after this returns; app processes end
                // with the job when the process exits.
                crate::info!("end_session");
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is this window, on its own thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling with the arguments Windows passed.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_class(
    name: &[u16],
    proc_: windows::Win32::UI::WindowsAndMessaging::WNDPROC,
) -> io::Result<()> {
    // SAFETY: `name` is NUL-terminated and static for the process (callers
    // pass leaked or long-lived buffers); the struct is fully initialized.
    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).map_err(to_io)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: proc_,
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut c_void),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Creates sysd's hidden control window. It is a top-level window that is
/// never shown, not a message-only window, because only top-level windows
/// receive `WM_QUERYENDSESSION` and `WM_ENDSESSION` at logoff. Must be
/// called on the thread that then runs [`run_message_loop`].
pub fn create_control_window() -> io::Result<WindowRef> {
    let class = wide("Andruna.sysd.Control");
    register_class(&class, Some(control_wnd_proc))?;
    let title = wide("Andruna system server");
    // SAFETY: the class was registered above; the buffers live through the
    // call.
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
        .map_err(to_io)?;
        Ok(WindowRef(hwnd.0 as isize))
    }
}

/// Runs the thread's message loop until `WM_QUIT`.
pub fn run_message_loop() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out pointer; GetMessage returns 0 on WM_QUIT
    // and -1 on error, both end the loop.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

struct HostText {
    text: Vec<u16>,
    rtl: bool,
}

static HOST_TEXT: OnceLock<HostText> = OnceLock::new();

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: painting `hwnd` on its own thread with a valid struct;
            // the text is copied into a local buffer for DrawText.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                rc.left += 24;
                rc.top += 24;
                rc.right -= 24;
                if let Some(t) = HOST_TEXT.get() {
                    let mut buf = t.text.clone();
                    let mut fmt = DT_WORDBREAK | DT_NOPREFIX;
                    fmt |= if t.rtl {
                        DT_RIGHT | DT_RTLREADING
                    } else {
                        DT_LEFT
                    };
                    DrawTextW(hdc, &mut buf, &mut rc, fmt);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            // SAFETY: repaint the whole client area of this window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling with the arguments Windows passed.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Shows the placeholder host's window with `text`. Call once, then run
/// [`run_message_loop`]; closing the window ends the loop.
pub fn show_host_window(title: &str, text: &str, rtl: bool) -> io::Result<()> {
    let _ = HOST_TEXT.set(HostText {
        text: text.encode_utf16().collect(),
        rtl,
    });
    let class = wide("Andruna.Host.Placeholder");
    register_class(&class, Some(host_wnd_proc))?;
    let title = wide(title);
    let ex = WINDOW_EX_STYLE(0);
    // SAFETY: the class was registered above; the buffers live through the
    // call.
    unsafe {
        let hwnd = CreateWindowExW(
            ex,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            640,
            360,
            None,
            None,
            None,
            None,
        )
        .map_err(to_io)?;
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

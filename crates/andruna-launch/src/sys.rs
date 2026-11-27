//! Wrappers around the Windows APIs the launcher uses. All `unsafe` code of
//! the crate is here.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    CreateEventW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE, WaitForMultipleObjects,
};
use windows::Win32::UI::Controls::{
    TASKDIALOG_FLAGS, TASKDIALOGCONFIG, TD_ERROR_ICON, TD_WARNING_ICON, TDCBF_CLOSE_BUTTON,
    TDCBF_NO_BUTTON, TDCBF_YES_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_RTL_LAYOUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_RIGHT, MB_RTLREADING,
    MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
};
use windows::core::{BOOL, HRESULT, PCWSTR, s, w};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Stops child processes from inheriting this process's standard handles.
/// sysd gets null stdio, but Windows passes every inheritable handle on; a
/// long-lived sysd holding the caller's output pipe would keep whoever
/// reads the launcher's output waiting until sysd exits.
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

/// Lets process `pid` take the foreground. The launcher has the right
/// because the user just started it; passing it on lets sysd bring the
/// app's window forward.
pub fn allow_foreground(pid: u32) {
    // SAFETY: no memory preconditions.
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

/// Whether a named mutex exists in this session.
pub fn mutex_exists(name: &str) -> bool {
    let w = wide(name);
    // SAFETY: `w` is a NUL-terminated UTF-16 string that outlives the call;
    // the returned handle is owned and closed right away.
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(w.as_ptr())) {
            Ok(h) => {
                drop(OwnedHandle::from_raw_handle(h.0));
                true
            }
            Err(_) => false,
        }
    }
}

/// A manual-reset event whose handle is inherited by child processes. Its
/// value is passed to sysd, which signals it when its pipe is ready.
pub struct InheritableEvent(OwnedHandle);

impl InheritableEvent {
    pub fn new() -> io::Result<Self> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        // SAFETY: `sa` is valid for the call; the handle is owned right
        // after.
        unsafe {
            let h = CreateEventW(Some(&sa), true, false, PCWSTR::null())
                .map_err(|e| io::Error::other(e.message()))?;
            Ok(Self(OwnedHandle::from_raw_handle(h.0)))
        }
    }

    /// The handle value as the child sees it (inherited handles keep their
    /// value).
    pub fn value(&self) -> usize {
        self.0.as_raw_handle() as usize
    }
}

pub enum Wait {
    /// The event was signaled.
    Signaled,
    /// The process exited first.
    Exited,
    TimedOut,
}

/// Waits for the event or the end of the process, whichever comes first.
pub fn wait_event_or_exit(
    event: &InheritableEvent,
    process: &impl AsRawHandle,
    timeout: Duration,
) -> Wait {
    let handles = [
        HANDLE(event.0.as_raw_handle()),
        HANDLE(process.as_raw_handle()),
    ];
    let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
    // SAFETY: both handles are valid for the duration of the call.
    let r = unsafe { WaitForMultipleObjects(&handles, false, ms) };
    if r == WAIT_OBJECT_0 {
        Wait::Signaled
    } else if r.0 == WAIT_OBJECT_0.0 + 1 {
        Wait::Exited
    } else {
        Wait::TimedOut
    }
}

type TaskDialogIndirectFn =
    unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// Shows an error. Uses the task dialog (Common Controls 6, selected by the
/// embedded manifest), loaded only on this path so that the normal launch
/// does not load comctl32; falls back to a message box.
pub fn show_error(title: &str, instruction: &str, content: &str, rtl: bool) {
    let title_w = wide(title);
    let instruction_w = wide(instruction);
    let content_w = wide(content);
    // SAFETY: comctl32 is a system DLL; the symbol, when present, has the
    // documented TaskDialogIndirect signature. The config struct and its
    // strings live until the call returns.
    let shown = unsafe {
        LoadLibraryW(w!("comctl32.dll"))
            .ok()
            .and_then(|m| GetProcAddress(m, s!("TaskDialogIndirect")))
            .map(|f| {
                let f: TaskDialogIndirectFn = std::mem::transmute(f);
                let mut flags = TDF_ALLOW_DIALOG_CANCELLATION;
                if rtl {
                    flags = TASKDIALOG_FLAGS(flags.0 | TDF_RTL_LAYOUT.0);
                }
                let mut cfg = TASKDIALOGCONFIG {
                    cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
                    dwFlags: flags,
                    dwCommonButtons: TDCBF_CLOSE_BUTTON,
                    pszWindowTitle: PCWSTR(title_w.as_ptr()),
                    pszMainInstruction: PCWSTR(instruction_w.as_ptr()),
                    pszContent: PCWSTR(content_w.as_ptr()),
                    ..Default::default()
                };
                cfg.Anonymous1.pszMainIcon = TD_ERROR_ICON;
                f(
                    &cfg,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
                .is_ok()
            })
            .unwrap_or(false)
    };
    if shown {
        return;
    }
    let text = wide(&format!("{instruction}\n\n{content}"));
    let mut style = MB_OK | MB_ICONERROR | MB_SETFOREGROUND;
    if rtl {
        style |= MB_RTLREADING | MB_RIGHT;
    }
    // SAFETY: both strings are NUL-terminated and live through the call.
    unsafe {
        MessageBoxW(
            None::<HWND>,
            PCWSTR(text.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            style,
        );
    }
}

/// The answer to [`confirm`].
pub struct Confirmation {
    pub yes: bool,
    /// The state of the verification check box.
    pub checked: bool,
}

/// Asks a yes/no question with a check box (task dialog), or a plain
/// yes/no message box where the task dialog is not available (the check
/// box then counts as checked, the safer answer for "keep data").
pub fn confirm(
    title: &str,
    instruction: &str,
    content: &str,
    check: &str,
    rtl: bool,
) -> Confirmation {
    let title_w = wide(title);
    let instruction_w = wide(instruction);
    let content_w = wide(content);
    let check_w = wide(check);
    let mut button = 0i32;
    let mut checked = BOOL(0);
    // SAFETY: as in `show_error`; the out pointers are valid locals.
    let shown = unsafe {
        LoadLibraryW(w!("comctl32.dll"))
            .ok()
            .and_then(|m| GetProcAddress(m, s!("TaskDialogIndirect")))
            .map(|f| {
                let f: TaskDialogIndirectFn = std::mem::transmute(f);
                let mut flags = TDF_ALLOW_DIALOG_CANCELLATION;
                if rtl {
                    flags = TASKDIALOG_FLAGS(flags.0 | TDF_RTL_LAYOUT.0);
                }
                let mut cfg = TASKDIALOGCONFIG {
                    cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
                    dwFlags: flags,
                    dwCommonButtons: TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                    pszWindowTitle: PCWSTR(title_w.as_ptr()),
                    pszMainInstruction: PCWSTR(instruction_w.as_ptr()),
                    pszContent: PCWSTR(content_w.as_ptr()),
                    pszVerificationText: PCWSTR(check_w.as_ptr()),
                    nDefaultButton: IDYES.0,
                    ..Default::default()
                };
                cfg.Anonymous1.pszMainIcon = TD_WARNING_ICON;
                f(&cfg, &mut button, std::ptr::null_mut(), &mut checked).is_ok()
            })
            .unwrap_or(false)
    };
    if shown {
        return Confirmation {
            yes: button == IDYES.0,
            checked: checked.as_bool(),
        };
    }
    let text = wide(&format!(
        "{instruction}

{content}"
    ));
    let mut style = MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND;
    if rtl {
        style |= MB_RTLREADING | MB_RIGHT;
    }
    // SAFETY: both strings are NUL-terminated and live through the call.
    let r = unsafe {
        MessageBoxW(
            None::<HWND>,
            PCWSTR(text.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            style,
        )
    };
    Confirmation {
        yes: r == IDYES,
        checked: true,
    }
}

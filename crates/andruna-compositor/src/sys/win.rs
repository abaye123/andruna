//! Top-level windows: the window class, the Windows 11 frame (Mica, the
//! caption drawn by us with DWM's caption buttons), DPI, monitors and the
//! message loop.
//!
//! The window procedure forwards every message to one handler per thread
//! ([`set_handler`]); the compositor's logic is safe code on top of the
//! wrappers here.

use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::rc::Rc;

use windows::Win32::Foundation::{
    COLORREF, ERROR_SUCCESS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_CAPTION_BUTTON_BOUNDS, DWMWA_CAPTION_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    DwmDefWindowProc, DwmExtendFrameIntoClientArea, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ScreenToClient, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL};
use windows::Win32::UI::Input::Pointer::EnableMouseInPointer;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::geom::{Rect, Size};

pub use windows::Win32::Foundation::HWND as RawHwnd;

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A window handle (no ownership: windows belong to their thread).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hwnd(pub isize);

impl Hwnd {
    pub fn raw(self) -> HWND {
        HWND(self.0 as *mut c_void)
    }

    fn from(h: HWND) -> Self {
        Hwnd(h.0 as isize)
    }
}

/// A message as the window procedure receives it.
#[derive(Clone, Copy, Debug)]
pub struct Msg {
    pub hwnd: Hwnd,
    pub msg: u32,
    pub wparam: usize,
    pub lparam: isize,
}

type Handler = Rc<dyn Fn(Msg) -> Option<isize>>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

/// Sets the handler of every window message on this thread. Returning
/// `None` runs the default processing.
pub fn set_handler(h: impl Fn(Msg) -> Option<isize> + 'static) {
    HANDLER.with(|c| *c.borrow_mut() = Some(Rc::new(h)));
}

pub fn clear_handler() {
    HANDLER.with(|c| *c.borrow_mut() = None);
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handler = HANDLER.with(|c| c.borrow().clone());
    let m = Msg {
        hwnd: Hwnd::from(hwnd),
        msg,
        wparam: wparam.0,
        lparam: lparam.0,
    };
    // A panic must not unwind into Windows.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.and_then(|h| h(m))
    }))
    .unwrap_or(None);
    match r {
        Some(v) => LRESULT(v),
        // SAFETY: forwarding the message as received.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

const CLASS: PCWSTR = w!("Andruna.Window");

/// Makes this thread per-monitor DPI aware (v2) and routes mouse input
/// through pointer messages (process wide; failing when already enabled is
/// fine).
pub fn init_thread() {
    // SAFETY: no pointers; both calls only change process/thread state.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = EnableMouseInPointer(true);
    }
}

fn register_class() -> io::Result<HINSTANCE> {
    // SAFETY: the module handle of this executable.
    let inst: HINSTANCE = unsafe { GetModuleHandleW(None) }.map_err(to_io)?.into();
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: inst,
        // SAFETY: a system cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialized; registering twice fails harmlessly.
    unsafe { RegisterClassExW(&wc) };
    Ok(inst)
}

/// Creates a hidden top-level window whose content is drawn only by
/// DirectComposition, with the given outer size at the position Windows
/// chooses.
pub fn create_window(title: &str, client: Size, dpi: u32) -> io::Result<Hwnd> {
    let inst = register_class()?;
    let style = WS_OVERLAPPEDWINDOW;
    // The caption is part of the client area ([`nccalcsize`]): the window
    // adds only the side and bottom resize borders.
    let outer_w = client.width + 2 * frame_x(dpi);
    let outer_h = client.height + frame_y(dpi);
    let t = wide(title);
    // SAFETY: class registered above; strings live across the call.
    let h = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW,
            CLASS,
            PCWSTR(t.as_ptr()),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            outer_w,
            outer_h,
            None,
            None,
            Some(inst),
            None,
        )
    }
    .map_err(to_io)?;
    Ok(Hwnd::from(h))
}

/// The resize border height at `dpi` (frame plus padding).
pub fn frame_y(dpi: u32) -> i32 {
    // SAFETY: no pointers.
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

/// The resize border width at `dpi`.
pub fn frame_x(dpi: u32) -> i32 {
    // SAFETY: no pointers.
    unsafe { GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

fn set_attr<T>(h: Hwnd, attr: DWMWINDOWATTRIBUTE, v: &T) {
    // SAFETY: `v` points to a value of the size passed.
    unsafe {
        let _ = DwmSetWindowAttribute(h.raw(), attr, v as *const T as *const c_void, size_of::<T>() as u32);
    }
}

/// The Windows 11 frame: the whole window is DWM's frame (so the backdrop
/// shows where our visuals are transparent and DWM draws the caption
/// buttons), with Mica, or a solid caption color, and dark or light
/// caption buttons.
pub fn style_frame(h: Hwnd, dark: bool, caption_color: Option<u32>) {
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: `margins` is a valid pointer for the call.
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(h.raw(), &margins);
    }
    set_attr(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(dark));
    let backdrop = if caption_color.is_some() {
        DWMSBT_NONE
    } else {
        DWMSBT_MAINWINDOW
    };
    set_attr(h, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop);
    if let Some(rgb) = caption_color {
        // COLORREF is 0x00BBGGRR.
        let c = COLORREF(((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF));
        set_attr(h, DWMWA_CAPTION_COLOR, &c);
    }
}

/// The caption buttons' rectangle in client coordinates.
pub fn caption_button_bounds(h: Hwnd) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: `r` is a valid out pointer of the size passed.
    unsafe {
        DwmGetWindowAttribute(
            h.raw(),
            DWMWA_CAPTION_BUTTON_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(Rect::new(r.left, r.top, r.right, r.bottom))
}

/// DWM's handling of caption button messages (hover, press, snap layouts).
pub fn dwm_def_window_proc(m: Msg) -> Option<isize> {
    let mut r = LRESULT(0);
    // SAFETY: forwarding a message as received; `r` is a valid out pointer.
    let handled = unsafe {
        DwmDefWindowProc(m.hwnd.raw(), m.msg, WPARAM(m.wparam), LPARAM(m.lparam), &mut r)
    };
    handled.as_bool().then_some(r.0)
}

pub fn def_window_proc(m: Msg) -> isize {
    // SAFETY: forwarding a message as received.
    unsafe { DefWindowProcW(m.hwnd.raw(), m.msg, WPARAM(m.wparam), LPARAM(m.lparam)).0 }
}

/// `WM_NCCALCSIZE` with `wParam` TRUE: the standard frame on the sides and
/// bottom, the caption in the client area. A maximized window keeps its
/// top below the screen edge.
pub fn nccalcsize(m: Msg) -> isize {
    // SAFETY: for WM_NCCALCSIZE with wParam TRUE, lParam points to a
    // NCCALCSIZE_PARAMS owned by the caller for the duration of the call.
    unsafe {
        let p = m.lparam as *mut NCCALCSIZE_PARAMS;
        let top = (*p).rgrc[0].top;
        DefWindowProcW(m.hwnd.raw(), m.msg, WPARAM(m.wparam), LPARAM(m.lparam));
        let maximized = IsZoomed(m.hwnd.raw()).as_bool();
        (*p).rgrc[0].top = top + if maximized { frame_y(dpi(m.hwnd)) } else { 0 };
    }
    0
}

/// Sets the minimum track size in `WM_GETMINMAXINFO` from a minimum client
/// size.
pub fn set_min_track_size(m: Msg, min_client: Size) {
    let d = dpi(m.hwnd);
    let w = min_client.width + 2 * frame_x(d);
    let h = min_client.height + frame_y(d);
    // SAFETY: for WM_GETMINMAXINFO, lParam points to a MINMAXINFO owned by
    // the caller for the duration of the call.
    unsafe {
        let p = m.lparam as *mut MINMAXINFO;
        (*p).ptMinTrackSize = POINT { x: w, y: h };
    }
}

/// The rectangle `WM_DPICHANGED` suggests.
pub fn dpi_changed_rect(m: Msg) -> Rect {
    // SAFETY: for WM_DPICHANGED, lParam points to a RECT owned by the
    // caller for the duration of the call.
    let r = unsafe { *(m.lparam as *const RECT) };
    Rect::new(r.left, r.top, r.right, r.bottom)
}

pub fn dpi(h: Hwnd) -> u32 {
    // SAFETY: no pointers; 0 for an invalid window.
    let d = unsafe { GetDpiForWindow(h.raw()) };
    if d == 0 { 96 } else { d }
}

pub fn is_maximized(h: Hwnd) -> bool {
    // SAFETY: no pointers.
    unsafe { IsZoomed(h.raw()).as_bool() }
}

/// The client size.
pub fn client_size(h: Hwnd) -> Size {
    let mut r = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetClientRect(h.raw(), &mut r);
    }
    Size::new(r.right - r.left, r.bottom - r.top)
}

/// The client's top left corner in screen coordinates.
pub fn client_origin(h: Hwnd) -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: valid in/out pointer.
    unsafe {
        let _ = ClientToScreen(h.raw(), &mut p);
    }
    (p.x, p.y)
}

pub fn screen_to_client(h: Hwnd, x: i32, y: i32) -> (i32, i32) {
    let mut p = POINT { x, y };
    // SAFETY: valid in/out pointer.
    unsafe {
        let _ = ScreenToClient(h.raw(), &mut p);
    }
    (p.x, p.y)
}

/// A monitor: its rectangle and work area in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub handle: isize,
    pub rect: Rect,
    pub work: Rect,
}

pub fn monitor_of(h: Hwnd) -> Monitor {
    // SAFETY: valid out pointer with cbSize set.
    unsafe {
        let m: HMONITOR = MonitorFromWindow(h.raw(), MONITOR_DEFAULTTONEAREST);
        let mut i = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(m, &mut i);
        let r = |r: RECT| Rect::new(r.left, r.top, r.right, r.bottom);
        Monitor {
            handle: m.0 as isize,
            rect: r(i.rcMonitor),
            work: r(i.rcWork),
        }
    }
}

/// Moves and sizes the window (outer rectangle in screen coordinates).
pub fn set_window_rect(h: Hwnd, r: Rect) {
    // SAFETY: no pointers.
    unsafe {
        let _ = SetWindowPos(
            h.raw(),
            None,
            r.left,
            r.top,
            r.width(),
            r.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Resizes the window so its client area has `client` size.
pub fn set_client_size(h: Hwnd, client: Size) {
    let d = dpi(h);
    let w = client.width + 2 * frame_x(d);
    let ht = client.height + frame_y(d);
    // SAFETY: no pointers.
    unsafe {
        let _ = SetWindowPos(
            h.raw(),
            None,
            0,
            0,
            w,
            ht,
            SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE,
        );
    }
}

/// Centers the window on its monitor's work area.
pub fn center_on_monitor(h: Hwnd) {
    let mut r = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetWindowRect(h.raw(), &mut r);
    }
    let size = Size::new(r.right - r.left, r.bottom - r.top);
    let c = monitor_of(h).work.center(size);
    set_window_rect(h, c);
}

pub fn show(h: Hwnd, activate: bool) {
    // SAFETY: no pointers.
    unsafe {
        let _ = ShowWindow(h.raw(), if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
        if activate {
            let _ = SetForegroundWindow(h.raw());
        }
    }
}

pub fn set_title(h: Hwnd, title: &str) {
    let t = wide(title);
    // SAFETY: the string lives across the call.
    unsafe {
        let _ = SetWindowTextW(h.raw(), PCWSTR(t.as_ptr()));
    }
}

pub fn destroy(h: Hwnd) {
    // SAFETY: no pointers; must run on the window's thread (callers do).
    unsafe {
        let _ = DestroyWindow(h.raw());
    }
}

pub fn validate(h: Hwnd) {
    // SAFETY: no pointers.
    unsafe {
        let _ = ValidateRect(Some(h.raw()), None);
    }
}

pub fn post(h: Hwnd, msg: u32) {
    // SAFETY: no pointers.
    unsafe {
        let _ = PostMessageW(Some(h.raw()), msg, WPARAM(0), LPARAM(0));
    }
}

pub fn maximize(h: Hwnd) {
    // SAFETY: no pointers.
    unsafe {
        let _ = ShowWindow(h.raw(), SW_MAXIMIZE);
    }
}

pub fn restore(h: Hwnd) {
    // SAFETY: no pointers.
    unsafe {
        let _ = ShowWindow(h.raw(), SW_RESTORE);
    }
}

pub fn current_thread_id() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetCurrentThreadId() }
}

pub fn post_thread(thread: u32, msg: u32) -> bool {
    // SAFETY: no pointers.
    unsafe { PostThreadMessageW(thread, msg, WPARAM(0), LPARAM(0)).is_ok() }
}

/// Runs one message; `thread_msg` gets messages without a window. Returns
/// false on `WM_QUIT`.
pub fn pump_one(thread_msg: &mut dyn FnMut(u32)) -> bool {
    let mut m = MSG::default();
    // SAFETY: `m` is a valid out pointer.
    let r = unsafe { GetMessageW(&mut m, None, 0, 0) };
    if r.0 <= 0 {
        return false;
    }
    if m.hwnd.0.is_null() {
        thread_msg(m.message);
        return true;
    }
    // SAFETY: a message just retrieved.
    unsafe {
        let _ = TranslateMessage(&m);
        DispatchMessageW(&m);
    }
    true
}

/// Creates the thread's message queue, so other threads can post to it.
pub fn ensure_queue() {
    let mut m = MSG::default();
    // SAFETY: `m` is a valid out pointer; nothing is removed.
    unsafe {
        let _ = PeekMessageW(&mut m, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
}

/// True when the next queued keyboard message is right Alt with the given
/// message time: Windows sends a fake left Ctrl before AltGr.
pub fn next_is_altgr(time: u32) -> bool {
    let mut m = MSG::default();
    // SAFETY: `m` is a valid out pointer; nothing is removed.
    let found = unsafe { PeekMessageW(&mut m, None, WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE) };
    found.as_bool()
        && m.time == time
        && matches!(m.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
        && m.wParam.0 == 0x12 // VK_MENU
        && (m.lParam.0 >> 24) & 1 == 1
}

/// The window's outer rectangle in screen coordinates.
pub fn window_rect(h: Hwnd) -> Rect {
    let mut r = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetWindowRect(h.raw(), &mut r);
    }
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// Removes the resize border (non-resizable apps) or restores it.
pub fn set_resizable(h: Hwnd, resizable: bool) {
    // SAFETY: no pointers; the style change is applied with SWP_FRAMECHANGED.
    unsafe {
        let style = GetWindowLongPtrW(h.raw(), GWL_STYLE);
        let thick = WS_THICKFRAME.0 as isize;
        let new = if resizable { style | thick } else { style & !thick };
        SetWindowLongPtrW(h.raw(), GWL_STYLE, new);
        let _ = SetWindowPos(
            h.raw(),
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

pub fn is_resizable(h: Hwnd) -> bool {
    // SAFETY: no pointers.
    unsafe { GetWindowLongPtrW(h.raw(), GWL_STYLE) & WS_THICKFRAME.0 as isize != 0 }
}

pub fn set_timer(h: Hwnd, id: usize, ms: u32) {
    // SAFETY: no callback; WM_TIMER goes to the window.
    unsafe {
        SetTimer(Some(h.raw()), id, ms, None);
    }
}

pub fn kill_timer(h: Hwnd, id: usize) {
    // SAFETY: no pointers.
    unsafe {
        let _ = KillTimer(Some(h.raw()), id);
    }
}

pub fn quit() {
    // SAFETY: no pointers.
    unsafe { PostQuitMessage(0) };
}

/// Lock key states, from the thread's keyboard state.
pub fn locks() -> crate::input::keys::Locks {
    // SAFETY: no pointers.
    let on = |vk: u16| unsafe { GetKeyState(vk as i32) } & 1 != 0;
    crate::input::keys::Locks {
        caps: on(VK_CAPITAL.0),
        num: on(VK_NUMLOCK.0),
        scroll: on(VK_SCROLL.0),
    }
}

/// The time of the message being processed (tick count, ms).
pub fn message_time() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetMessageTime() as u32 }
}

/// Windows apps use the dark theme (Settings > Personalization > Colors).
pub fn apps_use_dark_theme() -> bool {
    let mut v = 1u32;
    let mut n = 4u32;
    // SAFETY: `v`/`n` are valid out pointers of the sizes given.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut n),
        )
    };
    r == ERROR_SUCCESS && v == 0
}

/// The PC has a touch screen.
pub fn has_touch() -> bool {
    // SAFETY: no pointers.
    unsafe { GetSystemMetrics(SM_MAXIMUMTOUCHES) > 0 }
}

/// Whether `lparam` of a `WM_SETTINGCHANGE` names the theme setting.
pub fn is_theme_change(lparam: isize) -> bool {
    if lparam == 0 {
        return false;
    }
    // SAFETY: for WM_SETTINGCHANGE, a non-null lParam is a NUL-terminated
    // string owned by the sender for the duration of the call.
    let s = unsafe { PCWSTR(lparam as *const u16).to_string() }.unwrap_or_default();
    s == "ImmersiveColorSet"
}

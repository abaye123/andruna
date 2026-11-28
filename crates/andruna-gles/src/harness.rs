//! A standalone window for tests and demos: a [`WindowTarget`] with its
//! own Windows window and DirectComposition tree, standing in for
//! andruna-compositor. A headless variant has a composition surface handle
//! and no window (presents go nowhere), for automated runs.
//!
//! Guest programs reach it through `libandruna_window.so`:
//!
//! ```c
//! ANativeWindow *andruna_window_create(int width, int height, const char *title);
//!     // title NULL (or ANDRUNA_GLES_HEADLESS=1): headless
//! void andruna_window_destroy(ANativeWindow *w);
//! int andruna_window_is_open(ANativeWindow *w);     // 0 once closed by the user
//! int andruna_window_resize(ANativeWindow *w, int width, int height);
//! int andruna_window_frame_stats(ANativeWindow *w, struct andruna_frame_stats *out);
//! ```
//!
//! In an app process a compositor provides the windows instead
//! ([`set_window_provider`]): `andruna_window_create` then returns the
//! next surface the compositor hands to the process, at the size the
//! compositor chose (`width` and `height` are ignored, as the system sizes
//! a `NativeActivity`'s window), and `andruna_window_resize` returns -1
//! (the user resizes the window).

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, DCompositionCreateSurfaceHandle, IDCompositionDevice,
    IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GetMessageW, IDC_ARROW, LoadCursorW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, TranslateMessage,
    WM_CLOSE, WM_DESTROY, WM_SIZE, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::native_window;
use crate::window::{WindowTarget, flip_transform, summary};

/// `COMPOSITIONOBJECT_ALL_ACCESS` (dcomptypes.h: read | write).
const COMPOSITIONOBJECT_ALL_ACCESS: u32 = 0x0003;

struct Composition {
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    height: u32,
}

/// A harness window (or a headless target).
pub struct HarnessWindow {
    hwnd: AtomicUsize,
    size: Mutex<(u32, u32)>,
    open: AtomicBool,
    handle: HANDLE,
    comp: Mutex<Option<Composition>>,
}

// SAFETY: the handle is a kernel handle; the DirectComposition objects are
// only used under `comp`'s mutex, and DirectComposition devices may be
// used from any thread.
unsafe impl Send for HarnessWindow {}
// SAFETY: as above.
unsafe impl Sync for HarnessWindow {}

impl WindowTarget for HarnessWindow {
    fn size(&self) -> (u32, u32) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn surface_handle(&self) -> HANDLE {
        self.handle
    }

    fn presented(&self, _width: u32, height: u32) {
        let mut g = self.comp.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.as_mut()
            && c.height != height
        {
            // SAFETY: COM calls on live DirectComposition objects.
            unsafe {
                let _ = c.visual.SetTransform2(&flip_transform(height));
                let _ = c.device.Commit();
            }
            c.height = height;
        }
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
}

impl Drop for HarnessWindow {
    fn drop(&mut self) {
        self.close();
        // SAFETY: the surface handle this object created.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn surface_handle() -> windows::core::Result<HANDLE> {
    // SAFETY: creates a new composition surface handle.
    unsafe { DCompositionCreateSurfaceHandle(COMPOSITIONOBJECT_ALL_ACCESS, None) }
}

impl HarnessWindow {
    /// A target without a window.
    pub fn headless(width: u32, height: u32) -> windows::core::Result<Arc<HarnessWindow>> {
        Ok(Arc::new(HarnessWindow {
            hwnd: AtomicUsize::new(0),
            size: Mutex::new((width, height)),
            open: AtomicBool::new(true),
            handle: surface_handle()?,
            comp: Mutex::new(None),
        }))
    }

    /// A visible window whose client area shows the surface; it runs its
    /// message loop on its own thread.
    pub fn open(width: u32, height: u32, title: &str) -> Result<Arc<HarnessWindow>, String> {
        let w = Arc::new(HarnessWindow {
            hwnd: AtomicUsize::new(0),
            size: Mutex::new((width, height)),
            open: AtomicBool::new(true),
            handle: surface_handle().map_err(|e| e.to_string())?,
            comp: Mutex::new(None),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let w2 = w.clone();
        let title = title.to_string();
        std::thread::Builder::new()
            .name("andruna-gles-window".into())
            .spawn(move || ui_thread(w2, width, height, &title, tx))
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())??;
        Ok(w)
    }

    /// Closes the window (no effect when headless or already closed).
    pub fn close(&self) {
        self.open.store(false, Ordering::Release);
        let h = self.hwnd.swap(0, Ordering::AcqRel);
        if h != 0 {
            // SAFETY: posts to a window of this process.
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Resizes the client area (headless: the reported size).
    pub fn resize(&self, width: u32, height: u32) {
        let h = self.hwnd.load(Ordering::Acquire);
        if h == 0 {
            *self.size.lock().unwrap_or_else(|e| e.into_inner()) = (width, height);
            return;
        }
        let (ow, oh) = outer_size(width, height);
        // SAFETY: resizes a window of this process; WM_SIZE updates `size`.
        unsafe {
            let _ = SetWindowPos(
                HWND(h as *mut _),
                None,
                0,
                0,
                ow,
                oh,
                SWP_NOMOVE | SWP_NOZORDER,
            );
        }
    }
}

fn outer_size(width: u32, height: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    // SAFETY: adjusts a local rectangle.
    let _ = unsafe {
        AdjustWindowRectEx(
            &mut r,
            WS_OVERLAPPEDWINDOW,
            false,
            WS_EX_NOREDIRECTIONBITMAP,
        )
    };
    (r.right - r.left, r.bottom - r.top)
}

thread_local! {
    static THIS: std::cell::RefCell<Option<Arc<HarnessWindow>>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            let (w, h) = ((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32);
            if w > 0 && h > 0 {
                THIS.with(|t| {
                    if let Some(win) = t.borrow().as_ref() {
                        *win.size.lock().unwrap_or_else(|e| e.into_inner()) = (w, h);
                    }
                });
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            THIS.with(|t| {
                if let Some(win) = t.borrow().as_ref() {
                    win.open.store(false, Ordering::Release);
                    win.hwnd.store(0, Ordering::Release);
                }
            });
            // SAFETY: this thread's window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling of this thread's window.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn ui_thread(
    w: Arc<HarnessWindow>,
    width: u32,
    height: u32,
    title: &str,
    tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let setup = || -> windows::core::Result<HWND> {
        static CLASS: Once = Once::new();
        // SAFETY: module handle of this executable.
        let inst = unsafe { GetModuleHandleW(None) }?;
        CLASS.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: inst.into(),
                // SAFETY: a system cursor.
                hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
                lpszClassName: w!("AndrunaGlesWindow"),
                ..Default::default()
            };
            // SAFETY: registers a class with a static name and procedure.
            unsafe { RegisterClassExW(&wc) };
        });
        let (ow, oh) = outer_size(width, height);
        // SAFETY: creates a window of the registered class on this thread.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("AndrunaGlesWindow"),
                PCWSTR(HSTRING::from(title).as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                ow,
                oh,
                None,
                None,
                Some(inst.into()),
                None,
            )
        }?;
        // SAFETY: COM calls creating the composition tree for this window.
        let comp = unsafe {
            let device: IDCompositionDevice = DCompositionCreateDevice(None)?;
            let target = device.CreateTargetForHwnd(hwnd, true)?;
            let visual = device.CreateVisual()?;
            let surface = device.CreateSurfaceFromHandle(w.handle)?;
            visual.SetContent(&surface)?;
            visual.SetTransform2(&flip_transform(height))?;
            target.SetRoot(&visual)?;
            device.Commit()?;
            Composition {
                device,
                _target: target,
                visual,
                height,
            }
        };
        *w.comp.lock().unwrap_or_else(|e| e.into_inner()) = Some(comp);
        Ok(hwnd)
    };
    THIS.with(|t| *t.borrow_mut() = Some(w.clone()));
    match setup() {
        Ok(hwnd) => {
            w.hwnd.store(hwnd.0 as usize, Ordering::Release);
            if crate::tracing() {
                crate::log(format_args!("harness window {:#x}", hwnd.0 as usize));
            }
            let _ = tx.send(Ok(()));
        }
        Err(e) => {
            w.open.store(false, Ordering::Release);
            let _ = tx.send(Err(format!("harness window: {e}")));
            return;
        }
    }
    let mut msg = MSG::default();
    // SAFETY: this thread's message loop.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    THIS.with(|t| *t.borrow_mut() = None);
}

// ---------------------------------------------------------------------------
// The guest API.

/// Where guest windows come from in a process with a compositor: called
/// with the guest's requested size and title, returns the target.
pub type WindowProvider =
    dyn Fn(u32, u32, Option<&str>) -> Result<Arc<dyn WindowTarget>, String> + Send + Sync;

static PROVIDER: Mutex<Option<Arc<WindowProvider>>> = Mutex::new(None);

/// Makes `andruna_window_create` return windows from `p` (a compositor's
/// surfaces) instead of harness windows; `None` restores the harness.
pub fn set_window_provider(p: Option<Box<WindowProvider>>) {
    *PROVIDER.lock().unwrap_or_else(|e| e.into_inner()) = p.map(Arc::from);
}

/// A guest window: a harness window, or a target from the provider.
#[derive(Clone)]
enum GuestWindow {
    Harness(Arc<HarnessWindow>),
    Provided(Arc<dyn WindowTarget>),
}

static WINDOWS: Mutex<Option<HashMap<usize, GuestWindow>>> = Mutex::new(None);

fn windows_map() -> std::sync::MutexGuard<'static, Option<HashMap<usize, GuestWindow>>> {
    WINDOWS.lock().unwrap_or_else(|e| e.into_inner())
}

fn window_of(p: usize) -> Option<GuestWindow> {
    windows_map().as_ref()?.get(&p).cloned()
}

/// `struct andruna_frame_stats` of the guest API.
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct GuestFrameStats {
    pub frames: u64,
    pub mean_interval_ms: f64,
    pub p99_interval_ms: f64,
    pub mean_swap_ms: f64,
    pub mean_wait_ms: f64,
    pub p99_wait_ms: f64,
    /// Frames in flight before `eglSwapBuffers` waits.
    pub max_frame_latency: u64,
}

const _: () = assert!(size_of::<GuestFrameStats>() == 56);

/// Creates a harness window and its `ANativeWindow` (one reference, owned
/// by the caller). `title == None` makes a headless target. With a window
/// provider, the window comes from it.
pub fn create_window(width: u32, height: u32, title: Option<&str>) -> Result<usize, String> {
    let provider = PROVIDER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(prov) = provider {
        let t = prov(width, height, title)?;
        let p = native_window::create(t.clone()) as usize;
        windows_map()
            .get_or_insert_with(HashMap::new)
            .insert(p, GuestWindow::Provided(t));
        return Ok(p);
    }
    let headless = title.is_none() || std::env::var_os("ANDRUNA_GLES_HEADLESS").is_some();
    let h = if headless {
        HarnessWindow::headless(width, height).map_err(|e| e.to_string())?
    } else {
        HarnessWindow::open(width, height, title.unwrap_or("Andruna"))?
    };
    let p = native_window::create(h.clone()) as usize;
    windows_map()
        .get_or_insert_with(HashMap::new)
        .insert(p, GuestWindow::Harness(h));
    Ok(p)
}

gfn! {
    unsafe fn window_create(width: i32, height: i32, title: *const c_char) -> usize {
        crate::guard(0, || {
            if width <= 0 || height <= 0 {
                return 0;
            }
            let t = (!title.is_null())
                // SAFETY: a NUL-terminated title (API contract).
                .then(|| unsafe { CStr::from_ptr(title) }.to_string_lossy().into_owned());
            match create_window(width as u32, height as u32, t.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    crate::log(format_args!("andruna_window_create: {e}"));
                    0
                }
            }
        })
    }
}

gfn! {
    unsafe fn window_destroy(win: usize) -> () {
        crate::guard((), || {
            let h = windows_map().as_mut().and_then(|m| m.remove(&win));
            if let Some(h) = h {
                if std::env::var_os("ANDRUNA_GLES_STATS").is_some()
                    && let Some(st) = frame_stats(win)
                {
                    crate::log(format_args!(
                        "window {win:#x}: {} frames, interval mean {:.2} ms p99 {:.2} ms, eglSwapBuffers mean {:.2} ms, wait mean {:.2} ms p99 {:.2} ms, {} frames in flight",
                        st.frames,
                        st.mean_interval_ms,
                        st.p99_interval_ms,
                        st.mean_swap_ms,
                        st.mean_wait_ms,
                        st.p99_wait_ms,
                        st.max_frame_latency
                    ));
                }
                if let GuestWindow::Harness(h) = h {
                    h.close();
                }
                native_window::release(win);
            }
        })
    }
}

gfn! {
    unsafe fn window_is_open(win: usize) -> i32 {
        crate::guard(0, || {
            window_of(win).map_or(0, |w| {
                i32::from(match w {
                    GuestWindow::Harness(h) => h.is_open(),
                    GuestWindow::Provided(t) => t.is_open(),
                })
            })
        })
    }
}

gfn! {
    unsafe fn window_resize(win: usize, width: i32, height: i32) -> i32 {
        crate::guard(-1, || match window_of(win) {
            Some(GuestWindow::Harness(h)) if width > 0 && height > 0 => {
                h.resize(width as u32, height as u32);
                0
            }
            _ => -1,
        })
    }
}

/// The frame statistics of a window.
pub fn frame_stats(win: usize) -> Option<GuestFrameStats> {
    let w = native_window::lookup(win)?;
    let s = w.stats.lock().unwrap_or_else(|e| e.into_inner());
    let (mi, pi) = summary(&s.intervals);
    let (ms, _) = summary(&s.swap_times);
    let (mw, pw) = summary(&s.waits);
    Some(GuestFrameStats {
        frames: s.frames,
        mean_interval_ms: mi,
        p99_interval_ms: pi,
        mean_swap_ms: ms,
        mean_wait_ms: mw,
        p99_wait_ms: pw,
        max_frame_latency: u64::from(crate::window::max_latency()),
    })
}

gfn! {
    unsafe fn window_frame_stats(win: usize, out: usize) -> i32 {
        crate::guard(-1, || match frame_stats(win) {
            Some(s) if out != 0 => {
                // SAFETY: the guest's struct (API contract).
                unsafe { (out as *mut GuestFrameStats).write_unaligned(s) };
                0
            }
            _ => -1,
        })
    }
}

/// The harness functions, by name.
pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("andruna_window_create", window_create as usize),
        ("andruna_window_destroy", window_destroy as usize),
        ("andruna_window_is_open", window_is_open as usize),
        ("andruna_window_resize", window_resize as usize),
        ("andruna_window_frame_stats", window_frame_stats as usize),
    ]
}

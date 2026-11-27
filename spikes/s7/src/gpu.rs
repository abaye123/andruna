//! D3D11 + DirectComposition (and ANGLE, when a build is given) inside an
//! AppContainer. The child draws one known color into a composition swap
//! chain on its window; the parent, outside the container, reads the pixel
//! from the screen.

use std::ffi::c_void;
use std::io;
use std::time::{Duration, Instant};

use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetDC, GetPixel, ReleaseDC};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect,
    PeekMessageW, RegisterClassW, SetForegroundWindow, SetWindowPos, TranslateMessage,
    HWND_TOPMOST, MSG, PM_REMOVE, SWP_SHOWWINDOW, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::args::Args;
use crate::env::{self, Env, Grant};
use crate::proc::{quote, Proc};
use crate::win::{self, Container};

/// The color the child clears to: (r, g, b) in 0..=255.
const COLOR: (u8, u8, u8) = (32, 160, 96);

unsafe extern "system" fn wnd_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: default handling with the arguments Windows passed.
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn make_window(title: &str) -> io::Result<HWND> {
    let class: Vec<u16> = win::wide("Andruna.S7");
    let t = win::wide(title);
    // SAFETY: registered class name and title live through the calls.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst = GetModuleHandleW(PCWSTR::null()).map_err(win::werr)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            PCWSTR(t.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            480,
            320,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(win::werr)?;
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 200, 200, 480, 320, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut msg = MSG::default();
    while Instant::now() < end {
        // SAFETY: valid message struct.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Screen coordinates of the window's client center.
fn center(hwnd: HWND) -> (i32, i32, u32, u32) {
    let mut rc = RECT::default();
    // SAFETY: valid window and out structs.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
        let mut p = POINT {
            x: rc.right / 2,
            y: rc.bottom / 2,
        };
        let _ = ClientToScreen(hwnd, &mut p);
        (p.x, p.y, rc.right as u32, rc.bottom as u32)
    }
}

fn wait_quit() {
    // The parent writes a line when it has read the pixel.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = io::stdin().read_line(&mut s);
        let _ = tx.send(());
    });
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && rx.try_recv().is_err() {
        pump(20);
    }
}

pub fn child(_a: &Args) -> io::Result<()> {
    let (ac, il) = win::own_token();
    println!("token: appcontainer={ac} integrity=0x{il:x}");
    let t0 = Instant::now();
    let hwnd = make_window("Andruna S7 D3D11 + DComp")?;
    let (w, h) = {
        let c = center(hwnd);
        (c.2.max(1), c.3.max(1))
    };
    // SAFETY: COM calls on objects created here, on this thread.
    unsafe {
        let mut dev: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        let mut fl = D3D_FEATURE_LEVEL::default();
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            Some(&mut fl),
            Some(&mut ctx),
        )
        .map_err(win::werr)?;
        let dev = dev.unwrap();
        let ctx = ctx.unwrap();
        let dxgi: IDXGIDevice = dev.cast().map_err(win::werr)?;
        let adapter = dxgi.GetAdapter().map_err(win::werr)?;
        let desc = adapter.GetDesc().map_err(win::werr)?;
        let name = String::from_utf16_lossy(&desc.Description);
        println!(
            "d3d11 device: feature level {:#x}, adapter {}",
            fl.0,
            name.trim_end_matches('\0')
        );
        let factory: IDXGIFactory2 = adapter.GetParent().map_err(win::werr)?;
        let scd = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let sc: IDXGISwapChain1 = factory
            .CreateSwapChainForComposition(&dev, &scd, None)
            .map_err(win::werr)?;
        let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi).map_err(win::werr)?;
        let target: IDCompositionTarget =
            dcomp.CreateTargetForHwnd(hwnd, true).map_err(win::werr)?;
        let visual: IDCompositionVisual = dcomp.CreateVisual().map_err(win::werr)?;
        visual.SetContent(&sc).map_err(win::werr)?;
        target.SetRoot(&visual).map_err(win::werr)?;
        dcomp.Commit().map_err(win::werr)?;
        let back: ID3D11Texture2D = sc.GetBuffer(0).map_err(win::werr)?;
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        dev.CreateRenderTargetView(&back, None, Some(&mut rtv))
            .map_err(win::werr)?;
        let rtv = rtv.unwrap();
        let c = [
            COLOR.0 as f32 / 255.0,
            COLOR.1 as f32 / 255.0,
            COLOR.2 as f32 / 255.0,
            1.0,
        ];
        ctx.ClearRenderTargetView(&rtv, &c);
        // Read back one pixel before presenting.
        let mut td = D3D11_TEXTURE2D_DESC::default();
        back.GetDesc(&mut td);
        td.Usage = D3D11_USAGE_STAGING;
        td.BindFlags = 0;
        td.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        td.MiscFlags = 0;
        let mut staging: Option<ID3D11Texture2D> = None;
        dev.CreateTexture2D(&td, None, Some(&mut staging))
            .map_err(win::werr)?;
        let staging = staging.unwrap();
        ctx.CopyResource(&staging, &back);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
            .map_err(win::werr)?;
        let px = std::slice::from_raw_parts(m.pData as *const u8, 4);
        println!("readback bgra {:?}", px);
        ctx.Unmap(&staging, 0);
        sc.Present(1, DXGI_PRESENT(0)).ok().map_err(win::werr)?;
        dcomp.Commit().map_err(win::werr)?;
        dcomp.WaitForCommitCompletion().map_err(win::werr)?;
        let (x, y, _, _) = center(hwnd);
        println!(
            "presented after {:.1} ms; center {x} {y} hwnd {}",
            t0.elapsed().as_secs_f64() * 1e3,
            hwnd.0 as usize
        );
        wait_quit();
    }
    Ok(())
}

fn screen_pixel(x: i32, y: i32) -> (u8, u8, u8) {
    // SAFETY: the screen DC is released after use.
    unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, x, y).0;
        ReleaseDC(None, dc);
        (
            (c & 0xFF) as u8,
            ((c >> 8) & 0xFF) as u8,
            ((c >> 16) & 0xFF) as u8,
        )
    }
}

/// The client-center pixel of window `hwnd` as DWM composes it
/// (PrintWindow with PW_CLIENTONLY | PW_RENDERFULLCONTENT): works while the
/// window is covered or the session is locked.
fn window_pixel(hwnd: usize) -> Option<(u8, u8, u8)> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, SelectObject,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: GDI objects created here are released here; a stale window
    // handle makes the calls fail.
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(hwnd, &mut rc).ok()?;
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, rc.right, rc.bottom);
        let old = SelectObject(mem, bmp.into());
        let ok = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(1 | 2)).as_bool();
        let c = GetPixel(mem, rc.right / 2, rc.bottom / 2).0;
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        ok.then_some((
            (c & 0xFF) as u8,
            ((c >> 8) & 0xFF) as u8,
            ((c >> 16) & 0xFF) as u8,
        ))
    }
}

fn run_child(a: &Args, sub: &str, extra: &str) -> io::Result<()> {
    let env = Env::new(a);
    let grant = Grant::parse(&a.str("--grant", "cap"))?;
    let lpac = a.flag("--lpac");
    let p = env::prepare(&env, grant, lpac, "gpu")?;
    if let Some(d) = a.opt("--angle-dir") {
        let who = if grant == Grant::Pkg {
            p.sid.clone()
        } else {
            p.runtime_cap.clone()
        };
        win::set_access(
            std::path::Path::new(&d),
            &who,
            win::FILE_GENERIC_READ_EXECUTE,
            false,
        )?;
    }
    // SAFETY: no preconditions.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut caps = vec![p.runtime_cap.clone()];
    for name in a.str("--caps", "").split(',').filter(|s| !s.is_empty()) {
        caps.push(win::capability_sid(name)?);
    }
    let c = Container {
        sid: &p.sid,
        capabilities: &caps,
        lpac,
    };
    let container = if a.flag("--no-container") {
        None
    } else {
        Some(&c)
    };
    let cmd = format!("{} {sub} {extra}", quote(&env.s7().display().to_string()));
    let mut ch = Proc::spawn(&cmd, container, false, &[])?;
    // SAFETY: no memory preconditions.
    unsafe {
        let _ = AllowSetForegroundWindow(ch.child.pid);
    }
    let line = ch.wait_line("presented", Duration::from_secs(30));
    let mut verdict = "no frame".to_string();
    if let Some((t, l)) = &line {
        let v: Vec<i64> = l
            .split("center ")
            .nth(1)
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        std::thread::sleep(Duration::from_millis(500));
        let near = |px: (u8, u8, u8)| {
            px.0.abs_diff(COLOR.0) <= 2
                && px.1.abs_diff(COLOR.1) <= 2
                && px.2.abs_diff(COLOR.2) <= 2
        };
        let px = screen_pixel(v[0] as i32, v[1] as i32);
        let wp = window_pixel(v[2] as usize);
        verdict = format!(
            "expected {:?}; screen pixel at ({}, {}) = {:?} ({}); window content (PrintWindow) = {:?} ({}); spawn to present {:.1} ms",
            COLOR,
            v[0],
            v[1],
            px,
            if near(px) { "MATCH" } else { "mismatch" },
            wp,
            if wp.is_some_and(near) { "MATCH" } else { "mismatch" },
            crate::proc::ms(ch.started, *t)
        );
    }
    let _ = ch.write(b"q\n");
    let code = ch.wait(Duration::from_secs(10));
    println!("{verdict}");
    println!(
        "child exit {code:?}\n--- child stdout\n{}--- child stderr\n{}",
        ch.out(),
        ch.err()
    );
    Ok(())
}

pub fn parent(a: &Args) -> io::Result<()> {
    run_child(a, "gpu-child", "")
}

pub fn angle(a: &Args) -> io::Result<()> {
    let d = a
        .opt("--angle-dir")
        .ok_or_else(|| io::Error::other("--angle-dir <dir with libEGL.dll and libGLESv2.dll>"))?;
    run_child(a, "angle-child", &format!("--angle-dir {}", quote(&d)))
}

type EglGetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void;
type EglInitialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32;
type EglChooseConfig =
    unsafe extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32;
type EglCreateWindowSurface =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
type EglCreateContext =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
type EglMakeCurrent =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;
type EglSwapBuffers = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type EglGetError = unsafe extern "C" fn() -> i32;
type GlClearColor = unsafe extern "C" fn(f32, f32, f32, f32);
type GlClear = unsafe extern "C" fn(u32);
type GlGetString = unsafe extern "C" fn(u32) -> *const u8;

/// ANGLE on D3D11 with a DirectComposition window surface.
pub fn angle_child(a: &Args) -> io::Result<()> {
    let (ac, il) = win::own_token();
    println!("token: appcontainer={ac} integrity=0x{il:x}");
    let dir = std::path::PathBuf::from(a.str("--angle-dir", ""));
    let t0 = Instant::now();
    let hwnd = make_window("Andruna S7 ANGLE")?;
    // SAFETY: the libraries are ANGLE's; each symbol is cast to its EGL/GLES
    // signature; attribute lists are EGL_NONE-terminated.
    unsafe {
        let gles = LoadLibraryW(PCWSTR(win::wpath(&dir.join("libGLESv2.dll")).as_ptr()))
            .map_err(win::werr)?;
        let egl = LoadLibraryW(PCWSTR(win::wpath(&dir.join("libEGL.dll")).as_ptr()))
            .map_err(win::werr)?;
        macro_rules! sym {
            ($lib:expr, $name:literal, $t:ty) => {
                std::mem::transmute::<_, $t>(
                    GetProcAddress($lib, PCSTR(concat!($name, "\0").as_ptr()))
                        .ok_or_else(|| io::Error::other(concat!("missing ", $name)))?,
                )
            };
        }
        let get_display = sym!(egl, "eglGetPlatformDisplayEXT", EglGetPlatformDisplay);
        let initialize = sym!(egl, "eglInitialize", EglInitialize);
        let choose = sym!(egl, "eglChooseConfig", EglChooseConfig);
        let create_surface = sym!(egl, "eglCreateWindowSurface", EglCreateWindowSurface);
        let create_context = sym!(egl, "eglCreateContext", EglCreateContext);
        let make_current = sym!(egl, "eglMakeCurrent", EglMakeCurrent);
        let swap = sym!(egl, "eglSwapBuffers", EglSwapBuffers);
        let get_error = sym!(egl, "eglGetError", EglGetError);
        let clear_color = sym!(gles, "glClearColor", GlClearColor);
        let clear = sym!(gles, "glClear", GlClear);
        let get_string = sym!(gles, "glGetString", GlGetString);
        const EGL_PLATFORM_ANGLE_ANGLE: u32 = 0x3202;
        const EGL_PLATFORM_ANGLE_TYPE_ANGLE: isize = 0x3203;
        const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: isize = 0x3208;
        const EGL_NONE: i32 = 0x3038;
        let dattrs = [
            EGL_PLATFORM_ANGLE_TYPE_ANGLE,
            EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE,
            EGL_NONE as isize,
        ];
        let d = get_display(
            EGL_PLATFORM_ANGLE_ANGLE,
            std::ptr::null_mut(),
            dattrs.as_ptr(),
        );
        let (mut maj, mut min) = (0, 0);
        if d.is_null() || initialize(d, &mut maj, &mut min) == 0 {
            return Err(io::Error::other(format!(
                "eglInitialize: {:#x}",
                get_error()
            )));
        }
        println!("egl {maj}.{min}");
        let cattrs = [
            0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, // RGBA sizes
            0x3040, 0x0040, // EGL_RENDERABLE_TYPE = EGL_OPENGL_ES3_BIT
            EGL_NONE,
        ];
        let mut cfg = std::ptr::null_mut();
        let mut n = 0;
        if choose(d, cattrs.as_ptr(), &mut cfg, 1, &mut n) == 0 || n == 0 {
            return Err(io::Error::other(format!(
                "eglChooseConfig: {:#x}",
                get_error()
            )));
        }
        // EGL_DIRECT_COMPOSITION_ANGLE = EGL_TRUE
        let sattrs = [0x33A5, 1, EGL_NONE];
        let s = create_surface(d, cfg, hwnd.0, sattrs.as_ptr());
        if s.is_null() {
            return Err(io::Error::other(format!(
                "eglCreateWindowSurface: {:#x}",
                get_error()
            )));
        }
        let xattrs = [0x3098, 3, EGL_NONE]; // EGL_CONTEXT_CLIENT_VERSION 3
        let ctx = create_context(d, cfg, std::ptr::null_mut(), xattrs.as_ptr());
        if ctx.is_null() || make_current(d, s, s, ctx) == 0 {
            return Err(io::Error::other(format!(
                "eglMakeCurrent: {:#x}",
                get_error()
            )));
        }
        let r = get_string(0x1F01); // GL_RENDERER
        if !r.is_null() {
            println!(
                "GL_RENDERER {}",
                std::ffi::CStr::from_ptr(r as *const i8).to_string_lossy()
            );
        }
        clear_color(
            COLOR.0 as f32 / 255.0,
            COLOR.1 as f32 / 255.0,
            COLOR.2 as f32 / 255.0,
            1.0,
        );
        clear(0x4000); // GL_COLOR_BUFFER_BIT
        if swap(d, s) == 0 {
            return Err(io::Error::other(format!(
                "eglSwapBuffers: {:#x}",
                get_error()
            )));
        }
        pump(50);
        let (x, y, _, _) = center(hwnd);
        println!(
            "presented after {:.1} ms; center {x} {y} hwnd {}",
            t0.elapsed().as_secs_f64() * 1e3,
            hwnd.0 as usize
        );
        wait_quit();
    }
    Ok(())
}

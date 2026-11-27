//! The EGL functions andruna-gles implements on top of ANGLE.
//!
//! Everything else goes straight to ANGLE (through [`crate::thunk`]). The
//! host versions here cover what differs between Android and ANGLE on
//! Windows:
//!
//! - the default display is ANGLE's Direct3D 11 display
//!   (`ANDRUNA_GLES_DEVICE=warp` selects WARP, the software rasterizer);
//!   `EGL_PLATFORM_ANDROID_KHR` maps to it;
//! - window surfaces are made for Andruna's `ANativeWindow`s
//!   ([`crate::window::WindowSurface`]); their `EGLSurface` handles are
//!   this module's, translated to the ANGLE pbuffer in every function that
//!   takes or returns a surface;
//! - `EGL_NATIVE_VISUAL_ID` is the Android pixel format of the config, and
//!   `EGL_RECORDABLE_ANDROID` / `EGL_FRAMEBUFFER_TARGET_ANDROID` are
//!   answered (no config is recordable or a framebuffer target yet);
//! - `eglGetProcAddress` returns guest-callable addresses;
//! - `eglGetError` also reports errors raised here.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::core::Interface;

use crate::angle::angle_fn;
use crate::window::WindowSurface;

pub const EGL_FALSE: u32 = 0;
pub const EGL_TRUE: u32 = 1;
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_SUCCESS: i32 = 0x3000;
pub const EGL_ALPHA_SIZE: i32 = 0x3021;
pub const EGL_BLUE_SIZE: i32 = 0x3022;
pub const EGL_GREEN_SIZE: i32 = 0x3023;
pub const EGL_RED_SIZE: i32 = 0x3024;
pub const EGL_NATIVE_VISUAL_ID: i32 = 0x302E;
pub const EGL_DRAW: i32 = 0x3059;
pub const EGL_READ: i32 = 0x305A;
pub const EGL_RENDER_BUFFER: i32 = 0x3086;
pub const EGL_BACK_BUFFER: i32 = 0x3084;
pub const EGL_GL_COLORSPACE_KHR: i32 = 0x309D;
pub const EGL_GL_COLORSPACE_LINEAR_KHR: i32 = 0x308A;
pub const EGL_PLATFORM_ANDROID_KHR: u32 = 0x3141;
pub const EGL_RECORDABLE_ANDROID: i32 = 0x3142;
pub const EGL_FRAMEBUFFER_TARGET_ANDROID: i32 = 0x3147;
pub const EGL_PLATFORM_ANGLE_ANGLE: u32 = 0x3202;
pub const EGL_PLATFORM_ANGLE_TYPE_ANGLE: isize = 0x3203;
pub const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: isize = 0x3208;
pub const EGL_PLATFORM_ANGLE_DEVICE_TYPE_ANGLE: isize = 0x3209;
pub const EGL_PLATFORM_ANGLE_DEVICE_TYPE_HARDWARE_ANGLE: isize = 0x320A;
pub const EGL_PLATFORM_ANGLE_DEVICE_TYPE_D3D_WARP_ANGLE: isize = 0x320B;
pub const EGL_D3D_TEXTURE_ANGLE: u32 = 0x33A3;
pub const EGL_D3D11_DEVICE_ANGLE: i32 = 0x33A1;
pub const EGL_DEVICE_EXT: i32 = 0x322C;

/// HAL pixel formats for `EGL_NATIVE_VISUAL_ID`.
const HAL_RGBA_8888: i32 = 1;
const HAL_RGBX_8888: i32 = 2;
const HAL_RGB_565: i32 = 4;

/// An EGL error raised by andruna-gles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EglError {
    BadAlloc,
    BadAttribute,
    BadDisplay,
    BadMatch,
    BadNativeWindow,
    BadParameter,
    BadSurface,
    /// An error ANGLE reported (its code).
    Angle(i32),
}

impl EglError {
    pub fn code(self) -> i32 {
        match self {
            EglError::BadAlloc => 0x3003,
            EglError::BadAttribute => 0x3004,
            EglError::BadDisplay => 0x3008,
            EglError::BadMatch => 0x3009,
            EglError::BadNativeWindow => 0x300B,
            EglError::BadParameter => 0x300C,
            EglError::BadSurface => 0x300D,
            EglError::Angle(c) => c,
        }
    }
}

thread_local! {
    /// An error raised here, reported by the next `eglGetError`.
    static PENDING: Cell<i32> = const { Cell::new(0) };
}

fn set_error(e: EglError) {
    PENDING.with(|p| p.set(e.code()));
}

fn clear_error() {
    PENDING.with(|p| p.set(0));
}

/// ANGLE's current error, consumed (`eglGetError`).
fn angle_error() -> i32 {
    match angle_fn!("eglGetError", fn() -> i32) {
        // SAFETY: ANGLE's eglGetError.
        Some(f) => unsafe { f() },
        None => 0x3001,
    }
}

// ---------------------------------------------------------------------------
// Window surface handles.

type Shared = Arc<Mutex<WindowSurface>>;

static SURFACES: Mutex<Option<HashMap<usize, Shared>>> = Mutex::new(None);

fn surfaces() -> std::sync::MutexGuard<'static, Option<HashMap<usize, Shared>>> {
    SURFACES.lock().unwrap_or_else(|e| e.into_inner())
}

fn ours(s: usize) -> Option<Shared> {
    surfaces().as_ref()?.get(&s).cloned()
}

/// The ANGLE surface for a guest handle.
fn map(s: usize) -> usize {
    match ours(s) {
        Some(w) => w.lock().unwrap_or_else(|e| e.into_inner()).pbuffer,
        None => s,
    }
}

/// The guest handle for an ANGLE surface.
fn unmap(p: usize) -> usize {
    if p == 0 {
        return 0;
    }
    let g = surfaces();
    if let Some(m) = g.as_ref() {
        for (h, w) in m {
            if w.lock().unwrap_or_else(|e| e.into_inner()).pbuffer == p {
                return *h;
            }
        }
    }
    p
}

/// The frame statistics of a guest window surface handle.
pub fn surface_stats(s: usize) -> Option<Arc<Mutex<crate::window::FrameStats>>> {
    ours(s).map(|w| w.lock().unwrap_or_else(|e| e.into_inner()).stats())
}

// ---------------------------------------------------------------------------
// Helpers on ANGLE, used by the window surfaces.

/// ANGLE's Direct3D 11 device of a display.
pub fn d3d11_device(dpy: usize) -> Option<ID3D11Device> {
    let qd = angle_fn!("eglQueryDisplayAttribEXT", fn(usize, i32, usize) -> u32)?;
    let qv = angle_fn!("eglQueryDeviceAttribEXT", fn(usize, i32, usize) -> u32)?;
    let mut dev: isize = 0;
    let mut d3d: isize = 0;
    // SAFETY: ANGLE writes one EGLAttrib into each local.
    unsafe {
        if qd(dpy, EGL_DEVICE_EXT, &mut dev as *mut isize as usize) != EGL_TRUE
            || qv(
                dev as usize,
                EGL_D3D11_DEVICE_ANGLE,
                &mut d3d as *mut isize as usize,
            ) != EGL_TRUE
        {
            return None;
        }
    }
    let raw = d3d as *mut core::ffi::c_void;
    // SAFETY: ANGLE's live ID3D11Device; `cloned` adds a reference.
    unsafe { ID3D11Device::from_raw_borrowed(&raw) }.cloned()
}

/// A pbuffer on a Direct3D 11 texture (`EGL_ANGLE_d3d_texture_client_buffer`).
pub fn pbuffer_from_texture(
    dpy: usize,
    config: usize,
    texture: usize,
    extra: &[i32],
) -> Result<usize, EglError> {
    let f = angle_fn!(
        "eglCreatePbufferFromClientBuffer",
        fn(usize, u32, usize, usize, usize) -> usize
    )
    .ok_or(EglError::BadDisplay)?;
    let mut attrs: Vec<i32> = extra.to_vec();
    attrs.push(EGL_NONE);
    // SAFETY: ANGLE's function with a live texture and a terminated list.
    let s = unsafe {
        f(
            dpy,
            EGL_D3D_TEXTURE_ANGLE,
            texture,
            config,
            attrs.as_ptr() as usize,
        )
    };
    if s == 0 {
        let e = angle_error();
        if crate::tracing() {
            crate::log(format_args!("pbuffer on the back buffer: EGL error {e:#x}"));
        }
        return Err(EglError::Angle(e));
    }
    Ok(s)
}

/// `glFlush` on the current context.
pub fn flush() {
    if let Some(f) = angle_fn!("glFlush", fn()) {
        // SAFETY: ANGLE's glFlush (no effect without a current context).
        unsafe { f() }
    }
}

/// The calling thread's current EGL state, in ANGLE's handles.
pub struct Current {
    pub dpy: usize,
    pub draw: usize,
    pub read: usize,
    pub ctx: usize,
}

impl Current {
    pub fn get() -> Current {
        let d = angle_fn!("eglGetCurrentDisplay", fn() -> usize);
        let s = angle_fn!("eglGetCurrentSurface", fn(i32) -> usize);
        let c = angle_fn!("eglGetCurrentContext", fn() -> usize);
        // SAFETY: ANGLE's query functions.
        unsafe {
            Current {
                dpy: d.map_or(0, |f| f()),
                draw: s.map_or(0, |f| f(EGL_DRAW)),
                read: s.map_or(0, |f| f(EGL_READ)),
                ctx: c.map_or(0, |f| f()),
            }
        }
    }
}

pub fn make_current_raw(dpy: usize, draw: usize, read: usize, ctx: usize) -> bool {
    match angle_fn!("eglMakeCurrent", fn(usize, usize, usize, usize) -> u32) {
        // SAFETY: ANGLE's eglMakeCurrent with handles it gave out.
        Some(f) => unsafe { f(dpy, draw, read, ctx) == EGL_TRUE },
        None => false,
    }
}

pub fn destroy_surface_raw(dpy: usize, s: usize) {
    if let Some(f) = angle_fn!("eglDestroySurface", fn(usize, usize) -> u32) {
        // SAFETY: ANGLE's eglDestroySurface on its own handle.
        unsafe {
            f(dpy, s);
        }
    }
}

// ---------------------------------------------------------------------------
// Displays.

fn device_type() -> isize {
    match std::env::var("ANDRUNA_GLES_DEVICE").as_deref() {
        Ok("warp") => EGL_PLATFORM_ANGLE_DEVICE_TYPE_D3D_WARP_ANGLE,
        _ => EGL_PLATFORM_ANGLE_DEVICE_TYPE_HARDWARE_ANGLE,
    }
}

/// The default display: ANGLE on Direct3D 11.
pub fn default_display() -> usize {
    let Some(f) = angle_fn!("eglGetPlatformDisplay", fn(u32, usize, usize) -> usize) else {
        return 0;
    };
    let attrs: [isize; 5] = [
        EGL_PLATFORM_ANGLE_TYPE_ANGLE,
        EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE,
        EGL_PLATFORM_ANGLE_DEVICE_TYPE_ANGLE,
        device_type(),
        EGL_NONE as isize,
    ];
    // SAFETY: ANGLE's function with a terminated attribute list.
    unsafe { f(EGL_PLATFORM_ANGLE_ANGLE, 0, attrs.as_ptr() as usize) }
}

/// Marks the calling thread as a guest thread (callbacks into guest code
/// are only made on such threads).
fn entered() {
    crate::callbacks::mark_guest_thread();
    clear_error();
}

gfn! {
    unsafe fn get_display(native: usize) -> usize {
        crate::guard(0, || {
            entered();
            // Android accepts only EGL_DEFAULT_DISPLAY.
            if native != 0 {
                return 0;
            }
            default_display()
        })
    }
}

gfn! {
    unsafe fn get_platform_display(platform: u32, native: usize, attribs: usize) -> usize {
        crate::guard(0, || {
            entered();
            if platform == EGL_PLATFORM_ANDROID_KHR {
                return if native == 0 { default_display() } else { 0 };
            }
            match angle_fn!("eglGetPlatformDisplay", fn(u32, usize, usize) -> usize) {
                // SAFETY: forwarded; the list is the caller's.
                Some(f) => unsafe { f(platform, native, attribs) },
                None => 0,
            }
        })
    }
}

gfn! {
    unsafe fn get_platform_display_ext(platform: u32, native: usize, attribs: usize) -> usize {
        crate::guard(0, || {
            entered();
            if platform == EGL_PLATFORM_ANDROID_KHR {
                return if native == 0 { default_display() } else { 0 };
            }
            match angle_fn!("eglGetPlatformDisplayEXT", fn(u32, usize, usize) -> usize) {
                // SAFETY: forwarded; the list is the caller's.
                Some(f) => unsafe { f(platform, native, attribs) },
                None => 0,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Configs.

/// Reads an `EGL_NONE`-terminated list of `T` pairs.
///
/// # Safety
///
/// `p` is null or a terminated attribute list.
unsafe fn read_list<T: Copy + Into<i64>>(p: usize) -> Vec<(i64, i64)> {
    let mut v = Vec::new();
    if p == 0 {
        return v;
    }
    let p = p as *const T;
    let mut i = 0;
    loop {
        // SAFETY: inside the caller's terminated list (contract).
        let k: i64 = unsafe { p.add(i).read_unaligned() }.into();
        if k == i64::from(EGL_NONE) || i > 512 {
            break;
        }
        // SAFETY: as above.
        let val: i64 = unsafe { p.add(i + 1).read_unaligned() }.into();
        v.push((k, val));
        i += 2;
    }
    v
}

fn config_attrib(dpy: usize, config: usize, attr: i32) -> Option<i32> {
    let f = angle_fn!("eglGetConfigAttrib", fn(usize, usize, i32, usize) -> u32)?;
    let mut v = 0i32;
    // SAFETY: ANGLE writes one EGLint.
    (unsafe { f(dpy, config, attr, &mut v as *mut i32 as usize) } == EGL_TRUE).then_some(v)
}

gfn! {
    unsafe fn choose_config(dpy: usize, attribs: usize, configs: usize, size: i32, num: usize) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            // SAFETY: the guest's terminated list (EGL contract).
            let list = unsafe { read_list::<i32>(attribs) };
            let mut out: Vec<i32> = Vec::with_capacity(list.len() * 2 + 1);
            for (k, v) in list {
                let k = k as i32;
                if k == EGL_RECORDABLE_ANDROID || k == EGL_FRAMEBUFFER_TARGET_ANDROID {
                    if v == i64::from(EGL_TRUE) {
                        // No config can be recorded by a video encoder or
                        // be a compositor target yet.
                        if num != 0 {
                            // SAFETY: the guest's EGLint* (EGL contract).
                            unsafe { (num as *mut i32).write_unaligned(0) };
                        }
                        return EGL_TRUE;
                    }
                    continue;
                }
                out.extend([k, v as i32]);
            }
            out.push(EGL_NONE);
            match angle_fn!("eglChooseConfig", fn(usize, usize, usize, i32, usize) -> u32) {
                // SAFETY: forwarded with a terminated copy of the list.
                Some(f) => unsafe { f(dpy, out.as_ptr() as usize, configs, size, num) },
                None => EGL_FALSE,
            }
        })
    }
}

gfn! {
    unsafe fn get_config_attrib(dpy: usize, config: usize, attr: i32, value: usize) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            let v = match attr {
                EGL_NATIVE_VISUAL_ID => {
                    let sizes = [EGL_RED_SIZE, EGL_GREEN_SIZE, EGL_BLUE_SIZE, EGL_ALPHA_SIZE]
                        .map(|a| config_attrib(dpy, config, a));
                    match sizes {
                        [Some(8), Some(8), Some(8), Some(8)] => HAL_RGBA_8888,
                        [Some(8), Some(8), Some(8), Some(0)] => HAL_RGBX_8888,
                        [Some(5), Some(6), Some(5), Some(0)] => HAL_RGB_565,
                        [Some(_), ..] => 0,
                        _ => return EGL_FALSE,
                    }
                }
                EGL_RECORDABLE_ANDROID | EGL_FRAMEBUFFER_TARGET_ANDROID => {
                    if config_attrib(dpy, config, EGL_RED_SIZE).is_none() {
                        return EGL_FALSE;
                    }
                    EGL_FALSE as i32
                }
                _ => {
                    return match angle_fn!("eglGetConfigAttrib", fn(usize, usize, i32, usize) -> u32) {
                        // SAFETY: forwarded.
                        Some(f) => unsafe { f(dpy, config, attr, value) },
                        None => EGL_FALSE,
                    };
                }
            };
            if value == 0 {
                set_error(EglError::BadParameter);
                return EGL_FALSE;
            }
            // SAFETY: the guest's EGLint* (EGL contract).
            unsafe { (value as *mut i32).write_unaligned(v) };
            EGL_TRUE
        })
    }
}

// ---------------------------------------------------------------------------
// Window surfaces.

fn create_window_surface(dpy: usize, config: usize, win: usize, list: Vec<(i64, i64)>) -> usize {
    entered();
    let mut extra = Vec::new();
    for (k, v) in list {
        match k as i32 {
            // Linear is the default; sRGB goes to ANGLE.
            EGL_GL_COLORSPACE_KHR if v as i32 == EGL_GL_COLORSPACE_LINEAR_KHR => {}
            EGL_GL_COLORSPACE_KHR => extra.extend([k as i32, v as i32]),
            EGL_RENDER_BUFFER if v as i32 == EGL_BACK_BUFFER => {}
            _ => {
                set_error(EglError::BadAttribute);
                return 0;
            }
        }
    }
    if crate::native_window::lookup(win).is_none() {
        set_error(EglError::BadNativeWindow);
        return 0;
    }
    match WindowSurface::create(dpy, config, win, extra) {
        Ok(s) => {
            let s = Arc::new(Mutex::new(s));
            let h = Arc::as_ptr(&s) as usize;
            surfaces().get_or_insert_with(HashMap::new).insert(h, s);
            h
        }
        Err(e) => {
            set_error(e);
            0
        }
    }
}

gfn! {
    unsafe fn create_window_surf(dpy: usize, config: usize, win: usize, attribs: usize) -> usize {
        crate::guard(0, || {
            // SAFETY: the guest's EGLint list (EGL contract).
            let l = unsafe { read_list::<i32>(attribs) };
            create_window_surface(dpy, config, win, l)
        })
    }
}

gfn! {
    unsafe fn create_platform_window_surf(dpy: usize, config: usize, win: usize, attribs: usize) -> usize {
        crate::guard(0, || {
            // SAFETY: the guest's EGLAttrib list (EGL 1.5 contract).
            let l = unsafe { read_list::<i64>(attribs) };
            create_window_surface(dpy, config, win, l)
        })
    }
}

gfn! {
    unsafe fn destroy_surface(dpy: usize, s: usize) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            let w = surfaces().as_mut().and_then(|m| m.remove(&s));
            if w.is_some() {
                // Dropped here: ANGLE destroys the pbuffer once it is no
                // longer current.
                return EGL_TRUE;
            }
            match angle_fn!("eglDestroySurface", fn(usize, usize) -> u32) {
                // SAFETY: forwarded.
                Some(f) => unsafe { f(dpy, s) },
                None => EGL_FALSE,
            }
        })
    }
}

gfn! {
    unsafe fn make_current(dpy: usize, draw: usize, read: usize, ctx: usize) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            u32::from(make_current_raw(dpy, map(draw), map(read), ctx))
        })
    }
}

fn swap_ours(s: usize) -> Option<u32> {
    let w = ours(s)?;
    let r = w.lock().unwrap_or_else(|e| e.into_inner()).swap();
    Some(match r {
        Ok(()) => EGL_TRUE,
        Err(e) => {
            set_error(e);
            EGL_FALSE
        }
    })
}

gfn! {
    unsafe fn swap_buffers(dpy: usize, s: usize) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            if let Some(r) = swap_ours(s) {
                return r;
            }
            match angle_fn!("eglSwapBuffers", fn(usize, usize) -> u32) {
                // SAFETY: forwarded.
                Some(f) => unsafe { f(dpy, s) },
                None => EGL_FALSE,
            }
        })
    }
}

gfn! {
    /// Damage is a hint: the whole buffer is presented.
    unsafe fn swap_buffers_with_damage(dpy: usize, s: usize, rects: usize, n: i32) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            if let Some(r) = swap_ours(s) {
                return r;
            }
            match angle_fn!("eglSwapBuffersWithDamageKHR", fn(usize, usize, usize, i32) -> u32) {
                // SAFETY: forwarded.
                Some(f) => unsafe { f(dpy, s, rects, n) },
                None => EGL_FALSE,
            }
        })
    }
}

gfn! {
    unsafe fn get_current_surface(which: i32) -> usize {
        crate::guard(0, || {
            entered();
            match angle_fn!("eglGetCurrentSurface", fn(i32) -> usize) {
                // SAFETY: forwarded.
                Some(f) => unmap(unsafe { f(which) }),
                None => 0,
            }
        })
    }
}

gfn! {
    unsafe fn swap_interval(dpy: usize, interval: i32) -> u32 {
        crate::guard(EGL_FALSE, || {
            entered();
            let cur = Current::get();
            let h = unmap(cur.draw);
            if let Some(w) = ours(h) {
                w.lock().unwrap_or_else(|e| e.into_inner()).interval = interval.clamp(0, 4) as u32;
                return EGL_TRUE;
            }
            match angle_fn!("eglSwapInterval", fn(usize, i32) -> u32) {
                // SAFETY: forwarded.
                Some(f) => unsafe { f(dpy, interval) },
                None => EGL_FALSE,
            }
        })
    }
}

gfn! {
    unsafe fn get_proc_address(name: *const c_char) -> usize {
        crate::guard(0, || {
            entered();
            if name.is_null() {
                return 0;
            }
            // SAFETY: a NUL-terminated name (EGL contract).
            let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            crate::resolve(&n).unwrap_or(0)
        })
    }
}

gfn! {
    unsafe fn get_error() -> i32 {
        crate::guard(EGL_SUCCESS, || {
            let p = PENDING.with(|p| p.replace(0));
            let a = angle_error();
            if p != 0 { p } else { a }
        })
    }
}

/// Functions that take a surface after the display: the handle is mapped
/// and the call forwarded.
macro_rules! surface_fns {
    ($($f:ident = $angle:literal ($($a:ident: $t:ty),*);)*) => {
        $(
            gfn! {
                unsafe fn $f(dpy: usize, s: usize, $($a: $t),*) -> u32 {
                    crate::guard(EGL_FALSE, || {
                        entered();
                        match angle_fn!($angle, fn(usize, usize, $($t),*) -> u32) {
                            // SAFETY: forwarded with ANGLE's surface handle.
                            Some(f) => unsafe { f(dpy, map(s), $($a),*) },
                            None => EGL_FALSE,
                        }
                    })
                }
            }
        )*
        fn surface_fn_table() -> Vec<(&'static str, usize)> {
            vec![$(($angle, $f as usize)),*]
        }
    };
}

surface_fns! {
    query_surface = "eglQuerySurface"(attr: i32, value: usize);
    surface_attrib = "eglSurfaceAttrib"(attr: i32, value: i32);
    bind_tex_image = "eglBindTexImage"(buffer: i32);
    release_tex_image = "eglReleaseTexImage"(buffer: i32);
    copy_buffers = "eglCopyBuffers"(target: usize);
    set_damage_region = "eglSetDamageRegionKHR"(rects: usize, n: i32);
    presentation_time = "eglPresentationTimeANDROID"(time: i64);
    query_surface64 = "eglQuerySurface64KHR"(attr: i32, value: usize);
    post_sub_buffer = "eglPostSubBufferNV"(x: i32, y: i32, w: i32, h: i32);
    query_surface_pointer = "eglQuerySurfacePointerANGLE"(attr: i32, value: usize);
    lock_surface = "eglLockSurfaceKHR"(attribs: usize);
    unlock_surface = "eglUnlockSurfaceKHR"();
    next_frame_id = "eglGetNextFrameIdANDROID"(id: usize);
    frame_timestamps = "eglGetFrameTimestampsANDROID"(frame: u64, n: i32, names: usize, values: usize);
    compositor_timing = "eglGetCompositorTimingANDROID"(n: i32, names: usize, values: usize);
    frame_timestamp_supported = "eglGetFrameTimestampSupportedANDROID"(name: i32);
    compositor_timing_supported = "eglGetCompositorTimingSupportedANDROID"(name: i32);
    msc_rate = "eglGetMscRateANGLE"(num: usize, den: usize);
    prepare_swap = "eglPrepareSwapBuffersANGLE"();
}

/// The EGL functions implemented here, by name.
pub fn intercepts() -> Vec<(&'static str, usize)> {
    let mut v = vec![
        ("eglGetDisplay", get_display as usize),
        ("eglGetPlatformDisplay", get_platform_display as usize),
        (
            "eglGetPlatformDisplayEXT",
            get_platform_display_ext as usize,
        ),
        ("eglChooseConfig", choose_config as usize),
        ("eglGetConfigAttrib", get_config_attrib as usize),
        ("eglCreateWindowSurface", create_window_surf as usize),
        (
            "eglCreatePlatformWindowSurface",
            create_platform_window_surf as usize,
        ),
        // The EXT variant takes an EGLint list, like eglCreateWindowSurface.
        (
            "eglCreatePlatformWindowSurfaceEXT",
            create_window_surf as usize,
        ),
        ("eglDestroySurface", destroy_surface as usize),
        ("eglMakeCurrent", make_current as usize),
        ("eglSwapBuffers", swap_buffers as usize),
        (
            "eglSwapBuffersWithDamageKHR",
            swap_buffers_with_damage as usize,
        ),
        (
            "eglSwapBuffersWithDamageEXT",
            swap_buffers_with_damage as usize,
        ),
        ("eglGetCurrentSurface", get_current_surface as usize),
        ("eglSwapInterval", swap_interval as usize),
        ("eglGetProcAddress", get_proc_address as usize),
        ("eglGetError", get_error as usize),
    ];
    v.extend(surface_fn_table());
    v
}

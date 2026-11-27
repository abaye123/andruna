//! Callbacks from ANGLE into guest code: debug output and the blob cache.
//!
//! The guest passes functions with the guest convention; ANGLE calls
//! callbacks with the Microsoft convention. Each registration is replaced by
//! a host trampoline that calls the guest function with its convention.
//!
//! Guest code may only run on guest threads (with the guest's thread
//! pointer and stack set up by andruna-linker). ANGLE can call back from
//! its own worker threads (shader compilation, program linking), so a
//! trampoline on another thread does not call the guest: a debug message
//! is dropped, a blob cache lookup misses and a store is skipped. A thread
//! counts as a guest thread once it entered one of the EGL functions in
//! [`crate::egl`] (every GL thread makes a context current first).

use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::angle::angle_fn;

thread_local! {
    static GUEST: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as a guest thread.
pub fn mark_guest_thread() {
    GUEST.with(|g| g.set(true));
}

fn on_guest_thread() -> bool {
    GUEST.with(|g| g.get())
}

type GlDebugProc = gty!(fn(u32, u32, u32, u32, i32, *const c_char, *const c_void));
type EglDebugProc = gty!(fn(u32, *const c_char, i32, usize, usize, *const c_char));
type EglSetBlob = gty!(fn(*const c_void, isize, *const c_void, isize));
type EglGetBlob = gty!(fn(*const c_void, isize, *mut c_void, isize) -> isize);
type GlSetBlob = gty!(fn(*const c_void, isize, *const c_void, isize, *const c_void));
type GlGetBlob = gty!(fn(*const c_void, isize, *mut c_void, isize, *const c_void) -> isize);

// ---------------------------------------------------------------------------
// GL debug output (glDebugMessageCallback, KHR_debug).

struct GlDebug {
    f: GlDebugProc,
    user: *const c_void,
}

unsafe extern "C" fn gl_debug_tramp(
    source: u32,
    ty: u32,
    id: u32,
    severity: u32,
    length: i32,
    message: *const c_char,
    user: *const c_void,
) {
    if user.is_null() || !on_guest_thread() {
        return;
    }
    // SAFETY: `user` is the leaked GlDebug of the registration.
    let d = unsafe { &*(user as *const GlDebug) };
    // SAFETY: the guest's callback with its convention and ANGLE's message.
    unsafe { (d.f)(source, ty, id, severity, length, message, d.user) }
}

fn set_gl_debug(name: &'static str, cb: usize, user: usize) {
    let f = match name {
        "glDebugMessageCallback" => angle_fn!("glDebugMessageCallback", fn(usize, usize)),
        _ => angle_fn!("glDebugMessageCallbackKHR", fn(usize, usize)),
    };
    let Some(f) = f else { return };
    if cb == 0 {
        // SAFETY: ANGLE's function; null removes the callback.
        unsafe { f(0, 0) };
        return;
    }
    // One small allocation per registration, kept for the process: ANGLE
    // may still call the previous one while it is replaced.
    let d = Box::leak(Box::new(GlDebug {
        // SAFETY: a guest function of this type (KHR_debug contract).
        f: unsafe { core::mem::transmute::<usize, GlDebugProc>(cb) },
        user: user as *const c_void,
    }));
    // SAFETY: ANGLE's function with a host callback and its data.
    unsafe { f(gl_debug_tramp as usize, d as *const GlDebug as usize) };
}

gfn! {
    unsafe fn gl_debug_message_callback(cb: usize, user: usize) -> () {
        crate::guard((), || set_gl_debug("glDebugMessageCallback", cb, user))
    }
}

gfn! {
    unsafe fn gl_debug_message_callback_khr(cb: usize, user: usize) -> () {
        crate::guard((), || set_gl_debug("glDebugMessageCallbackKHR", cb, user))
    }
}

// ---------------------------------------------------------------------------
// EGL debug output (EGL_KHR_debug): one callback per process.

static EGL_DEBUG: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn egl_debug_tramp(
    error: u32,
    command: *const c_char,
    ty: i32,
    thread_label: usize,
    object_label: usize,
    message: *const c_char,
) {
    let cb = EGL_DEBUG.load(Ordering::Acquire);
    if cb == 0 || !on_guest_thread() {
        return;
    }
    // SAFETY: the guest's registered callback of this type.
    let f: EglDebugProc = unsafe { core::mem::transmute(cb) };
    // SAFETY: as above, with ANGLE's arguments.
    unsafe { f(error, command, ty, thread_label, object_label, message) }
}

gfn! {
    unsafe fn egl_debug_message_control(cb: usize, attribs: usize) -> i32 {
        crate::guard(0x300C, || {
            let Some(f) = angle_fn!("eglDebugMessageControlKHR", fn(usize, usize) -> i32) else {
                return 0x3001;
            };
            EGL_DEBUG.store(cb, Ordering::Release);
            let host = if cb == 0 { 0 } else { egl_debug_tramp as usize };
            // SAFETY: ANGLE's function with a host callback.
            unsafe { f(host, attribs) }
        })
    }
}

// ---------------------------------------------------------------------------
// Blob caches: EGL_ANDROID_blob_cache (per display, no user data: one pair
// per process, like Android's single cache) and GL_ANGLE_blob_cache.

static EGL_SET: AtomicUsize = AtomicUsize::new(0);
static EGL_GET: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn egl_set_tramp(k: *const c_void, ks: isize, v: *const c_void, vs: isize) {
    let cb = EGL_SET.load(Ordering::Acquire);
    if cb == 0 || !on_guest_thread() {
        return;
    }
    // SAFETY: the guest's registered function of this type.
    let f: EglSetBlob = unsafe { core::mem::transmute(cb) };
    // SAFETY: as above, with ANGLE's buffers.
    unsafe { f(k, ks, v, vs) }
}

unsafe extern "C" fn egl_get_tramp(
    k: *const c_void,
    ks: isize,
    v: *mut c_void,
    vs: isize,
) -> isize {
    let cb = EGL_GET.load(Ordering::Acquire);
    if cb == 0 || !on_guest_thread() {
        return 0;
    }
    // SAFETY: the guest's registered function of this type.
    let f: EglGetBlob = unsafe { core::mem::transmute(cb) };
    // SAFETY: as above, with ANGLE's buffers.
    unsafe { f(k, ks, v, vs) }
}

gfn! {
    unsafe fn egl_set_blob_cache_funcs(dpy: usize, set: usize, get: usize) -> () {
        crate::guard((), || {
            let Some(f) = angle_fn!("eglSetBlobCacheFuncsANDROID", fn(usize, usize, usize)) else {
                return;
            };
            EGL_SET.store(set, Ordering::Release);
            EGL_GET.store(get, Ordering::Release);
            let (s, g) = if set == 0 || get == 0 {
                (0, 0)
            } else {
                (egl_set_tramp as usize, egl_get_tramp as usize)
            };
            // SAFETY: ANGLE's function with host callbacks.
            unsafe { f(dpy, s, g) }
        })
    }
}

struct GlBlob {
    set: GlSetBlob,
    get: GlGetBlob,
    user: *const c_void,
}

unsafe extern "C" fn gl_set_tramp(
    k: *const c_void,
    ks: isize,
    v: *const c_void,
    vs: isize,
    u: *const c_void,
) {
    if u.is_null() || !on_guest_thread() {
        return;
    }
    // SAFETY: `u` is the leaked GlBlob of the registration.
    let b = unsafe { &*(u as *const GlBlob) };
    // SAFETY: the guest's function with its convention.
    unsafe { (b.set)(k, ks, v, vs, b.user) }
}

unsafe extern "C" fn gl_get_tramp(
    k: *const c_void,
    ks: isize,
    v: *mut c_void,
    vs: isize,
    u: *const c_void,
) -> isize {
    if u.is_null() || !on_guest_thread() {
        return 0;
    }
    // SAFETY: as above.
    let b = unsafe { &*(u as *const GlBlob) };
    // SAFETY: as above.
    unsafe { (b.get)(k, ks, v, vs, b.user) }
}

gfn! {
    unsafe fn gl_blob_cache_callbacks(set: usize, get: usize, user: usize) -> () {
        crate::guard((), || {
            let Some(f) = angle_fn!("glBlobCacheCallbacksANGLE", fn(usize, usize, usize)) else {
                return;
            };
            if set == 0 || get == 0 {
                // SAFETY: ANGLE's function; null callbacks remove them.
                unsafe { f(0, 0, 0) };
                return;
            }
            let b = Box::leak(Box::new(GlBlob {
                // SAFETY: guest functions of these types (extension contract).
                set: unsafe { core::mem::transmute::<usize, GlSetBlob>(set) },
                // SAFETY: as above.
                get: unsafe { core::mem::transmute::<usize, GlGetBlob>(get) },
                user: user as *const c_void,
            }));
            // SAFETY: ANGLE's function with host callbacks and their data.
            unsafe { f(gl_set_tramp as usize, gl_get_tramp as usize, b as *const GlBlob as usize) }
        })
    }
}

/// The callback-taking functions implemented here, by name.
pub fn intercepts() -> Vec<(&'static str, usize)> {
    vec![
        ("glDebugMessageCallback", gl_debug_message_callback as usize),
        (
            "glDebugMessageCallbackKHR",
            gl_debug_message_callback_khr as usize,
        ),
        (
            "eglDebugMessageControlKHR",
            egl_debug_message_control as usize,
        ),
        (
            "eglSetBlobCacheFuncsANDROID",
            egl_set_blob_cache_funcs as usize,
        ),
        (
            "glBlobCacheCallbacksANGLE",
            gl_blob_cache_callbacks as usize,
        ),
    ]
}

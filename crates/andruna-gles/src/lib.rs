//! `andruna-gles`: OpenGL ES and EGL for guest code.
//!
//! Guest code (Android ELF, ADR 0013) links against `libEGL.so`,
//! `libGLESv1_CM.so`, `libGLESv2.so` and `libGLESv3.so` as on Android.
//! In Andruna those are small generated stub libraries (`guest/`, built
//! freestanding by `tools/build_guest.py`): each exported function jumps
//! through a slot that the library's constructor fills with
//! [`__andruna_gles_resolve`](resolve_entry), a host function that the
//! loader binds (see [`register`]). The slots point to:
//!
//! - on x64, a generated thunk from the guest convention (System V) to
//!   ANGLE's DLLs (Microsoft x64), see [`thunk`];
//! - on arm64, ANGLE's export itself (the conventions agree for these
//!   functions);
//! - for the EGL functions that concern native windows, displays, errors,
//!   configs and callbacks, the host implementations in [`egl`].
//!
//! ANGLE runs on Direct3D 11 ([`angle`]). Window surfaces are composition
//! swap chains on a DirectComposition surface handle that a compositor
//! displays ([`window::WindowTarget`]); [`harness`] is a standalone
//! window that implements it for tests and demos.
//!
//! The current context is ANGLE's, per Windows thread: every guest thread
//! is one Windows thread (andruna-linker), so EGL's per-thread current
//! context and error state are the guest thread's.

/// An (unsafe) function pointer type with the guest convention.
#[cfg(target_arch = "x86_64")]
macro_rules! gty {
    ($($t:tt)*) => { unsafe extern "sysv64" $($t)* };
}
#[cfg(not(target_arch = "x86_64"))]
macro_rules! gty {
    ($($t:tt)*) => { unsafe extern "C" $($t)* };
}

/// Defines a host function that guest code calls, with the guest
/// convention (System V on x64, AAPCS64 on arm64).
#[cfg(target_arch = "x86_64")]
macro_rules! gfn {
    ($(#[$m:meta])* $v:vis unsafe fn $name:ident($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)? $body:block) => {
        $(#[$m])* $v unsafe extern "sysv64" fn $name($($a: $t),*) $(-> $r)? $body
    };
}
#[cfg(not(target_arch = "x86_64"))]
macro_rules! gfn {
    ($(#[$m:meta])* $v:vis unsafe fn $name:ident($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)? $body:block) => {
        $(#[$m])* $v unsafe extern "C" fn $name($($a: $t),*) $(-> $r)? $body
    };
}

pub mod angle;
pub mod callbacks;
pub mod egl;
pub mod harness;
pub mod native_window;
pub mod thunk;
pub mod window;

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Once, OnceLock};

/// Writes a diagnostic line to standard error.
pub fn log(args: std::fmt::Arguments<'_>) {
    eprintln!("[andruna-gles] {args}");
}

/// `ANDRUNA_GLES_TRACE`: report resolution and surface events.
pub fn tracing() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var_os("ANDRUNA_GLES_TRACE").is_some())
}

/// Runs a host function body; a panic becomes `fail` (nothing unwinds into
/// guest code).
pub(crate) fn guard<T>(fail: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            log(format_args!("host function panicked"));
            fail
        }
    }
}

/// The entry point table by name.
pub(crate) fn index() -> &'static HashMap<&'static str, thunk::Entry> {
    static I: OnceLock<HashMap<&'static str, thunk::Entry>> = OnceLock::new();
    I.get_or_init(|| thunk::entries().into_iter().map(|e| (e.name, e)).collect())
}

/// Host implementations that replace ANGLE's functions or have no ANGLE
/// counterpart (the window harness).
fn host_functions() -> &'static HashMap<&'static str, usize> {
    static H: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    H.get_or_init(|| {
        egl::intercepts()
            .into_iter()
            .chain(callbacks::intercepts())
            .chain(harness::symbols())
            .collect()
    })
}

/// The guest address of function `name`, or `None` when neither a host
/// implementation nor ANGLE provides it.
pub fn resolve(name: &str) -> Option<usize> {
    if let Some(&a) = host_functions().get(name) {
        // Host EGL functions call ANGLE: load it now.
        let _ = angle::load();
        return Some(a);
    }
    let e = index().get(name)?;
    angle::load().ok()?;
    let p = thunk::SLOTS[e.slot].load(std::sync::atomic::Ordering::Relaxed);
    if p == 0 {
        return None;
    }
    if cfg!(target_arch = "x86_64") {
        Some(e.thunk)
    } else {
        Some(p)
    }
}

gfn! {
    /// What a stub slot points to when its function is unavailable (ANGLE
    /// missing or not exporting it): returns 0, which is `EGL_FALSE`,
    /// `EGL_NO_*`, `GL_FALSE` or a null pointer.
    unsafe fn unavailable() -> usize {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| log(format_args!(
            "an unavailable EGL/GLES function was called (ANDRUNA_GLES_TRACE lists them)"
        )));
        0
    }
}

gfn! {
    /// `__andruna_gles_resolve(name)`: the stub libraries' binding
    /// function. Never returns null.
    ///
    /// # Safety
    ///
    /// `name` is null or a NUL-terminated string.
    pub unsafe fn resolve_entry(name: *const c_char) -> usize {
        guard(unavailable as usize, || {
            if name.is_null() {
                return unavailable as usize;
            }
            // SAFETY: the stub libraries pass NUL-terminated names.
            let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            match resolve(&n) {
                Some(a) => a,
                None => {
                    if tracing() {
                        log(format_args!("unavailable: {n}"));
                    }
                    unavailable as usize
                }
            }
        })
    }
}

/// The host symbols guest code imports from this crate.
pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![("__andruna_gles_resolve", resolve_entry as usize as u64)]
}

/// Adds this crate's host symbols to andruna-linker's table (once).
pub fn register() {
    static R: Once = Once::new();
    R.call_once(|| andruna_linker::host::register(&symbols()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_host_functions() {
        let i = index();
        assert_eq!(i.len(), thunk::COUNT);
        for n in [
            "eglGetDisplay",
            "glDrawArrays",
            "glAlphaFunc",
            "glDispatchCompute",
        ] {
            assert!(i.contains_key(n), "{n}");
        }
        // Host functions resolve without ANGLE; unknown names do not.
        assert!(resolve("andruna_window_create").is_some());
        assert!(resolve("glNoSuchFunctionANDRUNA").is_none());
        let n = std::ffi::CString::new("glNoSuchFunctionANDRUNA").unwrap();
        // SAFETY: the binding function with a NUL-terminated name.
        assert_eq!(unsafe { resolve_entry(n.as_ptr()) }, unavailable as usize);
    }
}

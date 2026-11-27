//! `ANativeWindow` objects for guest code, backed by a [`WindowTarget`].
//!
//! The layout is Android's (`system/window.h`, `nativebase/nativebase.h`,
//! LP64), checked at compile time below. Guest code passes the pointer to
//! `eglCreateWindowSurface`, and libnativewindow-style callers can use
//! `query`/`perform` and the reference count. Only rendering through EGL
//! is supported: the buffer queue functions (`dequeueBuffer`,
//! `queueBuffer`, CPU `lock`) report `-ENOSYS`, so CPU rendering into the
//! window fails honestly instead of silently drawing nothing.
//!
//! The function pointers are host functions with the guest convention.

use std::collections::HashSet;
use std::ffi::{c_int, c_void};
use std::mem::offset_of;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::window::{FrameStats, WindowTarget};

/// `ANDROID_NATIVE_MAKE_CONSTANT('_','w','n','d')`.
pub const ANDROID_NATIVE_WINDOW_MAGIC: i32 =
    ((b'_' as i32) << 24) | ((b'w' as i32) << 16) | ((b'n' as i32) << 8) | (b'd' as i32);

const EINVAL: c_int = 22;
const ENOSYS: c_int = 38;

// query() keys (system/window.h).
const NATIVE_WINDOW_WIDTH: c_int = 0;
const NATIVE_WINDOW_HEIGHT: c_int = 1;
const NATIVE_WINDOW_FORMAT: c_int = 2;
const NATIVE_WINDOW_MIN_UNDEQUEUED_BUFFERS: c_int = 3;
const NATIVE_WINDOW_QUEUES_TO_WINDOW_COMPOSER: c_int = 4;
const NATIVE_WINDOW_CONCRETE_TYPE: c_int = 5;
const NATIVE_WINDOW_DEFAULT_WIDTH: c_int = 6;
const NATIVE_WINDOW_DEFAULT_HEIGHT: c_int = 7;
const NATIVE_WINDOW_TRANSFORM_HINT: c_int = 8;
const NATIVE_WINDOW_CONSUMER_RUNNING_BEHIND: c_int = 9;
const NATIVE_WINDOW_IS_VALID: c_int = 17;
const NATIVE_WINDOW_SURFACE: c_int = 1;

// perform() operations (system/window.h) that need no buffer queue.
const NATIVE_WINDOW_SET_USAGE: c_int = 0;
const NATIVE_WINDOW_CONNECT: c_int = 1;
const NATIVE_WINDOW_DISCONNECT: c_int = 2;
const NATIVE_WINDOW_SET_BUFFERS_FORMAT: c_int = 9;
const NATIVE_WINDOW_API_CONNECT: c_int = 13;
const NATIVE_WINDOW_API_DISCONNECT: c_int = 14;
const NATIVE_WINDOW_SET_BUFFERS_DATASPACE: c_int = 19;
const NATIVE_WINDOW_SET_SURFACE_DAMAGE: c_int = 20;

/// `HAL_PIXEL_FORMAT_RGBA_8888`.
pub const HAL_PIXEL_FORMAT_RGBA_8888: c_int = 1;

/// `struct android_native_base_t`.
#[repr(C)]
pub struct NativeBase {
    pub magic: c_int,
    pub version: c_int,
    pub reserved: [usize; 4],
    pub inc_ref: usize,
    pub dec_ref: usize,
}

/// `struct ANativeWindow`.
#[repr(C)]
pub struct ANativeWindow {
    pub common: NativeBase,
    pub flags: u32,
    pub min_swap_interval: c_int,
    pub max_swap_interval: c_int,
    pub xdpi: f32,
    pub ydpi: f32,
    pub oem: [isize; 4],
    pub set_swap_interval: usize,
    pub dequeue_buffer_deprecated: usize,
    pub lock_buffer_deprecated: usize,
    pub queue_buffer_deprecated: usize,
    pub query: usize,
    pub perform: usize,
    pub cancel_buffer_deprecated: usize,
    pub dequeue_buffer: usize,
    pub queue_buffer: usize,
    pub cancel_buffer: usize,
}

// Android's LP64 layout (x86_64 and arm64).
const _: () = {
    assert!(size_of::<NativeBase>() == 56);
    assert!(offset_of!(NativeBase, inc_ref) == 40);
    assert!(offset_of!(NativeBase, dec_ref) == 48);
    assert!(offset_of!(ANativeWindow, flags) == 56);
    assert!(offset_of!(ANativeWindow, min_swap_interval) == 60);
    assert!(offset_of!(ANativeWindow, max_swap_interval) == 64);
    assert!(offset_of!(ANativeWindow, xdpi) == 68);
    assert!(offset_of!(ANativeWindow, ydpi) == 72);
    assert!(offset_of!(ANativeWindow, oem) == 80);
    assert!(offset_of!(ANativeWindow, set_swap_interval) == 112);
    assert!(offset_of!(ANativeWindow, query) == 144);
    assert!(offset_of!(ANativeWindow, perform) == 152);
    assert!(offset_of!(ANativeWindow, dequeue_buffer) == 168);
    assert!(offset_of!(ANativeWindow, cancel_buffer) == 184);
    assert!(size_of::<ANativeWindow>() == 192);
};

/// The object behind a guest `ANativeWindow*` (the window is its first
/// field, so the two pointers are equal).
#[repr(C)]
pub struct NativeWindow {
    pub anw: ANativeWindow,
    refs: AtomicUsize,
    /// `setSwapInterval` value: the default of surfaces made from it.
    pub swap_interval: AtomicI32,
    pub target: Arc<dyn WindowTarget>,
    pub stats: Arc<Mutex<FrameStats>>,
}

/// Live windows (addresses), so pointers from guest code are checked
/// before use.
static LIVE: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

fn live() -> std::sync::MutexGuard<'static, Option<HashSet<usize>>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Creates a window for `target` with one reference, owned by the caller
/// (release it with [`release`]).
pub fn create(target: Arc<dyn WindowTarget>) -> *mut ANativeWindow {
    let w = Box::new(NativeWindow {
        anw: ANativeWindow {
            common: NativeBase {
                magic: ANDROID_NATIVE_WINDOW_MAGIC,
                version: size_of::<ANativeWindow>() as c_int,
                reserved: [0; 4],
                inc_ref: inc_ref as usize,
                dec_ref: dec_ref as usize,
            },
            flags: 0,
            min_swap_interval: 0,
            max_swap_interval: 1,
            xdpi: 96.0,
            ydpi: 96.0,
            oem: [0; 4],
            set_swap_interval: set_swap_interval as usize,
            dequeue_buffer_deprecated: no_buffer_queue as usize,
            lock_buffer_deprecated: no_buffer_queue as usize,
            queue_buffer_deprecated: no_buffer_queue as usize,
            query: query as usize,
            perform: perform as usize,
            cancel_buffer_deprecated: no_buffer_queue as usize,
            dequeue_buffer: no_buffer_queue as usize,
            queue_buffer: no_buffer_queue as usize,
            cancel_buffer: no_buffer_queue as usize,
        },
        refs: AtomicUsize::new(1),
        swap_interval: AtomicI32::new(1),
        target,
        stats: Arc::new(Mutex::new(FrameStats::default())),
    });
    let p = Box::into_raw(w);
    live().get_or_insert_with(HashSet::new).insert(p as usize);
    p.cast()
}

/// The window object of a guest pointer, if it is one of ours and alive.
/// The returned reference stays valid while the caller holds a reference.
pub fn lookup(p: usize) -> Option<&'static NativeWindow> {
    if live().as_ref().is_some_and(|s| s.contains(&p)) {
        // SAFETY: `p` is a live window made by `create`.
        Some(unsafe { &*(p as *const NativeWindow) })
    } else {
        None
    }
}

/// Adds a reference.
pub fn acquire(p: usize) -> bool {
    match lookup(p) {
        Some(w) => {
            w.refs.fetch_add(1, Ordering::AcqRel);
            true
        }
        None => false,
    }
}

/// Drops a reference; the last one frees the window.
pub fn release(p: usize) {
    let mut g = live();
    let Some(set) = g.as_mut() else { return };
    if !set.contains(&p) {
        return;
    }
    // SAFETY: a live window (checked under the lock).
    let w = unsafe { &*(p as *const NativeWindow) };
    if w.refs.fetch_sub(1, Ordering::AcqRel) == 1 {
        set.remove(&p);
        drop(g);
        // SAFETY: the last reference of a window made by `create`.
        drop(unsafe { Box::from_raw(p as *mut NativeWindow) });
    }
}

gfn! {
    unsafe fn inc_ref(base: *mut c_void) -> () {
        crate::guard((), || {
            acquire(base as usize);
        })
    }
}

gfn! {
    unsafe fn dec_ref(base: *mut c_void) -> () {
        crate::guard((), || release(base as usize))
    }
}

gfn! {
    unsafe fn set_swap_interval(win: *mut c_void, interval: c_int) -> c_int {
        crate::guard(-EINVAL, || match lookup(win as usize) {
            Some(w) => {
                w.swap_interval.store(interval.clamp(0, 1), Ordering::Relaxed);
                0
            }
            None => -EINVAL,
        })
    }
}

gfn! {
    unsafe fn query(win: *const c_void, what: c_int, value: *mut c_int) -> c_int {
        crate::guard(-EINVAL, || {
            let Some(w) = lookup(win as usize) else { return -EINVAL };
            if value.is_null() {
                return -EINVAL;
            }
            let (width, height) = w.target.size();
            let v = match what {
                NATIVE_WINDOW_WIDTH | NATIVE_WINDOW_DEFAULT_WIDTH => width as c_int,
                NATIVE_WINDOW_HEIGHT | NATIVE_WINDOW_DEFAULT_HEIGHT => height as c_int,
                NATIVE_WINDOW_FORMAT => HAL_PIXEL_FORMAT_RGBA_8888,
                NATIVE_WINDOW_MIN_UNDEQUEUED_BUFFERS => 1,
                NATIVE_WINDOW_QUEUES_TO_WINDOW_COMPOSER => 1,
                NATIVE_WINDOW_CONCRETE_TYPE => NATIVE_WINDOW_SURFACE,
                NATIVE_WINDOW_TRANSFORM_HINT | NATIVE_WINDOW_CONSUMER_RUNNING_BEHIND => 0,
                NATIVE_WINDOW_IS_VALID => c_int::from(w.target.is_open()),
                _ => return -EINVAL,
            };
            // SAFETY: the caller passes a writable int (contract).
            unsafe { value.write(v) };
            0
        })
    }
}

gfn! {
    /// `perform(window, operation, ...)`. Variadic in C; the integer
    /// arguments arrive in the same registers as named ones on both
    /// architectures (System V and Android's AAPCS64).
    unsafe fn perform(win: *mut c_void, op: c_int, _a: usize, _b: usize, _c: usize) -> c_int {
        crate::guard(-EINVAL, || {
            if lookup(win as usize).is_none() {
                return -EINVAL;
            }
            match op {
                // Accepted and without effect here: the swap chain's usage,
                // format and connection are fixed by the EGL surface, and
                // damage is a hint.
                NATIVE_WINDOW_SET_USAGE
                | NATIVE_WINDOW_CONNECT
                | NATIVE_WINDOW_DISCONNECT
                | NATIVE_WINDOW_API_CONNECT
                | NATIVE_WINDOW_API_DISCONNECT
                | NATIVE_WINDOW_SET_SURFACE_DAMAGE => 0,
                // RGBA_8888 and the default data space are what the swap
                // chain has; others are not supported yet.
                NATIVE_WINDOW_SET_BUFFERS_FORMAT if matches!(_a as c_int, 0 | HAL_PIXEL_FORMAT_RGBA_8888) => 0,
                NATIVE_WINDOW_SET_BUFFERS_DATASPACE if _a as c_int == 0 => 0,
                _ => -EINVAL,
            }
        })
    }
}

gfn! {
    /// The buffer queue (CPU rendering, media producers) is not available.
    unsafe fn no_buffer_queue() -> c_int {
        -ENOSYS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HANDLE;

    struct Fixed;
    impl WindowTarget for Fixed {
        fn size(&self) -> (u32, u32) {
            (320, 200)
        }
        fn surface_handle(&self) -> HANDLE {
            HANDLE::default()
        }
        fn presented(&self, _: u32, _: u32) {}
    }

    #[test]
    fn query_perform_and_references() {
        let p = create(Arc::new(Fixed)) as usize;
        let w = lookup(p).expect("live");
        assert_eq!(w.anw.common.magic, ANDROID_NATIVE_WINDOW_MAGIC);
        assert_eq!(w.anw.common.version, 192);
        let mut v = 0;
        // SAFETY: host functions called directly with valid arguments.
        unsafe {
            assert_eq!(query(p as *const c_void, NATIVE_WINDOW_WIDTH, &mut v), 0);
            assert_eq!(v, 320);
            assert_eq!(query(p as *const c_void, NATIVE_WINDOW_HEIGHT, &mut v), 0);
            assert_eq!(v, 200);
            assert_eq!(query(p as *const c_void, 9999, &mut v), -EINVAL);
            assert_eq!(
                perform(p as *mut c_void, NATIVE_WINDOW_API_CONNECT, 1, 0, 0),
                0
            );
            assert_eq!(perform(p as *mut c_void, 11, 0, 0, 0), -EINVAL);
            assert_eq!(no_buffer_queue(), -ENOSYS);
            // Not a window.
            let other = 0i32;
            assert_eq!(
                query(&other as *const i32 as *const c_void, 0, &mut v),
                -EINVAL
            );
            inc_ref(p as *mut c_void);
        }
        release(p);
        assert!(lookup(p).is_some(), "one reference left");
        release(p);
        assert!(lookup(p).is_none(), "freed with the last reference");
    }
}

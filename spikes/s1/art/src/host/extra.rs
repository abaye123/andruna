//! libc and libm functions ART and the libcore natives need that the S8
//! host did not have (spike S1 round 3). Process-wide answers are those of
//! a single-user Android process: one uid, no scheduling classes, default
//! limits. Each function documents where it is not Linux's.

use core::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use super::guard;

const UTS_LEN: usize = 65;

gfn! {
    /// `uname`: a Linux 6.6 kernel on x86_64 (ART checks the version for
    /// features such as `membarrier` and `userfaultfd`, which lx answers
    /// with ENOSYS anyway).
    pub unsafe fn h_uname(u: *mut u8) -> i32 {
        guard(|| {
            let fields: [&[u8]; 6] = [b"Linux", b"localhost", b"6.6.0-andruna", b"#1 SMP", b"x86_64", b"(none)"];
            for (i, f) in fields.iter().enumerate() {
                // SAFETY: the guest's struct utsname, 6 fields of 65 bytes.
                unsafe {
                    let d = u.add(i * UTS_LEN);
                    core::ptr::write_bytes(d, 0, UTS_LEN);
                    core::ptr::copy_nonoverlapping(f.as_ptr(), d, f.len());
                }
            }
            0
        })
    }
}

fn stack_limits() -> (usize, usize) {
    let mut low = 0usize;
    let mut high = 0usize;
    // SAFETY: two out-pointers.
    unsafe {
        windows::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high)
    };
    (low, high)
}

gfn! {
    /// `pthread_getattr_np`: the calling thread's stack (from the TEB
    /// limits; guest threads have committed stacks). Other threads get
    /// the caller's values (spike limit; ART only asks about itself).
    pub unsafe fn h_pthread_getattr_np(_t: usize, attr: *mut u8) -> i32 {
        guard(|| {
            let (low, high) = stack_limits();
            // The lowest 256 KiB of a Windows stack are never usable: the
            // guard page and the stack guarantee sit there, and touching
            // them raises STATUS_STACK_OVERFLOW (ART's implicit
            // stack-overflow set-up touches the whole reported stack).
            let low = low + (256 << 10);
            // SAFETY: bionic's LP64 pthread_attr_t (56 bytes): flags@0,
            // stack_base@8, stack_size@16, guard_size@24, policy@32, priority@36.
            unsafe {
                core::ptr::write_bytes(attr, 0, 56);
                *(attr.add(8) as *mut usize) = low;
                *(attr.add(16) as *mut usize) = high - low;
                *(attr.add(24) as *mut usize) = 4096;
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_getstack(attr: *const u8, base: *mut usize, size: *mut usize) -> i32 {
        // SAFETY: bionic's pthread_attr_t and two out-pointers.
        unsafe {
            *base = *(attr.add(8) as *const usize);
            *size = *(attr.add(16) as *const usize);
        }
        0
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_getguardsize(attr: *const u8, size: *mut usize) -> i32 {
        // SAFETY: as above.
        unsafe { *size = *(attr.add(24) as *const usize) };
        0
    }
}

gfn! {
    pub unsafe fn h_zero_out_int(_a: usize, p: *mut i32) -> i32 {
        if !p.is_null() {
            // SAFETY: an int out-parameter (sched_param, policy).
            unsafe { *p = 0 };
        }
        0
    }
}

gfn! {
    /// `getrlimit`: 8 MiB stack, everything else unlimited.
    pub unsafe fn h_getrlimit(res: i32, r: *mut u64) -> i32 {
        const RLIMIT_STACK: i32 = 3;
        const RLIMIT_NOFILE: i32 = 7;
        let v = match res {
            RLIMIT_STACK => 8 << 20,
            RLIMIT_NOFILE => 32768,
            _ => u64::MAX,
        };
        // SAFETY: the guest's struct rlimit.
        unsafe {
            *r = v;
            *r.add(1) = v;
        }
        0
    }
}

gfn! {
    /// Accepted and ignored: `prctl`, `setpriority`, `msync`, `fsync`,
    /// `fdatasync`, `pthread_kill` (no asynchronous signals), `setpgid`.
    pub unsafe fn h_ok(_a: usize, _b: usize, _c: usize, _d: usize, _e: usize) -> i32 {
        0
    }
}

gfn! {
    pub unsafe fn h_getpriority(_w: i32, _who: i32) -> i32 {
        0
    }
}

gfn! {
    /// `mincore`: every page resident.
    pub unsafe fn h_mincore(_a: *mut c_void, len: usize, vec: *mut u8) -> i32 {
        // SAFETY: one byte per page of the range.
        unsafe { core::ptr::write_bytes(vec, 1, len.div_ceil(4096)) };
        0
    }
}

gfn! {
    /// `getrusage`: all zero (no accounting yet).
    pub unsafe fn h_getrusage(_who: i32, r: *mut u8) -> i32 {
        // SAFETY: struct rusage is 144 bytes on LP64.
        unsafe { core::ptr::write_bytes(r, 0, 144) };
        0
    }
}

gfn! {
    pub unsafe fn h_sigrtmin() -> i32 {
        // bionic reserves the first real-time signals for itself.
        35
    }
}

gfn! {
    pub unsafe fn h_sigrtmax() -> i32 {
        64
    }
}

gfn! {
    pub unsafe fn h_getppid() -> i32 {
        1
    }
}

static UMASK: AtomicU32 = AtomicU32::new(0o022);

gfn! {
    pub unsafe fn h_umask(m: u32) -> u32 {
        UMASK.swap(m & 0o777, Ordering::Relaxed)
    }
}

gfn! {
    pub unsafe fn h_api_level() -> i32 {
        // Android 17 (the pinned AOSP tag, ADR 0011).
        37
    }
}

gfn! {
    pub unsafe fn h_void1(_a: usize) -> () {}
}

gfn! {
    /// `mallinfo` (returned in memory, System V): all zero.
    pub unsafe fn h_mallinfo(out: *mut u8) -> *mut u8 {
        // SAFETY: the caller's 80-byte struct mallinfo return slot.
        unsafe { core::ptr::write_bytes(out, 0, 80) };
        out
    }
}

gfn! {
    pub unsafe fn h_arc4random() -> u32 {
        let mut b = [0u8; 4];
        super::random_bytes(&mut b);
        u32::from_ne_bytes(b)
    }
}

gfn! {
    pub unsafe fn h_pthread_getname_np(_t: usize, buf: *mut c_char, len: usize) -> i32 {
        if len > 0 {
            // SAFETY: the guest's buffer of `len` bytes.
            unsafe { *buf = 0 };
        }
        0
    }
}

macro_rules! math1 {
    ($($name:ident => $f:expr;)*) => {$(
        gfn! { pub unsafe fn $name(x: f64) -> f64 { let f: fn(f64) -> f64 = $f; f(x) } }
    )*};
}
macro_rules! math2 {
    ($($name:ident => $f:expr;)*) => {$(
        gfn! { pub unsafe fn $name(x: f64, y: f64) -> f64 { let f: fn(f64, f64) -> f64 = $f; f(x, y) } }
    )*};
}

math1! {
    m_sin => f64::sin; m_cos => f64::cos; m_tan => f64::tan; m_asin => f64::asin;
    m_acos => f64::acos; m_atan => f64::atan; m_exp => f64::exp; m_log => f64::ln;
    m_log10 => f64::log10; m_cbrt => f64::cbrt; m_cosh => f64::cosh; m_sinh => f64::sinh;
    m_tanh => f64::tanh; m_log1p => f64::ln_1p; m_expm1 => f64::exp_m1;
}
math2! {
    m_atan2 => f64::atan2; m_pow => f64::powf; m_hypot => f64::hypot;
    m_fmod => |x, y| x % y;
    m_remainder => |x, y| x - (x / y).round_ties_even() * y;
}

gfn! { pub unsafe fn m_expf(x: f32) -> f32 { x.exp() } }
gfn! { pub unsafe fn m_tanhf(x: f32) -> f32 { x.tanh() } }
gfn! {
    pub unsafe fn m_modf(x: f64, ip: *mut f64) -> f64 {
        // SAFETY: the guest's out-pointer.
        unsafe { *ip = x.trunc() };
        x - x.trunc()
    }
}
gfn! {
    pub unsafe fn m_sincos(x: f64, s: *mut f64, c: *mut f64) -> () {
        // SAFETY: the guest's out-pointers.
        unsafe {
            *s = x.sin();
            *c = x.cos();
        }
    }
}

gfn! {
    /// `getpwuid_r`: no user database (result NULL, return 0, as for an
    /// unknown uid).
    pub unsafe fn h_getpwuid_r(_uid: u32, _pw: usize, _buf: usize, _len: usize, res: *mut usize) -> i32 {
        if !res.is_null() {
            // SAFETY: the guest's result pointer.
            unsafe { *res = 0 };
        }
        0
    }
}

gfn! {
    /// `android_get_LD_LIBRARY_PATH`: empty.
    pub unsafe fn h_get_ld_library_path(buf: *mut c_char, len: usize) -> () {
        if len > 0 {
            // SAFETY: the guest's buffer.
            unsafe { *buf = 0 };
        }
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("uname", h_uname as usize),
        ("getpwuid_r", h_getpwuid_r as usize),
        ("getpwnam_r", h_getpwuid_r as usize),
        ("android_get_LD_LIBRARY_PATH", h_get_ld_library_path as usize),
        ("pthread_getattr_np", h_pthread_getattr_np as usize),
        ("pthread_attr_getstack", h_pthread_attr_getstack as usize),
        ("pthread_attr_getguardsize", h_pthread_attr_getguardsize as usize),
        ("pthread_attr_getschedparam", h_zero_out_int as usize),
        ("sched_getparam", h_zero_out_int as usize),
        ("sched_getscheduler", h_getpriority as usize),
        ("getrlimit", h_getrlimit as usize),
        ("prctl", h_ok as usize),
        ("setpriority", h_ok as usize),
        ("msync", h_ok as usize),
        ("fsync", h_ok as usize),
        ("fdatasync", h_ok as usize),
        ("setpgid", h_ok as usize),
        ("getpriority", h_getpriority as usize),
        ("mincore", h_mincore as usize),
        ("getrusage", h_getrusage as usize),
        ("__libc_current_sigrtmin", h_sigrtmin as usize),
        ("__libc_current_sigrtmax", h_sigrtmax as usize),
        ("getppid", h_getppid as usize),
        ("umask", h_umask as usize),
        ("android_get_device_api_level", h_api_level as usize),
        ("android_set_application_target_sdk_version", h_void1 as usize),
        ("mallinfo", h_mallinfo as usize),
        ("arc4random", h_arc4random as usize),
        ("pthread_getname_np", h_pthread_getname_np as usize),
        ("sin", m_sin as usize),
        ("cos", m_cos as usize),
        ("tan", m_tan as usize),
        ("asin", m_asin as usize),
        ("acos", m_acos as usize),
        ("atan", m_atan as usize),
        ("exp", m_exp as usize),
        ("log", m_log as usize),
        ("log10", m_log10 as usize),
        ("cbrt", m_cbrt as usize),
        ("cosh", m_cosh as usize),
        ("sinh", m_sinh as usize),
        ("tanh", m_tanh as usize),
        ("log1p", m_log1p as usize),
        ("expm1", m_expm1 as usize),
        ("atan2", m_atan2 as usize),
        ("pow", m_pow as usize),
        ("hypot", m_hypot as usize),
        ("fmod", m_fmod as usize),
        ("remainder", m_remainder as usize),
        ("expf", m_expf as usize),
        ("tanhf", m_tanhf as usize),
        ("modf", m_modf as usize),
        ("sincos", m_sincos as usize),
    ]
}

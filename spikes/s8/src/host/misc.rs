//! The rest of the host side: allocation, process identity and exit,
//! system properties, fdsan, number conversion and formatting, and the
//! calls the spike does not support (`ENOSYS`).

use core::ffi::{c_char, c_void};
use std::collections::HashMap;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use andruna_lx::linux::errno::*;

use super::{cstr, fail_with, guard, set_errno};

// --- allocation ---------------------------------------------------------------
//
// Blocks come from the Rust system allocator with a 16-byte header just
// below the returned pointer: the raw allocation and its total size.

const HDR: usize = 16;

fn alloc(size: usize, align: usize, zero: bool) -> *mut c_void {
    let align = align.max(16);
    let total = size.saturating_add(align + HDR);
    let Ok(layout) = std::alloc::Layout::from_size_align(total, 16) else {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    };
    // SAFETY: non-zero size.
    let raw = unsafe {
        if zero {
            std::alloc::alloc_zeroed(layout)
        } else {
            std::alloc::alloc(layout)
        }
    };
    if raw.is_null() {
        set_errno(ENOMEM);
        return core::ptr::null_mut();
    }
    let user = (raw as usize + HDR + align - 1) & !(align - 1);
    // SAFETY: `user - HDR` is inside the allocation (user >= raw + HDR).
    unsafe {
        let h = (user - HDR) as *mut usize;
        h.write(raw as usize);
        h.add(1).write(total);
    }
    user as *mut c_void
}

/// # Safety
///
/// `p` is NULL or a block from this allocator.
unsafe fn header(p: *mut c_void) -> (usize, usize) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let h = (p as usize - HDR) as *const usize;
        (h.read(), h.add(1).read())
    }
}

gfn! {
    pub unsafe fn h_malloc(n: usize) -> *mut c_void {
        guard(|| alloc(n, 16, false))
    }
}

gfn! {
    pub unsafe fn h_calloc(a: usize, b: usize) -> *mut c_void {
        guard(|| match a.checked_mul(b) {
            Some(n) => alloc(n, 16, true),
            None => { set_errno(ENOMEM); core::ptr::null_mut() }
        })
    }
}

gfn! {
    pub unsafe fn h_free(p: *mut c_void) -> () {
        guard(|| {
            if p.is_null() {
                return;
            }
            // SAFETY: a block from this allocator.
            let (raw, total) = unsafe { header(p) };
            // SAFETY: allocated with this layout.
            unsafe { std::alloc::dealloc(raw as *mut u8, std::alloc::Layout::from_size_align_unchecked(total, 16)) };
        })
    }
}

gfn! {
    pub unsafe fn h_malloc_usable_size(p: *mut c_void) -> usize {
        guard(|| {
            if p.is_null() {
                return 0;
            }
            // SAFETY: a block from this allocator.
            let (raw, total) = unsafe { header(p) };
            raw + total - p as usize
        })
    }
}

gfn! {
    pub unsafe fn h_realloc(p: *mut c_void, n: usize) -> *mut c_void {
        guard(|| {
            if p.is_null() {
                return alloc(n, 16, false);
            }
            // SAFETY: a block from this allocator.
            let old = unsafe { h_malloc_usable_size(p) };
            if n <= old {
                return p;
            }
            let q = alloc(n, 16, false);
            if !q.is_null() {
                // SAFETY: both blocks are at least `old` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(p as *const u8, q as *mut u8, old);
                    h_free(p);
                }
            }
            q
        })
    }
}

gfn! {
    pub unsafe fn h_aligned_alloc(align: usize, n: usize) -> *mut c_void {
        guard(|| {
            if !align.is_power_of_two() {
                set_errno(EINVAL);
                return core::ptr::null_mut();
            }
            alloc(n, align, false)
        })
    }
}

gfn! {
    pub unsafe fn h_memalign(align: usize, n: usize) -> *mut c_void {
        guard(|| alloc(n, align.next_power_of_two(), false))
    }
}

gfn! {
    pub unsafe fn h_posix_memalign(out: *mut *mut c_void, align: usize, n: usize) -> i32 {
        guard(|| {
            if !align.is_power_of_two() || align < 8 {
                return EINVAL;
            }
            let p = alloc(n, align, false);
            if p.is_null() {
                return ENOMEM;
            }
            // SAFETY: the guest's out parameter.
            unsafe { *out = p };
            0
        })
    }
}

// --- process ----------------------------------------------------------------

gfn! {
    pub unsafe fn h_exit(status: i32) -> () {
        guard(|| {
            crate::report::at_guest_exit(status);
            std::process::exit(status)
        })
    }
}

gfn! {
    pub unsafe fn h_abort() -> () {
        guard(|| {
            eprintln!("[s8] guest called abort()");
            crate::debug::print_guest_backtrace();
            crate::report::at_guest_exit(134);
            std::process::exit(134)
        })
    }
}

static ABORT_MESSAGE: Mutex<String> = Mutex::new(String::new());

gfn! {
    pub unsafe fn h_android_set_abort_message(m: *const c_char) -> () {
        guard(|| {
            // SAFETY: a guest string.
            if let Some(b) = unsafe { cstr(m) } {
                *ABORT_MESSAGE.lock().unwrap() = String::from_utf8_lossy(b).into_owned();
                eprintln!("[s8] abort message: {}", String::from_utf8_lossy(b));
            }
        })
    }
}

gfn! {
    pub unsafe fn h_getuid() -> u32 {
        guard(andruna_lx::process::uid)
    }
}

gfn! {
    pub unsafe fn h_getgid() -> u32 {
        guard(andruna_lx::process::gid)
    }
}

gfn! {
    pub unsafe fn h_getgroups(_n: i32, _l: *mut u32) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_set_id(_id: u32) -> i32 {
        guard(|| fail_with(EPERM))
    }
}

gfn! {
    pub unsafe fn h_kill(pid: i32, sig: i32) -> i32 {
        guard(|| {
            if sig == 0 && (pid == andruna_lx::process::getpid() || pid == 0) {
                return 0;
            }
            fail_with(ENOSYS)
        })
    }
}

gfn! {
    pub unsafe fn h_sysconf(name: i32) -> i64 {
        guard(|| match name {
            0x27 | 0x28 => 4096,
            0x06 => 100,
            0x0b => 1024,
            0x48 => 1024,
            0x00 => 131072,
            0x60 | 0x61 => std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1),
            0x62 => 1 << 20,
            _ => { set_errno(EINVAL); -1 }
        })
    }
}

gfn! {
    pub unsafe fn h_getpagesize() -> i32 {
        guard(|| 4096)
    }
}

gfn! {
    /// `getauxval`: the page size, and on arm64 the `AT_HWCAP` bits that
    /// compiler-rt's outline atomics test (`HWCAP_ATOMICS`).
    pub unsafe fn h_getauxval(ty: u64) -> u64 {
        guard(|| match ty {
            6 => 4096, // AT_PAGESZ
            16 => hwcap(), // AT_HWCAP
            _ => {
                set_errno(ENOENT);
                0
            }
        })
    }
}

#[cfg(target_arch = "aarch64")]
fn hwcap() -> u64 {
    use windows::Win32::System::Threading::{
        IsProcessorFeaturePresent, PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE,
    };
    // SAFETY: a plain query.
    let lse =
        unsafe { IsProcessorFeaturePresent(PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE) }.as_bool();
    // HWCAP_FP | HWCAP_ASIMD, plus HWCAP_ATOMICS when present.
    0b11 | if lse { 1 << 8 } else { 0 }
}

#[cfg(not(target_arch = "aarch64"))]
fn hwcap() -> u64 {
    0
}

// --- system properties: an in-process store ------------------------------------

struct Prop {
    name: Vec<u8>,
    value: Mutex<Vec<u8>>,
    serial: AtomicU32,
}

static PROPS: Mutex<Option<HashMap<Vec<u8>, &'static Prop>>> = Mutex::new(None);
static AREA_SERIAL: AtomicU32 = AtomicU32::new(1);

fn find_prop(name: &[u8]) -> Option<&'static Prop> {
    PROPS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(name)
        .copied()
}

gfn! {
    pub unsafe fn h_prop_find(name: *const c_char) -> *const c_void {
        guard(|| {
            // SAFETY: a guest string.
            let n = unsafe { cstr(name) }.unwrap_or(b"");
            find_prop(n).map(|p| p as *const Prop as *const c_void).unwrap_or(core::ptr::null())
        })
    }
}

gfn! {
    pub unsafe fn h_prop_get(name: *const c_char, value: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: a guest string.
            let n = unsafe { cstr(name) }.unwrap_or(b"");
            let v = find_prop(n).map(|p| p.value.lock().unwrap().clone()).unwrap_or_default();
            let l = v.len().min(91);
            // SAFETY: PROP_VALUE_MAX (92) bytes, as the API requires.
            unsafe {
                core::ptr::copy_nonoverlapping(v.as_ptr(), value, l);
                *value.add(l) = 0;
            }
            l as i32
        })
    }
}

gfn! {
    pub unsafe fn h_prop_set(name: *const c_char, value: *const c_char) -> i32 {
        guard(|| {
            // SAFETY: guest strings.
            let (Some(n), Some(v)) = (unsafe { cstr(name) }, unsafe { cstr(value) }) else { return -1 };
            // bionic's rules: a name is required; values are shorter than
            // PROP_VALUE_MAX (92) unless the property is read-only.
            if n.is_empty() || (v.len() >= 92 && !n.starts_with(b"ro.")) {
                return -1;
            }
            let mut g = PROPS.lock().unwrap();
            let map = g.get_or_insert_with(HashMap::new);
            let p = *map.entry(n.to_vec()).or_insert_with(|| {
                Box::leak(Box::new(Prop { name: n.to_vec(), value: Mutex::new(vec![]), serial: AtomicU32::new(0) }))
            });
            *p.value.lock().unwrap() = v.to_vec();
            p.serial.fetch_add(2, Ordering::Release);
            AREA_SERIAL.fetch_add(1, Ordering::Release);
            0
        })
    }
}

#[cfg(target_arch = "x86_64")]
type PropCb = extern "sysv64" fn(*mut c_void, *const c_char, *const c_char, u32);
#[cfg(not(target_arch = "x86_64"))]
type PropCb = extern "C" fn(*mut c_void, *const c_char, *const c_char, u32);

gfn! {
    pub unsafe fn h_prop_read_callback(pi: *const c_void, cb: PropCb, cookie: *mut c_void) -> () {
        guard(|| {
            // SAFETY: a prop_info from find.
            let p = unsafe { &*(pi as *const Prop) };
            let mut n = p.name.clone();
            n.push(0);
            let mut v = p.value.lock().unwrap().clone();
            v.push(0);
            cb(cookie, n.as_ptr() as *const c_char, v.as_ptr() as *const c_char, p.serial.load(Ordering::Acquire));
        })
    }
}

gfn! {
    pub unsafe fn h_prop_read(pi: *const c_void, name: *mut u8, value: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: a prop_info from find.
            let p = unsafe { &*(pi as *const Prop) };
            let v = p.value.lock().unwrap().clone();
            // SAFETY: PROP_NAME_MAX / PROP_VALUE_MAX buffers.
            unsafe {
                if !name.is_null() {
                    let l = p.name.len().min(31);
                    core::ptr::copy_nonoverlapping(p.name.as_ptr(), name, l);
                    *name.add(l) = 0;
                }
                let l = v.len().min(91);
                core::ptr::copy_nonoverlapping(v.as_ptr(), value, l);
                *value.add(l) = 0;
                l as i32
            }
        })
    }
}

gfn! {
    pub unsafe fn h_prop_serial(pi: *const c_void) -> u32 {
        // SAFETY: a prop_info from find.
        guard(|| unsafe { &*(pi as *const Prop) }.serial.load(Ordering::Acquire))
    }
}

gfn! {
    pub unsafe fn h_prop_area_serial() -> u32 {
        guard(|| AREA_SERIAL.load(Ordering::Acquire))
    }
}

gfn! {
    pub unsafe fn h_prop_wait(pi: *const c_void, old: u32, new: *mut u32, timeout: *const [i64; 2]) -> bool {
        guard(|| {
            let deadline = if timeout.is_null() {
                None
            } else {
                // SAFETY: the guest's relative timespec.
                let [s, ns] = unsafe { *timeout };
                Some(std::time::Instant::now() + std::time::Duration::new(s.max(0) as u64, ns.clamp(0, 999_999_999) as u32))
            };
            loop {
                let cur = if pi.is_null() {
                    AREA_SERIAL.load(Ordering::Acquire)
                } else {
                    // SAFETY: a prop_info from find.
                    unsafe { &*(pi as *const Prop) }.serial.load(Ordering::Acquire)
                };
                if cur != old {
                    // SAFETY: the guest's out parameter.
                    unsafe { *new = cur };
                    return true;
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    }
}

// --- fdsan (no ownership tracking in the spike) --------------------------------

gfn! {
    pub unsafe fn h_fdsan_create_owner_tag(ty: u32, tag: u64) -> u64 {
        guard(|| ((ty as u64) << 56) | (tag & ((1 << 56) - 1)))
    }
}

gfn! {
    pub unsafe fn h_fdsan_exchange_owner_tag(_fd: i32, _e: u64, _n: u64) -> () {
        guard(|| ())
    }
}

gfn! {
    pub unsafe fn h_fdsan_close_with_tag(fd: i32, _tag: u64) -> i32 {
        // SAFETY: forwarded.
        guard(|| unsafe { super::lx::h_close(fd) })
    }
}

// --- numbers ------------------------------------------------------------------

unsafe extern "C" {
    fn snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> i32;
    fn strtod(s: *const c_char, end: *mut *mut c_char) -> f64;
    fn strtof(s: *const c_char, end: *mut *mut c_char) -> f32;
    fn nextafter(a: f64, b: f64) -> f64;
}

gfn! {
    /// One floating-point conversion through the Windows C runtime.
    pub unsafe fn h_fmt_fp(buf: *mut c_char, cap: usize, spec: *const c_char, v: f64) -> i32 {
        // SAFETY: the guest's buffer and a conversion spec built by the
        // guest libc (flags, width, precision and one of f F e E g G a A).
        guard(|| unsafe { snprintf(buf, cap, spec, v) })
    }
}

gfn! {
    pub unsafe fn h_strtod(s: *const c_char, end: *mut *mut c_char) -> f64 {
        // SAFETY: a guest string.
        guard(|| unsafe { strtod(s, end) })
    }
}

gfn! {
    pub unsafe fn h_strtof(s: *const c_char, end: *mut *mut c_char) -> f32 {
        // SAFETY: a guest string.
        guard(|| unsafe { strtof(s, end) })
    }
}

gfn! {
    pub unsafe fn h_nextafter(a: f64, b: f64) -> f64 {
        // SAFETY: a pure function.
        guard(|| unsafe { nextafter(a, b) })
    }
}

gfn! {
    pub unsafe fn h_arc4random_buf(p: *mut u8, n: usize) -> () {
        // SAFETY: the guest's buffer.
        guard(|| super::random_bytes(unsafe { core::slice::from_raw_parts_mut(p, n) }))
    }
}

gfn! {
    pub unsafe fn h_bench_nop() -> i32 {
        0
    }
}

// --- quiet no-ops and unsupported calls ------------------------------------------

gfn! {
    pub unsafe fn h_void() -> () {
        guard(|| ())
    }
}

gfn! {
    pub unsafe fn h_enosys() -> i32 {
        guard(|| fail_with(ENOSYS))
    }
}

gfn! {
    pub unsafe fn h_enotty() -> i32 {
        guard(|| fail_with(ENOTTY))
    }
}

gfn! {
    pub unsafe fn h_null() -> *mut c_void {
        guard(|| { set_errno(ENOSYS); core::ptr::null_mut() })
    }
}

gfn! {
    /// `getaddrinfo`: `EAI_FAIL` (no network in the spike).
    pub unsafe fn h_eai_fail() -> i32 {
        guard(|| 4)
    }
}

gfn! {
    pub unsafe fn h_gai_strerror(_e: i32) -> *const c_char {
        guard(|| c"Non-recoverable failure in name resolution".as_ptr())
    }
}

gfn! {
    pub unsafe fn h_sigaction(_sig: i32, _act: *const c_void, old: *mut u8) -> i32 {
        guard(|| {
            if !old.is_null() {
                // SAFETY: the guest's struct sigaction (32 bytes on LP64).
                unsafe { core::ptr::write_bytes(old, 0, 32) };
            }
            0
        })
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("malloc", h_malloc as usize),
        ("calloc", h_calloc as usize),
        ("realloc", h_realloc as usize),
        ("free", h_free as usize),
        ("aligned_alloc", h_aligned_alloc as usize),
        ("memalign", h_memalign as usize),
        ("posix_memalign", h_posix_memalign as usize),
        ("malloc_usable_size", h_malloc_usable_size as usize),
        ("_exit", h_exit as usize),
        ("abort", h_abort as usize),
        (
            "android_set_abort_message",
            h_android_set_abort_message as usize,
        ),
        ("getuid", h_getuid as usize),
        ("geteuid", h_getuid as usize),
        ("getgid", h_getgid as usize),
        ("getegid", h_getgid as usize),
        ("getgroups", h_getgroups as usize),
        ("setuid", h_set_id as usize),
        ("setgid", h_set_id as usize),
        ("kill", h_kill as usize),
        ("sysconf", h_sysconf as usize),
        ("getpagesize", h_getpagesize as usize),
        ("getauxval", h_getauxval as usize),
        ("__system_property_find", h_prop_find as usize),
        ("__system_property_get", h_prop_get as usize),
        ("__system_property_set", h_prop_set as usize),
        (
            "__system_property_read_callback",
            h_prop_read_callback as usize,
        ),
        ("__system_property_read", h_prop_read as usize),
        ("__system_property_serial", h_prop_serial as usize),
        ("__system_property_area_serial", h_prop_area_serial as usize),
        ("__system_property_wait", h_prop_wait as usize),
        (
            "android_fdsan_create_owner_tag",
            h_fdsan_create_owner_tag as usize,
        ),
        (
            "android_fdsan_exchange_owner_tag",
            h_fdsan_exchange_owner_tag as usize,
        ),
        (
            "android_fdsan_close_with_tag",
            h_fdsan_close_with_tag as usize,
        ),
        ("__andruna_fmt_fp", h_fmt_fp as usize),
        ("strtod", h_strtod as usize),
        ("strtof", h_strtof as usize),
        ("nextafter", h_nextafter as usize),
        ("arc4random_buf", h_arc4random_buf as usize),
        ("__andruna_bench_nop", h_bench_nop as usize),
        ("sigaction", h_sigaction as usize),
        ("ioctl", h_enotty as usize),
        ("openlog", h_void as usize),
        ("syslog", h_void as usize),
        ("closelog", h_void as usize),
        ("getaddrinfo", h_eai_fail as usize),
        ("freeaddrinfo", h_void as usize),
        ("gai_strerror", h_gai_strerror as usize),
        ("popen", h_null as usize),
        // Not supported in the spike: processes, pipes, sockets, polling.
        ("pclose", h_enosys as usize),
        ("fork", h_enosys as usize),
        ("clone", h_enosys as usize),
        ("execv", h_enosys as usize),
        ("waitpid", h_enosys as usize),
        ("pipe", h_enosys as usize),
        ("poll", h_enosys as usize),
        ("socket", h_enosys as usize),
        ("socketpair", h_enosys as usize),
        ("connect", h_enosys as usize),
        ("setsockopt", h_enosys as usize),
        ("recv", h_enosys as usize),
        ("recvmsg", h_enosys as usize),
        ("sendmsg", h_enosys as usize),
        ("sendfile", h_enosys as usize),
        ("ftruncate", h_enosys as usize),
        ("symlink", h_enosys as usize),
        ("link", h_enosys as usize),
        ("malloc_info", h_enosys as usize),
    ]
}

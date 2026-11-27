//! The host side of the guest libc: functions guest code imports by name,
//! entered with the System V convention (x64) and forwarded to andruna-lx
//! where lx has the call.
//!
//! The list of names is `guest/libc/host_symbols.txt`; the guest objects
//! are linked against a stub library generated from it
//! (`libandruna_host.so`, as the NDK links against stub libraries), and the
//! loader binds those names to this table.
//!
//! Every function here is `extern "sysv64"` and runs inside [`guard`], so
//! no panic crosses into guest code; a panic is reported as `EIO`.

use std::collections::HashMap;

/// Defines a function that guest code calls: System V on x64 (the Android
/// convention), the platform C convention on arm64 (AAPCS64, which Windows
/// ARM64 shares for fixed arguments).
#[macro_export]
macro_rules! gfn {
    ($(#[$m:meta])* $v:vis unsafe fn $name:ident($($a:ident : $t:ty),* $(,)?) -> $r:ty $body:block) => {
        $(#[$m])* #[cfg(target_arch = "x86_64")]
        $v unsafe extern "sysv64" fn $name($($a: $t),*) -> $r $body
        $(#[$m])* #[cfg(not(target_arch = "x86_64"))]
        $v unsafe extern "C" fn $name($($a: $t),*) -> $r $body
    };
}
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

pub mod dl;
pub mod extra;
pub mod fs;
pub mod lx;
pub mod misc;
pub mod signal;
pub mod threads;

/// The guest-visible soname of the host library.
pub const HOST_LIBRARY: &str = "libandruna_host.so";

pub fn is_host_library(name: &str) -> bool {
    name == HOST_LIBRARY
}

/// The failure value of a host function's return type.
pub trait Fail {
    fn fail() -> Self;
}

macro_rules! fail_minus_one {
    ($($t:ty),*) => { $(impl Fail for $t { fn fail() -> Self { -1 as $t } })* };
}
fail_minus_one!(i32, i64, isize);

impl Fail for usize {
    fn fail() -> Self {
        usize::MAX
    }
}
impl Fail for u64 {
    fn fail() -> Self {
        0
    }
}
impl Fail for u32 {
    fn fail() -> Self {
        0
    }
}
impl Fail for f64 {
    fn fail() -> Self {
        0.0
    }
}
impl Fail for f32 {
    fn fail() -> Self {
        0.0
    }
}
impl Fail for bool {
    fn fail() -> Self {
        false
    }
}
impl Fail for () {
    fn fail() -> Self {}
}
impl<T> Fail for *mut T {
    fn fail() -> Self {
        core::ptr::null_mut()
    }
}
impl<T> Fail for *const T {
    fn fail() -> Self {
        core::ptr::null()
    }
}

/// Runs a host function body; a panic becomes `EIO` and the failure value.
pub fn guard<T: Fail>(f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            set_errno(andruna_lx::linux::errno::EIO);
            T::fail()
        }
    }
}

pub fn set_errno(e: i32) {
    andruna_lx::errno::set_errno(andruna_lx::errno::Errno(e));
}

/// Sets `errno` and returns -1, the usual failure of a libc call.
pub fn fail_with(e: i32) -> i32 {
    set_errno(e);
    -1
}

/// Borrows a C string argument.
///
/// # Safety
///
/// `p` is NULL or a NUL-terminated string valid for the call.
pub unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a [u8]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(p) }.to_bytes())
    }
}

/// The host symbol table.
pub fn table() -> HashMap<&'static str, usize> {
    let mut t = HashMap::new();
    for (n, a) in lx::symbols()
        .into_iter()
        .chain(fs::symbols())
        .chain(threads::symbols())
        .chain(misc::symbols())
        .chain(dl::symbols())
        .chain(signal::symbols())
        .chain(extra::symbols())
    {
        let old = t.insert(n, a);
        assert!(old.is_none(), "host symbol {n} defined twice");
    }
    t
}

/// Names the guest objects were linked against (guest/libc/host_symbols.txt).
pub const DECLARED: &str = include_str!("../../guest/libc/host_symbols.txt");

/// Names of `host_symbols.txt` that the table does not define.
pub fn missing_from_table() -> Vec<&'static str> {
    let t = table();
    DECLARED
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|n| !t.contains_key(n))
        .collect()
}

static STUBS: Mutex<Vec<(CString, usize)>> = Mutex::new(Vec::new());
static TLSDESC: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// The TLSDESC function for static TLS: returns the descriptor's second
/// word (x64: `mov rax, [rax+8]; ret`; arm64: `ldr x0, [x0, #8]; ret`).
pub fn tlsdesc_static_resolver() -> usize {
    *TLSDESC.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        let code: &[u8] = &[0x48, 0x8b, 0x40, 0x08, 0xc3];
        #[cfg(target_arch = "aarch64")]
        let code: &[u8] = &[0x00, 0x04, 0x40, 0xf9, 0xc0, 0x03, 0x5f, 0xd6];
        code_stub(code)
    })
}

/// Copies `code` into the executable stub page.
fn code_stub(code: &[u8]) -> usize {
    let _g = STUBS.lock().unwrap();
    // SAFETY: the stub page is only touched under the STUBS lock.
    let (page, used) = unsafe { STUB_PAGE };
    let (page, used) = if page == 0 || used + 32 > 65536 {
        (new_stub_page(), 0)
    } else {
        (page, used)
    };
    // SAFETY: inside the RWX stub page, unused so far.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), (page + used) as *mut u8, code.len()) };
    // SAFETY: as above.
    unsafe { STUB_PAGE = (page, used + 32) };
    page + used
}

fn new_stub_page() -> usize {
    andruna_lx::mem::mmap(
        0,
        65536,
        andruna_lx::linux::mman::PROT_READ
            | andruna_lx::linux::mman::PROT_WRITE
            | andruna_lx::linux::mman::PROT_EXEC,
        andruna_lx::linux::mman::MAP_PRIVATE | andruna_lx::linux::mman::MAP_ANONYMOUS,
        -1,
        0,
    )
    .expect("stub page")
}
static mut STUB_PAGE: (usize, usize) = (0, 0);

/// An executable stub for a symbol nothing defines: it reports the call
/// and fails with `ENOSYS` (instead of failing the load), so a test that
/// never calls the function still runs.
pub fn unresolved_stub(name: &str) -> usize {
    let mut stubs = STUBS.lock().unwrap();
    let c = CString::new(name).unwrap();
    let np = c.as_ptr() as u64;
    // SAFETY: the stub page is only touched under the STUBS lock.
    let (page, used) = unsafe { STUB_PAGE };
    let (page, used) = if page == 0 || used + 32 > 65536 {
        let p = andruna_lx::mem::mmap(
            0,
            65536,
            andruna_lx::linux::mman::PROT_READ
                | andruna_lx::linux::mman::PROT_WRITE
                | andruna_lx::linux::mman::PROT_EXEC,
            andruna_lx::linux::mman::MAP_PRIVATE | andruna_lx::linux::mman::MAP_ANONYMOUS,
            -1,
            0,
        )
        .expect("stub page");
        (p, 0)
    } else {
        (page, used)
    };
    let at = page + used;
    let h = unresolved_called as usize as u64;
    let mut code = Vec::with_capacity(32);
    #[cfg(target_arch = "x86_64")]
    {
        code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, name
        code.extend_from_slice(&np.to_le_bytes());
        code.extend_from_slice(&[0x48, 0xb8]); // movabs rax, handler
        code.extend_from_slice(&h.to_le_bytes());
        code.extend_from_slice(&[0xff, 0xe0]); // jmp rax
    }
    #[cfg(target_arch = "aarch64")]
    {
        // ldr x0, +16; ldr x16, +20; br x16; nop; .quad name; .quad handler
        for w in [0x5800_0080u32, 0x5800_00b0, 0xd61f_0200, 0xd503_201f] {
            code.extend_from_slice(&w.to_le_bytes());
        }
        code.extend_from_slice(&np.to_le_bytes());
        code.extend_from_slice(&h.to_le_bytes());
    }
    // SAFETY: `at..at+len` lies in the RWX stub page, unused so far.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
    // SAFETY: as above.
    unsafe { STUB_PAGE = (page, used + 32) };
    stubs.push((c, at));
    at
}

gfn! {
    unsafe fn unresolved_called(name: *const c_char) -> i64 {
        guard(|| {
            // SAFETY: the stub passes its own NUL-terminated name.
            let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            eprintln!("[s8] unresolved guest import called: {n} (ENOSYS)");
            set_errno(andruna_lx::linux::errno::ENOSYS);
            -1
        })
    }
}

/// Random bytes from the system generator.
pub fn random_bytes(buf: &mut [u8]) {
    use windows::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // SAFETY: a valid buffer; no algorithm handle with the preferred-RNG flag.
    let st = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(st.is_ok(), "BCryptGenRandom failed");
}

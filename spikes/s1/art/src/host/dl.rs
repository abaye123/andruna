//! `dlopen` and friends for guest code (bionic's libdl API), over the
//! spike's single global linker.
//!
//! The runner preloads every object ART will open (libart.so, the libcore
//! natives) before any guest code runs, so static TLS is laid out once.
//! `dlopen` of a loaded object returns its handle; `dlopen` of anything
//! else loads it then, which works for objects without `PT_TLS` (spike
//! limit: no dynamic TLS for late objects). No namespaces: the
//! `android_*namespace*` calls report that no namespace exists.

use core::ffi::{c_char, c_void};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;

use super::{cstr, guard};
use crate::loader::Linker;

struct Global(*mut Linker);
// SAFETY: the linker is only touched under the LINKER mutex.
unsafe impl Send for Global {}

static LINKER: Mutex<Option<Global>> = Mutex::new(None);
static NAMES: Mutex<Vec<CString>> = Mutex::new(Vec::new());

thread_local! {
    static ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Makes `l` the linker that guest `dlopen` calls use. `l` must outlive all
/// guest code.
pub fn set_linker(l: &mut Linker) {
    *LINKER.lock().unwrap() = Some(Global(l));
}

fn with_linker<T>(f: impl FnOnce(&mut Linker) -> T) -> Option<T> {
    let g = LINKER.lock().unwrap();
    // SAFETY: set_linker's contract; serialized by the mutex.
    g.as_ref().map(|p| f(unsafe { &mut *p.0 }))
}

fn set_error(msg: String) {
    ERROR.with(|e| *e.borrow_mut() = Some(CString::new(msg).unwrap_or_default()));
}

fn handle(i: usize) -> *mut c_void {
    (i + 1) as *mut c_void
}

/// A C string that lives as long as the process (names handed to guests).
fn leak_name(s: &str) -> *const c_char {
    let c = CString::new(s).unwrap_or_default();
    let p = c.as_ptr();
    NAMES.lock().unwrap().push(c);
    p
}

const RTLD_NEXT: usize = usize::MAX;

gfn! {
    pub unsafe fn h_dlopen(name: *const c_char, _flags: i32) -> *mut c_void {
        guard(|| {
            // SAFETY: a guest C string or NULL.
            let Some(n) = (unsafe { cstr(name) }) else {
                return handle(0);
            };
            let n = String::from_utf8_lossy(n).into_owned();
            let base = n.rsplit('/').next().unwrap_or(&n).to_string();
            let r = with_linker(|l| {
                if let Some(i) = l.objects.iter().position(|o| o.name == base) {
                    return Ok(i);
                }
                let p = l.find_library(&base)?;
                let first = l.objects.len();
                let i = l.load(&p)?;
                l.run_initializers_from(first);
                Ok::<usize, String>(i)
            });
            match r {
                Some(Ok(i)) => handle(i),
                Some(Err(e)) => {
                    set_error(format!("dlopen failed: {n}: {e}"));
                    core::ptr::null_mut()
                }
                None => {
                    set_error("dlopen: no linker".into());
                    core::ptr::null_mut()
                }
            }
        })
    }
}

gfn! {
    pub unsafe fn h_android_dlopen_ext(name: *const c_char, flags: i32, _info: *const c_void) -> *mut c_void {
        // SAFETY: forwarded.
        unsafe { h_dlopen(name, flags) }
    }
}

gfn! {
    pub unsafe fn h_dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void {
        guard(|| {
            // SAFETY: a guest C string.
            let Some(n) = (unsafe { cstr(name) }) else {
                return core::ptr::null_mut();
            };
            let n = String::from_utf8_lossy(n).into_owned();
            let r = with_linker(|l| {
                let h = h as usize;
                if h == RTLD_NEXT {
                    // libsigchain asks for the "next" sigaction: the host's.
                    return l.lookup_host(&n).or_else(|| l.lookup_global(&n));
                }
                if h != 0 && h - 1 < l.objects.len() {
                    if let Some(a) = l.lookup_in(h - 1, &n) {
                        return Some(a);
                    }
                    // The guest libc's functions that the host implements
                    // (libsigchain looks up the real sigaction in libc.so).
                    if l.objects[h - 1].name == "libc.so"
                        && let Some(a) = l.lookup_host(&n) {
                            return Some(a);
                        }
                }
                l.lookup_global(&n).or_else(|| l.lookup_host(&n))
            })
            .flatten();
            match r {
                Some(a) => a as *mut c_void,
                None => {
                    set_error(format!("undefined symbol: {n}"));
                    core::ptr::null_mut()
                }
            }
        })
    }
}

gfn! {
    pub unsafe fn h_dlvsym(h: *mut c_void, name: *const c_char, _v: *const c_char) -> *mut c_void {
        // SAFETY: forwarded.
        unsafe { h_dlsym(h, name) }
    }
}

gfn! {
    pub unsafe fn h_dlclose(_h: *mut c_void) -> i32 {
        // Objects are never unloaded.
        0
    }
}

gfn! {
    pub unsafe fn h_dlerror() -> *const c_char {
        guard(|| {
            ERROR.with(|e| match e.borrow_mut().take() {
                // Kept alive until the next dlerror on this thread.
                Some(c) => {

                    leak_name(c.to_str().unwrap_or(""))
                }
                None => core::ptr::null(),
            })
        })
    }
}

#[repr(C)]
pub struct DlInfo {
    fname: *const c_char,
    fbase: *mut c_void,
    sname: *const c_char,
    saddr: *mut c_void,
}

static SYM_NAMES: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);

gfn! {
    pub unsafe fn h_dladdr(addr: *const c_void, info: *mut DlInfo) -> i32 {
        guard(|| {
            let a = addr as u64;
            let r = with_linker(|l| {
                let o = l.objects.iter().find(|o| a >= o.base && a < o.base + o.size)?;
                let sym = o
                    .local_symbols
                    .iter()
                    .rev()
                    .find(|(s, len, _)| *s <= a - o.bias && (a - o.bias) < s + (*len).max(1));
                Some((o.name.clone(), o.base, sym.map(|(s, _, n)| (o.bias + s, n.clone()))))
            })
            .flatten();
            let Some((name, base, sym)) = r else { return 0 };
            let mut cache = SYM_NAMES.lock().unwrap();
            let cache = cache.get_or_insert_with(HashMap::new);
            let fname = *cache.entry(base).or_insert_with(|| leak_name(&name) as usize);
            // SAFETY: the guest's Dl_info.
            unsafe {
                (*info).fname = fname as *const c_char;
                (*info).fbase = base as *mut c_void;
                match sym {
                    Some((s, n)) => {
                        let p = *cache.entry(s).or_insert_with(|| leak_name(&n) as usize);
                        (*info).sname = p as *const c_char;
                        (*info).saddr = s as *mut c_void;
                    }
                    None => {
                        (*info).sname = core::ptr::null();
                        (*info).saddr = core::ptr::null_mut();
                    }
                }
            }
            1
        })
    }
}

/// bionic's `struct dl_phdr_info`.
#[repr(C)]
pub struct DlPhdrInfo {
    addr: u64,
    name: *const c_char,
    phdr: *const crate::elf::Phdr,
    phnum: u16,
    adds: u64,
    subs: u64,
    tls_modid: usize,
    tls_data: *mut c_void,
}

#[cfg(target_arch = "x86_64")]
type PhdrCb = extern "sysv64" fn(*mut DlPhdrInfo, usize, *mut c_void) -> i32;
#[cfg(not(target_arch = "x86_64"))]
type PhdrCb = extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> i32;

gfn! {
    pub unsafe fn h_dl_iterate_phdr(cb: PhdrCb, data: *mut c_void) -> i32 {
        guard(|| {
            // Collected first: the callback may call dladdr or dlsym.
            let infos: Vec<(u64, String, *const crate::elf::Phdr, u16, usize)> = with_linker(|l| {
                l.objects
                    .iter()
                    .map(|o| (o.bias, o.name.clone(), o.phdrs.as_ptr(), o.phdrs.len() as u16, o.tls_module))
                    .collect()
            })
            .unwrap_or_default();
            let n = infos.len() as u64;
            for (bias, name, phdr, phnum, tls) in infos {
                let mut i = DlPhdrInfo {
                    addr: bias,
                    name: leak_name(&name),
                    phdr,
                    phnum,
                    adds: n,
                    subs: 0,
                    tls_modid: tls,
                    tls_data: core::ptr::null_mut(),
                };
                let r = cb(&mut i, core::mem::size_of::<DlPhdrInfo>(), data);
                if r != 0 {
                    return r;
                }
            }
            0
        })
    }
}

/// The one linker namespace the runner pretends to have. ART asks for the
/// "system" namespace before it dlopens an oat file and aborts without
/// one; the dlopen of an oat file then fails (oat files are not in the
/// library path), and ART loads the oat file with its own ELF loader, as
/// on a host build.
static SYSTEM_NAMESPACE: u64 = 0;

gfn! {
    pub unsafe fn h_exported_namespace(_name: *const c_char) -> *mut c_void {
        &SYSTEM_NAMESPACE as *const u64 as *mut c_void
    }
}

gfn! {
    pub unsafe fn h_null_namespace() -> *mut c_void {
        core::ptr::null_mut()
    }
}

gfn! {
    pub unsafe fn h_false() -> bool {
        false
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("dlopen", h_dlopen as usize),
        ("android_dlopen_ext", h_android_dlopen_ext as usize),
        ("dlsym", h_dlsym as usize),
        ("dlvsym", h_dlvsym as usize),
        ("dlclose", h_dlclose as usize),
        ("dlerror", h_dlerror as usize),
        ("dladdr", h_dladdr as usize),
        ("dl_iterate_phdr", h_dl_iterate_phdr as usize),
        (
            "android_get_exported_namespace",
            h_exported_namespace as usize,
        ),
        ("android_create_namespace", h_null_namespace as usize),
        ("android_link_namespaces", h_false as usize),
        ("android_init_anonymous_namespace", h_false as usize),
        // libnativeloader's namespace queries (its Android code is not
        // built, see aosp/modules/art_elf.cmake): no namespace, not bridged.
        (
            "FindNativeLoaderNamespaceByClassLoader",
            h_null_namespace as usize,
        ),
        ("IsNamespaceNativeBridged", h_false as usize),
    ]
}

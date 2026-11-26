//! The `dlopen` family for guest code: the `__loader_*` functions that
//! bionic's `libdl.so` (and `libc.so`) call, on the process's linker.
//!
//! The linker lock is held while objects are mapped, linked and looked up,
//! and released while guest initializers and finalizers run (they may call
//! `dlopen` themselves).
//!
//! Not supported yet, reported as failures with a `dlerror` message:
//! linker namespaces (`android_create_namespace` and friends) and
//! `android_dlopen_ext` options.

use core::ffi::{CStr, c_char, c_void};
use std::cell::RefCell;
use std::ffi::CString;
use std::sync::{Mutex, MutexGuard};

use crate::linker::{Linker, rtld, run_fini_fns, run_init_fns};
use crate::object::ObjectId;

/// The process's linker.
static LINKER: Mutex<Option<Linker>> = Mutex::new(None);

pub fn lock() -> MutexGuard<'static, Option<Linker>> {
    LINKER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Makes `l` the process's linker (the one guest `dlopen` uses).
pub fn install(l: Linker) {
    *lock() = Some(l);
}

/// Runs `f` on the process's linker.
pub fn with<R>(f: impl FnOnce(&mut Linker) -> R) -> Option<R> {
    lock().as_mut().map(f)
}

const RTLD_DEFAULT: u64 = 0;
const RTLD_NEXT: u64 = u64::MAX;

static TARGET_SDK: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(10000);
static ADDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SUBS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn handle_of(id: ObjectId) -> u64 {
    ((id as u64) << 1) | 1
}

fn id_of(h: u64) -> Option<ObjectId> {
    (h & 1 == 1 && h != RTLD_NEXT).then_some((h >> 1) as ObjectId)
}

thread_local! {
    /// The pending `dlerror` message and the one last returned (kept alive
    /// until the next call, as bionic's per-thread buffer).
    static DLERROR: RefCell<(Option<CString>, Option<CString>)> = const { RefCell::new((None, None)) };
}

fn set_error(msg: impl Into<String>) {
    let c = CString::new(msg.into().replace('\0', "")).unwrap_or_default();
    DLERROR.with(|e| e.borrow_mut().0 = Some(c));
}

/// Loads `name` for guest code; runs its initializers.
pub fn dlopen(name: Option<&str>, flags: i32, caller: u64) -> Result<u64, String> {
    let (root, fns) = {
        let mut g = lock();
        let l = g.as_mut().ok_or("no linker in this process")?;
        let Some(name) = name else {
            let main = l
                .objects()
                .find(|o| o.is_main)
                .map(|o| o.id)
                .ok_or("no main executable")?;
            return Ok(handle_of(main));
        };
        let by = l.object_at(caller).map(|o| o.id);
        let loaded = l.load(name, by, flags).map_err(|e| e.to_string())?;
        let fns = l.init_fns(&loaded.init_order).map_err(|e| e.to_string())?;
        if !loaded.init_order.is_empty() {
            ADDS.fetch_add(loaded.init_order.len() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        (loaded.root, fns)
    };
    let (argc, argv, envp) = crate::process::args();
    run_init_fns(&fns, argc, argv, envp);
    Ok(handle_of(root))
}

/// Drops a `dlopen` reference; runs finalizers and unmaps unused objects.
pub fn dlclose(h: u64) -> Result<(), String> {
    let id = id_of(h).ok_or("invalid handle")?;
    let (fns, dead) = {
        let mut g = lock();
        let l = g.as_mut().ok_or("no linker")?;
        if l.get(id).is_none() {
            return Err(format!("invalid handle: {h:#x}"));
        }
        l.release(id).map_err(|e| e.to_string())?
    };
    run_fini_fns(&fns);
    if !dead.is_empty() {
        SUBS.fetch_add(dead.len() as u64, std::sync::atomic::Ordering::Relaxed);
        with(|l| l.unmap(&dead));
    }
    Ok(())
}

/// `dlsym`/`dlvsym`.
pub fn dlsym(h: u64, name: &str, version: Option<&str>, caller: u64) -> Result<u64, String> {
    let g = lock();
    let l = g.as_ref().ok_or("no linker")?;
    let caller_obj = l.object_at(caller).map(|o| o.id);
    let found = match h {
        RTLD_DEFAULT => {
            let mut scope = l.global_scope();
            if let Some(c) = caller_obj {
                for o in l.local_group(l.get(c).map(|o| o.group_root).unwrap_or(c)) {
                    if !scope.contains(&o) {
                        scope.push(o);
                    }
                }
            }
            l.dlsym_in(&scope, name, version, true)
        }
        RTLD_NEXT => {
            let c = caller_obj.ok_or("RTLD_NEXT used in code not loaded by the linker")?;
            let scope = l.global_scope();
            let after: Vec<_> = match scope.iter().position(|&x| x == c) {
                Some(i) => scope[i + 1..].to_vec(),
                None => vec![],
            };
            l.dlsym_in(&after, name, version, false)
        }
        h => {
            let id = id_of(h).filter(|&i| l.get(i).is_some()).ok_or_else(|| format!("invalid handle: {h:#x}"))?;
            l.dlsym_in(&l.local_group(id), name, version, false)
        }
    };
    found.ok_or_else(|| match version {
        Some(v) => format!("undefined symbol: {name}, version {v}"),
        None => format!("undefined symbol: {name}"),
    })
}

/// bionic's `Dl_info`.
#[repr(C)]
pub struct DlInfo {
    pub dli_fname: *const c_char,
    pub dli_fbase: *mut c_void,
    pub dli_sname: *const c_char,
    pub dli_saddr: *mut c_void,
}

/// `struct dl_phdr_info` (LP64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DlPhdrInfo {
    pub dlpi_addr: u64,
    pub dlpi_name: *const c_char,
    pub dlpi_phdr: *const c_void,
    pub dlpi_phnum: u16,
    pub dlpi_adds: u64,
    pub dlpi_subs: u64,
    pub dlpi_tls_modid: usize,
    pub dlpi_tls_data: *mut c_void,
}

const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<DlInfo>() == 32);
    assert!(size_of::<DlPhdrInfo>() == 64);
    assert!(offset_of!(DlPhdrInfo, dlpi_phnum) == 24);
    assert!(offset_of!(DlPhdrInfo, dlpi_adds) == 32);
    assert!(offset_of!(DlPhdrInfo, dlpi_tls_modid) == 48);
    assert!(offset_of!(DlPhdrInfo, dlpi_tls_data) == 56);
};

pub fn dladdr(addr: u64) -> Option<DlInfo> {
    let g = lock();
    let l = g.as_ref()?;
    let o = l.object_at(addr)?;
    let mut info = DlInfo {
        dli_fname: o.path_c.as_ptr(),
        dli_fbase: o.image.base as *mut c_void,
        dli_sname: core::ptr::null(),
        dli_saddr: core::ptr::null_mut(),
    };
    if let Some((s, _)) = o.symbol_at(addr) {
        // The name in the mapped DT_STRTAB is NUL-terminated.
        info.dli_sname = o.addr(o.dynamic.strtab + u64::from(s.st_name)) as *const c_char;
        info.dli_saddr = o.addr(s.st_value) as *mut c_void;
    }
    Some(info)
}

/// A snapshot of the loaded objects for `dl_iterate_phdr`.
pub fn phdr_infos() -> Vec<DlPhdrInfo> {
    let g = lock();
    let Some(l) = g.as_ref() else { return vec![] };
    let adds = ADDS.load(std::sync::atomic::Ordering::Relaxed) + l.objects().count() as u64;
    let subs = SUBS.load(std::sync::atomic::Ordering::Relaxed);
    l.objects()
        .map(|o| {
            let (modid, data) = match o.tls {
                Some(t) if t.module != 0 => (
                    t.module,
                    t.tp_offset
                        .map(|off| crate::tls::current().wrapping_add_signed(off) as *mut c_void)
                        .unwrap_or(core::ptr::null_mut()),
                ),
                _ => (0, core::ptr::null_mut()),
            };
            DlPhdrInfo {
                dlpi_addr: o.image.bias,
                dlpi_name: o.path_c.as_ptr(),
                dlpi_phdr: o.phdr_addr as *const c_void,
                dlpi_phnum: o.headers.ehdr.e_phnum,
                dlpi_adds: adds,
                dlpi_subs: subs,
                dlpi_tls_modid: modid,
                dlpi_tls_data: data,
            }
        })
        .collect()
}

/// # Safety
///
/// `p` is NULL or a NUL-terminated string valid for the call.
unsafe fn str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

type PhdrCallback = gty!(fn(*mut DlPhdrInfo, usize, *mut c_void) -> i32);
type WarningCallback = gty!(fn(*mut c_void, *const c_char));

gfn! {
    unsafe fn loader_dlopen(name: *const c_char, flags: i32, caller: *const c_void) -> *mut c_void {
        crate::host::guard(core::ptr::null_mut(), || {
            // SAFETY: guest string argument.
            let n = unsafe { str_arg(name) };
            if !name.is_null() && n.is_none() {
                set_error("dlopen failed: file name is not UTF-8");
                return core::ptr::null_mut();
            }
            match dlopen(n, flags, caller as u64) {
                Ok(h) => h as *mut c_void,
                Err(e) => {
                    set_error(format!("dlopen failed: {e}"));
                    core::ptr::null_mut()
                }
            }
        })
    }
}

gfn! {
    unsafe fn loader_android_dlopen_ext(
        name: *const c_char,
        flags: i32,
        ext: *const u64,
        caller: *const c_void,
    ) -> *mut c_void {
        crate::host::guard(core::ptr::null_mut(), || {
            // SAFETY: a non-NULL extinfo starts with its 64-bit flags.
            let ext_flags = if ext.is_null() { 0 } else { unsafe { ext.read_unaligned() } };
            if ext_flags != 0 {
                set_error(format!(
                    "android_dlopen_ext failed: extinfo flags {ext_flags:#x} are not supported yet"
                ));
                return core::ptr::null_mut();
            }
            // SAFETY: forwarded guest arguments.
            unsafe { loader_dlopen(name, flags, caller) }
        })
    }
}

gfn! {
    unsafe fn loader_dlerror() -> *mut c_char {
        crate::host::guard(core::ptr::null_mut(), || {
            DLERROR.with(|e| {
                let mut e = e.borrow_mut();
                let cur = e.0.take();
                e.1 = cur;
                e.1.as_ref().map_or(core::ptr::null_mut(), |c| c.as_ptr() as *mut c_char)
            })
        })
    }
}

gfn! {
    unsafe fn loader_dlsym(h: *mut c_void, name: *const c_char, caller: *const c_void) -> *mut c_void {
        // SAFETY: forwarded guest arguments.
        unsafe { loader_dlvsym(h, name, core::ptr::null(), caller) }
    }
}

gfn! {
    unsafe fn loader_dlvsym(
        h: *mut c_void,
        name: *const c_char,
        version: *const c_char,
        caller: *const c_void,
    ) -> *mut c_void {
        crate::host::guard(core::ptr::null_mut(), || {
            // SAFETY: guest string arguments.
            let (Some(n), v) = (unsafe { str_arg(name) }, unsafe { str_arg(version) }) else {
                set_error("dlsym failed: symbol name is null");
                return core::ptr::null_mut();
            };
            match dlsym(h as u64, n, v, caller as u64) {
                Ok(a) => a as *mut c_void,
                Err(e) => {
                    set_error(e);
                    core::ptr::null_mut()
                }
            }
        })
    }
}

gfn! {
    unsafe fn loader_dlclose(h: *mut c_void) -> i32 {
        crate::host::guard(-1, || match dlclose(h as u64) {
            Ok(()) => 0,
            Err(e) => {
                set_error(e);
                -1
            }
        })
    }
}

gfn! {
    unsafe fn loader_dladdr(addr: *const c_void, info: *mut DlInfo) -> i32 {
        crate::host::guard(0, || match dladdr(addr as u64) {
            Some(i) if !info.is_null() => {
                // SAFETY: the caller's Dl_info.
                unsafe { info.write(i) };
                1
            }
            _ => 0,
        })
    }
}

gfn! {
    unsafe fn loader_dl_iterate_phdr(cb: Option<PhdrCallback>, data: *mut c_void) -> i32 {
        crate::host::guard(0, || {
            let Some(cb) = cb else { return 0 };
            for mut info in phdr_infos() {
                let r = cb(&mut info, core::mem::size_of::<DlPhdrInfo>(), data);
                if r != 0 {
                    return r;
                }
            }
            0
        })
    }
}

gfn! {
    unsafe fn loader_get_ld_library_path(buf: *mut c_char, size: usize) -> () {
        crate::host::guard((), || {
            let s = with(|l| {
                l.config
                    .ld_library_path
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .unwrap_or_default();
            if buf.is_null() || size == 0 {
                return;
            }
            let n = s.len().min(size - 1);
            // SAFETY: the caller's buffer of `size` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
                *buf.add(n) = 0;
            }
        })
    }
}

gfn! {
    unsafe fn loader_update_ld_library_path(path: *const c_char) -> () {
        crate::host::guard((), || {
            // SAFETY: guest string argument.
            let p = unsafe { str_arg(path) }.unwrap_or("");
            with(|l| {
                l.config.ld_library_path =
                    p.split(':').filter(|s| !s.is_empty()).map(Into::into).collect()
            });
        })
    }
}

gfn! {
    unsafe fn loader_get_target_sdk() -> i32 {
        TARGET_SDK.load(std::sync::atomic::Ordering::Relaxed)
    }
}

gfn! {
    unsafe fn loader_set_target_sdk(v: i32) -> () {
        TARGET_SDK.store(v, std::sync::atomic::Ordering::Relaxed)
    }
}

gfn! {
    unsafe fn loader_namespace_unsupported() -> *mut c_void {
        set_error("linker namespaces are not supported yet");
        core::ptr::null_mut()
    }
}

gfn! {
    unsafe fn loader_namespace_unsupported_bool() -> bool {
        set_error("linker namespaces are not supported yet");
        false
    }
}

gfn! {
    unsafe fn loader_dlwarning(_obj: *mut c_void, _f: Option<WarningCallback>) -> () {
        // The linker records no warnings.
    }
}

gfn! {
    unsafe fn loader_handle_signal(_sig: i32, _info: *mut c_void, _ctx: *mut c_void) -> bool {
        false
    }
}

gfn! {
    unsafe fn loader_set_16kb_appcompat(_on: bool) -> () {
        // Pages are 4 KiB on both architectures; nothing to emulate.
    }
}

gfn! {
    unsafe fn loader_cfi_fail(_id: u64, ptr: *mut c_void, _diag: *mut c_void, pc: *mut c_void) -> () {
        crate::debug::report(format_args!(
            "CFI check failed: call to {ptr:p} from {pc:p} (CFI shadow is not supported)"
        ));
        crate::process::exit_process(134)
    }
}

fn dso_refcount(dso: u64, delta: isize) {
    with(|l| {
        if let Some(id) = l.object_at(dso).map(|o| o.id) {
            l.adjust_refcount(id, delta);
        }
    });
}

gfn! {
    unsafe fn loader_add_thread_local_dtor(dso: *mut c_void) -> () {
        crate::host::guard((), || dso_refcount(dso as u64, 1))
    }
}

gfn! {
    unsafe fn loader_remove_thread_local_dtor(dso: *mut c_void) -> () {
        crate::host::guard((), || dso_refcount(dso as u64, -1))
    }
}

pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![
        ("__loader_dlopen", loader_dlopen as usize as u64),
        ("__loader_android_dlopen_ext", loader_android_dlopen_ext as usize as u64),
        ("__loader_dlerror", loader_dlerror as usize as u64),
        ("__loader_dlsym", loader_dlsym as usize as u64),
        ("__loader_dlvsym", loader_dlvsym as usize as u64),
        ("__loader_dlclose", loader_dlclose as usize as u64),
        ("__loader_dladdr", loader_dladdr as usize as u64),
        ("__loader_dl_iterate_phdr", loader_dl_iterate_phdr as usize as u64),
        ("__loader_android_get_LD_LIBRARY_PATH", loader_get_ld_library_path as usize as u64),
        ("__loader_android_update_LD_LIBRARY_PATH", loader_update_ld_library_path as usize as u64),
        ("__loader_android_get_application_target_sdk_version", loader_get_target_sdk as usize as u64),
        ("__loader_android_set_application_target_sdk_version", loader_set_target_sdk as usize as u64),
        ("__loader_android_create_namespace", loader_namespace_unsupported as usize as u64),
        ("__loader_android_get_exported_namespace", loader_namespace_unsupported as usize as u64),
        ("__loader_android_init_anonymous_namespace", loader_namespace_unsupported_bool as usize as u64),
        ("__loader_android_link_namespaces", loader_namespace_unsupported_bool as usize as u64),
        ("__loader_android_link_namespaces_all_libs", loader_namespace_unsupported_bool as usize as u64),
        ("__loader_android_dlwarning", loader_dlwarning as usize as u64),
        ("__loader_android_handle_signal", loader_handle_signal as usize as u64),
        ("__loader_android_set_16kb_appcompat_mode", loader_set_16kb_appcompat as usize as u64),
        ("__loader_cfi_fail", loader_cfi_fail as usize as u64),
        ("__loader_add_thread_local_dtor", loader_add_thread_local_dtor as usize as u64),
        ("__loader_remove_thread_local_dtor", loader_remove_thread_local_dtor as usize as u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_round_trip() {
        for id in [0usize, 1, 7, 1000] {
            assert_eq!(id_of(handle_of(id)), Some(id));
        }
        assert_eq!(id_of(RTLD_DEFAULT), None);
        assert_eq!(id_of(RTLD_NEXT), None);
        assert_eq!(id_of(0x1000), None);
    }

    #[test]
    fn dlerror_is_per_call() {
        set_error("x");
        // SAFETY: host function called as guest code would.
        let p = unsafe { loader_dlerror() };
        // SAFETY: the returned message stays valid until the next call.
        assert_eq!(unsafe { CStr::from_ptr(p) }.to_str().unwrap(), "x");
        // SAFETY: as above.
        assert!(unsafe { loader_dlerror() }.is_null());
    }

    #[test]
    fn unused_flags_constant() {
        assert_eq!(rtld::GLOBAL, 0x100);
    }
}

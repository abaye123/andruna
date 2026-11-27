//! The host symbol table: functions guest code imports by name.
//!
//! Guest objects link against `libandruna_host.so`, a stub library made
//! from `aosp/bionic/host_symbols.txt` (as NDK code links against stub
//! libraries); the loader never loads it and binds those names here,
//! after every loaded object. Each function uses the guest convention
//! (`gfn!`) and catches panics, so none crosses into guest code.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use crate::error::Result;

/// The soname of the stub library.
pub const HOST_LIBRARY: &str = "libandruna_host.so";

/// The names guest objects are linked against (the stub library's list).
pub const DECLARED: &str = include_str!("../../../aosp/bionic/host_symbols.txt");

/// Runs a host function body; a panic becomes `fail`.
pub fn guard<T>(fail: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            crate::debug::report(format_args!("host function panicked"));
            fail
        }
    }
}

/// The table: every host function by name.
pub fn table() -> HashMap<Vec<u8>, u64> {
    let mut t = HashMap::new();
    for (n, a) in crate::syscall::symbols()
        .into_iter()
        .chain(crate::thread::symbols())
        .chain(crate::dl::symbols())
        .chain(crate::icu::symbols())
        .chain(registered())
    {
        let old = t.insert(n.as_bytes().to_vec(), a);
        assert!(old.is_none(), "host symbol {n} defined twice");
    }
    t
}

static REGISTERED: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

/// Adds host functions from another crate (for example andruna-gles's
/// `__andruna_gles_resolve`) to the table of every [`crate::Linker`]
/// created afterwards. Each address must be a function with the guest
/// convention that catches its own panics, like the loader's own.
pub fn register(symbols: &[(&'static str, u64)]) {
    REGISTERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend_from_slice(symbols);
}

fn registered() -> Vec<(&'static str, u64)> {
    REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Names of the declared list that the table does not define.
pub fn missing_from_table() -> Vec<&'static str> {
    let t = table();
    DECLARED
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|n| !t.contains_key(n.as_bytes()))
        .collect()
}

static STUB_NAMES: Mutex<Vec<CString>> = Mutex::new(Vec::new());

/// An executable stub for a symbol nothing defines (only with
/// `Config::unresolved_stubs`): it reports the call and fails with
/// `ENOSYS`.
pub fn unresolved_stub(name: &str) -> Result<u64> {
    let c = CString::new(name).unwrap_or_default();
    let np = c.as_ptr() as u64;
    STUB_NAMES.lock().unwrap_or_else(|e| e.into_inner()).push(c);
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
        // ldr x0, #16; ldr x16, #20; br x16; nop; .quad name; .quad handler
        for w in [0x5800_0080u32, 0x5800_00b0, 0xd61f_0200, 0xd503_201f] {
            code.extend_from_slice(&w.to_le_bytes());
        }
        code.extend_from_slice(&np.to_le_bytes());
        code.extend_from_slice(&h.to_le_bytes());
    }
    crate::tls::code_stub(&code)
}

gfn! {
    unsafe fn unresolved_called(name: *const core::ffi::c_char) -> i64 {
        guard(-1, || {
            // SAFETY: the stub passes its own NUL-terminated name.
            let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            crate::debug::report(format_args!("unresolved guest import called: {n}"));
            -i64::from(andruna_lx::linux::errno::ENOSYS)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_covers_the_declared_list() {
        assert_eq!(missing_from_table(), Vec::<&str>::new());
    }
}

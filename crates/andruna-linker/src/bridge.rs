//! The bridge: bionic's loader-side state, in an ELF object.
//!
//! On Android the dynamic linker contains a private copy of libc that
//! sets up the main thread (`__libc_init_main_thread_*`), owns
//! `__libc_shared_globals` (TLS layout and module table, which `libc.so`
//! uses for every later thread and for `__tls_get_addr`) and the dynamic
//! TLSDESC resolver. andruna-linker is Rust, so this part is a small ELF
//! object built from bionic's sources with the same headers as `libc.so`,
//! installed as `ld-android.so` (the name `libc.so` and `libdl.so` need):
//! `aosp/bionic/ld_android_bridge.cpp`. The loader links it first and calls
//! it at the points where bionic's linker does the same work.

use core::ffi::c_char;
use std::ffi::CString;

use crate::error::{Error, Result};
use crate::linker::Linker;
use crate::object::{Object, ObjectId, TlsInfo};

/// `AndrunaTlsSegment` in the bridge.
#[repr(C)]
struct Segment {
    init_ptr: u64,
    init_size: u64,
    size: u64,
    align: u64,
}

pub const SONAME: &str = "ld-android.so";

type InitFn = gty!(fn(u64));
type TlsExeFn = gty!(fn(*const Segment, *const c_char, *mut i64) -> usize);
type TlsRegisterFn = gty!(fn(*const Segment, u64, *mut i64, *mut i32) -> usize);
type SizeFn = gty!(fn(usize) -> usize);
type VoidFn = gty!(fn());
type AddrFn = gty!(fn() -> u64);

/// Whether `o` is the bridge.
pub fn is_bridge(o: &Object) -> bool {
    (o.soname.as_deref() == Some(SONAME) || o.name == SONAME)
        && o.lookup(b"__andruna_bridge_init", None).is_some()
}

pub struct Bridge {
    pub id: ObjectId,
    tls_exe: u64,
    tls_register: u64,
    tls_unregister: u64,
    tls_generation: u64,
    tls_finalize: u64,
    finish: u64,
    tlsdesc_dynamic: u64,
}

impl Bridge {
    /// Finds the bridge's entry points and runs its early initialization
    /// (bionic's `__libc_init_main_thread_early` and `_late`): the main
    /// thread gets a temporary TCB and thread pointer.
    pub fn new(l: &Linker, id: ObjectId) -> Result<Self> {
        let o = l.get(id).expect("bridge object");
        let f = |n: &str| -> Result<u64> {
            o.lookup(n.as_bytes(), None)
                .map(|(_, s)| o.addr(s.st_value))
                .ok_or_else(|| Error::invalid(&o.name, &format!("bridge has no {n}")))
        };
        let init = f("__andruna_bridge_init")?;
        let b = Bridge {
            id,
            tls_exe: f("__andruna_bridge_tls_exe")?,
            tls_register: f("__andruna_bridge_tls_register")?,
            tls_unregister: f("__andruna_bridge_tls_unregister")?,
            tls_generation: f("__andruna_bridge_tls_generation")?,
            tls_finalize: f("__andruna_bridge_tls_finalize")?,
            finish: f("__andruna_bridge_finish_main_thread")?,
            tlsdesc_dynamic: 0,
        };
        let args = l
            .startup
            .as_ref()
            .map(|s| s.block)
            .ok_or_else(|| Error::new("bionic's libc can only be loaded by a program start"))?;
        // SAFETY: the bridge's init function (relocated, protected), with
        // the initial stack block it expects.
        let init: InitFn = unsafe { core::mem::transmute(init) };
        init(args);
        let tlsdesc: AddrFn = {
            let a = f("__andruna_bridge_tlsdesc_dynamic")?;
            // SAFETY: a bridge function returning an address.
            unsafe { core::mem::transmute(a) }
        };
        Ok(Bridge {
            tlsdesc_dynamic: tlsdesc(),
            ..b
        })
    }

    fn segment(t: &TlsInfo) -> Segment {
        Segment {
            init_ptr: t.image,
            init_size: t.filesz,
            size: t.memsz,
            align: t.align,
        }
    }

    /// Reserves the executable's TLS segment (or none) and bionic's TCB
    /// and `bionic_tls` (`linker_setup_exe_static_tls`). Returns the
    /// module id and thread-pointer offset.
    pub fn tls_exe(&self, t: Option<&TlsInfo>, progname: &str) -> Result<(usize, Option<i64>)> {
        let seg = t.map(Self::segment);
        let name = CString::new(progname.replace('\0', "")).unwrap_or_default();
        let mut off = 0i64;
        // SAFETY: a bridge function; the arguments live across the call.
        let f: TlsExeFn = unsafe { core::mem::transmute(self.tls_exe) };
        let id = f(
            seg.as_ref()
                .map_or(core::ptr::null(), |s| s as *const Segment),
            name.as_ptr(),
            &mut off,
        );
        Ok((id, t.map(|_| off)))
    }

    /// Registers a shared object's TLS segment (`register_soinfo_tls`):
    /// static while the start-up set loads, dynamic afterwards.
    pub fn tls_register(&self, t: &TlsInfo, handle: ObjectId) -> Result<(usize, Option<i64>)> {
        let seg = Self::segment(t);
        let (mut off, mut is_static) = (0i64, 0i32);
        // SAFETY: as above.
        let f: TlsRegisterFn = unsafe { core::mem::transmute(self.tls_register) };
        let id = f(&seg, handle as u64 + 1, &mut off, &mut is_static);
        if id == 0 {
            return Err(Error::new("bionic refused the TLS segment"));
        }
        Ok((id, (is_static != 0).then_some(off)))
    }

    pub fn tls_unregister(&self, module: usize) -> Result<()> {
        // SAFETY: as above.
        let f: SizeFn = unsafe { core::mem::transmute(self.tls_unregister) };
        f(module);
        Ok(())
    }

    /// The generation in which `module` was registered (for the dynamic
    /// TLSDESC argument).
    pub fn tls_generation(&self, module: usize) -> Result<u64> {
        // SAFETY: as above.
        let f: SizeFn = unsafe { core::mem::transmute(self.tls_generation) };
        Ok(f(module) as u64)
    }

    /// Ends the static TLS layout (`linker_finalize_static_tls`).
    pub fn tls_finalize(&self) -> Result<()> {
        // SAFETY: as above.
        let f: VoidFn = unsafe { core::mem::transmute(self.tls_finalize) };
        f();
        Ok(())
    }

    /// Allocates the main thread's final static TLS and copies the
    /// (relocated) TLS images (`__libc_init_main_thread_final`).
    pub fn finish_main_thread(&self) {
        // SAFETY: as above.
        let f: VoidFn = unsafe { core::mem::transmute(self.finish) };
        f();
    }

    /// bionic's `tlsdesc_resolver_dynamic`.
    pub fn tlsdesc_dynamic(&self) -> u64 {
        self.tlsdesc_dynamic
    }
}

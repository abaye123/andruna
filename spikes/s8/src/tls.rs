//! Guest thread pointer and ELF TLS.
//!
//! Static TLS only (every module is loaded before the first guest thread
//! starts). Layout per thread, as bionic's on each architecture:
//!
//! - x86_64 (TLS variant II): the module blocks lie below the thread
//!   pointer; at the thread pointer is bionic's slot array, with slot 0 =
//!   the thread pointer itself (`%fs:0`, required by the psABI) and slot 5
//!   = the stack guard (`%fs:0x28`, read by every protected function).
//! - arm64 (variant I): bionic's slots from `tp - 8*1` to `tp + 8*7`; the
//!   first module block starts at `tp + align_up(16, p_align)` (Android
//!   requires `p_align >= 64`, so at `tp + 64`). Not run in this spike.
//!
//! On x64 the thread pointer is the FS base, written with `WRFSBASE` and
//! restored lazily by a first vectored exception handler when Windows has
//! reset it to 0 after a context switch (spike S4).

use std::cell::Cell;
use std::sync::OnceLock;

use crate::loader::Object;

/// Number of bionic TLS slots reserved at the thread pointer.
const SLOTS: usize = 8;
const SLOT_SELF: usize = 0;
const SLOT_STACK_GUARD: usize = 5;

#[derive(Debug, Clone)]
struct Module {
    image: u64,
    filesz: u64,
    memsz: u64,
    /// x64: block at `tp - offset`; arm64: block at `tp + offset`.
    offset: u64,
}

#[derive(Debug, Default)]
struct Layout {
    modules: Vec<Module>,
    /// Bytes of module blocks (below tp on x64, above the slots on arm64).
    size: u64,
    align: u64,
}

static LAYOUT: OnceLock<Layout> = OnceLock::new();
static mut STACK_GUARD: u64 = 0;

thread_local! {
    /// The guest thread pointer of this thread (0: not a guest thread).
    static TP: Cell<u64> = const { Cell::new(0) };
    static RESTORES: Cell<u64> = const { Cell::new(0) };
}

fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

/// Assigns module ids and static offsets to the objects with `PT_TLS`.
pub fn layout(objs: &mut [Object]) -> Result<(), String> {
    if LAYOUT.get().is_some() {
        return Err("static TLS is laid out once (no dlopen in this spike)".into());
    }
    let mut l = Layout {
        align: 64,
        ..Layout::default()
    };
    #[cfg(target_arch = "aarch64")]
    {
        l.size = 16;
    }
    for o in objs.iter_mut() {
        let Some(p) = o.phdrs.iter().find(|p| p.p_type == crate::elf::PT_TLS) else {
            continue;
        };
        // arm64: bionic's slots occupy tp..tp+64, so blocks are aligned to
        // at least 64 (lld does the same for Android executables).
        let a = if cfg!(target_arch = "aarch64") { p.p_align.max(64) } else { p.p_align.max(1) };
        l.align = l.align.max(a);
        #[cfg(target_arch = "x86_64")]
        let offset = {
            l.size = align_up(l.size + p.p_memsz, a);
            l.size
        };
        #[cfg(target_arch = "aarch64")]
        let offset = {
            let off = align_up(l.size, a);
            l.size = off + p.p_memsz;
            off
        };
        l.modules.push(Module {
            image: o.bias + p.p_vaddr,
            filesz: p.p_filesz,
            memsz: p.p_memsz,
            offset,
        });
        o.tls_module = l.modules.len();
    }
    let mut guard = [0u8; 8];
    crate::host::random_bytes(&mut guard);
    // SAFETY: written once, before any guest thread exists.
    unsafe { STACK_GUARD = u64::from_ne_bytes(guard) & !0xff };
    LAYOUT.set(l).map_err(|_| "TLS layout set twice".to_string())
}

/// The value a `TPOFF64` relocation stores for module `m` (1-based).
pub fn tp_offset(m: usize) -> i64 {
    let l = LAYOUT.get().expect("TLS laid out");
    let off = l.modules[m - 1].offset as i64;
    if cfg!(target_arch = "x86_64") { -off } else { off }
}

/// The thread's TLS area: allocated, initialized, owned by the thread.
pub struct ThreadArea {
    alloc: *mut u8,
    layout: std::alloc::Layout,
    pub tp: u64,
}

// SAFETY: the area is plain memory used by one thread at a time.
unsafe impl Send for ThreadArea {}

impl ThreadArea {
    pub fn new() -> ThreadArea {
        let l = LAYOUT.get().expect("TLS laid out");
        let slots = (SLOTS * 8) as u64;
        #[cfg(target_arch = "x86_64")]
        let (total, tp_off) = {
            let below = align_up(l.size, l.align);
            (below + slots, below)
        };
        #[cfg(target_arch = "aarch64")]
        // Slot -1 below tp, slots 0..7 above it, then the module blocks
        // (the first one at tp + 64 or more).
        let (total, tp_off) = (l.align + align_up(l.size.max(slots), l.align), l.align);
        let layout = std::alloc::Layout::from_size_align(total as usize, l.align as usize).expect("layout");
        // SAFETY: non-zero size.
        let alloc = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!alloc.is_null(), "out of memory for TLS");
        let tp = alloc as u64 + tp_off;
        for m in &l.modules {
            #[cfg(target_arch = "x86_64")]
            let block = tp - m.offset;
            #[cfg(target_arch = "aarch64")]
            let block = tp + m.offset;
            // SAFETY: the block lies inside the allocation (layout above)
            // and the image is the object's mapped PT_TLS data.
            unsafe {
                core::ptr::copy_nonoverlapping(m.image as *const u8, block as *mut u8, m.filesz as usize);
                let _ = m.memsz;
            }
        }
        // SAFETY: the slots lie inside the allocation.
        unsafe {
            let s = tp as *mut u64;
            #[cfg(target_arch = "x86_64")]
            s.add(SLOT_SELF).write(tp);
            s.add(SLOT_STACK_GUARD).write(STACK_GUARD);
        }
        let _ = SLOT_SELF;
        ThreadArea { alloc, layout, tp }
    }

    /// Makes this area the current thread's guest thread pointer.
    pub fn activate(&self) {
        TP.with(|c| c.set(self.tp));
        set_thread_pointer(self.tp);
    }
}

impl Drop for ThreadArea {
    fn drop(&mut self) {
        TP.with(|c| {
            if c.get() == self.tp {
                c.set(0)
            }
        });
        // SAFETY: allocated in `new` with this layout.
        unsafe { std::alloc::dealloc(self.alloc, self.layout) };
    }
}

/// The current guest thread pointer (from the host's own TLS, so it is
/// right even after Windows reset FS base).
pub fn current_tp() -> u64 {
    TP.with(|c| c.get())
}

#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn restores_here() -> u64 {
    RESTORES.with(|c| c.get())
}

/// `__tls_get_addr(tls_index*)`: static TLS, so a fixed offset from the
/// thread pointer.
pub fn tls_get_addr(module: u64, offset: u64) -> u64 {
    let tp = current_tp();
    let l = LAYOUT.get().expect("TLS laid out");
    let m = &l.modules[module as usize - 1];
    #[cfg(target_arch = "x86_64")]
    return tp - m.offset + offset;
    #[cfg(target_arch = "aarch64")]
    return tp + m.offset + offset;
}

#[cfg(target_arch = "x86_64")]
fn set_thread_pointer(tp: u64) {
    crate::debug::install_handler();
    wrfsbase(tp);
}

#[cfg(target_arch = "aarch64")]
fn set_thread_pointer(tp: u64) {
    // S4: whether Windows preserves TPIDR_EL0 is not verified; this is the
    // option-1 path (write it and rely on it being kept).
    crate::debug::install_handler();
    // SAFETY: writes the user-mode thread ID register.
    unsafe { core::arch::asm!("msr tpidr_el0, {}", in(reg) tp, options(nostack)) };
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn rdfsbase() -> u64 {
    let v: u64;
    // SAFETY: RDFSBASE is enabled for user mode on Windows 11 (S4); the
    // start-up check in main verifies it before any guest code runs.
    unsafe {
        core::arch::asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xc0", out("rax") v, options(nomem, nostack, preserves_flags))
    };
    v
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn wrfsbase(base: u64) {
    // SAFETY: as `rdfsbase`; FS is not used by Windows or Rust code on x64.
    unsafe {
        core::arch::asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xd0", in("rax") base, options(nomem, nostack, preserves_flags))
    };
}

/// Called by the exception handler on an access violation: restores FS
/// base if the faulting instruction uses FS and the base is stale.
#[cfg(target_arch = "x86_64")]
pub fn lazy_restore(rip: u64) -> bool {
    let tp = current_tp();
    if tp == 0 || rdfsbase() == tp || !has_fs_prefix(rip) {
        return false;
    }
    wrfsbase(tp);
    RESTORES.with(|c| c.set(c.get() + 1));
    true
}

#[cfg(target_arch = "x86_64")]
fn has_fs_prefix(rip: u64) -> bool {
    for i in 0..15 {
        // SAFETY: rip is the faulting instruction in mapped code; at most
        // 15 bytes (the longest instruction) are read.
        let b = unsafe { *((rip + i) as *const u8) };
        match b {
            0x64 => return true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 | 0x40..=0x4f => continue,
            _ => return false,
        }
    }
    false
}

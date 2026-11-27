//! The guest thread pointer and ELF TLS.
//!
//! # Thread pointer (spike S4)
//!
//! - x64: the FS base, written with `WRFSBASE`. Windows resets it to 0 when
//!   a thread is switched out; the next FS-relative access then faults and
//!   the first vectored exception handler ([`lazy_restore`]) writes the
//!   thread's registered value again and retries the instruction.
//! - arm64: `TPIDR_EL0`, written once with `msr` (bionic's `__set_tls`).
//!   The ARM64 runs of S4 measured that Windows preserves it per thread
//!   (no change in 1.8 billion checks across context switches, exceptions,
//!   APCs and suspend/context calls; new threads start at 0), so there is no
//!   lazy restore and no code rewriting. x18 must stay the TEB at all times
//!   (a fault with another x18 kills the process), which is why bionic's
//!   shadow call stack set-up is disabled on arm64.
//!
//! The registered value lives in the host's own TLS, so it is right even
//! while the FS base is stale.
//!
//! # Modules and layout
//!
//! Each object with `PT_TLS` gets a module id. With real bionic in the
//! process, bionic owns the layout: the loader asks the bridge object
//! (`ld-android.so`, [`crate::bridge`]) to place each module, exactly as
//! bionic's linker does with `StaticTlsLayout`, and bionic's `libc.so`
//! allocates every thread's TLS. Without bionic (objects built
//! `-nostdlib`, tests), a built-in layout with bionic's slot positions is
//! used for the objects loaded at start-up; dynamic TLS then needs bionic.

use std::cell::Cell;
use std::sync::Mutex;

use crate::error::{Error, Result};

/// Number of bionic TLS slots at the thread pointer in the built-in
/// layout, and the two the compiler and psABI rely on.
const SLOTS: usize = 8;
#[cfg(target_arch = "x86_64")]
const SLOT_SELF: usize = 0;
const SLOT_STACK_GUARD: usize = 5;

thread_local! {
    /// The guest thread pointer of this thread (0: none).
    static TP: Cell<u64> = const { Cell::new(0) };
    static RESTORES: Cell<u64> = const { Cell::new(0) };
}

/// Sets the current thread's guest thread pointer.
pub fn set_thread_pointer(tp: u64) {
    crate::debug::install_handler();
    TP.with(|c| c.set(tp));
    write_tp(tp);
}

/// The current thread's registered guest thread pointer (0: none).
pub fn current() -> u64 {
    TP.with(|c| c.get())
}

/// Lazy FS-base restores taken by the current thread.
pub fn restores() -> u64 {
    RESTORES.with(|c| c.get())
}

/// Whether the CPU supports the thread-pointer mechanism of this
/// architecture.
pub fn supported() -> Result<()> {
    #[cfg(target_arch = "x86_64")]
    {
        // CPUID.(EAX=7,ECX=0):EBX[0] = FSGSBASE. Every CPU Windows 11
        // supports has it; Windows enabling it for user mode was measured
        // in S4.
        // SAFETY: CPUID is always available on x86_64.
        let r = unsafe { core::arch::x86_64::__cpuid_count(7, 0) };
        if r.ebx & 1 == 0 {
            return Err(Error::new("the CPU has no FSGSBASE instructions"));
        }
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn write_tp(tp: u64) {
    // SAFETY: WRFSBASE is enabled for user mode on Windows 11 (S4); FS is
    // not used by Windows or Rust code on x64.
    unsafe {
        core::arch::asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xd0", in("rax") tp, options(nomem, nostack, preserves_flags))
    };
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn read_fs_base() -> u64 {
    let v: u64;
    // SAFETY: RDFSBASE is enabled with WRFSBASE (S4).
    unsafe {
        core::arch::asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xc0", out("rax") v, options(nomem, nostack, preserves_flags))
    };
    v
}

#[cfg(target_arch = "aarch64")]
fn write_tp(tp: u64) {
    // SAFETY: writes the user read-write thread ID register, which Windows
    // preserves per thread and does not use itself (S4, ARM64 runs).
    unsafe { core::arch::asm!("msr tpidr_el0, {}", in(reg) tp, options(nostack, preserves_flags)) };
}

/// Called by the first vectored handler on an access violation at `pc`:
/// restores the FS base if the instruction uses FS and the base is stale.
/// Takes no locks and does not allocate.
#[cfg(target_arch = "x86_64")]
pub fn lazy_restore(pc: u64) -> bool {
    let tp = current();
    if tp == 0 || read_fs_base() == tp || !has_fs_prefix(pc) {
        return false;
    }
    write_tp(tp);
    RESTORES.with(|c| c.set(c.get() + 1));
    true
}

#[cfg(target_arch = "x86_64")]
fn has_fs_prefix(pc: u64) -> bool {
    for i in 0..15 {
        // SAFETY: `pc` is the faulting instruction in mapped code; at most
        // the 15 bytes of the longest instruction are read, stopping at the
        // first byte that is not a prefix.
        let b = unsafe { *((pc + i) as *const u8) };
        match b {
            0x64 => return true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 | 0x40..=0x4f => {}
            _ => return false,
        }
    }
    false
}

/// One registered TLS module.
#[derive(Clone, Debug)]
pub struct Module {
    pub id: usize,
    pub image: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
    /// Offset of the block from the thread pointer (static TLS).
    pub tp_offset: Option<i64>,
}

/// The built-in static layout (no bionic): blocks below the thread pointer
/// on x64 (variant II), after the 64-byte slot area on arm64 (variant I).
#[derive(Default, Debug)]
pub struct BuiltinLayout {
    modules: Vec<Module>,
    /// x64: bytes below tp; arm64: bytes from tp.
    size: u64,
    align: u64,
    frozen: bool,
}

impl BuiltinLayout {
    pub fn new() -> Self {
        Self {
            modules: vec![],
            #[cfg(target_arch = "aarch64")]
            size: (SLOTS * 8) as u64,
            #[cfg(not(target_arch = "aarch64"))]
            size: 0,
            align: 64,
            frozen: false,
        }
    }

    /// Places a module; returns its id and thread-pointer offset.
    pub fn reserve(&mut self, image: u64, filesz: u64, memsz: u64, align: u64) -> Result<Module> {
        if self.frozen {
            return Err(Error::new(
                "dynamic TLS needs bionic (no libc.so with TLS support is loaded)",
            ));
        }
        let a = align.max(1);
        self.align = self.align.max(a);
        let off = if cfg!(target_arch = "x86_64") {
            self.size = (self.size + memsz).div_ceil(a) * a;
            -(self.size as i64)
        } else {
            let o = self.size.div_ceil(a) * a;
            self.size = o + memsz;
            o as i64
        };
        let m = Module {
            id: self.modules.len() + 1,
            image,
            filesz,
            memsz,
            align: a,
            tp_offset: Some(off),
        };
        self.modules.push(m.clone());
        Ok(m)
    }

    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    pub fn module(&self, id: usize) -> Option<&Module> {
        id.checked_sub(1).and_then(|i| self.modules.get(i))
    }
}

/// A thread's TLS area in the built-in layout.
pub struct ThreadArea {
    alloc: *mut u8,
    layout: std::alloc::Layout,
    pub tp: u64,
}

// SAFETY: plain memory owned by one thread at a time.
unsafe impl Send for ThreadArea {}

impl ThreadArea {
    /// Allocates and initializes a TLS area for `l` (static modules
    /// copied, slots set). `guard` is the stack guard value.
    pub fn new(l: &BuiltinLayout, guard: u64) -> ThreadArea {
        let slots = (SLOTS * 8) as u64;
        let align = l.align.max(64);
        let (total, tp_off) = if cfg!(target_arch = "x86_64") {
            let below = l.size.div_ceil(align) * align;
            (below + slots, below)
        } else {
            (l.size.max(slots).div_ceil(align) * align, 0)
        };
        let layout = std::alloc::Layout::from_size_align(total as usize, align as usize)
            .expect("TLS layout");
        // SAFETY: non-zero size.
        let alloc = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!alloc.is_null(), "out of memory for TLS");
        let tp = alloc as u64 + tp_off;
        for m in &l.modules {
            let block = tp.wrapping_add_signed(m.tp_offset.expect("static"));
            // SAFETY: the block lies inside the allocation by construction
            // of the layout, and the image is the object's PT_TLS data.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    m.image as *const u8,
                    block as *mut u8,
                    m.filesz as usize,
                )
            };
        }
        // SAFETY: the slot area lies inside the allocation.
        unsafe {
            let s = tp as *mut u64;
            #[cfg(target_arch = "x86_64")]
            s.add(SLOT_SELF).write(tp);
            s.add(SLOT_STACK_GUARD).write(guard);
        }
        ThreadArea { alloc, layout, tp }
    }
}

impl Drop for ThreadArea {
    fn drop(&mut self) {
        if current() == self.tp {
            TP.with(|c| c.set(0));
        }
        // SAFETY: allocated in `new` with this layout.
        unsafe { std::alloc::dealloc(self.alloc, self.layout) };
    }
}

/// `__tls_get_addr` for the built-in layout (static modules only).
pub fn builtin_tls_get_addr(l: &BuiltinLayout, module: u64, offset: u64) -> u64 {
    let tp = current();
    match l.module(module as usize).and_then(|m| m.tp_offset) {
        Some(off) => tp.wrapping_add_signed(off).wrapping_add(offset),
        None => 0,
    }
}

/// Executable code stubs made at run time (TLSDESC resolvers).
static CODE: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// Copies `code` into an executable page; returns its address.
pub fn code_stub(code: &[u8]) -> Result<u64> {
    use andruna_lx::linux::mman::*;
    let mut c = CODE.lock().unwrap_or_else(|e| e.into_inner());
    let (page, used) = *c;
    let len = code.len().div_ceil(16) as u64 * 16;
    let (page, used) = if page == 0 || used + len > PAGE_SIZE as u64 {
        let p = andruna_lx::mem::mmap(
            0,
            PAGE_SIZE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )? as u64;
        (p, 0)
    } else {
        andruna_lx::mem::mprotect(page as usize, PAGE_SIZE, PROT_READ | PROT_WRITE)?;
        (page, used)
    };
    let at = page + used;
    // SAFETY: `at..at+len` lies in the page, mapped read-write above.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
    andruna_lx::mem::mprotect(page as usize, PAGE_SIZE, PROT_READ | PROT_EXEC)?;
    crate::image::flush_code(page, PAGE_SIZE as u64);
    *c = (page, used + len);
    Ok(at)
}

/// The TLSDESC function for static TLS: returns the descriptor's second
/// word, the variable's offset from the thread pointer (bionic's
/// `tlsdesc_resolver_static`).
pub fn tlsdesc_static(machine: u16) -> Result<u64> {
    static ADDR: Mutex<[u64; 2]> = Mutex::new([0; 2]);
    let (i, code): (usize, &[u8]) = match machine {
        // mov rax, [rax + 8]; ret
        crate::elf::EM_X86_64 => (0, &[0x48, 0x8b, 0x40, 0x08, 0xc3]),
        // ldr x0, [x0, #8]; ret
        _ => (1, &[0x00, 0x04, 0x40, 0xf9, 0xc0, 0x03, 0x5f, 0xd6]),
    };
    let mut a = ADDR.lock().unwrap_or_else(|e| e.into_inner());
    if a[i] == 0 {
        a[i] = code_stub(code)?;
    }
    Ok(a[i])
}

/// The TLSDESC function for an undefined weak TLS symbol: returns
/// `addend - tp`, so that `tp + result` is the addend from NULL (bionic's
/// `tlsdesc_resolver_unresolved_weak`). Only the result register changes.
pub fn tlsdesc_undefined_weak(machine: u16) -> Result<u64> {
    static ADDR: Mutex<[u64; 2]> = Mutex::new([0; 2]);
    let (i, code): (usize, &[u8]) = match machine {
        // mov rax, [rax + 8]; sub rax, fs:[0]; ret
        crate::elf::EM_X86_64 => (
            0,
            &[
                0x48, 0x8b, 0x40, 0x08, 0x64, 0x48, 0x2b, 0x04, 0x25, 0, 0, 0, 0, 0xc3,
            ],
        ),
        // str x19, [sp, #-16]!; mrs x19, tpidr_el0; ldr x0, [x0, #8];
        // sub x0, x0, x19; ldr x19, [sp], #16; ret
        _ => (
            1,
            &[
                0xf3, 0x0f, 0x1f, 0xf8, 0x53, 0xd0, 0x3b, 0xd5, 0x00, 0x04, 0x40, 0xf9, 0x00, 0x00,
                0x13, 0xcb, 0xf3, 0x07, 0x41, 0xf8, 0xc0, 0x03, 0x5f, 0xd6,
            ],
        ),
    };
    let mut a = ADDR.lock().unwrap_or_else(|e| e.into_inner());
    if a[i] == 0 {
        a[i] = code_stub(code)?;
    }
    Ok(a[i])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layout_offsets() {
        let mut l = BuiltinLayout::new();
        let a = l.reserve(0, 0, 24, 8).unwrap();
        let b = l.reserve(0, 0, 100, 64).unwrap();
        assert_eq!((a.id, b.id), (1, 2));
        if cfg!(target_arch = "x86_64") {
            assert_eq!(a.tp_offset, Some(-24));
            assert_eq!(b.tp_offset, Some(-128));
        } else {
            assert_eq!(a.tp_offset, Some(64));
            assert_eq!(b.tp_offset, Some(128));
        }
        l.freeze();
        assert!(l.reserve(0, 0, 8, 8).is_err());
    }

    #[test]
    fn thread_area_copies_images_and_slots() {
        let image = [1u8, 2, 3, 4];
        let mut l = BuiltinLayout::new();
        let m = l.reserve(image.as_ptr() as u64, 4, 16, 16).unwrap();
        let area = ThreadArea::new(&l, 0x1234_5600);
        let block = area.tp.wrapping_add_signed(m.tp_offset.unwrap());
        // SAFETY: the block and slots lie inside the area.
        unsafe {
            assert_eq!(*(block as *const [u8; 4]), image);
            assert_eq!(*((block + 4) as *const u32), 0);
            assert_eq!(*((area.tp + 40) as *const u64), 0x1234_5600);
            #[cfg(target_arch = "x86_64")]
            assert_eq!(*(area.tp as *const u64), area.tp);
        }
    }

    #[test]
    fn thread_pointer_round_trip() {
        supported().unwrap();
        std::thread::spawn(|| {
            let slots = Box::new([0u64; 8]);
            let tp = slots.as_ptr() as u64;
            set_thread_pointer(tp);
            assert_eq!(current(), tp);
            #[cfg(target_arch = "x86_64")]
            {
                // A context switch may reset the base; reading through FS
                // must still give the slot (lazy restore).
                let mut v = 0u64;
                for _ in 0..1000 {
                    std::thread::yield_now();
                    // SAFETY: reads slot 0 through FS, which is `tp`.
                    unsafe { core::arch::asm!("mov {}, fs:[0]", out(reg) v) };
                }
                assert_eq!(v, 0);
                // Clear the base before the slots are freed.
                write_tp(0);
            }
            TP.with(|c| c.set(0));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn tlsdesc_stubs_run() {
        let st = tlsdesc_static(crate::elf::EM_NATIVE).unwrap();
        let desc = [st, 0x1234u64];
        #[cfg(target_arch = "x86_64")]
        {
            let r: u64;
            // SAFETY: calls the stub with the TLSDESC convention (rax =
            // descriptor); it only reads the descriptor.
            unsafe {
                core::arch::asm!("call {f}", f = in(reg) st, inout("rax") desc.as_ptr() as u64 => r, clobber_abi("C"))
            };
            assert_eq!(r, 0x1234);
        }
        #[cfg(target_arch = "aarch64")]
        {
            let r: u64;
            // SAFETY: as above with x0 = descriptor.
            unsafe {
                core::arch::asm!("blr {f}", f = in(reg) st, inout("x0") desc.as_ptr() as u64 => r, clobber_abi("C"))
            };
            assert_eq!(r, 0x1234);
        }
    }
}

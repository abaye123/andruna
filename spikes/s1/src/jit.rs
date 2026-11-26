//! A JIT-like executable buffer: code is copied in while the buffer is
//! writable, pointer slots are patched, then the buffer becomes read+execute
//! (W^X). Unwind data for the generated code is registered with the OS
//! either as a static table (`RtlAddFunctionTable`) or through a callback
//! (`RtlInstallFunctionTableCallback`), which is how a JIT code cache whose
//! contents change would do it.

use crate::arch;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ,
    PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::GetCurrentProcess;

#[cfg(target_arch = "aarch64")]
use windows::Win32::System::Diagnostics::Debug::IMAGE_ARM64_RUNTIME_FUNCTION_ENTRY as RuntimeFunction;
#[cfg(target_arch = "x86_64")]
use windows::Win32::System::Diagnostics::Debug::IMAGE_RUNTIME_FUNCTION_ENTRY as RuntimeFunction;

/// Writable cells the generated code stores into (through pointer slots).
pub static NPE_RETADDR: AtomicU64 = AtomicU64::new(0);
pub static SAVED_SP: AtomicU64 = AtomicU64::new(0);
pub static ENTRY_SP: AtomicU64 = AtomicU64::new(0);

pub static JIT_BASE: AtomicUsize = AtomicUsize::new(0);
pub static JIT_SIZE: AtomicUsize = AtomicUsize::new(0);

const BUF_SIZE: usize = 64 * 1024;
/// Offset of the unwind data inside the buffer (after the code).
const UNWIND_OFF: usize = 0x8000;

pub struct Jit {
    pub base: usize,
    pub l: arch::BlobLayout,
}

static JIT: OnceLock<Jit> = OnceLock::new();

pub fn get() -> &'static Jit {
    JIT.get_or_init(Jit::create)
}

impl Jit {
    fn create() -> Jit {
        let (src, l) = arch::blob();
        assert!(l.len < UNWIND_OFF);
        // SAFETY: fresh private allocation; we own it for the process lifetime.
        let base = unsafe { VirtualAlloc(None, BUF_SIZE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) }
            as usize;
        assert!(base != 0, "VirtualAlloc failed");
        // SAFETY: `src..src+len` is the blob in our image (readable), the
        // destination is the new RW buffer of BUF_SIZE bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(src, base as *mut u8, l.len);
            let patch = |off: usize, v: usize| *((base + off) as *mut u64) = v as u64;
            patch(l.slot_npe, NPE_RETADDR.as_ptr() as usize);
            patch(l.slot_sp, SAVED_SP.as_ptr() as usize);
            patch(l.slot_entry_sp, ENTRY_SP.as_ptr() as usize);
        }
        write_unwind_data(base, &l);
        let mut old = PAGE_PROTECTION_FLAGS(0);
        // SAFETY: changes protection of our own buffer; then flush the
        // instruction cache for the written range (required on ARM64).
        unsafe {
            VirtualProtect(base as *const _, BUF_SIZE, PAGE_EXECUTE_READ, &mut old)
                .expect("VirtualProtect RX");
            FlushInstructionCache(GetCurrentProcess(), Some(base as *const _), BUF_SIZE)
                .expect("FlushInstructionCache");
        }
        JIT_BASE.store(base, Ordering::Release);
        JIT_SIZE.store(BUF_SIZE, Ordering::Release);
        Jit { base, l }
    }

    pub fn at(&self, off: usize) -> usize {
        self.base + off
    }
}

pub fn contains(pc: usize) -> bool {
    let b = JIT_BASE.load(Ordering::Acquire);
    b != 0 && pc >= b && pc < b + JIT_SIZE.load(Ordering::Acquire)
}

/// The runtime function entry for `unw_func`, built once; it must stay
/// valid while registered, so it lives in a static.
static ENTRY: OnceLock<RuntimeFunction> = OnceLock::new();

#[cfg(target_arch = "x86_64")]
fn write_unwind_data(base: usize, l: &arch::BlobLayout) {
    let prolog = (l.unw_prolog_end - l.unw_func) as u8;
    let ui = arch::unw_func_unwind_info(prolog);
    // SAFETY: UNWIND_OFF + 12 is inside the RW buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(ui.as_ptr(), (base + UNWIND_OFF) as *mut u8, ui.len())
    };
    let e = RuntimeFunction {
        BeginAddress: l.unw_func as u32,
        EndAddress: l.unw_func_end as u32,
        Anonymous: windows::Win32::System::Diagnostics::Debug::IMAGE_RUNTIME_FUNCTION_ENTRY_0 {
            UnwindInfoAddress: UNWIND_OFF as u32,
        },
    };
    let _ = ENTRY.set(e);
}

#[cfg(target_arch = "aarch64")]
fn write_unwind_data(_base: usize, l: &arch::BlobLayout) {
    // Packed unwind data (no .xdata needed): Flag=1, FunctionLength in
    // 4-byte units, RegF=0, RegI=0, H=0, CR=3 (fp/lr pair + mov x29, sp),
    // FrameSize=2 (32 bytes): `stp x29, x30, [sp, #-32]!; mov x29, sp`.
    // The packed form describes exactly this two-instruction prologue.
    assert_eq!(l.unw_prolog_end - l.unw_func, 8);
    let len_words = ((l.unw_func_end - l.unw_func) / 4) as u32;
    let packed: u32 = 1 | (len_words << 2) | (3 << 21) | (2 << 23);
    let e = RuntimeFunction {
        BeginAddress: l.unw_func as u32,
        Anonymous:
            windows::Win32::System::Diagnostics::Debug::IMAGE_ARM64_RUNTIME_FUNCTION_ENTRY_0 {
                UnwindData: packed,
            },
    };
    let _ = ENTRY.set(e);
}

/// How unwind data for the JIT buffer is made known to the OS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Registration {
    None,
    Table,
    Callback,
    /// RtlAddGrowableFunctionTable: a sorted table the JIT can extend in
    /// place (RtlGrowFunctionTable) as it emits code.
    Growable,
}

static GROWABLE: AtomicUsize = AtomicUsize::new(0);

/// Callback-table identifier: low two bits must be set.
fn callback_id(base: usize) -> u64 {
    (base as u64) | 3
}

/// Registers unwind data. Only one registration is active at a time.
pub fn register(reg: Registration) {
    let j = get();
    let e: &'static RuntimeFunction = ENTRY.get().expect("entry");
    match reg {
        Registration::None => {}
        Registration::Table => {
            // SAFETY: the entry is a 'static value; base is our buffer.
            let ok = unsafe { table_add(e, j.base) };
            assert!(ok, "RtlAddFunctionTable failed");
        }
        Registration::Callback => {
            // SAFETY: the callback and its data live for the whole process.
            let ok = unsafe {
                windows::Win32::System::Diagnostics::Debug::RtlInstallFunctionTableCallback(
                    callback_id(j.base),
                    j.base as u64,
                    BUF_SIZE as u32,
                    Some(lookup_callback),
                    Some(e as *const RuntimeFunction as *const core::ffi::c_void),
                    windows::core::PCWSTR::null(),
                )
            };
            assert!(ok, "RtlInstallFunctionTableCallback failed");
        }
        Registration::Growable => {
            let mut h: *mut core::ffi::c_void = core::ptr::null_mut();
            // SAFETY: the entry is a 'static value; the range is our buffer.
            let st = unsafe {
                windows::Win32::System::Diagnostics::Debug::RtlAddGrowableFunctionTable(
                    &mut h,
                    core::slice::from_ref(e),
                    1,
                    j.base,
                    j.base + BUF_SIZE,
                )
            };
            assert!(st == 0, "RtlAddGrowableFunctionTable failed: {st:#x}");
            GROWABLE.store(h as usize, Ordering::Release);
        }
    }
}

pub fn unregister(reg: Registration) {
    let j = get();
    let e: &'static RuntimeFunction = ENTRY.get().expect("entry");
    // SAFETY: removes a registration made by `register` with the same key.
    unsafe {
        match reg {
            Registration::None => {}
            Registration::Table => {
                windows::Win32::System::Diagnostics::Debug::RtlDeleteFunctionTable(e);
            }
            Registration::Callback => {
                windows::Win32::System::Diagnostics::Debug::RtlDeleteFunctionTable(callback_id(
                    j.base,
                )
                    as *const RuntimeFunction);
            }
            Registration::Growable => {
                windows::Win32::System::Diagnostics::Debug::RtlDeleteGrowableFunctionTable(
                    GROWABLE.swap(0, Ordering::AcqRel) as *const core::ffi::c_void,
                );
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn table_add(e: &'static RuntimeFunction, base: usize) -> bool {
    // SAFETY: caller guarantees the entry outlives the registration.
    unsafe {
        windows::Win32::System::Diagnostics::Debug::RtlAddFunctionTable(
            core::slice::from_ref(e),
            base as u64,
        )
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn table_add(e: &'static RuntimeFunction, base: usize) -> bool {
    // SAFETY: caller guarantees the entry outlives the registration.
    unsafe {
        windows::Win32::System::Diagnostics::Debug::RtlAddFunctionTable(
            core::slice::from_ref(e),
            base,
        )
    }
}

/// Counts callback invocations (the OS calls it on every lookup in range).
pub static CALLBACK_HITS: AtomicUsize = AtomicUsize::new(0);

/// Lookup callback: a real JIT would find the method containing `pc` in
/// its code cache and synthesize the entry from the method header.
unsafe extern "system" fn lookup_callback(
    pc: u64,
    context: *const core::ffi::c_void,
) -> *mut RuntimeFunction {
    CALLBACK_HITS.fetch_add(1, Ordering::Relaxed);
    let j = get();
    let off = (pc as usize).wrapping_sub(j.base);
    if off >= j.l.unw_func && off < j.l.unw_func_end {
        context as *mut RuntimeFunction
    } else {
        core::ptr::null_mut()
    }
}

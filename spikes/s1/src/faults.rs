//! Fault delivery experiments: ART-style implicit null checks, implicit
//! stack overflow checks (protected region + probe), Windows' own stack
//! overflow (guard page + SetThreadStackGuarantee), SIGILL and SIGFPE, and
//! unwinding through JIT-like code.

use crate::linux_abi::*;
use crate::{arch, jit, signals, suspend, util};
use core::cell::Cell;
use core::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows::Win32::System::Diagnostics::Debug::{RtlCaptureStackBackTrace, CONTEXT};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RESERVE,
    PAGE_GUARD, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{GetCurrentThreadStackLimits, SetThreadStackGuarantee};

#[cfg(target_arch = "x86_64")]
pub type JitFn1 = unsafe extern "sysv64" fn(usize) -> u64;
#[cfg(target_arch = "x86_64")]
pub type JitFn2 = unsafe extern "sysv64-unwind" fn(usize, usize) -> u32;
#[cfg(target_arch = "aarch64")]
pub type JitFn1 = unsafe extern "C" fn(usize) -> u64;
#[cfg(target_arch = "aarch64")]
pub type JitFn2 = unsafe extern "C-unwind" fn(usize, usize) -> u32;
/// Windows-ABI callback called from JIT code.
pub type Callback = unsafe extern "system-unwind" fn(usize);

pub const TRAP_MARKER: u64 = 0x5160;
pub const UNW_MARKER: u64 = 0xDEAD;

thread_local! {
    /// [low, high) of the current thread's stack, captured before any
    /// alternate-stack switch changes the TEB bounds.
    static STACK_RANGE: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

pub static NPE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static SO_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static LAST_FAULT_SP: AtomicUsize = AtomicUsize::new(0);
pub static LAST_FAULT_ADDR: AtomicUsize = AtomicUsize::new(0);

fn jit1(off: usize) -> JitFn1 {
    // SAFETY: `off` is the offset of a System V function in the JIT buffer.
    unsafe { core::mem::transmute::<usize, JitFn1>(jit::get().at(off)) }
}
fn jit2(off: usize) -> JitFn2 {
    // SAFETY: as above.
    unsafe { core::mem::transmute::<usize, JitFn2>(jit::get().at(off)) }
}

pub fn capture_stack_range() {
    let (mut lo, mut hi) = (0usize, 0usize);
    // SAFETY: writes two usize out-parameters.
    unsafe { GetCurrentThreadStackLimits(&mut lo, &mut hi) };
    STACK_RANGE.with(|r| r.set((lo, hi)));
}

/// Allocates and registers a 64 KiB alternate signal stack for this thread.
pub fn setup_altstack() -> usize {
    const SZ: usize = 64 * 1024;
    // SAFETY: fresh private allocation, owned by the thread until exit.
    let p = unsafe { VirtualAlloc(None, SZ, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) } as usize;
    assert!(p != 0);
    signals::sigaltstack(p, SZ);
    p
}

/// The process-wide SA_SIGINFO handler for SIGSEGV, SIGILL and SIGFPE. It
/// plays the role of ART's fault manager: it claims faults it recognizes
/// and otherwise restores the default disposition (as libsigchain does when
/// no handler claims a signal), so the re-executed fault terminates.
pub unsafe extern "C" fn handler(sig: i32, info: *mut siginfo_t, ucv: *mut c_void) {
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the signal layer passes valid pointers for this call.
        let (info, uc) = unsafe { (&*info, &mut *(ucv as *mut arch::UContext)) };
        handle(sig, info, uc)
    }));
    if r.is_err() {
        std::process::abort();
    }
}

fn handle(sig: i32, info: &siginfo_t, uc: &mut arch::UContext) {
    let pc = arch::uc_pc(uc);
    let addr = info.si_addr as usize;
    if sig == SIGSEGV && suspend::is_suspend_page(addr) {
        suspend::checkpoint();
        return; // the load re-executes after the page is unprotected
    }
    if sig == SIGILL || sig == SIGFPE {
        if let Some(n) = arch::trap_insn_len(pc) {
            arch::uc_set_pc(uc, pc + n);
            arch::uc_set_ret(uc, TRAP_MARKER + sig as u64);
            return;
        }
    }
    if sig == SIGSEGV && jit::contains(pc) {
        let j = jit::get();
        let l = &j.l;
        LAST_FAULT_SP.store(arch::uc_sp(uc), Ordering::Relaxed);
        LAST_FAULT_ADDR.store(addr, Ordering::Relaxed);
        // Implicit null check: a known load site and a fault in page zero.
        if pc == j.at(l.null_site) && addr < 4096 {
            arch::uc_redirect_call(uc, j.at(l.null_load_after), j.at(l.npe_stub));
            NPE_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if pc >= j.at(l.unw_func) && pc < j.at(l.unw_func_end) && addr < 4096 {
            arch::uc_set_pc(uc, j.at(l.unw_after));
            arch::uc_set_ret(uc, UNW_MARKER);
            return;
        }
        // Stack overflow: the fault address lies in this thread's stack.
        let (lo, hi) = STACK_RANGE.with(|r| r.get());
        if addr >= lo && addr < hi {
            arch::uc_set_pc(uc, j.at(l.so_stub));
            SO_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    signals::sigaction(sig, None, 0);
}

pub fn install_handlers(onstack: bool) {
    let f = if onstack { SA_ONSTACK } else { 0 };
    signals::sigaction(SIGSEGV, Some(handler), f);
    signals::sigaction(SIGILL, Some(handler), f);
    signals::sigaction(SIGFPE, Some(handler), f);
}

// ---------------------------------------------------------------------------

pub struct NullCheckResult {
    pub correct: bool,
    pub baseline_ns: f64,
    pub fault_us: f64,
    pub fault_alt_us: f64,
    pub raw_us: f64,
    pub veh_stack_use: usize,
}

pub fn null_check() -> NullCheckResult {
    let j = jit::get();
    let f = jit1(j.l.null_load);
    let data: [u32; 4] = [0, 0, 42, 0];
    install_handlers(false);
    // SAFETY (calls below): the JIT function reads [arg + 8]; the argument is
    // either a valid 16-byte array or null (handled as an implicit null check).
    let ok_valid = unsafe { f(data.as_ptr() as usize) } == 42;
    let r = unsafe { f(0) };
    let ok_null = r == 0xFFFF_FFFF
        && jit::NPE_RETADDR.load(Ordering::Relaxed) as usize == j.at(j.l.null_load_after);
    let veh_stack_use = signals::LAST_VEH_STACK_USE.load(Ordering::Relaxed);

    let n_base = 2_000_000;
    let baseline_ns = util::time_ns(n_base, || unsafe { f(data.as_ptr() as usize) });
    let n = 100_000;
    let fault_us = util::time_ns(n, || unsafe { f(0) }) / 1000.0;
    install_handlers(true);
    let fault_alt_us = util::time_ns(n, || unsafe { f(0) }) / 1000.0;
    install_handlers(false);
    signals::RAW_RET.store(j.at(j.l.null_load_after), Ordering::Relaxed);
    signals::RAW_TARGET.store(j.at(j.l.npe_stub), Ordering::Relaxed);
    signals::RAW_PC.store(j.at(j.l.null_load), Ordering::Release);
    let raw_us = util::time_ns(n, || unsafe { f(0) }) / 1000.0;
    signals::RAW_PC.store(0, Ordering::Release);
    NullCheckResult {
        correct: ok_valid && ok_null,
        baseline_ns,
        fault_us,
        fault_alt_us,
        raw_us,
        veh_stack_use,
    }
}

pub fn traps() -> (u64, u64) {
    install_handlers(false);
    (arch::exec_ud2(), arch::exec_div0())
}

// ---------------------------------------------------------------------------

// Fields are reported through Debug.
#[allow(dead_code)]
#[derive(Default, Debug)]
pub struct SoArtResult {
    pub committed_kib: usize,
    pub reserved_kib: usize,
    pub depths: Vec<u64>,
    pub fault_sp_above_region: usize,
    pub veh_stack_use: usize,
    pub caught: usize,
}

/// Lowest committed, non-guard address of the current thread's stack.
fn lowest_committed(lo: usize, hi: usize) -> usize {
    let mut a = lo;
    while a < hi {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: VirtualQuery only reads the address space map.
        unsafe {
            VirtualQuery(
                Some(a as *const c_void),
                &mut mbi,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if mbi.State == MEM_COMMIT && (mbi.Protect & PAGE_GUARD).0 == 0 {
            return a;
        }
        a = mbi.BaseAddress as usize + mbi.RegionSize;
    }
    hi
}

/// ART-style implicit stack overflow check. Must run on a thread whose
/// stack is fully committed: the probe skips 8 KiB below SP, which would
/// jump over Windows' one-page guard on a growable stack.
pub fn so_art(onstack: bool) -> SoArtResult {
    capture_stack_range();
    let (lo, hi) = STACK_RANGE.with(|r| r.get());
    let low = lowest_committed(lo, hi);
    let prot_lo = low + 16 * 1024;
    let prot_hi = prot_lo + 16 * 1024;
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: the region is committed stack memory of this thread far below
    // the current SP; making it inaccessible is the protected region.
    unsafe {
        VirtualProtect(
            prot_lo as *const c_void,
            prot_hi - prot_lo,
            PAGE_NOACCESS,
            &mut old,
        )
    }
    .expect("protect stack region");
    if onstack {
        setup_altstack();
    }
    install_handlers(onstack);
    let mut res = SoArtResult {
        committed_kib: (hi - low) / 1024,
        reserved_kib: (hi - lo) / 1024,
        ..Default::default()
    };
    let before = SO_COUNT.load(Ordering::Relaxed);
    #[cfg(target_arch = "x86_64")]
    let f = jit1(jit::get().l.so_entry);
    // arm64: the probe gap is a parameter (S1_SO_GAP, default ART's 8 KiB).
    #[cfg(target_arch = "aarch64")]
    let f = {
        let gap: usize = std::env::var("S1_SO_GAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let g = jit2(jit::get().l.so_entry);
        // SAFETY: JIT recursion; overflow is caught by the handler.
        move |d: usize| unsafe { g(d, gap) } as u64
    };
    for _ in 0..3 {
        // SAFETY: JIT recursion; overflow is caught by the handler.
        #[allow(unused_unsafe)]
        res.depths.push(unsafe { f(0) });
    }
    res.caught = SO_COUNT.load(Ordering::Relaxed) - before;
    res.fault_sp_above_region = LAST_FAULT_SP
        .load(Ordering::Relaxed)
        .saturating_sub(prot_hi);
    res.veh_stack_use = signals::LAST_VEH_STACK_USE.load(Ordering::Relaxed);
    res
}

#[derive(Default, Debug)]
pub struct SoWinResult {
    pub depths: Vec<u64>,
    pub codes: Vec<u32>,
    pub veh_stack_use: usize,
}

/// Re-arms the guard pages after STATUS_STACK_OVERFLOW (what the CRT's
/// _resetstkoflw does): the lowest committed pages become guard pages again.
fn rearm_guard(guarantee: usize) {
    let (lo, hi) = STACK_RANGE.with(|r| r.get());
    let low = lowest_committed(lo, hi);
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: committed pages at the bottom of this thread's stack, far
    // below the current SP after the overflow was unwound.
    unsafe {
        VirtualProtect(
            low as *const c_void,
            guarantee + 4096,
            PAGE_READWRITE | PAGE_GUARD,
            &mut old,
        )
    }
    .expect("re-arm guard");
}

/// Windows' own stack overflow: guard page + SetThreadStackGuarantee.
pub fn so_windows(guarantee: u32) -> SoWinResult {
    capture_stack_range();
    let mut g = guarantee;
    // SAFETY: sets this thread's guarantee; `g` receives the previous value.
    unsafe { SetThreadStackGuarantee(&mut g) }.expect("SetThreadStackGuarantee");
    setup_altstack();
    install_handlers(true);
    let f = jit1(jit::get().l.wso_entry);
    let mut res = SoWinResult::default();
    for _ in 0..3 {
        signals::LAST_CODE.store(0, Ordering::Relaxed);
        // SAFETY: JIT recursion; overflow is caught by the handler.
        res.depths.push(unsafe { f(0) });
        res.codes.push(signals::LAST_CODE.load(Ordering::Relaxed));
        res.veh_stack_use = signals::LAST_VEH_STACK_USE.load(Ordering::Relaxed);
        rearm_guard(guarantee as usize);
    }
    res
}

// ---------------------------------------------------------------------------

static UNW_FOUND: AtomicUsize = AtomicUsize::new(0);
static UNW_PC: AtomicUsize = AtomicUsize::new(0);
static UNW_SP: AtomicUsize = AtomicUsize::new(0);
static BT_STATE: AtomicUsize = AtomicUsize::new(0);
static BT_MANUAL: AtomicUsize = AtomicUsize::new(0);

fn unwind_probe(ctx: &CONTEXT) {
    let j = jit::get();
    let pc = arch::pc(ctx);
    if pc >= j.at(j.l.unw_func) && pc < j.at(j.l.unw_func_end) {
        let (found, upc, usp) = arch::virtual_unwind(ctx);
        UNW_FOUND.store(found as usize, Ordering::Relaxed);
        UNW_PC.store(upc, Ordering::Relaxed);
        UNW_SP.store(usp, Ordering::Relaxed);
    }
}

/// Captures a stack trace from a callback called by JIT code. State:
/// 0 = no JIT frame in the trace, 1 = JIT frame but wrong caller,
/// 2 = JIT frame followed by the correct caller.
unsafe extern "system-unwind" fn backtrace_cb(_p: usize) {
    let mut frames = [core::ptr::null_mut::<c_void>(); 32];
    // SAFETY: fills the local array.
    let n = unsafe { RtlCaptureStackBackTrace(0, &mut frames, None) } as usize;
    let j = jit::get();
    let (ret, _) = arch::expected_caller(jit::ENTRY_SP.load(Ordering::Relaxed) as usize);
    let mut state = 0;
    for k in 0..n {
        let f = frames[k] as usize;
        if f >= j.at(j.l.unw_func) && f < j.at(j.l.unw_func_end) {
            state = if k + 1 < n && frames[k + 1] as usize == ret {
                2
            } else {
                1
            };
            break;
        }
    }
    BT_STATE.store(state, Ordering::Relaxed);
    let mut pcs = [0usize; 32];
    let m = arch::manual_walk(&mut pcs);
    let mut mstate = 0;
    for k in 0..m {
        if pcs[k] >= j.at(j.l.unw_func) && pcs[k] < j.at(j.l.unw_func_end) {
            mstate = if k + 1 < m && pcs[k + 1] == ret { 2 } else { 1 };
            break;
        }
    }
    BT_MANUAL.store(mstate, Ordering::Relaxed);
    if std::env::var_os("S1_DEBUG_BT").is_some() {
        eprintln!(
            "bt: n={n} jit=[{:#x},{:#x}) expected ret {ret:#x} frames {:x?}",
            j.at(j.l.unw_func),
            j.at(j.l.unw_func_end),
            &frames[..n]
        );
    }
}

unsafe extern "system-unwind" fn panic_cb(_p: usize) {
    panic!("panic thrown through JIT frames");
}

// Fields are reported through Debug.
#[allow(dead_code)]
#[derive(Debug)]
pub struct UnwindResult {
    pub reg: jit::Registration,
    pub fault_entry_found: bool,
    pub fault_unwind_correct: bool,
    /// RtlCaptureStackBackTrace: 0 = no JIT frame, 1 = walk stops or goes
    /// wrong after the JIT frame, 2 = correct caller after the JIT frame.
    pub backtrace_os: usize,
    /// Same, walking with RtlLookupFunctionEntry + RtlVirtualUnwind.
    pub backtrace_manual: usize,
}

pub fn unwind(reg: jit::Registration) -> UnwindResult {
    install_handlers(false);
    jit::register(reg);
    let j = jit::get();
    let f = jit2(j.l.unw_func);
    signals::CONTEXT_PROBE.store(
        unwind_probe as signals::ContextProbe as usize,
        Ordering::Release,
    );
    // SAFETY: JIT function; the null load is handled by the SIGSEGV handler.
    let r = unsafe { f(0, 0) };
    signals::CONTEXT_PROBE.store(0, Ordering::Release);
    let (exp_pc, exp_sp) = arch::expected_caller(jit::ENTRY_SP.load(Ordering::Relaxed) as usize);
    let fault_unwind_correct = r as u64 == UNW_MARKER
        && UNW_PC.load(Ordering::Relaxed) == exp_pc
        && UNW_SP.load(Ordering::Relaxed) == exp_sp;
    let data = [7u32; 4];
    BT_STATE.store(usize::MAX, Ordering::Relaxed);
    // SAFETY: JIT function with a valid pointer and a Windows-ABI callback.
    unsafe { f(data.as_ptr() as usize, backtrace_cb as Callback as usize) };
    let res = UnwindResult {
        reg,
        fault_entry_found: UNW_FOUND.load(Ordering::Relaxed) != 0,
        fault_unwind_correct,
        backtrace_os: BT_STATE.load(Ordering::Relaxed),
        backtrace_manual: BT_MANUAL.load(Ordering::Relaxed),
    };
    jit::unregister(reg);
    res
}

/// Child-process body: panic from a callback through a JIT frame and catch
/// it in the Rust caller. Exit 0 = caught; an abort means unwinding failed.
pub fn child_panic(reg: jit::Registration) -> i32 {
    std::panic::set_hook(Box::new(|_| {}));
    jit::register(reg);
    let j = jit::get();
    let f = jit2(j.l.unw_func);
    let data = [7u32; 4];
    let r = catch_unwind(|| {
        // SAFETY: JIT function with a valid pointer; the callback panics and
        // the panic unwinds through the JIT frame.
        unsafe { f(data.as_ptr() as usize, panic_cb as Callback as usize) }
    });
    if r.is_err() {
        0
    } else {
        3
    }
}

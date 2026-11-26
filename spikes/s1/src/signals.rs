//! Synchronous signal delivery from a Vectored Exception Handler.
//!
//! A hardware exception is mapped to a Linux signal, the Windows CONTEXT is
//! converted into a Linux `ucontext_t`, the registered `SA_SIGINFO` handler
//! runs (optionally on an alternate stack), and the possibly modified
//! ucontext is written back before execution resumes. The handler's return
//! means "resume at the context", exactly as `rt_sigreturn` does on Linux.

use crate::arch;
use crate::linux_abi::*;
use core::cell::Cell;
use core::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS,
};
use windows::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};

pub type SigActionFn = unsafe extern "C" fn(i32, *mut siginfo_t, *mut c_void);

const NSIG: usize = 65;
const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

const STATUS_ACCESS_VIOLATION: i32 = 0xC000_0005_u32 as i32;
const STATUS_IN_PAGE_ERROR: i32 = 0xC000_0006_u32 as i32;
const STATUS_GUARD_PAGE_VIOLATION: i32 = 0x8000_0001_u32 as i32;
const STATUS_STACK_OVERFLOW: i32 = 0xC000_00FD_u32 as i32;
const STATUS_ILLEGAL_INSTRUCTION: i32 = 0xC000_001D_u32 as i32;
const STATUS_PRIVILEGED_INSTRUCTION: i32 = 0xC000_0096_u32 as i32;
const STATUS_INTEGER_DIVIDE_BY_ZERO: i32 = 0xC000_0094_u32 as i32;
const STATUS_INTEGER_OVERFLOW: i32 = 0xC000_0095_u32 as i32;

static HANDLERS: [AtomicUsize; NSIG] = [const { AtomicUsize::new(0) }; NSIG];
static FLAGS: [AtomicI32; NSIG] = [const { AtomicI32::new(0) }; NSIG];

/// Bytes of the faulting thread's stack used between the fault SP and the
/// VEH frame (kernel exception frame + ntdll dispatcher), last fault.
pub static LAST_VEH_STACK_USE: AtomicUsize = AtomicUsize::new(0);
/// Last NTSTATUS seen by the VEH and mapped to a signal.
pub static LAST_CODE: AtomicU32 = AtomicU32::new(0);
/// Number of signals delivered.
pub static DELIVERED: AtomicUsize = AtomicUsize::new(0);

/// Benchmark hook: when set, a fault at `RAW_PC` is redirected directly in
/// the VEH (no ucontext conversion) to `RAW_TARGET` with return address
/// `RAW_RET`. Measures the cost of Windows exception dispatch alone.
pub static RAW_PC: AtomicUsize = AtomicUsize::new(0);
pub static RAW_RET: AtomicUsize = AtomicUsize::new(0);
pub static RAW_TARGET: AtomicUsize = AtomicUsize::new(0);

/// Test hook: called with the raw Windows CONTEXT before delivery.
pub static CONTEXT_PROBE: AtomicUsize = AtomicUsize::new(0);
pub type ContextProbe = fn(&CONTEXT);

thread_local! {
    /// The calling thread's alternate signal stack (base, size), as set by
    /// `sigaltstack`. Const-initialized: no allocation on first use in the VEH.
    static ALTSTACK: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// Installs the VEH first in the chain. Returns false on failure.
pub fn install() -> bool {
    // SAFETY: `veh` has the PVECTORED_EXCEPTION_HANDLER signature and lives
    // for the whole process.
    let h = unsafe { AddVectoredExceptionHandler(1, Some(veh)) };
    !h.is_null()
}

/// `sigaction` subset: installs an SA_SIGINFO handler (or 0 for SIG_DFL).
pub fn sigaction(sig: i32, handler: Option<SigActionFn>, flags: i32) {
    let i = sig as usize;
    FLAGS[i].store(flags | SA_SIGINFO, Ordering::Release);
    HANDLERS[i].store(handler.map_or(0, |f| f as usize), Ordering::Release);
}

/// `sigaltstack` subset for the calling thread.
pub fn sigaltstack(base: usize, size: usize) {
    ALTSTACK.with(|a| a.set((base, size)));
}

unsafe extern "system" fn veh(ep: *mut EXCEPTION_POINTERS) -> i32 {
    // Never unwind out of the VEH: a panic here would cross the OS boundary.
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the OS passes valid exception and context records that
        // stay valid until the handler returns.
        unsafe { veh_inner(&mut *ep) }
    })) {
        Ok(v) => v,
        Err(_) => std::process::abort(),
    }
}

struct Delivery {
    ctx: *mut CONTEXT,
    info: siginfo_t,
    fault: Option<(usize, bool, bool)>,
    handler: usize,
    alt: stack_t,
}

unsafe fn veh_inner(ep: &mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY (whole function): the pointers come from the OS and are valid.
    let rec = unsafe { &*ep.ExceptionRecord };
    let ctx = unsafe { &mut *ep.ContextRecord };
    let marker = 0u8;
    LAST_VEH_STACK_USE.store(
        arch::sp(ctx).wrapping_sub(&marker as *const u8 as usize),
        Ordering::Relaxed,
    );

    let NTSTATUS(code) = rec.ExceptionCode;
    let pc = arch::pc(ctx);
    let mem_addr = || {
        if rec.NumberParameters >= 2 {
            (
                rec.ExceptionInformation[1],
                rec.ExceptionInformation[0] == 1,
            )
        } else {
            (arch::sp(ctx), true)
        }
    };
    let (sig, si_code, addr, is_mem) = match code {
        STATUS_ACCESS_VIOLATION | STATUS_GUARD_PAGE_VIOLATION | STATUS_STACK_OVERFLOW => {
            (SIGSEGV, 0, mem_addr().0, true)
        }
        STATUS_IN_PAGE_ERROR => (SIGBUS, 2 /* BUS_ADRERR */, mem_addr().0, false),
        STATUS_ILLEGAL_INSTRUCTION | STATUS_PRIVILEGED_INSTRUCTION => {
            (SIGILL, ILL_ILLOPC, pc, false)
        }
        STATUS_INTEGER_DIVIDE_BY_ZERO => (SIGFPE, FPE_INTDIV, pc, false),
        STATUS_INTEGER_OVERFLOW => (SIGFPE, FPE_INTOVF, pc, false),
        _ => return EXCEPTION_CONTINUE_SEARCH,
    };

    // Benchmark path: Windows dispatch cost only.
    let raw = RAW_PC.load(Ordering::Relaxed);
    if raw != 0 && raw == pc {
        arch::redirect_call(
            ctx,
            RAW_RET.load(Ordering::Relaxed),
            RAW_TARGET.load(Ordering::Relaxed),
        );
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    let handler = HANDLERS[sig as usize].load(Ordering::Acquire);
    if handler == 0 {
        return EXCEPTION_CONTINUE_SEARCH; // SIG_DFL: let Windows terminate.
    }
    LAST_CODE.store(code as u32, Ordering::Relaxed);

    let probe = CONTEXT_PROBE.load(Ordering::Acquire);
    if probe != 0 {
        // SAFETY: only `ContextProbe` values are stored in CONTEXT_PROBE.
        let f: ContextProbe = unsafe { core::mem::transmute::<usize, ContextProbe>(probe) };
        f(ctx);
    }

    let fault = if is_mem {
        let (a, write) = mem_addr();
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: VirtualQuery only reads the address space map.
        let n = unsafe {
            VirtualQuery(
                Some(a as *const c_void),
                &mut mbi,
                core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let present = n != 0 && mbi.State == MEM_COMMIT;
        Some((a, write, present))
    } else {
        None
    };
    let mut info = siginfo_t::zeroed();
    info.si_signo = sig;
    info.si_code = match fault {
        Some((_, _, true)) => SEGV_ACCERR,
        Some((_, _, false)) => SEGV_MAPERR,
        None => si_code,
    };
    info.si_addr = addr as *mut c_void;

    let flags = FLAGS[sig as usize].load(Ordering::Acquire);
    let (alt_base, alt_size) = ALTSTACK.with(|a| a.get());
    let sp_now = &marker as *const u8 as usize;
    let on_alt = alt_size != 0 && sp_now >= alt_base && sp_now < alt_base + alt_size;
    let use_alt = flags & SA_ONSTACK != 0 && alt_size != 0 && !on_alt;
    let alt = stack_t {
        ss_sp: alt_base as *mut c_void,
        ss_flags: if alt_size == 0 {
            SS_DISABLE
        } else if on_alt {
            SS_ONSTACK
        } else {
            0
        },
        ss_size: alt_size,
    };
    let mut d = Delivery {
        ctx,
        info,
        fault,
        handler,
        alt,
    };
    let dp = &mut d as *mut Delivery as *mut c_void;
    if use_alt {
        let top = (alt_base + alt_size) & !15;
        // SAFETY: the alternate stack is a committed, thread-owned region
        // registered with sigaltstack; `deliver_here` matches the callee type.
        unsafe { arch::s1_call_on_stack(dp, 0, deliver_here, top, alt_base) };
    } else {
        // SAFETY: `dp` points to a live Delivery on this frame.
        unsafe { deliver_here(dp, 0) };
    }
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    EXCEPTION_CONTINUE_EXECUTION
}

/// Builds siginfo/ucontext on the current stack, runs the handler, writes
/// the context back. Exported with the C ABI because it is entered from the
/// stack-switch assembly; never unwinds.
unsafe extern "C" fn deliver_here(p: *mut c_void, _unused: usize) {
    // SAFETY: `p` is the Delivery passed by veh_inner, alive for this call.
    let d = unsafe { &mut *(p as *mut Delivery) };
    // SAFETY: ucontext_t is plain data; all-zero is a valid value.
    let mut uc: arch::UContext = unsafe { core::mem::zeroed() };
    // SAFETY: d.ctx is the OS context record, valid during dispatch.
    arch::context_to_ucontext(unsafe { &*d.ctx }, &d.info, d.fault, d.alt, &mut uc);
    // SAFETY: only SigActionFn values are stored in HANDLERS.
    let h: SigActionFn = unsafe { core::mem::transmute::<usize, SigActionFn>(d.handler) };
    let sig = d.info.si_signo;
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the handler gets valid siginfo and ucontext pointers.
        unsafe {
            h(
                sig,
                &mut d.info,
                &mut uc as *mut arch::UContext as *mut c_void,
            )
        }
    }));
    if r.is_err() {
        std::process::abort();
    }
    // SAFETY: as above.
    arch::ucontext_to_context(&uc, unsafe { &mut *d.ctx });
}

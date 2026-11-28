//! Signal frames: the exception hook, the Linux frame written for a
//! handler, and `rt_sigreturn`.
//!
//! A handler is started by changing the Windows exception context: the
//! stack pointer moves to the new frame, the program counter to the
//! handler, and Windows resumes the thread there when the vectored handler
//! returns. The exception's own context record (which also holds state a
//! `ucontext_t` cannot, such as the upper halves of AVX registers) stays
//! where Windows put it, below the interrupted stack pointer, and its
//! address is kept in a hidden part of the frame; `rt_sigreturn` applies
//! the `ucontext_t` to it and resumes it with `RtlRestoreContext`.
//!
//! On the interrupted stack the frame is written 4 KiB below the vectored
//! handler's own frames, so that neither the exception record nor lx's
//! frames are overwritten. On the alternate stack it starts at the top.
//!
//! While a handler runs on the alternate stack, the TEB's stack bounds
//! cover both stacks (so that Windows code called from the handler accepts
//! its stack pointer); `rt_sigreturn` restores them. A handler that leaves
//! with `siglongjmp` keeps the widened bounds, which still contain the
//! stack it returns to.

use core::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, RaiseException, RtlCaptureContext,
    RtlRestoreContext,
};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_IMAGE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY,
    PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

use super::{action, current, reset_action};
use crate::linux::signal::*;

/// The exception code lx raises to run a handler for a queued signal
/// ("customer" bit set, so it never collides with a Windows status).
pub const LX_SIGNAL: u32 = 0xE04C_5853;

const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_IN_PAGE_ERROR: u32 = 0xC000_0006;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xC000_0096;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
const STATUS_INTEGER_OVERFLOW: u32 = 0xC000_0095;
const STATUS_STACK_OVERFLOW: u32 = 0xC000_00FD;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;

/// Distance kept between the vectored handler's frame and a signal frame
/// written on the same stack.
const SAME_STACK_GAP: u64 = 4096;

#[cfg(target_arch = "x86_64")]
const RED_ZONE: u64 = 128;
#[cfg(target_arch = "aarch64")]
const RED_ZONE: u64 = 0;

thread_local! {
    /// Set while lx writes a frame: a fault then is fatal, not a signal.
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
    /// Consecutive retried faults on one page with no `MADV_DONTNEED`
    /// window closed in between: (page, windows closed, count), see `retry`.
    static RETRIES: Cell<(u64, u64, u32)> = const { Cell::new((0, 0, 0)) };
}

/// Statistics for tests and diagnostics.
pub static DELIVERED: AtomicU64 = AtomicU64::new(0);

/// Hidden data after the Linux part of a frame.
#[repr(C)]
#[derive(Clone, Copy)]
struct Hidden {
    magic: u64,
    context: u64,
    stack_base: u64,
    stack_limit: u64,
    alt_sp: u64,
    alt_size: u64,
    alt_flags: u64,
    /// 1 if `SS_AUTODISARM` disabled the alternate stack for the handler.
    disarmed: u64,
}

/// A per-process secret mixed into the frame magic.
fn cookie() -> u64 {
    static C: AtomicU64 = AtomicU64::new(0);
    let c = C.load(Ordering::Relaxed);
    if c != 0 {
        return c;
    }
    let v = (crate::time::monotonic_ns() as u64 ^ (&C as *const AtomicU64 as u64)) | 1;
    match C.compare_exchange(0, v, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => v,
        Err(x) => x,
    }
}

fn magic_for(uc: u64) -> u64 {
    cookie() ^ uc.rotate_left(17) ^ 0x4c58_5349_474e_414c
}

// ---- TEB stack bounds --------------------------------------------------------

fn teb() -> *mut u64 {
    let t: *mut u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: gs:[0x30] is the TEB's self pointer on x64 Windows.
    unsafe {
        core::arch::asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly, preserves_flags))
    };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: x18 holds the TEB in user mode on Windows ARM64.
    unsafe {
        core::arch::asm!("mov {}, x18", out(reg) t, options(nomem, nostack, preserves_flags))
    };
    t
}

/// The TEB's `NT_TIB` stack base and limit.
pub(super) fn stack_bounds() -> (u64, u64) {
    let t = teb();
    // SAFETY: NT_TIB.StackBase and StackLimit are the second and third
    // words of the calling thread's TEB.
    unsafe { (t.add(1).read_volatile(), t.add(2).read_volatile()) }
}

pub(super) fn set_stack_bounds(base: u64, limit: u64) {
    let t = teb();
    // SAFETY: as in `stack_bounds`; a thread writes only its own TEB.
    unsafe {
        t.add(1).write_volatile(base);
        t.add(2).write_volatile(limit);
    }
}

// ---- the exception hook ------------------------------------------------------

static INSTALL: Once = Once::new();

/// Installs a first vectored exception handler that calls
/// [`handle_exception`], for hosts that have no handler of their own.
pub fn install_handler() {
    INSTALL.call_once(|| {
        // SAFETY: a valid handler that is never removed.
        let h = unsafe { AddVectoredExceptionHandler(1, Some(veh)) };
        assert!(!h.is_null(), "AddVectoredExceptionHandler failed");
    });
}

unsafe extern "system" fn veh(ep: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid records.
    if unsafe { handle_exception(ep) } {
        -1 // EXCEPTION_CONTINUE_EXECUTION
    } else {
        0 // EXCEPTION_CONTINUE_SEARCH
    }
}

/// Turns a Windows exception into a Linux signal for the calling thread.
/// Returns true when the thread should continue at the (changed) context:
/// a handler frame was set up, or a transient fault is retried. Returns
/// false when lx does not handle the exception (no handler, the signal is
/// blocked, or the exception has no signal), so that the next handler or
/// Windows' default handling (the end of the process) follows.
///
/// # Safety
///
/// `ep` is the exception information Windows passed to a vectored
/// exception handler, and the call is made from that handler.
pub unsafe fn handle_exception(ep: *mut EXCEPTION_POINTERS) -> bool {
    if IN_FRAME.with(Cell::get) {
        return false;
    }
    // SAFETY: forwarded from this function's contract. Panics never leave
    // the handler.
    catch_unwind(AssertUnwindSafe(|| unsafe { handle(ep) })).unwrap_or(false)
}

/// What a Windows exception means as a signal.
struct Fault {
    sig: i32,
    code: i32,
    addr: u64,
    /// For memory faults: (write, page present).
    mem: Option<(bool, bool)>,
}

/// # Safety
///
/// As for [`handle_exception`].
unsafe fn handle(ep: *mut EXCEPTION_POINTERS) -> bool {
    // SAFETY: the system passes valid records for the handler's duration.
    let (rec, ctx) = unsafe { (&*(*ep).ExceptionRecord, &mut *(*ep).ContextRecord) };
    let code = rec.ExceptionCode.0 as u32;
    if code == LX_SIGNAL && rec.NumberParameters == 1 {
        // SAFETY: `raise` passes the address of a live `Delivery`.
        let d = unsafe { &*(rec.ExceptionInformation[0] as *const Delivery) };
        let act = action(d.info.si_signo);
        if act.handler == SIG_DFL || act.handler == SIG_IGN {
            // The action changed after the signal was taken: nothing to run.
            return true;
        }
        // SAFETY: `ctx` is the exception context (contract).
        return unsafe { setup(ctx, &d.info, act, d.saved_mask, None) };
    }
    let pc = arch::pc(ctx);
    let mem_info = || {
        if rec.NumberParameters >= 2 {
            (
                rec.ExceptionInformation[1] as u64,
                rec.ExceptionInformation[0] == 1,
            )
        } else {
            (arch::sp(ctx), true)
        }
    };
    let fault = match code {
        STATUS_ACCESS_VIOLATION | STATUS_STACK_OVERFLOW => {
            let (addr, write) = mem_info();
            if code == STATUS_ACCESS_VIOLATION && retry(addr, write) {
                return true;
            }
            match crate::mem::fault_kind(addr as usize) {
                // A page of a file mapping past the end of the file.
                Some(crate::mem::FaultKind::PastEof) => Fault {
                    sig: SIGBUS,
                    code: BUS_ADRERR,
                    addr,
                    mem: None,
                },
                kind => {
                    let present = match kind {
                        Some(k) => k == crate::mem::FaultKind::Mapped,
                        None => mapped(addr),
                    };
                    Fault {
                        sig: SIGSEGV,
                        code: if present { SEGV_ACCERR } else { SEGV_MAPERR },
                        addr,
                        mem: Some((write, present)),
                    }
                }
            }
        }
        STATUS_IN_PAGE_ERROR => Fault {
            sig: SIGBUS,
            code: BUS_ADRERR,
            addr: mem_info().0,
            mem: None,
        },
        STATUS_ILLEGAL_INSTRUCTION => Fault {
            sig: SIGILL,
            code: ILL_ILLOPC,
            addr: pc,
            mem: None,
        },
        STATUS_PRIVILEGED_INSTRUCTION => Fault {
            sig: SIGILL,
            code: ILL_PRVOPC,
            addr: pc,
            mem: None,
        },
        STATUS_INTEGER_DIVIDE_BY_ZERO => Fault {
            sig: SIGFPE,
            code: FPE_INTDIV,
            addr: pc,
            mem: None,
        },
        STATUS_INTEGER_OVERFLOW => Fault {
            sig: SIGFPE,
            code: FPE_INTOVF,
            addr: pc,
            mem: None,
        },
        STATUS_BREAKPOINT => Fault {
            sig: SIGTRAP,
            code: TRAP_BRKPT,
            addr: pc,
            mem: None,
        },
        _ => return false,
    };
    let act = action(fault.sig);
    let t = current();
    let mask = t.blocked();
    if act.handler == SIG_DFL || act.handler == SIG_IGN || mask & sigbit(fault.sig) != 0 {
        // Linux forces the default action for an ignored or blocked fault
        // too: the process ends. Faults in Windows images (host code) are
        // Windows' to handle (structured exception handlers may expect
        // them), so they go on to the next handler.
        if host_code(pc) {
            return false;
        }
        super::terminate_after(fault.sig, ep);
    }
    let info = SigInfo::fault(fault.sig, fault.code, fault.addr);
    let mem = fault.mem.map(|(w, p)| (fault.addr, w, p));
    // SAFETY: `ctx` is the exception context (contract).
    unsafe { setup(ctx, &info, act, mask, mem) }
}

/// Whether `pc` lies in a Windows image (the host executable or a DLL):
/// host code. Guest code runs from lx's private memory or file views.
fn host_code(pc: u64) -> bool {
    crate::sys::mem::query(pc as usize).is_some_and(|r| r.kind == MEM_IMAGE)
}

/// An access violation on a page that is accessible now was a transient
/// state (lx's `madvise(MADV_DONTNEED)` decommits and recommits pages):
/// retried. A fault while such a window is open waits for it to close. The
/// same page faulting again with no window closed in between is not
/// transient: after a few tries it is a real fault.
fn retry(addr: u64, write: bool) -> bool {
    use crate::mem::{DISCARDS_ACTIVE, DISCARDS_DONE};
    // A window is a decommit and a commit: short. The bound only guards
    // against waiting forever on a window that never closes.
    for _ in 0..100_000 {
        let done = DISCARDS_DONE.load(Ordering::SeqCst);
        if accessible(addr, write) {
            let page = addr & !0xfff;
            return RETRIES.with(|c| {
                let (a, d, n) = c.get();
                let n = if a == page && d == done { n + 1 } else { 1 };
                c.set((page, done, n));
                n <= 4
            });
        }
        if DISCARDS_ACTIVE.load(Ordering::SeqCst) == 0
            && DISCARDS_DONE.load(Ordering::SeqCst) == done
        {
            return false;
        }
        std::thread::yield_now();
    }
    false
}

/// Whether the page of `addr` is committed with a protection that allows
/// the access.
fn accessible(addr: u64, write: bool) -> bool {
    let Some(r) = crate::sys::mem::query(addr as usize) else {
        return false;
    };
    if r.state != MEM_COMMIT || r.protect.0 & PAGE_GUARD.0 != 0 {
        return false;
    }
    let p = r.protect.0 & 0xff;
    let readable = [
        PAGE_READONLY.0,
        PAGE_READWRITE.0,
        PAGE_WRITECOPY.0,
        PAGE_EXECUTE_READ.0,
        PAGE_EXECUTE_READWRITE.0,
        PAGE_EXECUTE_WRITECOPY.0,
    ]
    .contains(&p);
    let writable = [
        PAGE_READWRITE.0,
        PAGE_WRITECOPY.0,
        PAGE_EXECUTE_READWRITE.0,
        PAGE_EXECUTE_WRITECOPY.0,
    ]
    .contains(&p);
    if write { writable } else { readable }
}

/// Whether `addr`, outside lx's table (or while the table is busy), is
/// mapped (`SEGV_ACCERR`) rather than unmapped (`SEGV_MAPERR`).
fn mapped(addr: u64) -> bool {
    matches!(crate::sys::mem::query(addr as usize), Some(r) if r.state == MEM_COMMIT)
}

// ---- queued signals ----------------------------------------------------------

struct Delivery {
    info: SigInfo,
    saved_mask: SigSet,
}

/// Runs the handler for `info` on the calling thread, as if interrupted
/// here: raises [`LX_SIGNAL`], whose exception context becomes the
/// interrupted context of the frame. Returns after the handler returned.
pub(super) fn raise(info: &SigInfo, saved_mask: SigSet) {
    let d = Delivery {
        info: *info,
        saved_mask,
    };
    let args = [&d as *const Delivery as usize];
    // SAFETY: lx's exception hook handles LX_SIGNAL and reads `d`, which
    // outlives the call.
    unsafe { RaiseException(LX_SIGNAL, 0, Some(&args)) };
}

// ---- frame set-up --------------------------------------------------------------

/// Writes the frame for `info` and points `ctx` at the handler. Returns
/// false if the frame cannot be placed (the alternate stack is too small).
///
/// # Safety
///
/// `ctx` is the context record of the exception being handled on this
/// thread, which stays in place until the thread resumes.
#[inline(never)]
unsafe fn setup(
    ctx: &mut CONTEXT,
    info: &SigInfo,
    act: KernelSigaction,
    saved_mask: SigSet,
    fault: Option<(u64, bool, bool)>,
) -> bool {
    let marker = 0u8;
    let here = &marker as *const u8 as u64;
    let t = current();
    let sp = arch::sp(ctx);
    let (alt_sp, alt_size, alt_flags) = t.alt();
    let alt_on = alt_flags & SS_DISABLE == 0 && alt_size != 0;
    let on_alt = alt_on && sp >= alt_sp && sp < alt_sp + alt_size;
    let use_alt = act.flags & SA_ONSTACK != 0 && alt_on && !on_alt;
    let top = if use_alt {
        alt_sp + alt_size
    } else {
        let mut top = sp - RED_ZONE;
        if here < sp {
            top = top.min(here - SAME_STACK_GAP);
        }
        top
    };
    let (frame_sp, uc) = arch::layout(top);
    let frame_low = frame_sp;
    if use_alt && frame_low < alt_sp {
        return false;
    }
    if !use_alt {
        // Touch the pages down to the frame in order, so that a Windows
        // stack grows through its guard page instead of being jumped over.
        let mut p = here & !0xfff;
        while p > frame_low {
            p -= 0x1000;
            // SAFETY: stack memory below the current frame; reading it
            // has no effect except committing a guard page.
            unsafe { core::ptr::read_volatile(p.max(frame_low) as *const u8) };
        }
    }

    let (base, limit) = stack_bounds();
    let stack = StackT {
        ss_sp: alt_sp,
        ss_flags: if !alt_on {
            SS_DISABLE
        } else if on_alt {
            SS_ONSTACK
        } else {
            alt_flags & SS_AUTODISARM
        },
        _pad: 0,
        ss_size: alt_size,
    };
    let disarm = use_alt && alt_flags & SS_AUTODISARM != 0;
    let hidden = Hidden {
        magic: magic_for(uc),
        context: ctx as *mut CONTEXT as u64,
        stack_base: base,
        stack_limit: limit,
        alt_sp,
        alt_size,
        alt_flags: alt_flags as u64,
        disarmed: u64::from(disarm),
    };
    IN_FRAME.with(|c| c.set(true));
    // SAFETY: the frame lies on the chosen stack below `top`, inside the
    // alternate stack (checked) or below lx's own frames on the current
    // one; nothing else uses that memory now.
    unsafe {
        arch::write_frame(ctx, frame_sp, uc, info, &stack, saved_mask, fault, &hidden);
        let restorer = if act.flags & SA_RESTORER != 0 && act.restorer != 0 {
            act.restorer
        } else {
            restorer as usize as u64
        };
        arch::enter_handler(ctx, frame_sp, uc, info.si_signo, act.handler, restorer);
    }
    IN_FRAME.with(|c| c.set(false));

    if use_alt {
        set_stack_bounds(base.max(alt_sp + alt_size), limit.min(alt_sp));
    }
    if disarm {
        t.set_alt(0, 0, SS_DISABLE);
    }
    let mut mask = saved_mask | act.mask;
    if act.flags & SA_NODEFER == 0 {
        mask |= sigbit(info.si_signo);
    }
    t.set_blocked(mask);
    if act.flags & SA_RESETHAND != 0 {
        reset_action(info.si_signo);
    }
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    true
}

// ---- rt_sigreturn ------------------------------------------------------------

/// `rt_sigreturn` with the address of the frame's `ucontext_t`: resumes
/// the context it holds. Returns only if `uc` is NULL.
///
/// # Safety
///
/// `uc` is the `ucontext_t` of a signal frame of the calling thread, as
/// the restorer passes it.
pub unsafe fn sigreturn(uc: u64) -> crate::errno::Errno {
    if uc == 0 {
        return crate::errno::Errno::EFAULT;
    }
    let t = current();
    // SAFETY: `uc` is a frame's ucontext (contract); the hidden part
    // follows it.
    let h = unsafe { ((uc + arch::HIDDEN_OFFSET) as *const Hidden).read() };
    let valid = h.magic == magic_for(uc) && h.context != 0;
    let mut fresh = CONTEXT::default();
    let ctx: &mut CONTEXT = if valid {
        // SAFETY: the exception context record the frame was made from,
        // still in place below the interrupted stack pointer.
        unsafe { &mut *(h.context as *mut CONTEXT) }
    } else {
        // SAFETY: fills a local context record.
        unsafe { RtlCaptureContext(&mut fresh) };
        &mut fresh
    };
    // SAFETY: `uc` is readable (contract).
    let mask = unsafe { arch::read_ucontext(uc, ctx) };
    t.set_blocked(mask);
    if valid {
        if h.disarmed != 0 {
            t.set_alt(h.alt_sp, h.alt_size, h.alt_flags as i32);
        }
        set_stack_bounds(h.stack_base, h.stack_limit);
    }
    // SAFETY: a complete context for this thread.
    unsafe { RtlRestoreContext(ctx, None) };
    unreachable!("RtlRestoreContext returned")
}

/// The restorer lx uses when the action has none: `rt_sigreturn` with the
/// frame's `ucontext_t`.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "sysv64" fn restorer() {
    // After the handler's `ret`, rsp points at the ucontext and is 16-byte
    // aligned.
    core::arch::naked_asm!("mov rdi, rsp", "call {f}", "ud2", f = sym restorer_entry)
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn restorer() {
    // sp points at the frame: siginfo, then the ucontext.
    core::arch::naked_asm!("add x0, sp, #128", "bl {f}", "brk #0x3e8", f = sym restorer_entry)
}

#[cfg(target_arch = "x86_64")]
unsafe extern "sysv64" fn restorer_entry(uc: u64) -> ! {
    // SAFETY: called by `restorer` with its frame's ucontext.
    let _ = unsafe { sigreturn(uc) };
    super::terminate(SIGSEGV)
}

#[cfg(target_arch = "aarch64")]
unsafe extern "C" fn restorer_entry(uc: u64) -> ! {
    // SAFETY: called by `restorer` with its frame's ucontext.
    let _ = unsafe { sigreturn(uc) };
    super::terminate(SIGSEGV)
}

// ---- architecture parts ------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::Hidden;
    use crate::linux::signal::x86_64::*;
    use crate::linux::signal::{SIGFPE, SIGILL, SIGSEGV, SigInfo, SigSet, StackT};
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    const UC_SIZE: u64 = core::mem::size_of::<UContext>() as u64;
    /// From the ucontext to the hidden data: ucontext, siginfo.
    pub const HIDDEN_OFFSET: u64 = UC_SIZE + 128;
    /// pretcode, ucontext, siginfo, hidden.
    const FRAME: u64 = 8 + HIDDEN_OFFSET + core::mem::size_of::<Hidden>() as u64;

    pub fn pc(c: &CONTEXT) -> u64 {
        c.Rip
    }

    pub fn sp(c: &CONTEXT) -> u64 {
        c.Rsp
    }

    /// The frame below `top`: (stack pointer at handler entry, ucontext).
    /// At entry `rsp + 8` is 16-byte aligned, as after a call.
    pub fn layout(top: u64) -> (u64, u64) {
        let uc = (top - FRAME + 8) & !15;
        (uc - 8, uc)
    }

    /// # Safety
    ///
    /// `[sp, sp + FRAME)` is writable and unused.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn write_frame(
        c: &CONTEXT,
        sp: u64,
        uc: u64,
        info: &SigInfo,
        stack: &StackT,
        mask: SigSet,
        fault: Option<(u64, bool, bool)>,
        hidden: &Hidden,
    ) {
        // SAFETY: the frame memory (contract); every field is written below.
        let u = unsafe {
            core::ptr::write_bytes(sp as *mut u8, 0, FRAME as usize);
            &mut *(uc as *mut UContext)
        };
        u.uc_stack = *stack;
        let g = &mut u.gregs;
        let r = [
            c.R8, c.R9, c.R10, c.R11, c.R12, c.R13, c.R14, c.R15, c.Rdi, c.Rsi, c.Rbp, c.Rbx,
            c.Rdx, c.Rax, c.Rcx, c.Rsp, c.Rip,
        ];
        for (i, v) in r.iter().enumerate() {
            g[REG_R8 + i] = *v as i64;
        }
        g[REG_EFL] = i64::from(c.EFlags);
        g[REG_CSGSFS] = i64::from(c.SegCs)
            | (i64::from(c.SegGs) << 16)
            | (i64::from(c.SegFs) << 32)
            | (i64::from(c.SegSs) << 48);
        let (trapno, err, cr2) = match (info.si_signo, fault) {
            (SIGSEGV, Some((addr, write, present))) => (
                14,
                4 | if write { 2 } else { 0 } | i64::from(present),
                addr as i64,
            ),
            (SIGFPE, _) => (0, 0, 0),
            (SIGILL, _) => (6, 0, 0),
            _ => (0, 0, 0),
        };
        g[REG_TRAPNO] = trapno;
        g[REG_ERR] = err;
        g[REG_CR2] = cr2;
        // SAFETY: `Anonymous` starts with the 512-byte FXSAVE image, the
        // layout of `_libc_fpstate`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                core::ptr::addr_of!(c.Anonymous) as *const u8,
                u.fpregs_mem.as_mut_ptr(),
                512,
            )
        };
        u.fpregs = u.fpregs_mem.as_ptr() as u64;
        u.uc_sigmask = mask;
        // SAFETY: siginfo and hidden data follow the ucontext in the frame.
        unsafe {
            ((uc + UC_SIZE) as *mut SigInfo).write(*info);
            ((uc + HIDDEN_OFFSET) as *mut Hidden).write(*hidden);
        }
    }

    /// Points the context at the handler: `rsp` at the return address
    /// (the restorer), arguments in `rdi`, `rsi`, `rdx`.
    ///
    /// # Safety
    ///
    /// `sp` is the frame written by [`write_frame`].
    pub unsafe fn enter_handler(
        c: &mut CONTEXT,
        sp: u64,
        uc: u64,
        sig: i32,
        handler: u64,
        restorer: u64,
    ) {
        // SAFETY: the frame's first word (contract).
        unsafe { (sp as *mut u64).write(restorer) };
        c.Rsp = sp;
        c.Rip = handler;
        c.Rdi = sig as u64;
        c.Rsi = uc + UC_SIZE;
        c.Rdx = uc;
        c.Rax = 0;
        // Clear DF, TF, RF and AC, as Linux does.
        c.EFlags &= !(0x400 | 0x100 | 0x1_0000 | 0x4_0000);
    }

    /// Applies a ucontext to a context record; returns its signal mask.
    ///
    /// # Safety
    ///
    /// `uc` points to a readable ucontext whose `fpregs` is NULL or a
    /// readable 512-byte FXSAVE image.
    pub unsafe fn read_ucontext(uc: u64, c: &mut CONTEXT) -> SigSet {
        // SAFETY: contract.
        let u = unsafe { &*(uc as *const UContext) };
        let g = &u.gregs;
        let r: [&mut u64; 17] = [
            &mut c.R8, &mut c.R9, &mut c.R10, &mut c.R11, &mut c.R12, &mut c.R13, &mut c.R14,
            &mut c.R15, &mut c.Rdi, &mut c.Rsi, &mut c.Rbp, &mut c.Rbx, &mut c.Rdx, &mut c.Rax,
            &mut c.Rcx, &mut c.Rsp, &mut c.Rip,
        ];
        for (i, v) in r.into_iter().enumerate() {
            *v = g[REG_R8 + i] as u64;
        }
        // Only the flags user code may change (CF PF AF ZF SF TF DF OF AC).
        const USER: u32 = 0x0000_0cd5 | 0x100 | 0x4_0000;
        c.EFlags = (c.EFlags & !USER) | (g[REG_EFL] as u32 & USER);
        if u.fpregs != 0 {
            // SAFETY: contract; the context's FXSAVE image is 512 bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    u.fpregs as *const u8,
                    core::ptr::addr_of_mut!(c.Anonymous) as *mut u8,
                    512,
                );
                c.MxCsr = ((u.fpregs + FX_MXCSR as u64) as *const u32).read_unaligned();
            }
        }
        u.uc_sigmask
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::Hidden;
    use crate::linux::signal::aarch64::*;
    use crate::linux::signal::{SigInfo, SigSet, StackT};
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    const UC_SIZE: u64 = core::mem::size_of::<UContext>() as u64;
    /// From the ucontext to the hidden data: ucontext, frame record.
    pub const HIDDEN_OFFSET: u64 = UC_SIZE + 16;
    /// siginfo, ucontext, frame record, hidden.
    const FRAME: u64 = 128 + HIDDEN_OFFSET + core::mem::size_of::<Hidden>() as u64;

    const ESR_EC_DABT_LOW: u64 = 0x24 << 26;
    const ESR_IL: u64 = 1 << 25;
    const ESR_WNR: u64 = 1 << 6;
    const DFSC_TRANSLATION_L3: u64 = 0x07;
    const DFSC_PERMISSION_L3: u64 = 0x0f;

    pub fn pc(c: &CONTEXT) -> u64 {
        c.Pc
    }

    pub fn sp(c: &CONTEXT) -> u64 {
        c.Sp
    }

    /// The frame below `top`: (stack pointer at handler entry, ucontext).
    pub fn layout(top: u64) -> (u64, u64) {
        let sp = (top - FRAME) & !15;
        (sp, sp + 128)
    }

    /// # Safety
    ///
    /// `[sp, sp + FRAME)` is writable and unused.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn write_frame(
        c: &CONTEXT,
        sp: u64,
        uc: u64,
        info: &SigInfo,
        stack: &StackT,
        mask: SigSet,
        fault: Option<(u64, bool, bool)>,
        hidden: &Hidden,
    ) {
        // SAFETY: the frame memory (contract); every field is written below.
        let u = unsafe {
            core::ptr::write_bytes(sp as *mut u8, 0, FRAME as usize);
            &mut *(uc as *mut UContext)
        };
        u.uc_stack = *stack;
        u.uc_sigmask = mask;
        // SAFETY: the X array view of the register union is always valid.
        u.regs = unsafe { c.Anonymous.X };
        u.sp = c.Sp;
        u.pc = c.Pc;
        u.pstate = u64::from(c.Cpsr);
        // SAFETY: records at 16-byte aligned offsets inside the zeroed
        // 4096-byte area; a zero header ends the list.
        unsafe {
            let fp = &mut *(u.reserved.as_mut_ptr() as *mut FpsimdContext);
            fp.magic = FPSIMD_MAGIC;
            fp.size = core::mem::size_of::<FpsimdContext>() as u32;
            fp.fpsr = c.Fpsr;
            fp.fpcr = c.Fpcr;
            for (i, v) in c.V.iter().enumerate() {
                fp.vregs[i] = [v.Anonymous.Low, v.Anonymous.High as u64];
            }
            if let Some((addr, write, present)) = fault {
                u.fault_address = addr;
                let e = &mut *(u.reserved.as_mut_ptr().add(528) as *mut EsrContext);
                e.magic = ESR_MAGIC;
                e.size = 16;
                e.esr = ESR_EC_DABT_LOW
                    | ESR_IL
                    | if write { ESR_WNR } else { 0 }
                    | if present {
                        DFSC_PERMISSION_L3
                    } else {
                        DFSC_TRANSLATION_L3
                    };
            }
            (sp as *mut SigInfo).write(*info);
            // The frame record: the interrupted fp and lr.
            let rec = (uc + UC_SIZE) as *mut u64;
            rec.write(c.Anonymous.Anonymous.Fp);
            rec.add(1).write(c.Anonymous.Anonymous.Lr);
            ((uc + HIDDEN_OFFSET) as *mut Hidden).write(*hidden);
        }
    }

    /// Points the context at the handler: arguments in x0-x2, x29 at the
    /// frame record, x30 the restorer.
    ///
    /// # Safety
    ///
    /// `sp` is the frame written by [`write_frame`].
    pub unsafe fn enter_handler(
        c: &mut CONTEXT,
        sp: u64,
        uc: u64,
        sig: i32,
        handler: u64,
        restorer: u64,
    ) {
        // SAFETY: plain register views of the union.
        unsafe {
            c.Anonymous.X[0] = sig as u64;
            c.Anonymous.X[1] = sp;
            c.Anonymous.X[2] = uc;
            c.Anonymous.X[29] = uc + UC_SIZE;
            c.Anonymous.X[30] = restorer;
        }
        c.Sp = sp;
        c.Pc = handler;
    }

    /// Applies a ucontext to a context record; returns its signal mask.
    ///
    /// # Safety
    ///
    /// `uc` points to a readable ucontext.
    pub unsafe fn read_ucontext(uc: u64, c: &mut CONTEXT) -> SigSet {
        // SAFETY: contract.
        let u = unsafe { &*(uc as *const UContext) };
        c.Anonymous.X = u.regs;
        c.Sp = u.sp;
        c.Pc = u.pc;
        // Only NZCV can be changed by a handler.
        c.Cpsr = (c.Cpsr & 0x0fff_ffff) | (u.pstate as u32 & 0xf000_0000);
        // SAFETY: the first record of the area, if the handler kept it.
        let fp = unsafe { &*(u.reserved.as_ptr() as *const FpsimdContext) };
        if fp.magic == FPSIMD_MAGIC {
            c.Fpsr = fp.fpsr;
            c.Fpcr = fp.fpcr;
            for (i, v) in c.V.iter_mut().enumerate() {
                v.Anonymous.Low = fp.vregs[i][0];
                v.Anonymous.High = fp.vregs[i][1] as i64;
            }
        }
        u.uc_sigmask
    }
}

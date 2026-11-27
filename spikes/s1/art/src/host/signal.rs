//! Minimal synchronous signal delivery for guest code (spike S1 round 3),
//! ported from the round 1 prototype (spikes/s1/src/signals.rs) until
//! andruna-lx has signals.
//!
//! `sigaction`/`sigaction64` record the guest's handlers. The loader's
//! first vectored exception handler (debug.rs), after the FS-base lazy
//! restore, calls [`deliver`]: an access violation, illegal instruction or
//! integer division fault becomes SIGSEGV, SIGILL or SIGFPE with bionic's
//! x86_64 `siginfo_t` and `ucontext_t`, the `SA_SIGINFO` handler runs on the
//! faulting stack (guest stacks are fully committed; the alternate stack is
//! not used, see S1.md), and the context it leaves is resumed, as
//! `rt_sigreturn` does. Asynchronous signals are recorded and never
//! delivered; masks are accepted and ignored.

use core::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};

use super::{fail_with, guard};
use andruna_lx::linux::errno::EINVAL;

pub const SIGILL: i32 = 4;
pub const SIGFPE: i32 = 8;
pub const SIGSEGV: i32 = 11;
const SA_SIGINFO: i32 = 4;
const NSIG: usize = 65;

static HANDLERS: [AtomicUsize; NSIG] = [const { AtomicUsize::new(0) }; NSIG];
static FLAGS: [AtomicI32; NSIG] = [const { AtomicI32::new(0) }; NSIG];
static MASKS: [AtomicU64; NSIG] = [const { AtomicU64::new(0) }; NSIG];
/// Signals delivered, for the report.
pub static DELIVERED: AtomicUsize = AtomicUsize::new(0);

/// bionic LP64 `struct sigaction` (and `sigaction64`, the same on LP64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigAction {
    sa_flags: i32,
    sa_handler: usize,
    sa_mask: u64,
    sa_restorer: usize,
}
const _: () = assert!(core::mem::size_of::<SigAction>() == 32);

gfn! {
    pub unsafe fn h_sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32 {
        guard(|| {
            if sig <= 0 || sig as usize >= NSIG {
                return fail_with(EINVAL);
            }
            let i = sig as usize;
            if !old.is_null() {
                // SAFETY: the guest's struct sigaction.
                unsafe {
                    *old = SigAction {
                        sa_flags: FLAGS[i].load(Ordering::Acquire),
                        sa_handler: HANDLERS[i].load(Ordering::Acquire),
                        sa_mask: MASKS[i].load(Ordering::Acquire),
                        sa_restorer: 0,
                    }
                };
            }
            if !act.is_null() {
                // SAFETY: the guest's struct sigaction.
                let a = unsafe { *act };
                FLAGS[i].store(a.sa_flags, Ordering::Release);
                MASKS[i].store(a.sa_mask, Ordering::Release);
                HANDLERS[i].store(a.sa_handler, Ordering::Release);
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_sigprocmask(_how: i32, _set: *const u64, old: *mut u64) -> i32 {
        if !old.is_null() {
            // SAFETY: the guest's sigset_t (8 bytes on LP64).
            unsafe { *old = 0 };
        }
        0
    }
}

gfn! {
    pub unsafe fn h_sigaltstack(_ss: *const c_void, old: *mut u8) -> i32 {
        if !old.is_null() {
            // SAFETY: the guest's stack_t (24 bytes): report SS_DISABLE.
            unsafe {
                core::ptr::write_bytes(old, 0, 24);
                *(old.add(8) as *mut i32) = 2;
            }
        }
        0
    }
}

// Process-directed asynchronous signals, only as far as ART's signal
// catcher needs them: `pthread_kill`/`kill`/`raise` of a signal that has
// no handler run queue it here, and `sigwait*` takes it (any thread, any
// set: the spike does not track masks).
static PENDING: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());
static PENDING_CV: std::sync::Condvar = std::sync::Condvar::new();

/// Queues a process-directed signal for `sigwait*` (the watchdog's
/// SIGQUIT, which makes ART's signal catcher dump every thread).
pub fn queue_signal(sig: i32) {
    PENDING.lock().unwrap().push(sig);
    PENDING_CV.notify_all();
}

fn take_pending() -> i32 {
    let mut p = PENDING.lock().unwrap();
    loop {
        if !p.is_empty() {
            return p.remove(0);
        }
        p = PENDING_CV.wait(p).unwrap();
    }
}

gfn! {
    pub unsafe fn h_pthread_kill(_t: usize, sig: i32) -> i32 {
        guard(|| {
            if sig > 0 {
                PENDING.lock().unwrap().push(sig);
                PENDING_CV.notify_all();
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_sigwait(_set: *const u64, sig: *mut i32) -> i32 {
        let s = take_pending();
        // SAFETY: the guest's out-parameter.
        unsafe { *sig = s };
        0
    }
}

gfn! {
    pub unsafe fn h_sigwaitinfo(_set: *const u64, info: *mut u8, _t: *const c_void) -> i32 {
        let s = take_pending();
        if !info.is_null() {
            // SAFETY: the guest's 128-byte siginfo_t.
            unsafe {
                core::ptr::write_bytes(info, 0, 128);
                *(info as *mut i32) = s;
                *(info.add(8) as *mut i32) = -6; // SI_TKILL
            }
        }
        s
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("pthread_kill", h_pthread_kill as usize),
        ("sigwait", h_sigwait as usize),
        ("sigwait64", h_sigwait as usize),
        ("sigwaitinfo", h_sigwaitinfo as usize),
        ("sigwaitinfo64", h_sigwaitinfo as usize),
        ("sigtimedwait", h_sigwaitinfo as usize),
        ("sigtimedwait64", h_sigwaitinfo as usize),
        ("sigaction", h_sigaction as usize),
        ("sigaction64", h_sigaction as usize),
        ("sigprocmask", h_sigprocmask as usize),
        ("sigprocmask64", h_sigprocmask as usize),
        ("pthread_sigmask", h_sigprocmask as usize),
        ("pthread_sigmask64", h_sigprocmask as usize),
        ("sigaltstack", h_sigaltstack as usize),
    ]
}

// --- delivery (x86_64) -------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x64 {
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    // gregs[] indices of bionic's x86_64 mcontext_t.
    pub const REG_R8: usize = 0;
    pub const REG_RDI: usize = 8;
    pub const REG_RSI: usize = 9;
    pub const REG_RBP: usize = 10;
    pub const REG_RBX: usize = 11;
    pub const REG_RDX: usize = 12;
    pub const REG_RAX: usize = 13;
    pub const REG_RCX: usize = 14;
    pub const REG_RSP: usize = 15;
    pub const REG_RIP: usize = 16;
    pub const REG_EFL: usize = 17;
    pub const REG_CSGSFS: usize = 18;
    pub const REG_ERR: usize = 19;
    pub const REG_TRAPNO: usize = 20;
    pub const REG_CR2: usize = 22;

    #[repr(C)]
    pub struct SigInfo {
        pub signo: i32,
        pub errno: i32,
        pub code: i32,
        pub pad: i32,
        pub addr: u64,
        pub rest: [u64; 13],
    }

    #[repr(C)]
    pub struct UContext {
        pub flags: u64,
        pub link: u64,
        pub stack: [u64; 3],
        pub gregs: [i64; 23],
        pub fpregs: *mut [u8; 512],
        pub reserved1: [u64; 8],
        pub sigmask: u64,
        pub fpregs_mem: [u8; 512],
    }

    const _: () = {
        assert!(core::mem::size_of::<SigInfo>() == 128);
        assert!(core::mem::size_of::<UContext>() == 816);
        assert!(core::mem::offset_of!(UContext, gregs) == 40);
        assert!(core::mem::offset_of!(UContext, fpregs) == 224);
        assert!(core::mem::offset_of!(UContext, sigmask) == 296);
        assert!(core::mem::offset_of!(UContext, fpregs_mem) == 304);
    };

    pub fn to_uc(c: &CONTEXT, uc: &mut UContext) {
        let g = &mut uc.gregs;
        let r = [
            c.R8, c.R9, c.R10, c.R11, c.R12, c.R13, c.R14, c.R15, c.Rdi, c.Rsi, c.Rbp, c.Rbx,
            c.Rdx, c.Rax, c.Rcx, c.Rsp, c.Rip,
        ];
        for (i, v) in r.iter().enumerate() {
            g[REG_R8 + i] = *v as i64;
        }
        g[REG_EFL] = c.EFlags as i64;
        g[REG_CSGSFS] = (c.SegCs as i64) | ((c.SegGs as i64) << 16) | ((c.SegFs as i64) << 32);
        // SAFETY: FltSave is the 512-byte FXSAVE image, the layout of
        // _libc_fpstate.
        unsafe {
            core::ptr::copy_nonoverlapping(
                &c.Anonymous.FltSave as *const _ as *const u8,
                uc.fpregs_mem.as_mut_ptr(),
                512,
            )
        };
        uc.fpregs = &mut uc.fpregs_mem;
    }

    pub fn from_uc(uc: &UContext, c: &mut CONTEXT) {
        let g = &uc.gregs;
        let r: [&mut u64; 17] = [
            &mut c.R8, &mut c.R9, &mut c.R10, &mut c.R11, &mut c.R12, &mut c.R13, &mut c.R14,
            &mut c.R15, &mut c.Rdi, &mut c.Rsi, &mut c.Rbp, &mut c.Rbx, &mut c.Rdx, &mut c.Rax,
            &mut c.Rcx, &mut c.Rsp, &mut c.Rip,
        ];
        for (i, v) in r.into_iter().enumerate() {
            *v = g[REG_R8 + i] as u64;
        }
        const USER_FLAGS: u32 = 0x0000_0cd5 | 0x0000_0100 | 0x0004_0000;
        c.EFlags = (c.EFlags & !USER_FLAGS) | (g[REG_EFL] as u32 & USER_FLAGS);
        // SAFETY: as in to_uc.
        unsafe {
            core::ptr::copy_nonoverlapping(
                uc.fpregs_mem.as_ptr(),
                &mut c.Anonymous.FltSave as *mut _ as *mut u8,
                512,
            )
        };
    }
    #[allow(dead_code)]
    pub const UNUSED: [usize; 9] = [
        REG_RDI, REG_RSI, REG_RBP, REG_RBX, REG_RDX, REG_RAX, REG_RCX, REG_RSP, REG_RIP,
    ];
}

#[cfg(target_arch = "x86_64")]
type Handler = extern "sysv64" fn(i32, *mut x64::SigInfo, *mut x64::UContext);

/// Delivers a Windows exception as a guest signal if the guest has a
/// handler for it. Returns true when execution continues at the (possibly
/// changed) context.
#[cfg(target_arch = "x86_64")]
pub fn deliver(
    code: u32,
    info: &[usize; 15],
    nparams: u32,
    ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT,
) -> bool {
    use x64::*;
    let (sig, si_code, addr, write) = match code {
        0xC000_0005 | 0xC000_00FD => {
            let (a, w) = if nparams >= 2 {
                (info[1] as u64, info[0] == 1)
            } else {
                (ctx.Rsp, true)
            };
            (SIGSEGV, 0, a, w)
        }
        0xC000_001D | 0xC000_0096 => (SIGILL, 1, ctx.Rip, false),
        0xC000_0094 => (SIGFPE, 1, ctx.Rip, false),
        0xC000_0095 => (SIGFPE, 2, ctx.Rip, false),
        _ => return false,
    };
    let h = HANDLERS[sig as usize].load(Ordering::Acquire);
    // SIG_DFL (0), SIG_IGN (1), SIG_ERR: not handled here.
    if h <= 1 || h == usize::MAX || FLAGS[sig as usize].load(Ordering::Acquire) & SA_SIGINFO == 0 {
        return false;
    }
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("S1_SIGLOG").is_some()) {
        eprintln!(
            "[s1] signal {sig} code {code:#x} pc {:#x} {} addr {addr:#x}",
            ctx.Rip,
            crate::debug::describe(ctx.Rip).unwrap_or_default()
        );
    }
    let mut si = SigInfo {
        signo: sig,
        errno: 0,
        code: si_code,
        pad: 0,
        addr,
        rest: [0; 13],
    };
    if sig == SIGSEGV {
        si.code = if mapped(addr) { 2 } else { 1 }; // SEGV_ACCERR / SEGV_MAPERR
    }
    // SAFETY: plain data.
    let mut uc: Box<UContext> = Box::new(unsafe { core::mem::zeroed() });
    to_uc(ctx, &mut uc);
    if sig == SIGSEGV {
        uc.gregs[REG_TRAPNO] = 14;
        uc.gregs[REG_ERR] = 4 | if write { 2 } else { 0 } | if si.code == 2 { 1 } else { 0 };
        uc.gregs[REG_CR2] = addr as i64;
    }
    // SAFETY: the guest registered an SA_SIGINFO handler, System V.
    let f: Handler = unsafe { core::mem::transmute(h) };
    // The handler is guest code: its thread pointer must be in FS base.
    crate::tls::wrfsbase(crate::tls::current_tp());
    f(sig, &mut si, &mut *uc);
    from_uc(&uc, ctx);
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    true
}

// --- delivery (aarch64) -------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod a64 {
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
    pub const ESR_MAGIC: u32 = 0x4553_5201;

    #[repr(C)]
    pub struct SigInfo {
        pub signo: i32,
        pub errno: i32,
        pub code: i32,
        pub pad: i32,
        pub addr: u64,
        pub rest: [u64; 13],
    }

    /// bionic's arm64 `ucontext_t` (4560 bytes): flags, link, stack,
    /// sigset (8 bytes, then padding to 128), then `struct sigcontext`
    /// (fault_address, x0-x30, sp, pc, pstate, the 4096-byte record area).
    #[repr(C, align(16))]
    pub struct UContext {
        pub flags: u64,
        pub link: u64,
        pub stack: [u64; 3],
        pub sigmask: u64,
        pub pad: [u8; 128],
        pub fault_address: u64,
        pub regs: [u64; 31],
        pub sp: u64,
        pub pc: u64,
        pub pstate: u64,
        pub pad2: u64,
        pub reserved: [u8; 4096],
    }

    #[repr(C)]
    pub struct FpsimdContext {
        pub magic: u32,
        pub size: u32,
        pub fpsr: u32,
        pub fpcr: u32,
        pub vregs: [[u64; 2]; 32],
    }

    const _: () = {
        assert!(core::mem::size_of::<SigInfo>() == 128);
        assert!(core::mem::offset_of!(UContext, sigmask) == 40);
        assert!(core::mem::offset_of!(UContext, fault_address) == 176);
        assert!(core::mem::offset_of!(UContext, regs) == 184);
        assert!(core::mem::offset_of!(UContext, sp) == 432);
        assert!(core::mem::offset_of!(UContext, pc) == 440);
        assert!(core::mem::offset_of!(UContext, pstate) == 448);
        assert!(core::mem::offset_of!(UContext, reserved) == 464);
        assert!(core::mem::size_of::<UContext>() == 4560);
        assert!(core::mem::size_of::<FpsimdContext>() == 528);
    };

    const ESR_EC_DABT_LOW: u64 = 0x24 << 26;
    const ESR_IL: u64 = 1 << 25;
    const ESR_WNR: u64 = 1 << 6;
    const DFSC_TRANSLATION_L3: u64 = 0x07;
    const DFSC_PERMISSION_L3: u64 = 0x0f;

    /// The CONTEXT as the kernel's signal frame: registers, an
    /// `fpsimd_context` record and, for data aborts, an `esr_context`.
    pub fn to_uc(c: &CONTEXT, uc: &mut UContext, fault: Option<(u64, bool, bool)>) {
        // SAFETY: the X array view of the register union is always valid.
        uc.regs = unsafe { c.Anonymous.X };
        uc.sp = c.Sp;
        uc.pc = c.Pc;
        uc.pstate = c.Cpsr as u64;
        let mut fp = FpsimdContext {
            magic: FPSIMD_MAGIC,
            size: 528,
            fpsr: c.Fpsr,
            fpcr: c.Fpcr,
            vregs: [[0; 2]; 32],
        };
        for (i, v) in c.V.iter().enumerate() {
            // SAFETY: the Low/High view of a NEON register is always valid.
            fp.vregs[i] = unsafe { [v.Anonymous.Low, v.Anonymous.High as u64] };
        }
        // SAFETY: records at 16-byte aligned offsets inside the zeroed
        // 4096-byte area; a zero header ends the list.
        unsafe {
            core::ptr::write(uc.reserved.as_mut_ptr() as *mut FpsimdContext, fp);
            if let Some((addr, write, present)) = fault {
                uc.fault_address = addr;
                let esr = ESR_EC_DABT_LOW
                    | ESR_IL
                    | if write { ESR_WNR } else { 0 }
                    | if present {
                        DFSC_PERMISSION_L3
                    } else {
                        DFSC_TRANSLATION_L3
                    };
                let e = uc.reserved.as_mut_ptr().add(528) as *mut u32;
                e.write(ESR_MAGIC);
                e.add(1).write(16);
                (e.add(2) as *mut u64).write(esr);
            }
        }
    }

    pub fn from_uc(uc: &UContext, c: &mut CONTEXT) {
        c.Anonymous.X = uc.regs;
        c.Sp = uc.sp;
        c.Pc = uc.pc;
        // Only NZCV can be changed by a handler.
        c.Cpsr = (c.Cpsr & 0x0fff_ffff) | (uc.pstate as u32 & 0xf000_0000);
        // SAFETY: the first record of the area, written by to_uc.
        let fp = unsafe { &*(uc.reserved.as_ptr() as *const FpsimdContext) };
        if fp.magic == FPSIMD_MAGIC {
            c.Fpsr = fp.fpsr;
            c.Fpcr = fp.fpcr;
            for (i, v) in c.V.iter_mut().enumerate() {
                v.Anonymous.Low = fp.vregs[i][0];
                v.Anonymous.High = fp.vregs[i][1] as i64;
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
type Handler = extern "C" fn(i32, *mut a64::SigInfo, *mut a64::UContext);

/// As the x86_64 `deliver`, with bionic's arm64 signal frame. The guest
/// thread pointer is TPIDR_EL0, which Windows code on the thread leaves
/// alone, so nothing is restored before the handler runs.
#[cfg(target_arch = "aarch64")]
pub fn deliver(
    code: u32,
    info: &[usize; 15],
    nparams: u32,
    ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT,
) -> bool {
    use a64::*;
    let (sig, si_code, addr, write) = match code {
        0xC000_0005 | 0xC000_00FD => {
            let (a, w) = if nparams >= 2 {
                (info[1] as u64, info[0] == 1)
            } else {
                (ctx.Sp, true)
            };
            (SIGSEGV, 0, a, w)
        }
        0xC000_001D | 0xC000_0096 => (SIGILL, 1, ctx.Pc, false),
        0xC000_0094 => (SIGFPE, 1, ctx.Pc, false),
        0xC000_0095 => (SIGFPE, 2, ctx.Pc, false),
        _ => return false,
    };
    let h = HANDLERS[sig as usize].load(Ordering::Acquire);
    if h <= 1 || h == usize::MAX || FLAGS[sig as usize].load(Ordering::Acquire) & SA_SIGINFO == 0 {
        return false;
    }
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("S1_SIGLOG").is_some()) {
        eprintln!(
            "[s1] signal {sig} code {code:#x} pc {:#x} {} addr {addr:#x}",
            ctx.Pc,
            crate::debug::describe(ctx.Pc).unwrap_or_default()
        );
    }
    let mut si = SigInfo {
        signo: sig,
        errno: 0,
        code: si_code,
        pad: 0,
        addr,
        rest: [0; 13],
    };
    let mut fault = None;
    if sig == SIGSEGV {
        let present = mapped(addr);
        si.code = if present { 2 } else { 1 }; // SEGV_ACCERR / SEGV_MAPERR
        fault = Some((addr, write, present));
    }
    // SAFETY: plain data.
    let mut uc: Box<UContext> = Box::new(unsafe { core::mem::zeroed() });
    to_uc(ctx, &mut uc, fault);
    // SAFETY: the guest registered an SA_SIGINFO handler (AAPCS64).
    let f: Handler = unsafe { core::mem::transmute(h) };
    f(sig, &mut si, &mut *uc);
    from_uc(&uc, ctx);
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    true
}

fn mapped(addr: u64) -> bool {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let mut m = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: queries the address space only.
    let n = unsafe {
        VirtualQuery(
            Some(addr as *const c_void),
            &mut m,
            core::mem::size_of_val(&m),
        )
    };
    n != 0 && m.State == MEM_COMMIT
}

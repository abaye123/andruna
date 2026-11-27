//! Signal tests. Handlers are guest-style functions (System V on x86_64),
//! entered through lx's real delivery path: the vectored exception hook,
//! the Linux frame, and `rt_sigreturn` through lx's restorer.

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use super::*;
use crate::linux::mman::*;

/// Actions are process-wide: signal tests run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    install_handler();
    lock(&SERIAL)
}

macro_rules! handler {
    ($name:ident, |$sig:ident, $info:ident, $uc:ident| $body:block) => {
        #[cfg(target_arch = "x86_64")]
        extern "sysv64" fn $name($sig: i32, $info: *mut SigInfo, $uc: *mut c_void) $body
        #[cfg(target_arch = "aarch64")]
        extern "C" fn $name($sig: i32, $info: *mut SigInfo, $uc: *mut c_void) $body
    };
}

fn set_action(sig: i32, h: usize, flags: u64) -> KernelSigaction {
    let a = KernelSigaction {
        handler: h as u64,
        flags: flags | SA_SIGINFO,
        restorer: 0,
        mask: 0,
    };
    sigaction(sig, Some(a), 8).unwrap()
}

fn restore(sig: i32, old: KernelSigaction) {
    sigaction(sig, Some(old), 8).unwrap();
}

fn uc_pc(uc: *mut c_void) -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut x86_64::UContext)).gregs[x86_64::REG_RIP] as u64
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut aarch64::UContext)).pc
    }
}

fn uc_sp(uc: *mut c_void) -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut x86_64::UContext)).gregs[x86_64::REG_RSP] as u64
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut aarch64::UContext)).sp
    }
}

fn uc_set_pc(uc: *mut c_void, pc: u64) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut x86_64::UContext)).gregs[x86_64::REG_RIP] = pc as i64
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the handler's ucontext.
    unsafe {
        (*(uc as *mut aarch64::UContext)).pc = pc
    }
}

fn uc_stack(uc: *mut c_void) -> StackT {
    // SAFETY: uc_stack is at offset 16 in both layouts.
    unsafe { ((uc as u64 + 16) as *const StackT).read() }
}

fn here() -> u64 {
    let m = 0u8;
    core::hint::black_box(&m) as *const u8 as u64
}

fn anon(len: usize, prot: u32) -> usize {
    crate::mem::mmap(0, len, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0).unwrap()
}

#[test]
fn sigaction_rules() {
    let _g = serial();
    assert_eq!(
        sigaction(SIGKILL, Some(KernelSigaction::default()), 8),
        Err(Errno::EINVAL)
    );
    assert_eq!(sigaction(SIGSTOP, None, 8).map(|a| a.handler), Ok(SIG_DFL));
    assert_eq!(sigaction(0, None, 8), Err(Errno::EINVAL));
    assert_eq!(sigaction(65, None, 8), Err(Errno::EINVAL));
    assert_eq!(sigaction(SIGUSR1, None, 4), Err(Errno::EINVAL));
    let a = KernelSigaction {
        handler: 0x1234,
        flags: SA_SIGINFO | SA_RESTART,
        restorer: 0x5678,
        mask: sigbit(SIGUSR2) | sigbit(SIGKILL),
    };
    let old = sigaction(SIGPWR, Some(a), 8).unwrap();
    let got = sigaction(SIGPWR, Some(old), 8).unwrap();
    assert_eq!(got.handler, 0x1234);
    assert_eq!(got.restorer, 0x5678);
    // SIGKILL can never be in a handler mask.
    assert_eq!(got.mask, sigbit(SIGUSR2));
}

#[test]
fn procmask_and_altstack_rules() {
    let _g = serial();
    std::thread::spawn(|| {
        let full = !0u64;
        assert_eq!(sigprocmask(SIG_SETMASK, Some(full), 8), Ok(0));
        let m = sigprocmask(SIG_BLOCK, None, 8).unwrap();
        assert_eq!(m, full & !UNBLOCKABLE);
        assert_eq!(sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR1)), 8), Ok(m));
        assert_eq!(
            sigprocmask(SIG_BLOCK, None, 8).unwrap() & sigbit(SIGUSR1),
            0
        );
        assert_eq!(sigprocmask(7, Some(0), 8), Err(Errno::EINVAL));
        assert_eq!(sigprocmask(SIG_SETMASK, Some(0), 1), Err(Errno::EINVAL));

        let old = sigaltstack(None, here()).unwrap();
        assert_eq!(old.ss_flags, SS_DISABLE);
        let small = StackT {
            ss_sp: 0x10000,
            ss_flags: 0,
            _pad: 0,
            ss_size: 100,
        };
        assert_eq!(sigaltstack(Some(small), here()), Err(Errno::ENOMEM));
        let bad = StackT {
            ss_flags: 77,
            ss_size: 65536,
            ..small
        };
        assert_eq!(sigaltstack(Some(bad), here()), Err(Errno::EINVAL));
        let ok = StackT {
            ss_size: 65536,
            ss_flags: SS_AUTODISARM,
            ..small
        };
        assert!(sigaltstack(Some(ok), here()).is_ok());
        let cur = sigaltstack(None, here()).unwrap();
        assert_eq!(
            (cur.ss_sp, cur.ss_size, cur.ss_flags),
            (0x10000, 65536, SS_AUTODISARM)
        );
        // Being on the stack is reported and prevents changes.
        let on = sigaltstack(None, 0x10000 + 100).unwrap();
        assert_eq!(on.ss_flags, SS_ONSTACK);
        assert_eq!(sigaltstack(Some(ok), 0x10000 + 100), Err(Errno::EPERM));
        let off = StackT {
            ss_flags: SS_DISABLE,
            ..small
        };
        sigaltstack(Some(off), here()).unwrap();
        assert_eq!(sigaltstack(None, here()).unwrap().ss_flags, SS_DISABLE);
    })
    .join()
    .unwrap();
}

// ---- synchronous faults -------------------------------------------------------

static SEGV_ADDR: AtomicU64 = AtomicU64::new(0);
static SEGV_CODE: AtomicI32 = AtomicI32::new(0);
static SEGV_HANDLER_SP: AtomicU64 = AtomicU64::new(0);
static SEGV_UC_FLAGS: AtomicI32 = AtomicI32::new(0);
static SEGV_MASK: AtomicU64 = AtomicU64::new(0);
static FIX_PAGE: AtomicU64 = AtomicU64::new(0);

handler!(segv_fix, |sig, info, uc| {
    assert_eq!(sig, SIGSEGV);
    // SAFETY: the frame's siginfo.
    let i = unsafe { &*info };
    SEGV_ADDR.store(i.addr(), Ordering::SeqCst);
    SEGV_CODE.store(i.si_code, Ordering::SeqCst);
    SEGV_HANDLER_SP.store(here(), Ordering::SeqCst);
    SEGV_UC_FLAGS.store(uc_stack(uc).ss_flags, Ordering::SeqCst);
    SEGV_MASK.store(current().blocked(), Ordering::SeqCst);
    // Make the page accessible; returning re-executes the access.
    crate::mem::mprotect(
        FIX_PAGE.load(Ordering::SeqCst) as usize,
        PAGE_SIZE,
        PROT_READ | PROT_WRITE,
    )
    .unwrap();
});

#[test]
fn segv_on_alternate_stack_and_resume() {
    let _g = serial();
    let old = set_action(SIGSEGV, segv_fix as usize, SA_ONSTACK);
    std::thread::spawn(|| {
        let alt = anon(64 << 10, PROT_READ | PROT_WRITE);
        sigaltstack(
            Some(StackT {
                ss_sp: alt as u64,
                ss_flags: 0,
                _pad: 0,
                ss_size: 64 << 10,
            }),
            here(),
        )
        .unwrap();
        let page = anon(PAGE_SIZE, PROT_READ);
        FIX_PAGE.store(page as u64, Ordering::SeqCst);
        let mask_before = current().blocked();
        // SAFETY: the page is mapped; the write faults (read-only) and the
        // handler makes it writable.
        unsafe { core::ptr::write_volatile((page + 8) as *mut u64, 42) };
        // SAFETY: as above, now writable.
        let v = unsafe { core::ptr::read_volatile((page + 8) as *const u64) };
        assert_eq!(v, 42);
        assert_eq!(SEGV_ADDR.load(Ordering::SeqCst), page as u64 + 8);
        assert_eq!(SEGV_CODE.load(Ordering::SeqCst), SEGV_ACCERR);
        let hsp = SEGV_HANDLER_SP.load(Ordering::SeqCst);
        assert!(
            hsp > alt as u64 && hsp < alt as u64 + (64 << 10),
            "handler ran on the alternate stack"
        );
        assert_eq!(SEGV_UC_FLAGS.load(Ordering::SeqCst), 0);
        // The signal is blocked inside its handler and unblocked after.
        assert_ne!(SEGV_MASK.load(Ordering::SeqCst) & sigbit(SIGSEGV), 0);
        assert_eq!(current().blocked(), mask_before);
        crate::mem::munmap(page, PAGE_SIZE).unwrap();

        // Unmapped memory: SEGV_MAPERR, handler on the same stack without
        // SA_ONSTACK.
        set_action(SIGSEGV, segv_fix as usize, 0);
        let page = anon(PAGE_SIZE, PROT_NONE);
        FIX_PAGE.store(page as u64, Ordering::SeqCst);
        // SAFETY: a PROT_NONE lx mapping (reserved, not committed); the
        // handler makes it accessible.
        unsafe { core::ptr::write_volatile(page as *mut u64, 1) };
        assert_eq!(
            SEGV_CODE.load(Ordering::SeqCst),
            SEGV_ACCERR,
            "PROT_NONE mapping is mapped"
        );
        let hsp = SEGV_HANDLER_SP.load(Ordering::SeqCst);
        assert!(!(hsp > alt as u64 && hsp < alt as u64 + (64 << 10)));
        crate::mem::munmap(page, PAGE_SIZE).unwrap();
        crate::mem::munmap(alt, 64 << 10).unwrap();
    })
    .join()
    .unwrap();
    restore(SIGSEGV, old);
}

static ILL_PC: AtomicU64 = AtomicU64::new(0);

handler!(ill_skip, |sig, _info, uc| {
    assert_eq!(sig, SIGILL);
    let pc = uc_pc(uc);
    ILL_PC.store(pc, Ordering::SeqCst);
    // ud2 is 2 bytes, udf 4.
    let len = if cfg!(target_arch = "x86_64") { 2 } else { 4 };
    uc_set_pc(uc, pc + len);
});

#[test]
fn sigill_handler_changes_the_context() {
    let _g = serial();
    let old = set_action(SIGILL, ill_skip as usize, 0);
    let r: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: ud2 raises SIGILL; the handler skips it.
    unsafe {
        core::arch::asm!("mov eax, 7", "ud2", "add eax, 1", out("rax") r)
    };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: udf raises SIGILL; the handler skips it.
    unsafe {
        core::arch::asm!("mov x0, #7", "udf #0", "add x0, x0, #1", out("x0") r)
    };
    assert_eq!(r, 8);
    assert_ne!(ILL_PC.load(Ordering::SeqCst), 0);
    restore(SIGILL, old);
}

// ---- queued signals ---------------------------------------------------------

static USR_COUNT: AtomicI32 = AtomicI32::new(0);
static USR_INFO_PID: AtomicI32 = AtomicI32::new(0);

handler!(usr_count, |_sig, info, _uc| {
    // SAFETY: the frame's siginfo.
    USR_INFO_PID.store(unsafe { (*info).fields[0] } as u32 as i32, Ordering::SeqCst);
    USR_COUNT.fetch_add(1, Ordering::SeqCst);
});

#[test]
fn tgkill_self_runs_handler_at_syscall_exit() {
    let _g = serial();
    let old = set_action(SIGUSR1, usr_count as usize, 0);
    std::thread::spawn(|| {
        USR_COUNT.store(0, Ordering::SeqCst);
        let me = crate::process::gettid();
        // Blocked: stays pending.
        sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR1)), 8).unwrap();
        tgkill(Some(crate::process::getpid()), me, SIGUSR1, None).unwrap();
        deliver_at_syscall_exit();
        assert_eq!(USR_COUNT.load(Ordering::SeqCst), 0);
        assert_eq!(sigpending(), sigbit(SIGUSR1));
        // A second instance of a standard signal does not queue.
        tgkill(None, me, SIGUSR1, None).unwrap();
        sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR1)), 8).unwrap();
        deliver_at_syscall_exit();
        assert_eq!(USR_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(
            USR_INFO_PID.load(Ordering::SeqCst),
            crate::process::getpid()
        );
        assert_eq!(sigpending(), 0);
        assert_eq!(current().blocked() & sigbit(SIGUSR1), 0);
        // Errors.
        assert_eq!(tgkill(Some(1), me, SIGUSR1, None), Err(Errno::ESRCH));
        assert_eq!(tgkill(None, me, 99, None), Err(Errno::EINVAL));
        assert_eq!(kill(1, 0, None), Err(Errno::ESRCH));
        assert_eq!(kill(crate::process::getpid(), 0, None), Ok(()));
    })
    .join()
    .unwrap();
    restore(SIGUSR1, old);
}

#[test]
fn sigtimedwait_takes_a_signal_from_another_thread() {
    let _g = serial();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR2)), 8).unwrap();
        tx.send(crate::process::gettid()).unwrap();
        let i = sigtimedwait(sigbit(SIGUSR2), None, 8).unwrap();
        let none = sigtimedwait(sigbit(SIGUSR2), Some(&Timespec::default()), 8);
        (i.si_signo, i.si_code, none)
    });
    let tid = rx.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    tgkill(None, tid, SIGUSR2, None).unwrap();
    let (sig, code, none) = waiter.join().unwrap();
    assert_eq!((sig, code), (SIGUSR2, SI_TKILL));
    assert_eq!(none, Err(Errno::EAGAIN));
}

#[test]
fn futex_wait_is_interrupted() {
    let _g = serial();
    let old = set_action(SIGUSR1, usr_count as usize, 0);
    USR_COUNT.store(0, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = current();
        tx.send(crate::process::gettid()).unwrap();
        let w = std::sync::atomic::AtomicU32::new(0);
        let r = crate::futex::wait(&w, 0, None);
        deliver_at_syscall_exit();
        r
    });
    let tid = rx.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    tgkill(None, tid, SIGUSR1, None).unwrap();
    assert_eq!(waiter.join().unwrap(), Err(Errno::EINTR));
    assert_eq!(USR_COUNT.load(Ordering::SeqCst), 1);
    restore(SIGUSR1, old);
}

// ---- stack overflow ---------------------------------------------------------

/// ART's implicit check: every frame probes `GAP` bytes below its stack
/// pointer; the protected region at the stack bottom makes the probe fault.
#[cfg(target_arch = "x86_64")]
const GAP: u64 = 8 << 10;
/// ART's arm64 gap must be raised for Windows (S1: dispatch uses 7.1 KiB).
#[cfg(target_arch = "aarch64")]
const GAP: u64 = 16 << 10;

static STOP: AtomicBool = AtomicBool::new(false);
static PROTECTED: AtomicU64 = AtomicU64::new(0);
static FAULT_SP: AtomicU64 = AtomicU64::new(0);
static FAULT_ADDR: AtomicU64 = AtomicU64::new(0);
static OVERFLOW_HANDLER_SP: AtomicU64 = AtomicU64::new(0);
const PROTECTED_LEN: usize = 16 << 10;

handler!(overflow, |_sig, info, uc| {
    FAULT_SP.store(uc_sp(uc), Ordering::SeqCst);
    // SAFETY: the frame's siginfo.
    FAULT_ADDR.store(unsafe { (*info).addr() }, Ordering::SeqCst);
    OVERFLOW_HANDLER_SP.store(here(), Ordering::SeqCst);
    STOP.store(true, Ordering::SeqCst);
    crate::mem::mprotect(
        PROTECTED.load(Ordering::SeqCst) as usize,
        PROTECTED_LEN,
        PROT_READ | PROT_WRITE,
    )
    .unwrap();
});

#[inline(never)]
extern "C" fn recurse(depth: u64) -> u64 {
    let mut pad = [0u8; 192];
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads the stack GAP bytes below rsp, as ART's probe does.
    unsafe {
        core::arch::asm!("test qword ptr [rsp - {gap}], rax", gap = const GAP, out("rax") _)
    };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above.
    unsafe {
        core::arch::asm!("sub {t}, sp, #{gap}", "ldr {t}, [{t}]", gap = const GAP, t = out(reg) _)
    };
    if STOP.load(Ordering::SeqCst) {
        return depth;
    }
    pad[(depth % 192) as usize] = depth as u8;
    let r = recurse(core::hint::black_box(depth + 1));
    core::hint::black_box(&pad);
    r
}

extern "C" fn run_recursion(_: u64) {
    recurse(0);
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn on_stack(top: u64, f: extern "C" fn(u64), arg: u64) {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "mov rsp, rcx",
        "sub rsp, 32",
        "mov rcx, r8",
        "call rdx",
        "mov rsp, rbp",
        "pop rbp",
        "ret",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn on_stack(top: u64, f: extern "C" fn(u64), arg: u64) {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "mov sp, x0",
        "mov x0, x2",
        "blr x1",
        "mov sp, x29",
        "ldp x29, x30, [sp], #16",
        "ret",
    )
}

const PATTERN: u64 = 0x5a5a_a5a5_5a5a_a5a5;

/// Runs the recursion on a committed 512 KiB stack whose lowest 16 KiB are
/// protected, as ART lays out a thread stack, and returns the bytes of the
/// faulting stack used below the faulting stack pointer.
fn overflow_once(onstack: bool) -> (u64, u64) {
    const LEN: usize = 512 << 10;
    let old = set_action(
        SIGSEGV,
        overflow as usize,
        if onstack { SA_ONSTACK } else { 0 },
    );
    let stack = anon(LEN, PROT_READ | PROT_WRITE);
    let alt = anon(64 << 10, PROT_READ | PROT_WRITE);
    sigaltstack(
        Some(StackT {
            ss_sp: alt as u64,
            ss_flags: 0,
            _pad: 0,
            ss_size: 64 << 10,
        }),
        here(),
    )
    .unwrap();
    let low = stack + PROTECTED_LEN;
    let top = (stack + LEN) as u64;
    for p in (low..stack + LEN).step_by(8) {
        // SAFETY: the new stack mapping, not in use yet.
        unsafe { (p as *mut u64).write(PATTERN) };
    }
    crate::mem::mprotect(stack, PROTECTED_LEN, PROT_NONE).unwrap();
    PROTECTED.store(stack as u64, Ordering::SeqCst);
    STOP.store(false, Ordering::SeqCst);
    let (base, limit) = super::frame::stack_bounds();
    super::frame::set_stack_bounds(top, stack as u64);
    // SAFETY: `top` is the 16-byte aligned top of a committed stack that
    // nothing else uses; `run_recursion` returns normally.
    unsafe { on_stack(top, run_recursion, 0) };
    super::frame::set_stack_bounds(base, limit);
    let mut lowest = low as u64;
    // SAFETY: the stack mapping, readable again.
    while unsafe { ((lowest) as *const u64).read() } == PATTERN {
        lowest += 8;
    }
    let fault_sp = FAULT_SP.load(Ordering::SeqCst);
    let addr = FAULT_ADDR.load(Ordering::SeqCst);
    let hsp = OVERFLOW_HANDLER_SP.load(Ordering::SeqCst);
    assert!(
        addr >= stack as u64 && addr < low as u64,
        "fault in the protected region"
    );
    assert_eq!(fault_sp - GAP, addr, "the probe faulted");
    let on_alt = hsp > alt as u64 && hsp < alt as u64 + (64 << 10);
    assert_eq!(on_alt, onstack);
    sigaltstack(
        Some(StackT {
            ss_flags: SS_DISABLE,
            ..StackT::default()
        }),
        here(),
    )
    .unwrap();
    crate::mem::munmap(stack, LEN).unwrap();
    crate::mem::munmap(alt, 64 << 10).unwrap();
    restore(SIGSEGV, old);
    (fault_sp - lowest, fault_sp - low as u64)
}

#[test]
fn stack_overflow_is_delivered_on_the_alternate_stack() {
    let _g = serial();
    std::thread::spawn(|| {
        let (used_alt, margin) = overflow_once(true);
        eprintln!(
            "stack overflow, SA_ONSTACK: {used_alt} bytes of the faulting stack used below the \
             faulting sp (gap {GAP}, {margin} bytes to the protected region)"
        );
        assert!(used_alt < GAP, "dispatch must fit in the gap");
        // Without the alternate stack the handler's frame and its calls
        // use the faulting stack too; with a larger gap it still works.
        let (used_same, _) = overflow_once_large_gap();
        eprintln!("stack overflow, same stack: {used_same} bytes used below the faulting sp");
        assert!(used_same > used_alt);
    })
    .join()
    .unwrap();
}

/// The same-stack variant needs more room than the gap; it runs with the
/// protected region 64 KiB below where the probe faults by moving the
/// probe target: it uses a recursion that faults on a plain access.
fn overflow_once_large_gap() -> (u64, u64) {
    // A fault on the same stack: SIGSEGV handler without SA_ONSTACK, raised
    // by a write to a read-only page far from the stack bottom.
    let old = set_action(SIGSEGV, segv_fix as usize, 0);
    let page = anon(PAGE_SIZE, PROT_READ);
    FIX_PAGE.store(page as u64, Ordering::SeqCst);
    let before = here();
    // SAFETY: as in `segv_on_alternate_stack_and_resume`.
    unsafe { core::ptr::write_volatile(page as *mut u64, 1) };
    let hsp = SEGV_HANDLER_SP.load(Ordering::SeqCst);
    crate::mem::munmap(page, PAGE_SIZE).unwrap();
    restore(SIGSEGV, old);
    (before - hsp, 0)
}

static TRANSIENT_SEGVS: AtomicU64 = AtomicU64::new(0);

handler!(segv_count, |_sig, _info, _uc| {
    // The page is accessible again when the handler runs: returning retries
    // the access.
    TRANSIENT_SEGVS.fetch_add(1, Ordering::SeqCst);
});

/// `madvise(MADV_DONTNEED)` decommits and recommits pages. A thread writing
/// to them meanwhile faults inside the window or just after it, again and
/// again on the same page (a GC clearing its card table while mutators mark
/// cards): none of those faults is a signal.
#[test]
fn faults_in_madvise_windows_are_not_signals() {
    let _g = serial();
    let old = set_action(SIGSEGV, segv_count as usize, 0);
    TRANSIENT_SEGVS.store(0, Ordering::SeqCst);
    let page = anon(PAGE_SIZE, PROT_READ | PROT_WRITE);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // SAFETY: an lx read-write mapping, unmapped only after the
                // thread is joined.
                unsafe { core::ptr::write_volatile((page + 64) as *mut u64, n) };
                n += 1;
            }
            n
        })
    };
    for _ in 0..20_000 {
        crate::mem::madvise(page, PAGE_SIZE, MADV_DONTNEED).unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let writes = writer.join().unwrap();
    assert!(writes > 0);
    assert_eq!(TRANSIENT_SEGVS.load(Ordering::SeqCst), 0);
    crate::mem::munmap(page, PAGE_SIZE).unwrap();
    restore(SIGSEGV, old);
}

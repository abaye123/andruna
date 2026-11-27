//! Signals for guest code, answered by the loader while andruna-lx has
//! none (its `signal` module marks the boundary). When lx implements the
//! signal system calls, [`crate::syscall`] stops reaching this module:
//! it is only used for calls lx answers with `ENOSYS`.
//!
//! - `rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`, `sigaltstack`
//!   keep bionic's kernel-side state (actions per process, masks per
//!   thread; new threads inherit their creator's mask, as with `clone`).
//! - Synchronous signals: the loader's first vectored exception handler
//!   ([`crate::debug`]), after the x64 FS-base lazy restore, calls
//!   [`deliver_fault`]; an access violation, illegal instruction or integer
//!   division fault becomes SIGSEGV, SIGILL or SIGFPE with bionic's
//!   `siginfo_t` and `ucontext_t`, the guest's `SA_SIGINFO` handler runs on
//!   the faulting stack, and the context it leaves is resumed, as
//!   `rt_sigreturn` does (spike S1, rounds 3 and 4).
//! - `kill`, `tkill`, `tgkill`, `rt_sigqueueinfo`: a signal that the target
//!   blocks is queued for `rt_sigtimedwait` (ART's signal catcher); an
//!   unblocked one with a handler runs the handler at once when the target
//!   is the calling thread; the default action ends the process or ignores
//!   the signal, as on Linux. A handled signal for another thread that it
//!   does not block stays pending: there is no asynchronous delivery yet.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use andruna_lx::linux::errno::*;

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGABRT: i32 = 6;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGKILL: i32 = 9;
pub const SIGSEGV: i32 = 11;
pub const SIGSTOP: i32 = 19;
const SIGCHLD: i32 = 17;
const SIGCONT: i32 = 18;
const SIGURG: i32 = 23;
const SIGWINCH: i32 = 28;
/// Highest signal number + 1.
const NSIG: usize = 65;

const SA_SIGINFO: u64 = 4;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;

const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;

const SS_ONSTACK: i32 = 1;
const SS_DISABLE: i32 = 2;

const SI_USER: i32 = 0;
const SI_TKILL: i32 = -6;

/// The kernel's `struct sigaction` on x86_64 and arm64 (both define
/// `SA_RESTORER`, so the restorer sits before the mask).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelSigaction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

/// `stack_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct StackT {
    pub sp: u64,
    pub flags: i32,
    pub size: u64,
}

/// `siginfo_t` (128 bytes): the common header and the first union words.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigInfo {
    pub signo: i32,
    pub errno: i32,
    pub code: i32,
    pub pad: i32,
    /// `si_addr` for faults; `si_pid` and `si_uid` for kill.
    pub word0: u64,
    pub rest: [u64; 13],
}

const _: () = {
    assert!(core::mem::size_of::<KernelSigaction>() == 32);
    assert!(core::mem::size_of::<StackT>() == 24);
    assert!(core::mem::offset_of!(StackT, flags) == 8);
    assert!(core::mem::offset_of!(StackT, size) == 16);
    assert!(core::mem::size_of::<SigInfo>() == 128);
    assert!(core::mem::offset_of!(SigInfo, word0) == 16);
};

impl SigInfo {
    fn new(signo: i32, code: i32) -> Self {
        SigInfo {
            signo,
            errno: 0,
            code,
            pad: 0,
            word0: 0,
            rest: [0; 13],
        }
    }
}

fn bit(sig: i32) -> u64 {
    1u64 << (sig - 1)
}

fn valid(sig: i32) -> bool {
    sig > 0 && (sig as usize) < NSIG
}

/// SIGKILL and SIGSTOP can be neither caught nor blocked.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

struct Action {
    handler: AtomicU64,
    flags: AtomicU64,
    restorer: AtomicU64,
    mask: AtomicU64,
}

static ACTIONS: [Action; NSIG] = [const {
    Action {
        handler: AtomicU64::new(0),
        flags: AtomicU64::new(0),
        restorer: AtomicU64::new(0),
        mask: AtomicU64::new(0),
    }
}; NSIG];

fn action(sig: i32) -> KernelSigaction {
    let a = &ACTIONS[sig as usize];
    KernelSigaction {
        handler: a.handler.load(Ordering::Acquire),
        flags: a.flags.load(Ordering::Acquire),
        restorer: a.restorer.load(Ordering::Acquire),
        mask: a.mask.load(Ordering::Acquire),
    }
}

fn set_action(sig: i32, k: &KernelSigaction) {
    let a = &ACTIONS[sig as usize];
    a.flags.store(k.flags, Ordering::Release);
    a.restorer.store(k.restorer, Ordering::Release);
    a.mask.store(k.mask & !UNBLOCKABLE, Ordering::Release);
    a.handler.store(k.handler, Ordering::Release);
}

/// Signal masks by thread id (the calling thread's is also cached).
static MASKS: Mutex<Option<HashMap<i32, u64>>> = Mutex::new(None);

thread_local! {
    static ALTSTACK: std::cell::Cell<(u64, u64, i32)> = const { std::cell::Cell::new((0, 0, SS_DISABLE)) };
}

fn tid() -> i32 {
    andruna_lx::process::gettid()
}

fn mask_of(t: i32) -> u64 {
    MASKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&t).copied())
        .unwrap_or(0)
}

fn set_mask_of(t: i32, m: u64) {
    MASKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(t, m & !UNBLOCKABLE);
}

/// A new thread `child` starts with the calling thread's mask (`clone`).
pub fn inherit(child: i32) {
    set_mask_of(child, mask_of(tid()));
}

/// A thread ended: forget its state.
pub fn thread_exited(t: i32) {
    if let Some(m) = MASKS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(&t);
    }
    let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|s| s.target != Some(t));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    sig: i32,
    /// None: process-directed.
    target: Option<i32>,
    code: i32,
    pid: i32,
    uid: u32,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static PENDING_CV: Condvar = Condvar::new();
/// Signals delivered to guest handlers (faults and synchronous raises).
pub static DELIVERED: AtomicUsize = AtomicUsize::new(0);

fn queue(p: Pending) {
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    // Standard signals are not queued twice (real-time ones are).
    if p.sig >= 32 || !q.iter().any(|x| x.sig == p.sig && x.target == p.target) {
        q.push(p);
    }
    PENDING_CV.notify_all();
}

/// Takes the first pending signal in `set` for thread `t`.
fn take(q: &mut Vec<Pending>, t: i32, set: u64) -> Option<Pending> {
    let i = q
        .iter()
        .position(|p| set & bit(p.sig) != 0 && p.target.is_none_or(|x| x == t))?;
    Some(q.remove(i))
}

// ---------------------------------------------------------------------------
// System calls
// ---------------------------------------------------------------------------

/// Linux system call numbers of this module.
#[cfg(target_arch = "x86_64")]
pub mod nr {
    pub const RT_SIGACTION: i64 = 13;
    pub const RT_SIGPROCMASK: i64 = 14;
    pub const KILL: i64 = 62;
    pub const RT_SIGPENDING: i64 = 127;
    pub const RT_SIGTIMEDWAIT: i64 = 128;
    pub const RT_SIGQUEUEINFO: i64 = 129;
    pub const RT_SIGSUSPEND: i64 = 130;
    pub const SIGALTSTACK: i64 = 131;
    pub const TKILL: i64 = 200;
    pub const TGKILL: i64 = 234;
    pub const RT_TGSIGQUEUEINFO: i64 = 297;
}

#[cfg(target_arch = "aarch64")]
pub mod nr {
    pub const KILL: i64 = 129;
    pub const TKILL: i64 = 130;
    pub const TGKILL: i64 = 131;
    pub const SIGALTSTACK: i64 = 132;
    pub const RT_SIGSUSPEND: i64 = 133;
    pub const RT_SIGACTION: i64 = 134;
    pub const RT_SIGPROCMASK: i64 = 135;
    pub const RT_SIGPENDING: i64 = 136;
    pub const RT_SIGTIMEDWAIT: i64 = 137;
    pub const RT_SIGQUEUEINFO: i64 = 138;
    pub const RT_TGSIGQUEUEINFO: i64 = 240;
}

/// Handles the signal system calls; `None` for other numbers.
///
/// # Safety
///
/// The pointer arguments satisfy the kernel's contract for call `n`.
pub unsafe fn syscall(n: i64, a: [i64; 6]) -> Option<i64> {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        Some(match n {
            nr::RT_SIGACTION => rt_sigaction(
                a[0] as i32,
                a[1] as *const KernelSigaction,
                a[2] as *mut KernelSigaction,
                a[3] as usize,
            ),
            nr::RT_SIGPROCMASK => rt_sigprocmask(
                a[0] as i32,
                a[1] as *const u64,
                a[2] as *mut u64,
                a[3] as usize,
            ),
            nr::RT_SIGPENDING => rt_sigpending(a[0] as *mut u64, a[1] as usize),
            nr::RT_SIGTIMEDWAIT => rt_sigtimedwait(
                a[0] as *const u64,
                a[1] as *mut SigInfo,
                a[2] as *const andruna_lx::linux::time::Timespec,
                a[3] as usize,
            ),
            nr::RT_SIGSUSPEND => rt_sigsuspend(a[0] as *const u64, a[1] as usize),
            nr::SIGALTSTACK => sigaltstack(a[0] as *const StackT, a[1] as *mut StackT),
            nr::KILL => kill(a[0] as i32, a[1] as i32, SI_USER),
            nr::TKILL => tgkill(crate::process::guest_pid(), a[0] as i32, a[1] as i32),
            nr::TGKILL => tgkill(a[0] as i32, a[1] as i32, a[2] as i32),
            nr::RT_SIGQUEUEINFO => kill(a[0] as i32, a[1] as i32, SI_TKILL),
            nr::RT_TGSIGQUEUEINFO => tgkill(a[0] as i32, a[1] as i32, a[2] as i32),
            _ => return None,
        })
    }
}

unsafe fn rt_sigaction(
    sig: i32,
    act: *const KernelSigaction,
    old: *mut KernelSigaction,
    setsize: usize,
) -> i64 {
    if setsize != 8 || !valid(sig) {
        return -i64::from(EINVAL);
    }
    if !act.is_null() && (sig == SIGKILL || sig == SIGSTOP) {
        return -i64::from(EINVAL);
    }
    if !old.is_null() {
        // SAFETY: the caller's struct (kernel contract).
        unsafe { old.write_unaligned(action(sig)) };
    }
    if !act.is_null() {
        // SAFETY: as above.
        let k = unsafe { act.read_unaligned() };
        set_action(sig, &k);
        if k.handler == SIG_IGN || (k.handler == SIG_DFL && default_ignores(sig)) {
            // Pending instances are discarded, as on Linux.
            PENDING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|p| p.sig != sig);
        }
    }
    0
}

unsafe fn rt_sigprocmask(how: i32, set: *const u64, old: *mut u64, setsize: usize) -> i64 {
    if setsize != 8 {
        return -i64::from(EINVAL);
    }
    let t = tid();
    let cur = mask_of(t);
    if !set.is_null() {
        // SAFETY: the caller's sigset (kernel contract).
        let s = unsafe { set.read_unaligned() };
        let new = match how {
            SIG_BLOCK => cur | s,
            SIG_UNBLOCK => cur & !s,
            SIG_SETMASK => s,
            _ => return -i64::from(EINVAL),
        };
        set_mask_of(t, new);
    }
    if !old.is_null() {
        // SAFETY: as above.
        unsafe { old.write_unaligned(cur) };
    }
    if !set.is_null() {
        run_unblocked();
    }
    0
}

/// Runs the handlers of pending signals the calling thread no longer
/// blocks (a signal is delivered when it is unblocked).
fn run_unblocked() {
    let t = tid();
    loop {
        let m = mask_of(t);
        let p = {
            let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            let i = q.iter().position(|p| {
                m & bit(p.sig) == 0
                    && p.target.is_none_or(|x| x == t)
                    && action(p.sig).handler > SIG_IGN
            });
            match i {
                Some(i) => q.remove(i),
                None => return,
            }
        };
        run_handler_now(&p);
    }
}

unsafe fn rt_sigpending(set: *mut u64, setsize: usize) -> i64 {
    if setsize != 8 || set.is_null() {
        return -i64::from(if set.is_null() { EFAULT } else { EINVAL });
    }
    let t = tid();
    let v = PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|p| p.target.is_none_or(|x| x == t))
        .fold(0u64, |a, p| a | bit(p.sig));
    // SAFETY: the caller's sigset.
    unsafe { set.write_unaligned(v) };
    0
}

unsafe fn rt_sigtimedwait(
    set: *const u64,
    info: *mut SigInfo,
    timeout: *const andruna_lx::linux::time::Timespec,
    setsize: usize,
) -> i64 {
    if setsize != 8 || set.is_null() {
        return -i64::from(EINVAL);
    }
    // SAFETY: the caller's arguments (kernel contract).
    let set = unsafe { set.read_unaligned() } & !UNBLOCKABLE;
    let deadline = if timeout.is_null() {
        None
    } else {
        // SAFETY: as above.
        let ts = unsafe { timeout.read_unaligned() };
        if ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 || ts.tv_sec < 0 {
            return -i64::from(EINVAL);
        }
        Some(
            std::time::Instant::now()
                + std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32),
        )
    };
    let t = tid();
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let p = loop {
        if let Some(p) = take(&mut q, t, set) {
            break p;
        }
        match deadline {
            None => q = PENDING_CV.wait(q).unwrap_or_else(|e| e.into_inner()),
            Some(d) => {
                let now = std::time::Instant::now();
                if now >= d {
                    return -i64::from(EAGAIN);
                }
                q = PENDING_CV
                    .wait_timeout(q, d - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
    };
    drop(q);
    if !info.is_null() {
        // SAFETY: the caller's siginfo_t.
        unsafe { info.write_unaligned(info_of(&p)) };
    }
    i64::from(p.sig)
}

unsafe fn rt_sigsuspend(set: *const u64, setsize: usize) -> i64 {
    if setsize != 8 || set.is_null() {
        return -i64::from(EINVAL);
    }
    let t = tid();
    let old = mask_of(t);
    // SAFETY: the caller's sigset.
    set_mask_of(t, unsafe { set.read_unaligned() });
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let m = mask_of(t);
        if q.iter()
            .any(|p| m & bit(p.sig) == 0 && p.target.is_none_or(|x| x == t))
        {
            break;
        }
        q = PENDING_CV.wait(q).unwrap_or_else(|e| e.into_inner());
    }
    drop(q);
    run_unblocked();
    set_mask_of(t, old);
    -i64::from(EINTR)
}

unsafe fn sigaltstack(ss: *const StackT, old: *mut StackT) -> i64 {
    let (sp, size, flags) = ALTSTACK.with(|c| c.get());
    if !old.is_null() {
        // SAFETY: the caller's stack_t.
        unsafe { old.write_unaligned(StackT { sp, flags, size }) };
    }
    if !ss.is_null() {
        // SAFETY: as above.
        let s = unsafe { ss.read_unaligned() };
        if s.flags & !(SS_DISABLE | SS_ONSTACK) != 0 {
            return -i64::from(EINVAL);
        }
        if s.flags & SS_DISABLE != 0 {
            ALTSTACK.with(|c| c.set((0, 0, SS_DISABLE)));
        } else {
            if s.size < 2048 {
                return -i64::from(ENOMEM);
            }
            ALTSTACK.with(|c| c.set((s.sp, s.size, 0)));
        }
    }
    0
}

fn default_ignores(sig: i32) -> bool {
    matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH)
}

fn info_of(p: &Pending) -> SigInfo {
    let mut si = SigInfo::new(p.sig, p.code);
    si.word0 = (p.pid as u32 as u64) | (u64::from(p.uid) << 32);
    si
}

fn kill(pid: i32, sig: i32, code: i32) -> i64 {
    let me = crate::process::guest_pid();
    if pid != me && pid != 0 && pid != -1 {
        return -i64::from(ESRCH);
    }
    if sig == 0 {
        return 0;
    }
    if !valid(sig) {
        return -i64::from(EINVAL);
    }
    raise(None, sig, code)
}

fn tgkill(tgid: i32, t: i32, sig: i32) -> i64 {
    if tgid != crate::process::guest_pid() || t <= 0 {
        return -i64::from(if t <= 0 { EINVAL } else { ESRCH });
    }
    if !valid(sig) {
        return -i64::from(EINVAL);
    }
    if sig == 0 {
        return 0;
    }
    raise(Some(t), sig, SI_TKILL)
}

fn raise(target: Option<i32>, sig: i32, code: i32) -> i64 {
    let me = tid();
    let p = Pending {
        sig,
        target,
        code,
        pid: crate::process::guest_pid(),
        uid: andruna_lx::process::uid(),
    };
    let k = action(sig);
    if k.handler == SIG_IGN || (k.handler == SIG_DFL && default_ignores(sig)) {
        return 0;
    }
    let blocked = mask_of(target.unwrap_or(me)) & bit(sig) != 0;
    if blocked {
        queue(p);
        return 0;
    }
    if k.handler == SIG_DFL {
        default_action(sig);
    }
    if target.is_none_or(|t| t == me) {
        run_handler_now(&p);
    } else {
        queue(p);
    }
    0
}

/// The default action of a signal that ends the process.
fn default_action(sig: i32) -> ! {
    crate::debug::report(format_args!(
        "process ended by signal {sig} (default action)"
    ));
    crate::process::exit_process(128 + sig)
}

/// Runs the guest handler of `p` on the calling thread, as the kernel
/// does when it delivers a signal raised by the thread itself.
fn run_handler_now(p: &Pending) {
    let k = action(p.sig);
    if k.handler <= SIG_IGN {
        if k.handler == SIG_DFL && !default_ignores(p.sig) {
            default_action(p.sig);
        }
        return;
    }
    let mut si = info_of(p);
    let mut uc = Box::new(arch::UContext::zeroed());
    let t = tid();
    let saved = mask_of(t);
    arch::set_sigmask(&mut uc, saved);
    begin_handler(t, p.sig, &k);
    // SAFETY: the guest registered this handler for `p.sig` (guest
    // convention); `si` and `uc` live across the call.
    unsafe { call_handler(&k, p.sig, &mut si, &mut *uc as *mut _ as *mut u8) };
    set_mask_of(t, arch::sigmask(&uc));
    DELIVERED.fetch_add(1, Ordering::Relaxed);
}

/// Mask and action changes when a handler starts.
fn begin_handler(t: i32, sig: i32, k: &KernelSigaction) {
    let mut m = mask_of(t) | k.mask;
    if k.flags & SA_NODEFER == 0 {
        m |= bit(sig);
    }
    set_mask_of(t, m);
    if k.flags & SA_RESETHAND != 0 {
        set_action(sig, &KernelSigaction::default());
    }
}

type HandlerFn = gty!(fn(i32, *mut SigInfo, *mut u8));
type PlainHandlerFn = gty!(fn(i32));

/// # Safety
///
/// `k.handler` is a guest function for the signal.
unsafe fn call_handler(k: &KernelSigaction, sig: i32, si: *mut SigInfo, uc: *mut u8) {
    // The handler is guest code: on x64 its thread pointer must be in the
    // FS base, which Windows may have reset.
    crate::tls::set_thread_pointer(crate::tls::current());
    if k.flags & SA_SIGINFO != 0 {
        // SAFETY: the function's contract.
        let f: HandlerFn = unsafe { core::mem::transmute(k.handler) };
        f(sig, si, uc);
    } else {
        // SAFETY: as above.
        let f: PlainHandlerFn = unsafe { core::mem::transmute(k.handler) };
        f(sig);
    }
}

// ---------------------------------------------------------------------------
// Synchronous delivery from the vectored exception handler
// ---------------------------------------------------------------------------

/// `ANDRUNA_SIGLOG`: report each fault delivered as a signal.
fn siglog() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("ANDRUNA_SIGLOG").is_some())
}

/// Delivers a Windows exception on a guest thread as a guest signal when
/// the guest has a handler for it. Returns true when execution continues
/// at the (possibly changed) context.
pub fn deliver_fault(
    code: u32,
    info: &[usize; 15],
    nparams: u32,
    ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT,
) -> bool {
    let pc = arch::pc(ctx);
    let (sig, si_code, addr, write) = match code {
        0xC000_0005 => {
            let (a, w) = if nparams >= 2 {
                (info[1] as u64, info[0] == 1)
            } else {
                (0, false)
            };
            (SIGSEGV, 0, a, w)
        }
        // Stack overflow on the guard page: report the stack pointer.
        0xC000_00FD => (SIGSEGV, 0, arch::sp(ctx), true),
        0xC000_001D | 0xC000_0096 => (SIGILL, 1, pc, false), // ILL_ILLOPC
        0xC000_0094 => (SIGFPE, 1, pc, false),               // FPE_INTDIV
        0xC000_0095 => (SIGFPE, 2, pc, false),               // FPE_INTOVF
        0x8000_0003 => (SIGTRAP, 1, pc, false),              // TRAP_BRKPT
        _ => return false,
    };
    let k = action(sig);
    if k.handler <= SIG_IGN || k.handler == u64::MAX {
        return false;
    }
    if crate::tls::current() == 0 {
        // Not a guest thread.
        return false;
    }
    let fault = Fault {
        code,
        sig,
        si_code,
        addr,
        write,
        pc,
        k,
        ctx,
    };
    let (alt_sp, alt_size, alt_flags) = ALTSTACK.with(|c| c.get());
    let sp = arch::sp(fault.ctx);
    let on_alt = sp > alt_sp && sp <= alt_sp + alt_size;
    if k.flags & SA_ONSTACK != 0 && alt_flags & SS_DISABLE == 0 && alt_size != 0 && !on_alt {
        // As the kernel: the handler runs on the alternate stack, so a
        // fault near the end of the thread's stack (ART's stack overflow
        // check) leaves the handler room. The TEB describes that stack
        // meanwhile, as Windows requires of the stack in use.
        let saved = crate::thread::stack_bounds();
        crate::thread::set_stack_bounds(alt_sp + alt_size, alt_sp);
        let mut job = (fault, false);
        // SAFETY: the alternate stack is the guest's, registered for this
        // thread and not in use (checked above); `run_on_alt` returns.
        unsafe {
            call_on_stack(
                &mut job as *mut (Fault, bool) as u64,
                run_on_alt as usize as u64,
                alt_sp + alt_size,
            )
        };
        crate::thread::set_stack_bounds(saved.0, saved.1);
        return job.1;
    }
    deliver(fault)
}

const SA_ONSTACK: u64 = 0x0800_0000;

struct Fault<'a> {
    code: u32,
    sig: i32,
    si_code: i32,
    addr: u64,
    write: bool,
    pc: u64,
    k: KernelSigaction,
    ctx: &'a mut windows::Win32::System::Diagnostics::Debug::CONTEXT,
}

gfn! {
    /// Delivery on the alternate stack.
    unsafe fn run_on_alt(job: u64) -> () {
        // SAFETY: the pair `deliver_fault` passes, alive across the call.
        let job = unsafe { &mut *(job as *mut (Fault<'static>, bool)) };
        // SAFETY: moves the fault out once; the slot is not used again.
        let f = unsafe { core::ptr::read(&job.0) };
        job.1 = crate::host::guard(false, move || deliver(f));
    }
}

/// Calls `f(arg)` (guest convention) with the stack pointer at `top`.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "sysv64" fn call_on_stack(arg: u64, f: u64, top: u64) {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "mov rsp, rdx",
        "and rsp, -16",
        "call rsi",
        "mov rsp, rbp",
        "pop rbp",
        "ret",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn call_on_stack(arg: u64, f: u64, top: u64) {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "and x9, x2, #-16",
        "mov sp, x9",
        "blr x1",
        "mov sp, x29",
        "ldp x29, x30, [sp], #16",
        "ret",
    )
}

fn deliver(f: Fault<'_>) -> bool {
    let Fault {
        code,
        sig,
        si_code,
        addr,
        write,
        pc,
        k,
        ctx,
    } = f;
    if siglog() {
        crate::debug::report(format_args!(
            "signal {sig} (exception {code:#x}) at {pc:#x} {} address {addr:#x}, sp {:#x}, handler stack {:#x}",
            crate::debug::describe(pc).unwrap_or_default(),
            arch::sp(ctx),
            &si_code as *const i32 as u64
        ));
    }
    let mut si = SigInfo::new(sig, si_code);
    si.word0 = addr;
    let present = sig == SIGSEGV && committed(addr);
    if sig == SIGSEGV {
        si.code = if present { 2 } else { 1 }; // SEGV_ACCERR / SEGV_MAPERR
    }
    let mut uc = Box::new(arch::UContext::zeroed());
    arch::to_uc(
        ctx,
        &mut uc,
        (sig == SIGSEGV).then_some((addr, write, present)),
    );
    let t = tid();
    let saved = mask_of(t);
    arch::set_sigmask(&mut uc, saved);
    begin_handler(t, sig, &k);
    // SAFETY: the guest's handler for `sig`; the frame lives across the call.
    unsafe { call_handler(&k, sig, &mut si, &mut *uc as *mut _ as *mut u8) };
    set_mask_of(t, arch::sigmask(&uc));
    arch::from_uc(&uc, ctx);
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    true
}

fn committed(addr: u64) -> bool {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let mut m = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: queries the address space only.
    let n = unsafe {
        VirtualQuery(
            Some(addr as *const core::ffi::c_void),
            &mut m,
            core::mem::size_of_val(&m),
        )
    };
    n != 0 && m.State == MEM_COMMIT
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    // gregs[] indices of bionic's x86_64 mcontext_t.
    const REG_R8: usize = 0;
    const REG_EFL: usize = 17;
    const REG_CSGSFS: usize = 18;
    const REG_ERR: usize = 19;
    const REG_TRAPNO: usize = 20;
    const REG_CR2: usize = 22;

    /// bionic's x86_64 `ucontext_t` (816 bytes) with the FXSAVE area
    /// (`__fpregs_mem`) inside.
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
        assert!(core::mem::size_of::<UContext>() == 816);
        assert!(core::mem::offset_of!(UContext, gregs) == 40);
        assert!(core::mem::offset_of!(UContext, fpregs) == 224);
        assert!(core::mem::offset_of!(UContext, sigmask) == 296);
        assert!(core::mem::offset_of!(UContext, fpregs_mem) == 304);
    };

    impl UContext {
        pub fn zeroed() -> Self {
            // SAFETY: plain data; a null `fpregs` is valid.
            unsafe { core::mem::zeroed() }
        }
    }

    pub fn set_sigmask(uc: &mut UContext, m: u64) {
        uc.sigmask = m;
    }

    pub fn sigmask(uc: &UContext) -> u64 {
        uc.sigmask
    }

    pub fn pc(c: &CONTEXT) -> u64 {
        c.Rip
    }

    pub fn sp(c: &CONTEXT) -> u64 {
        c.Rsp
    }

    pub fn to_uc(c: &CONTEXT, uc: &mut UContext, fault: Option<(u64, bool, bool)>) {
        let r = [
            c.R8, c.R9, c.R10, c.R11, c.R12, c.R13, c.R14, c.R15, c.Rdi, c.Rsi, c.Rbp, c.Rbx,
            c.Rdx, c.Rax, c.Rcx, c.Rsp, c.Rip,
        ];
        for (i, v) in r.iter().enumerate() {
            uc.gregs[REG_R8 + i] = *v as i64;
        }
        uc.gregs[REG_EFL] = i64::from(c.EFlags);
        uc.gregs[REG_CSGSFS] =
            i64::from(c.SegCs) | (i64::from(c.SegGs) << 16) | (i64::from(c.SegFs) << 32);
        if let Some((addr, write, present)) = fault {
            uc.gregs[REG_TRAPNO] = 14;
            uc.gregs[REG_ERR] = 4 | if write { 2 } else { 0 } | i64::from(present);
            uc.gregs[REG_CR2] = addr as i64;
        }
        // SAFETY: FltSave is the 512-byte FXSAVE image, the layout of
        // bionic's `_libc_fpstate`.
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
        // Only the flags user code may change (CF PF AF ZF SF TF DF OF AC).
        const USER_FLAGS: u32 = 0x0000_0cd5 | 0x0000_0100 | 0x0004_0000;
        c.EFlags = (c.EFlags & !USER_FLAGS) | (g[REG_EFL] as u32 & USER_FLAGS);
        // SAFETY: as in `to_uc`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                uc.fpregs_mem.as_ptr(),
                &mut c.Anonymous.FltSave as *mut _ as *mut u8,
                512,
            )
        };
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use windows::Win32::System::Diagnostics::Debug::CONTEXT;

    const FPSIMD_MAGIC: u32 = 0x4650_8001;
    const ESR_MAGIC: u32 = 0x4553_5201;

    /// bionic's arm64 `ucontext_t` (4560 bytes): flags, link, stack, the
    /// sigset (8 bytes, padded to 128), then `struct sigcontext`
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
    struct FpsimdContext {
        magic: u32,
        size: u32,
        fpsr: u32,
        fpcr: u32,
        vregs: [[u64; 2]; 32],
    }

    const _: () = {
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

    impl UContext {
        pub fn zeroed() -> Self {
            // SAFETY: plain data.
            unsafe { core::mem::zeroed() }
        }
    }

    pub fn set_sigmask(uc: &mut UContext, m: u64) {
        uc.sigmask = m;
    }

    pub fn sigmask(uc: &UContext) -> u64 {
        uc.sigmask
    }

    pub fn pc(c: &CONTEXT) -> u64 {
        c.Pc
    }

    pub fn sp(c: &CONTEXT) -> u64 {
        c.Sp
    }

    /// The CONTEXT as the kernel's signal frame: registers, an
    /// `fpsimd_context` record and, for data aborts, an `esr_context`.
    pub fn to_uc(c: &CONTEXT, uc: &mut UContext, fault: Option<(u64, bool, bool)>) {
        // SAFETY: the X array view of the register union is always valid.
        uc.regs = unsafe { c.Anonymous.X };
        uc.sp = c.Sp;
        uc.pc = c.Pc;
        uc.pstate = u64::from(c.Cpsr);
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
        // SAFETY: the first record of the area, written by `to_uc`.
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

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn call(n: i64, a: [i64; 6]) -> i64 {
        // SAFETY: forwarded.
        unsafe { syscall(n, a) }.unwrap()
    }

    #[test]
    fn actions_round_trip_and_reject_sigkill() {
        let sig = 40; // a real-time signal no other test uses
        let k = KernelSigaction {
            handler: 0x1234,
            flags: SA_SIGINFO,
            restorer: 0x5678,
            mask: u64::MAX,
        };
        let mut old = KernelSigaction::default();
        // SAFETY: valid pointers.
        unsafe {
            assert_eq!(
                call(nr::RT_SIGACTION, [sig, &k as *const _ as i64, 0, 8, 0, 0]),
                0
            );
            assert_eq!(
                call(
                    nr::RT_SIGACTION,
                    [sig, 0, &mut old as *mut _ as i64, 8, 0, 0]
                ),
                0
            );
            assert_eq!(
                call(
                    nr::RT_SIGACTION,
                    [i64::from(SIGKILL), &k as *const _ as i64, 0, 8, 0, 0]
                ),
                -i64::from(EINVAL)
            );
            assert_eq!(
                call(nr::RT_SIGACTION, [sig, 0, 0, 16, 0, 0]),
                -i64::from(EINVAL)
            );
        }
        assert_eq!(old.handler, 0x1234);
        assert_eq!(old.mask, u64::MAX & !UNBLOCKABLE);
        let dfl = KernelSigaction::default();
        // SAFETY: valid pointer.
        unsafe { call(nr::RT_SIGACTION, [sig, &dfl as *const _ as i64, 0, 8, 0, 0]) };
    }

    #[test]
    fn blocked_signal_is_queued_for_sigtimedwait() {
        let sig = 41;
        let set = bit(sig as i32);
        let mut old = 0u64;
        let k = KernelSigaction {
            handler: 0x1000,
            flags: SA_SIGINFO,
            ..Default::default()
        };
        let me = i64::from(andruna_lx::process::gettid());
        let pid = i64::from(crate::process::guest_pid());
        let mut si = SigInfo::new(0, 0);
        let zero = andruna_lx::linux::time::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid pointers.
        unsafe {
            call(nr::RT_SIGACTION, [sig, &k as *const _ as i64, 0, 8, 0, 0]);
            assert_eq!(
                call(
                    nr::RT_SIGPROCMASK,
                    [
                        i64::from(SIG_BLOCK),
                        &set as *const u64 as i64,
                        &mut old as *mut u64 as i64,
                        8,
                        0,
                        0
                    ]
                ),
                0
            );
            // Nothing pending yet: the zero timeout expires.
            assert_eq!(
                call(
                    nr::RT_SIGTIMEDWAIT,
                    [
                        &set as *const u64 as i64,
                        0,
                        &zero as *const _ as i64,
                        8,
                        0,
                        0
                    ]
                ),
                -i64::from(EAGAIN)
            );
            assert_eq!(call(nr::TGKILL, [pid, me, sig, 0, 0, 0]), 0);
            let mut pend = 0u64;
            call(
                nr::RT_SIGPENDING,
                [&mut pend as *mut u64 as i64, 8, 0, 0, 0, 0],
            );
            assert_eq!(pend & set, set);
            assert_eq!(
                call(
                    nr::RT_SIGTIMEDWAIT,
                    [
                        &set as *const u64 as i64,
                        &mut si as *mut _ as i64,
                        0,
                        8,
                        0,
                        0
                    ]
                ),
                sig
            );
            call(
                nr::RT_SIGPROCMASK,
                [
                    i64::from(SIG_SETMASK),
                    &old as *const u64 as i64,
                    0,
                    8,
                    0,
                    0,
                ],
            );
            let dfl = KernelSigaction::default();
            call(nr::RT_SIGACTION, [sig, &dfl as *const _ as i64, 0, 8, 0, 0]);
        }
        assert_eq!(si.signo, sig as i32);
        assert_eq!(si.code, SI_TKILL);
    }

    static SEEN: AtomicUsize = AtomicUsize::new(0);

    gfn! {
        unsafe fn on_signal(sig: i32, si: *mut SigInfo, _uc: *mut u8) -> () {
            // SAFETY: the siginfo passed by the delivery.
            let s = unsafe { (*si).signo };
            assert_eq!(s, sig);
            // The signal is blocked while its handler runs.
            assert_ne!(mask_of(tid()) & bit(sig), 0);
            SEEN.fetch_add(sig as usize, Ordering::SeqCst);
        }
    }

    #[test]
    fn unblocked_signal_to_self_runs_the_handler() {
        let sig = 42;
        let k = KernelSigaction {
            handler: on_signal as usize as u64,
            flags: SA_SIGINFO,
            ..Default::default()
        };
        let me = i64::from(andruna_lx::process::gettid());
        let pid = i64::from(crate::process::guest_pid());
        // SAFETY: valid pointers; the handler is a guest-convention function.
        unsafe {
            call(nr::RT_SIGACTION, [sig, &k as *const _ as i64, 0, 8, 0, 0]);
            assert_eq!(call(nr::TGKILL, [pid, me, sig, 0, 0, 0]), 0);
            let dfl = KernelSigaction::default();
            call(nr::RT_SIGACTION, [sig, &dfl as *const _ as i64, 0, 8, 0, 0]);
        }
        assert_eq!(SEEN.load(Ordering::SeqCst), 42);
        assert_eq!(mask_of(tid()) & bit(sig as i32), 0);
    }

    #[test]
    fn altstack_round_trip() {
        let ss = StackT {
            sp: 0x10000,
            flags: 0,
            size: 65536,
        };
        let mut old = StackT::default();
        // SAFETY: valid pointers.
        unsafe {
            assert_eq!(
                call(nr::SIGALTSTACK, [&ss as *const _ as i64, 0, 0, 0, 0, 0]),
                0
            );
            assert_eq!(
                call(nr::SIGALTSTACK, [0, &mut old as *mut _ as i64, 0, 0, 0, 0]),
                0
            );
        }
        assert_eq!((old.sp, old.size, old.flags), (0x10000, 65536, 0));
    }
}

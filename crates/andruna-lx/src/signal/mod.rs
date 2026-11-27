//! Signals: actions, masks, alternate stacks, pending signals and delivery.
//!
//! # Model
//!
//! - Actions (`rt_sigaction`) are process-wide; the blocked mask, the
//!   alternate stack (`sigaltstack`) and thread-directed pending signals are
//!   per thread ([`ThreadSignals`]).
//! - Synchronous signals (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`,
//!   `SIGTRAP`) come from Windows exceptions: the host's first vectored
//!   exception handler calls [`handle_exception`] (andruna-linker does this
//!   after its FS-base restore; a host without one calls
//!   [`install_handler`]).
//! - A handler runs as on Linux: lx writes a Linux signal frame
//!   (`siginfo_t`, `ucontext_t`, the restorer as return address) on the
//!   alternate stack when the action has `SA_ONSTACK` and one is set, or
//!   below the interrupted stack otherwise, and resumes the thread at the
//!   handler. The handler returns to the restorer (bionic's `__restore_rt`
//!   on x86_64, lx's own trampoline otherwise), whose `rt_sigreturn`
//!   resumes the context the handler left in the `ucontext_t`.
//! - Only the exception dispatch of Windows uses the faulting stack before
//!   the switch: the handler, its frame and everything it calls run on the
//!   alternate stack. This is what keeps ART's implicit stack-overflow
//!   check working (`docs/plan/phase1/lx-conformance.md` has the
//!   measurements).
//! - Signals sent with `kill`, `tkill`, `tgkill`, `rt_sigqueueinfo` and
//!   `rt_tgsigqueueinfo` are queued and delivered when the target thread
//!   leaves an lx system call (or sleeps in one, which then returns
//!   `EINTR`). Thread suspension stays cooperative (S1): a thread that runs
//!   without making system calls sees its signals at the next call.
//!
//! # Deviations from Linux
//!
//! - Asynchronous delivery is at system call boundaries only (above).
//! - A fault inside lx itself (for example on a bad buffer that lx reads)
//!   is delivered as `SIGSEGV`; Linux would return `EFAULT` from the call.
//! - Default actions are Linux's: "terminate" and "core" end the process
//!   at once with an exit code that encodes the signal
//!   ([`signal_exit_code`]; Windows has no "killed by signal" status, and
//!   no core file is written), "stop" suspends every thread of the process
//!   (`NtSuspendProcess`) until something outside resumes it, and
//!   "ignore" discards the signal. A synchronous fault (`SIGSEGV`,
//!   `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGTRAP`) in guest code whose action
//!   is the default, ignored or blocked ends the process the same way, as
//!   Linux forces the default action then. A fault in host code (a
//!   Windows image) is left to Windows' own exception handling.
//! - `SIGCONT` sent inside the process cannot continue it (every thread is
//!   stopped); only another process can resume a stopped one.
//! - Only this process exists for `kill`; any other pid is `ESRCH`.
//! - A signal that becomes deliverable through `rt_sigreturn`'s mask
//!   restore is delivered at the thread's next system call, not at once.
//! - `rt_sigreturn` as a system call needs the `ucontext_t` address as its
//!   first argument (Andruna's x86_64 `__restore_rt` passes it); on arm64
//!   the restorer is lx's trampoline and the system call is `ENOSYS`.

mod frame;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub use frame::{DELIVERED, LX_SIGNAL, handle_exception, install_handler, sigreturn};

use windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS;

use crate::errno::{Errno, SysResult};
use crate::linux::signal::*;
use crate::linux::time::Timespec;
use crate::sys::sync as os;
use crate::util::lock;

/// The process's actions, indexed by signal number.
static ACTIONS: Mutex<[KernelSigaction; NSIG]> = Mutex::new(
    [KernelSigaction {
        handler: SIG_DFL,
        flags: 0,
        restorer: 0,
        mask: 0,
    }; NSIG],
);

/// Number of signals queued anywhere, for a cheap check at system call
/// exit.
static QUEUED: AtomicUsize = AtomicUsize::new(0);

/// Process-directed pending signals.
static PROCESS_PENDING: Mutex<Vec<SigInfo>> = Mutex::new(Vec::new());

/// Signal state of the threads that used signals or were sent one, by tid.
static THREADS: Mutex<Option<HashMap<i32, Arc<ThreadSignals>>>> = Mutex::new(None);

/// One thread's signal state.
#[derive(Debug)]
pub struct ThreadSignals {
    tid: i32,
    blocked: AtomicU64,
    pending: Mutex<Vec<SigInfo>>,
    alt_sp: AtomicU64,
    alt_size: AtomicU64,
    alt_flags: AtomicI32,
    /// The address this thread sleeps on in an interruptible wait, or 0.
    wait_addr: AtomicUsize,
    /// Changed and woken by senders when the thread sleeps on it.
    wake_word: AtomicU32,
}

impl ThreadSignals {
    fn new(tid: i32) -> Self {
        Self {
            tid,
            blocked: AtomicU64::new(0),
            pending: Mutex::new(Vec::new()),
            alt_sp: AtomicU64::new(0),
            alt_size: AtomicU64::new(0),
            alt_flags: AtomicI32::new(SS_DISABLE),
            wait_addr: AtomicUsize::new(0),
            wake_word: AtomicU32::new(0),
        }
    }

    pub fn blocked(&self) -> SigSet {
        self.blocked.load(Ordering::SeqCst)
    }

    fn set_blocked(&self, m: SigSet) {
        self.blocked.store(m & !UNBLOCKABLE, Ordering::SeqCst);
    }

    /// The alternate stack: (sp, size, flags).
    fn alt(&self) -> (u64, u64, i32) {
        (
            self.alt_sp.load(Ordering::Relaxed),
            self.alt_size.load(Ordering::Relaxed),
            self.alt_flags.load(Ordering::Relaxed),
        )
    }

    fn set_alt(&self, sp: u64, size: u64, flags: i32) {
        self.alt_sp.store(sp, Ordering::Relaxed);
        self.alt_size.store(size, Ordering::Relaxed);
        self.alt_flags.store(flags, Ordering::Relaxed);
    }

    /// Wakes the thread if it sleeps in an interruptible wait.
    fn kick(&self) {
        self.wake_word.fetch_add(1, Ordering::SeqCst);
        let a = self.wait_addr.load(Ordering::SeqCst);
        if a != 0 {
            os::wake_all(a);
        }
    }
}

/// Removes the thread's entry when the thread ends.
struct Registration(Arc<ThreadSignals>);

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(m) = lock(&THREADS).as_mut()
            && m.get(&self.0.tid).is_some_and(|t| Arc::ptr_eq(t, &self.0))
        {
            m.remove(&self.0.tid);
        }
    }
}

thread_local! {
    static ME: std::cell::OnceCell<Registration> = const { std::cell::OnceCell::new() };
}

fn entry(tid: i32) -> Arc<ThreadSignals> {
    let mut g = lock(&THREADS);
    g.get_or_insert_with(HashMap::new)
        .entry(tid)
        .or_insert_with(|| Arc::new(ThreadSignals::new(tid)))
        .clone()
}

/// The calling thread's signal state.
pub fn current() -> Arc<ThreadSignals> {
    ME.with(|m| {
        m.get_or_init(|| Registration(entry(crate::process::gettid())))
            .0
            .clone()
    })
}

/// Gives the thread `tid`, created by the calling thread and not started
/// yet, the caller's signal mask (Linux threads inherit it; the alternate
/// stack is not inherited).
pub fn inherit_to(tid: i32) {
    let mask = current().blocked();
    entry(tid).set_blocked(mask);
}

/// The calling thread's state if it exists already (no allocation).
fn current_if_any() -> Option<Arc<ThreadSignals>> {
    ME.try_with(|m| m.get().map(|r| r.0.clone())).ok().flatten()
}

fn check_sig(sig: i32) -> SysResult<usize> {
    if (1..=SIGRTMAX).contains(&sig) {
        Ok(sig as usize)
    } else {
        Err(Errno::EINVAL)
    }
}

fn check_setsize(n: usize) -> SysResult<()> {
    if n == 8 { Ok(()) } else { Err(Errno::EINVAL) }
}

/// The action of `sig`.
pub fn action(sig: i32) -> KernelSigaction {
    lock(&ACTIONS)[sig as usize]
}

/// Resets `sig` to its default action (`SA_RESETHAND`).
fn reset_action(sig: i32) {
    let mut a = lock(&ACTIONS);
    a[sig as usize].handler = SIG_DFL;
    a[sig as usize].flags &= !SA_SIGINFO;
}

/// `rt_sigaction`.
pub fn sigaction(
    sig: i32,
    new: Option<KernelSigaction>,
    sigsetsize: usize,
) -> SysResult<KernelSigaction> {
    check_setsize(sigsetsize)?;
    let i = check_sig(sig)?;
    if new.is_some() && (sig == SIGKILL || sig == SIGSTOP) {
        return Err(Errno::EINVAL);
    }
    let mut a = lock(&ACTIONS);
    let old = a[i];
    if let Some(mut n) = new {
        n.mask &= !UNBLOCKABLE;
        a[i] = n;
        drop(a);
        if n.handler == SIG_IGN || (n.handler == SIG_DFL && default_ignores(sig)) {
            // POSIX: setting an action to ignore discards pending instances.
            discard_pending(sig);
        }
    }
    Ok(old)
}

fn discard_pending(sig: i32) {
    let mut n = 0;
    lock(&PROCESS_PENDING).retain(|i| {
        let keep = i.si_signo != sig;
        n += usize::from(!keep);
        keep
    });
    if let Some(m) = lock(&THREADS).as_ref() {
        for t in m.values() {
            lock(&t.pending).retain(|i| {
                let keep = i.si_signo != sig;
                n += usize::from(!keep);
                keep
            });
        }
    }
    QUEUED.fetch_sub(n, Ordering::SeqCst);
}

/// `rt_sigprocmask` for the calling thread; returns the old mask.
pub fn sigprocmask(how: i32, set: Option<SigSet>, sigsetsize: usize) -> SysResult<SigSet> {
    check_setsize(sigsetsize)?;
    let t = current();
    let old = t.blocked();
    if let Some(s) = set {
        let new = match how {
            SIG_BLOCK => old | s,
            SIG_UNBLOCK => old & !s,
            SIG_SETMASK => s,
            _ => return Err(Errno::EINVAL),
        };
        t.set_blocked(new);
    }
    Ok(old)
}

/// `sigaltstack` for the calling thread; returns the old setting.
pub fn sigaltstack(new: Option<StackT>, current_sp: u64) -> SysResult<StackT> {
    let t = current();
    let (sp, size, flags) = t.alt();
    let enabled = flags & SS_DISABLE == 0 && size != 0;
    let on = enabled && current_sp >= sp && current_sp < sp + size;
    let old = StackT {
        ss_sp: sp,
        ss_flags: if !enabled {
            SS_DISABLE
        } else if on {
            SS_ONSTACK
        } else {
            flags & SS_AUTODISARM
        },
        _pad: 0,
        ss_size: size,
    };
    if let Some(n) = new {
        if on {
            return Err(Errno::EPERM);
        }
        let mode = n.ss_flags & !SS_AUTODISARM;
        if mode == SS_DISABLE {
            t.set_alt(0, 0, SS_DISABLE);
        } else if mode == 0 || mode == SS_ONSTACK {
            if (n.ss_size as usize) < MINSIGSTKSZ {
                return Err(Errno::ENOMEM);
            }
            t.set_alt(n.ss_sp, n.ss_size, n.ss_flags & SS_AUTODISARM);
        } else {
            return Err(Errno::EINVAL);
        }
    }
    Ok(old)
}

/// The signals pending for the calling thread or the process
/// (`rt_sigpending`).
pub fn sigpending() -> SigSet {
    let t = current();
    let mut m = 0;
    for i in lock(&t.pending).iter() {
        m |= sigbit(i.si_signo);
    }
    for i in lock(&PROCESS_PENDING).iter() {
        m |= sigbit(i.si_signo);
    }
    m & t.blocked()
}

fn default_ignores(sig: i32) -> bool {
    matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH)
}

fn default_stops(sig: i32) -> bool {
    matches!(sig, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}

/// Whether a signal sent now is discarded at once (ignored and not
/// blocked anywhere it could be waited for).
fn ignored(sig: i32) -> bool {
    let a = action(sig);
    a.handler == SIG_IGN || (a.handler == SIG_DFL && default_ignores(sig))
}

/// Queues `info` for one thread or the process and wakes the receiver.
fn queue(target: Option<&Arc<ThreadSignals>>, info: SigInfo) {
    let sig = info.si_signo;
    if sig == SIGKILL {
        terminate(sig);
    }
    // Blocked signals are queued even when ignored, as on Linux, so that
    // sigwait can take them; unblocked ignored ones are dropped.
    let blocked = match target {
        Some(t) => t.blocked() & sigbit(sig) != 0,
        // Process signals are queued only when the sender blocks them.
        None => true,
    };
    // A stop signal discards pending SIGCONT, and SIGCONT discards pending
    // stop signals, whatever their actions (signal(7)).
    if sig == SIGCONT {
        for s in [SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU] {
            discard_pending(s);
        }
    } else if default_stops(sig) {
        discard_pending(SIGCONT);
    }
    if sig == SIGSTOP {
        // Cannot be caught, blocked or ignored.
        stop();
        return;
    }
    if ignored(sig) && !blocked {
        return;
    }
    let list = match target {
        Some(t) => &t.pending,
        None => &PROCESS_PENDING,
    };
    {
        let mut l = lock(list);
        // Standard signals do not queue: one instance at most.
        if sig < SIGRTMIN && l.iter().any(|i| i.si_signo == sig) {
            return;
        }
        l.push(info);
        QUEUED.fetch_add(1, Ordering::SeqCst);
    }
    match target {
        Some(t) => t.kick(),
        None => {
            if let Some(m) = lock(&THREADS).as_ref() {
                for t in m.values() {
                    if t.blocked() & sigbit(sig) == 0 || t.wait_addr.load(Ordering::SeqCst) != 0 {
                        t.kick();
                    }
                }
            }
        }
    }
}

/// The high half of the exit code of a process ended by a signal: an
/// NTSTATUS error code with the customer bit set (`0xE...`), which neither
/// `exit_group` (0 to 255) nor a Windows exception code can produce.
pub const SIGNAL_EXIT_BASE: u32 = 0xE0A0_0000;
/// The bit of [`signal_exit_code`] set when the default action is "core",
/// as in a Linux wait status (`WCOREDUMP`).
pub const SIGNAL_EXIT_CORE: u32 = 0x80;

/// The Windows exit code of a process ended by `sig`: [`SIGNAL_EXIT_BASE`]
/// plus the Linux wait status of a signal death (the signal number in bits
/// 0 to 6, [`SIGNAL_EXIT_CORE`] when the default action dumps core).
pub fn signal_exit_code(sig: i32) -> u32 {
    let core = if default_dumps_core(sig) {
        SIGNAL_EXIT_CORE
    } else {
        0
    };
    SIGNAL_EXIT_BASE | core | (sig as u32 & 0x7f)
}

/// What a process exit code means: the inverse of [`signal_exit_code`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// `exit_group(code)`.
    Exited(u8),
    /// Ended by a signal's default action.
    Signaled { sig: i32, core: bool },
    /// Anything else: a Windows status (an unhandled exception in host
    /// code, a forced termination).
    Other(u32),
}

/// Decodes a process exit code (for supervisors such as sysd).
pub fn decode_exit_code(code: u32) -> ExitStatus {
    if code <= 0xff {
        ExitStatus::Exited(code as u8)
    } else if code & 0xffff_ff00 == SIGNAL_EXIT_BASE && code & 0x7f != 0 {
        ExitStatus::Signaled {
            sig: (code & 0x7f) as i32,
            core: code & SIGNAL_EXIT_CORE != 0,
        }
    } else {
        ExitStatus::Other(code)
    }
}

/// Signals whose default action is "core".
fn default_dumps_core(sig: i32) -> bool {
    matches!(
        sig,
        SIGQUIT
            | SIGILL
            | SIGTRAP
            | SIGABRT
            | SIGBUS
            | SIGFPE
            | SIGSEGV
            | SIGXCPU
            | SIGXFSZ
            | SIGSYS
    )
}

/// A hook the host runs before a signal ends the process: the signal and,
/// for a fault, the exception (null for a queued signal).
pub type FatalHook = fn(i32, *mut EXCEPTION_POINTERS);

static ON_FATAL: std::sync::OnceLock<FatalHook> = std::sync::OnceLock::new();

/// Registers `f`, run before a signal's default action ends the process
/// (andruna-linker prints its crash report there). The first registration
/// stays.
pub fn set_fatal_hook(f: FatalHook) {
    let _ = ON_FATAL.set(f);
}

/// Ends the process for a signal whose default action terminates it
/// ("terminate" or "core").
pub fn terminate(sig: i32) -> ! {
    terminate_after(sig, core::ptr::null_mut())
}

/// [`terminate`] for a fault: `ep` is the exception that raised it.
pub(crate) fn terminate_after(sig: i32, ep: *mut EXCEPTION_POINTERS) -> ! {
    use std::io::Write;
    if let Some(f) = ON_FATAL.get() {
        f(sig, ep);
    }
    let _ = writeln!(
        std::io::stderr(),
        "[andruna-lx] process terminated by signal {sig}"
    );
    let _ = std::io::stderr().flush();
    crate::sys::thread::terminate_process(signal_exit_code(sig))
}

/// The default action "stop": suspends the whole process. Returns when
/// another process resumes it.
fn stop() {
    crate::sys::thread::suspend_process();
}

/// Takes the next signal the thread can receive: thread-directed first,
/// lowest number first, only those not in `blocked` (or, when `want` is
/// given, only those in `want`).
fn dequeue(t: &ThreadSignals, blocked: SigSet, want: Option<SigSet>) -> Option<SigInfo> {
    if QUEUED.load(Ordering::SeqCst) == 0 {
        return None;
    }
    let ok = |i: &SigInfo| {
        let b = sigbit(i.si_signo);
        match want {
            Some(w) => w & b != 0,
            None => blocked & b == 0,
        }
    };
    for list in [&t.pending, &PROCESS_PENDING] {
        let mut l = lock(list);
        let best = l
            .iter()
            .enumerate()
            .filter(|(_, i)| ok(i))
            .min_by_key(|(_, i)| i.si_signo)
            .map(|(n, _)| n);
        if let Some(n) = best {
            QUEUED.fetch_sub(1, Ordering::SeqCst);
            return Some(l.remove(n));
        }
    }
    None
}

/// Whether the calling thread has a signal it would receive now with mask
/// `blocked` (a handler to run or a default action to take).
fn deliverable(t: &ThreadSignals, blocked: SigSet) -> bool {
    if QUEUED.load(Ordering::SeqCst) == 0 {
        return false;
    }
    let any = |l: &Mutex<Vec<SigInfo>>| lock(l).iter().any(|i| blocked & sigbit(i.si_signo) == 0);
    any(&t.pending) || any(&PROCESS_PENDING)
}

/// For interruptible waits: whether a signal is waiting for the calling
/// thread, which makes the wait return `EINTR`.
pub fn interrupted() -> bool {
    if QUEUED.load(Ordering::SeqCst) == 0 {
        return false;
    }
    match current_if_any() {
        Some(t) => deliverable(&t, t.blocked()),
        None => false,
    }
}

/// Registers the calling thread as sleeping on `addr` until the returned
/// guard is dropped, so that senders wake it.
pub fn sleeping_on(addr: usize) -> Option<impl Drop> {
    struct Guard(Arc<ThreadSignals>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.wait_addr.store(0, Ordering::SeqCst);
        }
    }
    let t = current_if_any()?;
    t.wait_addr.store(addr, Ordering::SeqCst);
    Some(Guard(t))
}

/// Delivers the signals the calling thread can receive now: runs handlers
/// (through [`frame::raise`]) and default actions. Called when a system
/// call returns.
pub fn deliver_pending() {
    if QUEUED.load(Ordering::SeqCst) == 0 {
        return;
    }
    // A thread that never touched signals can still have been sent one
    // (its state exists by tid) or get a process signal: `current` finds
    // or creates its state.
    let t = current_if_any().unwrap_or_else(current);
    deliver_with(&t, None);
}

/// Delivers pending signals; `saved_mask` is the mask the handlers' frames
/// restore (`rt_sigsuspend` passes the mask from before the call).
fn deliver_with(t: &Arc<ThreadSignals>, saved_mask: Option<SigSet>) -> bool {
    let mut ran = false;
    while let Some(info) = dequeue(t, t.blocked(), None) {
        let sig = info.si_signo;
        let a = action(sig);
        match a.handler {
            SIG_IGN => {}
            SIG_DFL => {
                if default_ignores(sig) {
                    continue;
                }
                if default_stops(sig) {
                    stop();
                    continue;
                }
                terminate(sig);
            }
            _ => {
                frame::raise(&info, saved_mask.unwrap_or_else(|| t.blocked()));
                ran = true;
            }
        }
    }
    ran
}

fn sender_info(sig: i32, code: i32) -> SigInfo {
    SigInfo::kill(sig, code, crate::process::getpid(), crate::process::uid())
}

/// Finds the state of thread `tid` of this process.
fn thread_of(tid: i32) -> SysResult<Arc<ThreadSignals>> {
    if tid <= 0 {
        return Err(Errno::EINVAL);
    }
    if tid != crate::process::gettid() && !crate::sys::thread::is_own_thread(tid as u32) {
        return Err(Errno::ESRCH);
    }
    Ok(entry(tid))
}

/// `tgkill` / `tkill`.
pub fn tgkill(tgid: Option<i32>, tid: i32, sig: i32, info: Option<SigInfo>) -> SysResult<()> {
    if let Some(g) = tgid
        && g <= 0
    {
        return Err(Errno::EINVAL);
    }
    if sig != 0 {
        check_sig(sig)?;
    }
    if tid <= 0 {
        return Err(Errno::EINVAL);
    }
    if tgid.is_some_and(|g| g != crate::process::getpid()) {
        return Err(Errno::ESRCH);
    }
    let t = thread_of(tid)?;
    if sig == 0 {
        return Ok(());
    }
    let info = info.unwrap_or_else(|| sender_info(sig, SI_TKILL));
    queue(
        Some(&t),
        SigInfo {
            si_signo: sig,
            ..info
        },
    );
    Ok(())
}

/// `kill`. This process is the only one lx knows.
pub fn kill(pid: i32, sig: i32, info: Option<SigInfo>) -> SysResult<()> {
    if sig != 0 {
        check_sig(sig)?;
    }
    let me = crate::process::getpid();
    if !(pid == me || pid == 0 || pid == -1 || pid == -me) {
        return Err(Errno::ESRCH);
    }
    if sig == 0 {
        return Ok(());
    }
    let info = info.unwrap_or_else(|| sender_info(sig, SI_USER));
    // Linux prefers the calling thread for a process signal it does not
    // block, which makes kill(getpid(), sig) synchronous for it.
    let t = current();
    if t.blocked() & sigbit(sig) == 0 {
        queue(
            Some(&t),
            SigInfo {
                si_signo: sig,
                ..info
            },
        );
    } else {
        queue(
            None,
            SigInfo {
                si_signo: sig,
                ..info
            },
        );
    }
    Ok(())
}

/// Validates a user-supplied `siginfo_t` for `rt_sigqueueinfo`: a process
/// may not pretend to be the kernel or `kill` for another process.
pub fn check_queue_info(info: &SigInfo) -> SysResult<()> {
    if (info.si_code >= 0 || info.si_code == SI_TKILL)
        && crate::process::getpid() != (info.fields[0] as u32) as i32
    {
        return Err(Errno::EPERM);
    }
    Ok(())
}

/// Sleeps until `cond()` holds, a signal arrives (`EINTR` when
/// `interruptible`), or `deadline_ns` (monotonic) passes (`EAGAIN`).
fn sleep_until(
    t: &Arc<ThreadSignals>,
    mut cond: impl FnMut() -> bool,
    deadline_ns: Option<i128>,
) -> SysResult<()> {
    let addr = t.wake_word.as_ptr() as usize;
    t.wait_addr.store(addr, Ordering::SeqCst);
    let r = loop {
        let v = t.wake_word.load(Ordering::SeqCst);
        if cond() {
            break Ok(());
        }
        let ms = match deadline_ns {
            None => None,
            Some(d) => {
                let left = d - crate::time::monotonic_ns();
                if left <= 0 {
                    break Err(Errno::EAGAIN);
                }
                Some(((left + 999_999) / 1_000_000).min(i128::from(u32::MAX - 1)) as u32)
            }
        };
        os::wait_on_address(&t.wake_word, v, ms);
    };
    t.wait_addr.store(0, Ordering::SeqCst);
    r
}

/// `rt_sigtimedwait`: takes a pending signal of `set`; `EAGAIN` on
/// timeout, `EINTR` when a signal outside `set` is delivered.
pub fn sigtimedwait(
    set: SigSet,
    timeout: Option<&Timespec>,
    sigsetsize: usize,
) -> SysResult<SigInfo> {
    check_setsize(sigsetsize)?;
    let set = set & !UNBLOCKABLE;
    let deadline = match timeout {
        None => None,
        Some(ts) => {
            if !ts.is_valid_duration() {
                return Err(Errno::EINVAL);
            }
            Some(crate::time::monotonic_ns() + ts.as_nanos())
        }
    };
    let t = current();
    let mut got = None;
    let mut eintr = false;
    sleep_until(
        &t,
        || {
            if let Some(i) = dequeue(&t, 0, Some(set)) {
                got = Some(i);
                return true;
            }
            // A deliverable signal outside the set interrupts the wait.
            if deliverable(&t, t.blocked() | set) {
                eintr = true;
                return true;
            }
            false
        },
        deadline,
    )?;
    match got {
        Some(i) => Ok(i),
        None if eintr => Err(Errno::EINTR),
        None => Err(Errno::EAGAIN),
    }
}

/// `rt_sigsuspend`: waits with `mask` until a handler ran (or a default
/// action ended the process); always `EINTR`.
pub fn sigsuspend(mask: SigSet, sigsetsize: usize) -> SysResult<()> {
    check_setsize(sigsetsize)?;
    let t = current();
    let old = t.blocked();
    t.set_blocked(mask);
    loop {
        let _ = sleep_until(&t, || deliverable(&t, t.blocked()), None);
        // The handlers' frames restore the mask from before the call.
        if deliver_with(&t, Some(old)) {
            break;
        }
    }
    t.set_blocked(old);
    Err(Errno::EINTR)
}

/// Delivers the calling thread's queued signals after a system call made
/// with [`crate::syscall_without_delivery`], on the stack the caller runs
/// on again (the host's system call entry calls it after switching back).
pub fn deliver_after_syscall() {
    deliver_at_syscall_exit();
}

/// Saves the calling thread's lx `errno` around signal delivery at system
/// call exit, so that handlers cannot change the call's result.
pub(crate) fn deliver_at_syscall_exit() {
    if QUEUED.load(Ordering::SeqCst) == 0 {
        return;
    }
    let e = crate::errno::errno();
    deliver_pending();
    crate::errno::set_errno(Errno(e));
}

/// Queues the signal of a POSIX timer expiry (`si_code == SI_TIMER`, the
/// timer id in `si_timerid`) for thread `tid` or the process, unless the
/// same timer's previous signal is still pending. Returns whether it was
/// queued; when it was not, the caller counts an overrun, as Linux does.
pub(crate) fn queue_timer(tid: Option<i32>, info: SigInfo) -> SysResult<bool> {
    let target = match tid {
        Some(t) => Some(thread_of(t)?),
        None => None,
    };
    let timer = info.fields[0] as u32;
    let same = |i: &SigInfo| {
        i.si_code == SI_TIMER && i.si_signo == info.si_signo && i.fields[0] as u32 == timer
    };
    let pending = target
        .as_ref()
        .is_some_and(|t| lock(&t.pending).iter().any(same))
        || lock(&PROCESS_PENDING).iter().any(same);
    if pending {
        return Ok(false);
    }
    queue(target.as_ref(), info);
    Ok(true)
}

#[cfg(test)]
mod tests;

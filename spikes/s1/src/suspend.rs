//! Thread suspension experiments.
//!
//! - `Inject`: SuspendThread + GetThreadContext + SetThreadContext redirect
//!   the thread into a trampoline that captures its registers, runs the
//!   checkpoint and resumes with RtlRestoreContext (signal-style).
//! - `Flag`: cooperative polling of a flag (ART's explicit suspend check).
//! - `Page`: cooperative load from a suspend-check page that the requester
//!   protects to force a fault (ART's implicit suspend check, global form).
//! - `StopOnly`: SuspendThread + GetThreadContext only (stop and read the
//!   context, as a stack dumper or a conservative collector needs).

use crate::{arch, util};
use core::cell::Cell;
use core::ffi::c_void;
use std::os::windows::io::AsRawHandle;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;
use windows::core::s;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    GetThreadContext, RtlRestoreContext, SetThreadContext, CONTEXT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
    PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, ResumeThread, SetEvent, SetThreadAffinityMask, SuspendThread,
    SwitchToThread, WaitForSingleObject, WaitOnAddress, WakeByAddressAll, INFINITE,
};

/// Address of ntdll!RtlCaptureContext, called directly by the injection
/// trampoline (a wrapper would capture the wrapper's registers).
pub static S1_RTL_CAPTURE_CONTEXT: AtomicUsize = AtomicUsize::new(0);

pub fn init() {
    // SAFETY: ntdll is always loaded; the name is a valid C string.
    let p = unsafe {
        let m = GetModuleHandleA(s!("ntdll.dll")).expect("ntdll");
        GetProcAddress(m, s!("RtlCaptureContext"))
    };
    S1_RTL_CAPTURE_CONTEXT.store(p.expect("RtlCaptureContext") as usize, Ordering::Release);
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct AlignedContext(pub CONTEXT);

impl AlignedContext {
    pub fn new(flags: windows::Win32::System::Diagnostics::Debug::CONTEXT_FLAGS) -> Box<Self> {
        // SAFETY: CONTEXT is plain data; all-zero is valid.
        let mut b: Box<Self> = Box::new(unsafe { core::mem::zeroed() });
        b.0.ContextFlags = flags;
        b
    }
}

const MAX_THREADS: usize = 64;

struct Slot {
    pc: AtomicUsize,
    sp: AtomicUsize,
}
static SLOTS: [Slot; MAX_THREADS] = [const {
    Slot {
        pc: AtomicUsize::new(0),
        sp: AtomicUsize::new(0),
    }
}; MAX_THREADS];

thread_local! {
    static MY_ID: Cell<usize> = const { Cell::new(usize::MAX) };
}

static ACK: AtomicUsize = AtomicUsize::new(0);
static EXITED: AtomicUsize = AtomicUsize::new(0);
static EPOCH: AtomicU32 = AtomicU32::new(0);
static FLAG: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static BLOCKING: AtomicBool = AtomicBool::new(true);
static PAGE: AtomicUsize = AtomicUsize::new(0);

pub fn is_suspend_page(addr: usize) -> bool {
    let p = PAGE.load(Ordering::Relaxed);
    p != 0 && addr >= p && addr < p + 4096
}

/// The checkpoint: acknowledge, then (when blocking) stay suspended until
/// the requester bumps the epoch. Uses only atomics and SwitchToThread, so
/// it is safe at an arbitrary instruction (async-signal-safe).
pub fn checkpoint() {
    let e = EPOCH.load(Ordering::Acquire);
    ACK.fetch_add(1, Ordering::AcqRel);
    if BLOCKING.load(Ordering::Acquire) {
        // Block like a futex wait (what lx's futex does), so suspended
        // threads do not compete for CPU with threads still running.
        while EPOCH.load(Ordering::Acquire) == e {
            // SAFETY: waits while EPOCH still equals `e`; both pointers are
            // valid for 4 bytes.
            let _ = unsafe {
                WaitOnAddress(
                    EPOCH.as_ptr() as *const c_void,
                    &e as *const u32 as *const c_void,
                    4,
                    None,
                )
            };
        }
    }
    EXITED.fetch_add(1, Ordering::AcqRel);
}

/// Entered from `s1_inject_tramp` with the captured context. Runs the
/// checkpoint and resumes the interrupted code. Never returns.
#[no_mangle]
pub unsafe extern "C" fn s1_inject_entry(ctx: *mut CONTEXT, extra: usize) -> ! {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let id = MY_ID.with(|c| c.get());
        let s = &SLOTS[id];
        // SAFETY: ctx points to the trampoline's CONTEXT on this stack.
        let c = unsafe { &mut *ctx };
        arch::inject_fixup(
            c,
            extra,
            s.pc.load(Ordering::Acquire),
            s.sp.load(Ordering::Acquire),
        );
        checkpoint();
    }));
    if r.is_err() {
        std::process::abort();
    }
    // SAFETY: the context is complete (captured registers, original pc/sp).
    unsafe { RtlRestoreContext(ctx, None) };
    std::process::abort()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Inject,
    Flag,
    Page,
    StopOnly,
}

#[inline(always)]
fn work(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

fn worker<const M: u8>(id: usize, iters: &AtomicUsize) {
    MY_ID.with(|c| c.set(id));
    let mut x = id as u64;
    let mut n = 0usize;
    let page = PAGE.load(Ordering::Acquire) as *const u8;
    while !STOP.load(Ordering::Relaxed) {
        for _ in 0..64 {
            x = work(core::hint::black_box(x));
            match M {
                1 => {
                    if FLAG.load(Ordering::Relaxed) != 0 {
                        checkpoint();
                    }
                }
                2 => {
                    // SAFETY: the suspend-check page is valid memory; when
                    // protected the load faults and the SIGSEGV handler runs
                    // the checkpoint.
                    unsafe { core::ptr::read_volatile(page) };
                }
                _ => {}
            }
        }
        n += 64;
    }
    iters.fetch_add(n, Ordering::Relaxed);
    core::hint::black_box(x);
}

fn wait_count(c: &AtomicUsize, n: usize) {
    let mut spins = 0u32;
    while c.load(Ordering::Acquire) < n {
        spins += 1;
        if spins > 256 {
            // SAFETY: plain yield, gives the CPU to workers that must ack.
            unsafe {
                let _ = SwitchToThread();
            }
        } else {
            core::hint::spin_loop();
        }
    }
}

fn ensure_page() -> usize {
    let p = PAGE.load(Ordering::Acquire);
    if p != 0 {
        return p;
    }
    // SAFETY: fresh private page.
    let p = unsafe { VirtualAlloc(None, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) } as usize;
    PAGE.store(p, Ordering::Release);
    p
}

fn protect_page(p: usize, prot: windows::Win32::System::Memory::PAGE_PROTECTION_FLAGS) {
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: our own page.
    unsafe { VirtualProtect(p as *const c_void, 4096, prot, &mut old) }.expect("VirtualProtect");
}

/// Redirects thread `h` (worker `id`) into the injection trampoline.
fn inject(h: HANDLE, id: usize, ctx: &mut AlignedContext) {
    // SAFETY: `h` is a live worker thread handle with suspend rights.
    unsafe { SuspendThread(h) };
    redirect_suspended(h, id, ctx);
    // SAFETY: resumes the thread suspended above.
    unsafe { ResumeThread(h) };
}

/// Changes the context of the suspended thread `h` to enter the trampoline.
fn redirect_suspended(h: HANDLE, id: usize, ctx: &mut AlignedContext) {
    // SAFETY: `h` is suspended; its context is read, changed and written
    // back before it resumes.
    unsafe {
        ctx.0.ContextFlags = arch::CONTEXT_CONTROL;
        GetThreadContext(h, &mut ctx.0).expect("GetThreadContext");
        let (pc, sp) =
            arch::inject_redirect(&mut ctx.0, &arch::s1_inject_tramp as *const u8 as usize);
        SLOTS[id].pc.store(pc, Ordering::Release);
        SLOTS[id].sp.store(sp, Ordering::Release);
        SetThreadContext(h, &ctx.0).expect("SetThreadContext");
    }
}

pub struct LatencyResult {
    pub mode: Mode,
    pub threads: usize,
    pub median_us: f64,
    pub p99_us: f64,
}

/// Measures the time until all `n` threads are stopped (acknowledged).
pub fn latency(mode: Mode, n: usize, trials: usize) -> LatencyResult {
    assert!(n <= MAX_THREADS);
    STOP.store(false, Ordering::Release);
    BLOCKING.store(true, Ordering::Release);
    ACK.store(0, Ordering::Release);
    EXITED.store(0, Ordering::Release);
    let page = ensure_page();
    if mode == Mode::Page {
        crate::faults::install_handlers(false);
    }
    let iters = std::sync::Arc::new(AtomicUsize::new(0));
    let ready = std::sync::Arc::new(AtomicUsize::new(0));
    let mut hs = Vec::new();
    for id in 0..n {
        let it = iters.clone();
        let rd = ready.clone();
        hs.push(std::thread::spawn(move || {
            rd.fetch_add(1, Ordering::AcqRel);
            match mode {
                Mode::Flag => worker::<1>(id, &it),
                Mode::Page => worker::<2>(id, &it),
                _ => worker::<0>(id, &it),
            }
        }));
    }
    wait_count(&ready, n);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let handles: Vec<HANDLE> = hs.iter().map(|h| HANDLE(h.as_raw_handle())).collect();
    let mut ctx = AlignedContext::new(arch::CONTEXT_CONTROL);
    let mut samples = Vec::with_capacity(trials);
    for _ in 0..trials {
        let t0 = Instant::now();
        match mode {
            Mode::Flag => FLAG.store(1, Ordering::Release),
            Mode::Page => protect_page(page, PAGE_NOACCESS),
            Mode::Inject => {
                // Suspend all first (asynchronous), then redirect each.
                for &h in &handles {
                    // SAFETY: live worker handle.
                    unsafe { SuspendThread(h) };
                }
                for (id, &h) in handles.iter().enumerate() {
                    redirect_suspended(h, id, &mut ctx);
                    // SAFETY: resumes the thread suspended above.
                    unsafe { ResumeThread(h) };
                }
            }
            Mode::StopOnly => {
                for &h in &handles {
                    // SAFETY: live worker handle.
                    unsafe { SuspendThread(h) };
                }
                for &h in &handles {
                    // SAFETY: GetThreadContext makes the asynchronous
                    // suspension complete.
                    unsafe { GetThreadContext(h, &mut ctx.0) }.expect("GetThreadContext");
                }
            }
        }
        if mode != Mode::StopOnly {
            wait_count(&ACK, n);
        }
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
        // Release.
        match mode {
            Mode::Flag => FLAG.store(0, Ordering::Release),
            Mode::Page => protect_page(page, PAGE_READWRITE),
            Mode::StopOnly => {
                for &h in &handles {
                    // SAFETY: resumes the threads suspended above.
                    unsafe { ResumeThread(h) };
                }
            }
            Mode::Inject => {}
        }
        if mode != Mode::StopOnly {
            ACK.store(0, Ordering::Release);
            EPOCH.fetch_add(1, Ordering::AcqRel);
            // SAFETY: wakes waiters on EPOCH.
            unsafe { WakeByAddressAll(EPOCH.as_ptr() as *const c_void) };
            wait_count(&EXITED, n);
            EXITED.store(0, Ordering::Release);
        }
    }
    STOP.store(true, Ordering::Release);
    for h in hs {
        h.join().unwrap();
    }
    let (median_us, p99_us) = util::median_p99(&mut samples);
    LatencyResult {
        mode,
        threads: n,
        median_us,
        p99_us,
    }
}

/// Loop throughput of one thread with no, flag-polling or page-load checks
/// (millions of iterations per second), median of 5 runs.
pub fn overhead() -> [(Mode, f64); 3] {
    ensure_page();
    let mut res = [
        (Mode::StopOnly, Vec::new()),
        (Mode::Flag, Vec::new()),
        (Mode::Page, Vec::new()),
    ];
    for _ in 0..5 {
        for (m, (_, v)) in res.iter_mut().enumerate() {
            STOP.store(false, Ordering::Release);
            let it = std::sync::Arc::new(AtomicUsize::new(0));
            let it2 = it.clone();
            let t0 = Instant::now();
            let h = std::thread::spawn(move || {
                // Pin to one logical processor so P-core/E-core placement
                // does not dominate the comparison.
                // SAFETY: changes only this thread's affinity.
                unsafe { SetThreadAffinityMask(GetCurrentThread(), 1) };
                match m {
                    1 => worker::<1>(0, &it2),
                    2 => worker::<2>(0, &it2),
                    _ => worker::<0>(0, &it2),
                }
            });
            std::thread::sleep(std::time::Duration::from_millis(200));
            STOP.store(true, Ordering::Release);
            h.join().unwrap();
            v.push(it.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64() / 1e6);
        }
    }
    res.map(|(m, mut v)| (m, util::median_p99(&mut v).0))
}

// ---------------------------------------------------------------------------
// Hazards
// ---------------------------------------------------------------------------

pub struct SyscallHazard {
    pub captured_in_syscall: bool,
    pub checkpoint_before_wake: bool,
    pub checkpoint_after_wake: bool,
    pub wait_result: u32,
    pub captured_rax: u64,
}

/// Injects into a thread blocked in WaitForSingleObject. Checks whether the
/// wait is interrupted and whether the wait's return value survives.
pub fn syscall_hazard() -> SyscallHazard {
    BLOCKING.store(false, Ordering::Release);
    ACK.store(0, Ordering::Release);
    EXITED.store(0, Ordering::Release);
    // SAFETY: unnamed auto-reset event.
    let ev = unsafe { CreateEventW(None, false, false, None) }.expect("event");
    let evv = ev.0 as usize;
    let result = std::sync::Arc::new(AtomicU32::new(u32::MAX));
    let r2 = result.clone();
    let t = std::thread::spawn(move || {
        MY_ID.with(|c| c.set(0));
        // SAFETY: waits on the event created above.
        let r = unsafe { WaitForSingleObject(HANDLE(evv as *mut c_void), 10_000) };
        r2.store(r.0, Ordering::Release);
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let h = HANDLE(t.as_raw_handle());
    let mut full = AlignedContext::new(arch::CONTEXT_FULL);
    let mut ctx = AlignedContext::new(arch::CONTEXT_CONTROL);
    // SAFETY: suspend, read the full context for inspection, then resume
    // and inject. The thread is blocked in the kernel throughout.
    unsafe {
        SuspendThread(h);
        GetThreadContext(h, &mut full.0).expect("GetThreadContext");
        ResumeThread(h);
    }
    let captured_in_syscall = arch::in_syscall(arch::pc(&full.0));
    let captured_rax = arch::ret_reg(&full.0);
    inject(h, 0, &mut ctx);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let checkpoint_before_wake = ACK.load(Ordering::Acquire) != 0;
    // SAFETY: signals the event the thread waits on.
    unsafe { SetEvent(ev) }.expect("SetEvent");
    t.join().unwrap();
    let checkpoint_after_wake = ACK.load(Ordering::Acquire) != 0;
    BLOCKING.store(true, Ordering::Release);
    SyscallHazard {
        captured_in_syscall,
        checkpoint_before_wake,
        checkpoint_after_wake,
        wait_result: result.load(Ordering::Acquire),
        captured_rax,
    }
}

/// Suspends a thread that allocates 64 KiB blocks in a loop, then lets a
/// helper thread allocate. Counts trials in which the helper could not
/// allocate within 50 ms because the suspended thread held the heap lock.
pub fn heap_lock_hazard(trials: usize) -> usize {
    STOP.store(false, Ordering::Release);
    let w = std::thread::spawn(|| {
        while !STOP.load(Ordering::Relaxed) {
            let v: Vec<u8> = Vec::with_capacity(64 * 1024);
            core::hint::black_box(&v);
        }
    });
    // SAFETY: unnamed auto-reset events.
    let (go, done) = unsafe {
        (
            CreateEventW(None, false, false, None).expect("event"),
            CreateEventW(None, false, false, None).expect("event"),
        )
    };
    let (gov, donev) = (go.0 as usize, done.0 as usize);
    let helper = std::thread::spawn(move || loop {
        // SAFETY: waits for and signals the events created above.
        unsafe { WaitForSingleObject(HANDLE(gov as *mut c_void), INFINITE) };
        if STOP.load(Ordering::Acquire) {
            break;
        }
        let v: Vec<u8> = Vec::with_capacity(64 * 1024);
        core::hint::black_box(&v);
        drop(v);
        // SAFETY: as above.
        unsafe { SetEvent(HANDLE(donev as *mut c_void)) }.expect("SetEvent");
    });
    std::thread::sleep(std::time::Duration::from_millis(20));
    let h = HANDLE(w.as_raw_handle());
    let mut ctx = AlignedContext::new(arch::CONTEXT_CONTROL);
    let mut blocked = 0;
    for _ in 0..trials {
        // SAFETY: suspend/resume a live thread; no allocation happens on this
        // thread while the worker is suspended.
        unsafe {
            SuspendThread(h);
            GetThreadContext(h, &mut ctx.0).expect("GetThreadContext");
            SetEvent(go).expect("SetEvent");
            if WaitForSingleObject(done, 50).0 != 0 {
                blocked += 1;
                ResumeThread(h);
                WaitForSingleObject(done, INFINITE);
            } else {
                ResumeThread(h);
            }
        }
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    STOP.store(true, Ordering::Release);
    // SAFETY: wakes the helper so it can observe STOP.
    unsafe { SetEvent(go) }.expect("SetEvent");
    w.join().unwrap();
    helper.join().unwrap();
    blocked
}

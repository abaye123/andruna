//! ARM64 probes: who owns TPIDR_EL0 and x18 on Windows 11 ARM64?
//!
//! Not yet run: the development machine is x64 and the aarch64 Rust target
//! was not installed when this was written. See docs/plan/spike-results/S4.md
//! for the exact procedure.
//!
//! Every instruction that may fault runs under the probe protocol from
//! `veh.rs`: `adr x17, 2f` precedes it and label `2:` follows it.

use std::arch::asm;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    GetThreadContext, SetThreadContext, CONTEXT, CONTEXT_FULL_ARM64,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessorNumber, GetCurrentThread, QueueUserAPC, ResumeThread, Sleep, SleepEx,
    SuspendThread, SwitchToThread, TlsAlloc, TlsGetValue, TlsSetValue,
};

use crate::{bench, hogs, veh};

/// Offset of `TlsSlots[0]` in the 64-bit TEB.
const TEB_TLS_SLOTS: u64 = 0x1480;

fn x18() -> u64 {
    let v: u64;
    // SAFETY: reads the platform register without changing it.
    unsafe { asm!("mov {}, x18", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

fn teb_self() -> u64 {
    let v: u64;
    // SAFETY: x18 points at the TEB in user mode; NT_TIB.Self is at 0x30.
    unsafe {
        asm!("ldr {}, [x18, #0x30]", out(reg) v, options(nostack, readonly, preserves_flags))
    };
    v
}

type Probe = Result<u64, u32>;

fn finish(v: u64) -> Probe {
    match veh::take_probe() {
        Some(code) => Err(code),
        None => Ok(v),
    }
}

fn probe_mrs_tpidr() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: the read either succeeds or traps into the armed handler.
    unsafe {
        asm!("mov x0, xzr", "adr x17, 2f", "mrs x0, tpidr_el0", "2:", out("x0") v, out("x17") _, options(nostack))
    };
    finish(v)
}

fn probe_mrs_tpidrro() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: as above.
    unsafe {
        asm!("mov x0, xzr", "adr x17, 2f", "mrs x0, tpidrro_el0", "2:", out("x0") v, out("x17") _, options(nostack))
    };
    finish(v)
}

fn probe_msr_tpidr(val: u64) -> Probe {
    veh::arm_probe();
    // SAFETY: Windows documents TPIDR_EL0 as reserved; this spike measures
    // whether writing it is possible and whether the system notices.
    unsafe {
        asm!("adr x17, 2f", "msr tpidr_el0, x0", "2:", in("x0") val, out("x17") _, options(nostack))
    };
    finish(0)
}

#[inline(always)]
fn tpidr() -> u64 {
    let v: u64;
    // SAFETY: only called after a successful probe.
    unsafe { asm!("mrs {}, tpidr_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline(always)]
fn set_tpidr(v: u64) {
    // SAFETY: only called after a successful probe; see `probe_msr_tpidr`.
    unsafe { asm!("msr tpidr_el0, {}", in(reg) v, options(nomem, nostack, preserves_flags)) };
}

fn probe_null_read() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: address 0 is never mapped; the fault is caught by the handler.
    unsafe {
        asm!("mov x0, xzr", "adr x17, 2f", "ldr x0, [x9]", "2:", in("x9") 0u64, out("x0") v, out("x17") _, options(nostack))
    };
    finish(v)
}

fn show(p: Probe) -> String {
    match p {
        Ok(v) => format!("ok 0x{v:016x}"),
        Err(c) => format!("fault 0x{c:08x} {}", veh::code_name(c)),
    }
}

pub fn probe() {
    veh::install();
    println!("== arm64 TPIDR_EL0 / TPIDRRO_EL0 / x18 probe ==");
    println!("x18                          : 0x{:016x}", x18());
    println!("TEB self (x18+0x30)          : 0x{:016x}", teb_self());
    println!("main: TPIDR_EL0              : {}", show(probe_mrs_tpidr()));
    // SAFETY: plain query.
    let cpu = unsafe { GetCurrentProcessorNumber() };
    println!(
        "main: TPIDRRO_EL0            : {} (GetCurrentProcessorNumber {cpu})",
        show(probe_mrs_tpidrro())
    );
    for i in 0..4 {
        let (a, b, c) = std::thread::spawn(|| {
            // SAFETY: plain query.
            (probe_mrs_tpidr(), probe_mrs_tpidrro(), unsafe {
                GetCurrentProcessorNumber()
            })
        })
        .join()
        .unwrap();
        println!(
            "thread {i}: TPIDR_EL0 {} TPIDRRO_EL0 {} cpu {c}",
            show(a),
            show(b)
        );
    }

    let original = probe_mrs_tpidr().unwrap_or(0);
    let mine = tls_block(1);
    let w = probe_msr_tpidr(mine);
    println!("msr TPIDR_EL0, 0x{mine:x}   : {}", show(w));
    if w.is_err() {
        println!("TPIDR_EL0 not writable: survival tests skipped");
        return;
    }
    println!("read back                    : {}", show(probe_mrs_tpidr()));
    let _ = probe_msr_tpidr(original);
    survival();
    set_context_probe();
}

/// A heap block standing in for a bionic TLS slot array.
fn tls_block(tag: u64) -> u64 {
    let b: &'static mut [u64; 16] = Box::leak(Box::new([0u64; 16]));
    b[0] = b.as_ptr() as u64;
    b[5] = 0x5EED_0000_0000_0000 | tag;
    b.as_ptr() as u64
}

#[derive(Default)]
struct Tally {
    checks: u64,
    zero: u64,
    other: u64,
    last_bad: u64,
}

impl Tally {
    #[inline(always)]
    fn check(&mut self, want: u64) {
        self.checks += 1;
        let got = tpidr();
        if got != want {
            if got == 0 {
                self.zero += 1;
            } else {
                self.other += 1;
            }
            self.last_bad = got;
            set_tpidr(want);
        }
    }
    fn line(&self, name: &str) {
        println!(
            "{name:<46}: {} checks, {} became 0, {} became other (last 0x{:x})",
            self.checks, self.zero, self.other, self.last_bad
        );
    }
}

unsafe extern "system" fn apc(_: usize) {}

fn survival() {
    println!("-- survival of a TPIDR_EL0 value, idle machine --");
    let me = tls_block(7);
    set_tpidr(me);
    let mut t = Tally::default();
    for _ in 0..3000 {
        // SAFETY: plain yield.
        unsafe { Sleep(0) };
        t.check(me);
    }
    t.line("Sleep(0) x3000, no load");
    let mut t = Tally::default();
    for _ in 0..300 {
        // SAFETY: plain wait.
        unsafe { Sleep(1) };
        t.check(me);
    }
    t.line("Sleep(1) x300, no load");

    println!("-- survival of a TPIDR_EL0 value (CPU hogs on every core) --");
    let _h = hogs::Hogs::start(2);
    let mut t = Tally::default();
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        for _ in 0..1000 {
            t.check(me);
        }
    }
    t.line("preemption (2 s busy loop)");

    let mut t = Tally::default();
    for i in 0..3000 {
        match i % 3 {
            // SAFETY: plain waits.
            0 => unsafe { Sleep(0) },
            1 => unsafe { Sleep(1) },
            _ => unsafe {
                let _ = SwitchToThread();
            },
        }
        t.check(me);
    }
    t.line("Sleep(0) / Sleep(1) / SwitchToThread x3000");

    let mut t = Tally::default();
    for _ in 0..3000 {
        assert!(probe_null_read().is_err());
        t.check(me);
    }
    t.line("access violation handled by VEH x3000");

    let mut t = Tally::default();
    for _ in 0..3000 {
        // SAFETY: queues a no-op APC to the current thread and drains it.
        unsafe {
            QueueUserAPC(Some(apc), GetCurrentThread(), 0);
            SleepEx(0, true);
        }
        t.check(me);
    }
    t.line("user APC delivery x3000");

    let mut t = Tally::default();
    let dir = std::env::temp_dir();
    for i in 0..2000 {
        let _ = std::fs::metadata(&dir);
        let v = vec![i as u8; 1 << 16];
        std::hint::black_box(&v);
        t.check(me);
    }
    t.line("file system calls + heap x2000");

    let stop = Arc::new(AtomicBool::new(false));
    let bad = Arc::new(AtomicU64::new(0));
    let checks = Arc::new(AtomicU64::new(0));
    let (s2, b2, c2) = (stop.clone(), bad.clone(), checks.clone());
    let worker = std::thread::spawn(move || {
        let mine = tls_block(9);
        set_tpidr(mine);
        let mut t = Tally::default();
        while !s2.load(Ordering::Relaxed) {
            t.check(mine);
        }
        b2.store(t.zero + t.other, Ordering::Relaxed);
        c2.store(t.checks, Ordering::Relaxed);
    });
    let h = HANDLE(worker.as_raw_handle());
    for _ in 0..2000 {
        let mut ctx = Context::new(CONTEXT_FULL_ARM64.0);
        // SAFETY: valid handle; context is read and written back while suspended.
        unsafe {
            SuspendThread(h);
            GetThreadContext(h, ctx.as_mut()).unwrap();
            SetThreadContext(h, ctx.as_mut()).unwrap();
            ResumeThread(h);
        }
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    println!(
        "{:<46}: {} checks, {} changed",
        "Suspend+Get/SetThreadContext x2000 (other thr)",
        checks.load(Ordering::Relaxed),
        bad.load(Ordering::Relaxed)
    );

    let ths: Vec<_> = (0..16u64)
        .map(|i| {
            std::thread::spawn(move || {
                let mine = tls_block(100 + i);
                set_tpidr(mine);
                let mut t = Tally::default();
                let end = Instant::now() + Duration::from_secs(1);
                let mut n = 0u32;
                while Instant::now() < end {
                    t.check(mine);
                    n = n.wrapping_add(1);
                    if n.is_multiple_of(4096) {
                        // SAFETY: yield to force switches between owning threads.
                        unsafe { Sleep(0) };
                    }
                }
                t
            })
        })
        .collect();
    let (mut checks, mut zero, mut other) = (0, 0, 0);
    for t in ths {
        let t = t.join().unwrap();
        checks += t.checks;
        zero += t.zero;
        other += t.other;
    }
    println!(
        "{:<46}: {checks} checks, {zero} became 0, {other} became other",
        "16 threads, own value each, 1 s"
    );
}

/// CONTEXT must be 16-byte aligned for Get/SetThreadContext.
#[repr(C, align(16))]
struct Aligned(CONTEXT);
struct Context(Box<Aligned>);
impl Context {
    fn new(flags: u32) -> Self {
        // SAFETY: CONTEXT is plain data; all-zero is a valid value.
        let mut c: Box<Aligned> = Box::new(unsafe { std::mem::zeroed() });
        c.0.ContextFlags.0 = flags;
        Context(c)
    }
    fn as_mut(&mut self) -> *mut CONTEXT {
        &mut self.0 .0
    }
}

/// Does SetThreadContext (which has no TPIDR_EL0 field) disturb the value?
fn set_context_probe() {
    println!("-- TPIDR_EL0 after SetThreadContext with modified X registers --");
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicU64::new(0));
    let (s2, seen2) = (stop.clone(), seen.clone());
    let t = std::thread::spawn(move || {
        set_tpidr(0x1234_5678);
        while !s2.load(Ordering::Relaxed) {
            seen2.store(tpidr(), Ordering::Relaxed);
        }
    });
    std::thread::sleep(Duration::from_millis(50));
    let h = HANDLE(t.as_raw_handle());
    let mut ctx = Context::new(CONTEXT_FULL_ARM64.0);
    // SAFETY: valid handle; the thread is suspended while its context is edited.
    unsafe {
        SuspendThread(h);
        GetThreadContext(h, ctx.as_mut()).unwrap();
        SetThreadContext(h, ctx.as_mut()).unwrap();
        ResumeThread(h);
    }
    std::thread::sleep(Duration::from_millis(30));
    println!(
        "thread sees TPIDR_EL0 0x{:x} (wrote 0x12345678)",
        seen.load(Ordering::Relaxed)
    );
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
}

/// Clobbers x18 inside a call-free spin loop and reports whether the kernel
/// restores it (interrupts, preemption). Then clobbers x18 and takes a fault,
/// to see whether exception dispatch reaches the VEH. Run as its own process:
/// a crash is a valid result.
pub fn x18_clobber() {
    veh::install();
    let _h = hogs::Hogs::start(2);
    let teb = x18();
    let bogus: u64 = 0x0BAD_0000_0000_0018;
    let mut changed: u64 = 0;
    let iters: u64 = 2_000_000_000;
    // SAFETY: x18 is saved in x10 and restored before leaving the block; no
    // calls or memory accesses happen while it holds the bogus value.
    unsafe {
        asm!(
            "mov x10, x18",
            "mov x18, x11",
            "1:",
            "cmp x18, x11",
            "b.eq 2f",
            "add x12, x12, #1",
            "mov x18, x11",
            "2:",
            "subs x9, x9, #1",
            "b.ne 1b",
            "mov x18, x10",
            inout("x9") iters => _,
            out("x10") _,
            in("x11") bogus,
            inout("x12") changed,
            options(nomem, nostack)
        )
    };
    println!("x18 clobbered for {iters} iterations under load: kernel changed it {changed} times");
    println!("x18 restored: {}", x18() == teb);

    println!("now taking an access violation with x18 clobbered...");
    veh::arm_probe();
    // SAFETY: deliberately faulting with a bogus x18; x18 is restored at the
    // resume label. If the system needs x18 during dispatch, the process dies,
    // which is the result being measured.
    unsafe {
        asm!(
            "mov x10, x18",
            "mov x18, x11",
            "adr x17, 2f",
            "ldr x0, [x9]",
            "2:",
            "mov x18, x10",
            in("x9") 0u64,
            in("x11") bogus,
            out("x0") _,
            out("x10") _,
            out("x17") _,
            options(nostack)
        )
    };
    println!(
        "fault with bogus x18 dispatched to VEH: {:?}",
        veh::take_probe().map(veh::code_name)
    );
}

#[inline(never)]
fn rust_tls_read() -> u64 {
    thread_local! { static V: std::cell::Cell<u64> = const { std::cell::Cell::new(3) }; }
    V.with(|v| v.get())
}

pub fn benches() {
    veh::install();
    println!("== arm64 TLS access cost ==");
    let n: u64 = 200_000_000;
    let empty = bench::run("empty loop", n, |n| {
        // SAFETY: register-only loop.
        unsafe {
            asm!("2:", "subs x9, x9, #1", "b.ne 2b", inout("x9") n => _, options(nomem, nostack))
        };
    });

    // SAFETY: TlsAlloc/TlsSetValue on the current thread.
    let idx = unsafe { TlsAlloc() };
    assert!(idx < 64);
    let block = tls_block(3);
    // SAFETY: idx was just allocated.
    unsafe { TlsSetValue(idx, Some(block as *const _)).unwrap() };
    let slot_off = TEB_TLS_SLOTS + 8 * idx as u64;

    let r = bench::run("ldr [x18 + TlsSlots] (TEB slot)", n, |n| {
        // SAFETY: reads the current TEB's TLS slot.
        unsafe {
            asm!("2:", "ldr x0, [x18, x10]", "add x11, x11, x0", "subs x9, x9, #1", "b.ne 2b",
                 inout("x9") n => _, in("x10") slot_off, out("x0") _, out("x11") _, options(nostack, readonly))
        };
    });
    bench::report(&r, &empty);

    let r = bench::run("ldr [x18 + slot] -> [tp + 0x28]", n, |n| {
        // SAFETY: reads the slot array the TLS slot points to.
        unsafe {
            asm!("2:", "ldr x0, [x18, x10]", "ldr x0, [x0, #0x28]", "add x11, x11, x0", "subs x9, x9, #1", "b.ne 2b",
                 inout("x9") n => _, in("x10") slot_off, out("x0") _, out("x11") _, options(nostack, readonly))
        };
    });
    bench::report(&r, &empty);

    let r = bench::run("TlsGetValue()", n / 4, |n| {
        for _ in 0..n {
            // SAFETY: idx is a valid TLS index.
            std::hint::black_box(unsafe { TlsGetValue(idx) });
        }
    });
    bench::report(&r, &empty);

    let r = bench::run("Rust thread_local! (non-inlined fn)", n / 4, |n| {
        for _ in 0..n {
            std::hint::black_box(rust_tls_read());
        }
    });
    bench::report(&r, &empty);

    if probe_msr_tpidr(block).is_ok() {
        let r = bench::run("mrs TPIDR_EL0 -> [tp + 0x28] (bionic native)", n, |n| {
            // SAFETY: TPIDR_EL0 holds `block`.
            unsafe {
                asm!("2:", "mrs x0, tpidr_el0", "ldr x0, [x0, #0x28]", "add x11, x11, x0", "subs x9, x9, #1", "b.ne 2b",
                     inout("x9") n => _, out("x0") _, out("x11") _, options(nostack, readonly))
            };
        });
        bench::report(&r, &empty);
        let r = bench::run("msr TPIDR_EL0 (swap cost at a thunk)", n / 4, |n| {
            // SAFETY: rewrites the same value.
            unsafe {
                asm!("2:", "msr tpidr_el0, x10", "subs x9, x9, #1", "b.ne 2b",
                     inout("x9") n => _, in("x10") block, options(nomem, nostack))
            };
        });
        bench::report(&r, &empty);
    }
}

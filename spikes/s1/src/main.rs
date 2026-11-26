//! Spike S1, round 1 (before ART builds): the Windows mechanisms ART relies
//! on. Run `andruna-spike-s1` for the full report. Spike code, not
//! production code.

mod arch_common;
mod faults;
mod jit;
mod linux_abi;
mod signals;
mod suspend;
mod util;

#[cfg(target_arch = "x86_64")]
mod abi;
#[cfg(target_arch = "x86_64")]
mod arch_x64;
#[cfg(target_arch = "x86_64")]
use arch_x64 as arch;
#[cfg(target_arch = "aarch64")]
mod arch_arm64;
#[cfg(target_arch = "aarch64")]
use arch_arm64 as arch;

use std::panic::{catch_unwind, AssertUnwindSafe};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    CreateThread, WaitForSingleObject, INFINITE, STACK_SIZE_PARAM_IS_A_RESERVATION,
    THREAD_CREATION_FLAGS,
};

/// Runs `f` on a new raw Windows thread. `reserve_only` = the stack size is
/// a reservation (grown through the guard page); otherwise it is committed
/// up front. Rust's std threads always reserve, so this uses CreateThread.
fn on_raw_thread<R: Send + 'static>(
    stack: usize,
    reserve_only: bool,
    f: impl FnOnce() -> R + Send + 'static,
) -> R {
    type Job = Box<dyn FnOnce() + Send>;
    unsafe extern "system" fn start(p: *mut core::ffi::c_void) -> u32 {
        // SAFETY: `p` is the Box<Job> leaked by the creator, taken once.
        let job: Box<Job> = unsafe { Box::from_raw(p as *mut Job) };
        match catch_unwind(AssertUnwindSafe(job)) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let s2 = slot.clone();
    let job: Job = Box::new(move || {
        *s2.lock().unwrap() = Some(f());
    });
    let p = Box::into_raw(Box::new(job));
    let flags = if reserve_only {
        STACK_SIZE_PARAM_IS_A_RESERVATION
    } else {
        THREAD_CREATION_FLAGS(0)
    };
    // SAFETY: `start` takes ownership of `p`; we wait for the thread to exit
    // before reading the result.
    unsafe {
        let h = CreateThread(None, stack, Some(start), Some(p as *const _), flags, None)
            .expect("CreateThread");
        WaitForSingleObject(h, INFINITE);
        let _ = CloseHandle(h);
    }
    let r = slot.lock().unwrap().take().expect("thread failed");
    r
}

fn run_child(args: &[String]) -> i32 {
    match (args[0].as_str(), args.get(1).map(String::as_str)) {
        ("panic", Some(r)) => {
            let reg = match r {
                "table" => jit::Registration::Table,
                "callback" => jit::Registration::Callback,
                "growable" => jit::Registration::Growable,
                _ => jit::Registration::None,
            };
            faults::child_panic(reg)
        }
        ("so-noalt", _) => {
            let r = on_raw_thread(1 << 20, false, || faults::so_art(false));
            println!("{:?}", r);
            0
        }
        _ => 2,
    }
}

fn child(args: &[&str]) -> String {
    let exe = std::env::current_exe().unwrap();
    match std::process::Command::new(exe)
        .arg("--child")
        .args(args)
        .output()
    {
        Ok(o) => format!(
            "exit {:#x}{}",
            o.status.code().unwrap_or(-1) as u32,
            if o.stdout.is_empty() {
                String::new()
            } else {
                format!(", {}", String::from_utf8_lossy(&o.stdout).trim())
            }
        ),
        Err(e) => format!("spawn failed: {e}"),
    }
}

fn main() {
    assert!(signals::install(), "AddVectoredExceptionHandler failed");
    suspend::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        std::process::exit(run_child(&args[1..]));
    }
    let quick = args.iter().any(|a| a == "--quick");
    let only: Vec<&str> = args
        .iter()
        .filter_map(|a| a.strip_prefix("--only="))
        .collect();
    let want = |s: &str| only.is_empty() || only.contains(&s);

    println!("== S1 round 1 on {} ==", std::env::consts::ARCH);
    let j = jit::get();
    println!("jit buffer at {:#x}, blob {} bytes", j.base, j.l.len);

    if want("faults") {
        println!("\n-- 1. Fault delivery --");
        let nc = faults::null_check();
        println!("implicit null check correct: {}", nc.correct);
        println!("  load, no fault:                {:8.2} ns", nc.baseline_ns);
        println!("  fault round trip (full):       {:8.2} us", nc.fault_us);
        println!(
            "  fault round trip (alt stack):  {:8.2} us",
            nc.fault_alt_us
        );
        println!("  fault round trip (VEH only):   {:8.2} us", nc.raw_us);
        println!(
            "  stack used by kernel+dispatcher before the VEH frame: {} bytes",
            nc.veh_stack_use
        );
        let (ill, fpe) = faults::traps();
        println!(
            "SIGILL handled: {}  SIGFPE handled: {}",
            ill == faults::TRAP_MARKER + linux_abi::SIGILL as u64,
            fpe == faults::TRAP_MARKER + linux_abi::SIGFPE as u64
        );
        let so = on_raw_thread(1 << 20, false, || faults::so_art(true));
        println!(
            "ART-style stack overflow (protected region + probe, alt stack): {:?}",
            so
        );
        println!("  same without alt stack (child): {}", child(&["so-noalt"]));
        let sow = on_raw_thread(256 << 10, true, || faults::so_windows(32 << 10));
        println!(
        "Windows stack overflow (guard page + 32 KiB guarantee): depths {:?} codes {:x?} veh stack use {}",
        sow.depths, sow.codes, sow.veh_stack_use
    );
    }
    if want("unwind") {
        println!("\n-- 3b. Unwinding through JIT code --");
        for reg in [
            jit::Registration::None,
            jit::Registration::Table,
            jit::Registration::Callback,
            jit::Registration::Growable,
        ] {
            let u = faults::unwind(reg);
            println!("{:?}", u);
        }
        println!(
            "callback lookups: {}",
            jit::CALLBACK_HITS.load(std::sync::atomic::Ordering::Relaxed)
        );
        for r in ["none", "table", "callback", "growable"] {
            println!(
                "panic through JIT frame, registration {r}: {}",
                child(&["panic", r])
            );
        }
    }
    #[cfg(target_arch = "x86_64")]
    if want("abi") {
        println!("\n-- 3a. Calling convention (x64) --");
        let a = abi::run();
        println!(
            "hand-made sysv->win64 ok {}, sysv->win64 trampoline ok {}, win64->sysv trampoline ok {}",
            a.hand_ok, a.sysv_to_win64_ok, a.win64_to_sysv_ok
        );
        println!(
            "callee-saved clobber mask: raw sysv {:#x}, via trampoline {:#x}, via rustc {:#x}",
            a.mask_raw, a.mask_trampoline, a.mask_rustc
        );
        println!(
            "ns/call: win->win {:.2}, win->sysv (rustc) {:.2}, win->sysv (trampoline) {:.2}, sysv->win (trampoline) {:.2}",
            a.ns_win_win, a.ns_win_sysv_rustc, a.ns_win_sysv_tramp, a.ns_sysv_win_tramp
        );
    }

    if want("suspend") {
        println!("\n-- 2. Thread suspension --");
        let ov = suspend::overhead();
        println!(
            "loop throughput (M iter/s): no check {:.0}, flag poll {:.0}, page load {:.0}",
            ov[0].1, ov[1].1, ov[2].1
        );
        let trials = if quick { 50 } else { 300 };
        for n in [1usize, 4, 8, 32] {
            for m in [
                suspend::Mode::StopOnly,
                suspend::Mode::Inject,
                suspend::Mode::Flag,
                suspend::Mode::Page,
            ] {
                let r = suspend::latency(m, n, trials);
                println!(
                    "{:>9?} n={:2}: median {:8.1} us  p99 {:8.1} us",
                    r.mode, r.threads, r.median_us, r.p99_us
                );
            }
        }
        let h = suspend::syscall_hazard();
        println!(
        "inject into WaitForSingleObject: captured in syscall {}, rax {:#x}, checkpoint before wake {}, after wake {}, wait result {:#x}",
        h.captured_in_syscall, h.captured_rax, h.checkpoint_before_wake, h.checkpoint_after_wake, h.wait_result
    );
        let ht = if quick { 100 } else { 500 };
        let blocked = suspend::heap_lock_hazard(ht);
        println!("heap lock held by suspended allocating thread: {blocked} of {ht} suspensions");
    }
}

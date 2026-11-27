//! Default signal actions, in child processes: the test binary runs itself
//! with `LX_DEFAULT_ACTION` naming a scenario and checks how the child
//! ended (its exit code, as a supervisor sees it).

use std::process::Command;

use andruna_lx::linux::mman::*;
use andruna_lx::linux::signal::*;
use andruna_lx::signal::{self, ExitStatus, decode_exit_code, signal_exit_code};

const VAR: &str = "LX_DEFAULT_ACTION";

/// Runs `scenario` in a child and returns its exit code.
fn run(scenario: &str) -> u32 {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(VAR, scenario)
        .output()
        .expect("spawn the test binary");
    let code = out.status.code().expect("exit code") as u32;
    eprintln!(
        "{scenario}: exit {code:#x}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    code
}

/// Guest-style code outside any Windows image: a function that stores to
/// address 0, in lx's anonymous memory.
fn faulting_code() -> extern "C" fn() {
    #[cfg(target_arch = "x86_64")]
    let code: &[u8] = &[
        0xc6, 0x04, 0x25, 0, 0, 0, 0, 0,    // mov byte ptr [0], 0
        0xc3, // ret
    ];
    #[cfg(target_arch = "aarch64")]
    let code: &[u8] = &[
        0x00, 0x00, 0x80, 0xd2, // mov x0, #0
        0x1f, 0x00, 0x00, 0xb9, // str wzr, [x0]
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ];
    let a = andruna_lx::mem::mmap(
        0,
        4096,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0,
    )
    .unwrap();
    // SAFETY: a fresh writable page of 4096 bytes.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), a as *mut u8, code.len()) };
    andruna_lx::mem::mprotect(a, 4096, PROT_READ | PROT_EXEC).unwrap();
    // SAFETY: the page holds a complete function with the C ABI.
    unsafe { core::mem::transmute::<usize, extern "C" fn()>(a) }
}

fn set(sig: i32, handler: u64) {
    let a = KernelSigaction {
        handler,
        flags: 0,
        restorer: 0,
        mask: 0,
    };
    signal::sigaction(sig, Some(a), 8).unwrap();
}

static mut RESET_RAN: bool = false;

/// What libsigchain does with a fault nobody claims: restore the default
/// action and return, so that the instruction faults again.
#[cfg(target_arch = "x86_64")]
extern "sysv64" fn reset_to_default(sig: i32, _: *mut SigInfo, _: *mut core::ffi::c_void) {
    reset(sig)
}
#[cfg(target_arch = "aarch64")]
extern "C" fn reset_to_default(sig: i32, _: *mut SigInfo, _: *mut core::ffi::c_void) {
    reset(sig)
}

fn reset(sig: i32) {
    // SAFETY: only the faulting thread runs this.
    unsafe { RESET_RAN = true };
    set(sig, SIG_DFL);
}

#[test]
fn child() {
    let Ok(s) = std::env::var(VAR) else {
        return;
    };
    signal::install_handler();
    let pid = andruna_lx::process::getpid();
    match s.as_str() {
        "segv" => faulting_code()(),
        "segv-ignored" => {
            set(SIGSEGV, SIG_IGN);
            faulting_code()()
        }
        "segv-blocked" => {
            set(SIGSEGV, reset_to_default as *const () as u64);
            signal::sigprocmask(SIG_BLOCK, Some(1 << (SIGSEGV - 1)), 8).unwrap();
            faulting_code()()
        }
        "segv-reset" => {
            set(SIGSEGV, reset_to_default as *const () as u64);
            faulting_code()();
            // Not reached: the handler ran and the fault came back.
            // SAFETY: read after the handler.
            assert!(unsafe { RESET_RAN });
        }
        "abort" => signal::kill(pid, SIGABRT, None).unwrap(),
        "term" => signal::kill(pid, SIGTERM, None).unwrap(),
        "rt" => signal::kill(pid, SIGRTMIN + 3, None).unwrap(),
        "ignored" => {
            for sig in [SIGCHLD, SIGURG, SIGWINCH, SIGCONT] {
                signal::kill(pid, sig, None).unwrap();
                signal::deliver_pending();
            }
            std::process::exit(7);
        }
        "stop" => {
            signal::kill(pid, SIGSTOP, None).unwrap();
            // Runs once the parent resumed the process.
            std::process::exit(9);
        }
        other => panic!("unknown scenario {other}"),
    }
    signal::deliver_pending();
    std::process::exit(99);
}

fn signaled(sig: i32, core: bool) -> u32 {
    let c = signal_exit_code(sig);
    assert_eq!(decode_exit_code(c), ExitStatus::Signaled { sig, core });
    c
}

#[test]
fn fault_in_guest_code_with_the_default_action_ends_the_process() {
    assert_eq!(run("segv"), signaled(SIGSEGV, true));
    assert_eq!(run("segv-ignored"), signaled(SIGSEGV, true));
    assert_eq!(run("segv-blocked"), signaled(SIGSEGV, true));
}

#[test]
fn handler_that_restores_the_default_action_ends_the_process() {
    assert_eq!(run("segv-reset"), signaled(SIGSEGV, true));
}

#[test]
fn terminate_and_core_actions() {
    assert_eq!(run("abort"), signaled(SIGABRT, true));
    assert_eq!(run("term"), signaled(SIGTERM, false));
    assert_eq!(run("rt"), signaled(SIGRTMIN + 3, false));
    assert_eq!(signaled(SIGQUIT, true), 0xE0A0_0083);
    assert_eq!(signaled(SIGKILL, false), 0xE0A0_0009);
}

#[test]
fn ignore_action_discards() {
    assert_eq!(run("ignored"), 7);
    assert_eq!(decode_exit_code(7), ExitStatus::Exited(7));
    assert_eq!(
        decode_exit_code(0xC000_0005),
        ExitStatus::Other(0xC000_0005)
    );
}

/// `NtResumeProcess` through ntdll's export.
fn resume(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, NTSTATUS};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{s, w};
    type F = unsafe extern "system" fn(HANDLE) -> NTSTATUS;
    // SAFETY: ntdll is loaded; the export has this signature.
    let f: F = unsafe {
        let m = GetModuleHandleW(w!("ntdll.dll")).unwrap();
        core::mem::transmute(GetProcAddress(m, s!("NtResumeProcess")).unwrap())
    };
    // SAFETY: a live process handle with full access (our child).
    let st = unsafe { f(HANDLE(child.as_raw_handle())) };
    assert!(st.is_ok());
}

#[test]
fn stop_suspends_until_resumed() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(VAR, "stop")
        .spawn()
        .unwrap();
    // The child stops itself and stays stopped.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "stopped child ended");
    // Resume until it ends: a slow start could stop it only after the
    // first resume.
    for _ in 0..100 {
        resume(&child);
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(st) = child.try_wait().unwrap() {
            assert_eq!(st.code(), Some(9));
            return;
        }
    }
    let _ = child.kill();
    panic!("the stopped child did not end after being resumed");
}

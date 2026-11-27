//! `__andruna_syscall`: the system call entry of guest code.
//!
//! bionic's system call stubs (generated from `SYSCALLS.TXT` by
//! `aosp/tools/gensyscalls_andruna.py`) call this function instead of
//! executing `syscall`/`svc`: the arguments are in the C argument registers
//! (x64: `rdi rsi rdx rcx r8 r9`; arm64: `x0`-`x5`), the number in `rax`
//! (x64) or `x8` (arm64), and the result is the raw kernel value
//! (`-errno` on failure). The stubs then set bionic's `errno` themselves.
//!
//! The calls that concern threads, the thread pointer and the process's
//! lifetime are the loader's ([`crate::thread`], [`crate::tls`]), and
//! `getrandom` is implemented here until lx has it; everything else
//! goes to andruna-lx's `syscall` export, which reports `ENOSYS` for calls
//! it does not implement.

use andruna_lx::linux::errno::*;

/// Linux system call numbers the loader handles itself.
#[cfg(target_arch = "x86_64")]
pub mod nr {
    pub const READ: i64 = 0;
    pub const WRITE: i64 = 1;
    pub const READV: i64 = 19;
    pub const WRITEV: i64 = 20;
    pub const MADVISE: i64 = 28;
    pub const CLONE: i64 = 56;
    pub const FORK: i64 = 57;
    pub const VFORK: i64 = 58;
    pub const EXIT: i64 = 60;
    pub const GETUID: i64 = 102;
    pub const GETGID: i64 = 104;
    pub const GETEUID: i64 = 107;
    pub const GETEGID: i64 = 108;
    pub const ARCH_PRCTL: i64 = 158;
    pub const SET_TID_ADDRESS: i64 = 218;
    pub const EXIT_GROUP: i64 = 231;
    pub const GETRANDOM: i64 = 318;
    pub const CLONE3: i64 = 435;
}

#[cfg(target_arch = "aarch64")]
pub mod nr {
    pub const READ: i64 = 63;
    pub const WRITE: i64 = 64;
    pub const READV: i64 = 65;
    pub const WRITEV: i64 = 66;
    pub const MADVISE: i64 = 233;
    pub const EXIT: i64 = 93;
    pub const EXIT_GROUP: i64 = 94;
    pub const SET_TID_ADDRESS: i64 = 96;
    pub const GETUID: i64 = 174;
    pub const GETEUID: i64 = 175;
    pub const GETGID: i64 = 176;
    pub const GETEGID: i64 = 177;
    pub const CLONE: i64 = 220;
    pub const GETRANDOM: i64 = 278;
    pub const CLONE3: i64 = 435;
}

#[cfg(target_arch = "x86_64")]
const ARCH_SET_FS: i64 = 0x1002;
#[cfg(target_arch = "x86_64")]
const ARCH_GET_FS: i64 = 0x1003;
const GRND_NONBLOCK: i64 = 1;
const GRND_RANDOM: i64 = 2;
const GRND_INSECURE: i64 = 4;

/// Calls andruna-lx's dispatcher and returns the raw kernel value. Queued
/// signals are not delivered here: `__andruna_syscall` delivers them once
/// the guest's stack is back (`deliver_signals`).
///
/// # Safety
///
/// The pointer arguments satisfy the contract of system call `n`.
pub unsafe fn lx_raw(n: i64, a: [i64; 6]) -> i64 {
    andruna_lx::errno::set_errno(andruna_lx::errno::Errno(0));
    // SAFETY: forwarded from this function's contract.
    let r = unsafe { andruna_lx::syscall_without_delivery(n, a) };
    if r == -1 {
        let e = andruna_lx::errno::errno();
        if e != 0 {
            return -i64::from(e);
        }
    }
    r
}

/// Dispatches one system call; returns the raw kernel value.
///
/// # Safety
///
/// The pointer arguments satisfy the contract of system call `n`, as for
/// the Linux kernel.
pub unsafe fn dispatch(n: i64, a: [i64; 6]) -> i64 {
    let t0 = crate::profile::start();
    // SAFETY: forwarded from this function's contract.
    let r = unsafe { dispatch_inner(n, a) };
    crate::profile::syscall(n, t0);
    if trace() {
        crate::debug::report(format_args!(
            "syscall {n}({:#x}, {:#x}, {:#x}, {:#x}) = {r}",
            a[0], a[1], a[2], a[3]
        ));
    }
    r
}

/// `ANDRUNA_STRACE`: report every guest system call on standard error.
fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("ANDRUNA_STRACE").is_some())
}

/// # Safety
///
/// As for [`dispatch`].
unsafe fn dispatch_inner(n: i64, a: [i64; 6]) -> i64 {
    match n {
        nr::EXIT => crate::thread::exit_current(a[0] as i32, None),
        nr::EXIT_GROUP => crate::process::exit_process(a[0] as i32),
        nr::SET_TID_ADDRESS => crate::thread::set_tid_address(a[0] as u64),
        #[cfg(target_arch = "x86_64")]
        nr::ARCH_PRCTL => match a[0] {
            ARCH_SET_FS => {
                crate::tls::set_thread_pointer(a[1] as u64);
                0
            }
            ARCH_GET_FS => {
                let p = a[1] as *mut u64;
                if p.is_null() {
                    return -i64::from(EFAULT);
                }
                // SAFETY: the caller passes a writable u64 (contract).
                unsafe { p.write_unaligned(crate::tls::current()) };
                0
            }
            _ => -i64::from(EINVAL),
        },
        // Raw clone from `syscall(SYS_clone, ...)` cannot return twice on
        // Windows; threads come through `__andruna_clone_thread`, and
        // processes cannot be forked.
        nr::CLONE | nr::CLONE3 => -i64::from(ENOSYS),
        #[cfg(target_arch = "x86_64")]
        nr::FORK | nr::VFORK => -i64::from(ENOSYS),
        nr::GETUID | nr::GETEUID => i64::from(andruna_lx::process::uid()),
        nr::GETGID | nr::GETEGID => i64::from(andruna_lx::process::gid()),
        nr::GETRANDOM => getrandom(a[0] as *mut u8, a[1] as usize, a[2]),
        nr::MADVISE if matches!(a[2], 4 | 8) => {
            // SAFETY: forwarded from this function's contract.
            unsafe { discard_below_sp(a) }
        }
        // SAFETY: forwarded from this function's contract.
        _ => unsafe { forward(n, a) },
    }
}

/// Sends a call to lx, with the loader's interim answers
/// ([`crate::interim`]) before it and where lx reports `ENOSYS`.
///
/// # Safety
///
/// As for [`dispatch`].
unsafe fn forward(n: i64, a: [i64; 6]) -> i64 {
    // SAFETY: forwarded from this function's contract.
    if let Some(r) = unsafe { crate::interim::before(n, a) } {
        return r;
    }
    // SAFETY: as above.
    let r = unsafe { lx_raw(n, a) };
    if r < 0
        // SAFETY: as above.
        && let Some(v) = unsafe { crate::interim::after_error(n, a, r) }
    {
        return v;
    }
    if r != -i64::from(ENOSYS) {
        return r;
    }
    // SAFETY: as above.
    match unsafe { crate::interim::after_enosys(n, a) } {
        Some(r) => r,
        None => {
            crate::interim::report_enosys(n, a);
            r
        }
    }
}

/// Host code below a guest frame that `madvise(MADV_DONTNEED / MADV_FREE)`
/// is called from must keep its stack. System calls normally run on the
/// thread's Windows stack (`__andruna_syscall`), but nested ones and those
/// of threads the loader did not start run on the current stack. Threads release the
/// unused part of their stack this way (ART does, below the stack
/// pointer), and lx implements the advice by decommitting the pages, which
/// would free the host frames handling the call. The pages from 64 KiB
/// below the current stack pointer up are kept (their contents stay, which
/// only matters to code reading its own stack below the stack pointer).
///
/// # Safety
///
/// As for [`dispatch`].
unsafe fn discard_below_sp(mut a: [i64; 6]) -> i64 {
    const MARGIN: u64 = 64 << 10;
    let local = 0u8;
    let sp = (&local as *const u8 as u64) & !(crate::image::PAGE - 1);
    let (keep_lo, keep_hi) = (sp.saturating_sub(MARGIN), sp + MARGIN);
    let start = a[0] as u64;
    let end = start.saturating_add(a[1] as u64);
    if end <= keep_lo || start >= keep_hi {
        // SAFETY: forwarded from this function's contract.
        return unsafe { forward(nr::MADVISE, a) };
    }
    // The parts below and above the kept range.
    for (lo, hi) in [(start, keep_lo.min(end)), (keep_hi.max(start), end)] {
        if lo < hi {
            a[0] = lo as i64;
            a[1] = (hi - lo) as i64;
            // SAFETY: as above, on a part of the caller's range.
            let r = unsafe { forward(nr::MADVISE, a) };
            if r < 0 {
                return r;
            }
        }
    }
    0
}

fn getrandom(buf: *mut u8, len: usize, flags: i64) -> i64 {
    if flags & !(GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE) != 0 {
        return -i64::from(EINVAL);
    }
    if len == 0 {
        return 0;
    }
    if buf.is_null() {
        return -i64::from(EFAULT);
    }
    let len = len.min(i32::MAX as usize);
    // SAFETY: the caller passes a writable buffer of `len` bytes (contract).
    let b = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    crate::tls::random_bytes(b);
    len as i64
}

gfn! {
    /// The Rust side of `__andruna_syscall`: the number arrives as the
    /// seventh argument (pushed by the x64 entry, moved to x6 on arm64).
    unsafe fn raw_entry(a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64, n: i64) -> i64 {
        crate::host::guard(-i64::from(EIO), || {
            // SAFETY: guest code calls with the kernel's contract.
            unsafe { dispatch(n, [a1, a2, a3, a4, a5, a6]) }
        })
    }
}

gfn! {
    /// Before a system call: on a guest thread that is not already in one,
    /// switches the TEB to the thread's Windows stack and returns its top
    /// (the entry moves the stack pointer there); otherwise 0.
    unsafe fn stack_enter() -> u64 {
        crate::thread::enter_host_stack()
    }
}

gfn! {
    /// After every system call, on the guest's stack with the TEB bounds
    /// of that stack: delivers the signals queued for the thread, so that
    /// handlers run (and may `siglongjmp`) as they would after a Linux
    /// system call, not on the Windows stack the call ran on.
    unsafe fn deliver_signals() -> () {
        andruna_lx::signal::deliver_after_syscall()
    }
}

gfn! {
    /// After a system call that [`stack_enter`] moved to the Windows stack.
    unsafe fn stack_leave() -> () {
        crate::thread::leave_host_stack()
    }
}

/// `__andruna_syscall` (x64): number in `rax`, arguments in the System V
/// argument registers.
///
/// The call runs on the thread's Windows stack, as a kernel runs on its
/// own stack: guest code leaves little stack below its frames in places
/// (ART's stack overflow handling, a thread discarding its unused stack
/// with `madvise`), and host code must not depend on it.
///
/// # Safety
///
/// Called only by guest code with that convention and the kernel's
/// contract for the call.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn andruna_syscall() {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "push rbx",
        "push rax",
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push r8",
        "push r9",
        // 16-byte aligned here (entry 8 mod 16, eight pushes).
        "call {enter}",
        "mov rbx, rax",
        "mov rdi, [rbp - 24]",
        "mov rsi, [rbp - 32]",
        "mov rdx, [rbp - 40]",
        "mov rcx, [rbp - 48]",
        "mov r8, [rbp - 56]",
        "mov r9, [rbp - 64]",
        "mov r10, [rbp - 16]",
        "test rbx, rbx",
        "jz 2f",
        "mov rsp, rbx",
        "and rsp, -16",
        "2:",
        // The number becomes the seventh (stack) argument; aligned at the
        // call.
        "sub rsp, 8",
        "push r10",
        "call {entry}",
        "test rbx, rbx",
        "jz 3f",
        "mov [rbp - 16], rax",
        "add rsp, 16",
        "call {leave}",
        "mov rax, [rbp - 16]",
        "3:",
        // Back on the guest stack (16-byte aligned at rbp - 64).
        "mov [rbp - 16], rax",
        "lea rsp, [rbp - 64]",
        "call {deliver}",
        "mov rax, [rbp - 16]",
        "lea rsp, [rbp - 8]",
        "pop rbx",
        "pop rbp",
        "ret",
        enter = sym stack_enter,
        leave = sym stack_leave,
        deliver = sym deliver_signals,
        entry = sym raw_entry,
    )
}

/// `__andruna_syscall` (arm64): number in `x8`, arguments in `x0`-`x5`.
///
/// # Safety
///
/// As for x64.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
pub unsafe extern "C" fn andruna_syscall() {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #-16]!",
        "sub sp, sp, #64",
        "stp x0, x1, [sp]",
        "stp x2, x3, [sp, #16]",
        "stp x4, x5, [sp, #32]",
        "str x8, [sp, #48]",
        "bl {enter}",
        "mov x19, x0",
        "ldp x0, x1, [sp]",
        "ldp x2, x3, [sp, #16]",
        "ldp x4, x5, [sp, #32]",
        "ldr x6, [sp, #48]",
        "cbz x19, 2f",
        "and x9, x19, #-16",
        "mov sp, x9",
        "2:",
        "bl {entry}",
        "cbz x19, 3f",
        "mov x20, x0",
        "bl {leave}",
        "mov x0, x20",
        "3:",
        // Back on the guest stack.
        "sub sp, x29, #80",
        "str x0, [sp, #56]",
        "bl {deliver}",
        "ldr x0, [sp, #56]",
        "sub sp, x29, #16",
        "ldp x19, x20, [sp], #16",
        "ldp x29, x30, [sp], #16",
        "ret",
        enter = sym stack_enter,
        leave = sym stack_leave,
        deliver = sym deliver_signals,
        entry = sym raw_entry,
    )
}

pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![("__andruna_syscall", andruna_syscall as usize as u64)]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Calls `__andruna_syscall` the way a bionic stub does.
    pub(crate) fn call(n: i64, a: [i64; 6]) -> i64 {
        let f = andruna_syscall as usize;
        let r: i64;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the entry's convention; the arguments are valid for `n`.
        unsafe {
            core::arch::asm!(
                "call {f}",
                f = in(reg) f,
                inout("rax") n => r,
                in("rdi") a[0], in("rsi") a[1], in("rdx") a[2],
                in("rcx") a[3], in("r8") a[4], in("r9") a[5],
                clobber_abi("sysv64"),
            )
        };
        #[cfg(target_arch = "aarch64")]
        // SAFETY: as above.
        unsafe {
            core::arch::asm!(
                "blr {f}",
                f = in(reg) f,
                in("x8") n,
                inout("x0") a[0] => r,
                in("x1") a[1], in("x2") a[2], in("x3") a[3], in("x4") a[4], in("x5") a[5],
                clobber_abi("C"),
            )
        };
        r
    }

    #[test]
    fn entry_routes_to_lx_with_raw_errors() {
        // getpid through lx.
        #[cfg(target_arch = "x86_64")]
        let getpid = 39;
        #[cfg(target_arch = "aarch64")]
        let getpid = 172;
        assert_eq!(call(getpid, [0; 6]), i64::from(std::process::id()));
        // close(-1): -EBADF, not -1.
        #[cfg(target_arch = "x86_64")]
        let close = 3;
        #[cfg(target_arch = "aarch64")]
        let close = 57;
        assert_eq!(call(close, [-1, 0, 0, 0, 0, 0]), -i64::from(EBADF));
        // Unknown numbers are ENOSYS.
        assert_eq!(call(100_000, [0; 6]), -i64::from(ENOSYS));
        // Raw clone is refused honestly.
        assert_eq!(call(nr::CLONE, [0; 6]), -i64::from(ENOSYS));
    }

    #[test]
    fn getrandom_fills_and_validates() {
        let mut b = [0u8; 64];
        assert_eq!(
            call(nr::GETRANDOM, [b.as_mut_ptr() as i64, 64, 0, 0, 0, 0]),
            64
        );
        assert!(b.iter().any(|&x| x != 0));
        assert_eq!(call(nr::GETRANDOM, [0, 8, 0, 0, 0, 0]), -i64::from(EFAULT));
        assert_eq!(
            call(nr::GETRANDOM, [b.as_mut_ptr() as i64, 8, 0x100, 0, 0, 0]),
            -i64::from(EINVAL)
        );
    }

    #[test]
    fn writev_to_a_bad_descriptor() {
        let data = *b"abc";
        let v = [[data.as_ptr() as u64, 3u64]];
        assert_eq!(
            call(nr::WRITEV, [-1, v.as_ptr() as i64, 1, 0, 0, 0]),
            -i64::from(EBADF)
        );
        assert_eq!(
            call(nr::WRITEV, [-1, v.as_ptr() as i64, -1, 0, 0, 0]),
            -i64::from(EINVAL)
        );
    }

    #[test]
    fn identity() {
        assert_eq!(
            call(nr::GETUID, [0; 6]),
            i64::from(andruna_lx::process::uid())
        );
    }
}

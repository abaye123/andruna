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
//! lifetime are the loader's ([`crate::thread`], [`crate::tls`]); a few
//! that are compositions of lx's public interface (`readv`, `writev`)
//! and `getrandom` are implemented here until lx has them; everything else
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
    pub const CLONE: i64 = 56;
    pub const FORK: i64 = 57;
    pub const VFORK: i64 = 58;
    pub const EXIT: i64 = 60;
    pub const GETPID: i64 = 39;
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
    pub const EXIT: i64 = 93;
    pub const EXIT_GROUP: i64 = 94;
    pub const SET_TID_ADDRESS: i64 = 96;
    pub const GETPID: i64 = 172;
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

mod lx {
    unsafe extern "C" {
        /// andruna-lx's `long syscall(long number, ...)` export (Windows
        /// convention): -1 with lx's `errno` set on failure.
        pub fn syscall(n: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64) -> i64;
    }
}

/// Calls andruna-lx's dispatcher and returns the raw kernel value.
///
/// # Safety
///
/// The pointer arguments satisfy the contract of system call `n`.
pub unsafe fn lx_raw(n: i64, a: [i64; 6]) -> i64 {
    andruna_lx::errno::set_errno(andruna_lx::errno::Errno(0));
    // SAFETY: forwarded from this function's contract.
    let r = unsafe { lx::syscall(n, a[0], a[1], a[2], a[3], a[4], a[5]) };
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
    // SAFETY: forwarded from this function's contract.
    let r = unsafe { dispatch_inner(n, a) };
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
        nr::GETPID => i64::from(crate::process::guest_pid()),
        nr::GETUID | nr::GETEUID => i64::from(andruna_lx::process::uid()),
        nr::GETGID | nr::GETEGID => i64::from(andruna_lx::process::gid()),
        nr::GETRANDOM => getrandom(a[0] as *mut u8, a[1] as usize, a[2]),
        // SAFETY: forwarded from this function's contract.
        nr::WRITEV => unsafe { iov(a[0] as i32, a[1] as *const [u64; 2], a[2] as i32, true) },
        // SAFETY: as above.
        nr::READV => unsafe { iov(a[0] as i32, a[1] as *const [u64; 2], a[2] as i32, false) },
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
        && let Some(v) = unsafe { crate::interim::after_error(n, a) }
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

/// `readv`/`writev` as a sequence of `read`/`write` on lx's descriptors;
/// stops at the first short transfer, as the kernel does for files.
///
/// # Safety
///
/// `v` points to `n` readable `struct iovec`, each a valid buffer.
unsafe fn iov(fd: i32, v: *const [u64; 2], n: i32, write: bool) -> i64 {
    if !(0..=1024).contains(&n) {
        return -i64::from(EINVAL);
    }
    if n > 0 && v.is_null() {
        return -i64::from(EFAULT);
    }
    let mut total: i64 = 0;
    for i in 0..n as usize {
        // SAFETY: inside the caller's array (contract).
        let [base, len] = unsafe { v.add(i).read_unaligned() };
        if len == 0 {
            continue;
        }
        let n = if write { nr::WRITE } else { nr::READ };
        // SAFETY: the caller's buffer (contract).
        let r = unsafe { lx_raw(n, [i64::from(fd), base as i64, len as i64, 0, 0, 0]) };
        let r = if r < 0 { Err(r) } else { Ok(r as u64) };
        match r {
            Ok(k) => {
                total += k as i64;
                if k < len {
                    break;
                }
            }
            Err(e) if total == 0 => return e,
            Err(_) => break,
        }
    }
    total
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

/// `__andruna_syscall` (x64): number in `rax`, arguments in the System V
/// argument registers.
///
/// # Safety
///
/// Called only by guest code with that convention and the kernel's
/// contract for the call.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn andruna_syscall() {
    core::arch::naked_asm!(
        // The number becomes the seventh (stack) argument; the stack is
        // 16-byte aligned again at the call.
        "push rax",
        "call {entry}",
        "add rsp, 8",
        "ret",
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
    core::arch::naked_asm!("mov x6, x8", "b {entry}", entry = sym raw_entry)
}

pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![("__andruna_syscall", andruna_syscall as usize as u64)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls `__andruna_syscall` the way a bionic stub does.
    fn call(n: i64, a: [i64; 6]) -> i64 {
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

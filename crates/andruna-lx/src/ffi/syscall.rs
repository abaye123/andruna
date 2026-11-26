//! `long syscall(long number, ...)` with the Linux numbering of the build's
//! architecture ([`crate::linux::syscall_nr`]).
//!
//! Each argument is converted to the type the kernel declares for it (for
//! example `int` for a descriptor), so the unspecified upper bits of 32-bit
//! arguments passed through the variadic call are ignored, as the kernel
//! ignores them. Unknown numbers fail with `ENOSYS`.
//!
//! # Safety
//!
//! The pointer arguments must satisfy the contract of the system call named
//! by `number`, as for the corresponding exports.

use core::ffi::c_char;
use core::sync::atomic::AtomicU32;

use super::fs::{sys_fstat, sys_fstatat, sys_openat, sys_pread, sys_pwrite, sys_read, sys_write};
use super::mem::sys_mmap;
use super::time::{sys_clock_getres, sys_clock_gettime, sys_clock_nanosleep, sys_nanosleep};
use super::{guard, read_arg};
use crate::errno::{Errno, SysResult};
use crate::fd::FDS;
use crate::linux::fcntl::{AT_FDCWD, AT_SYMLINK_NOFOLLOW};
use crate::linux::syscall_nr::{NATIVE, Sysno};
use crate::linux::time::Timespec;
use crate::{fs, futex, mem, process};

/// Dispatches one system call. Arguments are the raw 64-bit values.
///
/// # Safety
///
/// See the module documentation.
pub(crate) unsafe fn dispatch(nr: i64, a: [i64; 6]) -> SysResult<i64> {
    let sysno = NATIVE.lookup(nr).ok_or(Errno::ENOSYS)?;
    let int = |i: usize| a[i] as i32;
    let uint = |i: usize| a[i] as u32;
    let ptr = |i: usize| a[i] as usize;
    // SAFETY: for every call below, the pointer arguments are forwarded
    // under the contract of this function.
    unsafe {
        match sysno {
            Sysno::Read => sys_read(int(0), ptr(1) as _, ptr(2)),
            Sysno::Write => sys_write(int(0), ptr(1) as _, ptr(2)),
            Sysno::Pread64 => sys_pread(int(0), ptr(1) as _, ptr(2), a[3]),
            Sysno::Pwrite64 => sys_pwrite(int(0), ptr(1) as _, ptr(2), a[3]),
            Sysno::Open => sys_openat(AT_FDCWD, ptr(0) as *const c_char, uint(1), uint(2)),
            Sysno::Openat => sys_openat(int(0), ptr(1) as *const c_char, uint(2), uint(3)),
            Sysno::Close => fs::close(int(0)).map(|()| 0),
            Sysno::Lseek => fs::lseek(int(0), a[1], int(2)),
            Sysno::Fstat => sys_fstat(int(0), ptr(1) as _),
            Sysno::Newfstatat => sys_fstatat(int(0), ptr(1) as _, ptr(2) as _, uint(3)),
            Sysno::Stat => sys_fstatat(AT_FDCWD, ptr(0) as _, ptr(1) as _, 0),
            Sysno::Lstat => sys_fstatat(AT_FDCWD, ptr(0) as _, ptr(1) as _, AT_SYMLINK_NOFOLLOW),
            Sysno::Dup => FDS.dup_min(int(0), 0, false).map(i64::from),
            Sysno::Dup2 => FDS.dup2(int(0), int(1)).map(i64::from),
            Sysno::Dup3 => FDS.dup3(int(0), int(1), uint(2)).map(i64::from),
            Sysno::Fcntl => FDS.fcntl(int(0), int(1), a[2] as u64).map(i64::from),
            Sysno::Mmap => sys_mmap(ptr(0), ptr(1), uint(2), uint(3), int(4), a[5]),
            Sysno::Munmap => mem::munmap(ptr(0), ptr(1)).map(|()| 0),
            Sysno::Mprotect => mem::mprotect(ptr(0), ptr(1), uint(2)).map(|()| 0),
            Sysno::Madvise => mem::madvise(ptr(0), ptr(1), int(2)).map(|()| 0),
            Sysno::SchedYield => {
                process::sched_yield();
                Ok(0)
            }
            Sysno::Getpid => Ok(i64::from(process::getpid())),
            Sysno::Gettid => Ok(i64::from(process::gettid())),
            Sysno::Nanosleep => sys_nanosleep(ptr(0) as _),
            Sysno::ClockGettime => sys_clock_gettime(int(0), ptr(1) as _),
            Sysno::ClockGetres => sys_clock_getres(int(0), ptr(1) as _),
            Sysno::ClockNanosleep => sys_clock_nanosleep(int(0), int(1), ptr(2) as _).map(|()| 0),
            Sysno::Futex => sys_futex(ptr(0), int(1), uint(2), ptr(3), uint(5)),
            Sysno::Getcwd => sys_getcwd(ptr(0) as *mut u8, ptr(1)),
            Sysno::Chdir => fs::chdir(super::path_arg(ptr(0) as *const c_char)?).map(|()| 0),
        }
    }
}

/// The `getcwd` system call: the path with its NUL, `ERANGE` if it does
/// not fit; returns the length written including the NUL.
///
/// # Safety
///
/// `buf` is NULL or writable for `size` bytes.
unsafe fn sys_getcwd(buf: *mut u8, size: usize) -> SysResult<i64> {
    let mut p = fs::getcwd();
    p.push(0);
    if buf.is_null() {
        return Err(Errno::EFAULT);
    }
    if p.len() > size {
        return Err(Errno::ERANGE);
    }
    // SAFETY: the caller's buffer, at least `p.len()` bytes (checked).
    unsafe { core::ptr::copy_nonoverlapping(p.as_ptr(), buf, p.len()) };
    Ok(p.len() as i64)
}

/// `futex(uint32_t* uaddr, int op, uint32_t val, const struct timespec*
/// timeout, uint32_t* uaddr2, uint32_t val3)`.
///
/// # Safety
///
/// `uaddr` is a futex word the caller owns, or NULL / misaligned (which is
/// rejected); `timeout` is NULL or a readable `struct timespec` for the wait
/// operations.
unsafe fn sys_futex(uaddr: usize, op: i32, val: u32, timeout: usize, val3: u32) -> SysResult<i64> {
    futex::check_uaddr(uaddr)?;
    // SAFETY: `uaddr` is non-NULL and 4-byte aligned, and the caller passes
    // a live futex word, which C code accesses atomically like `AtomicU32`.
    let word = unsafe { &*(uaddr as *const AtomicU32) };
    let cmd = op & crate::linux::futex::FUTEX_CMD_MASK;
    let waits = matches!(
        cmd,
        crate::linux::futex::FUTEX_WAIT | crate::linux::futex::FUTEX_WAIT_BITSET
    );
    let t = if waits && timeout != 0 {
        // SAFETY: a non-NULL timeout is a readable timespec (contract).
        Some(unsafe { read_arg(timeout as *const Timespec) }?)
    } else {
        None
    };
    futex::futex(word, op, val, t.as_ref(), val3)
}

/// `long syscall(long number, ...)`.
///
/// # Safety
///
/// See the module documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syscall(
    number: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
) -> i64 {
    // SAFETY: forwarded from this function's contract.
    guard(|| unsafe { dispatch(number, [a1, a2, a3, a4, a5, a6]) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::futex::{FUTEX_PRIVATE_FLAG, FUTEX_WAKE};
    use crate::linux::mman::*;
    use crate::linux::syscall_nr::NATIVE;

    fn nr(s: Sysno) -> i64 {
        NATIVE.number(s).unwrap()
    }

    fn call(s: Sysno, a: [i64; 6]) -> SysResult<i64> {
        // SAFETY: the tests pass valid pointers or none.
        unsafe { dispatch(nr(s), a) }
    }

    #[test]
    fn unknown_number_is_enosys() {
        // SAFETY: no pointers are involved.
        assert_eq!(unsafe { dispatch(100_000, [0; 6]) }, Err(Errno::ENOSYS));
        // SAFETY: as above.
        assert_eq!(unsafe { syscall(-1, 0, 0, 0, 0, 0, 0) }, -1);
        assert_eq!(crate::errno::errno(), crate::linux::errno::ENOSYS);
    }

    #[test]
    fn identity_and_upper_bits_ignored() {
        assert_eq!(
            call(Sysno::Getpid, [0; 6]),
            Ok(i64::from(process::getpid()))
        );
        assert_eq!(
            call(Sysno::Gettid, [0; 6]),
            Ok(i64::from(process::gettid()))
        );
        // A descriptor with garbage in the upper 32 bits is still -1 -> EBADF.
        let fd = 0x1234_5678_ffff_ffffu64 as i64;
        assert_eq!(call(Sysno::Close, [fd, 0, 0, 0, 0, 0]), Err(Errno::EBADF));
    }

    #[test]
    fn futex_and_memory_through_syscall() {
        let w = AtomicU32::new(0);
        let op = i64::from(FUTEX_WAKE | FUTEX_PRIVATE_FLAG);
        let a = w.as_ptr() as i64;
        assert_eq!(call(Sysno::Futex, [a, op, 1, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::Futex, [0, op, 1, 0, 0, 0]), Err(Errno::EFAULT));
        assert_eq!(
            call(Sysno::Futex, [a + 1, op, 1, 0, 0, 0]),
            Err(Errno::EINVAL)
        );

        let len = 2 * PAGE_SIZE as i64;
        let flags = i64::from(MAP_PRIVATE | MAP_ANONYMOUS);
        let prot = i64::from(PROT_READ | PROT_WRITE);
        let p = call(Sysno::Mmap, [0, len, prot, flags, -1, 0]).unwrap();
        assert_eq!(call(Sysno::Mprotect, [p, len, 1, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::Madvise, [p, len, 4, 0, 0, 0]), Ok(0));
        // Fork advice: no process is forked, nothing to do.
        assert_eq!(call(Sysno::Madvise, [p, len, 18, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::Madvise, [p, len, 19, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::Munmap, [p, len, 0, 0, 0, 0]), Ok(0));
    }

    #[test]
    fn clock_through_syscall() {
        let mut t = Timespec::default();
        let tp = &mut t as *mut Timespec as i64;
        assert_eq!(call(Sysno::ClockGettime, [1, tp, 0, 0, 0, 0]), Ok(0));
        assert!(t.tv_sec > 0 || t.tv_nsec > 0);
        assert_eq!(
            call(Sysno::ClockGettime, [1, 0, 0, 0, 0, 0]),
            Err(Errno::EFAULT)
        );
        assert_eq!(call(Sysno::ClockGetres, [1, 0, 0, 0, 0, 0]), Ok(0));
        let z = Timespec::default();
        let zp = &z as *const Timespec as i64;
        assert_eq!(call(Sysno::Nanosleep, [zp, 0, 0, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::ClockNanosleep, [1, 0, zp, 0, 0, 0]), Ok(0));
        assert_eq!(call(Sysno::SchedYield, [0; 6]), Ok(0));
    }

    #[test]
    fn getcwd_through_syscall() {
        let mut b = [0u8; 256];
        let n = call(Sysno::Getcwd, [b.as_mut_ptr() as i64, 256, 0, 0, 0, 0]).unwrap();
        assert!(n >= 2 && b[0] == b'/' && b[n as usize - 1] == 0);
        assert_eq!(
            call(Sysno::Getcwd, [b.as_mut_ptr() as i64, 1, 0, 0, 0, 0]),
            Err(Errno::ERANGE)
        );
        assert_eq!(
            call(Sysno::Getcwd, [0, 256, 0, 0, 0, 0]),
            Err(Errno::EFAULT)
        );
        assert_eq!(call(Sysno::Chdir, [0, 0, 0, 0, 0, 0]), Err(Errno::EFAULT));
    }
}

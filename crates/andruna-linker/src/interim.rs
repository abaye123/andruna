//! System calls the loader answers while andruna-lx does not implement
//! them yet (lx answers `ENOSYS`). [`crate::syscall`] calls lx first and
//! comes here only on `ENOSYS`, so each entry stops being used as soon as
//! lx implements the call; then the entry should be deleted.
//!
//! Everything here is what ART, dex2oat, libcore's natives and bionic need
//! on the paths phase 1 runs (docs/plan/phase1/art-on-linker.md). Where
//! the behavior is a shortcut it says so:
//!
//! - File mappings are private anonymous copies of the file (lx has no
//!   file mappings yet). Writable `MAP_SHARED` mappings are written back
//!   with `pwrite` on `msync`, `munmap` and process exit; other mappings of
//!   the same file do not see the changes before that.
//! - `FUTEX_REQUEUE` and `FUTEX_CMP_REQUEUE` wake the waiters instead of
//!   moving them (a correct, more expensive wake-up: waiters recheck).
//! - `ftruncate` can grow a file (it writes the last byte), not shrink it.
//! - Scheduling calls (`sched_setaffinity`, `setpriority`,
//!   `sched_setscheduler` with a normal policy) are accepted without
//!   effect; `fsync`/`fdatasync` succeed without flushing.
//! - Directory calls (`mkdirat`, `unlinkat`, `renameat`, `faccessat`,
//!   `readlinkat`, `fchmod*`) go to the Windows file system through lx's
//!   mount table.

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::Mutex;

use andruna_lx::linux::errno::*;

use crate::syscall::lx_raw;

#[cfg(target_arch = "x86_64")]
mod nr {
    pub const OPEN: i64 = 2;
    pub const SOCKET: i64 = 41;
    pub const CONNECT: i64 = 42;
    pub const SENDMSG: i64 = 46;
    pub const OPENAT: i64 = 257;
    pub const MMAP: i64 = 9;
    pub const MUNMAP: i64 = 11;
    pub const ACCESS: i64 = 21;
    pub const MSYNC: i64 = 26;
    pub const MINCORE: i64 = 27;
    pub const PREAD64: i64 = 17;
    pub const PWRITE64: i64 = 18;
    pub const DUP: i64 = 32;
    pub const CLOSE: i64 = 3;
    pub const UNAME: i64 = 63;
    pub const FSYNC: i64 = 74;
    pub const FDATASYNC: i64 = 75;
    pub const FTRUNCATE: i64 = 77;
    pub const RENAME: i64 = 82;
    pub const MKDIR: i64 = 83;
    pub const RMDIR: i64 = 84;
    pub const UNLINK: i64 = 87;
    pub const READLINK: i64 = 89;
    pub const CHMOD: i64 = 90;
    pub const FCHMOD: i64 = 91;
    pub const UMASK: i64 = 95;
    pub const GETRLIMIT: i64 = 97;
    pub const GETRUSAGE: i64 = 98;
    pub const SYSINFO: i64 = 99;
    pub const GETPPID: i64 = 110;
    pub const GETPRIORITY: i64 = 140;
    pub const SETPRIORITY: i64 = 141;
    pub const SCHED_SETPARAM: i64 = 142;
    pub const SCHED_GETPARAM: i64 = 143;
    pub const SCHED_SETSCHEDULER: i64 = 144;
    pub const SCHED_GETSCHEDULER: i64 = 145;
    pub const SCHED_GET_PRIORITY_MAX: i64 = 146;
    pub const SCHED_GET_PRIORITY_MIN: i64 = 147;
    pub const PRCTL: i64 = 157;
    pub const SETRLIMIT: i64 = 160;
    pub const FUTEX: i64 = 202;
    pub const SCHED_SETAFFINITY: i64 = 203;
    pub const SCHED_GETAFFINITY: i64 = 204;
    pub const MKDIRAT: i64 = 258;
    pub const UNLINKAT: i64 = 263;
    pub const RENAMEAT: i64 = 264;
    pub const READLINKAT: i64 = 267;
    pub const FCHMODAT: i64 = 268;
    pub const FACCESSAT: i64 = 269;
    pub const PRLIMIT64: i64 = 302;
    pub const GETCPU: i64 = 309;
    pub const RENAMEAT2: i64 = 316;
    pub const MEMBARRIER: i64 = 324;
    pub const FACCESSAT2: i64 = 439;
}

#[cfg(target_arch = "aarch64")]
mod nr {
    pub const OPEN: i64 = -8;
    pub const SOCKET: i64 = 198;
    pub const CONNECT: i64 = 203;
    pub const SENDMSG: i64 = 211;
    pub const OPENAT: i64 = 56;
    pub const MMAP: i64 = 222;
    pub const MUNMAP: i64 = 215;
    pub const ACCESS: i64 = -1;
    pub const MSYNC: i64 = 227;
    pub const MINCORE: i64 = 232;
    pub const PREAD64: i64 = 67;
    pub const PWRITE64: i64 = 68;
    pub const DUP: i64 = 23;
    pub const CLOSE: i64 = 57;
    pub const UNAME: i64 = 160;
    pub const FSYNC: i64 = 82;
    pub const FDATASYNC: i64 = 83;
    pub const FTRUNCATE: i64 = 46;
    pub const RENAME: i64 = -2;
    pub const MKDIR: i64 = -3;
    pub const RMDIR: i64 = -4;
    pub const UNLINK: i64 = -5;
    pub const READLINK: i64 = -6;
    pub const CHMOD: i64 = -7;
    pub const FCHMOD: i64 = 52;
    pub const UMASK: i64 = 166;
    pub const GETRLIMIT: i64 = 163;
    pub const GETRUSAGE: i64 = 165;
    pub const SYSINFO: i64 = 179;
    pub const GETPPID: i64 = 173;
    pub const GETPRIORITY: i64 = 141;
    pub const SETPRIORITY: i64 = 140;
    pub const SCHED_SETPARAM: i64 = 118;
    pub const SCHED_GETPARAM: i64 = 121;
    pub const SCHED_SETSCHEDULER: i64 = 119;
    pub const SCHED_GETSCHEDULER: i64 = 120;
    pub const SCHED_GET_PRIORITY_MAX: i64 = 125;
    pub const SCHED_GET_PRIORITY_MIN: i64 = 126;
    pub const PRCTL: i64 = 167;
    pub const SETRLIMIT: i64 = 164;
    pub const FUTEX: i64 = 98;
    pub const SCHED_SETAFFINITY: i64 = 122;
    pub const SCHED_GETAFFINITY: i64 = 123;
    pub const MKDIRAT: i64 = 34;
    pub const UNLINKAT: i64 = 35;
    pub const RENAMEAT: i64 = 38;
    pub const READLINKAT: i64 = 78;
    pub const FCHMODAT: i64 = 53;
    pub const FACCESSAT: i64 = 48;
    pub const PRLIMIT64: i64 = 261;
    pub const GETCPU: i64 = 168;
    pub const RENAMEAT2: i64 = 276;
    pub const MEMBARRIER: i64 = 283;
    pub const FACCESSAT2: i64 = 439;
}

const AT_FDCWD: i32 = -100;
const AT_REMOVEDIR: i32 = 0x200;

fn err(e: i32) -> i64 {
    -i64::from(e)
}

/// Calls the loader handles before lx sees them. `None`: go on to lx.
///
/// # Safety
///
/// The pointer arguments satisfy the kernel's contract for call `n`.
pub unsafe fn before(n: i64, a: [i64; 6]) -> Option<i64> {
    if n == nr::MUNMAP {
        write_back(a[0] as u64, a[1] as u64, true);
    }
    if n == nr::CLOSE {
        crate::logd::closed(a[0] as i32);
    }
    None
}

/// Retries a call lx failed (any error): opening the `/proc` files the
/// loader provides ([`crate::procfs`]).
///
/// # Safety
///
/// As for [`before`].
pub unsafe fn after_error(n: i64, a: [i64; 6]) -> Option<i64> {
    let (dirfd, path, flags) = match n {
        nr::OPEN => (AT_FDCWD, a[0] as u64, a[1] as u32),
        nr::OPENAT => (a[0] as i32, a[1] as u64, a[2] as u32),
        _ => return None,
    };
    if path == 0 {
        return None;
    }
    // SAFETY: a NUL-terminated path (contract).
    let p = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
    if !p.starts_with(b"/proc/") || dirfd != AT_FDCWD && !p.starts_with(b"/") {
        return None;
    }
    let data = crate::procfs::contents(p)?;
    if flags & 3 != 0 {
        return Some(err(EACCES));
    }
    let f = andruna_lx::fd::OpenFile::new(Box::new(crate::procfs::MemFile::new(data)), 0);
    Some(
        match andruna_lx::fd::FDS.insert(std::sync::Arc::new(f), flags & O_CLOEXEC != 0, 0) {
            Ok(fd) => i64::from(fd),
            Err(e) => err(e.0),
        },
    )
}

const O_CLOEXEC: u32 = 0x80000;

/// Handles a call lx answered with `ENOSYS`; `None` keeps `ENOSYS`.
///
/// # Safety
///
/// As for [`before`].
pub unsafe fn after_enosys(n: i64, a: [i64; 6]) -> Option<i64> {
    // SAFETY: forwarded from this function's contract.
    if let Some(r) = unsafe { crate::signal::syscall(n, a) } {
        return Some(r);
    }
    let int = |i: usize| a[i] as i32;
    let ptr = |i: usize| a[i] as u64;
    // SAFETY: forwarded from this function's contract.
    unsafe {
        Some(match n {
            nr::MMAP => {
                return mmap_file(ptr(0), ptr(1), int(2) as u32, int(3) as u32, int(4), a[5]);
            }
            nr::MSYNC => {
                write_back(ptr(0), ptr(1), false);
                0
            }
            nr::FUTEX => {
                return futex_requeue(ptr(0), int(1), a[2] as u32, a[3] as u32, a[5] as u32);
            }
            nr::FTRUNCATE => ftruncate(int(0), a[1]),
            nr::SOCKET => return socket(int(0), int(1)),
            nr::CONNECT => return connect(int(0), ptr(1), ptr(2) as usize),
            nr::SENDMSG => return sendmsg(int(0), ptr(1)),
            nr::FSYNC | nr::FDATASYNC => check_fd(int(0)),
            nr::UNAME => uname(ptr(0) as *mut u8),
            nr::GETRLIMIT => prlimit(0, int(0), 0, ptr(1)),
            nr::SETRLIMIT => prlimit(0, int(0), ptr(1), 0),
            nr::PRLIMIT64 => prlimit(int(0), int(1), ptr(2), ptr(3)),
            nr::GETRUSAGE => getrusage(int(0), ptr(1) as *mut u8),
            nr::SYSINFO => sysinfo(ptr(0) as *mut u8),
            nr::GETPPID => 1,
            nr::PRCTL => prctl(int(0), ptr(1), ptr(2)),
            nr::GETPRIORITY => 20,
            nr::SETPRIORITY => 0,
            nr::SCHED_GETSCHEDULER => 0,
            nr::SCHED_SETSCHEDULER => sched_setscheduler(int(1)),
            nr::SCHED_GETPARAM => write_zero(ptr(1), 4),
            nr::SCHED_SETPARAM => 0,
            nr::SCHED_GET_PRIORITY_MAX => match int(0) {
                1 | 2 => 99,
                _ => 0,
            },
            nr::SCHED_GET_PRIORITY_MIN => match int(0) {
                1 | 2 => 1,
                _ => 0,
            },
            nr::SCHED_SETAFFINITY => 0,
            nr::SCHED_GETAFFINITY => sched_getaffinity(ptr(1) as usize, ptr(2) as *mut u8),
            nr::GETCPU => getcpu(ptr(0) as *mut u32, ptr(1) as *mut u32),
            nr::MEMBARRIER => membarrier(int(0)),
            nr::MINCORE => mincore(ptr(0), ptr(1), ptr(2) as *mut u8),
            nr::UMASK => 0o022,
            nr::ACCESS => faccessat(AT_FDCWD, ptr(0)),
            nr::FACCESSAT | nr::FACCESSAT2 => faccessat(int(0), ptr(1)),
            nr::MKDIR => mkdirat(AT_FDCWD, ptr(0)),
            nr::MKDIRAT => mkdirat(int(0), ptr(1)),
            nr::RMDIR => unlinkat(AT_FDCWD, ptr(0), AT_REMOVEDIR),
            nr::UNLINK => unlinkat(AT_FDCWD, ptr(0), 0),
            nr::UNLINKAT => unlinkat(int(0), ptr(1), int(2)),
            nr::RENAME => renameat(AT_FDCWD, ptr(0), AT_FDCWD, ptr(1)),
            nr::RENAMEAT => renameat(int(0), ptr(1), int(2), ptr(3)),
            nr::RENAMEAT2 if a[4] == 0 => renameat(int(0), ptr(1), int(2), ptr(3)),
            nr::READLINK => readlinkat(AT_FDCWD, ptr(0), ptr(1) as *mut u8, ptr(2) as usize),
            nr::READLINKAT => readlinkat(int(0), ptr(1), ptr(2) as *mut u8, ptr(3) as usize),
            nr::CHMOD => faccessat(AT_FDCWD, ptr(0)),
            nr::FCHMOD => check_fd(int(0)),
            nr::FCHMODAT => faccessat(int(0), ptr(1)),
            _ => return None,
        })
    }
}

/// `ANDRUNA_ENOSYS`: report every system call that ends with `ENOSYS`.
pub fn report_enosys(n: i64, a: [i64; 6]) {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *T.get_or_init(|| std::env::var_os("ANDRUNA_ENOSYS").is_some()) {
        crate::debug::report(format_args!(
            "system call {n}({:#x}, {:#x}, {:#x}, {:#x}) = -ENOSYS",
            a[0], a[1], a[2], a[3]
        ));
    }
}

fn check_fd(fd: i32) -> i64 {
    match andruna_lx::fd::FDS.get(fd) {
        Ok(_) => 0,
        Err(e) => err(e.0),
    }
}

/// # Safety
///
/// `p` is null or writable for `n` bytes.
unsafe fn write_zero(p: u64, n: usize) -> i64 {
    if p == 0 {
        return err(EINVAL);
    }
    // SAFETY: the function's contract.
    unsafe { core::ptr::write_bytes(p as *mut u8, 0, n) };
    0
}

// ---------------------------------------------------------------------------
// File mappings
// ---------------------------------------------------------------------------

const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;
const MAP_SHARED_VALIDATE: u32 = 3;
const MAP_TYPE: u32 = 0xf;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_POPULATE: u32 = 0x8000;
const MAP_SYNC: u32 = 0x80000;
const PAGE: u64 = 4096;

/// A writable shared file mapping, written back to the file.
struct Shared {
    addr: u64,
    len: u64,
    /// lx descriptor (a duplicate the mapping owns).
    fd: i32,
    off: i64,
}

static SHARED: Mutex<Vec<Shared>> = Mutex::new(Vec::new());

fn file_size(fd: i32) -> Result<i64, i32> {
    andruna_lx::fd::FDS
        .get(fd)
        .and_then(|f| f.stat())
        .map(|s| s.size)
        .map_err(|e| e.0)
}

/// `mmap` of a file as an anonymous copy.
///
/// # Safety
///
/// As for `mmap`: a fixed mapping replaces what is at the address.
unsafe fn mmap_file(addr: u64, len: u64, prot: u32, flags: u32, fd: i32, off: i64) -> Option<i64> {
    if flags & MAP_ANONYMOUS != 0 {
        // Anonymous shared memory: lx's answer stands.
        return None;
    }
    let ty = flags & MAP_TYPE;
    if !matches!(ty, MAP_SHARED | MAP_PRIVATE | MAP_SHARED_VALIDATE) {
        return Some(err(EINVAL));
    }
    if off < 0 || off as u64 % PAGE != 0 || len == 0 {
        return Some(err(EINVAL));
    }
    let size = match file_size(fd) {
        Ok(s) => s,
        Err(e) => return Some(err(e)),
    };
    let anon = (flags & !(MAP_TYPE | MAP_SYNC | MAP_POPULATE)) | MAP_PRIVATE | MAP_ANONYMOUS;
    let a = match andruna_lx::mem::mmap(
        addr as usize,
        len as usize,
        PROT_READ | PROT_WRITE,
        anon,
        -1,
        0,
    ) {
        Ok(a) => a as u64,
        Err(e) => return Some(err(e.0)),
    };
    let avail = (size - off).clamp(0, len as i64) as u64;
    let mut done = 0u64;
    while done < avail {
        // SAFETY: the new mapping is writable for `len >= avail` bytes.
        let r = unsafe {
            lx_raw(
                nr::PREAD64,
                [
                    i64::from(fd),
                    (a + done) as i64,
                    (avail - done) as i64,
                    off + done as i64,
                    0,
                    0,
                ],
            )
        };
        if r <= 0 {
            break;
        }
        done += r as u64;
    }
    if prot != PROT_READ | PROT_WRITE
        && let Err(e) = andruna_lx::mem::mprotect(a as usize, len as usize, prot)
    {
        let _ = andruna_lx::mem::munmap(a as usize, len as usize);
        return Some(err(e.0));
    }
    if ty != MAP_PRIVATE && prot & PROT_WRITE != 0 {
        // SAFETY: dup of a valid descriptor.
        let d = unsafe { lx_raw(nr::DUP, [i64::from(fd), 0, 0, 0, 0, 0]) };
        if d >= 0 {
            SHARED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Shared {
                    addr: a,
                    len,
                    fd: d as i32,
                    off,
                });
        }
    }
    Some(a as i64)
}

/// Writes the shared mappings that overlap `[addr, addr + len)` back to
/// their files; with `unmap`, forgets the ones the range covers.
fn write_back(addr: u64, len: u64, unmap: bool) {
    let mut v = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    let end = addr.saturating_add(len.max(1));
    v.retain(|s| {
        if s.addr >= end || s.addr + s.len <= addr {
            return true;
        }
        flush(s);
        let covered = addr <= s.addr && end >= s.addr + s.len;
        if unmap && covered {
            // SAFETY: closes the mapping's own descriptor.
            unsafe { lx_raw(nr::CLOSE, [i64::from(s.fd), 0, 0, 0, 0, 0]) };
            return false;
        }
        true
    });
}

fn flush(s: &Shared) {
    // Only the part inside the file is written back (the kernel never
    // extends a file through a mapping). A mapping made unreadable since
    // fails the write with EFAULT, and nothing is written.
    let size = file_size(s.fd).unwrap_or(0);
    let n = (size - s.off).clamp(0, s.len as i64);
    if n > 0 {
        // SAFETY: lx reads at most `n <= len` bytes of the mapping.
        unsafe {
            lx_raw(
                nr::PWRITE64,
                [i64::from(s.fd), s.addr as i64, n, s.off, 0, 0],
            )
        };
    }
}

/// Writes every shared mapping back (process exit).
pub fn flush_all() {
    let v = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    for s in v.iter() {
        flush(s);
    }
}

fn ftruncate(fd: i32, len: i64) -> i64 {
    if len < 0 {
        return err(EINVAL);
    }
    let size = match file_size(fd) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    if len == size {
        return 0;
    }
    if len < size {
        crate::debug::report(format_args!(
            "ftruncate: shrinking a file is not supported yet (fd {fd}, {size} -> {len})"
        ));
        return err(EINVAL);
    }
    let zero = 0u8;
    // SAFETY: writes one byte from a local.
    let r = unsafe {
        lx_raw(
            nr::PWRITE64,
            [i64::from(fd), &zero as *const u8 as i64, 1, len - 1, 0, 0],
        )
    };
    if r < 0 { r } else { 0 }
}

// ---------------------------------------------------------------------------
// Sockets: only liblog's logd socket ([`crate::logd`])
// ---------------------------------------------------------------------------

const AF_UNIX: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOCK_CLOEXEC: i32 = 0x80000;

fn socket(domain: i32, ty: i32) -> Option<i64> {
    if domain == AF_UNIX && ty & 0xf == SOCK_DGRAM {
        Some(crate::logd::socket(ty & SOCK_CLOEXEC != 0))
    } else {
        None
    }
}

/// # Safety
///
/// `addr` is a readable `sockaddr` of `len` bytes.
unsafe fn connect(fd: i32, addr: u64, len: usize) -> Option<i64> {
    if !crate::logd::is_socket(fd) {
        return None;
    }
    if addr == 0 || !(2..=110).contains(&len) {
        return Some(err(EINVAL));
    }
    // SAFETY: the function's contract.
    let b = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    let path = &b[2..];
    let path = &path[..path.iter().position(|&c| c == 0).unwrap_or(path.len())];
    Some(crate::logd::connect(fd, path))
}

/// # Safety
///
/// `msg` is a readable `struct msghdr` with valid buffers.
unsafe fn sendmsg(fd: i32, msg: u64) -> Option<i64> {
    if !crate::logd::is_socket(fd) {
        return None;
    }
    if msg == 0 {
        return Some(err(EFAULT));
    }
    // SAFETY: msghdr: name, namelen, iov, iovlen (contract).
    let (iov, n) = unsafe { (*((msg + 16) as *const u64), *((msg + 24) as *const u64)) };
    // SAFETY: as above.
    let d = unsafe { crate::logd::gather(iov as *const [u64; 2], n as usize) };
    crate::logd::record(&d);
    Some(d.len() as i64)
}

// ---------------------------------------------------------------------------
// Futex requeue
// ---------------------------------------------------------------------------

const FUTEX_REQUEUE: i32 = 3;
const FUTEX_CMP_REQUEUE: i32 = 4;
const FUTEX_CMD_MASK: i32 = !(128 | 256);

/// `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` as a wake-up of up to
/// `val + val2` waiters on `uaddr` (waiters always recheck their
/// condition, so waking instead of moving is correct).
///
/// # Safety
///
/// `uaddr` is a futex word of the caller.
unsafe fn futex_requeue(uaddr: u64, op: i32, val: u32, val2: u32, val3: u32) -> Option<i64> {
    let cmd = op & FUTEX_CMD_MASK;
    if cmd != FUTEX_REQUEUE && cmd != FUTEX_CMP_REQUEUE {
        return None;
    }
    if uaddr == 0 || uaddr % 4 != 0 {
        return Some(err(if uaddr == 0 { EFAULT } else { EINVAL }));
    }
    if cmd == FUTEX_CMP_REQUEUE {
        // SAFETY: an aligned futex word of the caller (contract).
        let cur = unsafe {
            (*(uaddr as *const std::sync::atomic::AtomicU32))
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        if cur != val3 {
            return Some(err(EAGAIN));
        }
    }
    let n = (val as i64 + val2 as i64).min(i32::MAX as i64) as i32;
    Some(i64::from(andruna_lx::futex::wake(uaddr as usize, n)))
}

// ---------------------------------------------------------------------------
// System information
// ---------------------------------------------------------------------------

fn copy_field(dst: *mut u8, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(64);
    // SAFETY: each utsname field has 65 bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(b.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
}

fn uname(buf: *mut u8) -> i64 {
    if buf.is_null() {
        return err(EFAULT);
    }
    let machine = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "aarch64"
    };
    // SAFETY: the caller's struct utsname (6 fields of 65 bytes).
    unsafe { core::ptr::write_bytes(buf, 0, 6 * 65) };
    for (i, s) in [
        "Linux",
        "localhost",
        "6.6.0-andruna",
        "#1 SMP PREEMPT",
        machine,
        "(none)",
    ]
    .iter()
    .enumerate()
    {
        // SAFETY: inside the struct.
        copy_field(unsafe { buf.add(i * 65) }, s);
    }
    0
}

const RLIMIT_STACK: i32 = 3;
const RLIMIT_NOFILE: i32 = 7;
const RLIM_NLIMITS: i32 = 16;
const RLIM_INFINITY: u64 = u64::MAX;

static RLIMITS: Mutex<Option<HashMap<i32, (u64, u64)>>> = Mutex::new(None);

fn default_rlimit(r: i32) -> (u64, u64) {
    match r {
        RLIMIT_STACK => (crate::process::MAIN_STACK, RLIM_INFINITY),
        RLIMIT_NOFILE => {
            let n = andruna_lx::fd::NOFILE_MAX as u64;
            (n, n)
        }
        _ => (RLIM_INFINITY, RLIM_INFINITY),
    }
}

/// # Safety
///
/// `new` and `old` are null or point to `struct rlimit` (two u64).
unsafe fn prlimit(pid: i32, r: i32, new: u64, old: u64) -> i64 {
    if pid != 0 && pid != andruna_lx::process::getpid() {
        return err(ESRCH);
    }
    if !(0..RLIM_NLIMITS).contains(&r) {
        return err(EINVAL);
    }
    let mut t = RLIMITS.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.get_or_insert_with(HashMap::new);
    let cur = *t.get(&r).unwrap_or(&default_rlimit(r));
    if old != 0 {
        // SAFETY: the caller's struct rlimit.
        unsafe { (old as *mut [u64; 2]).write_unaligned([cur.0, cur.1]) };
    }
    if new != 0 {
        // SAFETY: as above.
        let [s, h] = unsafe { (new as *const [u64; 2]).read_unaligned() };
        if s > h || h > cur.1 {
            return err(if s > h { EINVAL } else { EPERM });
        }
        t.insert(r, (s, h));
    }
    0
}

fn filetime_us(f: windows::Win32::Foundation::FILETIME) -> u64 {
    ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) / 10
}

fn getrusage(who: i32, buf: *mut u8) -> i64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes,
    };
    if buf.is_null() {
        return err(EFAULT);
    }
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the caller's struct rusage (144 bytes).
    unsafe { core::ptr::write_bytes(buf, 0, 144) };
    let ok = match who {
        // SAFETY: pseudo handles of the current process and thread.
        0 => {
            unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.is_ok()
        }
        // SAFETY: as above.
        1 => unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) }.is_ok(),
        -1 => return 0,
        _ => return err(EINVAL),
    };
    if ok {
        let (ut, st) = (filetime_us(u), filetime_us(k));
        let words = [
            ut / 1_000_000,
            ut % 1_000_000,
            st / 1_000_000,
            st % 1_000_000,
        ];
        // SAFETY: ru_utime and ru_stime, the first 32 bytes.
        unsafe { (buf as *mut [u64; 4]).write_unaligned(words) };
    }
    0
}

fn sysinfo(buf: *mut u8) -> i64 {
    use windows::Win32::System::SystemInformation::{
        GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
    };
    if buf.is_null() {
        return err(EFAULT);
    }
    let mut m = MEMORYSTATUSEX {
        dwLength: core::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: a correctly sized structure.
    let _ = unsafe { GlobalMemoryStatusEx(&mut m) };
    // struct sysinfo on LP64 (112 bytes): uptime, loads[3], totalram,
    // freeram, sharedram, bufferram, totalswap, freeswap, procs (u16),
    // pad, totalhigh, freehigh, mem_unit (u32).
    let mut w = [0u64; 14];
    // SAFETY: no arguments.
    w[0] = unsafe { GetTickCount64() } / 1000;
    w[4] = m.ullTotalPhys;
    w[5] = m.ullAvailPhys;
    w[8] = m.ullTotalPageFile.saturating_sub(m.ullTotalPhys);
    w[9] = m.ullAvailPageFile.saturating_sub(m.ullAvailPhys);
    w[10] = 1; // procs
    w[13] = 1; // mem_unit
    // SAFETY: the caller's struct sysinfo.
    unsafe { (buf as *mut [u64; 14]).write_unaligned(w) };
    0
}

const PR_SET_PDEATHSIG: i32 = 1;
const PR_GET_DUMPABLE: i32 = 3;
const PR_SET_DUMPABLE: i32 = 4;
const PR_SET_NAME: i32 = 15;
const PR_GET_NAME: i32 = 16;
const PR_SET_TIMERSLACK: i32 = 29;
const PR_SET_NO_NEW_PRIVS: i32 = 38;
const PR_SET_VMA: i32 = 0x5356_4d41;

thread_local! {
    static NAME: std::cell::Cell<[u8; 16]> = const { std::cell::Cell::new([0; 16]) };
}

fn prctl(op: i32, a1: u64, _a2: u64) -> i64 {
    match op {
        PR_SET_NAME => {
            if a1 == 0 {
                return err(EFAULT);
            }
            let mut n = [0u8; 16];
            // SAFETY: a NUL-terminated name; at most 15 bytes are kept.
            let s = unsafe { CStr::from_ptr(a1 as *const core::ffi::c_char) }.to_bytes();
            let k = s.len().min(15);
            n[..k].copy_from_slice(&s[..k]);
            NAME.with(|c| c.set(n));
            set_thread_description(&s[..k]);
            0
        }
        PR_GET_NAME => {
            if a1 == 0 {
                return err(EFAULT);
            }
            let n = NAME.with(|c| c.get());
            // SAFETY: the caller's 16-byte buffer.
            unsafe { core::ptr::copy_nonoverlapping(n.as_ptr(), a1 as *mut u8, 16) };
            0
        }
        PR_GET_DUMPABLE => 1,
        PR_SET_DUMPABLE | PR_SET_PDEATHSIG | PR_SET_TIMERSLACK | PR_SET_NO_NEW_PRIVS => 0,
        // As a kernel without CONFIG_ANON_VMA_NAME.
        PR_SET_VMA => err(EINVAL),
        _ => err(EINVAL),
    }
}

/// The Windows thread description, so debuggers and crash dumps show
/// guest thread names.
fn set_thread_description(name: &[u8]) {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadDescription};
    let w: Vec<u16> = String::from_utf8_lossy(name)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: a NUL-terminated wide string that outlives the call.
    let _ = unsafe { SetThreadDescription(GetCurrentThread(), windows::core::PCWSTR(w.as_ptr())) };
}

fn sched_setscheduler(policy: i32) -> i64 {
    // SCHED_OTHER, SCHED_BATCH, SCHED_IDLE (and SCHED_RESET_ON_FORK).
    match policy & !0x4000_0000 {
        0 | 3 | 5 => 0,
        _ => err(EPERM),
    }
}

fn cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(1024)
}

fn sched_getaffinity(size: usize, mask: *mut u8) -> i64 {
    let n = cpus();
    let bytes = n.div_ceil(64) * 8;
    if size < bytes || size % 8 != 0 {
        return err(EINVAL);
    }
    if mask.is_null() {
        return err(EFAULT);
    }
    // SAFETY: the caller's mask of `size >= bytes` bytes.
    unsafe {
        core::ptr::write_bytes(mask, 0, bytes);
        for i in 0..n {
            *mask.add(i / 8) |= 1 << (i % 8);
        }
    }
    bytes as i64
}

fn getcpu(cpu: *mut u32, node: *mut u32) -> i64 {
    use windows::Win32::System::Threading::GetCurrentProcessorNumber;
    // SAFETY: no arguments.
    let c = unsafe { GetCurrentProcessorNumber() };
    // SAFETY: null or the caller's out-parameters.
    unsafe {
        if !cpu.is_null() {
            *cpu = c;
        }
        if !node.is_null() {
            *node = 0;
        }
    }
    0
}

const MEMBARRIER_CMD_QUERY: i32 = 0;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 8;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 16;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE: i32 = 32;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE: i32 = 64;

fn membarrier(cmd: i32) -> i64 {
    use windows::Win32::System::Threading::FlushProcessWriteBuffers;
    match cmd {
        MEMBARRIER_CMD_QUERY => i64::from(
            MEMBARRIER_CMD_PRIVATE_EXPEDITED
                | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED
                | MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE
                | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE,
        ),
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED
        | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE => 0,
        MEMBARRIER_CMD_PRIVATE_EXPEDITED | MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE => {
            // An IPI to every processor running a thread of the process:
            // the same guarantee as the expedited membarrier.
            // SAFETY: no arguments.
            unsafe { FlushProcessWriteBuffers() };
            0
        }
        _ => err(EINVAL),
    }
}

fn mincore(addr: u64, len: u64, vec: *mut u8) -> i64 {
    use windows::Win32::System::Memory::{MEM_FREE, MEMORY_BASIC_INFORMATION, VirtualQuery};
    if addr % PAGE != 0 {
        return err(EINVAL);
    }
    let pages = len.div_ceil(PAGE);
    let mut p = addr;
    for i in 0..pages {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries the address space only.
        let n = unsafe {
            VirtualQuery(
                Some(p as *const core::ffi::c_void),
                &mut m,
                core::mem::size_of_val(&m),
            )
        };
        if n == 0 || m.State == MEM_FREE {
            return err(ENOMEM);
        }
        if !vec.is_null() {
            // SAFETY: the caller's vector of `pages` bytes.
            unsafe { *vec.add(i as usize) = u8::from(m.State.0 == 0x1000) };
        }
        p += PAGE;
    }
    0
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The Windows path of a guest path relative to `dirfd`.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
unsafe fn host_path(dirfd: i32, path: u64) -> Result<(Vec<u8>, PathBuf), i32> {
    use andruna_lx::fs::mount::MOUNTS;
    use andruna_lx::fs::path::normalize;
    if path == 0 {
        return Err(EFAULT);
    }
    // SAFETY: the function's contract.
    let p = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
    if p.is_empty() {
        return Err(ENOENT);
    }
    let base = if p[0] == b'/' {
        b"/".to_vec()
    } else if dirfd == AT_FDCWD {
        andruna_lx::fs::getcwd()
    } else {
        let f = andruna_lx::fd::FDS.get(dirfd).map_err(|e| e.0)?;
        f.ops().dir_path().ok_or(ENOTDIR)?.to_vec()
    };
    let lp = normalize(&base, p).map_err(|e| e.0)?;
    let w = MOUNTS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(&lp)
        .map_err(|e| e.0)?;
    let s = String::from_utf16_lossy(&w[..w.len().saturating_sub(1)]);
    Ok((lp.to_bytes(), PathBuf::from(s)))
}

fn io_errno(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => ENOENT,
        PermissionDenied => EACCES,
        AlreadyExists => EEXIST,
        DirectoryNotEmpty => ENOTEMPTY,
        NotADirectory => ENOTDIR,
        IsADirectory => EISDIR,
        InvalidInput | InvalidFilename => EINVAL,
        _ => EIO,
    }
}

unsafe fn faccessat(dirfd: i32, path: u64) -> i64 {
    // SAFETY: forwarded.
    match unsafe { host_path(dirfd, path) } {
        Ok((_, p)) if p.exists() => 0,
        Ok(_) => err(ENOENT),
        Err(e) => err(e),
    }
}

unsafe fn mkdirat(dirfd: i32, path: u64) -> i64 {
    // SAFETY: forwarded.
    match unsafe { host_path(dirfd, path) } {
        Ok((_, p)) => match std::fs::create_dir(&p) {
            Ok(()) => 0,
            Err(e) => err(io_errno(&e)),
        },
        Err(e) => err(e),
    }
}

unsafe fn unlinkat(dirfd: i32, path: u64, flags: i32) -> i64 {
    // SAFETY: forwarded.
    let p = match unsafe { host_path(dirfd, path) } {
        Ok((_, p)) => p,
        Err(e) => return err(e),
    };
    let r = if flags & AT_REMOVEDIR != 0 {
        std::fs::remove_dir(&p)
    } else if p.is_dir() {
        return err(EISDIR);
    } else {
        std::fs::remove_file(&p)
    };
    match r {
        Ok(()) => 0,
        Err(e) => err(io_errno(&e)),
    }
}

unsafe fn renameat(olddir: i32, old: u64, newdir: i32, new: u64) -> i64 {
    // SAFETY: forwarded.
    let (a, b) = match unsafe { (host_path(olddir, old), host_path(newdir, new)) } {
        (Ok((_, a)), Ok((_, b))) => (a, b),
        (Err(e), _) | (_, Err(e)) => return err(e),
    };
    match std::fs::rename(&a, &b) {
        Ok(()) => 0,
        Err(e) => err(io_errno(&e)),
    }
}

/// The guest path of the running executable, for `/proc/self/exe`.
pub static EXE: Mutex<String> = Mutex::new(String::new());

unsafe fn readlinkat(dirfd: i32, path: u64, buf: *mut u8, size: usize) -> i64 {
    if buf.is_null() || size == 0 {
        return err(EINVAL);
    }
    if path == 0 {
        return err(EFAULT);
    }
    // SAFETY: a NUL-terminated string (contract).
    let s = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
    let target = if s == b"/proc/self/exe" {
        EXE.lock().unwrap_or_else(|e| e.into_inner()).clone()
    } else {
        // SAFETY: forwarded.
        return match unsafe { host_path(dirfd, path) } {
            // Symbolic links are not created by Andruna's tree yet: a file
            // that exists is not a link.
            Ok((_, p)) if p.exists() => err(EINVAL),
            Ok(_) => err(ENOENT),
            Err(e) => err(e),
        };
    };
    let n = target.len().min(size);
    // SAFETY: the caller's buffer of `size` bytes.
    unsafe { core::ptr::copy_nonoverlapping(target.as_ptr(), buf, n) };
    n as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uname_reports_linux() {
        let mut b = [0u8; 6 * 65];
        assert_eq!(uname(b.as_mut_ptr()), 0);
        assert!(b.starts_with(b"Linux\0"));
        let m = &b[4 * 65..];
        let machine = if cfg!(target_arch = "x86_64") {
            &b"x86_64\0"[..]
        } else {
            &b"aarch64\0"[..]
        };
        assert!(m.starts_with(machine));
    }

    #[test]
    fn rlimits() {
        let mut old = [0u64; 2];
        // SAFETY: valid pointers.
        unsafe {
            assert_eq!(prlimit(0, RLIMIT_NOFILE, 0, old.as_mut_ptr() as u64), 0);
            assert_eq!(old[0], andruna_lx::fd::NOFILE_MAX as u64);
            let new = [100u64, 200];
            assert_eq!(prlimit(0, 12, new.as_ptr() as u64, 0), 0);
            assert_eq!(prlimit(0, 12, 0, old.as_mut_ptr() as u64), 0);
            assert_eq!(old, [100, 200]);
            let bad = [300u64, 200];
            assert_eq!(prlimit(0, 12, bad.as_ptr() as u64, 0), err(EINVAL));
            assert_eq!(prlimit(0, 99, 0, 0), err(EINVAL));
        }
    }

    #[test]
    fn affinity_and_membarrier() {
        let mut m = [0u8; 128];
        let n = sched_getaffinity(128, m.as_mut_ptr());
        assert!(n >= 8);
        assert_eq!(m[0] & 1, 1);
        assert!(
            membarrier(MEMBARRIER_CMD_QUERY) & i64::from(MEMBARRIER_CMD_PRIVATE_EXPEDITED) != 0
        );
        assert_eq!(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED), 0);
        assert_eq!(membarrier(1), err(EINVAL));
    }

    #[test]
    fn prctl_names() {
        let name = c"worker-thread-name-long";
        assert_eq!(prctl(PR_SET_NAME, name.as_ptr() as u64, 0), 0);
        let mut b = [0u8; 16];
        assert_eq!(prctl(PR_GET_NAME, b.as_mut_ptr() as u64, 0), 0);
        assert_eq!(&b[..15], b"worker-thread-n");
        assert_eq!(b[15], 0);
        assert_eq!(prctl(PR_SET_VMA, 0, 0), err(EINVAL));
    }

    #[test]
    fn requeue_wakes_and_compares() {
        let w = std::sync::atomic::AtomicU32::new(5);
        let a = w.as_ptr() as u64;
        // SAFETY: a live futex word.
        unsafe {
            assert_eq!(
                futex_requeue(a, FUTEX_CMP_REQUEUE, 1, 10, 4),
                Some(err(EAGAIN))
            );
            assert_eq!(futex_requeue(a, FUTEX_CMP_REQUEUE | 128, 1, 10, 5), Some(0));
            assert_eq!(futex_requeue(a, FUTEX_REQUEUE, 1, 0, 0), Some(0));
            assert_eq!(futex_requeue(a, 1, 1, 0, 0), None);
        }
    }

    #[test]
    fn mincore_of_mapped_and_free_memory() {
        let v = vec![0u8; 3 * PAGE as usize];
        let base = (v.as_ptr() as u64).next_multiple_of(PAGE);
        let mut out = [0u8; 1];
        assert_eq!(mincore(base, PAGE, out.as_mut_ptr()), 0);
        assert_eq!(out[0], 1);
        assert_eq!(mincore(base + 1, PAGE, out.as_mut_ptr()), err(EINVAL));
        assert_eq!(mincore(0x1000, PAGE, out.as_mut_ptr()), err(ENOMEM));
    }
}

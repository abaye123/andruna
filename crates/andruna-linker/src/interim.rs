//! System calls the loader answers while andruna-lx does not implement
//! them (lx answers `ENOSYS`). [`crate::syscall`] calls lx first and comes
//! here only on `ENOSYS`, so each entry stops being used as soon as lx
//! implements the call; then the entry should be deleted.
//!
//! Signals, file mappings, futex requeue, `/proc`, directory calls,
//! `prctl` and scheduling are andruna-lx's; what remains here is what ART,
//! dex2oat, libcore's natives and bionic still need on the paths of
//! docs/plan/phase1/art-on-linker.md:
//!
//! - `uname`, rlimits (`getrlimit`, `setrlimit`, `prlimit64`), `getrusage`,
//!   `sysinfo`, `getppid`, `getpriority`/`setpriority` (accepted without
//!   effect), `getcpu`, `membarrier`, `mincore`, `umask`, `fsync`,
//!   `msync` (shared file mappings already reach the file through their
//!   section; nothing is forced to disk), `chmod`/`fchmod`/`fchmodat`
//!   (accepted on files that exist: lx reports fixed modes).
//! - Writable `MAP_SHARED` file mappings (lx answers `ENODEV`; dex2oat
//!   writes vdex and oat files through them): lx's private copy of the
//!   file, written back with `pwrite` on `msync`, `munmap` and process
//!   exit. Other mappings of the file do not see the changes before that.
//! - `socket(AF_UNIX, SOCK_DGRAM)`, `connect` and `sendmsg` for liblog's
//!   logd socket only ([`crate::logd`]).
//! - `/sys/devices/system/cpu/{online,possible,present}`
//!   ([`crate::procfs`]), opened when lx fails to.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::Mutex;

use andruna_lx::linux::errno::*;

use crate::syscall::lx_raw;

#[cfg(target_arch = "x86_64")]
mod nr {
    pub const OPEN: i64 = 2;
    pub const MMAP: i64 = 9;
    pub const MUNMAP: i64 = 11;
    pub const PWRITE64: i64 = 18;
    pub const DUP: i64 = 32;
    pub const CLOSE: i64 = 3;
    pub const CHMOD: i64 = 90;
    pub const FCHMOD: i64 = 91;
    pub const MSYNC: i64 = 26;
    pub const MINCORE: i64 = 27;
    pub const SOCKET: i64 = 41;
    pub const CONNECT: i64 = 42;
    pub const SENDMSG: i64 = 46;
    pub const UNAME: i64 = 63;
    pub const FSYNC: i64 = 74;
    pub const UMASK: i64 = 95;
    pub const GETRLIMIT: i64 = 97;
    pub const GETRUSAGE: i64 = 98;
    pub const SYSINFO: i64 = 99;
    pub const GETPPID: i64 = 110;
    pub const GETPRIORITY: i64 = 140;
    pub const SETPRIORITY: i64 = 141;
    pub const SETRLIMIT: i64 = 160;
    pub const OPENAT: i64 = 257;
    pub const FCHMODAT: i64 = 268;
    pub const PRLIMIT64: i64 = 302;
    pub const GETCPU: i64 = 309;
    pub const MEMBARRIER: i64 = 324;
}

#[cfg(target_arch = "aarch64")]
mod nr {
    /// Not on arm64 (only the `*at` forms exist).
    pub const OPEN: i64 = -1;
    pub const CHMOD: i64 = -2;
    pub const CLOSE: i64 = 57;
    pub const MMAP: i64 = 222;
    pub const MUNMAP: i64 = 215;
    pub const PWRITE64: i64 = 68;
    pub const DUP: i64 = 23;
    pub const FCHMOD: i64 = 52;
    pub const MSYNC: i64 = 227;
    pub const MINCORE: i64 = 232;
    pub const SOCKET: i64 = 198;
    pub const CONNECT: i64 = 203;
    pub const SENDMSG: i64 = 211;
    pub const UNAME: i64 = 160;
    pub const FSYNC: i64 = 82;
    pub const UMASK: i64 = 166;
    pub const GETRLIMIT: i64 = 163;
    pub const GETRUSAGE: i64 = 165;
    pub const SYSINFO: i64 = 179;
    pub const GETPPID: i64 = 173;
    pub const GETPRIORITY: i64 = 141;
    pub const SETPRIORITY: i64 = 140;
    pub const SETRLIMIT: i64 = 164;
    pub const OPENAT: i64 = 56;
    pub const FCHMODAT: i64 = 53;
    pub const PRLIMIT64: i64 = 261;
    pub const GETCPU: i64 = 168;
    pub const MEMBARRIER: i64 = 283;
}

const AT_FDCWD: i32 = -100;
const PAGE: u64 = 4096;

fn err(e: i32) -> i64 {
    -i64::from(e)
}

/// Calls the loader looks at before lx sees them. `None`: go on to lx.
///
/// # Safety
///
/// The pointer arguments satisfy the kernel's contract for call `n`.
pub unsafe fn before(n: i64, a: [i64; 6]) -> Option<i64> {
    if n == nr::CLOSE {
        crate::logd::closed(a[0] as i32);
    }
    if n == nr::MUNMAP {
        write_back(a[0] as u64, a[1] as u64, true);
    }
    None
}

/// Retries a call lx failed (any error): opening the `/sys` files the
/// loader provides ([`crate::procfs`]).
///
/// # Safety
///
/// As for [`before`].
pub unsafe fn after_error(n: i64, a: [i64; 6], r: i64) -> Option<i64> {
    if n == nr::MMAP && r == -i64::from(ENODEV) {
        // SAFETY: forwarded from this function's contract.
        return unsafe { mmap_shared_writable(a) };
    }
    // Absolute paths only, so the directory descriptor does not matter.
    let (path, flags) = match n {
        nr::OPEN => (a[0] as u64, a[1] as u32),
        nr::OPENAT => (a[1] as u64, a[2] as u32),
        _ => return None,
    };
    if path == 0 {
        return None;
    }
    // SAFETY: a NUL-terminated path (contract).
    let p = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
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
    let int = |i: usize| a[i] as i32;
    let ptr = |i: usize| a[i] as u64;
    // SAFETY: forwarded from this function's contract.
    unsafe {
        Some(match n {
            nr::MSYNC => {
                write_back(ptr(0), ptr(1), false);
                0
            }
            nr::SOCKET => return socket(int(0), int(1)),
            nr::CONNECT => return connect(int(0), ptr(1), ptr(2) as usize),
            nr::SENDMSG => return sendmsg(int(0), ptr(1)),
            nr::FSYNC => check_fd(int(0)),
            nr::UNAME => uname(ptr(0) as *mut u8),
            nr::GETRLIMIT => prlimit(0, int(0), 0, ptr(1)),
            nr::SETRLIMIT => prlimit(0, int(0), ptr(1), 0),
            nr::PRLIMIT64 => prlimit(int(0), int(1), ptr(2), ptr(3)),
            nr::GETRUSAGE => getrusage(int(0), ptr(1) as *mut u8),
            nr::SYSINFO => sysinfo(ptr(0) as *mut u8),
            nr::GETPPID => 1,
            nr::GETPRIORITY => 20,
            nr::SETPRIORITY => 0,
            nr::GETCPU => getcpu(ptr(0) as *mut u32, ptr(1) as *mut u32),
            nr::MEMBARRIER => membarrier(int(0)),
            nr::MINCORE => mincore(ptr(0), ptr(1), ptr(2) as *mut u8),
            nr::UMASK => 0o022,
            nr::CHMOD => exists(AT_FDCWD, ptr(0)),
            nr::FCHMOD => check_fd(int(0)),
            nr::FCHMODAT => exists(int(0), ptr(1)),
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

/// 0 when `path` (relative to `dirfd`) exists, else lx's error.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
unsafe fn exists(dirfd: i32, path: u64) -> i64 {
    if path == 0 {
        return err(EFAULT);
    }
    // SAFETY: the function's contract.
    let p = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
    match andruna_lx::fs::fstatat(dirfd, p, 0) {
        Ok(_) => 0,
        Err(e) => err(e.0),
    }
}

// ---------------------------------------------------------------------------
// Writable shared file mappings
// ---------------------------------------------------------------------------

const PROT_WRITE: u32 = 2;
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;
const MAP_SHARED_VALIDATE: u32 = 3;
const MAP_TYPE: u32 = 0xf;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_SYNC: u32 = 0x80000;

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

/// After lx refused a writable `MAP_SHARED` file mapping (`ENODEV`): lx's
/// private copy of the file, registered for write-back. `None` for every
/// other `mmap` failure.
///
/// # Safety
///
/// As for `mmap`: a fixed mapping replaces what is at the address.
unsafe fn mmap_shared_writable(mut a: [i64; 6]) -> Option<i64> {
    let (len, prot, flags, fd) = (a[1] as u64, a[2] as u32, a[3] as u32, a[4] as i32);
    let ty = flags & MAP_TYPE;
    if flags & MAP_ANONYMOUS != 0
        || prot & PROT_WRITE == 0
        || !matches!(ty, MAP_SHARED | MAP_SHARED_VALIDATE)
        || fd < 0
    {
        return None;
    }
    a[3] = i64::from((flags & !(MAP_TYPE | MAP_SYNC)) | MAP_PRIVATE);
    // SAFETY: the caller's arguments with a private mapping type.
    let r = unsafe { lx_raw(nr::MMAP, a) };
    if r < 0 {
        return Some(r);
    }
    // SAFETY: dup of the descriptor the mapping was made from.
    let d = unsafe { lx_raw(nr::DUP, [i64::from(fd), 0, 0, 0, 0, 0]) };
    if d >= 0 {
        SHARED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Shared {
                addr: r as u64,
                len,
                fd: d as i32,
                off: a[5],
            });
    }
    Some(r)
}

/// Writes the shared mappings that overlap `[addr, addr + len)` back to
/// their files; with `unmap`, forgets the ones the range covers.
fn write_back(addr: u64, len: u64, unmap: bool) {
    let mut v = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if v.is_empty() {
        return;
    }
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
        0 => {
            // SAFETY: pseudo handle of the current process.
            unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.is_ok()
        }
        // SAFETY: pseudo handle of the current thread.
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
    if !addr.is_multiple_of(PAGE) {
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
    fn membarrier_commands() {
        assert!(
            membarrier(MEMBARRIER_CMD_QUERY) & i64::from(MEMBARRIER_CMD_PRIVATE_EXPEDITED) != 0
        );
        assert_eq!(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED), 0);
        assert_eq!(membarrier(1), err(EINVAL));
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

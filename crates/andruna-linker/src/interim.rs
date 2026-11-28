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
//! - Writable `MAP_SHARED` file mappings (lx answers `ENODEV`; dex2oat
//!   writes vdex and oat files through them): lx's private copy of the
//!   file, written back with `pwrite` on `msync`, `munmap` and process
//!   exit. Other mappings of the file do not see the changes before that.

use std::sync::Mutex;

use andruna_lx::linux::errno::*;

use crate::syscall::lx_raw;

#[cfg(target_arch = "x86_64")]
mod nr {
    pub const MMAP: i64 = 9;
    pub const MUNMAP: i64 = 11;
    pub const PWRITE64: i64 = 18;
    pub const DUP: i64 = 32;
    pub const CLOSE: i64 = 3;
    pub const MSYNC: i64 = 26;
}

#[cfg(target_arch = "aarch64")]
mod nr {
    pub const CLOSE: i64 = 57;
    pub const MMAP: i64 = 222;
    pub const MUNMAP: i64 = 215;
    pub const PWRITE64: i64 = 68;
    pub const DUP: i64 = 23;
    pub const MSYNC: i64 = 227;
}

/// Calls the loader looks at before lx sees them. `None`: go on to lx.
///
/// # Safety
///
/// The pointer arguments satisfy the kernel's contract for call `n`.
pub unsafe fn before(n: i64, a: [i64; 6]) -> Option<i64> {
    if n == nr::MUNMAP {
        write_back(a[0] as u64, a[1] as u64, true);
    } else if n == nr::MSYNC {
        write_back(a[0] as u64, a[1] as u64, false);
    }
    None
}

/// Retries a call lx failed: a writable shared file mapping (`ENODEV`).
///
/// # Safety
///
/// As for [`before`].
pub unsafe fn after_error(n: i64, a: [i64; 6], r: i64) -> Option<i64> {
    if n == nr::MMAP && r == -i64::from(ENODEV) {
        // SAFETY: forwarded from this function's contract.
        return unsafe { mmap_shared_writable(a) };
    }
    None
}

/// Handles a call lx answered with `ENOSYS`; `None` keeps `ENOSYS`.
/// Nothing is left here: every call the loader answered is lx's now.
///
/// # Safety
///
/// As for [`before`].
pub unsafe fn after_enosys(_n: i64, _a: [i64; 6]) -> Option<i64> {
    None
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

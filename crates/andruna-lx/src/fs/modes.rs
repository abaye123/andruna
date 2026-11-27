//! File modes and ownership: `umask`, `chmod` family, `chown` family.
//!
//! Modes are stored per file ([`crate::sys::ea`]) when lx creates a file
//! or directory (the `open`/`mkdir` mode less the umask) and when a
//! program changes them; `stat` and `access` report them. Files that lx
//! did not create and nobody changed report a mode derived from their
//! Windows attributes (`0644`, `0444` read-only, `0755` directories).
//!
//! # Deviations from Linux
//!
//! - Permission bits are recorded and reported, not enforced: Windows'
//!   access control decides what the process may open. `faccessat` checks
//!   the owner bits.
//! - Every file belongs to the process's user and group. `chown` to those
//!   ids (or `-1`) succeeds; any other owner is `EPERM`, as for a process
//!   without `CAP_CHOWN`.
//! - On a volume without extended attributes (FAT, some network shares)
//!   modes are not stored and the derived ones are reported.

use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::linux::stat::{S_IFMT, S_IFREG, S_IFSOCK};
use crate::sys::ea;

/// The process's umask. Android starts application processes with 077.
static UMASK: AtomicU32 = AtomicU32::new(0o077);

/// `umask`: sets the mask, returns the old one.
pub fn umask(mask: u32) -> u32 {
    UMASK.swap(mask & 0o777, Ordering::Relaxed)
}

/// The current umask.
pub fn current_umask() -> u32 {
    UMASK.load(Ordering::Relaxed)
}

/// The mode a new file of type `kind` gets for the requested `mode`.
pub(crate) fn creation_mode(kind: u32, mode: u32) -> u32 {
    kind | (mode & 0o7777 & !current_umask())
}

/// Records the mode of a file just created through `h` (opened with
/// `FILE_WRITE_EA`). A volume without extended attributes keeps the
/// derived mode.
pub(crate) fn set_created(h: &OwnedHandle, mode: u32) {
    let _ = ea::write_mode(h.raw(), mode);
}

/// Stores `perm` on the open file `h`, keeping the stored file type of a
/// special file.
fn store(h: windows::Win32::Foundation::HANDLE, perm: u32, info: &os::Info) -> SysResult<()> {
    let kind = match info.mode {
        Some(m) if m & S_IFMT == S_IFSOCK => S_IFSOCK,
        _ if info.directory => crate::linux::stat::S_IFDIR,
        _ => S_IFREG,
    };
    let w = ea::reopen_for_ea(h)?;
    ea::write_mode(w.raw(), kind | (perm & 0o7777))
}

/// `fchmod`.
pub fn fchmod(fd: i32, mode: u32) -> SysResult<()> {
    let f = FDS.get(fd)?;
    if f.access() == O_PATH {
        return Err(Errno::EBADF);
    }
    let h = if let Some(r) = f.downcast::<RegularFile>() {
        r.raw_handle()
    } else if let Some(d) = f.downcast::<Directory>() {
        d.raw_handle()
    } else {
        // Pipes, sockets and lx's own files have no stored mode.
        return Err(Errno::EPERM);
    };
    let info = os::info(h)?;
    store(h, mode, &info)
}

/// `fchmodat` / `fchmodat2`.
pub fn fchmodat(dirfd: i32, path: &[u8], mode: u32, flags: u32) -> SysResult<()> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(Errno::EINVAL);
    }
    if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
        return fchmod(dirfd, mode);
    }
    if proc_lookup(dirfd, path, flags & AT_SYMLINK_NOFOLLOW != 0)?.is_some() {
        return Err(Errno::EPERM);
    }
    let r = resolve_at(dirfd, path)?;
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    let (h, info) = open_handle(
        &r,
        FILE_READ_ATTRIBUTES.0 | FILE_WRITE_EA.0,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        nofollow,
    )?;
    if nofollow && is_symlink_tag(info.reparse_tag) {
        // Linux cannot change the mode of a symbolic link.
        return Err(Errno::EOPNOTSUPP);
    }
    let info = os::Info {
        mode: ea::read_mode(h.raw()).or(info.mode),
        ..info
    };
    store(h.raw(), mode, &info)
}

fn check_owner(uid: u32, gid: u32) -> SysResult<()> {
    let ok = |want: u32, have: u32| want == u32::MAX || want == have;
    if ok(uid, crate::process::uid()) && ok(gid, crate::process::gid()) {
        Ok(())
    } else {
        Err(Errno::EPERM)
    }
}

/// `fchown`.
pub fn fchown(fd: i32, uid: u32, gid: u32) -> SysResult<()> {
    FDS.get(fd)?;
    check_owner(uid, gid)
}

/// `fchownat` (and `chown`, `lchown`).
pub fn fchownat(dirfd: i32, path: &[u8], uid: u32, gid: u32, flags: u32) -> SysResult<()> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(Errno::EINVAL);
    }
    fstatat(dirfd, path, flags)?;
    check_owner(uid, gid)
}

/// Marks the file at `path` (just created by `bind`) as a socket.
pub(crate) fn mark_socket(path: &[u8]) -> SysResult<()> {
    let r = resolve_at(AT_FDCWD, path)?;
    let (h, _) = open_handle(
        &r,
        FILE_READ_ATTRIBUTES.0 | FILE_WRITE_EA.0,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        true,
    )?;
    ea::write_mode(h.raw(), creation_mode(S_IFSOCK, 0o777))
}

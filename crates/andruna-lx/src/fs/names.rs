//! Operations on names: `unlinkat`, `mkdirat`, `renameat2`, `symlinkat`,
//! `readlinkat`, `truncate`, `faccessat`.
//!
//! # Deviations from Linux
//!
//! - Symbolic links are Windows symbolic links, which Windows follows when
//!   a path is opened. Creating one needs developer mode or the
//!   symbolic-link privilege; without it `symlink` fails with `EPERM`, as on
//!   a Linux file system that does not support links. An absolute target is
//!   stored as the Windows path of its mount, a relative one as relative
//!   Windows components; `readlink` converts both back. Links created by
//!   WSL (Linux targets) are read too.
//! - `faccessat` checks against the fixed modes lx reports
//!   ([`super::file::stat_from_info`]).
//! - `RENAME_EXCHANGE` and `RENAME_WHITEOUT` fail with `EINVAL`, as on file
//!   systems that do not support them.

use super::*;
use crate::sys::file::LinkTarget;

/// The `DELETE` access right.
const DELETE: u32 = 0x0001_0000;

pub const AT_REMOVEDIR: u32 = 0x200;
pub const RENAME_NOREPLACE: u32 = 1;
pub const RENAME_EXCHANGE: u32 = 2;
pub const RENAME_WHITEOUT: u32 = 4;
pub const F_OK: i32 = 0;
pub const X_OK: i32 = 1;
pub const W_OK: i32 = 2;
pub const R_OK: i32 = 4;
pub const AT_EACCESS: u32 = 0x200;

fn name_error(code: u32, r: &Resolved) -> Errno {
    match code {
        80 | 183 => Errno::EEXIST,
        145 => Errno::ENOTEMPTY,
        17 => Errno(crate::linux::errno::EXDEV),
        32 => Errno::EBUSY,
        1314 => Errno::EPERM,
        2 | 3 => not_found(&r.linux),
        c => Errno::from_win32(c),
    }
}

/// Opens the name itself (a link is not followed) for `access`.
fn open_name(r: &Resolved, access: u32) -> SysResult<(OwnedHandle, os::Info)> {
    open_handle(
        r,
        access | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        true,
    )
}

fn is_dir(info: &os::Info) -> bool {
    info.directory && !is_symlink_tag(info.reparse_tag)
}

/// `unlinkat`.
pub fn unlinkat(dirfd: i32, path: &[u8], flags: u32) -> SysResult<()> {
    if flags & !AT_REMOVEDIR != 0 {
        return Err(Errno::EINVAL);
    }
    let r = resolve_at(dirfd, path)?;
    if flags & AT_REMOVEDIR != 0 && r.linux.components.last().is_none() {
        return Err(Errno::EBUSY);
    }
    let (h, info) = open_name(&r, DELETE)?;
    match (flags & AT_REMOVEDIR != 0, is_dir(&info)) {
        (true, false) => return Err(Errno::ENOTDIR),
        (false, true) => return Err(Errno::EISDIR),
        _ => {}
    }
    if !is_dir(&info) && r.linux.trailing_slash {
        return Err(Errno::ENOTDIR);
    }
    os::delete(h.raw()).map_err(|c| name_error(c, &r))
}

/// `mkdirat`. The mode is not stored (see [`super`]).
pub fn mkdirat(dirfd: i32, path: &[u8], mode: u32) -> SysResult<()> {
    let r = resolve_at(dirfd, path)?;
    if os::attributes(&r.windows).is_some() {
        return Err(Errno::EEXIST);
    }
    os::create_directory(&r.windows).map_err(|c| name_error(c, &r))?;
    // Record the mode; the directory exists either way.
    if let Ok((h, _)) = open_name(&r, FILE_WRITE_EA.0) {
        let m = super::modes::creation_mode(crate::linux::stat::S_IFDIR, mode);
        super::modes::set_created(&h, m);
    }
    Ok(())
}

/// `renameat2`.
pub fn renameat2(
    olddirfd: i32,
    old: &[u8],
    newdirfd: i32,
    new: &[u8],
    flags: u32,
) -> SysResult<()> {
    if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE | RENAME_WHITEOUT) != 0
        || flags & (RENAME_EXCHANGE | RENAME_WHITEOUT) != 0
    {
        return Err(Errno::EINVAL);
    }
    let from = resolve_at(olddirfd, old)?;
    let to = resolve_at(newdirfd, new)?;
    let (h, info) = open_name(&from, DELETE)?;
    let from_dir = is_dir(&info);
    if to.linux.components.starts_with(&from.linux.components)
        && to.linux.components.len() > from.linux.components.len()
    {
        // A directory cannot move into itself.
        return Err(Errno::EINVAL);
    }
    if let Ok((_, t)) = open_name(&to, 0) {
        if flags & RENAME_NOREPLACE != 0 {
            return Err(Errno::EEXIST);
        }
        if t.volume == info.volume && t.file_id == info.file_id {
            // The same file: nothing to do.
            return Ok(());
        }
        match (from_dir, is_dir(&t)) {
            (true, false) => return Err(Errno::ENOTDIR),
            (false, true) => return Err(Errno::EISDIR),
            _ => {}
        }
    } else if !from_dir && to.linux.trailing_slash {
        return Err(Errno::ENOTDIR);
    }
    os::rename(h.raw(), &to.windows, flags & RENAME_NOREPLACE == 0).map_err(|c| name_error(c, &to))
}

/// Converts a Linux link target to the Windows form stored in the link.
fn windows_target(target: &[u8]) -> SysResult<Vec<u16>> {
    let mut out = Vec::new();
    if target.first() == Some(&b'/') {
        let p = path::normalize(b"/", target)?;
        out = util::read(&MOUNTS).resolve(&p)?;
        out.pop();
    } else {
        for c in target.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
            if !out.is_empty() {
                out.push(u16::from(b'\\'));
            }
            match c {
                b"." | b".." => out.extend(c.iter().map(|&b| u16::from(b))),
                c => out.extend(path::encode_component(c)),
            }
        }
    }
    out.push(0);
    Ok(out)
}

/// `symlinkat`.
pub fn symlinkat(target: &[u8], newdirfd: i32, linkpath: &[u8]) -> SysResult<()> {
    if target.is_empty() {
        return Err(Errno::ENOENT);
    }
    let link = resolve_at(newdirfd, linkpath)?;
    if os::attributes(&link.windows).is_some() || open_name(&link, 0).is_ok() {
        return Err(Errno::EEXIST);
    }
    // Windows needs to know whether the target is a directory.
    let parent = link
        .linux
        .prefix(link.linux.components.len().saturating_sub(1));
    let dir = path::normalize(&parent.to_bytes(), target)
        .ok()
        .and_then(|p| util::read(&MOUNTS).resolve(&p).ok())
        .is_some_and(|w| is_directory(&w));
    let t = windows_target(target)?;
    os::create_symlink(&link.windows, &t, dir).map_err(|c| name_error(c, &link))
}

/// The Linux form of a stored link target.
fn linux_target(t: LinkTarget) -> Vec<u8> {
    match t {
        LinkTarget::Linux(b) => b,
        LinkTarget::Windows { name, relative } => {
            let parts: Vec<&[u16]> = name.split(|&c| c == u16::from(b'\\')).collect();
            if relative {
                let mut out = Vec::new();
                for (i, c) in parts.iter().enumerate() {
                    if i > 0 {
                        out.push(b'/');
                    }
                    out.extend(path::decode_component(c));
                }
                return out;
            }
            // `\??\C:\dir\name` -> `\\?\C:\dir\name`, then back through the
            // mount table.
            let mut w: Vec<u16> = name.clone();
            if w.starts_with(&[92, 63, 63, 92]) {
                w[1] = 92;
            }
            match util::read(&MOUNTS).unresolve(&w) {
                Some(p) => p,
                None => String::from_utf16_lossy(&name).into_bytes(),
            }
        }
    }
}

/// `readlinkat`: the target without a terminating NUL, at most `size`
/// bytes.
pub fn readlinkat(dirfd: i32, path: &[u8], size: usize) -> SysResult<Vec<u8>> {
    if size == 0 || size > i32::MAX as usize {
        return Err(Errno::EINVAL);
    }
    if let Some((_, node)) = proc_lookup(dirfd, path, true)? {
        let proc::Node::Link(mut t) = node else {
            return Err(Errno::EINVAL);
        };
        t.truncate(size);
        return Ok(t);
    }
    let r = resolve_at(dirfd, path)?;
    let (h, info) = open_name(&r, 0)?;
    if !is_symlink_tag(info.reparse_tag) {
        return Err(Errno::EINVAL);
    }
    let mut t = linux_target(os::read_link(h.raw())?);
    t.truncate(size);
    Ok(t)
}

/// `truncate`.
pub fn truncate(path: &[u8], len: i64) -> SysResult<()> {
    let len = u64::try_from(len).map_err(|_| Errno::EINVAL)?;
    let r = resolve_at(AT_FDCWD, path)?;
    let (h, info) = open_handle(
        &r,
        GENERIC_WRITE.0 | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        false,
    )?;
    if info.directory {
        return Err(Errno::EISDIR);
    }
    os::set_len(h.raw(), len)
}

/// `faccessat` / `faccessat2`.
pub fn faccessat(dirfd: i32, path: &[u8], mode: i32, flags: u32) -> SysResult<()> {
    if mode & !(R_OK | W_OK | X_OK) != 0
        || flags & !(AT_EACCESS | AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0
    {
        return Err(Errno::EINVAL);
    }
    let st = fstatat(dirfd, path, flags & (AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH))?;
    // The owner's bits apply: lx's files belong to the process's ids.
    let bits = (st.mode >> 6) & 7;
    let want = mode as u32;
    if want & bits != want {
        return Err(Errno::EACCES);
    }
    Ok(())
}

//! The mount table: which Windows directory holds each part of the Linux
//! file tree.
//!
//! A mount maps a normalized Linux directory (`/`, `/data/user/0/pkg`, ...)
//! to an absolute Windows directory. A path is resolved through the mount
//! with the longest matching prefix; the remaining components are escaped
//! ([`super::path::encode_component`]) and appended. Windows paths are
//! produced in the `\\?\` form, so they are not limited to `MAX_PATH` and
//! Windows does not reinterpret them.
//!
//! This round sets up one root mapping; the full Android layout
//! (`/system`, `/data`, `/storage`, ...) is configured by the host process
//! when it exists.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::RwLock;

use super::path::{LinuxPath, encode_component, normalize};
use crate::errno::{Errno, SysResult};
use crate::util;

#[derive(Clone, Debug)]
struct Mount {
    /// Normalized Linux components of the mount point.
    linux: Vec<Vec<u8>>,
    /// Absolute Windows directory in `\\?\` form, without a trailing
    /// separator (except a drive root, `\\?\C:\`).
    windows: Vec<u16>,
}

/// A set of mounts.
#[derive(Clone, Debug, Default)]
pub struct MountTable {
    mounts: Vec<Mount>,
}

/// The process's mount table.
pub static MOUNTS: RwLock<MountTable> = RwLock::new(MountTable::new());

/// Adds or replaces a mount in the process's table.
pub fn mount(linux_dir: &str, windows_dir: &Path) -> SysResult<()> {
    util::write(&MOUNTS).add(linux_dir, windows_dir)
}

/// Converts an absolute Windows path to the `\\?\` form with backslashes.
fn verbatim(windows_dir: &Path) -> SysResult<Vec<u16>> {
    let s: Vec<u16> = windows_dir
        .as_os_str()
        .encode_wide()
        .map(|c| {
            if c == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                c
            }
        })
        .collect();
    let text = String::from_utf16_lossy(&s);
    let mut out: Vec<u16> = if text.starts_with(r"\\?\") {
        s
    } else if let Some(unc) = text.strip_prefix(r"\\") {
        r"\\?\UNC\"
            .encode_utf16()
            .chain(unc.encode_utf16())
            .collect()
    } else if text.len() >= 3 && text.as_bytes()[1] == b':' && text.as_bytes()[2] == b'\\' {
        r"\\?\".encode_utf16().chain(s).collect()
    } else {
        // Relative or drive-relative paths are not allowed.
        return Err(Errno::EINVAL);
    };
    // Remove `.` and `..` (the `\\?\` form is taken literally) and repeated
    // separators.
    let prefix_len = 4;
    let rest = String::from_utf16_lossy(&out[prefix_len..]);
    let mut parts: Vec<&str> = Vec::new();
    for p in rest.split('\\') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            p => parts.push(p),
        }
    }
    let mut joined = parts.join("\\");
    if parts.len() == 1 && joined.ends_with(':') {
        joined.push('\\');
    }
    out.truncate(prefix_len);
    out.extend(joined.encode_utf16());
    Ok(out)
}

impl MountTable {
    pub const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Maps the Linux directory `linux_dir` (absolute) to the absolute
    /// Windows directory `windows_dir`. Replaces an existing mount at the
    /// same Linux directory.
    pub fn add(&mut self, linux_dir: &str, windows_dir: &Path) -> SysResult<()> {
        if !linux_dir.starts_with('/') {
            return Err(Errno::EINVAL);
        }
        let linux = normalize(b"/", linux_dir.as_bytes())?.components;
        let windows = verbatim(windows_dir)?;
        self.mounts.retain(|m| m.linux != linux);
        self.mounts.push(Mount { linux, windows });
        // Longest prefix first.
        self.mounts
            .sort_by_key(|m| core::cmp::Reverse(m.linux.len()));
        Ok(())
    }

    /// The Windows path of `path`, NUL-terminated. `ENOENT` when no mount
    /// covers it.
    pub fn resolve(&self, path: &LinuxPath) -> SysResult<Vec<u16>> {
        let m = self
            .mounts
            .iter()
            .find(|m| path.components.starts_with(&m.linux))
            .ok_or(Errno::ENOENT)?;
        let mut out = m.windows.clone();
        for c in &path.components[m.linux.len()..] {
            if out.last() != Some(&u16::from(b'\\')) {
                out.push(u16::from(b'\\'));
            }
            out.extend(encode_component(c));
        }
        out.push(0);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(t: &MountTable, p: &str) -> SysResult<String> {
        let lp = normalize(b"/", p.as_bytes())?;
        let w = t.resolve(&lp)?;
        Ok(String::from_utf16(&w[..w.len() - 1]).unwrap())
    }

    #[test]
    fn root_mount() {
        let mut t = MountTable::new();
        assert_eq!(resolve(&t, "/a"), Err(Errno::ENOENT));
        t.add("/", Path::new(r"C:\andruna\root\")).unwrap();
        assert_eq!(resolve(&t, "/").unwrap(), r"\\?\C:\andruna\root");
        assert_eq!(
            resolve(&t, "/data/a:b").unwrap(),
            "\\\\?\\C:\\andruna\\root\\data\\a\u{f03a}b"
        );
    }

    #[test]
    fn longest_prefix_wins() {
        let mut t = MountTable::new();
        t.add("/", Path::new(r"C:\r")).unwrap();
        t.add("/data/user", Path::new("D:/u/./x/../y")).unwrap();
        assert_eq!(resolve(&t, "/data/user/0").unwrap(), r"\\?\D:\u\y\0");
        assert_eq!(
            resolve(&t, "/data/username").unwrap(),
            r"\\?\C:\r\data\username"
        );
        assert_eq!(resolve(&t, "/data").unwrap(), r"\\?\C:\r\data");
        // Replacing a mount.
        t.add("/data/user/", Path::new(r"E:\z")).unwrap();
        assert_eq!(resolve(&t, "/data/user/1").unwrap(), r"\\?\E:\z\1");
    }

    #[test]
    fn windows_forms() {
        let mut t = MountTable::new();
        t.add("/", Path::new(r"C:\")).unwrap();
        assert_eq!(resolve(&t, "/").unwrap(), r"\\?\C:\");
        assert_eq!(resolve(&t, "/x").unwrap(), r"\\?\C:\x");
        t.add("/", Path::new(r"\\server\share\dir")).unwrap();
        assert_eq!(resolve(&t, "/x").unwrap(), r"\\?\UNC\server\share\dir\x");
        t.add("/", Path::new(r"\\?\C:\v")).unwrap();
        assert_eq!(resolve(&t, "/x").unwrap(), r"\\?\C:\v\x");
        assert_eq!(t.add("/", Path::new("relative")), Err(Errno::EINVAL));
        assert_eq!(t.add("/", Path::new("C:rel")), Err(Errno::EINVAL));
        assert_eq!(t.add("rel", Path::new(r"C:\x")), Err(Errno::EINVAL));
    }
}

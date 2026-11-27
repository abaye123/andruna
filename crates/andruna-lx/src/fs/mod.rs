//! Files and paths: `open`/`openat`, I/O on descriptors, `stat` family.
//!
//! # Deviations from Linux (this round)
//!
//! - Permission bits are not stored; `stat` reports fixed modes (see
//!   [`file::stat_from_info`]) and the `mode` argument of `open` is ignored.
//! - Symbolic links are not resolved by lx yet. Windows follows its own
//!   symbolic links on open; `O_NOFOLLOW` and `AT_SYMLINK_NOFOLLOW` detect
//!   them (`ELOOP`, `S_IFLNK`).
//! - `..` is resolved lexically ([`path::normalize`]).
//! - `*at` calls relative to a directory descriptor use the directory's
//!   path at open time; a directory renamed while open is not followed.
//! - `O_DIRECT` is accepted and ignored (I/O stays buffered); `O_TMPFILE`
//!   fails with `EOPNOTSUPP`, as on file systems without support for it.

pub mod file;
pub mod mount;
pub mod path;
pub mod stream;

use std::sync::{Arc, RwLock};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
    GENERIC_WRITE,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_CREATION_DISPOSITION,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_WRITE_THROUGH,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_WRITE_DATA, OPEN_ALWAYS, OPEN_EXISTING,
    SYNCHRONIZE,
};

use self::file::{Directory, RegularFile, is_symlink_tag, stat_from_info};
use self::mount::MOUNTS;
use self::path::{LinuxPath, normalize};
use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, OpenFile};
use crate::linux::fcntl::*;
use crate::linux::stat::StatData;
use crate::sys::OwnedHandle;
use crate::sys::file as os;
use crate::util::{self, UserBuf};

/// The current working directory, a normalized Linux path (empty means
/// `/`).
static CWD: RwLock<Vec<u8>> = RwLock::new(Vec::new());

/// Sets the working directory used for relative paths (`chdir` is added
/// with the rest of the path calls).
pub fn set_cwd(dir: &str) -> SysResult<()> {
    let p = normalize(b"/", dir.as_bytes())?;
    *util::write(&CWD) = p.to_bytes();
    Ok(())
}

/// `chdir`: the directory must exist (`ENOENT`) and be a directory
/// (`ENOTDIR`).
pub fn chdir(path: &[u8]) -> SysResult<()> {
    let st = fstatat(AT_FDCWD, path, 0)?;
    if st.mode & crate::linux::stat::S_IFMT != crate::linux::stat::S_IFDIR {
        return Err(Errno::ENOTDIR);
    }
    let p = normalize(&cwd(), path)?;
    *util::write(&CWD) = p.to_bytes();
    Ok(())
}

/// The working directory, an absolute Linux path.
pub fn getcwd() -> Vec<u8> {
    cwd()
}

fn cwd() -> Vec<u8> {
    let c = util::read(&CWD);
    if c.is_empty() {
        b"/".to_vec()
    } else {
        c.clone()
    }
}

/// A path resolved for `*at` calls.
struct Resolved {
    linux: LinuxPath,
    /// NUL-terminated Windows path.
    windows: Vec<u16>,
}

fn resolve_at(dirfd: i32, path: &[u8]) -> SysResult<Resolved> {
    // An empty path is checked before the directory descriptor.
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    let base = if path.first() == Some(&b'/') {
        b"/".to_vec()
    } else if dirfd == AT_FDCWD {
        cwd()
    } else {
        let f = FDS.get(dirfd)?;
        f.ops().dir_path().ok_or(Errno::ENOTDIR)?.to_vec()
    };
    let linux = normalize(&base, path)?;
    let windows = util::read(&MOUNTS).resolve(&linux)?;
    Ok(Resolved { linux, windows })
}

/// Refines "not found" for `path`: `ENOTDIR` when a parent component
/// exists but is not a directory.
fn not_found(path: &LinuxPath) -> Errno {
    let mounts = util::read(&MOUNTS);
    for n in 1..path.components.len() {
        let Ok(w) = mounts.resolve(&path.prefix(n)) else {
            break;
        };
        match os::attributes(&w) {
            None => return Errno::ENOENT,
            Some(a) if a & FILE_ATTRIBUTE_DIRECTORY.0 == 0 => return Errno::ENOTDIR,
            Some(_) => {}
        }
    }
    Errno::ENOENT
}

fn is_directory(windows: &[u16]) -> bool {
    os::attributes(windows).is_some_and(|a| a & FILE_ATTRIBUTE_DIRECTORY.0 != 0)
}

/// Opens `r` and handles reparse points: with `nofollow`, a symbolic link
/// is opened itself; any other reparse point (mount points, cloud files) is
/// opened normally.
fn open_handle(
    r: &Resolved,
    access: u32,
    disposition: FILE_CREATION_DISPOSITION,
    mut flags: FILE_FLAGS_AND_ATTRIBUTES,
    nofollow: bool,
) -> SysResult<(OwnedHandle, os::Info)> {
    flags |= FILE_FLAG_BACKUP_SEMANTICS;
    let base_flags = flags;
    if nofollow {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let map_err = |code: u32| -> Errno {
        match windows::Win32::Foundation::WIN32_ERROR(code) {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_DIRECTORY => not_found(&r.linux),
            ERROR_ACCESS_DENIED
                if access & (GENERIC_WRITE.0 | FILE_WRITE_DATA.0) != 0
                    && is_directory(&r.windows) =>
            {
                Errno::EISDIR
            }
            _ => Errno::from_win32(code),
        }
    };
    let h = os::create(&r.windows, access, disposition, flags).map_err(map_err)?;
    let info = os::info(h.raw())?;
    if nofollow && info.reparse_tag != 0 && !is_symlink_tag(info.reparse_tag) {
        drop(h);
        let h = os::create(&r.windows, access, disposition, base_flags).map_err(map_err)?;
        let info = os::info(h.raw())?;
        return Ok((h, info));
    }
    Ok((h, info))
}

/// `openat`. Returns the new descriptor.
pub fn openat(dirfd: i32, path: &[u8], flags: u32, _mode: u32) -> SysResult<i32> {
    if flags & __O_TMPFILE != 0 {
        return Err(if flags & O_TMPFILE == O_TMPFILE {
            Errno::EOPNOTSUPP
        } else {
            Errno::EINVAL
        });
    }
    let path_only = flags & O_PATH != 0;
    // O_PATH ignores every flag except these.
    let flags = if path_only {
        flags & (O_PATH | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    } else {
        flags
    };
    let acc = flags & O_ACCMODE;
    if !path_only && acc == O_ACCMODE {
        return Err(Errno::EINVAL);
    }
    let r = resolve_at(dirfd, path)?;
    let creat = flags & O_CREAT != 0;
    let trunc = flags & O_TRUNC != 0;
    if creat && r.linux.trailing_slash {
        return Err(Errno::EISDIR);
    }

    let mut access = FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0;
    if !path_only {
        access |= match acc {
            O_RDONLY => GENERIC_READ.0,
            O_WRONLY => GENERIC_WRITE.0,
            _ => GENERIC_READ.0 | GENERIC_WRITE.0,
        };
        if trunc {
            access |= FILE_WRITE_DATA.0;
        }
    }
    let disposition = match (creat, flags & O_EXCL != 0) {
        (true, true) => CREATE_NEW,
        (true, false) => OPEN_ALWAYS,
        (false, _) => OPEN_EXISTING,
    };
    let mut attrs = FILE_ATTRIBUTE_NORMAL;
    if flags & (O_SYNC | O_DSYNC) != 0 {
        attrs |= FILE_FLAG_WRITE_THROUGH;
    }
    let nofollow = flags & O_NOFOLLOW != 0;
    let (h, info) = open_handle(&r, access, disposition, attrs, nofollow)?;

    if is_symlink_tag(info.reparse_tag) && nofollow && !path_only {
        return Err(Errno::ELOOP);
    }
    let ops: Box<dyn crate::fd::FileOps> = if info.directory {
        if !path_only && (acc != O_RDONLY || creat || trunc) {
            return Err(Errno::EISDIR);
        }
        Box::new(Directory::new(h, r.linux.to_bytes()))
    } else {
        if flags & O_DIRECTORY != 0 || r.linux.trailing_slash {
            return Err(Errno::ENOTDIR);
        }
        if trunc && !path_only {
            os::set_len(h.raw(), 0)?;
        }
        Box::new(RegularFile::new(h))
    };
    let file = Arc::new(OpenFile::new(ops, flags));
    FDS.insert(file, flags & O_CLOEXEC != 0, 0)
}

/// `fstatat`.
pub fn fstatat(dirfd: i32, path: &[u8], flags: u32) -> SysResult<StatData> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH | AT_NO_AUTOMOUNT) != 0 {
        return Err(Errno::EINVAL);
    }
    if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
        if dirfd == AT_FDCWD {
            return fstatat(AT_FDCWD, b".", flags & !AT_EMPTY_PATH);
        }
        return FDS.get(dirfd)?.stat();
    }
    let r = resolve_at(dirfd, path)?;
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    let (_h, info) = open_handle(
        &r,
        FILE_READ_ATTRIBUTES.0,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        nofollow,
    )?;
    if r.linux.trailing_slash && !info.directory {
        return Err(Errno::ENOTDIR);
    }
    Ok(stat_from_info(&info))
}

pub fn fstat(fd: i32) -> SysResult<StatData> {
    FDS.get(fd)?.stat()
}

pub fn read(fd: i32, buf: UserBuf) -> SysResult<usize> {
    FDS.get(fd)?.read(buf)
}

pub fn write(fd: i32, buf: UserBuf) -> SysResult<usize> {
    FDS.get(fd)?.write(buf)
}

pub fn pread(fd: i32, buf: UserBuf, offset: i64) -> SysResult<usize> {
    FDS.get(fd)?.pread(buf, offset)
}

pub fn pwrite(fd: i32, buf: UserBuf, offset: i64) -> SysResult<usize> {
    FDS.get(fd)?.pwrite(buf, offset)
}

pub fn lseek(fd: i32, offset: i64, whence: i32) -> SysResult<i64> {
    Ok(FDS.get(fd)?.seek(offset, whence)? as i64)
}

pub fn close(fd: i32) -> SysResult<()> {
    FDS.close(fd)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::linux::stat::{S_IFDIR, S_IFMT, S_IFREG};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    /// Mounts a fresh temporary directory at `/` once per test process and
    /// returns a unique Linux directory under it for one test.
    pub fn test_dir(name: &str) -> String {
        static ROOT: OnceLock<PathBuf> = OnceLock::new();
        let root = ROOT.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("andruna-lx-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            mount::mount("/", &dir).unwrap();
            dir
        });
        std::fs::create_dir_all(root.join(name)).unwrap();
        format!("/{name}")
    }

    fn open(p: &str, flags: u32) -> SysResult<i32> {
        openat(AT_FDCWD, p.as_bytes(), flags, 0o644)
    }

    fn write_all(fd: i32, data: &[u8]) {
        assert_eq!(write(fd, UserBuf::from_ref(data)), Ok(data.len()));
    }

    fn read_n(fd: i32, n: usize) -> Vec<u8> {
        let mut b = vec![0u8; n];
        let got = read(fd, UserBuf::from_mut(&mut b)).unwrap();
        b.truncate(got);
        b
    }

    #[test]
    fn create_write_read_seek() {
        let d = test_dir("crw");
        let p = format!("{d}/f.txt");
        assert_eq!(open(&p, O_RDONLY), Err(Errno::ENOENT));
        let fd = open(&p, O_CREAT | O_RDWR | O_CLOEXEC).unwrap();
        assert_eq!(FDS.cloexec(fd), Ok(true));
        write_all(fd, b"hello world");
        assert_eq!(lseek(fd, 0, SEEK_CUR), Ok(11));
        assert_eq!(lseek(fd, 6, SEEK_SET), Ok(6));
        assert_eq!(read_n(fd, 100), b"world");
        assert_eq!(read_n(fd, 100), b"");
        assert_eq!(lseek(fd, -5, SEEK_END), Ok(6));
        assert_eq!(lseek(fd, -100, SEEK_CUR), Err(Errno::EINVAL));
        assert_eq!(lseek(fd, 0, 99), Err(Errno::EINVAL));
        assert_eq!(lseek(fd, 3, SEEK_DATA), Ok(3));
        assert_eq!(lseek(fd, 3, SEEK_HOLE), Ok(11));
        assert_eq!(lseek(fd, 11, SEEK_DATA), Err(Errno::ENXIO));

        // pread/pwrite leave the offset alone.
        lseek(fd, 2, SEEK_SET).unwrap();
        let mut b = [0u8; 5];
        assert_eq!(pread(fd, UserBuf::from_mut(&mut b), 6,), Ok(5));
        assert_eq!(&b, b"world");
        assert_eq!(pwrite(fd, UserBuf::from_ref(b"W"), 6), Ok(1));
        assert_eq!(lseek(fd, 0, SEEK_CUR), Ok(2));
        assert_eq!(read_n(fd, 100), b"llo World");
        assert_eq!(pread(fd, UserBuf::from_mut(&mut b), -1), Err(Errno::EINVAL));

        let st = fstat(fd).unwrap();
        assert_eq!(st.mode & S_IFMT, S_IFREG);
        assert_eq!(st.size, 11);
        assert_eq!(st.nlink, 1);
        // (Closing twice is covered by the fd table tests; descriptors are
        // shared by the tests running in parallel here.)
        close(fd).unwrap();

        let st2 = fstatat(AT_FDCWD, p.as_bytes(), 0).unwrap();
        assert_eq!(st2.ino, st.ino);
        assert_eq!(st2.dev, st.dev);
        assert_ne!(st2.ino, 0);
    }

    #[test]
    fn create_flags() {
        let d = test_dir("flags");
        let p = format!("{d}/x");
        let fd = open(&p, O_CREAT | O_EXCL | O_WRONLY).unwrap();
        write_all(fd, b"12345");
        close(fd).unwrap();
        assert_eq!(open(&p, O_CREAT | O_EXCL | O_WRONLY), Err(Errno::EEXIST));
        // O_TRUNC empties the file.
        let fd = open(&p, O_WRONLY | O_TRUNC).unwrap();
        assert_eq!(fstat(fd).unwrap().size, 0);
        close(fd).unwrap();
        // Access mode enforcement.
        let fd = open(&p, O_RDONLY).unwrap();
        assert_eq!(write(fd, UserBuf::from_ref(b"x")), Err(Errno::EBADF));
        close(fd).unwrap();
        assert_eq!(open(&p, O_ACCMODE), Err(Errno::EINVAL));
        assert_eq!(open(&p, O_RDWR | O_TMPFILE), Err(Errno::EOPNOTSUPP));
        assert_eq!(open(&format!("{p}/"), O_RDONLY), Err(Errno::ENOTDIR));
        assert_eq!(open(&p, O_RDONLY | O_DIRECTORY), Err(Errno::ENOTDIR));
        assert_eq!(open(&format!("{p}/y"), O_RDONLY), Err(Errno::ENOTDIR));
        assert_eq!(open(&format!("{d}/nodir/y"), O_RDONLY), Err(Errno::ENOENT));
        assert_eq!(
            open(&format!("{d}/new/"), O_CREAT | O_WRONLY),
            Err(Errno::EISDIR)
        );
    }

    #[test]
    fn append() {
        let d = test_dir("append");
        let p = format!("{d}/log");
        let a = open(&p, O_CREAT | O_WRONLY | O_APPEND).unwrap();
        let b = open(&p, O_WRONLY).unwrap();
        write_all(b, b"0123456789");
        write_all(a, b"ab");
        assert_eq!(lseek(a, 0, SEEK_CUR), Ok(12));
        // pwrite with O_APPEND appends (Linux behavior).
        assert_eq!(pwrite(a, UserBuf::from_ref(b"cd"), 0), Ok(2));
        // F_SETFL turns it off.
        FDS.fcntl(a, F_SETFL, 0).unwrap();
        assert_eq!(pwrite(a, UserBuf::from_ref(b"Z"), 0), Ok(1));
        close(a).unwrap();
        close(b).unwrap();
        let r = open(&p, O_RDONLY).unwrap();
        assert_eq!(read_n(r, 100), b"Z123456789abcd");
        close(r).unwrap();
    }

    #[test]
    fn directories_and_at_calls() {
        let d = test_dir("dirs");
        std::fs::create_dir_all(
            std::env::temp_dir()
                .join(format!("andruna-lx-test-{}", std::process::id()))
                .join("dirs")
                .join("sub"),
        )
        .unwrap();
        let dfd = open(&d, O_RDONLY | O_DIRECTORY).unwrap();
        assert_eq!(fstat(dfd).unwrap().mode & S_IFMT, S_IFDIR);
        assert_eq!(open(&d, O_WRONLY), Err(Errno::EISDIR));
        assert_eq!(open(&d, O_RDONLY | O_CREAT), Err(Errno::EISDIR));
        let mut b = [0u8; 4];
        assert_eq!(read(dfd, UserBuf::from_mut(&mut b)), Err(Errno::EISDIR));

        // Relative to the directory descriptor.
        let f = openat(dfd, b"sub/../a:b", O_CREAT | O_RDWR, 0).unwrap();
        write_all(f, b"xyz");
        close(f).unwrap();
        let st = fstatat(dfd, b"a:b", 0).unwrap();
        assert_eq!(st.size, 3);
        // The colon was escaped on disk.
        let on_disk = std::env::temp_dir()
            .join(format!("andruna-lx-test-{}", std::process::id()))
            .join("dirs")
            .join("a\u{f03a}b");
        assert!(on_disk.is_file());
        // A relative path against a file descriptor fails with ENOTDIR.
        let f = openat(dfd, b"a:b", O_RDONLY, 0).unwrap();
        assert_eq!(openat(f, b"x", O_RDONLY, 0), Err(Errno::ENOTDIR));
        assert_eq!(fstatat(f, b"", AT_EMPTY_PATH).unwrap().size, 3);
        assert_eq!(fstatat(f, b"", 0), Err(Errno::ENOENT));
        assert_eq!(fstatat(f, b"x", 0x4), Err(Errno::EINVAL));
        // An absolute path ignores the descriptor.
        assert!(fstatat(f, d.as_bytes(), 0).is_ok());
        assert_eq!(fstatat(dfd, b"a:b/", 0), Err(Errno::ENOTDIR));
        assert_eq!(openat(1234, b"x", O_RDONLY, 0), Err(Errno::EBADF));
        close(f).unwrap();

        // O_PATH descriptors.
        let p = openat(dfd, b"a:b", O_PATH | O_RDWR, 0).unwrap();
        assert_eq!(read(p, UserBuf::from_mut(&mut b)), Err(Errno::EBADF));
        assert_eq!(fstat(p).unwrap().size, 3);
        assert_eq!(FDS.fcntl(p, F_GETFL, 0).unwrap() as u32 & O_PATH, O_PATH);
        close(p).unwrap();
        close(dfd).unwrap();
    }

    #[test]
    fn dup_shares_offset() {
        let d = test_dir("dupoff");
        let p = format!("{d}/f");
        let a = open(&p, O_CREAT | O_RDWR).unwrap();
        write_all(a, b"abcdef");
        lseek(a, 1, SEEK_SET).unwrap();
        let b = FDS.dup_min(a, 0, false).unwrap();
        assert_eq!(read_n(b, 2), b"bc");
        assert_eq!(lseek(a, 0, SEEK_CUR), Ok(3));
        close(a).unwrap();
        // The description stays open through the duplicate.
        assert_eq!(read_n(b, 10), b"def");
        close(b).unwrap();
    }

    #[test]
    fn unlink_while_open_is_allowed() {
        let d = test_dir("unlink");
        let p = format!("{d}/f");
        let fd = open(&p, O_CREAT | O_RDWR).unwrap();
        write_all(fd, b"data");
        let host = std::env::temp_dir()
            .join(format!("andruna-lx-test-{}", std::process::id()))
            .join("unlink")
            .join("f");
        std::fs::remove_file(host).unwrap();
        assert_eq!(pread(fd, UserBuf::from_mut(&mut [0u8; 4]), 0), Ok(4));
        close(fd).unwrap();
    }
}

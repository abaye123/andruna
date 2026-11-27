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
mod names;
pub mod path;
pub mod proc;
pub mod stream;

pub use names::*;

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

fn linux_at(dirfd: i32, path: &[u8]) -> SysResult<LinuxPath> {
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
    normalize(&base, path)
}

fn resolve_at(dirfd: i32, path: &[u8]) -> SysResult<Resolved> {
    let linux = linux_at(dirfd, path)?;
    if proc::is_proc(&linux) {
        // `/proc` is lx's own; names in it cannot be created or removed.
        return Err(Errno::EACCES);
    }
    let windows = util::read(&MOUNTS).resolve(&linux)?;
    Ok(Resolved { linux, windows })
}

/// A `/proc` path with its symbolic links followed (all but the last one
/// with `nofollow`), or `None` for paths outside `/proc`.
fn proc_lookup(
    dirfd: i32,
    path: &[u8],
    nofollow: bool,
) -> SysResult<Option<(LinuxPath, proc::Node)>> {
    let mut linux = linux_at(dirfd, path)?;
    if !proc::is_proc(&linux) {
        return Ok(None);
    }
    for _ in 0..8 {
        // Follow links in the middle of the path (`/proc/self/...`).
        let mut followed = false;
        for n in 2..linux.components.len() {
            let pre = linux.prefix(n);
            if let Ok(proc::Node::Link(t)) = proc::lookup(&pre) {
                let parent = linux.prefix(n - 1).to_bytes();
                let mut np = normalize(&parent, &t)?;
                np.components.extend_from_slice(&linux.components[n..]);
                np.trailing_slash = linux.trailing_slash;
                linux = np;
                followed = true;
                break;
            }
        }
        if followed {
            continue;
        }
        let node = proc::lookup(&linux)?;
        if let proc::Node::Link(t) = &node
            && !nofollow
        {
            let parent = linux.prefix(linux.components.len() - 1).to_bytes();
            let np = normalize(&parent, t)?;
            if !proc::is_proc(&np) {
                return Ok(Some((np, node)));
            }
            linux = np;
            continue;
        }
        return Ok(Some((linux, node)));
    }
    Err(Errno::ELOOP)
}

/// Opens a `/proc` node.
fn proc_open(dirfd: i32, path: &[u8], flags: u32) -> SysResult<Option<i32>> {
    let nofollow = flags & O_NOFOLLOW != 0;
    let Some((linux, node)) = proc_lookup(dirfd, path, nofollow)? else {
        return Ok(None);
    };
    let ops: Box<dyn crate::fd::FileOps> = match node {
        proc::Node::Link(_) if !proc::is_proc(&linux) => {
            // A link out of /proc (fd/N, exe): open the target itself.
            return openat(AT_FDCWD, &linux.to_bytes(), flags, 0).map(Some);
        }
        proc::Node::Link(_) if flags & O_PATH == 0 => return Err(Errno::ELOOP),
        proc::Node::Link(_) => return Err(Errno::ENXIO),
        proc::Node::Dir(e) => {
            if flags & O_ACCMODE != O_RDONLY || flags & O_CREAT != 0 {
                return Err(Errno::EISDIR);
            }
            Box::new(proc::ProcDir::new(linux.to_bytes(), e))
        }
        proc::Node::Props => {
            if flags & O_ACCMODE != O_RDONLY {
                return Err(Errno::EACCES);
            }
            Box::new(proc::ProcFile::props())
        }
        proc::Node::File { content, comm_of } => {
            if flags & O_DIRECTORY != 0 {
                return Err(Errno::ENOTDIR);
            }
            if flags & O_ACCMODE != O_RDONLY && comm_of.is_none() {
                return Err(Errno::EACCES);
            }
            Box::new(proc::ProcFile::new(content, comm_of))
        }
    };
    let file = Arc::new(OpenFile::new(ops, flags));
    FDS.insert(file, flags & O_CLOEXEC != 0, 0).map(Some)
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
    if let Some(fd) = proc_open(dirfd, path, flags)? {
        return Ok(fd);
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
        Box::new(RegularFile::with_path(h, r.linux.to_bytes()))
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
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    if let Some((linux, node)) = proc_lookup(dirfd, path, nofollow)? {
        if !proc::is_proc(&linux) {
            return fstatat(AT_FDCWD, &linux.to_bytes(), flags);
        }
        return Ok(proc::stat(&node));
    }
    let r = resolve_at(dirfd, path)?;
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

pub fn ftruncate(fd: i32, len: i64) -> SysResult<()> {
    FDS.get(fd)?.truncate(len)
}

pub fn fsync(fd: i32) -> SysResult<()> {
    FDS.get(fd)?.sync()
}

/// `getdents64` into a buffer of `max` bytes; returns the records.
pub fn getdents64(fd: i32, max: usize) -> SysResult<Vec<u8>> {
    let f = FDS.get(fd)?;
    let mut out = Vec::new();
    f.getdents(&mut out, max)?;
    Ok(out)
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

    fn names(buf: &[u8]) -> Vec<(String, u8)> {
        let mut out = Vec::new();
        let mut off = 0;
        while off < buf.len() {
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            let kind = buf[off + 18];
            let name = &buf[off + 19..off + reclen];
            let end = name.iter().position(|&b| b == 0).unwrap();
            out.push((String::from_utf8_lossy(&name[..end]).into_owned(), kind));
            off += reclen;
        }
        out
    }

    #[test]
    fn names_unlink_mkdir_rename_getdents() {
        let d = test_dir("names");
        let f = format!("{d}/a.txt");
        let fd = open(&f, O_CREAT | O_RDWR).unwrap();
        write_all(fd, b"hello world");
        // ftruncate shrinks and grows without moving the offset.
        assert_eq!(ftruncate(fd, 5), Ok(()));
        assert_eq!(fstat(fd).unwrap().size, 5);
        assert_eq!(ftruncate(fd, -1), Err(Errno::EINVAL));
        assert_eq!(fsync(fd), Ok(()));
        let ro = open(&f, O_RDONLY).unwrap();
        assert_eq!(ftruncate(ro, 1), Err(Errno::EINVAL));
        assert_eq!(fsync(ro), Ok(()));
        close(ro).unwrap();
        assert_eq!(truncate(f.as_bytes(), 3), Ok(()));
        assert_eq!(fstat(fd).unwrap().size, 3);

        assert_eq!(
            mkdirat(AT_FDCWD, format!("{d}/sub").as_bytes(), 0o755),
            Ok(())
        );
        assert_eq!(
            mkdirat(AT_FDCWD, format!("{d}/sub").as_bytes(), 0o755),
            Err(Errno::EEXIST)
        );
        assert_eq!(
            mkdirat(AT_FDCWD, format!("{d}/no/sub").as_bytes(), 0o755),
            Err(Errno::ENOENT)
        );
        let g = format!("{d}/sub/b.txt");
        assert_eq!(
            renameat2(AT_FDCWD, f.as_bytes(), AT_FDCWD, g.as_bytes(), 0),
            Ok(())
        );
        assert_eq!(
            fstatat(AT_FDCWD, f.as_bytes(), 0).err(),
            Some(Errno::ENOENT)
        );
        // The open descriptor follows the file.
        assert_eq!(fstat(fd).unwrap().size, 3);
        // Replace an existing file; NOREPLACE refuses.
        let h = format!("{d}/c.txt");
        close(open(&h, O_CREAT | O_WRONLY).unwrap()).unwrap();
        assert_eq!(
            renameat2(
                AT_FDCWD,
                h.as_bytes(),
                AT_FDCWD,
                g.as_bytes(),
                RENAME_NOREPLACE
            ),
            Err(Errno::EEXIST)
        );
        assert_eq!(
            renameat2(AT_FDCWD, h.as_bytes(), AT_FDCWD, g.as_bytes(), 0),
            Ok(())
        );
        assert_eq!(fstatat(AT_FDCWD, g.as_bytes(), 0).unwrap().size, 0);
        assert_eq!(
            renameat2(AT_FDCWD, g.as_bytes(), AT_FDCWD, b"/x", RENAME_EXCHANGE),
            Err(Errno::EINVAL)
        );

        // getdents64 lists ., .., the file and nothing else.
        let dfd = open(&format!("{d}/sub"), O_RDONLY | O_DIRECTORY).unwrap();
        let mut all = Vec::new();
        loop {
            let b = getdents64(dfd, 64).unwrap();
            if b.is_empty() {
                break;
            }
            all.extend(names(&b));
        }
        assert_eq!(
            all,
            vec![
                (".".into(), file::DT_DIR),
                ("..".into(), file::DT_DIR),
                ("b.txt".into(), file::DT_REG)
            ]
        );
        // At the end: nothing more, whatever the size.
        assert_eq!(getdents64(dfd, 8), Ok(Vec::new()));
        assert_eq!(lseek(dfd, 0, SEEK_SET), Ok(0));
        assert_eq!(names(&getdents64(dfd, 4096).unwrap()).len(), 3);
        assert_eq!(getdents64(fd, 4096), Err(Errno::ENOTDIR));
        close(dfd).unwrap();

        // unlink and rmdir.
        assert_eq!(
            unlinkat(AT_FDCWD, format!("{d}/sub").as_bytes(), 0),
            Err(Errno::EISDIR)
        );
        assert_eq!(
            unlinkat(AT_FDCWD, format!("{d}/sub").as_bytes(), AT_REMOVEDIR),
            Err(Errno::ENOTEMPTY)
        );
        assert_eq!(
            unlinkat(AT_FDCWD, g.as_bytes(), AT_REMOVEDIR),
            Err(Errno::ENOTDIR)
        );
        assert_eq!(unlinkat(AT_FDCWD, g.as_bytes(), 0), Ok(()));
        assert_eq!(unlinkat(AT_FDCWD, g.as_bytes(), 0), Err(Errno::ENOENT));
        assert_eq!(
            unlinkat(AT_FDCWD, format!("{d}/sub").as_bytes(), AT_REMOVEDIR),
            Ok(())
        );
        // The renamed and unlinked file stays usable through the descriptor.
        assert_eq!(fstat(fd).unwrap().size, 3);
        close(fd).unwrap();

        assert_eq!(
            faccessat(AT_FDCWD, d.as_bytes(), R_OK | W_OK | X_OK, 0),
            Ok(())
        );
        let e = format!("{d}/e");
        close(open(&e, O_CREAT | O_WRONLY).unwrap()).unwrap();
        assert_eq!(faccessat(AT_FDCWD, e.as_bytes(), R_OK | W_OK, 0), Ok(()));
        assert_eq!(
            faccessat(AT_FDCWD, e.as_bytes(), X_OK, 0),
            Err(Errno::EACCES)
        );
        assert_eq!(faccessat(AT_FDCWD, b"/none", F_OK, 0), Err(Errno::ENOENT));
        assert_eq!(faccessat(AT_FDCWD, e.as_bytes(), 8, 0), Err(Errno::EINVAL));
    }

    #[test]
    fn symbolic_links() {
        let d = test_dir("links");
        let target = format!("{d}/t.txt");
        close(open(&target, O_CREAT | O_WRONLY).unwrap()).unwrap();
        let link = format!("{d}/l");
        match symlinkat(b"t.txt", AT_FDCWD, link.as_bytes()) {
            Err(Errno::EPERM) => {
                // Windows refuses links without developer mode or the
                // privilege: lx reports EPERM, as Linux does on file systems
                // without links. Nothing else to check here.
                eprintln!("symbolic links are not available to this user");
                return;
            }
            r => assert_eq!(r, Ok(())),
        }
        assert_eq!(
            readlinkat(AT_FDCWD, link.as_bytes(), 100),
            Ok(b"t.txt".to_vec())
        );
        assert_eq!(readlinkat(AT_FDCWD, link.as_bytes(), 2), Ok(b"t.".to_vec()));
        assert_eq!(
            readlinkat(AT_FDCWD, target.as_bytes(), 100),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            symlinkat(b"t.txt", AT_FDCWD, link.as_bytes()),
            Err(Errno::EEXIST)
        );
        let st = fstatat(AT_FDCWD, link.as_bytes(), AT_SYMLINK_NOFOLLOW).unwrap();
        assert_eq!(st.mode & S_IFMT, crate::linux::stat::S_IFLNK);
        // Opening follows the link (Windows resolves it).
        let fd = open(&link, O_WRONLY).unwrap();
        write_all(fd, b"via link");
        close(fd).unwrap();
        assert_eq!(fstatat(AT_FDCWD, target.as_bytes(), 0).unwrap().size, 8);
        // Absolute targets round-trip through the mount table.
        let abs = format!("{d}/abs");
        symlinkat(target.as_bytes(), AT_FDCWD, abs.as_bytes()).unwrap();
        assert_eq!(
            readlinkat(AT_FDCWD, abs.as_bytes(), 200),
            Ok(target.as_bytes().to_vec())
        );
        // Relative targets with directories.
        let rel = format!("{d}/rel");
        symlinkat(b"../links/t.txt", AT_FDCWD, rel.as_bytes()).unwrap();
        assert_eq!(
            readlinkat(AT_FDCWD, rel.as_bytes(), 200),
            Ok(b"../links/t.txt".to_vec())
        );
        // unlink removes the link, not the target.
        unlinkat(AT_FDCWD, link.as_bytes(), 0).unwrap();
        assert!(fstatat(AT_FDCWD, target.as_bytes(), 0).is_ok());
        // A dangling link can be created and read.
        let dangling = format!("{d}/dangling");
        symlinkat(b"missing", AT_FDCWD, dangling.as_bytes()).unwrap();
        assert_eq!(
            readlinkat(AT_FDCWD, dangling.as_bytes(), 100),
            Ok(b"missing".to_vec())
        );
    }

    #[test]
    fn file_mappings() {
        use crate::linux::mman::*;
        let d = test_dir("mmap");
        let f = format!("{d}/m");
        let fd = open(&f, O_CREAT | O_RDWR).unwrap();
        let mut data = vec![0u8; 3 * PAGE_SIZE + 100];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        write_all(fd, &data);
        // Private, from a page offset, beyond the end of the file.
        let len = 4 * PAGE_SIZE;
        let a = crate::mem::mmap(
            0,
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE,
            fd,
            PAGE_SIZE as i64,
        )
        .unwrap();
        // SAFETY: a readable and writable mapping of `len` bytes.
        let m = unsafe { core::slice::from_raw_parts_mut(a as *mut u8, len) };
        assert_eq!(&m[..2 * PAGE_SIZE + 100], &data[PAGE_SIZE..]);
        assert!(m[2 * PAGE_SIZE + 100..].iter().all(|&b| b == 0));
        // Private writes do not reach the file.
        m[0] = 0xee;
        assert_eq!(
            pread(fd, UserBuf::from_mut(&mut [0u8; 1]), PAGE_SIZE as i64),
            Ok(1)
        );
        let mut one = [0u8; 1];
        pread(fd, UserBuf::from_mut(&mut one), PAGE_SIZE as i64).unwrap();
        assert_eq!(one[0], data[PAGE_SIZE]);
        crate::mem::munmap(a, len).unwrap();

        // Read-only shared: readable, and never writable.
        let a = crate::mem::mmap(0, PAGE_SIZE, PROT_READ, MAP_SHARED, fd, 0).unwrap();
        // SAFETY: a readable mapping of one page.
        assert_eq!(unsafe { *(a as *const u8).add(7) }, data[7]);
        assert_eq!(
            crate::mem::mprotect(a, PAGE_SIZE, PROT_READ | PROT_WRITE),
            Err(Errno::EACCES)
        );
        crate::mem::munmap(a, PAGE_SIZE).unwrap();

        // Errors.
        let rw = PROT_READ | PROT_WRITE;
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, rw, MAP_SHARED, fd, 0),
            Err(Errno::ENODEV)
        );
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, rw, MAP_PRIVATE, fd, 10),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, rw, MAP_PRIVATE, -1, 0),
            Err(Errno::EBADF)
        );
        let wo = open(&f, O_WRONLY).unwrap();
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, PROT_READ, MAP_PRIVATE, wo, 0),
            Err(Errno::EACCES)
        );
        close(wo).unwrap();
        let ro = open(&f, O_RDONLY).unwrap();
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, rw, MAP_SHARED, ro, 0),
            Err(Errno::EACCES)
        );
        // A private writable mapping of a read-only descriptor is allowed.
        let a = crate::mem::mmap(0, PAGE_SIZE, rw, MAP_PRIVATE, ro, 0).unwrap();
        crate::mem::munmap(a, PAGE_SIZE).unwrap();
        close(ro).unwrap();
        let dfd = open(&d, O_RDONLY | O_DIRECTORY).unwrap();
        assert_eq!(
            crate::mem::mmap(0, PAGE_SIZE, PROT_READ, MAP_PRIVATE, dfd, 0),
            Err(Errno::ENODEV)
        );
        close(dfd).unwrap();
        close(fd).unwrap();
    }

    fn read_all(fd: i32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = read_n(fd, 4096);
            if b.is_empty() {
                return out;
            }
            out.extend(b);
        }
    }

    fn read_path(p: &str) -> SysResult<Vec<u8>> {
        let fd = open(p, O_RDONLY)?;
        let b = read_all(fd);
        close(fd)?;
        Ok(b)
    }

    #[test]
    fn proc_self() {
        let d = test_dir("procfs");
        let pid = crate::process::getpid();
        let tid = crate::process::gettid();

        let status = String::from_utf8(read_path("/proc/self/status").unwrap()).unwrap();
        assert!(status.contains(&format!("\nTgid:\t{pid}\n")), "{status}");
        assert!(status.contains("TracerPid:\t0"));
        let by_pid = read_path(&format!("/proc/{pid}/status")).unwrap();
        assert!(String::from_utf8(by_pid).unwrap().contains(&format!(
            "
Pid:	{pid}
"
        )));
        assert_eq!(read_path("/proc/1/status"), Err(Errno::ENOENT));
        assert_eq!(read_path("/proc/self/nonexistent"), Err(Errno::ENOENT));
        assert_eq!(
            readlinkat(AT_FDCWD, b"/proc/self", 64),
            Ok(pid.to_string().into_bytes())
        );
        assert_eq!(
            fstatat(AT_FDCWD, b"/proc/self/fd", 0).unwrap().mode & S_IFMT,
            S_IFDIR
        );
        // Names in /proc cannot be created.
        assert_eq!(
            open("/proc/self/new", O_CREAT | O_WRONLY),
            Err(Errno::ENOENT)
        );
        assert_eq!(
            unlinkat(AT_FDCWD, b"/proc/self/status", 0),
            Err(Errno::EACCES)
        );

        // fd: links to the open files, listed by getdents.
        let f = format!("{d}/x");
        let fd = open(&f, O_CREAT | O_RDWR).unwrap();
        write_all(fd, b"data");
        let link = format!("/proc/self/fd/{fd}");
        assert_eq!(
            readlinkat(AT_FDCWD, link.as_bytes(), 256),
            Ok(f.as_bytes().to_vec())
        );
        let again = open(&link, O_RDONLY).unwrap();
        assert_eq!(read_n(again, 10), b"data");
        close(again).unwrap();
        let dfd = open("/proc/self/fd", O_RDONLY | O_DIRECTORY).unwrap();
        let listed = names(&getdents64(dfd, 65536).unwrap());
        assert!(
            listed
                .iter()
                .any(|(n, k)| *n == fd.to_string() && *k == file::DT_LNK)
        );
        // Relative lookups from the directory descriptor.
        assert_eq!(
            readlinkat(dfd, fd.to_string().as_bytes(), 256),
            Ok(f.as_bytes().to_vec())
        );
        close(dfd).unwrap();

        // task: this thread, with a writable comm.
        let tdir = open("/proc/self/task", O_RDONLY | O_DIRECTORY).unwrap();
        let tasks = names(&getdents64(tdir, 65536).unwrap());
        assert!(tasks.iter().any(|(n, _)| *n == tid.to_string()));
        close(tdir).unwrap();
        let comm = format!("/proc/self/task/{tid}/comm");
        let c = open(&comm, O_WRONLY).unwrap();
        write_all(c, b"lx-test-thread-name-long\n");
        close(c).unwrap();
        assert_eq!(read_path(&comm).unwrap(), b"lx-test-thread-\n");
        assert_eq!(
            read_path("/proc/thread-self/comm").unwrap(),
            b"lx-test-thread-\n"
        );

        // maps: an lx mapping with its name.
        use crate::linux::mman::*;
        let a = crate::mem::mmap(
            0,
            2 * PAGE_SIZE,
            PROT_READ,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
        .unwrap();
        let name = c"lx-test";
        // SAFETY: a NUL-terminated name.
        unsafe {
            crate::process::prctl(
                0x5356_4d41,
                0,
                a as u64,
                PAGE_SIZE as u64,
                name.as_ptr() as u64,
            )
        }
        .unwrap();
        let maps = String::from_utf8(read_path("/proc/self/maps").unwrap()).unwrap();
        let first = format!("{a:08x}-{:08x} r--p 00000000 00:00 0", a + PAGE_SIZE);
        let line = maps
            .lines()
            .find(|l| l.starts_with(&first))
            .expect("mapping listed");
        assert!(line.ends_with("[anon:lx-test]"), "{line}");
        let second = format!("{:08x}-{:08x} r--p", a + PAGE_SIZE, a + 2 * PAGE_SIZE);
        assert!(maps.lines().any(|l| l.starts_with(&second)));
        crate::mem::munmap(a, 2 * PAGE_SIZE).unwrap();
        // A file mapping is named after its file.
        let m = crate::mem::mmap(0, PAGE_SIZE, PROT_READ, MAP_PRIVATE, fd, 0).unwrap();
        let maps = String::from_utf8(read_path("/proc/self/maps").unwrap()).unwrap();
        assert!(
            maps.lines()
                .any(|l| l.starts_with(&format!("{m:08x}-")) && l.ends_with(&f))
        );
        crate::mem::munmap(m, PAGE_SIZE).unwrap();
        close(fd).unwrap();

        // exe and cmdline from the program the host recorded.
        let exe_rel = format!("{d}/prog");
        close(open(&exe_rel, O_CREAT | O_WRONLY).unwrap()).unwrap();
        let root = std::env::temp_dir().join(format!("andruna-lx-test-{}", std::process::id()));
        crate::process::set_program(
            &root.join("procfs").join("prog"),
            &["prog".into(), "-x".into()],
        );
        assert_eq!(
            readlinkat(AT_FDCWD, b"/proc/self/exe", 256),
            Ok(exe_rel.as_bytes().to_vec())
        );
        assert_eq!(read_path("/proc/self/cmdline").unwrap(), b"prog\0-x\0");

        // The property area, as bionic opens and maps it.
        let st = fstatat(AT_FDCWD, b"/dev/__properties__", 0).unwrap();
        assert_eq!((st.uid, st.gid, st.mode), (0, 0, S_IFREG | 0o444));
        let pfd = open("/dev/__properties__", O_RDONLY | O_NOFOLLOW | O_CLOEXEC).unwrap();
        assert_eq!(fstat(pfd).unwrap().size, st.size);
        let a = crate::mem::mmap(0, st.size as usize, PROT_READ, MAP_SHARED, pfd, 0).unwrap();
        // SAFETY: a readable mapping of the area; the magic is at offset 8.
        let magic = unsafe { *((a + 8) as *const u32) };
        assert_eq!(magic, 0x504f_5250);
        crate::mem::munmap(a, st.size as usize).unwrap();
        close(pfd).unwrap();
        assert_eq!(open("/dev/__properties__", O_RDWR), Err(Errno::EACCES));
    }
}

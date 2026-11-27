//! The file descriptor table.
//!
//! lx owns the process's descriptor table, independent of the CRT. A
//! descriptor refers to an *open file description* ([`OpenFile`]), shared by
//! duplicates as on Linux: duplicates share the file offset and the status
//! flags, while `FD_CLOEXEC` belongs to each descriptor.
//!
//! Each description holds a typed object implementing [`FileOps`]. This
//! round has regular files and directories ([`crate::fs`]); pipes, sockets,
//! eventfd, timerfd, epoll, memfd/ashmem, binder and inotify follow
//! (`docs/spec/03-components.md`).

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use crate::errno::{Errno, SysResult};
use crate::linux::fcntl::*;
use crate::linux::stat::StatData;
use crate::util::{self, UserBuf};

/// Highest number of descriptors (`RLIMIT_NOFILE`; Android raises the soft
/// limit to 32768 for apps).
pub const NOFILE_MAX: usize = 32768;

/// The soft `RLIMIT_NOFILE`: new descriptors are below it.
static NOFILE_LIMIT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(NOFILE_MAX);

/// Sets the soft `RLIMIT_NOFILE` ([`crate::rlimit`]).
pub fn set_nofile_limit(n: usize) {
    NOFILE_LIMIT.store(n.min(NOFILE_MAX), Ordering::Relaxed);
}

/// Operations of one kind of open file. Defaults give the Linux error for
/// objects that do not support the operation.
///
/// `Any` lets the calls specific to one kind of object (sockets, eventfd,
/// timerfd, epoll) find it behind a descriptor.
pub trait FileOps: core::any::Any + Send + Sync + fmt::Debug {
    /// `read` at the description's offset.
    fn read(&self, _file: &OpenFile, _buf: UserBuf) -> SysResult<usize> {
        Err(Errno::EINVAL)
    }

    /// `write` at the description's offset (or at the end with `O_APPEND`).
    fn write(&self, _file: &OpenFile, _buf: UserBuf) -> SysResult<usize> {
        Err(Errno::EINVAL)
    }

    fn pread(&self, _buf: UserBuf, _offset: u64) -> SysResult<usize> {
        Err(Errno::ESPIPE)
    }

    fn pwrite(&self, _file: &OpenFile, _buf: UserBuf, _offset: u64) -> SysResult<usize> {
        Err(Errno::ESPIPE)
    }

    fn seek(&self, _offset: i64, _whence: i32) -> SysResult<u64> {
        Err(Errno::ESPIPE)
    }

    fn stat(&self) -> SysResult<StatData>;

    /// The normalized Linux path of a directory, for `*at` calls relative
    /// to it; `None` for objects that are not directories.
    fn dir_path(&self) -> Option<&[u8]> {
        None
    }

    /// `ftruncate` (the description is writable).
    fn truncate(&self, _len: u64) -> SysResult<()> {
        Err(Errno::EINVAL)
    }

    /// `fsync` / `fdatasync`.
    fn sync(&self) -> SysResult<()> {
        Err(Errno::EINVAL)
    }

    /// `getdents64`: appends whole `linux_dirent64` records to `out`, at
    /// most `max` bytes in all.
    fn getdents(&self, _out: &mut Vec<u8>, _max: usize) -> SysResult<()> {
        Err(Errno::ENOTDIR)
    }

    /// The Linux path the object was opened by, for `/proc/self/fd`.
    fn path(&self) -> Option<Vec<u8>> {
        None
    }

    /// A short kind name for objects without a path (`anon_inode:[...]`).
    fn kind_name(&self) -> &'static str {
        "file"
    }

    /// Whether `mmap` can map the object (with `pread` for the contents);
    /// others give `ENODEV`.
    fn mappable(&self) -> bool {
        false
    }

    /// The `poll` events (`crate::linux::poll`) the object is ready for
    /// now. Files and directories are always readable and writable, as on
    /// Linux. Objects whose readiness changes call
    /// [`crate::waitq::notify`] when it does.
    fn poll(&self) -> u32 {
        use crate::linux::poll::*;
        POLLIN | POLLOUT | POLLRDNORM | POLLWRNORM
    }

    /// `ioctl`. `arg` is the raw argument (often a pointer into the
    /// caller's memory). Objects without control requests give `ENOTTY`.
    fn ioctl(&self, _file: &OpenFile, _cmd: u32, _arg: usize) -> SysResult<i64> {
        Err(Errno::ENOTTY)
    }

    /// A mapping the object makes itself (a device with its own memory,
    /// such as the binder receive buffer), instead of lx's file mapping.
    /// `None` lets lx map the object as a file ([`Self::mappable`]).
    fn device_mmap(
        &self,
        _file: &OpenFile,
        _addr: usize,
        _len: usize,
        _prot: u32,
        _flags: u32,
        _offset: i64,
    ) -> Option<SysResult<usize>> {
        None
    }
}

/// An open file description.
#[derive(Debug)]
pub struct OpenFile {
    ops: Box<dyn FileOps>,
    /// `O_RDONLY`, `O_WRONLY`, `O_RDWR`, or `O_PATH`.
    access: u32,
    /// Status flags that `F_SETFL` changes ([`SETFL_MASK`]).
    status: AtomicU32,
}

impl OpenFile {
    /// `flags` are the `open` flags; the access mode and status flags are
    /// kept.
    pub fn new(ops: Box<dyn FileOps>, flags: u32) -> Self {
        let access = if flags & O_PATH != 0 {
            O_PATH
        } else {
            flags & O_ACCMODE
        };
        Self {
            ops,
            access,
            status: AtomicU32::new(flags & SETFL_MASK),
        }
    }

    pub fn ops(&self) -> &dyn FileOps {
        &*self.ops
    }

    /// The object as its concrete type, if it is a `T`.
    pub fn downcast<T: FileOps>(&self) -> Option<&T> {
        let any: &dyn core::any::Any = &*self.ops;
        any.downcast_ref()
    }

    pub fn status_flags(&self) -> u32 {
        self.status.load(Ordering::Relaxed)
    }

    /// `F_GETFL`: access mode and status flags. 64-bit Linux adds
    /// `O_LARGEFILE` to every open file, and so does lx.
    pub fn get_fl(&self) -> u32 {
        self.access | self.status_flags() | O_LARGEFILE
    }

    /// `F_SETFL`: only the flags in [`SETFL_MASK`] change.
    pub fn set_fl(&self, flags: u32) {
        self.status.store(flags & SETFL_MASK, Ordering::Relaxed);
    }

    fn readable(&self) -> SysResult<()> {
        match self.access {
            O_RDONLY | O_RDWR => Ok(()),
            _ => Err(Errno::EBADF),
        }
    }

    fn writable(&self) -> SysResult<()> {
        match self.access {
            O_WRONLY | O_RDWR => Ok(()),
            _ => Err(Errno::EBADF),
        }
    }

    fn not_path(&self) -> SysResult<()> {
        if self.access == O_PATH {
            Err(Errno::EBADF)
        } else {
            Ok(())
        }
    }

    pub fn read(&self, buf: UserBuf) -> SysResult<usize> {
        self.readable()?;
        self.ops.read(self, buf)
    }

    pub fn write(&self, buf: UserBuf) -> SysResult<usize> {
        self.writable()?;
        self.ops.write(self, buf)
    }

    pub fn pread(&self, buf: UserBuf, offset: i64) -> SysResult<usize> {
        self.readable()?;
        let offset = u64::try_from(offset).map_err(|_| Errno::EINVAL)?;
        self.ops.pread(buf, offset)
    }

    pub fn pwrite(&self, buf: UserBuf, offset: i64) -> SysResult<usize> {
        self.writable()?;
        let offset = u64::try_from(offset).map_err(|_| Errno::EINVAL)?;
        self.ops.pwrite(self, buf, offset)
    }

    pub fn seek(&self, offset: i64, whence: i32) -> SysResult<u64> {
        self.not_path()?;
        self.ops.seek(offset, whence)
    }

    pub fn stat(&self) -> SysResult<StatData> {
        self.ops.stat()
    }

    /// `ftruncate`: Linux reports `EINVAL` for a description not open for
    /// writing.
    pub fn truncate(&self, len: i64) -> SysResult<()> {
        self.not_path()?;
        self.writable().map_err(|_| Errno::EINVAL)?;
        let len = u64::try_from(len).map_err(|_| Errno::EINVAL)?;
        self.ops.truncate(len)
    }

    pub fn sync(&self) -> SysResult<()> {
        self.not_path()?;
        self.ops.sync()
    }

    pub fn getdents(&self, out: &mut Vec<u8>, max: usize) -> SysResult<()> {
        self.not_path()?;
        self.ops.getdents(out, max)
    }

    /// The access mode (`O_RDONLY`, `O_WRONLY`, `O_RDWR` or `O_PATH`).
    pub fn access(&self) -> u32 {
        self.access
    }
}

#[derive(Clone, Debug)]
struct Entry {
    file: Arc<OpenFile>,
    cloexec: bool,
}

/// A descriptor table.
#[derive(Debug, Default)]
pub struct FdTable {
    slots: RwLock<Vec<Option<Entry>>>,
}

/// The process's descriptor table.
pub static FDS: FdTable = FdTable::new();

fn index(fd: i32) -> SysResult<usize> {
    usize::try_from(fd)
        .ok()
        .filter(|&i| i < NOFILE_MAX)
        .ok_or(Errno::EBADF)
}

impl FdTable {
    /// The open descriptors, in order.
    pub fn open_fds(&self) -> Vec<i32> {
        util::read(&self.slots)
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_some())
            .map(|(i, _)| i as i32)
            .collect()
    }

    pub const fn new() -> Self {
        Self {
            slots: RwLock::new(Vec::new()),
        }
    }

    /// The description `fd` refers to.
    pub fn get(&self, fd: i32) -> SysResult<Arc<OpenFile>> {
        let i = index(fd)?;
        util::read(&self.slots)
            .get(i)
            .and_then(|e| e.as_ref())
            .map(|e| e.file.clone())
            .ok_or(Errno::EBADF)
    }

    /// Installs `file` at the lowest free descriptor not below `min`.
    pub fn insert(&self, file: Arc<OpenFile>, cloexec: bool, min: usize) -> SysResult<i32> {
        let mut slots = util::write(&self.slots);
        let i = (min..NOFILE_LIMIT.load(Ordering::Relaxed))
            .find(|&i| slots.get(i).is_none_or(Option::is_none))
            .ok_or(Errno::EMFILE)?;
        if i >= slots.len() {
            slots.resize(i + 1, None);
        }
        slots[i] = Some(Entry { file, cloexec });
        Ok(i as i32)
    }

    /// `close`.
    pub fn close(&self, fd: i32) -> SysResult<()> {
        let i = index(fd)?;
        let removed = util::write(&self.slots)
            .get_mut(i)
            .and_then(Option::take)
            .ok_or(Errno::EBADF)?;
        // The description (and its Windows handle, when this was the last
        // reference) is dropped here, outside the lock.
        drop(removed);
        Ok(())
    }

    /// `dup`, and `fcntl(F_DUPFD / F_DUPFD_CLOEXEC)` with `min`.
    pub fn dup_min(&self, fd: i32, min: i32, cloexec: bool) -> SysResult<i32> {
        let min = usize::try_from(min)
            .ok()
            .filter(|&m| m < NOFILE_MAX)
            .ok_or(Errno::EINVAL)?;
        let file = self.get(fd)?;
        self.insert(file, cloexec, min)
    }

    /// `dup3` (and `dup2` when `old != new`): makes `new` refer to the
    /// description of `old`, closing what `new` referred to.
    pub fn dup3(&self, old: i32, new: i32, flags: u32) -> SysResult<i32> {
        if flags & !O_CLOEXEC != 0 || old == new {
            return Err(Errno::EINVAL);
        }
        let n = index(new)?;
        let replaced;
        {
            let mut slots = util::write(&self.slots);
            let file = slots
                .get(index(old)?)
                .and_then(|e| e.as_ref())
                .map(|e| e.file.clone())
                .ok_or(Errno::EBADF)?;
            if n >= slots.len() {
                slots.resize(n + 1, None);
            }
            replaced = slots[n].replace(Entry {
                file,
                cloexec: flags & O_CLOEXEC != 0,
            });
        }
        drop(replaced);
        Ok(new)
    }

    /// `dup2`.
    pub fn dup2(&self, old: i32, new: i32) -> SysResult<i32> {
        if old == new {
            // Linux returns `new` after checking that `old` is open.
            self.get(old)?;
            return Ok(new);
        }
        self.dup3(old, new, 0)
    }

    /// `FD_CLOEXEC` of `fd`.
    pub fn cloexec(&self, fd: i32) -> SysResult<bool> {
        let i = index(fd)?;
        util::read(&self.slots)
            .get(i)
            .and_then(|e| e.as_ref())
            .map(|e| e.cloexec)
            .ok_or(Errno::EBADF)
    }

    pub fn set_cloexec(&self, fd: i32, on: bool) -> SysResult<()> {
        let i = index(fd)?;
        let mut slots = util::write(&self.slots);
        let e = slots
            .get_mut(i)
            .and_then(|e| e.as_mut())
            .ok_or(Errno::EBADF)?;
        e.cloexec = on;
        Ok(())
    }

    /// `fcntl` for the commands that concern the table and the
    /// description's flags.
    pub fn fcntl(&self, fd: i32, cmd: i32, arg: u64) -> SysResult<i32> {
        match cmd {
            F_DUPFD => self.dup_min(fd, arg as i32, false),
            F_DUPFD_CLOEXEC => self.dup_min(fd, arg as i32, true),
            F_GETFD => Ok(if self.cloexec(fd)? { FD_CLOEXEC } else { 0 }),
            F_SETFD => {
                self.set_cloexec(fd, arg as i32 & FD_CLOEXEC != 0)?;
                Ok(0)
            }
            F_GETFL => Ok(self.get(fd)?.get_fl() as i32),
            F_SETFL => {
                let f = self.get(fd)?;
                if f.access == O_PATH {
                    return Err(Errno::EBADF);
                }
                f.set_fl(arg as u32);
                Ok(0)
            }
            _ => {
                // Validate the descriptor first, as Linux does.
                self.get(fd)?;
                Err(Errno::EINVAL)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Dummy;
    impl FileOps for Dummy {
        fn stat(&self) -> SysResult<StatData> {
            Ok(StatData::default())
        }
    }

    fn file(flags: u32) -> Arc<OpenFile> {
        Arc::new(OpenFile::new(Box::new(Dummy), flags))
    }

    #[test]
    fn lowest_free_descriptor() {
        let t = FdTable::new();
        assert_eq!(t.insert(file(0), false, 0), Ok(0));
        assert_eq!(t.insert(file(0), false, 0), Ok(1));
        assert_eq!(t.insert(file(0), false, 0), Ok(2));
        t.close(1).unwrap();
        assert_eq!(t.insert(file(0), false, 0), Ok(1));
        assert_eq!(t.insert(file(0), false, 10), Ok(10));
        assert_eq!(t.insert(file(0), false, 0), Ok(3));
    }

    #[test]
    fn close_errors() {
        let t = FdTable::new();
        assert_eq!(t.close(-1), Err(Errno::EBADF));
        assert_eq!(t.close(0), Err(Errno::EBADF));
        assert_eq!(t.close(NOFILE_MAX as i32), Err(Errno::EBADF));
        let fd = t.insert(file(0), false, 0).unwrap();
        t.close(fd).unwrap();
        assert_eq!(t.close(fd), Err(Errno::EBADF));
    }

    #[test]
    fn dup_shares_description_not_cloexec() {
        let t = FdTable::new();
        let a = t.insert(file(O_RDWR), true, 0).unwrap();
        let b = t.dup_min(a, 0, false).unwrap();
        assert_eq!(b, a + 1);
        assert!(Arc::ptr_eq(&t.get(a).unwrap(), &t.get(b).unwrap()));
        assert_eq!(t.fcntl(a, F_GETFD, 0), Ok(FD_CLOEXEC));
        assert_eq!(t.fcntl(b, F_GETFD, 0), Ok(0));
        // Status flags are shared.
        t.fcntl(a, F_SETFL, u64::from(O_APPEND | O_NONBLOCK))
            .unwrap();
        let fl = t.fcntl(b, F_GETFL, 0).unwrap() as u32;
        assert_eq!(fl & O_ACCMODE, O_RDWR);
        assert_eq!(fl & (O_APPEND | O_NONBLOCK), O_APPEND | O_NONBLOCK);
        assert_ne!(fl & O_LARGEFILE, 0);
        // Access mode cannot be changed with F_SETFL.
        t.fcntl(a, F_SETFL, u64::from(O_WRONLY)).unwrap();
        assert_eq!(t.fcntl(a, F_GETFL, 0).unwrap() as u32 & O_ACCMODE, O_RDWR);
    }

    #[test]
    fn dup2_and_dup3() {
        let t = FdTable::new();
        let a = t.insert(file(O_RDONLY), false, 0).unwrap();
        let b = t.insert(file(O_WRONLY), false, 0).unwrap();
        assert_eq!(t.dup2(a, a), Ok(a));
        assert_eq!(t.dup2(7, 7), Err(Errno::EBADF));
        assert_eq!(t.dup3(a, a, 0), Err(Errno::EINVAL));
        assert_eq!(t.dup3(a, b, 0x1), Err(Errno::EINVAL));
        // Replaces b.
        assert_eq!(t.dup3(a, b, O_CLOEXEC), Ok(b));
        assert!(Arc::ptr_eq(&t.get(a).unwrap(), &t.get(b).unwrap()));
        assert_eq!(t.cloexec(b), Ok(true));
        // Far target.
        assert_eq!(t.dup2(a, 100), Ok(100));
        assert_eq!(t.dup2(a, NOFILE_MAX as i32), Err(Errno::EBADF));
        assert_eq!(t.dup2(55, 3), Err(Errno::EBADF));
    }

    #[test]
    fn fcntl_errors_and_dupfd() {
        let t = FdTable::new();
        let a = t.insert(file(0), false, 0).unwrap();
        assert_eq!(t.fcntl(a, F_DUPFD_CLOEXEC, 5), Ok(5));
        assert_eq!(t.cloexec(5), Ok(true));
        assert_eq!(t.fcntl(a, F_DUPFD, NOFILE_MAX as u64), Err(Errno::EINVAL));
        assert_eq!(t.fcntl(a, 9999, 0), Err(Errno::EINVAL));
        assert_eq!(t.fcntl(42, 9999, 0), Err(Errno::EBADF));
        assert_eq!(t.fcntl(42, F_GETFD, 0), Err(Errno::EBADF));
        t.fcntl(a, F_SETFD, FD_CLOEXEC as u64).unwrap();
        assert_eq!(t.cloexec(a), Ok(true));
    }

    #[test]
    fn access_mode_is_enforced() {
        let mut buf = [0u8; 4];
        let r = file(O_WRONLY);
        assert_eq!(r.read(UserBuf::from_mut(&mut buf)), Err(Errno::EBADF));
        let w = file(O_RDONLY);
        assert_eq!(w.write(UserBuf::from_ref(&buf)), Err(Errno::EBADF));
        let p = file(O_PATH | O_RDWR);
        assert_eq!(p.read(UserBuf::from_mut(&mut buf)), Err(Errno::EBADF));
        assert_eq!(p.seek(0, SEEK_SET), Err(Errno::EBADF));
        let rw = file(O_RDWR);
        assert_eq!(
            rw.pread(UserBuf::from_mut(&mut buf), -1),
            Err(Errno::EINVAL)
        );
        assert_eq!(rw.pread(UserBuf::from_mut(&mut buf), 0), Err(Errno::ESPIPE));
    }

    #[test]
    fn concurrent_insert_and_close() {
        let t = Arc::new(FdTable::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let t = t.clone();
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        let fd = t.insert(file(0), false, 0).unwrap();
                        t.get(fd).unwrap();
                        t.close(fd).unwrap();
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().unwrap();
        }
        assert!(util::read(&t.slots).iter().all(Option::is_none));
    }
}

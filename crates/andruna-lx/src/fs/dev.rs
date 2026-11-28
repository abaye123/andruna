//! The memory character devices lx serves itself: `/dev/null`,
//! `/dev/zero`, `/dev/full`, `/dev/random` and `/dev/urandom`, and the
//! links `/dev/stdin`, `/dev/stdout`, `/dev/stderr` and `/dev/fd`.
//!
//! They exist in every process whatever the mount table holds at `/dev`,
//! with the device numbers of Linux (major 1). `/dev/random` and
//! `/dev/urandom` read from the Windows system random number generator,
//! which never blocks once Windows has started, as `/dev/random` on Linux
//! 5.6 and later.
//!
//! # Deviations from Linux
//!
//! - Writing to `/dev/random` or `/dev/urandom` is accepted and discarded:
//!   Windows has no interface to add entropy (Linux mixes the data in
//!   without crediting it, so readers see no difference).
//! - A shared mapping of `/dev/zero` is a private one (lx maps files with
//!   `pread`); it is shared with no other process either way.

use crate::errno::{Errno, SysResult};
use crate::fd::{FileOps, OpenFile};
use crate::linux::stat::{S_IFCHR, StatData};
use crate::util::UserBuf;

use super::path::LinuxPath;

/// Opens a device registered by the host process: the object for the
/// `open` flags given.
pub type DeviceOpen = fn(flags: u32) -> SysResult<Box<dyn FileOps>>;

/// A device registered by the host process: its name, opener and the
/// status `stat` reports for its path.
type Registered = (&'static [u8], DeviceOpen, StatData);

/// Devices the host process provides from outside lx (for example
/// `/dev/binder` from andruna-binder), by name below `/dev`.
static REGISTERED: std::sync::RwLock<Vec<Registered>> = std::sync::RwLock::new(Vec::new());

/// Registers the device `/dev/<name>`, served by `open`. A later
/// registration of the same name replaces the earlier one. `stat` and
/// `access` on the path see a character device, mode 0666, owned by root,
/// with no device number; [`register_with_stat`] gives the status.
pub fn register(name: &'static [u8], open: DeviceOpen) {
    register_with_stat(
        name,
        open,
        StatData {
            dev: 5,
            mode: S_IFCHR | 0o666,
            nlink: 1,
            blksize: 4096,
            ..StatData::default()
        },
    );
}

/// [`register`], with the status `stat` reports for the path (the same
/// the open device reports from `fstat`).
pub fn register_with_stat(name: &'static [u8], open: DeviceOpen, stat: StatData) {
    let mut r = crate::util::write(&REGISTERED);
    r.retain(|(n, _, _)| *n != name);
    r.push((name, open, stat));
}

fn find_registered(p: &LinuxPath) -> Option<Registered> {
    if p.components.len() != 2 || p.components[0] != b"dev" {
        return None;
    }
    crate::util::read(&REGISTERED)
        .iter()
        .find(|(n, _, _)| *n == p.components[1].as_slice())
        .cloned()
}

/// The opener of the registered device at `p`, if `p` names one.
pub fn registered(p: &LinuxPath) -> Option<DeviceOpen> {
    find_registered(p).map(|(_, o, _)| o)
}

/// The status of the registered device at `p`, if `p` names one.
pub fn registered_stat(p: &LinuxPath) -> Option<StatData> {
    find_registered(p).map(|(_, _, s)| s)
}

/// A memory device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Null,
    Zero,
    Full,
    Random,
    Urandom,
}

impl Device {
    /// The device at `p`, if `p` names one.
    pub fn at(p: &LinuxPath) -> Option<Self> {
        if p.components.len() != 2 || p.components[0] != b"dev" {
            return None;
        }
        Some(match p.components[1].as_slice() {
            b"null" => Self::Null,
            b"zero" => Self::Zero,
            b"full" => Self::Full,
            b"random" => Self::Random,
            b"urandom" => Self::Urandom,
            _ => return None,
        })
    }

    /// The target of the standard links in `/dev` (`stdin`, `stdout`,
    /// `stderr`, `fd`), if `p` names one.
    pub fn link_at(p: &LinuxPath) -> Option<&'static [u8]> {
        if p.components.len() != 2 || p.components[0] != b"dev" {
            return None;
        }
        Some(match p.components[1].as_slice() {
            b"stdin" => b"/proc/self/fd/0",
            b"stdout" => b"/proc/self/fd/1",
            b"stderr" => b"/proc/self/fd/2",
            b"fd" => b"/proc/self/fd",
            _ => return None,
        })
    }

    fn minor(self) -> u64 {
        match self {
            Self::Null => 3,
            Self::Zero => 5,
            Self::Full => 7,
            Self::Random => 8,
            Self::Urandom => 9,
        }
    }

    pub fn stat(self) -> StatData {
        StatData {
            // devtmpfs.
            dev: 5,
            ino: 0x3000 + self.minor(),
            mode: S_IFCHR | 0o666,
            nlink: 1,
            uid: 0,
            gid: 0,
            // makedev(1, minor).
            rdev: (1 << 8) | self.minor(),
            size: 0,
            blksize: 4096,
            ..StatData::default()
        }
    }
}

/// An open memory device.
#[derive(Debug)]
pub struct DevFile(pub Device);

fn fill(buf: UserBuf, random: bool) -> SysResult<usize> {
    let n = buf.len().min(crate::linux::fcntl::MAX_RW_COUNT);
    if n == 0 {
        return Ok(0);
    }
    if buf.ptr().is_null() {
        return Err(Errno::EFAULT);
    }
    // SAFETY: the caller's buffer of at least `n` bytes (UserBuf
    // contract); nothing else in lx refers to it during the call.
    let b = unsafe { core::slice::from_raw_parts_mut(buf.ptr(), n) };
    if random {
        crate::sys::random::fill(b)?;
    } else {
        b.fill(0);
    }
    Ok(n)
}

impl FileOps for DevFile {
    fn read(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        self.pread(buf, 0)
    }

    fn pread(&self, buf: UserBuf, _offset: u64) -> SysResult<usize> {
        match self.0 {
            Device::Null => Ok(0),
            Device::Zero | Device::Full => fill(buf, false),
            Device::Random | Device::Urandom => fill(buf, true),
        }
    }

    fn write(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        self.pwrite(file, buf, 0)
    }

    fn pwrite(&self, _file: &OpenFile, buf: UserBuf, _offset: u64) -> SysResult<usize> {
        match self.0 {
            Device::Full => Err(Errno::ENOSPC),
            _ => Ok(buf.len()),
        }
    }

    /// Seeking a memory device succeeds and keeps the position at 0.
    fn seek(&self, _offset: i64, _whence: i32) -> SysResult<u64> {
        Ok(0)
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(self.0.stat())
    }

    fn truncate(&self, _len: u64) -> SysResult<()> {
        Err(Errno::EINVAL)
    }

    fn sync(&self) -> SysResult<()> {
        Err(Errno::EINVAL)
    }

    fn path(&self) -> Option<Vec<u8>> {
        let name: &[u8] = match self.0 {
            Device::Null => b"null",
            Device::Zero => b"zero",
            Device::Full => b"full",
            Device::Random => b"random",
            Device::Urandom => b"urandom",
        };
        Some([b"/dev/".as_slice(), name].concat())
    }

    fn mappable(&self) -> bool {
        self.0 == Device::Zero
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::FDS;
    use crate::fs;
    use crate::linux::fcntl::*;

    fn open(p: &[u8], flags: u32) -> i32 {
        fs::openat(AT_FDCWD, p, flags, 0o666).unwrap()
    }

    #[test]
    fn null_zero_full_random() {
        let mut b = [7u8; 64];
        let fd = open(b"/dev/null", O_RDWR | O_CREAT | O_TRUNC);
        assert_eq!(fs::read(fd, UserBuf::from_mut(&mut b)), Ok(0));
        assert_eq!(fs::write(fd, UserBuf::from_ref(&b)), Ok(64));
        assert_eq!(fs::lseek(fd, 10, SEEK_SET), Ok(0));
        let st = fs::fstat(fd).unwrap();
        assert_eq!(st.mode, S_IFCHR | 0o666);
        assert_eq!(st.rdev, 0x103);
        fs::close(fd).unwrap();

        let fd = open(b"/dev/zero", O_RDONLY);
        assert_eq!(fs::read(fd, UserBuf::from_mut(&mut b)), Ok(64));
        assert!(b.iter().all(|&x| x == 0));
        fs::close(fd).unwrap();

        let fd = open(b"/dev/full", O_WRONLY);
        assert_eq!(fs::write(fd, UserBuf::from_ref(&b)), Err(Errno::ENOSPC));
        fs::close(fd).unwrap();

        let fd = open(b"/dev/urandom", O_RDONLY | O_CLOEXEC);
        assert_eq!(fs::read(fd, UserBuf::from_mut(&mut b)), Ok(64));
        assert!(b.iter().any(|&x| x != 0));
        assert_eq!(FDS.cloexec(fd), Ok(true));
        fs::close(fd).unwrap();

        let st = fs::fstatat(AT_FDCWD, b"/dev/random", 0).unwrap();
        assert_eq!(st.rdev, 0x108);
        assert_eq!(
            fs::openat(AT_FDCWD, b"/dev/null", O_RDONLY | O_DIRECTORY, 0),
            Err(Errno::ENOTDIR)
        );
        assert_eq!(
            fs::openat(AT_FDCWD, b"/dev/null", O_RDWR | O_CREAT | O_EXCL, 0),
            Err(Errno::EEXIST)
        );
    }

    #[derive(Debug)]
    struct TestDevice;
    impl FileOps for TestDevice {
        fn stat(&self) -> SysResult<StatData> {
            Ok(StatData::default())
        }
        fn ioctl(&self, _file: &OpenFile, cmd: u32, arg: usize) -> SysResult<i64> {
            match cmd {
                1 => Ok(arg as i64 + 1),
                _ => Err(Errno::EINVAL),
            }
        }
        fn device_mmap(
            &self,
            _file: &OpenFile,
            _addr: usize,
            len: usize,
            _prot: u32,
            _flags: u32,
            _offset: i64,
        ) -> Option<SysResult<usize>> {
            Some(if len == 4096 {
                Ok(0x1234_0000)
            } else {
                Err(Errno::ENOMEM)
            })
        }
    }

    fn open_test_device(_flags: u32) -> SysResult<Box<dyn FileOps>> {
        Ok(Box::new(TestDevice))
    }

    #[test]
    fn registered_device_ioctl_and_mmap() {
        use crate::linux::mman::*;
        register(b"lx-test-device", open_test_device);
        let fd = open(b"/dev/lx-test-device", O_RDWR | O_CLOEXEC);
        assert_eq!(FDS.cloexec(fd), Ok(true));
        let f = FDS.get(fd).unwrap();
        assert_eq!(f.ops().ioctl(&f, 1, 41), Ok(42));
        assert_eq!(f.ops().ioctl(&f, 2, 0), Err(Errno::EINVAL));
        assert_eq!(
            crate::mem::mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd, 0),
            Ok(0x1234_0000)
        );
        assert_eq!(
            crate::mem::mmap(0, 8192, PROT_READ, MAP_PRIVATE, fd, 0),
            Err(Errno::ENOMEM)
        );
        fs::close(fd).unwrap();
        // Other objects have no control requests.
        let fd = open(b"/dev/null", O_RDONLY);
        let f = FDS.get(fd).unwrap();
        assert_eq!(f.ops().ioctl(&f, 1, 0), Err(Errno::ENOTTY));
        fs::close(fd).unwrap();
    }
}

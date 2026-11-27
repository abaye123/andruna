//! Per-thread `errno` with Linux numbering, and the mapping from Windows
//! error codes.
//!
//! Every lx export that fails stores a Linux error number here and returns
//! the interface's failure value. Callers read it through bionic's
//! `__errno()`, exported by [`crate::ffi::errno`].

use core::cell::Cell;
use core::fmt;

use windows::Win32::Foundation as wf;

use crate::linux::errno::*;

/// A Linux error number.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Errno(pub i32);

/// Result of an lx operation: the value or the Linux error.
pub type SysResult<T> = Result<T, Errno>;

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Errno({})", self.0)
    }
}

impl Errno {
    pub const EPERM: Self = Self(EPERM);
    pub const ERANGE: Self = Self(ERANGE);
    pub const ENOENT: Self = Self(ENOENT);
    pub const EINTR: Self = Self(EINTR);
    pub const EIO: Self = Self(EIO);
    pub const ENXIO: Self = Self(ENXIO);
    pub const EBADF: Self = Self(EBADF);
    pub const EAGAIN: Self = Self(EAGAIN);
    pub const ENOMEM: Self = Self(ENOMEM);
    pub const EACCES: Self = Self(EACCES);
    pub const EFAULT: Self = Self(EFAULT);
    pub const EBUSY: Self = Self(EBUSY);
    pub const EEXIST: Self = Self(EEXIST);
    pub const ENOTDIR: Self = Self(ENOTDIR);
    pub const EISDIR: Self = Self(EISDIR);
    pub const EINVAL: Self = Self(EINVAL);
    pub const EMFILE: Self = Self(EMFILE);
    pub const EFBIG: Self = Self(EFBIG);
    pub const ENOSPC: Self = Self(ENOSPC);
    pub const ESPIPE: Self = Self(ESPIPE);
    pub const EROFS: Self = Self(EROFS);
    pub const EPIPE: Self = Self(EPIPE);
    pub const ENAMETOOLONG: Self = Self(ENAMETOOLONG);
    pub const ENOSYS: Self = Self(ENOSYS);
    pub const ENOTEMPTY: Self = Self(ENOTEMPTY);
    pub const ELOOP: Self = Self(ELOOP);
    pub const EOVERFLOW: Self = Self(EOVERFLOW);
    pub const EOPNOTSUPP: Self = Self(EOPNOTSUPP);
    pub const ETIMEDOUT: Self = Self(ETIMEDOUT);

    /// Maps a Win32 error code (`GetLastError`) to the closest Linux error.
    ///
    /// Callers that know more about the operation (for example that a
    /// directory was opened for writing) refine the result themselves.
    pub fn from_win32(code: u32) -> Self {
        let e = match wf::WIN32_ERROR(code) {
            wf::ERROR_FILE_NOT_FOUND
            | wf::ERROR_PATH_NOT_FOUND
            | wf::ERROR_INVALID_DRIVE
            | wf::ERROR_BAD_NETPATH
            | wf::ERROR_BAD_NET_NAME
            | wf::ERROR_INVALID_NAME
            | wf::ERROR_BAD_PATHNAME
            | wf::ERROR_DELETE_PENDING => ENOENT,
            wf::ERROR_ACCESS_DENIED
            | wf::ERROR_CANNOT_MAKE
            | wf::ERROR_PRIVILEGE_NOT_HELD
            | wf::ERROR_NETWORK_ACCESS_DENIED => EACCES,
            wf::ERROR_INVALID_HANDLE => EBADF,
            wf::ERROR_NOT_ENOUGH_MEMORY
            | wf::ERROR_OUTOFMEMORY
            | wf::ERROR_COMMITMENT_LIMIT
            | wf::ERROR_NOT_ENOUGH_QUOTA
            | wf::ERROR_INVALID_ADDRESS => ENOMEM,
            wf::ERROR_FILE_EXISTS | wf::ERROR_ALREADY_EXISTS => EEXIST,
            wf::ERROR_DIRECTORY => ENOTDIR,
            wf::ERROR_DIR_NOT_EMPTY => ENOTEMPTY,
            wf::ERROR_DISK_FULL | wf::ERROR_HANDLE_DISK_FULL => ENOSPC,
            wf::ERROR_DISK_QUOTA_EXCEEDED => EDQUOT,
            wf::ERROR_WRITE_PROTECT => EROFS,
            wf::ERROR_SHARING_VIOLATION | wf::ERROR_LOCK_VIOLATION | wf::ERROR_BUSY => EBUSY,
            wf::ERROR_BROKEN_PIPE | wf::ERROR_NO_DATA => EPIPE,
            wf::ERROR_FILENAME_EXCED_RANGE | wf::ERROR_BUFFER_OVERFLOW => ENAMETOOLONG,
            wf::ERROR_NEGATIVE_SEEK | wf::ERROR_INVALID_PARAMETER => EINVAL,
            wf::ERROR_NOT_SAME_DEVICE => EXDEV,
            wf::ERROR_CANT_RESOLVE_FILENAME => ELOOP,
            wf::ERROR_NOACCESS | wf::ERROR_INVALID_USER_BUFFER => EFAULT,
            wf::ERROR_NOT_SUPPORTED | wf::ERROR_INVALID_FUNCTION => EOPNOTSUPP,
            wf::ERROR_TIMEOUT => ETIMEDOUT,
            wf::ERROR_OPERATION_ABORTED => ECANCELED,
            wf::ERROR_FILE_TOO_LARGE => EFBIG,
            wf::ERROR_TOO_MANY_OPEN_FILES => EMFILE,
            wf::ERROR_TOO_MANY_LINKS => EMLINK,
            _ => EIO,
        };
        Self(e)
    }

    /// Maps an `NTSTATUS` failure to the closest Linux error.
    pub fn from_ntstatus(status: wf::NTSTATUS) -> Self {
        match status {
            wf::STATUS_ACCESS_VIOLATION | wf::STATUS_INVALID_USER_BUFFER => Self::EFAULT,
            wf::STATUS_FILE_IS_A_DIRECTORY => Self::EISDIR,
            wf::STATUS_NOT_A_DIRECTORY => Self::ENOTDIR,
            _ => Self::from_win32(crate::sys::nt_status_to_win32(status)),
        }
    }
}

impl From<windows::core::Error> for Errno {
    fn from(e: windows::core::Error) -> Self {
        let hr = e.code().0 as u32;
        // HRESULT_FROM_WIN32: facility 7 with the severity bit set.
        if hr & 0xffff_0000 == 0x8007_0000 {
            Self::from_win32(hr & 0xffff)
        } else if hr & 0x1000_0000 != 0 {
            // HRESULT_FROM_NT: the NTSTATUS with FACILITY_NT_BIT added.
            Self::from_ntstatus(wf::NTSTATUS((hr & !0x1000_0000) as i32))
        } else {
            Self::EIO
        }
    }
}

thread_local! {
    static ERRNO: Cell<i32> = const { Cell::new(0) };
}

/// Sets the calling thread's `errno`.
pub fn set_errno(e: Errno) {
    ERRNO.with(|v| v.set(e.0));
}

/// The calling thread's `errno`.
pub fn errno() -> i32 {
    ERRNO.with(Cell::get)
}

/// Address of the calling thread's `errno`, valid for the thread's lifetime.
pub fn errno_location() -> *mut i32 {
    ERRNO.with(Cell::as_ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_is_per_thread() {
        set_errno(Errno::ENOENT);
        let other = std::thread::spawn(|| {
            assert_eq!(errno(), 0);
            set_errno(Errno::EBADF);
            errno()
        })
        .join()
        .unwrap();
        assert_eq!(other, EBADF);
        assert_eq!(errno(), ENOENT);
    }

    #[test]
    fn location_is_stable_and_writable() {
        let p = errno_location();
        assert_eq!(p, errno_location());
        // SAFETY: `p` points to this thread's errno cell, which lives as long
        // as the thread; nothing else holds a reference to it here.
        unsafe { p.write(ETIMEDOUT) };
        assert_eq!(errno(), 110);
    }

    #[test]
    fn win32_mapping() {
        assert_eq!(Errno::from_win32(2), Errno::ENOENT);
        assert_eq!(Errno::from_win32(5), Errno::EACCES);
        assert_eq!(Errno::from_win32(80), Errno::EEXIST);
        assert_eq!(Errno::from_win32(1460), Errno::ETIMEDOUT);
        assert_eq!(Errno::from_win32(0xdead), Errno::EIO);
    }

    #[test]
    fn hresult_mapping() {
        let e = windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(3));
        assert_eq!(Errno::from(e), Errno::ENOENT);
        let nt = windows::core::Error::from_hresult(wf::STATUS_ACCESS_VIOLATION.to_hresult());
        assert_eq!(Errno::from(nt), Errno::EFAULT);
    }

    #[test]
    fn ntstatus_mapping() {
        assert_eq!(
            Errno::from_ntstatus(wf::STATUS_OBJECT_NAME_NOT_FOUND),
            Errno::ENOENT
        );
        assert_eq!(
            Errno::from_ntstatus(wf::STATUS_INVALID_HANDLE),
            Errno::EBADF
        );
    }
}

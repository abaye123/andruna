//! Descriptors on the host process's standard streams.
//!
//! A host process (the app process, a test runner) gives guest code its
//! own standard input, output and error as descriptors 0, 1 and 2 with
//! [`install_std_streams`]. Each is a stream on the inherited Windows
//! handle: no offset, no seeking (`ESPIPE`), and `fstat` reports a
//! character device for a console, a FIFO for a pipe and a regular file for
//! a redirected file, as Linux would for the same kind of object. The
//! handle stays owned by the Windows process; lx only borrows it.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType, ReadFile, WriteFile,
};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::{O_RDONLY, O_WRONLY};
use crate::linux::stat::{S_IFCHR, S_IFIFO, S_IFREG, StatData};
use crate::util::UserBuf;

/// A stream on a borrowed Windows handle.
#[derive(Debug)]
pub struct HostStream {
    handle: usize,
    mode: u32,
}

impl HostStream {
    /// # Safety
    ///
    /// `handle` stays open for the life of the process.
    pub unsafe fn new(handle: HANDLE) -> Self {
        // SAFETY: querying the type of a valid handle.
        let ty = unsafe { GetFileType(handle) };
        let mode = match ty {
            FILE_TYPE_CHAR => S_IFCHR | 0o620,
            FILE_TYPE_PIPE => S_IFIFO | 0o600,
            FILE_TYPE_DISK => S_IFREG | 0o644,
            _ => S_IFCHR | 0o600,
        };
        Self {
            handle: handle.0 as usize,
            mode,
        }
    }

    fn h(&self) -> HANDLE {
        HANDLE(self.handle as *mut core::ffi::c_void)
    }
}

fn len32(buf: &UserBuf) -> usize {
    buf.len().min(crate::linux::fcntl::MAX_RW_COUNT)
}

impl FileOps for HostStream {
    fn kind_name(&self) -> &'static str {
        "host-stream"
    }

    fn read(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let mut n = 0u32;
        // SAFETY: `buf` is a caller buffer no Rust reference aliases (see
        // `UserBuf::new`), valid for its length; the handle is synchronous.
        let r = unsafe {
            ReadFile(
                self.h(),
                Some(core::slice::from_raw_parts_mut(buf.ptr(), len32(&buf))),
                Some(&mut n),
                None,
            )
        };
        match r {
            Ok(()) => Ok(n as usize),
            // A closed pipe is the end of the stream.
            Err(e) if e.code().0 as u32 == 0x8007_006d => Ok(0),
            Err(e) => Err(Errno::from_win32(e.code().0 as u32 & 0xffff)),
        }
    }

    fn write(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let mut n = 0u32;
        // SAFETY: as in `read`; Windows only reads the buffer.
        let r = unsafe {
            WriteFile(
                self.h(),
                Some(core::slice::from_raw_parts(buf.ptr(), len32(&buf))),
                Some(&mut n),
                None,
            )
        };
        match r {
            Ok(()) => Ok(n as usize),
            // ERROR_BROKEN_PIPE / ERROR_NO_DATA: the reader is gone.
            Err(e) if matches!(e.code().0 as u32, 0x8007_006d | 0x8007_00e8) => Err(Errno::EPIPE),
            Err(e) => Err(Errno::from_win32(e.code().0 as u32 & 0xffff)),
        }
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            mode: self.mode,
            nlink: 1,
            uid: crate::process::uid(),
            gid: crate::process::gid(),
            blksize: 1024,
            ..StatData::default()
        })
    }
}

/// Installs descriptors 0, 1 and 2 on the process's standard handles.
/// A missing handle (a GUI process) leaves its descriptor closed.
/// Fails with `EBUSY` if a descriptor below 3 is already in use.
pub fn install_std_streams() -> SysResult<()> {
    let streams: [(STD_HANDLE, u32); 3] = [
        (STD_INPUT_HANDLE, O_RDONLY),
        (STD_OUTPUT_HANDLE, O_WRONLY),
        (STD_ERROR_HANDLE, O_WRONLY),
    ];
    for (fd, _) in streams.iter().enumerate() {
        if FDS.get(fd as i32).is_ok() {
            return Err(Errno::EBUSY);
        }
    }
    for (want, (which, flags)) in streams.into_iter().enumerate() {
        // SAFETY: reads the process's standard handle.
        let Ok(h) = (unsafe { GetStdHandle(which) }) else {
            continue;
        };
        if h.is_invalid() || h.0.is_null() {
            continue;
        }
        // SAFETY: standard handles stay open for the life of the process.
        let s = unsafe { HostStream::new(h) };
        let file = std::sync::Arc::new(OpenFile::new(Box::new(s), flags));
        let got = FDS.insert(file, false, want)?;
        if got != want as i32 {
            let _ = FDS.close(got);
            return Err(Errno::EBUSY);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_reports_its_kind() {
        // SAFETY: the test process's standard error handle.
        let h = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap();
        // SAFETY: it stays open.
        let s = unsafe { HostStream::new(h) };
        let st = s.stat().unwrap();
        assert!(matches!(st.mode & 0o170000, S_IFCHR | S_IFIFO | S_IFREG));
        let f = OpenFile::new(Box::new(s), O_WRONLY);
        assert_eq!(f.seek(0, 0), Err(Errno::ESPIPE));
        assert_eq!(f.write(UserBuf::from_ref(b"")), Ok(0));
        let mut b = [0u8; 1];
        assert_eq!(f.read(UserBuf::from_mut(&mut b)), Err(Errno::EBADF));
    }
}

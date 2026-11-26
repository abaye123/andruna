//! Files: open, positional read and write, metadata.

use core::ffi::c_void;

use windows::Wdk::Storage::FileSystem::{NtReadFile, NtWriteFile};
use windows::Win32::Foundation::{self as wf, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_BASIC_INFO, FILE_CREATION_DISPOSITION, FILE_END_OF_FILE_INFO, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileAttributeTagInfo, FileBasicInfo, FileEndOfFileInfo, FileIdInfo,
    FileStandardInfo, GetFileAttributesW, GetFileInformationByHandle, GetFileInformationByHandleEx,
    INVALID_FILE_ATTRIBUTES, SetFileInformationByHandle,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;
use windows::core::PCWSTR;

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};
use crate::util::UserBuf;

/// `IO_REPARSE_TAG_SYMLINK`.
pub const REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
/// `IO_REPARSE_TAG_LX_SYMLINK` (symbolic links created by WSL).
pub const REPARSE_TAG_LX_SYMLINK: u32 = 0xa000_001d;

/// `CreateFileW` with Linux-like sharing (other opens, renames and deletes
/// of an open file are allowed). `path` must be NUL-terminated.
///
/// Returns the Win32 error code on failure so callers can refine it.
pub fn create(
    path: &[u16],
    access: u32,
    disposition: FILE_CREATION_DISPOSITION,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> Result<OwnedHandle, u32> {
    assert_eq!(path.last(), Some(&0), "path must be NUL-terminated");
    // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the
    // call; no security attributes or template handle.
    let r = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            disposition,
            flags,
            None,
        )
    };
    match r {
        Ok(h) => Ok(OwnedHandle::from_raw(h)),
        Err(e) => Err((e.code().0 as u32) & 0xffff),
    }
}

/// `GetFileAttributesW`, or `None` when the path cannot be queried.
pub fn attributes(path: &[u16]) -> Option<u32> {
    assert_eq!(path.last(), Some(&0), "path must be NUL-terminated");
    // SAFETY: `path` is a NUL-terminated UTF-16 string.
    let a = unsafe { GetFileAttributesW(PCWSTR(path.as_ptr())) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a)
}

/// Where a write goes.
#[derive(Clone, Copy, Debug)]
pub enum WriteAt {
    Offset(u64),
    /// Atomically at the current end of the file.
    End,
}

/// Largest single transfer (Linux `MAX_RW_COUNT`, which fits in the `u32`
/// length Windows takes).
fn io_len(buf: &UserBuf) -> u32 {
    buf.len().min(crate::linux::fcntl::MAX_RW_COUNT) as u32
}

/// Reads into `buf` at `offset`. Returns 0 at end of file.
pub fn read_at(h: HANDLE, buf: UserBuf, offset: u64) -> SysResult<usize> {
    let mut iosb = IO_STATUS_BLOCK::default();
    let off = offset as i64;
    // SAFETY: `buf` is a caller buffer that no Rust reference aliases (see
    // `UserBuf::new`); Windows probes it and fails with an access violation
    // status if it is not writable. `iosb` and `off` are locals that outlive
    // the call. The handle is synchronous, so the call completes before it
    // returns.
    let status = unsafe {
        NtReadFile(
            h,
            None,
            None,
            None,
            &mut iosb,
            buf.ptr() as *mut c_void,
            io_len(&buf),
            Some(&off),
            None,
        )
    };
    match status {
        wf::STATUS_END_OF_FILE => Ok(0),
        s if s.is_ok() => Ok(iosb.Information),
        s => Err(Errno::from_ntstatus(s)),
    }
}

/// Writes from `buf`.
pub fn write_at(h: HANDLE, buf: UserBuf, at: WriteAt) -> SysResult<usize> {
    let mut iosb = IO_STATUS_BLOCK::default();
    // FILE_WRITE_TO_END_OF_FILE is a byte offset of -1.
    let off = match at {
        WriteAt::Offset(o) => o as i64,
        WriteAt::End => -1,
    };
    // SAFETY: as in `read_at`.
    let status = unsafe {
        NtWriteFile(
            h,
            None,
            None,
            None,
            &mut iosb,
            buf.ptr() as *const c_void,
            io_len(&buf),
            Some(&off),
            None,
        )
    };
    if status.is_ok() {
        Ok(iosb.Information)
    } else {
        Err(Errno::from_ntstatus(status))
    }
}

/// Metadata of an open file.
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub attributes: u32,
    pub reparse_tag: u32,
    /// Times in 100 ns units since 1601 (FILETIME).
    pub creation: i64,
    pub access: i64,
    pub write: i64,
    pub change: i64,
    pub size: i64,
    pub allocation: i64,
    pub links: u32,
    pub directory: bool,
    pub volume: u64,
    pub file_id: u64,
}

fn query<T: Default>(h: HANDLE, class: FILE_INFO_BY_HANDLE_CLASS) -> SysResult<T> {
    let mut v = T::default();
    // SAFETY: `v` is a valid buffer of the size passed, of the type that
    // matches `class` (checked by the callers below).
    unsafe {
        GetFileInformationByHandleEx(
            h,
            class,
            &mut v as *mut T as *mut c_void,
            size_of::<T>() as u32,
        )
    }
    .map_err(Errno::from)?;
    Ok(v)
}

/// Reads the metadata `stat` needs.
pub fn info(h: HANDLE) -> SysResult<Info> {
    let basic: FILE_BASIC_INFO = query(h, FileBasicInfo)?;
    let std: FILE_STANDARD_INFO = query(h, FileStandardInfo)?;
    let reparse_tag = if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        query::<FILE_ATTRIBUTE_TAG_INFO>(h, FileAttributeTagInfo)?.ReparseTag
    } else {
        0
    };
    let (volume, file_id) = match query::<FILE_ID_INFO>(h, FileIdInfo) {
        Ok(id) => {
            let b = id.FileId.Identifier;
            let lo = u64::from_le_bytes(b[..8].try_into().expect("8 bytes"));
            let hi = u64::from_le_bytes(b[8..].try_into().expect("8 bytes"));
            // NTFS ids fit in the low half; fold longer (ReFS) ids.
            (id.VolumeSerialNumber, lo ^ hi)
        }
        Err(_) => {
            let mut bh = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: `bh` is a valid out buffer.
            unsafe { GetFileInformationByHandle(h, &mut bh) }.map_err(Errno::from)?;
            (
                u64::from(bh.dwVolumeSerialNumber),
                (u64::from(bh.nFileIndexHigh) << 32) | u64::from(bh.nFileIndexLow),
            )
        }
    };
    Ok(Info {
        attributes: basic.FileAttributes,
        reparse_tag,
        creation: basic.CreationTime,
        access: basic.LastAccessTime,
        write: basic.LastWriteTime,
        change: basic.ChangeTime,
        size: std.EndOfFile,
        allocation: std.AllocationSize,
        links: std.NumberOfLinks,
        directory: std.Directory,
        volume,
        file_id,
    })
}

/// Sets the size of an open file (truncating or extending with zeros).
pub fn set_len(h: HANDLE, len: u64) -> SysResult<()> {
    let info = FILE_END_OF_FILE_INFO {
        EndOfFile: i64::try_from(len).map_err(|_| Errno::EFBIG)?,
    };
    // SAFETY: `info` is a valid FILE_END_OF_FILE_INFO of the size passed.
    unsafe {
        SetFileInformationByHandle(
            h,
            FileEndOfFileInfo,
            &info as *const FILE_END_OF_FILE_INFO as *const c_void,
            size_of::<FILE_END_OF_FILE_INFO>() as u32,
        )
    }
    .map_err(Errno::from)
}

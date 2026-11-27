//! Files: open, positional read and write, metadata.

use core::ffi::c_void;

use windows::Wdk::Storage::FileSystem::{NtReadFile, NtWriteFile};
use windows::Win32::Foundation::{self as wf, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, CreateSymbolicLinkW,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_CREATION_DISPOSITION, FILE_END_OF_FILE_INFO, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_RENAME_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileAttributeTagInfo,
    FileBasicInfo, FileDispositionInfoEx, FileEndOfFileInfo, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdInfo, FileRenameInfoEx, FileStandardInfo,
    FlushFileBuffers, GetFileAttributesW, GetFileInformationByHandle, GetFileInformationByHandleEx,
    INVALID_FILE_ATTRIBUTES, SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE,
    SYMBOLIC_LINK_FLAG_DIRECTORY, SetFileInformationByHandle,
};
use windows::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};
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
    /// The Linux mode stored with the file ([`super::ea`]), if any.
    pub mode: Option<u32>,
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
        mode: if super::ea::ea_size(h) > 0 {
            super::ea::read_mode(h)
        } else {
            None
        },
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

/// `FileDispositionInfoEx` flags.
const DISPOSITION_DELETE: u32 = 0x1;
const DISPOSITION_POSIX_SEMANTICS: u32 = 0x2;
const DISPOSITION_IGNORE_READONLY: u32 = 0x10;
/// `FileRenameInfoEx` flags.
const RENAME_REPLACE_IF_EXISTS: u32 = 0x1;
const RENAME_POSIX_SEMANTICS: u32 = 0x2;
const RENAME_IGNORE_READONLY: u32 = 0x40;

fn win32_code(e: windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xffff
}

fn set_info(h: HANDLE, class: FILE_INFO_BY_HANDLE_CLASS, data: &[u8]) -> Result<(), u32> {
    // SAFETY: `data` is a buffer of the structure `class` expects, of the
    // length passed (built by the callers below).
    unsafe {
        SetFileInformationByHandle(h, class, data.as_ptr() as *const c_void, data.len() as u32)
    }
    .map_err(win32_code)
}

/// Removes the name of an open file or empty directory with POSIX
/// semantics: the name goes away at once, open handles stay usable.
/// Returns the Win32 error code on failure.
pub fn delete(h: HANDLE) -> Result<(), u32> {
    let flags = DISPOSITION_DELETE | DISPOSITION_POSIX_SEMANTICS | DISPOSITION_IGNORE_READONLY;
    set_info(h, FileDispositionInfoEx, &flags.to_le_bytes())
}

/// Renames an open file to `target` (a full `\\?\` path, NUL-terminated),
/// replacing an existing file when `replace`. Returns the Win32 error code.
pub fn rename(h: HANDLE, target: &[u16], replace: bool) -> Result<(), u32> {
    let name = &target[..target.len() - 1];
    let name_off = core::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = name_off + name.len() * 2 + 2;
    // A u64 buffer keeps the HANDLE field aligned.
    let mut buf = vec![0u64; size.div_ceil(8)];
    let mut flags = RENAME_POSIX_SEMANTICS | RENAME_IGNORE_READONLY;
    if replace {
        flags |= RENAME_REPLACE_IF_EXISTS;
    }
    // SAFETY: `buf` is large enough for the header and the name, and
    // suitably aligned for FILE_RENAME_INFO.
    unsafe {
        let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
        (*info).Anonymous.Flags = flags;
        (*info).FileNameLength = (name.len() * 2) as u32;
        core::ptr::copy_nonoverlapping(
            name.as_ptr(),
            (buf.as_mut_ptr() as *mut u8).add(name_off) as *mut u16,
            name.len(),
        );
    }
    // SAFETY: reinterpretation of the u64 buffer as its bytes.
    let bytes = unsafe { core::slice::from_raw_parts(buf.as_ptr() as *const u8, size) };
    set_info(h, FileRenameInfoEx, bytes)
}

/// `CreateDirectoryW`. Returns the Win32 error code on failure.
pub fn create_directory(path: &[u16]) -> Result<(), u32> {
    assert_eq!(path.last(), Some(&0), "path must be NUL-terminated");
    // SAFETY: `path` is a NUL-terminated UTF-16 string.
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), None) }.map_err(win32_code)
}

/// `CreateSymbolicLinkW` without requiring the privilege where Windows
/// allows it (developer mode). Returns the Win32 error code on failure.
pub fn create_symlink(link: &[u16], target: &[u16], directory: bool) -> Result<(), u32> {
    assert_eq!(link.last(), Some(&0), "path must be NUL-terminated");
    assert_eq!(target.last(), Some(&0), "target must be NUL-terminated");
    let mut flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    if directory {
        flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
    }
    // SAFETY: both are NUL-terminated UTF-16 strings.
    let ok = unsafe { CreateSymbolicLinkW(PCWSTR(link.as_ptr()), PCWSTR(target.as_ptr()), flags) };
    if ok {
        Ok(())
    } else {
        // SAFETY: reads the calling thread's last error.
        Err(unsafe { wf::GetLastError() }.0)
    }
}

/// `FlushFileBuffers`.
pub fn flush(h: HANDLE) -> SysResult<()> {
    // SAFETY: `h` is a valid handle for the call.
    unsafe { FlushFileBuffers(h) }.map_err(Errno::from)
}

/// The target of a symbolic link, from its reparse data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// A Windows symbolic link: the substitute name and whether it is
    /// relative.
    Windows { name: Vec<u16>, relative: bool },
    /// A Linux symbolic link created by WSL: the target bytes.
    Linux(Vec<u8>),
}

/// Reads the reparse point of an open link (opened with
/// `FILE_FLAG_OPEN_REPARSE_POINT`).
pub fn read_link(h: HANDLE) -> SysResult<LinkTarget> {
    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00a8;
    let mut buf = vec![0u8; 16 * 1024];
    let mut n = 0u32;
    // SAFETY: `buf` is a writable buffer of the size passed; `n` is a valid
    // out pointer; the call is synchronous.
    unsafe {
        DeviceIoControl(
            h,
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some(buf.as_mut_ptr() as *mut c_void),
            buf.len() as u32,
            Some(&mut n),
            None,
        )
    }
    .map_err(|_| Errno::EINVAL)?;
    let b = &buf[..n as usize];
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let tag = b
        .get(..4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("4 bytes")))
        .ok_or(Errno::EINVAL)?;
    match tag {
        REPARSE_TAG_SYMLINK => {
            // ReparseTag, DataLength, Reserved, then SubstituteNameOffset,
            // SubstituteNameLength, PrintNameOffset, PrintNameLength,
            // Flags (u32), PathBuffer.
            let so = usize::from(u16_at(8).ok_or(Errno::EINVAL)?);
            let sl = usize::from(u16_at(10).ok_or(Errno::EINVAL)?);
            let flags = b.get(16..20).ok_or(Errno::EINVAL)?;
            let relative = u32::from_le_bytes(flags.try_into().expect("4 bytes")) & 1 != 0;
            let s = b.get(20 + so..20 + so + sl).ok_or(Errno::EINVAL)?;
            let name = s
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(LinkTarget::Windows { name, relative })
        }
        REPARSE_TAG_LX_SYMLINK => {
            // ReparseTag, DataLength, Reserved, a version word (2), then the
            // UTF-8 target.
            Ok(LinkTarget::Linux(
                b.get(12..).ok_or(Errno::EINVAL)?.to_vec(),
            ))
        }
        _ => Err(Errno::EINVAL),
    }
}

/// One directory entry as Windows reports it.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: Vec<u16>,
    pub attributes: u32,
    /// The reparse tag, for entries that are reparse points.
    pub reparse_tag: u32,
    pub file_id: u64,
}

/// Lists the directory open as `h`, from the start.
pub fn read_dir(h: HANDLE) -> SysResult<Vec<DirEntry>> {
    let mut out = Vec::new();
    let mut buf = vec![0u64; 8192];
    let mut restart = true;
    loop {
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        restart = false;
        // SAFETY: `buf` is a writable buffer of the size passed, aligned for
        // FILE_ID_BOTH_DIR_INFO.
        let r = unsafe {
            GetFileInformationByHandleEx(
                h,
                class,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
            )
        };
        if let Err(e) = r {
            if e.code() == wf::ERROR_NO_MORE_FILES.to_hresult() {
                return Ok(out);
            }
            return Err(Errno::from(e));
        }
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        loop {
            // SAFETY: Windows wrote a chain of entries into `buf`; each
            // entry and its name lie inside the buffer.
            let (next, entry) = unsafe {
                let e = &*(base.add(off) as *const FILE_ID_BOTH_DIR_INFO);
                let name = core::slice::from_raw_parts(
                    base.add(off + core::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName))
                        as *const u16,
                    e.FileNameLength as usize / 2,
                );
                (
                    e.NextEntryOffset,
                    DirEntry {
                        name: name.to_vec(),
                        attributes: e.FileAttributes,
                        reparse_tag: e.EaSize,
                        file_id: e.FileId as u64,
                    },
                )
            };
            out.push(entry);
            if next == 0 {
                break;
            }
            off += next as usize;
        }
    }
}

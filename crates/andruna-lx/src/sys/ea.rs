//! The Linux file mode stored in an NTFS extended attribute.
//!
//! lx keeps `st_mode` (permission bits, and the file type of special files
//! that NTFS has no type for, such as sockets) in the extended attribute
//! `ANDRUNA.MODE`: a 4-byte little-endian value. Files without it report
//! a mode derived from their Windows attributes.

use core::ffi::c_void;

use windows::Wdk::Storage::FileSystem::{
    FILE_EA_INFORMATION, FileEaInformation, NtQueryEaFile, NtQueryInformationFile, NtSetEaFile,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_EA, ReOpenFile,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

use super::OwnedHandle;
use crate::errno::{Errno, SysResult};

const NAME: &[u8] = b"ANDRUNA.MODE";

/// Size of all extended attributes of the file (0: none).
pub fn ea_size(h: HANDLE) -> u32 {
    let mut info = FILE_EA_INFORMATION::default();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `info` and `iosb` are valid out buffers of the sizes passed.
    let st = unsafe {
        NtQueryInformationFile(
            h,
            &mut iosb,
            (&mut info as *mut FILE_EA_INFORMATION).cast(),
            size_of::<FILE_EA_INFORMATION>() as u32,
            FileEaInformation,
        )
    };
    if st.is_ok() { info.EaSize } else { 0 }
}

/// The stored mode, if the file has one (and the handle may read
/// extended attributes).
pub fn read_mode(h: HANDLE) -> Option<u32> {
    // FILE_GET_EA_INFORMATION: next offset (u32), name length (u8), name
    // with its NUL.
    let mut list = vec![0u8; 5 + NAME.len() + 1];
    list[4] = NAME.len() as u8;
    list[5..5 + NAME.len()].copy_from_slice(NAME);
    let mut buf = [0u64; 8];
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `buf` is a writable buffer of the size passed; `list` is a
    // valid FILE_GET_EA_INFORMATION of the length passed.
    let st = unsafe {
        NtQueryEaFile(
            h,
            &mut iosb,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            true,
            Some(list.as_ptr().cast::<c_void>()),
            list.len() as u32,
            None,
            true,
        )
    };
    if st.is_err() {
        return None;
    }
    // FILE_FULL_EA_INFORMATION: next (u32), flags (u8), name length (u8),
    // value length (u16), name, NUL, value.
    // SAFETY: `buf` is 64 bytes, initialized, and outlives `b`.
    let b = unsafe { core::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size_of_val(&buf)) };
    let value_len = usize::from(u16::from_le_bytes([b[6], b[7]]));
    let at = 8 + usize::from(b[5]) + 1;
    if value_len != 4 || at + 4 > b.len() {
        return None;
    }
    Some(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

/// Stores `mode`; the handle needs `FILE_WRITE_EA` access.
pub fn write_mode(h: HANDLE, mode: u32) -> SysResult<()> {
    let mut b = vec![0u8; 8 + NAME.len() + 1 + 4];
    b[5] = NAME.len() as u8;
    b[6..8].copy_from_slice(&4u16.to_le_bytes());
    b[8..8 + NAME.len()].copy_from_slice(NAME);
    let at = 8 + NAME.len() + 1;
    b[at..at + 4].copy_from_slice(&mode.to_le_bytes());
    // FILE_FULL_EA_INFORMATION records are 4-byte aligned in the buffer.
    let words: Vec<u32> = b
        .chunks(4)
        .map(|c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            u32::from_ne_bytes(w)
        })
        .collect();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `words` holds a valid FILE_FULL_EA_INFORMATION of `b.len()`
    // bytes.
    let st = unsafe { NtSetEaFile(h, &mut iosb, words.as_ptr().cast(), b.len() as u32) };
    if st.is_err() {
        return Err(Errno::from_ntstatus(st));
    }
    Ok(())
}

/// A second handle to the same file with `FILE_WRITE_EA` access.
pub fn reopen_for_ea(h: HANDLE) -> SysResult<OwnedHandle> {
    // SAFETY: `h` is a live file handle; the new handle is owned by the
    // result.
    let n = unsafe {
        ReOpenFile(
            h,
            FILE_WRITE_EA.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    }
    .map_err(Errno::from)?;
    Ok(OwnedHandle::from_raw(n))
}

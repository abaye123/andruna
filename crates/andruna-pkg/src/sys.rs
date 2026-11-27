//! Windows API wrappers of andruna-pkg. All `unsafe` code of the crate is
//! here.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::ops::Deref;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY,
    UnmapViewOfFile,
};

pub(crate) fn to_io(e: windows::core::Error) -> io::Error {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((h & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// A file mapped read-only into memory.
///
/// The file is opened with read sharing only, so no other process can
/// write to it while it is mapped: the bytes behind the slice cannot change
/// under a reader.
pub struct MappedFile {
    _file: File,
    view: *const u8,
    len: usize,
}

// SAFETY: the view is read-only memory owned by this value; sharing a
// read-only slice between threads is safe.
unsafe impl Send for MappedFile {}
// SAFETY: as above.
unsafe impl Sync for MappedFile {}

impl MappedFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(path)?;
        let len = file.metadata()?.len();
        if len > isize::MAX as u64 {
            return Err(io::Error::other("file too large to map"));
        }
        if len == 0 {
            return Ok(Self {
                _file: file,
                view: std::ptr::null(),
                len: 0,
            });
        }
        let fh = HANDLE(file.as_raw_handle());
        // SAFETY: `fh` is a valid file handle opened for reading; a
        // read-only mapping of its whole size is requested.
        let mapping =
            unsafe { CreateFileMappingW(fh, None, PAGE_READONLY, 0, 0, None) }.map_err(to_io)?;
        // SAFETY: `mapping` is the valid mapping handle created above; the
        // view of the whole file is read-only. The view keeps the mapping
        // alive after the handle is closed.
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
        // SAFETY: `mapping` is a handle this function owns.
        unsafe {
            let _ = CloseHandle(mapping);
        }
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _file: file,
            view: view.Value as *const u8,
            len: len as usize,
        })
    }
}

impl Deref for MappedFile {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `view` points to `len` readable bytes mapped for the
        // lifetime of `self`, and no writer can change the file (see the
        // type's documentation).
        unsafe { std::slice::from_raw_parts(self.view, self.len) }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        if !self.view.is_null() {
            // SAFETY: `view` is the view mapped in `open` and is unmapped
            // once; no slice borrowed from `self` outlives it.
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut c_void,
                });
            }
        }
    }
}

/// Fills `buf` from the system's cryptographic random number generator.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    // SAFETY: `buf` is a valid writable slice; with
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG no algorithm handle is needed.
    unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(to_io)
}

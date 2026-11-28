//! Windows calls of the app lifecycle: the boot image section and its views
//! in other processes. All `unsafe` code of [`crate::apps`] is here.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{GENERIC_EXECUTE, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile3, PAGE_EXECUTE_WRITECOPY, PAGE_PROTECTION_FLAGS,
    UNMAP_VIEW_OF_FILE_FLAGS, UnmapViewOfFile2, VIRTUAL_ALLOCATION_TYPE,
};
use windows::core::PCWSTR;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn to_io(e: windows::core::Error) -> io::Error {
    let code = e.code().0;
    if (code as u32) & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error(code & 0xFFFF)
    } else {
        io::Error::other(e)
    }
}

/// A read-only, file-backed section of `path` whose views may be made
/// executable and copy-on-write (`PAGE_EXECUTE_WRITECOPY`). The file
/// handle is closed once the section holds the file.
pub fn file_section(path: &Path) -> io::Result<OwnedHandle> {
    let w = wide(path.as_os_str());
    // SAFETY: `w` is NUL-terminated; both handles are owned right after
    // creation.
    unsafe {
        let file = CreateFileW(
            PCWSTR(w.as_ptr()),
            (GENERIC_READ | GENERIC_EXECUTE).0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(to_io)?;
        let file = OwnedHandle::from_raw_handle(file.0);
        let s = CreateFileMappingW(
            HANDLE(file.as_raw_handle()),
            None,
            PAGE_EXECUTE_WRITECOPY,
            0,
            0,
            PCWSTR::null(),
        )
        .map_err(to_io)?;
        Ok(OwnedHandle::from_raw_handle(s.0))
    }
}

/// Maps `size` bytes of `section` from offset 0 at `base` in `process`
/// with `protect`. The process gets no handle to the section.
pub fn map_view(
    section: BorrowedHandle<'_>,
    process: BorrowedHandle<'_>,
    base: usize,
    size: usize,
    protect: PAGE_PROTECTION_FLAGS,
) -> io::Result<()> {
    // SAFETY: both handles are valid for the call; the view is created in
    // the target process at an address Windows checks to be free.
    let v = unsafe {
        MapViewOfFile3(
            HANDLE(section.as_raw_handle()),
            Some(HANDLE(process.as_raw_handle())),
            Some(base as *const c_void),
            0,
            size,
            VIRTUAL_ALLOCATION_TYPE(0),
            protect.0,
            None,
        )
    };
    if v.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    if v.Value as usize != base {
        unmap_view(process, v.Value as usize);
        return Err(io::Error::other(format!(
            "the view was mapped at {:#x}, not {base:#x}",
            v.Value as usize
        )));
    }
    Ok(())
}

/// Unmaps the view at `base` in `process` (best effort).
pub fn unmap_view(process: BorrowedHandle<'_>, base: usize) {
    // SAFETY: a valid process handle; Windows checks that a view starts at
    // `base`.
    unsafe {
        let _ = UnmapViewOfFile2(
            HANDLE(process.as_raw_handle()),
            windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                Value: base as *mut c_void,
            },
            UNMAP_VIEW_OF_FILE_FLAGS(0),
        );
    }
}

/// Memory of a process, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub working_set: usize,
    pub private_working_set: usize,
    /// Private commit (`PrivateUsage`).
    pub private_commit: usize,
}

/// The memory counters of `process`.
pub fn memory(process: BorrowedHandle<'_>) -> io::Result<Memory> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
    };
    let mut c = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a valid, writable struct of the size passed; the
    // EX2 layout starts with PROCESS_MEMORY_COUNTERS.
    unsafe {
        GetProcessMemoryInfo(
            HANDLE(process.as_raw_handle()),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX2).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    }
    .map_err(to_io)?;
    Ok(Memory {
        working_set: c.WorkingSetSize,
        private_working_set: c.PrivateWorkingSetSize,
        private_commit: c.PrivateUsage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsHandle;
    use windows::Win32::System::Memory::{
        PAGE_EXECUTE_READ, PAGE_READONLY, PAGE_WRITECOPY, VirtualProtect,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// The protections lx gives region pages work on a view mapped
    /// read-only from such a section, and copy-on-write never reaches the
    /// file.
    #[test]
    fn view_protections() {
        let dir = std::env::temp_dir().join(format!("andruna-sysd-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob");
        std::fs::write(&path, vec![7u8; 0x20000]).unwrap();
        let section = file_section(&path).unwrap();
        // SAFETY: the pseudo handle of this process needs no closing.
        let me = unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess().0) };
        // Find a free address by mapping anywhere first.
        // SAFETY: maps a view of a section this test owns into itself.
        let base = unsafe {
            let v = MapViewOfFile3(
                HANDLE(section.as_raw_handle()),
                None,
                None,
                0,
                0x20000,
                VIRTUAL_ALLOCATION_TYPE(0),
                PAGE_READONLY.0,
                None,
            );
            assert!(!v.Value.is_null());
            let b = v.Value as usize;
            unmap_view(me, b);
            b
        };
        // Mapped read-only, a view cannot become executable later; mapped
        // execute-read it can become any of these (and charges no commit).
        map_view(section.as_handle(), me, base, 0x20000, PAGE_READONLY).unwrap();
        let mut old0 = PAGE_PROTECTION_FLAGS(0);
        // SAFETY: a page of the view this test mapped.
        let r =
            unsafe { VirtualProtect(base as *const c_void, 0x1000, PAGE_EXECUTE_READ, &mut old0) };
        assert!(r.is_err());
        unmap_view(me, base);
        map_view(section.as_handle(), me, base, 0x20000, PAGE_EXECUTE_READ).unwrap();
        let mut old = PAGE_PROTECTION_FLAGS(0);
        for (off, p) in [
            (0x3000, PAGE_READONLY),
            (0x0000, PAGE_EXECUTE_READ),
            (0x1000, PAGE_WRITECOPY),
            (0x2000, PAGE_EXECUTE_WRITECOPY),
        ] {
            // SAFETY: pages of the view this test mapped.
            unsafe { VirtualProtect((base + off) as *const c_void, 0x1000, p, &mut old) }
                .unwrap_or_else(|e| panic!("{p:?}: {e}"));
        }
        // SAFETY: the page is copy-on-write now.
        unsafe { ((base + 0x1000) as *mut u8).write_volatile(1) };
        // SAFETY: readable.
        assert_eq!(unsafe { ((base + 0x1000) as *const u8).read_volatile() }, 1);
        unmap_view(me, base);
        drop(section);
        assert!(std::fs::read(&path).unwrap().iter().all(|b| *b == 7));
        // Mapping where something is already mapped fails.
        let section = file_section(&path).unwrap();
        map_view(section.as_handle(), me, base, 0x20000, PAGE_READONLY).unwrap();
        assert!(map_view(section.as_handle(), me, base, 0x20000, PAGE_READONLY).is_err());
        unmap_view(me, base);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Windows API wrappers of andruna-pkg. All `unsafe` code of the crate is
//! here.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::ops::Deref;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, PROPERTYKEY, WIN32_ERROR,
};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY,
    UnmapViewOfFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

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

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `PKEY_AppUserModel_ID` (propkey.h).
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

/// COM initialized on this thread for the life of the value.
struct ComScope(bool);

impl ComScope {
    fn enter() -> Self {
        // SAFETY: no preconditions; the matching CoUninitialize is in Drop
        // and only runs when this call succeeded (S_OK or S_FALSE). A
        // thread already in the multithreaded apartment gets
        // RPC_E_CHANGED_MODE; COM is usable there and is left as it is.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self(hr.is_ok())
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// A Start menu shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub target: std::path::PathBuf,
    pub arguments: String,
    pub icon: Option<std::path::PathBuf>,
    pub description: String,
    pub app_user_model_id: String,
}

/// Writes a `.lnk` shortcut with its AppUserModelID.
pub fn write_shortcut(path: &Path, s: &Shortcut) -> io::Result<()> {
    let _com = ComScope::enter();
    // SAFETY: COM is initialized on this thread (`_com`); every string
    // passed is a NUL-terminated HSTRING that outlives the call; the
    // PROPVARIANT is built with VT_LPWSTR and a CoTaskMemAlloc'd string,
    // which PropVariantClear frees.
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(to_io)?;
        link.SetPath(&HSTRING::from(s.target.as_os_str()))
            .map_err(to_io)?;
        link.SetArguments(&HSTRING::from(s.arguments.as_str()))
            .map_err(to_io)?;
        if let Some(dir) = s.target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                .map_err(to_io)?;
        }
        if let Some(icon) = &s.icon {
            link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)
                .map_err(to_io)?;
        }
        link.SetDescription(&HSTRING::from(s.description.as_str()))
            .map_err(to_io)?;
        let store: IPropertyStore = link.cast().map_err(to_io)?;
        let w = wide(&s.app_user_model_id);
        let mem = CoTaskMemAlloc(w.len() * 2) as *mut u16;
        if mem.is_null() {
            return Err(io::Error::from(io::ErrorKind::OutOfMemory));
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), mem, w.len());
        let mut pv = PROPVARIANT::default();
        (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*pv.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(mem);
        let set = store.SetValue(&PKEY_APP_USER_MODEL_ID, &pv);
        let _ = PropVariantClear(&mut pv);
        set.map_err(to_io)?;
        store.Commit().map_err(to_io)?;
        let file: IPersistFile = link.cast().map_err(to_io)?;
        file.Save(&HSTRING::from(path.as_os_str()), true)
            .map_err(to_io)?;
    }
    Ok(())
}

/// Reads a shortcut back (target, arguments, icon, AppUserModelID).
pub fn read_shortcut(path: &Path) -> io::Result<Shortcut> {
    let _com = ComScope::enter();
    // SAFETY: COM is initialized on this thread; buffers are sized as the
    // methods take them; the PROPVARIANT from GetValue is cleared.
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(to_io)?;
        let file: IPersistFile = link.cast().map_err(to_io)?;
        file.Load(&HSTRING::from(path.as_os_str()), STGM_READ)
            .map_err(to_io)?;
        let mut buf = vec![0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0)
            .map_err(to_io)?;
        let target = from_wide(&buf);
        buf.fill(0);
        link.GetArguments(&mut buf).map_err(to_io)?;
        let arguments = from_wide(&buf);
        buf.fill(0);
        let mut index = 0;
        link.GetIconLocation(&mut buf, &mut index).map_err(to_io)?;
        let icon = from_wide(&buf);
        buf.fill(0);
        link.GetDescription(&mut buf).map_err(to_io)?;
        let description = from_wide(&buf);
        let store: IPropertyStore = link.cast().map_err(to_io)?;
        let mut pv = store.GetValue(&PKEY_APP_USER_MODEL_ID).map_err(to_io)?;
        let aumid = match PropVariantToStringAlloc(&pv) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const c_void));
                s
            }
            Err(_) => String::new(),
        };
        let _ = PropVariantClear(&mut pv);
        Ok(Shortcut {
            target: target.into(),
            arguments,
            icon: (!icon.is_empty()).then(|| icon.into()),
            description,
            app_user_model_id: aumid,
        })
    }
}

/// The user's Start menu `Programs` folder.
pub fn programs_folder() -> io::Result<std::path::PathBuf> {
    // SAFETY: the returned string is CoTaskMemAlloc'd and freed here.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None).map_err(to_io)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(std::path::PathBuf::from(s.map_err(io::Error::other)?))
    }
}

fn win32(e: WIN32_ERROR) -> io::Result<()> {
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e.0 as i32))
    }
}

/// A registry value to write.
pub enum RegValue<'a> {
    Str(&'a str),
    Dword(u32),
}

/// Creates `HKEY_CURRENT_USER\<key>` and writes the values.
pub fn write_user_key(key: &str, values: &[(&str, RegValue<'_>)]) -> io::Result<()> {
    let mut hkey = HKEY::default();
    // SAFETY: `hkey` receives an open key handle, closed below; strings
    // are NUL-terminated HSTRINGs alive across the calls; data slices are
    // valid for their lengths.
    unsafe {
        win32(RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        ))?;
        let mut result = Ok(());
        for (name, v) in values {
            let r = match v {
                RegValue::Str(s) => {
                    let w = wide(s);
                    let bytes = std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2);
                    RegSetValueExW(hkey, &HSTRING::from(*name), None, REG_SZ, Some(bytes))
                }
                RegValue::Dword(d) => RegSetValueExW(
                    hkey,
                    &HSTRING::from(*name),
                    None,
                    REG_DWORD,
                    Some(&d.to_le_bytes()),
                ),
            };
            if let Err(e) = win32(r) {
                result = Err(e);
                break;
            }
        }
        let _ = RegCloseKey(hkey);
        result
    }
}

/// Reads a string value of `HKEY_CURRENT_USER\<key>`.
pub fn read_user_string(key: &str, name: &str) -> io::Result<String> {
    let mut buf = vec![0u16; 2048];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` has `len` writable bytes.
    unsafe {
        win32(RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut len),
        ))?;
    }
    Ok(from_wide(&buf))
}

/// Deletes `HKEY_CURRENT_USER\<key>` with its subkeys. A missing key is
/// not an error.
pub fn delete_user_key(key: &str) -> io::Result<()> {
    // SAFETY: the key path is a NUL-terminated HSTRING.
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    win32(r)?;
    // RegDeleteTreeW leaves the key itself.
    // SAFETY: as above.
    let r = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        win32(r)
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

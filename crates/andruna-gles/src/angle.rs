//! Loading ANGLE's DLLs (`libEGL.dll`, `libGLESv2.dll`, built by
//! `cargo xtask angle-build`) and filling the entry point slots.
//!
//! The directory is `ANDRUNA_ANGLE_DIR`, else the directory of the running
//! executable, else its `angle` subdirectory. ANGLE's D3D11 back end loads
//! `d3dcompiler_47.dll` from System32 itself.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LoadLibraryExW,
};
use windows::core::{HSTRING, PCSTR};

use crate::thunk::{Lib, SLOTS};

/// The result of loading ANGLE (once per process).
#[derive(Debug, Clone)]
pub struct Loaded {
    pub dir: PathBuf,
    /// Entry points ANGLE exports (filled slots).
    pub resolved: usize,
    /// Entry points of the table ANGLE does not export.
    pub missing: Vec<&'static str>,
}

static LOADED: OnceLock<Result<Loaded, String>> = OnceLock::new();

/// Environment variable naming the directory of ANGLE's DLLs.
pub const DIR_VAR: &str = "ANDRUNA_ANGLE_DIR";

fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os(DIR_VAR) {
        v.push(PathBuf::from(d));
    }
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        v.push(d.join("angle"));
        v.push(d);
    }
    v
}

fn load_dll(path: &Path) -> Result<HMODULE, String> {
    // SAFETY: loads a DLL by full path; its directory is searched for its
    // own dependencies (libEGL.dll loads libGLESv2.dll next to it).
    unsafe {
        LoadLibraryExW(
            &HSTRING::from(path.as_os_str()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map_err(|e| format!("{}: {e}", path.display()))
}

fn load_inner() -> Result<Loaded, String> {
    let dir = candidates()
        .into_iter()
        .find(|d| d.join("libEGL.dll").is_file() && d.join("libGLESv2.dll").is_file())
        .ok_or_else(|| format!("ANGLE's libEGL.dll and libGLESv2.dll not found (set {DIR_VAR})"))?;
    let gles = load_dll(&dir.join("libGLESv2.dll"))?;
    let egl = load_dll(&dir.join("libEGL.dll"))?;
    let mut resolved = 0;
    let mut missing = Vec::new();
    for e in crate::thunk::entries() {
        let m = match e.lib {
            Lib::Egl => egl,
            Lib::Gles => gles,
        };
        let name = std::ffi::CString::new(e.name).expect("entry names have no NUL");
        // SAFETY: a module this function loaded and keeps loaded.
        let p = unsafe { GetProcAddress(m, PCSTR(name.as_ptr().cast())) };
        match p {
            Some(f) => {
                SLOTS[e.slot].store(f as usize, Ordering::Relaxed);
                resolved += 1;
            }
            None => missing.push(e.name),
        }
    }
    Ok(Loaded {
        dir,
        resolved,
        missing,
    })
}

/// Loads ANGLE once; later calls return the first result.
pub fn load() -> Result<&'static Loaded, &'static str> {
    match LOADED.get_or_init(|| {
        let r = load_inner();
        if let Err(e) = &r {
            crate::log(format_args!("cannot load ANGLE: {e}"));
        }
        r
    }) {
        Ok(l) => Ok(l),
        Err(e) => Err(e.as_str()),
    }
}

/// ANGLE's function `name` as a typed pointer: `angle_fn!(name, fn(..) -> ..)`.
/// Evaluates to `Option<unsafe extern "C" fn(..) -> ..>`.
macro_rules! angle_fn {
    ($name:literal, fn($($t:ty),* $(,)?) $(-> $r:ty)?) => {{
        let p = $crate::thunk::angle_address($name);
        if p == 0 {
            None
        } else {
            // SAFETY: `p` is ANGLE's export of `$name`, whose C prototype
            // the caller spells here (Microsoft convention, `extern "C"`).
            Some(unsafe { core::mem::transmute::<usize, unsafe extern "C" fn($($t),*) $(-> $r)?>(p) })
        }
    }};
}
pub(crate) use angle_fn;

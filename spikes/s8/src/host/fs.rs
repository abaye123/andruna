//! Path calls lx does not export yet (directories, unlink, rename, cwd).
//!
//! The spike resolves Linux paths with lx's own path normalization and
//! mount table (`andruna_lx::fs::path`, `andruna_lx::fs::mount`) and calls
//! Windows directly. In the product these belong in lx.

use core::ffi::{c_char, c_void};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Mutex;

use andruna_lx::fs::mount::MOUNTS;
use andruna_lx::fs::path::{LinuxPath, decode_component, normalize};
use andruna_lx::linux::errno::*;

use super::{cstr, fail_with, guard, set_errno};

const AT_FDCWD: i32 = -100;
const AT_REMOVEDIR: i32 = 0x200;

static CWD: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// The Linux path of the guest executable (`/proc/self/exe`).
pub static EXE_PATH: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn cwd() -> Vec<u8> {
    let c = CWD.lock().unwrap();
    if c.is_empty() { b"/".to_vec() } else { c.clone() }
}

fn linux_path(dirfd: i32, p: &[u8]) -> Result<LinuxPath, i32> {
    if p.is_empty() {
        return Err(ENOENT);
    }
    let base = if p[0] == b'/' {
        b"/".to_vec()
    } else if dirfd == AT_FDCWD {
        cwd()
    } else {
        let f = andruna_lx::fd::FDS.get(dirfd).map_err(|e| e.0)?;
        f.ops().dir_path().ok_or(ENOTDIR)?.to_vec()
    };
    normalize(&base, p).map_err(|e| e.0)
}

fn win_path(dirfd: i32, p: &[u8]) -> Result<PathBuf, i32> {
    let lp = linux_path(dirfd, p)?;
    let mut w = MOUNTS.read().unwrap().resolve(&lp).map_err(|e| e.0)?;
    w.pop(); // NUL
    Ok(PathBuf::from(OsString::from_wide(&w)))
}

fn io_errno(e: &std::io::Error) -> i32 {
    match e.raw_os_error() {
        Some(c) => andruna_lx::errno::Errno::from_win32(c as u32).0,
        None => match e.kind() {
            std::io::ErrorKind::NotFound => ENOENT,
            std::io::ErrorKind::AlreadyExists => EEXIST,
            std::io::ErrorKind::PermissionDenied => EACCES,
            std::io::ErrorKind::DirectoryNotEmpty => ENOTEMPTY,
            _ => EIO,
        },
    }
}

fn result(r: std::io::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => fail_with(io_errno(&e)),
    }
}

/// Runs `f` on the Windows path of a guest path argument.
///
/// # Safety
///
/// `p` is NULL or a NUL-terminated string.
unsafe fn with_path(dirfd: i32, p: *const c_char, f: impl FnOnce(PathBuf) -> i32) -> i32 {
    // SAFETY: forwarded from the caller.
    let Some(b) = (unsafe { cstr(p) }) else { return fail_with(EFAULT) };
    match win_path(dirfd, b) {
        Ok(w) => f(w),
        Err(e) => fail_with(e),
    }
}

gfn! {
    pub unsafe fn h_access(p: *const c_char, _mode: i32) -> i32 {
        // SAFETY: a guest path.
        guard(|| unsafe { with_path(AT_FDCWD, p, |w| if w.exists() { 0 } else { fail_with(ENOENT) }) })
    }
}

gfn! {
    pub unsafe fn h_faccessat(d: i32, p: *const c_char, _mode: i32, _flags: i32) -> i32 {
        // SAFETY: a guest path.
        guard(|| unsafe { with_path(d, p, |w| if w.exists() { 0 } else { fail_with(ENOENT) }) })
    }
}

gfn! {
    pub unsafe fn h_mkdir(p: *const c_char, _mode: u32) -> i32 {
        // SAFETY: a guest path.
        guard(|| unsafe { with_path(AT_FDCWD, p, |w| result(std::fs::create_dir(w))) })
    }
}

gfn! {
    pub unsafe fn h_rmdir(p: *const c_char) -> i32 {
        // SAFETY: a guest path.
        guard(|| unsafe { with_path(AT_FDCWD, p, |w| result(std::fs::remove_dir(w))) })
    }
}

fn unlink_path(w: PathBuf) -> i32 {
    if w.is_dir() {
        return fail_with(EISDIR);
    }
    result(std::fs::remove_file(w))
}

gfn! {
    pub unsafe fn h_unlink(p: *const c_char) -> i32 {
        // SAFETY: a guest path.
        guard(|| unsafe { with_path(AT_FDCWD, p, unlink_path) })
    }
}

gfn! {
    pub unsafe fn h_unlinkat(d: i32, p: *const c_char, flags: i32) -> i32 {
        guard(|| {
            // SAFETY: a guest path.
            unsafe {
                with_path(d, p, |w| {
                    if flags & AT_REMOVEDIR != 0 { result(std::fs::remove_dir(w)) } else { unlink_path(w) }
                })
            }
        })
    }
}

gfn! {
    pub unsafe fn h_rename(a: *const c_char, b: *const c_char) -> i32 {
        guard(|| {
            // SAFETY: guest paths.
            unsafe { with_path(AT_FDCWD, a, |wa| with_path(AT_FDCWD, b, |wb| result(std::fs::rename(&wa, wb)))) }
        })
    }
}

gfn! {
    pub unsafe fn h_chdir(p: *const c_char) -> i32 {
        guard(|| {
            // SAFETY: a guest path.
            let Some(b) = (unsafe { cstr(p) }) else { return fail_with(EFAULT) };
            let lp = match linux_path(AT_FDCWD, b) {
                Ok(l) => l,
                Err(e) => return fail_with(e),
            };
            match win_path(AT_FDCWD, b) {
                Ok(w) if w.is_dir() => {}
                Ok(_) => return fail_with(ENOENT),
                Err(e) => return fail_with(e),
            }
            let s = lp.to_bytes();
            if let Err(e) = andruna_lx::fs::set_cwd(&String::from_utf8_lossy(&s)) {
                return fail_with(e.0);
            }
            *CWD.lock().unwrap() = s;
            0
        })
    }
}

gfn! {
    pub unsafe fn h_getcwd(buf: *mut u8, size: usize) -> *mut u8 {
        guard(|| {
            let c = cwd();
            if buf.is_null() {
                // bionic allocates when buf is NULL.
                // SAFETY: the guest allocator (host malloc).
                let p = unsafe { super::misc::h_malloc(c.len() + 1) } as *mut u8;
                // SAFETY: just allocated.
                unsafe {
                    core::ptr::copy_nonoverlapping(c.as_ptr(), p, c.len());
                    *p.add(c.len()) = 0;
                }
                return p;
            }
            if c.len() + 1 > size {
                set_errno(ERANGE);
                return core::ptr::null_mut();
            }
            // SAFETY: the guest's buffer of `size` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(c.as_ptr(), buf, c.len());
                *buf.add(c.len()) = 0;
            }
            buf
        })
    }
}

gfn! {
    pub unsafe fn h_realpath(p: *const c_char, out: *mut u8) -> *mut u8 {
        guard(|| {
            // SAFETY: a guest path.
            let Some(b) = (unsafe { cstr(p) }) else { set_errno(EINVAL); return core::ptr::null_mut() };
            let lp = match linux_path(AT_FDCWD, b) {
                Ok(l) => l,
                Err(e) => { set_errno(e); return core::ptr::null_mut() }
            };
            match win_path(AT_FDCWD, b) {
                Ok(w) if w.exists() => {}
                Ok(_) => { set_errno(ENOENT); return core::ptr::null_mut() }
                Err(e) => { set_errno(e); return core::ptr::null_mut() }
            }
            let s = lp.to_bytes();
            let dst = if out.is_null() {
                // SAFETY: host malloc.
                unsafe { super::misc::h_malloc(4096) as *mut u8 }
            } else {
                out
            };
            let n = s.len().min(4095);
            // SAFETY: PATH_MAX-sized destination, as realpath requires.
            unsafe {
                core::ptr::copy_nonoverlapping(s.as_ptr(), dst, n);
                *dst.add(n) = 0;
            }
            dst
        })
    }
}

gfn! {
    pub unsafe fn h_truncate(p: *const c_char, len: i64) -> i32 {
        guard(|| {
            // SAFETY: a guest path.
            unsafe {
                with_path(AT_FDCWD, p, |w| {
                    result(std::fs::OpenOptions::new().write(true).open(w).and_then(|f| f.set_len(len.max(0) as u64)))
                })
            }
        })
    }
}

gfn! {
    pub unsafe fn h_ok_path(_p: *const c_char, _a: usize) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_ok_fd(_fd: i32, _a: usize) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_readlink(p: *const c_char, buf: *mut u8, n: usize) -> isize {
        guard(|| {
            // SAFETY: a guest path.
            let b = unsafe { cstr(p) }.unwrap_or(b"");
            // lx has no /proc yet; the one link the tests read is
            // /proc/self/exe, the guest executable's Linux path.
            if b == b"/proc/self/exe" {
                let e = EXE_PATH.lock().unwrap().clone();
                let l = e.len().min(n);
                // SAFETY: the guest's buffer of `n` bytes (not terminated,
                // as readlink).
                unsafe { core::ptr::copy_nonoverlapping(e.as_ptr(), buf, l) };
                return l as isize;
            }
            // No symbolic links in the spike's file tree.
            // SAFETY: a guest path.
            unsafe { with_path(AT_FDCWD, p, |w| if w.exists() { fail_with(EINVAL) } else { fail_with(ENOENT) }) as isize }
        })
    }
}

gfn! {
    pub unsafe fn h_statvfs(_p: *const c_char, out: *mut u8) -> i32 {
        guard(|| {
            // struct statvfs (LP64): f_bsize, f_frsize, f_blocks, f_bfree,
            // f_bavail, f_files, f_ffree, f_favail (8 bytes each), f_fsid,
            // f_flag, f_namemax; 112 bytes.
            // SAFETY: the guest's buffer.
            unsafe {
                core::ptr::write_bytes(out, 0, 112);
                let w = out as *mut u64;
                *w = 4096;
                *w.add(1) = 4096;
                for i in 2..5 {
                    *w.add(i) = 1 << 24;
                }
                *w.add(10) = 255;
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pathconf(_p: *const c_char, name: i32) -> i64 {
        // _PC_NAME_MAX = 3, _PC_PATH_MAX = 4
        guard(|| match name { 3 => 255, 4 => 4096, _ => -1 })
    }
}

// --- directories --------------------------------------------------------------

struct Dir {
    entries: Vec<(Vec<u8>, u8)>,
    pos: usize,
    /// bionic `struct dirent`: d_ino u64, d_off i64, d_reclen u16, d_type
    /// u8, d_name[256]; 280 bytes.
    ent: [u8; 280],
}

const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

fn open_dir(w: PathBuf) -> *mut c_void {
    match std::fs::read_dir(&w) {
        Ok(it) => {
            let mut entries = vec![(b".".to_vec(), DT_DIR), (b"..".to_vec(), DT_DIR)];
            for e in it.flatten() {
                use std::os::windows::ffi::OsStrExt;
                let name: Vec<u16> = e.file_name().encode_wide().collect();
                let t = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { DT_DIR } else { DT_REG };
                entries.push((decode_component(&name), t));
            }
            Box::into_raw(Box::new(Dir { entries, pos: 0, ent: [0; 280] })) as *mut c_void
        }
        Err(e) => {
            set_errno(io_errno(&e));
            core::ptr::null_mut()
        }
    }
}

gfn! {
    pub unsafe fn h_opendir(p: *const c_char) -> *mut c_void {
        guard(|| {
            // SAFETY: a guest path.
            let Some(b) = (unsafe { cstr(p) }) else { set_errno(EFAULT); return core::ptr::null_mut() };
            match win_path(AT_FDCWD, b) {
                Ok(w) => open_dir(w),
                Err(e) => { set_errno(e); core::ptr::null_mut() }
            }
        })
    }
}

gfn! {
    pub unsafe fn h_fdopendir(fd: i32) -> *mut c_void {
        guard(|| {
            let path = match andruna_lx::fd::FDS.get(fd) {
                Ok(f) => match f.ops().dir_path() {
                    Some(p) => p.to_vec(),
                    None => { set_errno(ENOTDIR); return core::ptr::null_mut() }
                },
                Err(e) => { set_errno(e.0); return core::ptr::null_mut() }
            };
            match win_path(AT_FDCWD, &path) {
                Ok(w) => open_dir(w),
                Err(e) => { set_errno(e); core::ptr::null_mut() }
            }
        })
    }
}

gfn! {
    pub unsafe fn h_readdir(d: *mut c_void) -> *mut u8 {
        guard(|| {
            // SAFETY: a DIR* from opendir.
            let d = unsafe { &mut *(d as *mut Dir) };
            let Some((name, t)) = d.entries.get(d.pos) else { return core::ptr::null_mut() };
            d.pos += 1;
            d.ent = [0; 280];
            d.ent[0..8].copy_from_slice(&(d.pos as u64).to_le_bytes());
            d.ent[8..16].copy_from_slice(&(d.pos as i64).to_le_bytes());
            d.ent[16..18].copy_from_slice(&280u16.to_le_bytes());
            d.ent[18] = *t;
            let n = name.len().min(255);
            d.ent[19..19 + n].copy_from_slice(&name[..n]);
            d.ent.as_mut_ptr()
        })
    }
}

gfn! {
    pub unsafe fn h_closedir(d: *mut c_void) -> i32 {
        guard(|| {
            if !d.is_null() {
                // SAFETY: a DIR* from opendir, closed once.
                drop(unsafe { Box::from_raw(d as *mut Dir) });
            }
            0
        })
    }
}

#[cfg(target_arch = "x86_64")]
type NftwFn = extern "sysv64" fn(*const c_char, *const c_void, i32, *mut [i32; 2]) -> i32;
#[cfg(not(target_arch = "x86_64"))]
type NftwFn = extern "C" fn(*const c_char, *const c_void, i32, *mut [i32; 2]) -> i32;

const FTW_F: i32 = 0;
const FTW_D: i32 = 1;
const FTW_DP: i32 = 5;
const FTW_DEPTH: i32 = 8;

fn walk(path: &[u8], level: i32, f: NftwFn, flags: i32) -> i32 {
    let Ok(w) = win_path(AT_FDCWD, path) else { return -1 };
    let mut st = [0u8; 160];
    let c = std::ffi::CString::new(path.to_vec()).unwrap();
    // SAFETY: a valid path and a buffer larger than struct stat.
    unsafe { super::lx::h_lstat(c.as_ptr(), st.as_mut_ptr() as *mut c_void) };
    let base = path.iter().rposition(|&b| b == b'/').map(|i| i + 1).unwrap_or(0) as i32;
    let mut ftw = [base, level];
    if w.is_dir() {
        if flags & FTW_DEPTH == 0 {
            let r = f(c.as_ptr(), st.as_ptr() as *const c_void, FTW_D, &mut ftw);
            if r != 0 {
                return r;
            }
        }
        if let Ok(it) = std::fs::read_dir(&w) {
            for e in it.flatten() {
                use std::os::windows::ffi::OsStrExt;
                let name: Vec<u16> = e.file_name().encode_wide().collect();
                let mut child = path.to_vec();
                if child.last() != Some(&b'/') {
                    child.push(b'/');
                }
                child.extend(decode_component(&name));
                let r = walk(&child, level + 1, f, flags);
                if r != 0 {
                    return r;
                }
            }
        }
        if flags & FTW_DEPTH != 0 {
            return f(c.as_ptr(), st.as_ptr() as *const c_void, FTW_DP, &mut ftw);
        }
        0
    } else {
        f(c.as_ptr(), st.as_ptr() as *const c_void, FTW_F, &mut ftw)
    }
}

gfn! {
    pub unsafe fn h_nftw(p: *const c_char, f: NftwFn, _fds: i32, flags: i32) -> i32 {
        guard(|| {
            // SAFETY: a guest path.
            let Some(b) = (unsafe { cstr(p) }) else { return fail_with(EFAULT) };
            walk(b, 0, f, flags)
        })
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("access", h_access as usize),
        ("faccessat", h_faccessat as usize),
        ("mkdir", h_mkdir as usize),
        ("rmdir", h_rmdir as usize),
        ("unlink", h_unlink as usize),
        ("unlinkat", h_unlinkat as usize),
        ("rename", h_rename as usize),
        ("chdir", h_chdir as usize),
        ("getcwd", h_getcwd as usize),
        ("realpath", h_realpath as usize),
        ("truncate", h_truncate as usize),
        ("readlink", h_readlink as usize),
        ("statvfs", h_statvfs as usize),
        ("pathconf", h_pathconf as usize),
        ("opendir", h_opendir as usize),
        ("fdopendir", h_fdopendir as usize),
        ("readdir", h_readdir as usize),
        ("closedir", h_closedir as usize),
        ("nftw", h_nftw as usize),
        // Permissions and times are not modelled in the spike's file tree.
        ("chmod", h_ok_path as usize),
        ("fchmod", h_ok_fd as usize),
        ("fchmodat", h_ok_fd as usize),
        ("fchown", h_ok_fd as usize),
        ("utimensat", h_ok_fd as usize),
    ]
}

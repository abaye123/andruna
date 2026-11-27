//! Process and thread identity: `getpid`, `gettid`, `sched_yield`, and the
//! user and group ids lx reports.
//!
//! Linux gives the main thread `tid == pid` (bionic relies on it: the main
//! thread's tid is `getpid()`). Windows process and thread ids come from
//! one namespace and never coincide, so the host designates the thread that
//! runs the program's `main` ([`set_main_thread`]); `gettid()` on it is the
//! process id, and lx maps that id back to the Windows thread wherever it
//! takes a thread id. Other threads keep their Windows thread ids.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::sys::thread;

static UID: AtomicU32 = AtomicU32::new(0);
static GID: AtomicU32 = AtomicU32::new(0);

pub fn getpid() -> i32 {
    thread::current_process_id() as i32
}

/// The Windows id of the main thread (0 until designated).
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

/// Designates the calling thread as the main thread.
pub fn set_main_thread() {
    MAIN_THREAD.store(thread::current_thread_id(), Ordering::Relaxed);
}

pub fn gettid() -> i32 {
    let w = thread::current_thread_id();
    if w == MAIN_THREAD.load(Ordering::Relaxed) {
        getpid()
    } else {
        w as i32
    }
}

/// The Windows thread id of Linux thread id `tid`.
pub fn windows_tid(tid: u32) -> u32 {
    let main = MAIN_THREAD.load(Ordering::Relaxed);
    if main != 0 && tid == getpid() as u32 {
        main
    } else {
        tid
    }
}

/// The Linux thread id of Windows thread id `w`.
pub fn linux_tid(w: u32) -> u32 {
    if w != 0 && w == MAIN_THREAD.load(Ordering::Relaxed) {
        getpid() as u32
    } else {
        w
    }
}

/// `getppid`: the process that started this one (a Windows process id),
/// or 0 once it has ended, as Linux reports for a parent outside the
/// process's view.
pub fn getppid() -> i32 {
    thread::parent_process_id() as i32
}

pub fn sched_yield() {
    thread::yield_now();
}

/// Sets the user and group id that lx reports for this process and its
/// files. The host process sets the package's ids before running app code;
/// until then both are 0.
pub fn set_ids(uid: u32, gid: u32) {
    UID.store(uid, Ordering::Relaxed);
    GID.store(gid, Ordering::Relaxed);
}

static DUMPABLE: AtomicU32 = AtomicU32::new(1);
static NO_NEW_PRIVS: AtomicU32 = AtomicU32::new(0);

const PR_GET_DUMPABLE: i32 = 3;
const PR_SET_DUMPABLE: i32 = 4;
const PR_SET_NAME: i32 = 15;
const PR_GET_NAME: i32 = 16;
const PR_SET_NO_NEW_PRIVS: i32 = 38;
const PR_GET_NO_NEW_PRIVS: i32 = 39;
const PR_SET_VMA: i32 = 0x5356_4d41;
const PR_SET_VMA_ANON_NAME: u64 = 0;

/// `prctl`, for the options lx implements; others are `EINVAL`, as Linux
/// reports for options it does not know.
///
/// # Safety
///
/// Pointer arguments satisfy the option's contract (`PR_SET_NAME`,
/// `PR_GET_NAME`: 16 bytes; `PR_SET_VMA`: a NUL-terminated name or NULL).
pub unsafe fn prctl(
    option: i32,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> crate::errno::SysResult<i64> {
    use crate::errno::Errno;
    match option {
        PR_GET_DUMPABLE => Ok(i64::from(DUMPABLE.load(Ordering::Relaxed))),
        PR_SET_DUMPABLE => {
            if a2 > 1 {
                return Err(Errno::EINVAL);
            }
            DUMPABLE.store(a2 as u32, Ordering::Relaxed);
            Ok(0)
        }
        PR_SET_NO_NEW_PRIVS => {
            if a2 != 1 || a3 != 0 || a4 != 0 || a5 != 0 {
                return Err(Errno::EINVAL);
            }
            NO_NEW_PRIVS.store(1, Ordering::Relaxed);
            Ok(0)
        }
        PR_GET_NO_NEW_PRIVS => Ok(i64::from(NO_NEW_PRIVS.load(Ordering::Relaxed))),
        PR_SET_NAME => {
            if a2 == 0 {
                return Err(Errno::EFAULT);
            }
            // SAFETY: a 16-byte buffer (contract).
            let b = unsafe { core::slice::from_raw_parts(a2 as *const u8, 16) };
            let n = &b[..b.iter().position(|&c| c == 0).unwrap_or(15).min(15)];
            thread::set_description(gettid() as u32, &String::from_utf8_lossy(n))?;
            Ok(0)
        }
        PR_GET_NAME => {
            if a2 == 0 {
                return Err(Errno::EFAULT);
            }
            let n = crate::fs::proc::thread_name(gettid() as u32);
            let mut b = [0u8; 16];
            b[..n.len()].copy_from_slice(&n);
            // SAFETY: a 16-byte buffer (contract).
            unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), a2 as *mut u8, 16) };
            Ok(0)
        }
        PR_SET_VMA if a2 == PR_SET_VMA_ANON_NAME => {
            let name = if a5 == 0 {
                Vec::new()
            } else {
                // SAFETY: a NUL-terminated string (contract).
                let s = unsafe { core::ffi::CStr::from_ptr(a5 as *const core::ffi::c_char) };
                let s = s.to_bytes();
                if s.len() > 80 || s.iter().any(|&c| c < 0x20 || b"\\`$[]".contains(&c)) {
                    return Err(Errno::EINVAL);
                }
                let mut n = b"[anon:".to_vec();
                n.extend_from_slice(s);
                n.push(b']');
                n
            };
            crate::mem::set_name(a3 as usize, a4 as usize, &name).map(|()| 0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// The main thread's stack: the address of the initial block (argc,
/// argv, envp, auxv) and the stack's size; `(0, 0)` until the host sets it.
static MAIN_STACK: RwLock<(u64, u64)> = RwLock::new((0, 0));

/// Records the guest main thread's stack (`startstack` in
/// `/proc/self/stat`, `RLIMIT_STACK`).
pub fn set_main_stack(start: u64, size: u64) {
    *crate::util::write(&MAIN_STACK) = (start, size);
}

/// The main thread's initial stack block and stack size.
pub fn main_stack() -> (u64, u64) {
    *crate::util::read(&MAIN_STACK)
}

/// The program: its Windows path and its arguments.
static PROGRAM: RwLock<Option<(std::path::PathBuf, Vec<String>)>> = RwLock::new(None);

/// Records the program the host runs (the executable's Windows path and
/// `argv`), for `/proc/self/exe` and `/proc/self/cmdline`.
pub fn set_program(exe: &std::path::Path, argv: &[String]) {
    *crate::util::write(&PROGRAM) = Some((exe.to_path_buf(), argv.to_vec()));
}

/// The executable's Linux path, through the mount table; `None` when no
/// program was recorded or no mount holds it.
pub fn exe_path() -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    let g = crate::util::read(&PROGRAM);
    let (exe, _) = g.as_ref()?;
    let abs = std::path::absolute(exe).ok()?;
    let s = abs.to_string_lossy();
    let verbatim = if s.starts_with(r"\\?\") {
        s.into_owned()
    } else {
        format!(r"\\?\{s}")
    };
    let w: Vec<u16> = std::ffi::OsStr::new(&verbatim).encode_wide().collect();
    crate::util::read(&crate::fs::mount::MOUNTS).unresolve(&w)
}

/// `argv`, each argument followed by a NUL, as `/proc/self/cmdline`.
pub fn cmdline() -> Vec<u8> {
    let g = crate::util::read(&PROGRAM);
    let mut out = Vec::new();
    for a in g.as_ref().map(|p| p.1.as_slice()).unwrap_or_default() {
        out.extend_from_slice(a.as_bytes());
        out.push(0);
    }
    out
}

pub fn uid() -> u32 {
    UID.load(Ordering::Relaxed)
}

pub fn gid() -> u32 {
    GID.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert_eq!(getpid() as u32, std::process::id());
        let main_tid = gettid();
        let other = std::thread::spawn(gettid).join().unwrap();
        assert_ne!(main_tid, other);
        assert!(main_tid > 0 && other > 0);
        sched_yield();
    }

    #[test]
    fn parent_and_its_cpu_clock() {
        // The test runner started this process and is still running.
        let ppid = getppid();
        assert!(ppid > 0);
        let clk =
            crate::linux::time::make_process_cpuclock(ppid, crate::linux::time::CPUCLOCK_SCHED);
        assert!(crate::time::clock_gettime(clk).is_ok());
        let none = crate::linux::time::make_process_cpuclock(0x7fff_fff0, 2);
        assert!(crate::time::clock_gettime(none).is_err());
    }
}

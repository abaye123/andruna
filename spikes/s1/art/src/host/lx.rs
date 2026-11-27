//! Guest calls forwarded to andruna-lx's exports.
//!
//! lx's exports use the Windows convention (`extern "C"` on Windows); the
//! functions here are the System V entry points guest code binds to. The
//! structure layouts are the same on both sides (bionic's LP64 layouts,
//! which lx asserts), so pointers pass through unchanged.
//!
//! lx has no console descriptors yet. The spike reserves descriptors 0, 1
//! and 2 in lx (opened on `/dev/null`) and keeps an overlay that marks
//! which descriptor numbers currently mean a console stream; `dup`,
//! `dup2`, `dup3`, `fcntl(F_DUPFD)` and `close` keep the overlay in step,
//! so a test that redirects stderr into a file and back works.

use core::ffi::{c_char, c_void};
use std::sync::Mutex;

use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    CONSOLE_MODE, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};

use super::{fail_with, guard};
use andruna_lx::linux::errno::*;

mod sys {
    use core::ffi::{c_char, c_void};
    unsafe extern "C" {
        pub fn open(p: *const c_char, flags: i32, mode: u32) -> i32;
        pub fn openat(d: i32, p: *const c_char, flags: i32, mode: u32) -> i32;
        pub fn close(fd: i32) -> i32;
        pub fn read(fd: i32, b: *mut c_void, n: usize) -> isize;
        pub fn write(fd: i32, b: *const c_void, n: usize) -> isize;
        pub fn pread64(fd: i32, b: *mut c_void, n: usize, off: i64) -> isize;
        pub fn pwrite64(fd: i32, b: *const c_void, n: usize, off: i64) -> isize;
        pub fn lseek(fd: i32, off: i64, whence: i32) -> i64;
        pub fn fstat(fd: i32, st: *mut c_void) -> i32;
        pub fn stat(p: *const c_char, st: *mut c_void) -> i32;
        pub fn lstat(p: *const c_char, st: *mut c_void) -> i32;
        pub fn fstatat(d: i32, p: *const c_char, st: *mut c_void, flags: i32) -> i32;
        pub fn dup(fd: i32) -> i32;
        pub fn dup3(o: i32, n: i32, flags: i32) -> i32;
        pub fn dup2(o: i32, n: i32) -> i32;
        pub fn fcntl(fd: i32, cmd: i32, arg: usize) -> i32;
        pub fn mmap(
            a: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            off: i64,
        ) -> *mut c_void;
        pub fn munmap(a: *mut c_void, len: usize) -> i32;
        pub fn mprotect(a: *mut c_void, len: usize, prot: i32) -> i32;
        pub fn madvise(a: *mut c_void, len: usize, advice: i32) -> i32;
        pub fn syscall(n: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64) -> i64;
        pub fn clock_gettime(clk: i32, ts: *mut c_void) -> i32;
        pub fn clock_getres(clk: i32, ts: *mut c_void) -> i32;
        pub fn nanosleep(req: *const c_void, rem: *mut c_void) -> i32;
        pub fn clock_nanosleep(clk: i32, flags: i32, req: *const c_void, rem: *mut c_void) -> i32;
        pub fn sched_yield() -> i32;
        pub fn getpid() -> i32;
        pub fn gettid() -> i32;
        pub fn __errno() -> *mut i32;
    }
}

/// lx's `gettid`, called directly from Rust (Windows convention), for the
/// boundary benchmark.
pub fn lx_gettid() -> i32 {
    // SAFETY: no arguments.
    unsafe { sys::gettid() }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Std {
    In,
    Out,
    Err,
}

const MAX_FD: usize = 4096;
static CONSOLE: Mutex<[Option<Std>; MAX_FD]> = Mutex::new([None; MAX_FD]);

fn console(fd: i32) -> Option<Std> {
    if fd < 0 || fd as usize >= MAX_FD {
        return None;
    }
    CONSOLE.lock().unwrap()[fd as usize]
}

fn set_console(fd: i32, v: Option<Std>) {
    if fd >= 0 && (fd as usize) < MAX_FD {
        CONSOLE.lock().unwrap()[fd as usize] = v;
    }
}

fn handle(s: Std) -> STD_HANDLE {
    match s {
        Std::In => STD_INPUT_HANDLE,
        Std::Out => STD_OUTPUT_HANDLE,
        Std::Err => STD_ERROR_HANDLE,
    }
}

/// Reserves descriptors 0 to 2 in lx and marks them as the console.
pub fn init_std_fds() -> Result<(), String> {
    for (want, s) in [(0, Std::In), (1, Std::Out), (2, Std::Err)] {
        // SAFETY: a NUL-terminated path.
        let fd = unsafe { sys::open(c"/dev/null".as_ptr(), 2, 0) };
        if fd != want {
            return Err(format!(
                "expected lx descriptor {want} for the console, got {fd}"
            ));
        }
        set_console(fd, Some(s));
    }
    Ok(())
}

fn console_write(s: Std, b: *const c_void, n: usize) -> isize {
    // SAFETY: a standard handle of this process.
    let Ok(h) = (unsafe { GetStdHandle(handle(s)) }) else {
        return fail_with(EBADF) as isize;
    };
    let mut done = 0u32;
    // SAFETY: the guest's buffer of `n` bytes.
    let data = unsafe { core::slice::from_raw_parts(b as *const u8, n) };
    // SAFETY: valid handle and buffer.
    match unsafe { WriteFile(h, Some(data), Some(&mut done), None) } {
        Ok(()) => done as isize,
        Err(_) => fail_with(EIO) as isize,
    }
}

gfn! {
    pub unsafe fn h_write(fd: i32, b: *const c_void, n: usize) -> isize {
        guard(|| match console(fd) {
            Some(s) if s != Std::In => console_write(s, b, n),
            // SAFETY: forwarded unchanged; lx validates the buffer.
            _ => unsafe { sys::write(fd, b, n) },
        })
    }
}

gfn! {
    pub unsafe fn h_read(fd: i32, b: *mut c_void, n: usize) -> isize {
        guard(|| match console(fd) {
            Some(Std::In) => {
                // SAFETY: the process's input handle.
                let Ok(h) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else { return fail_with(EBADF) as isize };
                let mut done = 0u32;
                // SAFETY: the guest's buffer of `n` bytes.
                let data = unsafe { core::slice::from_raw_parts_mut(b as *mut u8, n) };
                // SAFETY: valid handle and buffer.
                match unsafe { ReadFile(h, Some(data), Some(&mut done), None) } {
                    Ok(()) => done as isize,
                    Err(_) => 0,
                }
            }
            Some(_) => fail_with(EBADF) as isize,
            // SAFETY: forwarded unchanged.
            None => unsafe { sys::read(fd, b, n) },
        })
    }
}

gfn! {
    pub unsafe fn h_close(fd: i32) -> i32 {
        guard(|| {
            // SAFETY: forwarded.
            let r = unsafe { sys::close(fd) };
            if r == 0 {
                set_console(fd, None);
            }
            r
        })
    }
}

gfn! {
    pub unsafe fn h_dup(fd: i32) -> i32 {
        guard(|| {
            // SAFETY: forwarded.
            let r = unsafe { sys::dup(fd) };
            if r >= 0 {
                set_console(r, console(fd));
            }
            r
        })
    }
}

gfn! {
    pub unsafe fn h_dup2(o: i32, n: i32) -> i32 {
        guard(|| {
            // SAFETY: forwarded.
            let r = unsafe { sys::dup2(o, n) };
            if r >= 0 {
                set_console(n, console(o));
            }
            r
        })
    }
}

gfn! {
    pub unsafe fn h_dup3(o: i32, n: i32, flags: i32) -> i32 {
        guard(|| {
            // SAFETY: forwarded.
            let r = unsafe { sys::dup3(o, n, flags) };
            if r >= 0 {
                set_console(n, console(o));
            }
            r
        })
    }
}

gfn! {
    pub unsafe fn h_fcntl(fd: i32, cmd: i32, arg: usize) -> i32 {
        guard(|| {
            // SAFETY: forwarded; the third variadic argument is read as an
            // integer, compatible for the integer and pointer forms.
            let r = unsafe { sys::fcntl(fd, cmd, arg) };
            // F_DUPFD, F_DUPFD_CLOEXEC
            if r >= 0 && (cmd == 0 || cmd == 1030) {
                set_console(r, console(fd));
            }
            r
        })
    }
}

gfn! {
    pub unsafe fn h_fstat(fd: i32, st: *mut c_void) -> i32 {
        guard(|| {
            if console(fd).is_some() {
                if st.is_null() {
                    return fail_with(EFAULT);
                }
                // A character device, as a terminal on Linux.
                // SAFETY: the guest's `struct stat`, 144 bytes on x86_64 and
                // 128 on arm64; st_mode is at offset 24 (x86_64) or 16.
                unsafe {
                    core::ptr::write_bytes(st as *mut u8, 0, if cfg!(target_arch = "x86_64") { 144 } else { 128 });
                    let mode_off = if cfg!(target_arch = "x86_64") { 24 } else { 16 };
                    ((st as *mut u8).add(mode_off) as *mut u32).write_unaligned(0o020620);
                }
                return 0;
            }
            // SAFETY: forwarded.
            unsafe { sys::fstat(fd, st) }
        })
    }
}

gfn! {
    pub unsafe fn h_isatty(fd: i32) -> i32 {
        guard(|| match console(fd) {
            Some(s) => {
                let mut m = CONSOLE_MODE(0);
                // SAFETY: standard handle and an out parameter.
                let ok = unsafe { GetStdHandle(handle(s)).map(|h| GetConsoleMode(h, &mut m).is_ok()) };
                if ok.unwrap_or(false) { 1 } else { super::set_errno(ENOTTY); 0 }
            }
            None => {
                super::set_errno(ENOTTY);
                0
            }
        })
    }
}

gfn! {
    pub unsafe fn h_lseek(fd: i32, off: i64, whence: i32) -> i64 {
        guard(|| {
            if console(fd).is_some() {
                return fail_with(ESPIPE) as i64;
            }
            // SAFETY: forwarded.
            unsafe { sys::lseek(fd, off, whence) }
        })
    }
}

gfn! {
    pub unsafe fn h_writev(fd: i32, iov: *const [usize; 2], n: i32) -> isize {
        guard(|| {
            let mut total = 0isize;
            for i in 0..n.max(0) as usize {
                // SAFETY: the guest's iovec array of `n` entries.
                let [b, l] = unsafe { iov.add(i).read() };
                // SAFETY: as `h_write`.
                let r = unsafe { h_write(fd, b as *const c_void, l) };
                if r < 0 {
                    return if total > 0 { total } else { r };
                }
                total += r;
                if (r as usize) < l {
                    break;
                }
            }
            total
        })
    }
}

gfn! {
    /// `mmap`. lx has no file-backed mappings yet: a file mapping is made
    /// as an anonymous private mapping filled with `pread` (spike limit:
    /// `MAP_SHARED` changes are not written back and other mappings of
    /// the file do not see them).
    pub unsafe fn h_mmap(a: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void {
        const MAP_SHARED: i32 = 1;
        const MAP_ANONYMOUS: i32 = 0x20;
        const PROT_WRITE: i32 = 2;
        guard(|| {
            if flags & MAP_ANONYMOUS != 0 || fd < 0 {
                // SAFETY: forwarded unchanged.
                return unsafe { sys::mmap(a, len, prot, flags, fd, off) };
            }
            let anon = (flags & !MAP_SHARED) | 2 /* MAP_PRIVATE */ | MAP_ANONYMOUS;
            // SAFETY: a new anonymous mapping, writable while it is filled.
            let p = unsafe { sys::mmap(a, len, prot | PROT_WRITE, anon, -1, 0) };
            if p as isize == -1 {
                return p;
            }
            let mut done = 0usize;
            while done < len {
                // SAFETY: inside the new mapping.
                let r = unsafe {
                    sys::pread64(fd, (p as *mut u8).add(done) as *mut c_void, len - done, off + done as i64)
                };
                if r <= 0 {
                    break; // past the end of the file: the rest stays zero
                }
                done += r as usize;
            }
            if prot & PROT_WRITE == 0 {
                // SAFETY: the mapping just made.
                unsafe { sys::mprotect(p, len, prot) };
            }
            p
        })
    }
}

gfn! {
    /// `syscall`, forwarded to lx, except `FUTEX_REQUEUE` and
    /// `FUTEX_CMP_REQUEUE`, which lx does not have yet: they wake the
    /// waiters that would have been requeued instead of moving them (a
    /// spurious wake-up, which every futex user must tolerate).
    pub unsafe fn h_syscall(n: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64) -> i64 {
        const NR_FUTEX: i64 = 202;
        const FUTEX_WAKE: i64 = 1;
        const FUTEX_REQUEUE: i64 = 3;
        const FUTEX_CMP_REQUEUE: i64 = 4;
        const FUTEX_PRIVATE: i64 = 128;
        guard(|| {
            if n == NR_FUTEX {
                use core::sync::atomic::Ordering::Relaxed;
                let op = a2 & 127;
                if op == 0 || op == 9 {
                    // FUTEX_WAIT, FUTEX_WAIT_BITSET
                    FUTEX_STATS[0].fetch_add(1, Relaxed);
                    // SAFETY: lx's gettid.
                    let tid = unsafe { sys::gettid() };
                    FUTEX_WAITS.lock().unwrap().push((tid, a1 as usize, a3 as i32, op));
                    // SAFETY: forwarded unchanged.
                    let r = unsafe { sys::syscall(n, a1, a2, a3, a4, a5, a6) };
                    let mut w = FUTEX_WAITS.lock().unwrap();
                    if let Some(i) = w.iter().position(|x| x.0 == tid) {
                        w.swap_remove(i);
                    }
                    return r;
                }
                if op == FUTEX_WAKE {
                    FUTEX_STATS[1].fetch_add(1, Relaxed);
                }
                if op == FUTEX_REQUEUE || op == FUTEX_CMP_REQUEUE {
                    FUTEX_STATS[2].fetch_add(1, Relaxed);
                    if futex_log() {
                        eprintln!("[s1] futex requeue op {op} word {a1:#x} wake {a3} requeue {a4} to {a5:#x} val3 {a6:#x}");
                    }
                    if op == FUTEX_CMP_REQUEUE {
                        // SAFETY: the futex word the guest passed.
                        let cur = unsafe { core::ptr::read_volatile(a1 as *const i32) };
                        if cur != a6 as i32 {
                            FUTEX_STATS[3].fetch_add(1, Relaxed);
                            return fail_with(EAGAIN) as i64;
                        }
                    }
                    let n_wake = (a3 as i32).max(0) as i64 + (a4 as i32).max(0) as i64;
                    // SAFETY: a FUTEX_WAKE on the same word.
                    return unsafe {
                        sys::syscall(NR_FUTEX, a1, FUTEX_WAKE | (a2 & FUTEX_PRIVATE), n_wake.min(i32::MAX as i64), 0, 0, 0)
                    };
                }
            }
            // SAFETY: forwarded unchanged.
            unsafe { sys::syscall(n, a1, a2, a3, a4, a5, a6) }
        })
    }
}

// --- futex diagnostics (S1_FUTEXLOG) ------------------------------------------
//
// The watchdog prints, for every thread blocked in a futex wait, the word it
// waits on and what that word belongs to. ART's Mutex, ReaderWriterMutex and
// ConditionVariable all keep their futex word 16 bytes after the object's
// `name_` pointer (BaseMutex: vtable, name_, level_; ConditionVariable:
// name_, guard_, sequence_), so the name is read from there when it points
// at a readable string. S1_FUTEXLOG=1 also logs every requeue.

/// tid -> (word, expected value, op), while the thread is in a futex wait.
pub static FUTEX_WAITS: Mutex<Vec<(i32, usize, i32, i64)>> = Mutex::new(Vec::new());
/// Counters: waits, wakes, requeues, requeues that hit EAGAIN.
pub static FUTEX_STATS: [core::sync::atomic::AtomicU64; 4] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 4];

fn futex_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("S1_FUTEXLOG").is_some())
}

/// The futex waits in progress, for the watchdog.
pub fn dump_futex_waits(e: &mut impl std::io::Write) {
    use core::sync::atomic::Ordering::Relaxed;
    let _ = writeln!(
        e,
        "[s1] futex: {} waits, {} wakes, {} requeues ({} EAGAIN)",
        FUTEX_STATS[0].load(Relaxed),
        FUTEX_STATS[1].load(Relaxed),
        FUTEX_STATS[2].load(Relaxed),
        FUTEX_STATS[3].load(Relaxed)
    );
    let waits = FUTEX_WAITS.lock().unwrap().clone();
    for (tid, addr, val, op) in waits {
        // SAFETY: the word was a valid futex word when the wait started;
        // guest objects outlive their waiters.
        let cur = unsafe { core::ptr::read_volatile(addr as *const i32) };
        // SAFETY: as above; ART objects with a name keep it 16 bytes before.
        let owner = unsafe { core::ptr::read_volatile((addr + 4) as *const i32) };
        let name = crate::debug::guest_cstr_at(addr.wrapping_sub(16));
        let _ = writeln!(
            e,
            "[s1] futex wait: tid {tid} op {op} word {addr:#x} expects {val:#x} now {cur:#x} (+4: {owner}) {}",
            name.unwrap_or_default()
        );
    }
}

/// Defines plain forwards: same arguments, same result.
macro_rules! forward {
    ($($name:ident => $target:ident ( $($a:ident : $t:ty),* ) -> $r:ty;)*) => {$(
        gfn! {
            pub unsafe fn $name($($a: $t),*) -> $r {
                // SAFETY: arguments forwarded unchanged to lx, which has the
                // same contract.
                guard(|| unsafe { sys::$target($($a),*) })
            }
        }
    )*};
}

forward! {
    h_open => open(p: *const c_char, flags: i32, mode: u32) -> i32;
    h_openat => openat(d: i32, p: *const c_char, flags: i32, mode: u32) -> i32;
    h_pread => pread64(fd: i32, b: *mut c_void, n: usize, off: i64) -> isize;
    h_pwrite => pwrite64(fd: i32, b: *const c_void, n: usize, off: i64) -> isize;
    h_stat => stat(p: *const c_char, st: *mut c_void) -> i32;
    h_lstat => lstat(p: *const c_char, st: *mut c_void) -> i32;
    h_fstatat => fstatat(d: i32, p: *const c_char, st: *mut c_void, flags: i32) -> i32;
    h_munmap => munmap(a: *mut c_void, len: usize) -> i32;
    h_mprotect => mprotect(a: *mut c_void, len: usize, prot: i32) -> i32;
    h_madvise => madvise(a: *mut c_void, len: usize, advice: i32) -> i32;
    h_clock_gettime => clock_gettime(clk: i32, ts: *mut c_void) -> i32;
    h_clock_getres => clock_getres(clk: i32, ts: *mut c_void) -> i32;
    h_nanosleep => nanosleep(req: *const c_void, rem: *mut c_void) -> i32;
    h_clock_nanosleep => clock_nanosleep(clk: i32, flags: i32, req: *const c_void, rem: *mut c_void) -> i32;
    h_sched_yield => sched_yield() -> i32;
    h_getpid => getpid() -> i32;
    h_gettid => gettid() -> i32;
    h_errno_location => __errno() -> *mut i32;
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("open", h_open as usize),
        ("open64", h_open as usize),
        ("openat", h_openat as usize),
        ("openat64", h_openat as usize),
        ("close", h_close as usize),
        ("read", h_read as usize),
        ("write", h_write as usize),
        ("writev", h_writev as usize),
        ("pread", h_pread as usize),
        ("pread64", h_pread as usize),
        ("pwrite", h_pwrite as usize),
        ("pwrite64", h_pwrite as usize),
        ("lseek", h_lseek as usize),
        ("lseek64", h_lseek as usize),
        ("fstat", h_fstat as usize),
        ("fstat64", h_fstat as usize),
        ("stat", h_stat as usize),
        ("stat64", h_stat as usize),
        ("lstat", h_lstat as usize),
        ("lstat64", h_lstat as usize),
        ("fstatat", h_fstatat as usize),
        ("fstatat64", h_fstatat as usize),
        ("dup", h_dup as usize),
        ("dup2", h_dup2 as usize),
        ("dup3", h_dup3 as usize),
        ("fcntl", h_fcntl as usize),
        ("isatty", h_isatty as usize),
        ("mmap", h_mmap as usize),
        ("mmap64", h_mmap as usize),
        ("munmap", h_munmap as usize),
        ("mprotect", h_mprotect as usize),
        ("madvise", h_madvise as usize),
        ("syscall", h_syscall as usize),
        ("clock_gettime", h_clock_gettime as usize),
        ("clock_getres", h_clock_getres as usize),
        ("nanosleep", h_nanosleep as usize),
        ("clock_nanosleep", h_clock_nanosleep as usize),
        ("sched_yield", h_sched_yield as usize),
        ("getpid", h_getpid as usize),
        ("gettid", h_gettid as usize),
        ("__errno", h_errno_location as usize),
    ]
}

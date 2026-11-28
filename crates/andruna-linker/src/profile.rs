//! `ANDRUNA_PROFILE`: where a guest process spends its time.
//!
//! When the variable is set, the loader records
//!
//! - phase marks (milliseconds since Windows created the process: mount,
//!   loading, the entry point, exit),
//! - every system call that goes through [`crate::syscall::dispatch`]: count
//!   and time per number (from the dispatcher's entry to its return, so the
//!   entry stub and the stack switch are not included),
//! - the exceptions the first vectored handler saw (x64 lazy FS-base
//!   restores, emulated arm64 cache instructions, signals delivered by
//!   andruna-lx, others),
//! - the calls answered by the vDSO ([`crate::vdso`]),
//!
//! and prints a report on standard error when the process exits. When it
//! is not set, each record is one relaxed load of a flag.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

const NR: usize = 512;

static STATE: AtomicU8 = AtomicU8::new(0);
static COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
static TICKS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
static RESTORES: AtomicU64 = AtomicU64::new(0);
static SIGNALS: AtomicU64 = AtomicU64::new(0);
static EMULATED: AtomicU64 = AtomicU64::new(0);
static OTHER_EXCEPTIONS: AtomicU64 = AtomicU64::new(0);
static VDSO: AtomicU64 = AtomicU64::new(0);
static MARKS: std::sync::Mutex<Vec<(&'static str, f64)>> = std::sync::Mutex::new(Vec::new());

/// Whether profiling is on (`ANDRUNA_PROFILE` set).
#[inline]
pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("ANDRUNA_PROFILE").is_some();
            if STATE
                .compare_exchange(
                    0,
                    if on { 2 } else { 1 },
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
                && on
            {
                start_periodic();
            }
            on
        }
        s => s == 2,
    }
}

/// `ANDRUNA_PROFILE_EVERY=<seconds>`: also print the report periodically,
/// for processes that hang or die without a normal exit.
fn start_periodic() {
    let Some(secs) = std::env::var("ANDRUNA_PROFILE_EVERY")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s > 0)
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("andruna-profile".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(secs));
                report_now("periodic");
            }
        });
}

fn origin() -> Instant {
    static O: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *O.get_or_init(Instant::now)
}

/// Milliseconds since the process was created (Windows' creation time).
fn since_creation_ms() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the pseudo handle of this process and four valid out
    // pointers.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.is_err() {
        return 0.0;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    let v = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    v(now).saturating_sub(v(c)) as f64 / 10_000.0
}

/// Records a phase mark.
pub fn mark(name: &'static str) {
    if !enabled() {
        return;
    }
    let _ = origin();
    let t = since_creation_ms();
    MARKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((name, t));
}

/// Start of a timed system call: an `Instant` when profiling.
#[inline]
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

/// System calls that take a path as their second argument (`openat`,
/// `newfstatat`/`fstatat`, `faccessat`, `readlinkat`).
#[cfg(target_arch = "x86_64")]
const PATH_CALLS: [i64; 4] = [257, 262, 269, 267];
#[cfg(target_arch = "aarch64")]
const PATH_CALLS: [i64; 4] = [56, 79, 48, 78];

/// Memory calls whose first two arguments are an address and a length
/// (`mmap`, `mprotect`, `munmap`, `mincore`, `madvise`).
#[cfg(target_arch = "x86_64")]
const MEMORY_CALLS: [i64; 5] = [9, 10, 11, 27, 28];
#[cfg(target_arch = "aarch64")]
const MEMORY_CALLS: [i64; 5] = [222, 226, 215, 232, 233];

/// The slowest calls: (ns, number, path or address range argument).
static SLOWEST: std::sync::Mutex<Vec<(u64, i64, String)>> = std::sync::Mutex::new(Vec::new());

/// End of a timed system call `n` with arguments `a`, started at `t0`.
///
/// # Safety
///
/// For the calls in `PATH_CALLS`, `a[1]` is NULL or the NUL-terminated
/// path the call was made with (the system call's own contract).
#[inline]
pub unsafe fn syscall(n: i64, a: &[i64; 6], t0: Option<Instant>) {
    if let Some(t0) = t0 {
        let ns = t0.elapsed().as_nanos() as u64;
        let i = (n as usize).min(NR - 1);
        COUNT[i].fetch_add(1, Ordering::Relaxed);
        TICKS[i].fetch_add(ns, Ordering::Relaxed);
        if ns > 200_000 {
            let path = if PATH_CALLS.contains(&n) && a[1] != 0 {
                // SAFETY: the caller's path argument (contract).
                unsafe { core::ffi::CStr::from_ptr(a[1] as *const core::ffi::c_char) }
                    .to_string_lossy()
                    .into_owned()
            } else if MEMORY_CALLS.contains(&n) {
                format!("addr {:#x} len {:#x}", a[0], a[1])
            } else {
                String::new()
            };
            let mut v = SLOWEST.lock().unwrap_or_else(|e| e.into_inner());
            v.push((ns, n, path));
            v.sort_by_key(|x| std::cmp::Reverse(x.0));
            v.truncate(15);
        }
    }
}

/// An exception the first vectored handler saw: a lazy restore, a signal
/// andruna-lx delivered, or another one.
pub fn exception(kind: Exception) {
    if !enabled() {
        return;
    }
    match kind {
        Exception::Restore => &RESTORES,
        Exception::Signal => &SIGNALS,
        Exception::Emulated => &EMULATED,
        Exception::Other => &OTHER_EXCEPTIONS,
    }
    .fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug)]
pub enum Exception {
    Restore,
    /// An instruction the handler emulated (arm64 cache maintenance).
    Emulated,
    Signal,
    Other,
}

/// A call answered by the vDSO.
#[inline]
pub fn vdso_call() {
    if enabled() {
        VDSO.fetch_add(1, Ordering::Relaxed);
    }
}

/// Prints the report (at process exit).
pub fn report() {
    if !enabled() {
        return;
    }
    mark("exit");
    report_now("exit");
}

/// Prints the report now; `when` names the occasion.
fn report_now(when: &str) {
    let mut rows: Vec<(usize, u64, u64)> = (0..NR)
        .map(|i| {
            (
                i,
                COUNT[i].load(Ordering::Relaxed),
                TICKS[i].load(Ordering::Relaxed),
            )
        })
        .filter(|r| r.1 > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    let calls: u64 = rows.iter().map(|r| r.1).sum();
    let ns: u64 = rows.iter().map(|r| r.2).sum();
    let mut s = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(
        s,
        "[andruna-profile] report ({when}, {:.0} ms since process creation)",
        since_creation_ms()
    );
    let _ = writeln!(s, "[andruna-profile] phases (ms since process creation):");
    for (n, t) in MARKS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let _ = writeln!(s, "[andruna-profile]   {t:9.2}  {n}");
    }
    let _ = writeln!(
        s,
        "[andruna-profile] system calls: {calls}, {:.2} ms in the dispatcher; vDSO calls: {}",
        ns as f64 / 1e6,
        VDSO.load(Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "[andruna-profile] exceptions: {} lazy FS-base restores, {} emulated instructions, {} signals delivered, {} other",
        RESTORES.load(Ordering::Relaxed),
        EMULATED.load(Ordering::Relaxed),
        SIGNALS.load(Ordering::Relaxed),
        OTHER_EXCEPTIONS.load(Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "[andruna-profile]   nr      calls     total ms   mean us"
    );
    for (i, c, t) in rows.iter().take(25) {
        let _ = writeln!(
            s,
            "[andruna-profile]   {i:<5} {c:>8} {:>12.3} {:>9.2}",
            *t as f64 / 1e6,
            *t as f64 / 1e3 / *c as f64
        );
    }
    let _ = writeln!(s, "[andruna-profile] slowest calls (over 0.2 ms):");
    for (ns, n, path) in SLOWEST.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let _ = writeln!(
            s,
            "[andruna-profile]   {:>9.3} ms  nr {n} {path}",
            *ns as f64 / 1e6
        );
    }
    use std::io::Write as _;
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

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
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        s => s == 2,
    }
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

/// End of a timed system call `n` started at `t0`.
#[inline]
pub fn syscall(n: i64, t0: Option<Instant>) {
    if let Some(t0) = t0 {
        let i = (n as usize).min(NR - 1);
        COUNT[i].fetch_add(1, Ordering::Relaxed);
        TICKS[i].fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
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
    use std::io::Write as _;
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

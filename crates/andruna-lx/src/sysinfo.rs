//! System information: `uname`, `sethostname`, `sysinfo`, `getrusage`,
//! `getpriority`/`setpriority`, `getcpu`, `membarrier`, `mincore`, and the
//! `/proc` files built from the same values (`/proc/version`,
//! `/proc/loadavg`, `/proc/sys/kernel/*`).
//!
//! # Kernel identity (ADR 0039, proposed)
//!
//! `uname` reports `Linux` as the system name (the interface lx
//! implements), `6.19.0-andruna` as the release (the version of the Linux
//! UAPI headers in the guest's bionic, which is the interface lx is written
//! against, and Andruna's name for the implementation), and
//! `#1 SMP PREEMPT Andruna lx <version>` as the version. The node name is
//! `localhost`, as on Android, rather than the Windows computer name (which
//! often holds the user's name); `sethostname` fails with `EPERM`, as for
//! an app without `CAP_SYS_ADMIN`.
//!
//! # Deviations from Linux
//!
//! - Windows keeps no load average. The loads of `sysinfo` and
//!   `/proc/loadavg` are an estimate: the number of busy processors
//!   (system processor busy time over elapsed time, times the processor
//!   count), averaged over 1, 5 and 15 minutes from the samples taken when
//!   they are read (the first read starts from the average since boot).
//! - Counts of processes and threads describe this process only, as lx's
//!   `/proc` does: `sysinfo.procs` and the totals in `/proc/loadavg` are its
//!   thread count.
//! - `getrusage`: `ru_minflt` is every page fault of the process (Windows
//!   does not separate faults that needed I/O); the other counters Windows
//!   does not keep are 0. `RUSAGE_CHILDREN` is all zero (no child
//!   processes).
//! - Nice values are kept per thread and mapped onto the Windows thread
//!   priorities (-20 to -15 highest, -14 to -5 above normal, -4 to 4
//!   normal, 5 to 14 below normal, 15 to 19 lowest). Lowering a nice
//!   value below `20 - RLIMIT_NICE` fails with `EACCES`, as on Linux; the
//!   limit starts at 40, as Android's init sets it for apps (ADR 0039), so
//!   apps' threads can raise their priority for display work.
//! - `mincore` reports a page resident when it is in the process's working
//!   set.
//! - `pid_max` is Linux's 64-bit limit, 4194304. Windows process and
//!   thread ids are multiples of 4 that stay far below it in practice, but
//!   Windows does not promise a bound.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::errno::{Errno, SysResult};
use crate::linux::info::*;
use crate::sys::sysinfo as os;
use crate::util::lock;

/// The release `uname` reports (see the module documentation).
pub const RELEASE: &str = "6.19.0-andruna";
/// `/proc/sys/kernel/pid_max`.
pub const PID_MAX: u32 = 4_194_304;

fn version() -> String {
    format!("#1 SMP PREEMPT Andruna lx {}", env!("CARGO_PKG_VERSION"))
}

fn machine() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "aarch64"
    }
}

fn field(s: &str) -> [u8; UTS_LEN] {
    let mut f = [0u8; UTS_LEN];
    let n = s.len().min(UTS_LEN - 1);
    f[..n].copy_from_slice(&s.as_bytes()[..n]);
    f
}

/// `uname`.
pub fn uname() -> Utsname {
    Utsname {
        sysname: field("Linux"),
        nodename: field("localhost"),
        release: field(RELEASE),
        version: field(&version()),
        machine: field(machine()),
        domainname: field("(none)"),
    }
}

/// `sethostname` / `setdomainname`: only privileged processes may.
pub fn sethostname(len: usize) -> SysResult<()> {
    if len > UTS_LEN - 1 {
        return Err(Errno::EINVAL);
    }
    Err(Errno::EPERM)
}

/// `/proc/version`.
pub fn proc_version() -> Vec<u8> {
    format!(
        "Linux version {RELEASE} (andruna-lx {}) {}\n",
        env!("CARGO_PKG_VERSION"),
        version()
    )
    .into_bytes()
}

// ---- load average -------------------------------------------------------------

struct Load {
    /// Last sample: (time since boot, busy time), 100 ns units.
    last: Option<(u64, u64)>,
    /// 1, 5 and 15 minute averages.
    avg: [f64; 3],
}

static LOAD: Mutex<Load> = Mutex::new(Load {
    last: None,
    avg: [0.0; 3],
});

/// The 1, 5 and 15 minute load estimates (see the module documentation).
pub fn loadavg() -> [f64; 3] {
    let now = crate::sys::time::interrupt_time();
    let Some((_, busy)) = os::system_times() else {
        return [0.0; 3];
    };
    let cpus = crate::sys::thread::processor_count() as f64;
    let mut l = lock(&LOAD);
    match l.last {
        None => {
            // Average since boot.
            let busy_cpus = busy as f64 / now.max(1) as f64;
            l.avg = [busy_cpus.min(cpus); 3];
        }
        Some((t0, b0)) if now > t0 => {
            let dt = (now - t0) as f64;
            let busy_cpus = (busy.saturating_sub(b0) as f64 / dt).min(cpus);
            let secs = dt / 1e7;
            for (a, period) in l.avg.iter_mut().zip([60.0, 300.0, 900.0]) {
                let k = (-secs / period).exp();
                *a = *a * k + busy_cpus * (1.0 - k);
            }
        }
        Some(_) => {}
    }
    l.last = Some((now, busy));
    l.avg
}

/// `/proc/loadavg`.
pub fn proc_loadavg() -> Vec<u8> {
    let a = loadavg();
    let threads = crate::sys::thread::threads().len();
    let running = 1; // the reader
    format!(
        "{:.2} {:.2} {:.2} {running}/{threads} {}\n",
        a[0],
        a[1],
        a[2],
        crate::process::getpid()
    )
    .into_bytes()
}

// ---- sysinfo and getrusage ----------------------------------------------------

/// `sysinfo`.
pub fn sysinfo() -> Sysinfo {
    let m = os::memory_status();
    let loads = loadavg().map(|v| (v * f64::from(1u32 << SI_LOAD_SHIFT)) as u64);
    Sysinfo {
        uptime: (crate::sys::time::interrupt_time() / 10_000_000) as i64,
        loads,
        totalram: m.total_phys,
        freeram: m.avail_phys,
        sharedram: 0,
        bufferram: 0,
        // The page files: commit beyond physical memory.
        totalswap: m.total_commit.saturating_sub(m.total_phys),
        freeswap: m.avail_commit.saturating_sub(m.avail_phys),
        procs: crate::sys::thread::threads()
            .len()
            .min(usize::from(u16::MAX)) as u16,
        mem_unit: 1,
        ..Sysinfo::default()
    }
}

fn timeval(hundred_ns: u64) -> Timeval {
    let us = hundred_ns / 10;
    Timeval {
        tv_sec: (us / 1_000_000) as i64,
        tv_usec: (us % 1_000_000) as i64,
    }
}

/// `getrusage`.
pub fn getrusage(who: i32) -> SysResult<Rusage> {
    let times = match who {
        RUSAGE_SELF => crate::sys::thread::process_times()?,
        RUSAGE_THREAD => crate::sys::thread::thread_times(None)?,
        RUSAGE_CHILDREN => return Ok(Rusage::default()),
        _ => return Err(Errno::EINVAL),
    };
    let (peak, faults) = os::process_counters();
    Ok(Rusage {
        utime: timeval(times.user),
        stime: timeval(times.kernel),
        maxrss: (peak / 1024) as i64,
        minflt: if who == RUSAGE_SELF { faults as i64 } else { 0 },
        ..Rusage::default()
    })
}

// ---- priorities -----------------------------------------------------------

/// Nice values set per thread (Linux tid, with the thread's creation time
/// so that a reused id does not inherit them); others are 0.
static NICE: Mutex<Option<HashMap<i32, (u64, i32)>>> = Mutex::new(None);

fn created(t: i32) -> u64 {
    crate::sys::procinfo::thread(crate::process::windows_tid(t as u32))
        .map(|i| i.created)
        .unwrap_or(0)
}

fn windows_priority(nice: i32) -> i32 {
    match nice {
        ..=-15 => 2,   // THREAD_PRIORITY_HIGHEST
        -14..=-5 => 1, // THREAD_PRIORITY_ABOVE_NORMAL
        -4..=4 => 0,   // THREAD_PRIORITY_NORMAL
        5..=14 => -1,  // THREAD_PRIORITY_BELOW_NORMAL
        _ => -2,       // THREAD_PRIORITY_LOWEST
    }
}

/// The threads `which`/`who` name.
fn targets(which: i32, who: i32) -> SysResult<Vec<i32>> {
    let me = crate::process::gettid();
    let pid = crate::process::getpid();
    let uid = crate::process::uid() as i32;
    match which {
        PRIO_PROCESS => {
            let t = if who == 0 { me } else { who };
            if t == me || t == pid || crate::sys::thread::is_own_thread(t as u32) {
                Ok(vec![t])
            } else {
                Err(Errno::ESRCH)
            }
        }
        PRIO_PGRP if who == 0 || who == pid => all_threads(),
        PRIO_USER if who == 0 || who == uid => all_threads(),
        PRIO_PGRP | PRIO_USER => Err(Errno::ESRCH),
        _ => Err(Errno::EINVAL),
    }
}

fn all_threads() -> SysResult<Vec<i32>> {
    Ok(crate::sys::thread::threads()
        .into_iter()
        .map(|t| t as i32)
        .collect())
}

fn nice_of(t: i32) -> i32 {
    let Some((when, nice)) = lock(&NICE).as_ref().and_then(|m| m.get(&t).copied()) else {
        return 0;
    };
    if when == created(t) { nice } else { 0 }
}

/// `getpriority` as the system call returns it: `20 - nice` of the
/// highest-priority thread named.
pub fn getpriority(which: i32, who: i32) -> SysResult<i32> {
    let nice = targets(which, who)?
        .into_iter()
        .map(nice_of)
        .min()
        .ok_or(Errno::ESRCH)?;
    Ok(20 - nice)
}

/// `setpriority`.
pub fn setpriority(which: i32, who: i32, nice: i32) -> SysResult<()> {
    let nice = nice.clamp(-20, 19);
    let threads = targets(which, who)?;
    let limit = crate::rlimit::prlimit(0, crate::rlimit::RLIMIT_NICE, None)?.cur;
    let floor = 20 - limit.min(40) as i32;
    if nice < floor && threads.iter().any(|&t| nice < nice_of(t)) {
        return Err(Errno::EACCES);
    }
    for &t in &threads {
        os::set_thread_priority(
            crate::process::windows_tid(t as u32),
            windows_priority(nice),
        )?;
    }
    let stamps: Vec<(i32, u64)> = threads.into_iter().map(|t| (t, created(t))).collect();
    let mut g = lock(&NICE);
    let m = g.get_or_insert_with(HashMap::new);
    for (t, when) in stamps {
        if nice == 0 {
            m.remove(&t);
        } else {
            m.insert(t, (when, nice));
        }
    }
    Ok(())
}

// ---- getcpu, membarrier, mincore ----------------------------------------------

/// `getcpu`: (cpu, node). Windows' NUMA node of the processor is not
/// mapped; Android devices have one node, and so does lx.
pub fn getcpu() -> (u32, u32) {
    (os::current_processor(), 0)
}

/// `membarrier`. The private expedited commands are an inter-processor
/// interrupt to the processors running this process's threads
/// (`FlushProcessWriteBuffers`), which is what Linux guarantees for them.
/// The global commands would need the same for every process and are not
/// offered (`EINVAL`, and `QUERY` does not list them).
pub fn membarrier(cmd: i32, flags: u32) -> SysResult<i32> {
    if flags != 0 {
        return Err(Errno::EINVAL);
    }
    match cmd {
        MEMBARRIER_CMD_QUERY => Ok(MEMBARRIER_CMD_PRIVATE_EXPEDITED
            | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED
            | MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE
            | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE),
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED
        | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE => Ok(0),
        MEMBARRIER_CMD_PRIVATE_EXPEDITED | MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE => {
            os::flush_process_write_buffers();
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// `mincore`: one byte per page of `[addr, addr+len)`, bit 0 set when the
/// page is resident. `ENOMEM` if part of the range is not mapped.
pub fn mincore(addr: usize, len: usize) -> SysResult<Vec<u8>> {
    if addr % 4096 != 0 {
        return Err(Errno::EINVAL);
    }
    let pages = len.div_ceil(4096);
    let end = addr.checked_add(pages * 4096).ok_or(Errno::ENOMEM)?;
    if !crate::mem::is_range_mapped(addr, end) {
        return Err(Errno::ENOMEM);
    }
    Ok(os::resident(addr, pages)?
        .into_iter()
        .map(u8::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: &[u8; UTS_LEN]) -> &str {
        let n = f.iter().position(|&b| b == 0).unwrap();
        core::str::from_utf8(&f[..n]).unwrap()
    }

    #[test]
    fn identity() {
        let u = uname();
        assert_eq!(s(&u.sysname), "Linux");
        assert_eq!(s(&u.release), RELEASE);
        assert_eq!(s(&u.machine), machine());
        assert!(s(&u.version).starts_with("#1 SMP"));
        let v = String::from_utf8(proc_version()).unwrap();
        assert!(v.starts_with("Linux version 6.19.0-andruna "));
        assert!(v.ends_with('\n'));
        assert_eq!(sethostname(5), Err(Errno::EPERM));
        assert_eq!(sethostname(100), Err(Errno::EINVAL));
    }

    #[test]
    fn load_and_memory() {
        let i = sysinfo();
        let cpus = crate::sys::thread::processor_count() as u64;
        assert!(i.totalram > 0 && i.freeram <= i.totalram);
        assert!(i.uptime > 0 && i.procs >= 1);
        for l in i.loads {
            assert!(l <= cpus << SI_LOAD_SHIFT);
        }
        let text = String::from_utf8(proc_loadavg()).unwrap();
        let parts: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(parts.len(), 5);
        assert!(parts[0].parse::<f64>().is_ok());
        assert!(parts[3].starts_with("1/"));
    }

    #[test]
    fn rusage() {
        let r = getrusage(RUSAGE_SELF).unwrap();
        assert!(r.maxrss > 0 && r.minflt > 0);
        assert!(getrusage(RUSAGE_THREAD).is_ok());
        assert_eq!(getrusage(RUSAGE_CHILDREN), Ok(Rusage::default()));
        assert_eq!(getrusage(5), Err(Errno::EINVAL));
    }

    #[test]
    fn priorities() {
        std::thread::spawn(|| {
            assert_eq!(getpriority(PRIO_PROCESS, 0), Ok(20));
            setpriority(PRIO_PROCESS, 0, 10).unwrap();
            assert_eq!(getpriority(PRIO_PROCESS, 0), Ok(10));
            setpriority(PRIO_PROCESS, 0, -8).unwrap();
            assert_eq!(getpriority(PRIO_PROCESS, 0), Ok(28));
            setpriority(PRIO_PROCESS, 0, -100).unwrap();
            assert_eq!(getpriority(PRIO_PROCESS, 0), Ok(40));
            assert_eq!(getpriority(9, 0), Err(Errno::EINVAL));
            assert_eq!(getpriority(PRIO_PROCESS, 3), Err(Errno::ESRCH));
        })
        .join()
        .unwrap();
        assert_eq!(windows_priority(19), -2);
        assert_eq!(windows_priority(0), 0);
    }

    #[test]
    fn cpu_barrier_and_residency() {
        let (cpu, node) = getcpu();
        assert!((cpu as usize) < 64 * 64 && node == 0);
        assert!(
            membarrier(MEMBARRIER_CMD_QUERY, 0).unwrap() & MEMBARRIER_CMD_PRIVATE_EXPEDITED != 0
        );
        assert_eq!(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0), Ok(0));
        assert_eq!(membarrier(MEMBARRIER_CMD_GLOBAL, 0), Err(Errno::EINVAL));
        use crate::linux::mman::*;
        let a = crate::mem::mmap(
            0,
            3 * 4096,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
        .unwrap();
        // SAFETY: a fresh writable mapping.
        unsafe { (a as *mut u8).write_volatile(1) };
        let v = mincore(a, 3 * 4096).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], 1);
        assert_eq!(mincore(a + 1, 4096), Err(Errno::EINVAL));
        crate::mem::munmap(a, 3 * 4096).unwrap();
        assert_eq!(mincore(0x1000, 4096), Err(Errno::ENOMEM));
    }
}

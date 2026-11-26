//! Reports: load statistics and the boundary benchmark.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::loader::Linker;

#[cfg(target_arch = "x86_64")]
pub type GuestU64 = extern "sysv64" fn() -> u64;
#[cfg(not(target_arch = "x86_64"))]
pub type GuestU64 = extern "C" fn() -> u64;
#[cfg(target_arch = "x86_64")]
type GuestLoop = extern "sysv64" fn(u64) -> u64;
#[cfg(not(target_arch = "x86_64"))]
type GuestLoop = extern "C" fn(u64) -> u64;
#[cfg(target_arch = "x86_64")]
type GuestLoopStr = extern "sysv64" fn(u64, *const u8) -> u64;
#[cfg(not(target_arch = "x86_64"))]
type GuestLoopStr = extern "C" fn(u64, *const u8) -> u64;
#[cfg(target_arch = "x86_64")]
type GuestNop = extern "sysv64" fn() -> i32;
#[cfg(not(target_arch = "x86_64"))]
type GuestNop = extern "C" fn() -> i32;

fn quiet() -> bool {
    std::env::var_os("S8_QUIET").is_some()
}

pub fn loaded(l: &Linker, total: Duration) {
    if quiet() {
        return;
    }
    let s = &l.stats;
    let bytes: u64 = l.objects.iter().map(|o| o.bytes_read).sum();
    eprintln!(
        "[s8] loaded {} objects ({:.1} MB read) in {:.2} ms: map+parse {:.2} ms, relocate+protect {:.2} ms",
        l.objects.len(),
        bytes as f64 / 1e6,
        total.as_secs_f64() * 1e3,
        s.map.as_secs_f64() * 1e3,
        s.relocate.as_secs_f64() * 1e3
    );
    eprintln!(
        "[s8]   {} relocations ({} RELR, {} from APS2 packed), {} symbol lookups, {} bound to the host",
        s.relocations, s.relr_relocations, s.packed_relocations, s.symbol_lookups, s.host_bindings
    );
    for o in &l.objects {
        eprintln!(
            "[s8]   {:<24} at {:#x} ({} KiB) tls module {}",
            o.name,
            o.base,
            o.size / 1024,
            o.tls_module
        );
    }
    if !s.unresolved.is_empty() {
        eprintln!(
            "[s8]   unresolved (bound to ENOSYS stubs): {}",
            s.unresolved.join(" ")
        );
    }
}

pub fn initialized(l: &Linker) {
    if !quiet() {
        eprintln!(
            "[s8] initializers ran in {:.2} ms",
            l.stats.init.as_secs_f64() * 1e3
        );
    }
}

pub fn at_guest_exit(status: i32) {
    if !quiet() {
        #[cfg(target_arch = "x86_64")]
        eprintln!(
            "[s8] guest exit({status}); FS-base lazy restores on this thread: {}",
            crate::tls::restores_here()
        );
        #[cfg(not(target_arch = "x86_64"))]
        eprintln!("[s8] guest exit({status})");
    }
}

fn best<F: FnMut()>(mut f: F) -> Duration {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap()
}

#[inline(never)]
extern "C" fn host_empty() -> i32 {
    0
}

pub fn bench(path: Vec<PathBuf>) -> Result<i32, String> {
    let mut r = Ok(0);
    crate::host::threads::run_on_guest_stack(|| r = bench_inner(path));
    r
}

fn bench_inner(path: Vec<PathBuf>) -> Result<i32, String> {
    let mut linker = Linker::new(path);
    let p = linker
        .search
        .iter()
        .map(|d| d.join("libs8bench.so"))
        .find(|p| p.exists())
        .ok_or("libs8bench.so not found")?;
    let t = Instant::now();
    linker.load(&p)?;
    loaded(&linker, t.elapsed());
    let rec = crate::host::threads::new_record();
    let _area = crate::host::threads::enter_guest_thread(rec);
    let a: [*const u8; 2] = [c"bench".as_ptr() as *const u8, core::ptr::null()];
    let e: [*const u8; 1] = [core::ptr::null()];
    linker.run_initializers(1, a.as_ptr(), e.as_ptr());

    let sym = |n: &str| linker.lookup_global(n).ok_or(format!("missing {n}"));
    // SAFETY: functions of libs8bench.so with the declared signatures.
    let (lg, lh, lt, lc, ls, ltls, nop, guard): (
        GuestLoop,
        GuestLoop,
        GuestLoop,
        GuestLoop,
        GuestLoopStr,
        GuestLoop,
        GuestNop,
        GuestU64,
    ) = unsafe {
        (
            core::mem::transmute(sym("s8_loop_guest")?),
            core::mem::transmute(sym("s8_loop_host_nop")?),
            core::mem::transmute(sym("s8_loop_gettid")?),
            core::mem::transmute(sym("s8_loop_clock")?),
            core::mem::transmute(sym("s8_loop_strlen")?),
            core::mem::transmute(sym("s8_loop_tls")?),
            core::mem::transmute(sym("s8_guest_nop")?),
            core::mem::transmute(sym("s8_read_stack_guard")?),
        )
    };
    const N: u64 = 10_000_000;
    let per = |d: Duration| d.as_secs_f64() * 1e9 / N as f64;
    let rows: Vec<(&str, f64)> = vec![
        (
            "guest -> guest (indirect call, empty function)",
            per(best(|| {
                std::hint::black_box(lg(N));
            })),
        ),
        (
            "guest -> host, empty System V function",
            per(best(|| {
                std::hint::black_box(lh(N));
            })),
        ),
        (
            "guest -> host -> lx gettid (Windows ABI)",
            per(best(|| {
                std::hint::black_box(lt(N));
            })),
        ),
        (
            "guest -> host -> lx clock_gettime(MONOTONIC)",
            per(best(|| {
                std::hint::black_box(lc(N));
            })),
        ),
        (
            "guest -> guest libc strlen(11 bytes)",
            per(best(|| {
                std::hint::black_box(ls(N, c"hello world".as_ptr() as *const u8));
            })),
        ),
        (
            "guest TLS variable ++ (general dynamic)",
            per(best(|| {
                std::hint::black_box(ltls(N));
            })),
        ),
        (
            "host -> guest, empty function (System V call from Rust)",
            per(best(|| {
                for _ in 0..N {
                    std::hint::black_box(nop());
                }
            })),
        ),
        (
            "host -> host, empty function (Windows ABI, no boundary)",
            per(best(|| {
                let f: extern "C" fn() -> i32 = std::hint::black_box(host_empty);
                for _ in 0..N {
                    std::hint::black_box(f());
                }
            })),
        ),
        (
            "host -> lx gettid directly (Windows ABI, as option 1)",
            per(best(|| {
                for _ in 0..N {
                    std::hint::black_box(crate::host::lx::lx_gettid());
                }
            })),
        ),
    ];
    println!("| Call ({N} iterations, best of 5) | ns per call |");
    println!("| --- | ---: |");
    for (n, v) in &rows {
        println!("| {n} | {v:.2} |");
    }
    let g = guard();
    println!(
        "stack guard read through the thread pointer: {}",
        if g != 0 { "ok (non-zero)" } else { "ZERO" }
    );
    #[cfg(target_arch = "x86_64")]
    println!(
        "FS-base lazy restores on the bench thread: {}",
        crate::tls::restores_here()
    );
    Ok(0)
}

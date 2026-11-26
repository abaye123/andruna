//! Spike S8: AOSP code built as Android ELF (`<arch>-linux-android`),
//! loaded into a Windows process by a minimal ELF loader and linked to
//! andruna-lx through System V entry points.
//!
//! ```text
//! andruna-spike-s8 run   <exe> [args...]   load a guest PIE and run main()
//! andruna-spike-s8 bench                   boundary benchmark (libs8bench.so)
//! andruna-spike-s8 load  <so>              load only; print load statistics
//! andruna-spike-s8 crash                   fault inside guest code (debugger checks)
//! andruna-spike-s8 check                   host table vs. host_symbols.txt
//! ```
//!
//! Objects are searched in `S8_LIB_PATH` (`;`-separated), by default
//! `out/s8/<arch>/{aosp,sysroot/usr/lib,libcxx-install/lib}` of the
//! repository. The guest file tree `/` is `S8_ROOT` (default
//! `%TEMP%\andruna-s8-root`).

mod debug;
mod elf;
mod host;
mod loader;
mod report;
mod tls;
mod unwind;

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(target_arch = "x86_64")]
pub const ARCH_ELF: u16 = elf::EM_X86_64;
#[cfg(target_arch = "aarch64")]
pub const ARCH_ELF: u16 = elf::EM_AARCH64;

#[cfg(target_arch = "x86_64")]
type MainFn = extern "sysv64" fn(i32, *const *const u8, *const *const u8) -> i32;
#[cfg(not(target_arch = "x86_64"))]
type MainFn = extern "C" fn(i32, *const *const u8, *const *const u8) -> i32;
#[cfg(target_arch = "x86_64")]
type ExitFn = extern "sysv64" fn(i32);
#[cfg(not(target_arch = "x86_64"))]
type ExitFn = extern "C" fn(i32);

/// Objects preloaded by `run` (`S1_PRELOAD`, `;`-separated).
const DEFAULT_PRELOAD: &str =
    "libart.so;libopenjdkjvm.so;libicu_jni.so;libjavacore.so;libopenjdk.so";

fn arch_dir() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

fn lib_path() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("S8_LIB_PATH") {
        return p
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    let arch = std::env::var("S8_ARCH").unwrap_or_else(|_| arch_dir().to_string());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../out/s1art")
        .join(arch);
    vec![
        root.join("aosp"),
        root.join("sysroot/usr/lib"),
        root.join("libcxx-install/lib"),
    ]
}

/// Sets up lx: the guest file tree and the console descriptors.
fn init_lx() -> Result<(), String> {
    let root = std::env::var("S8_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("andruna-s1-root"));
    for d in ["dev", "tmp", "data/local/tmp", "system/bin", "proc"] {
        std::fs::create_dir_all(root.join(d)).map_err(|e| e.to_string())?;
    }
    std::fs::write(root.join("dev/null"), b"").map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
    let r = root.to_string_lossy();
    let r = r.strip_prefix(r"\\?\").unwrap_or(&r).to_string();
    andruna_lx::fs::mount::mount("/", Path::new(&r))
        .map_err(|e| format!("mount: errno {}", e.0))?;
    host::lx::init_std_fds()
}

struct Args {
    _store: Vec<CString>,
    argv: Vec<*const u8>,
    envp: Vec<*const u8>,
}

// SAFETY: the pointers refer to `_store`, owned by the same value.
unsafe impl Send for Args {}

fn make_args(args: &[String]) -> Args {
    let env = [
        "HOME=/data/local/tmp",
        "TMPDIR=/data/local/tmp",
        "ANDROID_DATA=/data",
        "ANDROID_ROOT=/system",
        "PATH=/system/bin",
        "ANDROID_ART_ROOT=/apex/com.android.art",
        "ANDROID_I18N_ROOT=/apex/com.android.i18n",
        "ANDROID_TZDATA_ROOT=/apex/com.android.tzdata",
    ];
    let mut store: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap())
        .collect();
    let n = store.len();
    store.extend(env.iter().map(|e| CString::new(*e).unwrap()));
    let mut argv: Vec<*const u8> = store[..n].iter().map(|c| c.as_ptr() as *const u8).collect();
    argv.push(core::ptr::null());
    let mut envp: Vec<*const u8> = store[n..].iter().map(|c| c.as_ptr() as *const u8).collect();
    envp.push(core::ptr::null());
    Args {
        _store: store,
        argv,
        envp,
    }
}

fn load(linker: &mut loader::Linker, name: &str) -> Result<usize, String> {
    let p = if Path::new(name).exists() {
        PathBuf::from(name)
    } else {
        linker
            .search
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.exists())
            .ok_or_else(|| format!("{name} not found"))?
    };
    let t = Instant::now();
    let r = linker.load(&p)?;
    report::loaded(linker, t.elapsed());
    Ok(r)
}

fn cmd_run(args: &[String]) -> Result<i32, String> {
    let exe = args.first().ok_or("run: missing executable")?.clone();
    let name = Path::new(&exe)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    *host::fs::EXE_PATH.lock().unwrap() = format!("/data/local/tmp/{name}").into_bytes();
    let args = args.to_vec();
    let mut code = Ok(0);
    host::threads::run_on_guest_stack(|| {
        code = (|| {
            let mut linker = loader::Linker::new(lib_path());
            // Objects the guest will dlopen (ART: libart.so and the libcore
            // natives) are loaded with the executable, so static TLS is laid
            // out once.
            let mut paths = vec![PathBuf::from(&exe)];
            let preload = std::env::var("S1_PRELOAD").unwrap_or_else(|_| DEFAULT_PRELOAD.into());
            for p in preload.split(';').filter(|p| !p.is_empty()) {
                match linker.find_library(p) {
                    Ok(path) => paths.push(path),
                    Err(_) => eprintln!("[s1] preload: {p} not found, skipped"),
                }
            }
            let t = Instant::now();
            linker.load_all(&paths)?;
            report::loaded(&linker, t.elapsed());
            // Debug aid: S1_PEEK=<object>:<hex vaddr> prints 6 words there.
            if let Ok(p) = std::env::var("S1_PEEK") {
                if let Some((o, a)) = p.split_once(':') {
                    let a = u64::from_str_radix(a.trim_start_matches("0x"), 16).unwrap_or(0);
                    if let Some(ob) = linker.objects.iter().find(|x| x.name == o) {
                        // SAFETY: debug read inside a mapped object.
                        let w = unsafe { core::slice::from_raw_parts((ob.bias + a) as *const u64, 6) };
                        eprintln!("[s1] peek {o}+{a:#x} (bias {:#x}): {w:x?}", ob.bias);
                    }
                }
            }
            host::dl::set_linker(&mut linker);
            let t = host::threads::new_record();
            let _area = host::threads::enter_guest_thread(t);
            let a = make_args(&args);
            linker.run_initializers((a.argv.len() - 1) as i32, a.argv.as_ptr(), a.envp.as_ptr());
            report::initialized(&linker);
            // There is no logd: liblog's messages go to stderr (its own
            // stderr logger), as on a host build.
            if let (Some(set), Some(stderr_logger)) = (
                linker.lookup_global("__android_log_set_logger"),
                linker.lookup_global("__android_log_stderr_logger"),
            ) {
                // SAFETY: liblog's function, System V convention.
                let set: extern "sysv64" fn(u64) = unsafe { core::mem::transmute(set) };
                set(stderr_logger);
            }
            let main = linker.lookup_global("main").ok_or("no exported main")?;
            let exit = linker
                .lookup_global("exit")
                .ok_or("guest libc has no exit")?;
            // SAFETY: the guest's main and exit, System V convention.
            let (main, exit): (MainFn, ExitFn) =
                unsafe { (core::mem::transmute(main), core::mem::transmute(exit)) };
            let rc = main((a.argv.len() - 1) as i32, a.argv.as_ptr(), a.envp.as_ptr());
            exit(rc);
            Ok(rc)
        })();
    });
    code
}

fn cmd_load(args: &[String]) -> Result<i32, String> {
    let so = args.first().ok_or("load: missing object")?.clone();
    let mut r = Ok(0);
    host::threads::run_on_guest_stack(|| {
        r = (|| {
            let mut linker = loader::Linker::new(lib_path());
            load(&mut linker, &so)?;
            let t = host::threads::new_record();
            let _area = host::threads::enter_guest_thread(t);
            let a = make_args(&[so.clone()]);
            linker.run_initializers(1, a.argv.as_ptr(), a.envp.as_ptr());
            report::initialized(&linker);
            Ok(0)
        })();
    });
    r
}

/// Maps and links an object and its dependencies without running any
/// guest code; with S8_LINK_ONLY=1 the objects may be of another
/// architecture (the arm64 build checked on x64).
fn cmd_link(args: &[String]) -> Result<i32, String> {
    let so = args.first().ok_or("link: missing object")?.clone();
    let mut linker = loader::Linker::new(lib_path());
    load(&mut linker, &so)?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
type CallHost = extern "sysv64" fn(extern "sysv64" fn(u64) -> u64, u64) -> u64;

/// Host callback reached through two guest frames: walks the stack with
/// the Windows unwinder (`RtlCaptureStackBackTrace`).
#[cfg(target_arch = "x86_64")]
extern "sysv64" fn capture(x: u64) -> u64 {
    host::guard(|| {
        let mut frames = [core::ptr::null_mut(); 24];
        // SAFETY: a buffer of 24 entries.
        let n = unsafe {
            windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace(
                0,
                &mut frames,
                None,
            )
        };
        println!("RtlCaptureStackBackTrace from a host callback: {n} frames");
        for (i, f) in frames[..n as usize].iter().enumerate() {
            let a = *f as u64;
            println!(
                "  #{i:<2} {a:#x} {}",
                debug::describe(a).unwrap_or_else(|| "(host or system)".into())
            );
        }
        println!("RtlLookupFunctionEntry + RtlVirtualUnwind from the same callback:");
        virtual_unwind_walk();
        x
    })
}

/// Walks the current stack with RtlLookupFunctionEntry and
/// RtlVirtualUnwind, as exception dispatch and debuggers do.
#[cfg(target_arch = "x86_64")]
fn virtual_unwind_walk() {
    use windows::Win32::System::Diagnostics::Debug::{
        CONTEXT, RtlCaptureContext, RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER,
    };
    // CONTEXT must be 16-byte aligned (RtlCaptureContext uses fxsave).
    #[repr(C, align(16))]
    struct Aligned(CONTEXT);
    let mut a = Aligned(CONTEXT::default());
    let ctx = &mut a.0;
    // SAFETY: captures this thread's registers into an aligned record.
    unsafe { RtlCaptureContext(ctx) };
    for i in 0..32 {
        let mut base = 0u64;
        // SAFETY: looks up the function table for ctx.Rip.
        let f = unsafe { RtlLookupFunctionEntry(ctx.Rip, &mut base, None) };
        let what = debug::describe(ctx.Rip).unwrap_or_else(|| "(host or system)".into());
        println!(
            "  #{i:<2} {:#x} {what}{}",
            ctx.Rip,
            if f.is_null() {
                "  [no unwind entry: leaf]"
            } else {
                ""
            }
        );
        if f.is_null() {
            // A leaf function: the return address is at [rsp].
            // SAFETY: rsp points into this thread's stack.
            ctx.Rip = unsafe { *(ctx.Rsp as *const u64) };
            ctx.Rsp += 8;
        } else {
            let mut data = core::ptr::null_mut();
            let mut frame = 0u64;
            // SAFETY: a function entry for ctx.Rip and a valid context.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    base,
                    ctx.Rip,
                    f,
                    ctx,
                    &mut data,
                    &mut frame,
                    None,
                )
            };
        }
        if ctx.Rip == 0 {
            break;
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn cmd_unwind() -> Result<i32, String> {
    let mut r = Ok(0);
    host::threads::run_on_guest_stack(|| {
        r = (|| {
            let mut linker = loader::Linker::new(lib_path());
            load(&mut linker, "libs8bench.so")?;
            let t = host::threads::new_record();
            let _area = host::threads::enter_guest_thread(t);
            let f = linker
                .lookup_global("s8_call_host_outer")
                .ok_or("no s8_call_host_outer")?;
            // SAFETY: the guest function, System V convention.
            let f: CallHost = unsafe { core::mem::transmute(f) };
            let v = f(capture, 5);
            println!(
                "guest returned {v} (expected {})",
                ((6 + 6 * 3 + (6 ^ 0x55) + 6 + 7) * 2)
            );
            Ok(0)
        })();
    });
    r
}

#[cfg(not(target_arch = "x86_64"))]
fn cmd_unwind() -> Result<i32, String> {
    Err("unwind check is x64 only".into())
}

/// For comparison: the time `LoadLibraryW` takes for Windows DLLs.
fn cmd_winload(args: &[String]) -> Result<i32, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    for a in args {
        let w: Vec<u16> = std::ffi::OsStr::new(a).encode_wide().chain([0]).collect();
        let t = Instant::now();
        // SAFETY: a NUL-terminated path; the module stays loaded.
        let h = unsafe { LoadLibraryW(windows::core::PCWSTR(w.as_ptr())) };
        let d = t.elapsed();
        let size = std::fs::metadata(a).map(|m| m.len()).unwrap_or(0);
        println!(
            "LoadLibraryW {a} ({:.1} MB): {:.2} ms {}",
            size as f64 / 1e6,
            d.as_secs_f64() * 1e3,
            if h.is_ok() { "ok" } else { "FAILED" }
        );
    }
    Ok(0)
}

fn cmd_crash() -> Result<i32, String> {
    let mut r = Ok(0);
    host::threads::run_on_guest_stack(|| {
        r = (|| {
            let mut linker = loader::Linker::new(lib_path());
            load(&mut linker, "libs8bench.so")?;
            let t = host::threads::new_record();
            let _area = host::threads::enter_guest_thread(t);
            let a = make_args(&["crash".into()]);
            linker.run_initializers(1, a.argv.as_ptr(), a.envp.as_ptr());
            let f = linker.lookup_global("s8_crash").ok_or("no s8_crash")?;
            // SAFETY: the guest function, System V convention.
            let f: report::GuestU64 = unsafe { core::mem::transmute(f) };
            eprintln!("[s8] calling s8_crash at {f:p}");
            Ok(f() as i32)
        })();
    });
    r
}

/// Cost of reaching a Thread field the Android way (one segment-relative
/// load, `%gs:off`) and the ANDRUNA way (load the Thread* from the TLS slot
/// at `%fs:0x38`, then `off(reg)`), on a guest thread. The single load is
/// measured as `%fs:off`: the same instruction form with another segment.
#[cfg(target_arch = "x86_64")]
fn cmd_tlscost() -> Result<i32, String> {
    host::threads::run_on_guest_stack(|| {
        // Any object will do: the TLS layout needs one.
        let mut linker = loader::Linker::new(lib_path());
        load(&mut linker, "libc.so").expect("libc.so");
        let t = host::threads::new_record();
        let _area = host::threads::enter_guest_thread(t);
        let tp = tls::current_tp();
        // A fake Thread: 4 KiB, field at offset 0x100 holds 1.
        let thread = Box::leak(vec![0u64; 512].into_boxed_slice());
        thread[0x100 / 8] = 1;
        // SAFETY: slot 7 of this thread's TCB (bionic TLS_SLOT_ART_THREAD_SELF).
        let saved = unsafe { *((tp + 0x38) as *const u64) };
        unsafe { *((tp + 0x38) as *mut u64) = thread.as_ptr() as u64 };
        let n: u64 = 200_000_000;
        let mut best = [f64::MAX; 3];
        for _ in 0..5 {
            let t0 = Instant::now();
            let mut acc: u64 = 0;
            // SAFETY: reads %fs:0x100 of the TCB area (mapped) in a loop.
            unsafe {
                core::arch::asm!(
                    "2:", "add {acc}, qword ptr fs:[0x38]", "dec {i}", "jnz 2b",
                    acc = inout(reg) acc, i = inout(reg) n => _, options(nostack));
            }
            best[0] = best[0].min(t0.elapsed().as_secs_f64() * 1e9 / n as f64);
            let t0 = Instant::now();
            // SAFETY: loads the Thread* from the slot, then a field of it.
            unsafe {
                core::arch::asm!(
                    "2:", "mov r11, qword ptr fs:[0x38]", "add {acc}, qword ptr [r11 + 0x100]",
                    "dec {i}", "jnz 2b",
                    acc = inout(reg) acc, i = inout(reg) n => _, out("r11") _, options(nostack));
            }
            best[1] = best[1].min(t0.elapsed().as_secs_f64() * 1e9 / n as f64);
            let t0 = Instant::now();
            // SAFETY: as above, store form (what top_quick_frame updates do).
            unsafe {
                core::arch::asm!(
                    "2:", "mov r11, qword ptr fs:[0x38]", "mov qword ptr [r11 + 0x108], {i}",
                    "dec {i}", "jnz 2b",
                    i = inout(reg) n => _, out("r11") _, options(nostack));
            }
            best[2] = best[2].min(t0.elapsed().as_secs_f64() * 1e9 / n as f64);
            std::hint::black_box(acc);
        }
        println!("segment load fs:[off] (stands for Android's gs:[off]): {:.3} ns", best[0]);
        println!("fs:[0x38] then [reg+off] (ANDRUNA):                    {:.3} ns", best[1]);
        println!("fs:[0x38] then store [reg+off]:                         {:.3} ns", best[2]);
        println!("FS-base lazy restores on this thread: {}", tls::restores_here());
        // SAFETY: restores the slot.
        unsafe { *((tp + 0x38) as *mut u64) = saved };
    });
    Ok(0)
}

#[cfg(not(target_arch = "x86_64"))]
fn cmd_tlscost() -> Result<i32, String> {
    Err("x64 only".into())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_arch = "x86_64")]
    debug::start_watchdog();
    if let Err(e) = init_lx() {
        eprintln!("[s8] lx set-up failed: {e}");
        std::process::exit(2);
    }
    let r = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("load") => cmd_load(&args[1..]),
        Some("bench") => report::bench(lib_path()),
        Some("crash") => cmd_crash(),
        Some("link") => cmd_link(&args[1..]),
        Some("unwind") => cmd_unwind(),
        Some("winload") => cmd_winload(&args[1..]),
        Some("tlscost") => cmd_tlscost(),
        Some("check") => {
            let m = host::missing_from_table();
            println!("host_symbols.txt names missing from the host table: {m:?}");
            Ok(m.len() as i32)
        }
        _ => {
            eprintln!(
                "usage: andruna-spike-s8 run <exe> [args] | bench | load <so> | crash | check"
            );
            Ok(2)
        }
    };
    match r {
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("[s8] error: {e}");
            std::process::exit(1)
        }
    }
}

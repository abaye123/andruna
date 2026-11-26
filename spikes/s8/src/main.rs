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

fn arch_dir() -> &'static str {
    if cfg!(target_arch = "x86_64") { "x64" } else { "arm64" }
}

fn lib_path() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("S8_LIB_PATH") {
        return p.split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/s8").join(arch_dir());
    vec![root.join("aosp"), root.join("sysroot/usr/lib"), root.join("libcxx-install/lib")]
}

/// Sets up lx: the guest file tree and the console descriptors.
fn init_lx() -> Result<(), String> {
    let root = std::env::var("S8_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("andruna-s8-root"));
    for d in ["dev", "tmp", "data/local/tmp", "system/bin", "proc"] {
        std::fs::create_dir_all(root.join(d)).map_err(|e| e.to_string())?;
    }
    std::fs::write(root.join("dev/null"), b"").map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
    let r = root.to_string_lossy();
    let r = r.strip_prefix(r"\\?\").unwrap_or(&r).to_string();
    andruna_lx::fs::mount::mount("/", Path::new(&r)).map_err(|e| format!("mount: errno {}", e.0))?;
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
    ];
    let mut store: Vec<CString> = args.iter().map(|a| CString::new(a.as_str()).unwrap()).collect();
    let n = store.len();
    store.extend(env.iter().map(|e| CString::new(*e).unwrap()));
    let mut argv: Vec<*const u8> = store[..n].iter().map(|c| c.as_ptr() as *const u8).collect();
    argv.push(core::ptr::null());
    let mut envp: Vec<*const u8> = store[n..].iter().map(|c| c.as_ptr() as *const u8).collect();
    envp.push(core::ptr::null());
    Args { _store: store, argv, envp }
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
    let name = Path::new(&exe).file_name().unwrap().to_string_lossy().into_owned();
    *host::fs::EXE_PATH.lock().unwrap() = format!("/data/local/tmp/{name}").into_bytes();
    let args = args.to_vec();
    let mut code = Ok(0);
    host::threads::run_on_guest_stack(|| {
        code = (|| {
            let mut linker = loader::Linker::new(lib_path());
            load(&mut linker, &exe)?;
            let t = host::threads::new_record();
            let _area = host::threads::enter_guest_thread(t);
            let a = make_args(&args);
            linker.run_initializers((a.argv.len() - 1) as i32, a.argv.as_ptr(), a.envp.as_ptr());
            report::initialized(&linker);
            let main = linker.lookup_global("main").ok_or("no exported main")?;
            let exit = linker.lookup_global("exit").ok_or("guest libc has no exit")?;
            // SAFETY: the guest's main and exit, System V convention.
            let (main, exit): (MainFn, ExitFn) = unsafe { (core::mem::transmute(main), core::mem::transmute(exit)) };
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = init_lx() {
        eprintln!("[s8] lx set-up failed: {e}");
        std::process::exit(2);
    }
    let r = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("load") => cmd_load(&args[1..]),
        Some("bench") => report::bench(lib_path()),
        Some("crash") => cmd_crash(),
        Some("check") => {
            let m = host::missing_from_table();
            println!("host_symbols.txt names missing from the host table: {m:?}");
            Ok(m.len() as i32)
        }
        _ => {
            eprintln!("usage: andruna-spike-s8 run <exe> [args] | bench | load <so> | crash | check");
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

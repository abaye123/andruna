//! `andruna-elf`: runs or links Android ELF programs with andruna-linker.
//!
//! ```text
//! andruna-elf run  [options] <exe> [args...]  run a guest PIE
//! andruna-elf link [options] <object>         map and relocate only; with
//!                                             --arch of another machine
//!                                             nothing runs (link-only)
//! andruna-elf check                           host table vs host_symbols.txt
//!
//! options:
//!   -L <dir>        add a library directory (repeatable; also
//!                   ANDRUNA_LIB_PATH, `;`-separated)
//!   --root <dir>    Windows directory mounted as the guest `/`
//!                   (default %TEMP%\andruna-elf-root)
//!   --arch x64|arm64  machine of the objects (link only)
//!   --stubs         bind undefined symbols to ENOSYS stubs (diagnostics)
//! ```

use std::path::{Path, PathBuf};

use andruna_linker::elf::{EM_AARCH64, EM_X86_64};
use andruna_linker::linker::rtld;
use andruna_linker::process::{Program, run};
use andruna_linker::{Config, Linker};

struct Opts {
    config: Config,
    root: Option<PathBuf>,
    rest: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut config = Config::default();
    if let Ok(p) = std::env::var("ANDRUNA_LIB_PATH") {
        config
            .search
            .extend(p.split(';').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    let mut root = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-L" => {
                i += 1;
                config
                    .search
                    .push(args.get(i).ok_or("-L needs a directory")?.into());
            }
            "--root" => {
                i += 1;
                root = Some(args.get(i).ok_or("--root needs a directory")?.into());
            }
            "--arch" => {
                i += 1;
                config.machine = match args.get(i).map(String::as_str) {
                    Some("x64") => EM_X86_64,
                    Some("arm64") => EM_AARCH64,
                    _ => return Err("--arch is x64 or arm64".into()),
                };
            }
            "--stubs" => config.unresolved_stubs = true,
            _ => break,
        }
        i += 1;
    }
    Ok(Opts {
        config,
        root,
        rest: args[i..].to_vec(),
    })
}

/// Mounts the guest root and gives the program the standard streams.
fn init_lx(root: Option<PathBuf>) -> Result<(), String> {
    let root = root.unwrap_or_else(|| std::env::temp_dir().join("andruna-elf-root"));
    for d in ["dev", "tmp", "data/local/tmp", "system/bin", "proc"] {
        std::fs::create_dir_all(root.join(d)).map_err(|e| e.to_string())?;
    }
    let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
    let r = root.to_string_lossy();
    let r = r.strip_prefix(r"\\?\").unwrap_or(&r).to_string();
    andruna_lx::fs::mount::mount("/", Path::new(&r))
        .map_err(|e| format!("mount: errno {}", e.0))?;
    andruna_lx::fs::stream::install_std_streams().map_err(|e| format!("std streams: errno {}", e.0))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("link") => cmd_link(&args[1..]),
        Some("check") => {
            let m = andruna_linker::host::missing_from_table();
            if m.is_empty() {
                println!("host table covers host_symbols.txt");
                Ok(0)
            } else {
                Err(format!("missing host symbols: {m:?}"))
            }
        }
        _ => Err("usage: andruna-elf run|link|check ... (see the crate README)".into()),
    };
    match code {
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("andruna-elf: {e}");
            std::process::exit(2)
        }
    }
}

fn cmd_run(args: &[String]) -> Result<i32, String> {
    let o = parse(args)?;
    let exe = o.rest.first().ok_or("run: missing executable")?.clone();
    init_lx(o.root)?;
    let name = Path::new(&exe)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut argv = vec![format!("/data/local/tmp/{name}")];
    argv.extend(o.rest[1..].iter().cloned());
    let env = [
        "HOME=/data/local/tmp",
        "TMPDIR=/data/local/tmp",
        "ANDROID_DATA=/data",
        "ANDROID_ROOT=/system",
        "PATH=/system/bin",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(
        std::env::var("ANDRUNA_GUEST_ENV")
            .ok()
            .into_iter()
            .flat_map(|v| {
                v.split(';')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            }),
    )
    .collect();
    let _ = andruna_lx::fs::set_cwd("/data/local/tmp");
    match run(Program {
        config: o.config,
        exe,
        args: argv,
        env,
    }) {
        Ok(n) => match n {},
        Err(e) => Err(e.to_string()),
    }
}

fn cmd_link(args: &[String]) -> Result<i32, String> {
    let o = parse(args)?;
    let obj = o.rest.first().ok_or("link: missing object")?.clone();
    let mut l = Linker::new(o.config);
    let t = std::time::Instant::now();
    // Not the main program: no bionic start-up, no guest code runs unless
    // initializers are run (they are not).
    let r = l.load(&obj, None, rtld::NOW).map_err(|e| e.to_string())?;
    let s = &l.stats;
    println!(
        "linked {} objects in {:.2} ms: {} relocations ({} RELR, {} packed, {} TLSDESC), {} lookups, {} host bindings, {} initializers not run",
        s.objects,
        t.elapsed().as_secs_f64() * 1000.0,
        s.relocations,
        s.relr,
        s.packed,
        s.tlsdesc,
        s.lookups,
        s.host_bindings,
        r.init_order.len()
    );
    for ob in l.objects() {
        println!("  {:#014x} {} ({})", ob.image.base, ob.name, ob.path);
    }
    Ok(0)
}

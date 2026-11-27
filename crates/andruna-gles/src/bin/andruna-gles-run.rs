//! `andruna-gles-run`: runs an Android ELF program with andruna-linker and
//! the OpenGL ES and EGL host functions of andruna-gles (`andruna-elf run`
//! plus `__andruna_gles_resolve`).
//!
//! ```text
//! andruna-gles-run [-L <dir>]... [--root <dir>] <exe> [args...]
//! ```
//!
//! ANGLE's DLLs come from `ANDRUNA_ANGLE_DIR` (or next to this executable);
//! `ANDRUNA_GLES_DEVICE=warp` selects the software rasterizer;
//! `ANDRUNA_LIB_PATH` (`;`-separated) adds library directories.

use std::path::{Path, PathBuf};

use andruna_linker::Config;
use andruna_linker::process::{Program, run};

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

fn main_inner() -> Result<i32, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
                root = Some(PathBuf::from(
                    args.get(i).ok_or("--root needs a directory")?,
                ));
            }
            _ => break,
        }
        i += 1;
    }
    let rest = &args[i..];
    let exe = rest
        .first()
        .ok_or("usage: andruna-gles-run [-L <dir>]... [--root <dir>] <exe> [args...]")?
        .clone();
    init_lx(root)?;
    andruna_gles::register();
    let name = Path::new(&exe)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut argv = vec![format!("/data/local/tmp/{name}")];
    argv.extend(rest[1..].iter().cloned());
    let env = [
        "HOME=/data/local/tmp",
        "TMPDIR=/data/local/tmp",
        "ANDROID_DATA=/data",
        "ANDROID_ROOT=/system",
        "PATH=/system/bin",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let _ = andruna_lx::fs::set_cwd("/data/local/tmp");
    match run(Program {
        config,
        exe,
        args: argv,
        env,
    }) {
        Ok(n) => match n {},
        Err(e) => Err(e.to_string()),
    }
}

fn main() {
    match main_inner() {
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("andruna-gles-run: {e}");
            std::process::exit(2)
        }
    }
}

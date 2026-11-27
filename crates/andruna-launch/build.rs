//! Embeds the application manifest and version information
//! (`res/andruna-launch.rc`) with llvm-mingw's windres.
//!
//! The manifest selects Common Controls 6, which provides the task dialog
//! used for errors. Without it (windres not found), the launcher still
//! works and reports errors with a message box; the build prints a warning.

use std::path::PathBuf;
use std::process::Command;

fn windres(arch: &str) -> Option<PathBuf> {
    let tool = format!("{arch}-w64-mingw32-windres.exe");
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("ANDRUNA_LLVM_MINGW") {
        dirs.push(PathBuf::from(d).join("bin"));
    }
    dirs.push(PathBuf::from(r"C:\tools\llvm-mingw\bin"));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter()
        .map(|d| d.join(&tool))
        .find(|p| p.is_file())
}

fn main() {
    println!("cargo:rerun-if-changed=res/andruna-launch.rc");
    println!("cargo:rerun-if-changed=res/andruna-launch.manifest");
    println!("cargo:rerun-if-env-changed=ANDRUNA_LLVM_MINGW");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "gnu" {
        println!("cargo:warning=resources are only embedded for the gnullvm targets");
        return;
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let Some(tool) = windres(&arch) else {
        println!(
            "cargo:warning={arch}-w64-mingw32-windres not found (set ANDRUNA_LLVM_MINGW); \
             building without manifest"
        );
        return;
    };
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let res = manifest_dir.join("res");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("andruna-launch-res.o");
    let status = Command::new(&tool)
        .arg("--include-dir")
        .arg(&res)
        .arg("--input")
        .arg(res.join("andruna-launch.rc"))
        .arg("--output")
        .arg(&out)
        .arg("--output-format=coff")
        .status()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", tool.display()));
    assert!(status.success(), "{} failed: {status}", tool.display());
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

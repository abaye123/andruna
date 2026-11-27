//! `cargo xtask aosp-build`: configure and build `aosp/` with CMake and
//! Ninja, using llvm-mingw, for one architecture.
//!
//! The AOSP sources must have been fetched (`cargo xtask fetch`). Patches in
//! `patches/` are applied first (already applied ones are skipped, and so
//! are the patches of projects that are not fetched). The
//! build directory is `out/aosp/<arch>`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::aosp;
use crate::build_cmd::Arch;
use crate::toolchain;
use crate::util::{self, Error, Result};

pub struct Options {
    pub arch: Arch,
    /// Also compile libart (`ANDRUNA_AOSP_RUNTIME`).
    pub runtime: bool,
    /// Ninja targets; empty builds the default set.
    pub targets: Vec<String>,
    /// Keep going after errors (`ninja -k 0`).
    pub keep_going: bool,
}

impl Arch {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

pub fn build(opts: &Options) -> Result<()> {
    let root = util::repo_root();
    let llvm = toolchain::llvm_mingw_dir(None);
    if !llvm.join("bin").join(exe("clang")).is_file() {
        return Err(Error::msg(format!(
            "llvm-mingw not found in {}; run `cargo xtask toolchain` or set {}",
            llvm.display(),
            toolchain::ENV_VAR
        )));
    }
    let aosp_dir = root.join("third_party").join("aosp");
    if !aosp_dir.join("bionic").join(".git").exists() {
        return Err(Error::msg(
            "AOSP sources are missing; run `cargo xtask fetch`",
        ));
    }
    let cmake = find_tool("cmake", "ANDRUNA_CMAKE", &cmake_candidates())?;
    let ninja = find_tool("ninja", "ANDRUNA_NINJA", &ninja_candidates())?;

    // Projects that are not fetched are skipped: the build cannot use them.
    aosp::patch(
        &aosp::Selection::defaults(),
        &root.join("patches"),
        false,
        true,
    )?;

    let build_dir = root.join("out").join("aosp").join(opts.arch.name());
    let started = Instant::now();
    let mut configure = Command::new(&cmake);
    configure
        .arg("-G")
        .arg("Ninja")
        .arg("-S")
        .arg(root.join("aosp"))
        .arg("-B")
        .arg(&build_dir)
        .arg(define("CMAKE_MAKE_PROGRAM", &ninja))
        .arg(define(
            "CMAKE_TOOLCHAIN_FILE",
            &root
                .join("aosp")
                .join("cmake")
                .join("toolchain-llvm-mingw.cmake"),
        ))
        .arg(define("ANDRUNA_LLVM_MINGW", &llvm))
        .arg(format!("-DANDRUNA_ARCH={}", opts.arch.name()))
        .arg(define("ANDRUNA_AOSP_DIR", &aosp_dir))
        .arg(format!(
            "-DANDRUNA_AOSP_RUNTIME={}",
            if opts.runtime { "ON" } else { "OFF" }
        ));
    println!("configuring {}", build_dir.display());
    util::run(&mut configure)?;

    let mut ninja_cmd = Command::new(&ninja);
    ninja_cmd.arg("-C").arg(&build_dir);
    if opts.keep_going {
        ninja_cmd.args(["-k", "0"]);
    }
    ninja_cmd.args(&opts.targets);
    util::run(&mut ninja_cmd)?;
    println!(
        "AOSP build for {} finished in {:.1} s",
        opts.arch.name(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

pub(crate) fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// A `-D<name>=<path>` argument with forward slashes, which CMake expects.
pub(crate) fn define(name: &str, path: &Path) -> String {
    format!("-D{name}={}", path.display().to_string().replace('\\', "/"))
}

/// Finds a tool: the environment variable, then PATH, then known install
/// locations.
pub(crate) fn find_tool(name: &str, env: &str, candidates: &[PathBuf]) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(env) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(Error::msg(format!(
            "{env} is set to {}, which is not a file",
            p.display()
        )));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(exe(name));
            if p.is_file() {
                return Ok(resolve_link(&p));
            }
        }
    }
    for p in candidates {
        if p.is_file() {
            return Ok(resolve_link(p));
        }
    }
    Err(Error::msg(format!(
        "{name} not found; install it (for example `winget install Kitware.CMake Ninja-build.Ninja`), \
         put it on PATH or set {env}"
    )))
}

/// The target of a symbolic link (WinGet installs tools as links), so that
/// CMake records the real executable.
fn resolve_link(p: &Path) -> PathBuf {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(p) {
            Ok(t) if t.is_absolute() => t,
            Ok(t) => p.parent().map(|d| d.join(&t)).unwrap_or(t),
            Err(_) => p.to_path_buf(),
        },
        _ => p.to_path_buf(),
    }
}

pub(crate) fn cmake_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(pf) = std::env::var_os(var) {
            v.push(
                PathBuf::from(pf)
                    .join("CMake")
                    .join("bin")
                    .join("cmake.exe"),
            );
        }
    }
    v
}

pub(crate) fn ninja_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let winget = PathBuf::from(local).join("Microsoft").join("WinGet");
        // The WinGet link directory holds a symbolic link, which is_file follows.
        v.push(winget.join("Links").join("ninja.exe"));
        v.push(
            winget
                .join("Packages")
                .join("Ninja-build.Ninja_Microsoft.Winget.Source_8wekyb3d8bbwe")
                .join("ninja.exe"),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_use_forward_slashes() {
        assert_eq!(define("X", Path::new(r"C:\a\b")), "-DX=C:/a/b".to_string());
    }
}

//! `cargo xtask angle-build`: ANGLE (AOSP `external/angle`) as Windows DLLs.
//!
//! Configures and builds `aosp/angle` (CMake glue that reads ANGLE's `.gni`
//! source lists, ADR 0029) with llvm-mingw and Ninja into
//! `out/angle/<arch>/build`, then copies `libEGL.dll` and `libGLESv2.dll`
//! to `out/angle/<arch>/bin`. The DLLs ship, so a release build signs them
//! with `cargo xtask sign out/angle/<arch>/bin/libEGL.dll
//! out/angle/<arch>/bin/libGLESv2.dll`.
//!
//! The build uses `CMAKE_BUILD_PARALLEL_LEVEL` jobs, 4 by default: ANGLE's
//! translator and renderer sources need about 1 GB per compiler process.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use crate::aosp;
use crate::aosp_build::{cmake_candidates, define, exe, find_tool, ninja_candidates};
use crate::build_cmd::Arch;
use crate::toolchain;
use crate::util::{self, Error, Result};

/// The DLLs the build produces.
pub const DLLS: &[&str] = &["libEGL.dll", "libGLESv2.dll"];

pub struct Options {
    pub arch: Arch,
    pub debug: bool,
}

fn jobs() -> String {
    std::env::var("CMAKE_BUILD_PARALLEL_LEVEL")
        .ok()
        .filter(|j| j.parse::<u32>().is_ok_and(|n| n > 0))
        .unwrap_or_else(|| "4".into())
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
    for p in ["external/angle", "external/zlib"] {
        if !aosp_dir.join(p).join(".git").exists() {
            return Err(Error::msg(format!(
                "{p} is missing; run `cargo xtask fetch --project external/angle --project external/zlib`"
            )));
        }
    }
    let cmake = find_tool("cmake", "ANDRUNA_CMAKE", &cmake_candidates())?;
    let ninja = find_tool("ninja", "ANDRUNA_NINJA", &ninja_candidates())?;

    // ANGLE's patches (patches/external/angle); projects that are not
    // fetched are skipped.
    aosp::patch(
        &aosp::Selection::defaults(),
        &root.join("patches"),
        false,
        true,
    )?;

    let out = root.join("out").join("angle").join(opts.arch.name());
    let build_dir = out.join("build");
    let started = Instant::now();
    let mut configure = Command::new(&cmake);
    configure
        .arg("-G")
        .arg("Ninja")
        .arg("-S")
        .arg(root.join("aosp").join("angle"))
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
            "-DCMAKE_BUILD_TYPE={}",
            if opts.debug { "Debug" } else { "Release" }
        ));
    println!("configuring {}", build_dir.display());
    util::run(&mut configure)?;

    util::run(
        Command::new(&ninja)
            .arg("-C")
            .arg(&build_dir)
            .arg("-j")
            .arg(jobs()),
    )?;

    let bin = out.join("bin");
    std::fs::create_dir_all(&bin)?;
    for d in DLLS {
        copy(&build_dir.join(d), &bin.join(d))?;
    }
    println!(
        "ANGLE for {} built in {:.1} s: {}",
        opts.arch.name(),
        started.elapsed().as_secs_f64(),
        bin.display()
    );
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).map_err(|e| {
        Error::msg(format!(
            "copying {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })?;
    Ok(())
}

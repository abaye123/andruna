//! `cargo xtask build`: build everything for one architecture.
//!
//! For now this builds the Rust workspace for the gnullvm target, linked by
//! llvm-mingw. AOSP modules, Java, the manager and the installer are added
//! as those parts appear.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::toolchain;
use crate::util::{self, Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "x64" => Ok(Self::X64),
            "arm64" => Ok(Self::Arm64),
            _ => Err(Error::msg(format!(
                "unknown arch `{s}`; expected x64 or arm64"
            ))),
        }
    }

    pub fn host() -> Self {
        if util::host_is_arm64() {
            Self::Arm64
        } else {
            Self::X64
        }
    }

    pub fn rust_target(self) -> &'static str {
        match self {
            Self::X64 => "x86_64-pc-windows-gnullvm",
            Self::Arm64 => "aarch64-pc-windows-gnullvm",
        }
    }

    /// The llvm-mingw clang driver that links for this architecture.
    pub fn clang_driver(self) -> &'static str {
        match self {
            Self::X64 => "x86_64-w64-mingw32-clang",
            Self::Arm64 => "aarch64-w64-mingw32-clang",
        }
    }
}

/// Environment variable cargo reads for the linker of `target`.
fn linker_env_var(target: &str) -> String {
    format!(
        "CARGO_TARGET_{}_LINKER",
        target.to_ascii_uppercase().replace('-', "_")
    )
}

pub fn build(arch: Arch, release: bool) -> Result<()> {
    let llvm = toolchain::llvm_mingw_dir(None);
    let bin = llvm.join("bin");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let linker = bin.join(format!("{}{exe}", arch.clang_driver()));
    if !linker.is_file() {
        return Err(Error::msg(format!(
            "llvm-mingw not found ({} is missing); run `cargo xtask toolchain` \
             or set {} to an llvm-mingw directory",
            linker.display(),
            toolchain::ENV_VAR
        )));
    }
    if !toolchain::is_installed(&llvm) {
        eprintln!(
            "xtask: warning: {} is not the pinned llvm-mingw {} installed by \
             `cargo xtask toolchain`",
            llvm.display(),
            toolchain::VERSION
        );
    }

    let target = arch.rust_target();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args([
        "build",
        "--workspace",
        "--exclude",
        "xtask",
        "--target",
        target,
    ]);
    if release {
        cmd.arg("--release");
    }
    cmd.env(toolchain::ENV_VAR, &llvm)
        .env(linker_env_var(target), &linker)
        .env("PATH", prepend_path(&bin)?);
    println!(
        "building the Rust workspace for {target}{}",
        if release { " (release)" } else { "" }
    );
    util::run(&mut cmd)
}

fn prepend_path(dir: &Path) -> Result<OsString> {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(old) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&old));
    }
    std::env::join_paths(paths).map_err(|e| Error::msg(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arch() {
        assert_eq!(Arch::parse("x64").unwrap(), Arch::X64);
        assert_eq!(Arch::parse("arm64").unwrap(), Arch::Arm64);
        assert!(Arch::parse("x86").is_err());
    }

    #[test]
    fn linker_variable_name() {
        assert_eq!(
            linker_env_var("aarch64-pc-windows-gnullvm"),
            "CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER"
        );
    }
}

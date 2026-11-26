//! `cargo xtask toolchain`: install the pinned llvm-mingw release.
//!
//! llvm-mingw provides clang, lld, libc++ and the mingw-w64 UCRT runtime for
//! `x86_64-w64-windows-gnu` and `aarch64-w64-windows-gnu` (ADR 0008). The
//! release and the SHA-256 of each host archive are pinned below. To update,
//! change `VERSION` and the checksums together, taken from the release page.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::{self, Context, Error, Result};

/// Pinned llvm-mingw release (LLVM 23.1.6).
pub const VERSION: &str = "20261117";

const BASE_URL: &str = "https://github.com/mstorsjo/llvm-mingw/releases/download";

/// A host build of llvm-mingw: archive name and its SHA-256.
struct HostArchive {
    name: &'static str,
    sha256: &'static str,
}

const HOST_X64: HostArchive = HostArchive {
    name: "llvm-mingw-20261117-ucrt-x86_64.zip",
    sha256: "4d002e330a0f703625aa5ccbc37e96f8f824413e163bfc7336774aabd2c3a412",
};

const HOST_ARM64: HostArchive = HostArchive {
    name: "llvm-mingw-20261117-ucrt-aarch64.zip",
    sha256: "c7f56de1d7aca15196c19083a799205390397b8b87a825aec3485e1e4f3c2cdd",
};

/// Environment variable naming the llvm-mingw directory.
pub const ENV_VAR: &str = "ANDRUNA_LLVM_MINGW";

/// Default install directory on Windows.
const DEFAULT_DIR: &str = r"C:\tools\llvm-mingw";

/// File written into the install directory after a verified install.
const MARKER: &str = ".andruna-toolchain";

fn host_archive() -> &'static HostArchive {
    if util::host_is_arm64() {
        &HOST_ARM64
    } else {
        &HOST_X64
    }
}

fn marker_contents(archive: &HostArchive) -> String {
    format!("llvm-mingw {VERSION} {} {}\n", archive.name, archive.sha256)
}

/// The llvm-mingw directory: `--dir`, else `$ANDRUNA_LLVM_MINGW`, else the
/// default.
pub fn llvm_mingw_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os(ENV_VAR).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

/// True when `dir` holds the pinned release for this host.
pub fn is_installed(dir: &Path) -> bool {
    fs::read_to_string(dir.join(MARKER)).is_ok_and(|m| m == marker_contents(host_archive()))
}

pub fn install(dir: Option<PathBuf>, force: bool) -> Result<()> {
    if !cfg!(windows) {
        return Err(Error::msg(
            "`cargo xtask toolchain` currently installs Windows host builds only",
        ));
    }
    let dir = llvm_mingw_dir(dir);
    let archive = host_archive();

    if is_installed(&dir) && !force {
        println!(
            "llvm-mingw {VERSION} is already installed in {}",
            dir.display()
        );
        return export_for_github(&dir);
    }
    if dir.exists() && !force {
        return Err(Error::msg(format!(
            "{} exists but does not hold llvm-mingw {VERSION} installed by this command \
             (no matching {MARKER}); rerun with --force to replace it, or choose another \
             directory with --dir or {ENV_VAR}",
            dir.display()
        )));
    }

    let parent = dir
        .parent()
        .ok_or_else(|| Error::msg(format!("{} has no parent directory", dir.display())))?;
    fs::create_dir_all(parent).context(|| format!("cannot create {}", parent.display()))?;

    // Download next to the target so the final rename stays on one volume.
    let staging = sibling(&dir, ".partial");
    util::remove_dir_all_if_exists(&staging)?;
    fs::create_dir_all(&staging).context(|| format!("cannot create {}", staging.display()))?;
    let zip = staging.join(archive.name);
    let url = format!("{BASE_URL}/{VERSION}/{}", archive.name);
    println!("downloading {url}");
    util::run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--retry",
                "3",
                "--silent",
                "--show-error",
            ])
            .arg("--output")
            .arg(&zip)
            .arg(&url),
    )?;

    let actual = util::sha256_file(&zip)?;
    if actual != archive.sha256 {
        util::remove_dir_all_if_exists(&staging)?;
        return Err(Error::msg(format!(
            "checksum mismatch for {}: expected {}, got {actual}",
            archive.name, archive.sha256
        )));
    }
    println!("checksum verified: {actual}");

    // Windows ships bsdtar, which extracts zip archives. Use it by full path
    // so a GNU tar earlier on PATH (Git for Windows) is not picked up.
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
        .join("tar.exe");
    let extract = staging.join("x");
    fs::create_dir_all(&extract)?;
    println!("extracting into {}", staging.display());
    util::run(
        Command::new(&tar)
            .arg("-xf")
            .arg(&zip)
            .arg("-C")
            .arg(&extract),
    )?;

    let top = archive.name.trim_end_matches(".zip");
    let extracted = extract.join(top);
    let clang = extracted.join("bin").join("x86_64-w64-mingw32-clang.exe");
    if !clang.is_file() {
        return Err(Error::msg(format!(
            "unexpected archive layout: {} not found",
            clang.display()
        )));
    }

    if force {
        util::remove_dir_all_if_exists(&dir)?;
    }
    fs::rename(&extracted, &dir)
        .context(|| format!("cannot move {} to {}", extracted.display(), dir.display()))?;
    fs::File::create(dir.join(MARKER))?.write_all(marker_contents(archive).as_bytes())?;
    util::remove_dir_all_if_exists(&staging)?;
    println!("installed llvm-mingw {VERSION} into {}", dir.display());
    export_for_github(&dir)
}

/// In GitHub Actions, make the toolchain visible to later steps: add its
/// `bin` directory to PATH and set ANDRUNA_LLVM_MINGW.
fn export_for_github(dir: &Path) -> Result<()> {
    if let Some(path_file) = std::env::var_os("GITHUB_PATH") {
        append_line(
            Path::new(&path_file),
            &dir.join("bin").display().to_string(),
        )?;
    }
    if let Some(env_file) = std::env::var_os("GITHUB_ENV") {
        append_line(
            Path::new(&env_file),
            &format!("{ENV_VAR}={}", dir.display()),
        )?;
    }
    Ok(())
}

fn append_line(file: &Path, line: &str) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file)
        .context(|| format!("cannot open {}", file.display()))?;
    writeln!(f, "{line}")?;
    Ok(())
}

fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    dir.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_match_pinned_version() {
        for a in [&HOST_X64, &HOST_ARM64] {
            assert!(a.name.starts_with(&format!("llvm-mingw-{VERSION}-ucrt-")));
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn explicit_dir_wins() {
        let d = PathBuf::from("X:/somewhere");
        assert_eq!(llvm_mingw_dir(Some(d.clone())), d);
    }

    #[test]
    fn sibling_appends_suffix() {
        assert_eq!(
            sibling(Path::new("C:/tools/llvm-mingw"), ".partial"),
            PathBuf::from("C:/tools/llvm-mingw.partial")
        );
    }
}

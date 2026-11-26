//! Shared helpers: errors, paths, child processes, hashing.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Self(m.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Adds context to I/O errors.
pub trait Context<T> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| Error(format!("{}: {e}", what())))
    }
}

/// The repository root (the parent of the `xtask` directory).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in a subdirectory of the repository")
        .to_path_buf()
}

/// Runs a command, inheriting stdio; fails on a non-zero exit status.
pub fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .context(|| format!("failed to start {}", describe(cmd)))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!("{} failed with {status}", describe(cmd))))
    }
}

/// Runs a command and returns whether it exited successfully, with its
/// output suppressed.
pub fn succeeds(cmd: &mut Command) -> Result<bool> {
    let out = cmd
        .output()
        .context(|| format!("failed to start {}", describe(cmd)))?;
    Ok(out.status.success())
}

/// Runs a command and returns its trimmed standard output.
pub fn output(cmd: &mut Command) -> Result<String> {
    output_raw(cmd).map(|s| s.trim().to_string())
}

/// Runs a command and returns its standard output unchanged.
pub fn output_raw(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .context(|| format!("failed to start {}", describe(cmd)))?;
    if !out.status.success() {
        return Err(Error(format!(
            "{} failed with {}: {}",
            describe(cmd),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().into_owned();
    for a in cmd.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    format!("`{s}`")
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// True when the machine is ARM64, also when this process runs emulated.
pub fn host_is_arm64() -> bool {
    if cfg!(target_arch = "aarch64") {
        return true;
    }
    if cfg!(windows) {
        // An emulated x64 process sees PROCESSOR_ARCHITECTURE=AMD64; the
        // native architecture is in PROCESSOR_ARCHITEW6432 when it differs.
        for var in ["PROCESSOR_ARCHITEW6432", "PROCESSOR_ARCHITECTURE"] {
            if std::env::var(var).is_ok_and(|v| v.eq_ignore_ascii_case("ARM64")) {
                return true;
            }
        }
    }
    false
}

/// Removes a directory tree if it exists.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error(format!("cannot remove {}: {e}", path.display()))),
    }
}

/// Renames a directory, retrying for a few seconds when Windows denies
/// access. A freshly written tree is often still open in a file scanner (for
/// example the antivirus), which makes the rename fail with
/// `ERROR_ACCESS_DENIED` for a short time.
pub fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempt < 40 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            r => return r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_known_input() {
        let dir = std::env::temp_dir().join(format!("xtask-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc.txt");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

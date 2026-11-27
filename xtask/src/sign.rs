//! `cargo xtask sign`: Authenticode-sign shipped binaries with signtool.
//!
//! The certificate is selected by SHA-1 thumbprint from the signing
//! machine's certificate store ([build docs](../../docs/spec/08-build-test-ci.md#code-signing));
//! the thumbprint comes from `ANDRUNA_SIGN_THUMBPRINT` and is never
//! committed. Signatures use SHA-256 digests and an RFC 3161 timestamp.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build_cmd::Arch;
use crate::util::{self, Error, Result};

pub const THUMBPRINT_VAR: &str = "ANDRUNA_SIGN_THUMBPRINT";
pub const SIGNTOOL_VAR: &str = "ANDRUNA_SIGNTOOL";
pub const TIMESTAMP_URL: &str = "http://timestamp.digicert.com";

/// The executables of the workspace that ship.
pub const SHIPPED: &[&str] = &["andruna-sysd.exe", "andruna-launch.exe", "andruna-app.exe"];

const KITS_BIN: &str = r"C:\Program Files (x86)\Windows Kits\10\bin";

/// signtool: `$ANDRUNA_SIGNTOOL`, else the newest Windows SDK's signtool
/// for the host architecture.
fn signtool() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(SIGNTOOL_VAR) {
        return Ok(PathBuf::from(p));
    }
    let host = if util::host_is_arm64() {
        "arm64"
    } else {
        "x64"
    };
    let mut versions: Vec<PathBuf> = std::fs::read_dir(KITS_BIN)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    versions.retain(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("10."))
            && p.join(host).join("signtool.exe").is_file()
    });
    versions.sort_by_key(|p| version_key(p));
    versions
        .pop()
        .map(|v| v.join(host).join("signtool.exe"))
        .ok_or_else(|| {
            Error::msg(format!(
                "signtool.exe not found under {KITS_BIN}; install the Windows SDK or set {SIGNTOOL_VAR}"
            ))
        })
}

fn version_key(p: &Path) -> Vec<u32> {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .split('.')
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

/// Checks a SHA-1 thumbprint: 40 hex digits (spaces are removed).
pub fn normalize_thumbprint(t: &str) -> Result<String> {
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(t.to_ascii_uppercase())
    } else {
        Err(Error::msg(format!(
            "{THUMBPRINT_VAR} must be a 40-digit hex SHA-1 thumbprint"
        )))
    }
}

/// The signtool arguments for one signing run.
pub fn signtool_args(thumbprint: &str, files: &[PathBuf]) -> Vec<String> {
    let mut a: Vec<String> = [
        "sign",
        "/sha1",
        thumbprint,
        "/fd",
        "sha256",
        "/tr",
        TIMESTAMP_URL,
        "/td",
        "sha256",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(files.iter().map(|f| f.display().to_string()));
    a
}

/// The shipped executables of a `cargo xtask build`.
pub fn default_files(arch: Arch, release: bool) -> Vec<PathBuf> {
    let dir = util::repo_root()
        .join("target")
        .join(arch.rust_target())
        .join(if release { "release" } else { "debug" });
    SHIPPED.iter().map(|f| dir.join(f)).collect()
}

pub fn sign(files: &[PathBuf]) -> Result<()> {
    let thumbprint = std::env::var(THUMBPRINT_VAR).map_err(|_| {
        Error::msg(format!(
            "{THUMBPRINT_VAR} is not set; set it to the thumbprint of the code signing \
             certificate in the certificate store"
        ))
    })?;
    let thumbprint = normalize_thumbprint(&thumbprint)?;
    if files.is_empty() {
        return Err(Error::msg("no files to sign"));
    }
    for f in files {
        if !f.is_file() {
            return Err(Error::msg(format!("{} does not exist", f.display())));
        }
    }
    let tool = signtool()?;
    println!("signing {} file(s) with {}", files.len(), tool.display());
    util::run(Command::new(tool).args(signtool_args(&thumbprint, files)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbprints() {
        let t = "ebae118b2c6434273890de749ec457c34fe1a882";
        assert_eq!(
            normalize_thumbprint(t).unwrap(),
            "EBAE118B2C6434273890DE749EC457C34FE1A882"
        );
        assert!(
            normalize_thumbprint("EB AE 11 8B 2C 64 34 27 38 90 DE 74 9E C4 57 C3 4F E1 A8 82")
                .is_ok()
        );
        assert!(normalize_thumbprint("xyz").is_err());
        assert!(normalize_thumbprint(&"0".repeat(41)).is_err());
    }

    #[test]
    fn arguments() {
        let a = signtool_args("AB", &[PathBuf::from("a.exe")]);
        assert_eq!(
            a,
            [
                "sign",
                "/sha1",
                "AB",
                "/fd",
                "sha256",
                "/tr",
                "http://timestamp.digicert.com",
                "/td",
                "sha256",
                "a.exe"
            ]
        );
    }

    #[test]
    fn sdk_versions_sort_numerically() {
        let mut v = [
            PathBuf::from("10.0.9.0"),
            PathBuf::from("10.0.26100.0"),
            PathBuf::from("10.0.19041.0"),
        ];
        v.sort_by_key(|p| version_key(p));
        assert_eq!(v[2], PathBuf::from("10.0.26100.0"));
    }
}

//! Integration tests: real ELF objects built by the aosp ELF toolchain
//! (`python aosp/tools/build_guest.py --arch <arch>`, which compiles
//! `tests/guest/*` against bionic into `out/guest/<arch>/linker-tests`),
//! run in a child process by `andruna-elf`.
//!
//! When the guest build is missing the tests report that and pass, unless
//! `ANDRUNA_REQUIRE_GUEST=1` (CI) makes that a failure.

use std::path::{Path, PathBuf};
use std::process::Command;

fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

fn guest_dir(arch: &str) -> PathBuf {
    if let Ok(d) = std::env::var("ANDRUNA_GUEST_DIR") {
        return PathBuf::from(d);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out/guest")
        .join(arch)
}

fn lib_dirs(g: &Path) -> String {
    let mut v: Vec<PathBuf> = [
        "sysroot/usr/lib",
        "linker-tests",
        "libcxx-install/lib",
        "lib",
    ]
    .iter()
    .map(|d| g.join(d))
    .filter(|d| d.is_dir())
    .collect();
    if let Ok(extra) = std::env::var("ANDRUNA_LIB_PATH") {
        v.extend(
            extra
                .split(';')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
    }
    v.iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(";")
}

/// The test object, or None (skipped) when the guest build is missing.
fn object(arch: &str, name: &str) -> Option<PathBuf> {
    let p = guest_dir(arch).join("linker-tests").join(name);
    if p.is_file() {
        return Some(p);
    }
    if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
        panic!("{} is missing; run aosp/tools/build_guest.py", p.display());
    }
    eprintln!("skipped: {} not built", p.display());
    None
}

fn run(exe: &Path) -> (i32, String, String) {
    let g = guest_dir(arch());
    let out = Command::new(env!("CARGO_BIN_EXE_andruna-elf"))
        .arg("run")
        .arg(exe)
        .env("ANDRUNA_LIB_PATH", lib_dirs(&g))
        .output()
        .expect("spawn andruna-elf");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn linker_test_on_bionic() {
    let Some(exe) = object(arch(), "linker_test") else {
        return;
    };
    let (code, out, err) = run(&exe);
    assert!(
        code == 0 && out.contains("linker_test passed"),
        "exit {code}\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(out.contains("hello from bionic"));
    assert!(
        out.contains("plugin destructor"),
        "dlclose ran the destructor"
    );
}

#[test]
fn cxx_exceptions_on_bionic() {
    let Some(exe) = object(arch(), "cxx_test") else {
        return;
    };
    let (code, out, err) = run(&exe);
    assert!(
        code == 0 && out.contains("cxx_test passed"),
        "exit {code}\nstdout:\n{out}\nstderr:\n{err}"
    );
}

#[test]
fn exit_status_and_missing_library() {
    let Some(exe) = object(arch(), "linker_test") else {
        return;
    };
    // Without the library directories the executable cannot link.
    let out = Command::new(env!("CARGO_BIN_EXE_andruna-elf"))
        .arg("run")
        .arg(&exe)
        .env("ANDRUNA_LIB_PATH", "")
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not found"), "{err}");
}

/// The other architecture's objects map and relocate without running
/// (on x64 this checks the arm64 build).
#[test]
fn link_only_other_architecture() {
    let other = if arch() == "x64" { "arm64" } else { "x64" };
    let Some(exe) = object(other, "linker_test") else {
        return;
    };
    let g = guest_dir(other);
    let out = Command::new(env!("CARGO_BIN_EXE_andruna-elf"))
        .args(["link", "--arch", other])
        .arg(&exe)
        .env("ANDRUNA_LIB_PATH", lib_dirs(&g))
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && s.contains("linked"),
        "{s}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

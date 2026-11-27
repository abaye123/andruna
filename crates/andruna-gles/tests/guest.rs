//! Runs the guest test program (`tests/guest/gles_test.c`, an Android ELF
//! executable built by `tools/build_guest.py`) through andruna-linker with
//! andruna-gles's host functions, on WARP.
//!
//! Needs ANGLE (`cargo xtask angle-build`) and the guest build
//! (`python crates/andruna-gles/tools/build_guest.py --arch <arch>`); skips
//! without them unless `ANDRUNA_REQUIRE_GLES=1`.

use std::path::PathBuf;
use std::process::Command;

fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn guest_program_renders_through_angle() {
    let angle = std::env::var_os("ANDRUNA_ANGLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("out/angle").join(arch()).join("bin"));
    let guest = repo().join("out/gles").join(arch()).join("guest");
    let exe = guest.join("gles_test");
    if !angle.join("libGLESv2.dll").is_file() || !exe.is_file() {
        assert!(
            std::env::var_os("ANDRUNA_REQUIRE_GLES").is_none(),
            "ANGLE ({}) or the guest build ({}) is missing and ANDRUNA_REQUIRE_GLES is set",
            angle.display(),
            exe.display()
        );
        eprintln!("skipped: ANGLE or the guest build is missing");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_andruna-gles-run"))
        .arg("-L")
        .arg(&guest)
        .arg(&exe)
        .env("ANDRUNA_ANGLE_DIR", &angle)
        .env("ANDRUNA_GLES_DEVICE", "warp")
        .env("ANDRUNA_GLES_TRACE", "1")
        .output()
        .expect("andruna-gles-run starts");
    let log = String::from_utf8_lossy(&out.stderr);
    eprintln!("{log}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "gles_test failed at check {:?} (see tests/guest/gles_test.c)",
        out.status.code()
    );
    // The window surface was made and followed the resize.
    assert!(log.contains("window surface 48x40"), "{log}");
    assert!(log.contains("resized to 32x24"), "{log}");
}

//! C-level test: compiles `tests/c/abi_test.c` with llvm-mingw's clang,
//! links it against the crate's static library (`libandruna_lx.a`, as AOSP
//! code will link it) and runs it.
//!
//! llvm-mingw is found through `ANDRUNA_LLVM_MINGW` (default
//! `C:\tools\llvm-mingw`), the same as `cargo xtask build`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Libraries the Rust standard library needs from the platform
/// (`rustc --print native-static-libs` for the gnullvm targets).
const NATIVE_LIBS: &[&str] = &[
    "-lkernel32",
    "-lntdll",
    "-luserenv",
    "-lws2_32",
    "-ldbghelp",
    "-lunwind",
];

fn clang() -> PathBuf {
    let root = std::env::var_os("ANDRUNA_LLVM_MINGW")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\tools\llvm-mingw"));
    let driver = if cfg!(target_arch = "x86_64") {
        "x86_64-w64-mingw32-clang.exe"
    } else {
        "aarch64-w64-mingw32-clang.exe"
    };
    let p = root.join("bin").join(driver);
    assert!(
        p.is_file(),
        "{} not found; install llvm-mingw with `cargo xtask toolchain` or set ANDRUNA_LLVM_MINGW",
        p.display()
    );
    p
}

fn modified(p: &Path) -> SystemTime {
    p.metadata()
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The static library cargo built for this test run: the newest
/// `libandruna_lx*.a` next to the test executable.
fn staticlib() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let profile = deps.parent().unwrap();
    let mut found: Vec<PathBuf> = std::fs::read_dir(deps)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.starts_with("libandruna_lx") && n.ends_with(".a")
        })
        .collect();
    found.push(profile.join("libandruna_lx.a"));
    found.retain(|p| p.is_file());
    found
        .into_iter()
        .max_by_key(|p| modified(p))
        .expect("libandruna_lx.a was not built")
}

#[test]
fn c_callers_see_bionic_semantics() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/c/abi_test.c");
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let exe = tmp.join("lx_c_abi_test.exe");
    let lib = staticlib();

    let out = Command::new(clang())
        .arg("-std=c11")
        .arg("-O1")
        .arg("-Wall")
        .arg("-Werror")
        .arg("-fno-builtin")
        .arg(&src)
        .arg(&lib)
        .args(NATIVE_LIBS)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("run clang");
    assert!(
        out.status.success(),
        "compiling {} against {} failed:\n{}{}",
        src.display(),
        lib.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let root = tmp.join("lx_c_abi_root");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let run = Command::new(&exe).arg(&root).output().expect("run test");
    let stdout = String::from_utf8_lossy(&run.stdout);
    println!("{stdout}");
    assert!(
        run.status.success(),
        "C ABI test failed ({}):\n{stdout}{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout.contains(" 0 failures"));
}

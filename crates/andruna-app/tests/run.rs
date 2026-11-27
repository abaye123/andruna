//! Integration tests: `andruna-app` runs Java programs on ART with the real
//! guest bionic, through andruna-linker, in a child process.
//!
//! Inputs: the ART root (`ANDRUNA_ART_ROOT`, default
//! `out/guest/<arch>/art-root`, made by `python aosp/tools/build_guest.py
//! --arch <arch> art`) and the test dex files (`ANDRUNA_TEST_DEX`, default
//! `out/s1/java`: `hello.dex`, `faults.dex`, `jnistress.dex`, built by
//! `spikes/s1/java/build_boot.py`). When either is missing the tests report
//! that and pass, unless `ANDRUNA_REQUIRE_GUEST=1` makes that a failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn art_root() -> PathBuf {
    std::env::var_os("ANDRUNA_ART_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("out/guest").join(arch()).join("art-root"))
}

fn dex_dir() -> PathBuf {
    std::env::var_os("ANDRUNA_TEST_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("out/s1/java"))
}

/// The inputs, or None (skipped) when they are missing.
fn inputs(dex: &str) -> Option<(PathBuf, PathBuf)> {
    let root = art_root();
    let d = dex_dir().join(dex);
    if root.join("lib64/libart.so").is_file() && d.is_file() {
        return Some((root, d));
    }
    if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
        panic!(
            "{} or {} is missing; run aosp/tools/build_guest.py art",
            root.display(),
            d.display()
        );
    }
    eprintln!("skipped: {} or {} not built", root.display(), d.display());
    None
}

/// A fresh directory for one test (data root, class path copies).
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("andruna-app-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn app(root: &Path, data: &Path, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_andruna-app"))
        .arg("--art-root")
        .arg(root)
        .arg("--data")
        .arg(data)
        .args(args)
        .output()
        .expect("spawn andruna-app");
    let out = Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    };
    eprintln!(
        "andruna-app {args:?}: exit {}\n--- stdout\n{}--- stderr\n{}",
        out.code, out.stdout, out.stderr
    );
    out
}

fn hello(mode: &str, extra: &[&str]) {
    let Some((root, dex)) = inputs("hello.dex") else {
        return;
    };
    let data = scratch(&format!("hello-{mode}"));
    let mut args = vec!["--mode", mode, "--no-boot-image"];
    args.extend_from_slice(extra);
    let d = dex.display().to_string();
    args.extend(["--classpath", &d, "Hello"]);
    let o = app(&root, &data, &args);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("Hello, Andruna"));
}

#[test]
fn hello_in_the_interpreter() {
    hello("int", &[]);
}

#[test]
fn hello_with_the_jit() {
    hello("jit", &[]);
}

#[test]
fn hello_with_every_method_jit_compiled() {
    hello("jit", &["-Xjitthreshold:0"]);
}

#[test]
fn missing_main_class_fails_honestly() {
    let Some((root, dex)) = inputs("hello.dex") else {
        return;
    };
    let data = scratch("missing");
    let d = dex.display().to_string();
    let o = app(
        &root,
        &data,
        &[
            "--mode",
            "int",
            "--no-boot-image",
            "--classpath",
            &d,
            "NoSuchClass",
        ],
    );
    assert_ne!(o.code, 0);
    assert!(
        o.stderr.contains("NoSuchClass") || o.stdout.contains("NoSuchClass"),
        "the error names the class"
    );
}

#[test]
fn implicit_null_checks_and_stack_overflow_reach_art() {
    let Some((root, dex)) = inputs("faults.dex") else {
        return;
    };
    let data = scratch("faults");
    let d = dex.display().to_string();
    // Compiled at first call, so the null checks and the stack probe are
    // implicit (faults delivered as SIGSEGV to ART's handler).
    let o = app(
        &root,
        &data,
        &[
            "--mode",
            "jit",
            "--no-boot-image",
            "-Xjitthreshold:0",
            "--classpath",
            &d,
            "Faults",
        ],
    );
    assert_eq!(o.code, 0);
    assert!(
        o.stdout
            .contains("null checks: 2000 NullPointerException caught")
    );
    assert!(o.stdout.contains("StackOverflowError caught"));
}

/// The boot image is shared by the AOT tests and written once.
static BOOT: Mutex<()> = Mutex::new(());

#[test]
fn aot_boot_image_and_app_code() {
    let Some((root, dex)) = inputs("hello.dex") else {
        return;
    };
    let _g = BOOT.lock().unwrap_or_else(|e| e.into_inner());
    let data = scratch("aot");
    let isa = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "arm64"
    };
    if !root.join("javalib").join(isa).join("boot.art").is_file() {
        let o = app(&root, &data, &["--compile-boot"]);
        assert_eq!(o.code, 0, "dex2oat for the boot image");
    }
    // The app's dex in its own directory; dex2oat writes oat/<isa>/ there.
    let app_dir = data.join("app");
    std::fs::create_dir_all(&app_dir).unwrap();
    let local = app_dir.join("hello.dex");
    std::fs::copy(&dex, &local).unwrap();
    let l = local.display().to_string();
    let o = app(&root, &data, &["--compile", &l]);
    assert_eq!(o.code, 0, "dex2oat for the app");
    let odex = app_dir.join("oat").join(isa).join("hello.odex");
    assert!(odex.is_file(), "{} written", odex.display());
    let o = app(
        &root,
        &data,
        &["--mode", "aot", "-verbose:oat", "--classpath", &l, "Hello"],
    );
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("Hello, Andruna"));
}

/// Compiled JNI stubs take their slow paths (`art_jni_method_start` and
/// `art_jni_method_end`, which pass the `Thread*` to the runtime) when a
/// suspend or checkpoint request is pending. `JniStress` calls native
/// methods of the boot class path on several threads while the main thread
/// runs the GC and walks their stacks.
fn jni_stress(name: &str, extra: &[&str]) {
    let Some((root, dex)) = inputs("jnistress.dex") else {
        return;
    };
    let data = scratch(name);
    let d = dex.display().to_string();
    let mut args = extra.to_vec();
    args.extend(["--classpath", &d, "JniStress", "4", "2000"]);
    let o = app(&root, &data, &args);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("jni stress: 4 threads"));
    assert!(!o.stderr.contains("SIG_DFL"), "no unclaimed fault");
}

/// The JNI stubs of the boot class path as AOT code in the boot image.
#[test]
fn jni_stub_slow_paths_with_the_boot_image() {
    if inputs("jnistress.dex").is_none() {
        return;
    }
    let isa = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "arm64"
    };
    {
        let _g = BOOT.lock().unwrap_or_else(|e| e.into_inner());
        if !art_root()
            .join("javalib")
            .join(isa)
            .join("boot.art")
            .is_file()
        {
            let o = app(&art_root(), &scratch("jni-boot"), &["--compile-boot"]);
            assert_eq!(o.code, 0, "dex2oat for the boot image");
        }
    }
    jni_stress("jni-aot", &["--mode", "jit"]);
}

/// Without the boot image the JIT compiles the stubs of the native methods
/// once they are hot (the default threshold, as in the libcore runs).
#[test]
fn jni_stub_slow_paths_with_jit_compiled_stubs() {
    jni_stress("jni-jit", &["--mode", "jit", "--no-boot-image"]);
}

/// `System.currentTimeMillis()` (gettimeofday) and `Instant.now()`
/// (`CLOCK_REALTIME`) read the Windows system time, not the epoch.
#[test]
fn wall_clock_is_the_system_time() {
    let Some((root, dex)) = inputs("clock.dex") else {
        return;
    };
    let data = scratch("clock");
    let d = dex.display().to_string();
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
    };
    let before = now();
    let o = app(
        &root,
        &data,
        &[
            "--mode",
            "int",
            "--no-boot-image",
            "--classpath",
            &d,
            "Clock",
        ],
    );
    let after = now();
    assert_eq!(o.code, 0);
    let value = |key: &str| -> i64 {
        o.stdout
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("no '{key}' line"))
    };
    let ms = value("currentTimeMillis:");
    assert!(
        ms >= before.as_millis() as i64 && ms <= after.as_millis() as i64,
        "currentTimeMillis {ms} outside [{}, {}]",
        before.as_millis(),
        after.as_millis()
    );
    let s = value("Instant.now:");
    assert!(s >= before.as_secs() as i64 && s <= after.as_secs() as i64);
    assert!(o.stdout.contains("nanoTime advances: true"));
}

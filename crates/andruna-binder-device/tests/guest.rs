//! Guest programs (Android ELF, bionic) on /dev/binder: the broker runs
//! in this test process, as in sysd, and two guest processes run under
//! andruna-elf: `binder_raw manager` (the context manager) and
//! `binder_raw client` (identity, round trips, fd passing, death
//! notification). See `tests/guest/binder_raw.c`.
//!
//! Needs the guest build (`python aosp/tools/build_guest.py binder-tests`)
//! and andruna-elf (`cargo build -p andruna-linker [--release]`; or
//! `ANDRUNA_ELF`). Skips when either is missing, unless
//! `ANDRUNA_REQUIRE_GUEST=1`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use andruna_binder::host::Server;
use andruna_binder::transport::PIPE_ENV;

const CLIENT_UID: u32 = 10_077;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

fn andruna_elf() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ANDRUNA_ELF") {
        return Some(p.into());
    }
    // target/<profile>/deps/guest-*.exe -> target/<profile>/andruna-elf.exe,
    // or the release build.
    let exe = std::env::current_exe().ok()?;
    let profile = exe.parent()?.parent()?;
    [
        profile.join("andruna-elf.exe"),
        profile.parent()?.join("release").join("andruna-elf.exe"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

#[test]
fn guest_processes_call_through_dev_binder() {
    let out = repo().join("out/guest").join(arch());
    let prog = out.join("binder-tests/binder_raw");
    let sysroot = out.join("sysroot/usr/lib");
    let elf = andruna_elf();
    let required = std::env::var("ANDRUNA_REQUIRE_GUEST").as_deref() == Ok("1");
    let Some(elf) = elf.filter(|_| prog.exists()) else {
        assert!(!required, "guest build or andruna-elf missing");
        eprintln!("skipped: no {} or andruna-elf", prog.display());
        return;
    };

    let name = format!(r"\\.\pipe\andruna-binder-guest-test-{}", std::process::id());
    let uids: Arc<Mutex<Vec<(u32, u32)>>> = Arc::default();
    let u = uids.clone();
    let server = Server::start(
        &name,
        Box::new(move |pid| {
            for _ in 0..400 {
                if let Some(&(_, uid)) = u.lock().unwrap().iter().find(|(p, _)| *p == pid) {
                    return Some(uid);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        }),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("andruna-binder-guest-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let spawn = |args: &[&str]| {
        Command::new(&elf)
            .arg("run")
            .arg("-L")
            .arg(&sysroot)
            .arg("--root")
            .arg(&root)
            .arg(&prog)
            .args(args)
            .env(PIPE_ENV, &name)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let manager = spawn(&["manager"]);
    uids.lock().unwrap().push((manager.id(), 1000));
    let t = Instant::now();
    while server.broker().stats().nodes == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "manager did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let iters = std::env::var("BINDER_ITERS").unwrap_or_else(|_| "5000".into());
    let client = spawn(&["client", &iters]);
    let client_pid = client.id();
    uids.lock().unwrap().push((client_pid, CLIENT_UID));
    let out = client.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "client failed");
    // The pid is the Windows process id, assigned by the broker.
    assert!(
        text.contains(&format!("IDENTITY pid={client_pid} ")),
        "{text}"
    );
    assert!(text.contains(&format!("uid={CLIENT_UID}")), "{text}");
    assert!(text.contains("ROUNDTRIP bytes=100"));
    assert!(text.contains("FD_PASSING ok"));
    assert!(text.contains("DEATH ok"));
    let m = manager.wait_with_output().unwrap();
    assert!(m.status.success(), "{}", String::from_utf8_lossy(&m.stderr));
    let _ = std::fs::remove_dir_all(&root);
}

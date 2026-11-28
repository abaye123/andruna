//! servicemanager and libbinder end to end, as sysd runs them: sysd's
//! broker ([`andruna_sysd::binder::Binder`]), servicemanager started by
//! sysd as the `system` uid, and two guest processes using libbinder
//! (`binder_sm_test`, aosp/guest/binder/tests/binder_sm_test.cpp): a
//! service (`system` uid) that registers with servicemanager, and a client
//! with an app uid that finds and calls it (identity, file descriptors in
//! both directions, servicemanager's access policy, round-trip latency,
//! death notification).
//!
//! Needs the guest system root (`python aosp/tools/build_guest.py binder`,
//! out/guest/<arch>/system-root) and andruna-elf (`cargo build --release
//! -p andruna-linker`, or `ANDRUNA_ELF`). Skips without them unless
//! `ANDRUNA_REQUIRE_GUEST=1`. `BINDER_ITERS` sets the number of timed
//! calls (default 5000).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use andruna_sysd::binder::{AID_SYSTEM, Binder, ServiceManagerConfig};

const APP_UID: u32 = 10_077;

/// Kills servicemanager when the test ends, also on failure.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    let exe = std::env::current_exe().ok()?;
    let profile = exe.parent()?.parent()?;
    [
        profile.join("andruna-elf.exe"),
        profile.parent()?.join("release").join("andruna-elf.exe"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn line<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no '{prefix}' line in:\n{text}"))
}

#[test]
fn libbinder_processes_talk_through_servicemanager() {
    let system = repo().join("out/guest").join(arch()).join("system-root");
    let test_prog = system.join("bin/binder_sm_test");
    let required = std::env::var("ANDRUNA_REQUIRE_GUEST").as_deref() == Ok("1");
    let cfg = andruna_elf().and_then(|elf| ServiceManagerConfig::in_system_root(elf, &system));
    let Some(cfg) = cfg.filter(|_| test_prog.is_file()) else {
        assert!(!required, "guest system root or andruna-elf missing");
        eprintln!("skipped: no {} or andruna-elf", system.display());
        return;
    };

    let name = format!(r"\\.\pipe\Andruna.binder.smtest.{}", std::process::id());
    let binder = Binder::start(&name).unwrap();
    let root = std::env::temp_dir().join(format!("andruna-sm-test-{}", std::process::id()));
    let mut sm = KillOnDrop(
        binder
            .start_servicemanager(&cfg, &root.join("servicemanager"))
            .expect("servicemanager"),
    );
    assert!(binder.servicemanager_ready());

    let start = |args: &[&str], uid: u32, dir: &str| {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let r = root.join(dir);
        std::fs::create_dir_all(&r).unwrap();
        let mut cmd = binder.native_command(&cfg.elf_runner, &test_prog, &args, &cfg.lib_dirs, &r);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().unwrap();
        binder.allow(child.id(), uid);
        child
    };

    let service = start(&["service"], AID_SYSTEM, "service");
    let iters = std::env::var("BINDER_ITERS").unwrap_or_else(|_| "5000".into());
    let client = start(&["client", &iters, "app"], APP_UID, "client");
    let client_pid = client.id();
    // A hang fails the test instead of blocking it.
    let service_pid = service.id();
    let sm_pid = sm.0.id();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(180));
        eprintln!("timeout: killing the guest processes");
        for pid in [client_pid, service_pid, sm_pid] {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
    });

    let out = client.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    println!("{text}");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    let s = service.wait_with_output().unwrap();
    let stext = String::from_utf8_lossy(&s.stdout).into_owned();
    println!("service: {stext}");
    eprintln!("service: {}", String::from_utf8_lossy(&s.stderr));
    assert!(out.status.success(), "client failed");

    assert!(
        text.contains("LOOKUP ok descriptor=andruna.test.IEcho"),
        "{text}"
    );
    // The identity the service sees is the one sysd assigned.
    assert_eq!(
        line(&text, "IDENTITY"),
        format!("IDENTITY pid={client_pid} uid={APP_UID}")
    );
    assert!(text.contains("FD_PASSING ok"), "{text}");
    assert!(text.contains("POLICY ok"), "{text}");
    assert!(text.contains("ROUNDTRIP bytes=100 "), "{text}");
    assert!(text.contains("ROUNDTRIP bytes=4096 "), "{text}");
    assert!(text.contains("DEATH ok"), "{text}");
    assert!(stext.contains("SERVICE_READY "), "{stext}");
    assert!(s.status.success());

    // servicemanager keeps running until it is stopped.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(200) {
        assert!(sm.0.try_wait().unwrap().is_none(), "servicemanager exited");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(sm);
    let _ = std::fs::remove_dir_all(&root);
}

//! The launcher executable end to end: it starts the real andruna-sysd when
//! none runs, waits for its ready signal, sends start-package and exits.
//!
//! andruna-sysd.exe must be built next to andruna-launch.exe, which
//! `cargo test --workspace` does (sysd has integration tests, so its binary
//! is built too).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use andruna_ipc::{Client, Endpoint, Request, Response};

const LAUNCH: &str = env!("CARGO_BIN_EXE_andruna-launch");

fn sysd_exe() -> PathBuf {
    let p = Path::new(LAUNCH).with_file_name("andruna-sysd.exe");
    assert!(
        p.is_file(),
        "{} is missing; build the workspace (cargo test --workspace) first",
        p.display()
    );
    p
}

struct Env {
    instance: String,
    root: PathBuf,
}

impl Env {
    fn new(test: &str) -> Self {
        sysd_exe();
        let instance = format!("l{}-{test}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-launch-{instance}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { instance, root }
    }

    fn install(&self, package: &str) {
        fs::create_dir_all(self.root.join("app").join(package)).unwrap();
    }

    fn launch(&self, package: &str) -> Output {
        Command::new(LAUNCH)
            .args(["--quiet", "--instance", &self.instance, "--root"])
            .arg(&self.root)
            .arg(package)
            .output()
            .unwrap()
    }

    fn client(&self) -> Option<Client> {
        Client::connect(
            &Endpoint::for_current_user(&self.instance).unwrap(),
            Duration::ZERO,
        )
        .ok()
    }

    fn status(&self) -> andruna_ipc::Status {
        match self.client().unwrap().request(&Request::Status).unwrap() {
            Response::Status(s) => s,
            r => panic!("{r:?}"),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Ending sysd ends its job and with it the hosts.
        if let Some(c) = self.client() {
            let pid = c.server_pid();
            drop(c);
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
        let t = Instant::now();
        while self.client().is_some() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn starts_sysd_then_reuses_it() {
    let env = Env::new("start");
    env.install("com.example.demo");
    assert!(env.client().is_none(), "no sysd before the first launch");

    let t = Instant::now();
    let out = env.launch("com.example.demo");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    eprintln!(
        "cold launch (sysd start + host start) took {:?}",
        t.elapsed()
    );

    let s = env.status();
    assert_eq!(s.hosts, 1);
    let sysd_pid = s.pid;

    // sysd runs the launcher's own sibling executable.
    let t = Instant::now();
    let out = env.launch("com.example.demo");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    eprintln!("warm launch (already running) took {:?}", t.elapsed());
    let s = env.status();
    assert_eq!(s.pid, sysd_pid, "the same sysd served the second launch");
    assert_eq!(s.hosts, 1, "the running app was focused, not started again");

    let log = fs::read_to_string(env.root.join("logs").join("sysd.log")).unwrap();
    assert!(log.contains("event=host_started"), "{log}");
    assert!(log.contains("placeholder=true"), "{log}");
}

#[test]
fn reports_a_package_that_is_not_installed() {
    let env = Env::new("missing");
    let out = env.launch("com.example.missing");
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("not installed"), "{}", stderr(&out));
}

#[test]
fn rejects_bad_command_lines_without_starting_sysd() {
    let env = Env::new("usage");
    let out = env.launch("not a package");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("not a valid Android package name"));
    let out = env.launch("com.example.demo/not an activity");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("not a valid activity name"));

    let out = Command::new(LAUNCH).arg("--quiet").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Usage"));
    assert!(env.client().is_none());
}

#[test]
fn concurrent_launches_share_one_sysd() {
    let env = Env::new("race");
    env.install("com.example.a");
    env.install("com.example.b");
    let a = {
        let (i, r) = (env.instance.clone(), env.root.clone());
        std::thread::spawn(move || {
            Command::new(LAUNCH)
                .args(["--quiet", "--instance", &i, "--root"])
                .arg(&r)
                .arg("com.example.a")
                .output()
                .unwrap()
        })
    };
    let b = env.launch("com.example.b");
    let a = a.join().unwrap();
    assert_eq!(a.status.code(), Some(0), "{}", stderr(&a));
    assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
    assert_eq!(env.status().hosts, 2);
}

#[test]
fn starts_one_activity() {
    let env = Env::new("activity");
    env.install("com.example.demo");
    let out = env.launch("com.example.demo/.SettingsActivity");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(env.status().hosts, 1);
    // The host logs the activity it was started for, shortly after it starts.
    let t = Instant::now();
    let log = loop {
        let log = fs::read_to_string(env.root.join("logs").join("host.log")).unwrap_or_default();
        if log.contains("event=placeholder_host") || t.elapsed() > Duration::from_secs(10) {
            break log;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(log.contains("activity=.SettingsActivity"), "{log}");
}

//! The launcher executable end to end: it starts the real andruna-sysd when
//! none runs, waits for its ready signal, sends start-package and exits.
//!
//! andruna-sysd.exe must be built next to andruna-launch.exe, which
//! `cargo test --workspace` does (sysd has integration tests, so its binary
//! is built too). The test that starts an app needs andruna-app and the
//! ART root (`ANDRUNA_ART_ROOT`, default `out/guest/x64/art-root`) and the
//! test dex files (`ANDRUNA_TEST_DEX`, default `out/s1/java`); without them
//! it reports that and passes, unless `ANDRUNA_REQUIRE_GUEST=1`.

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
    /// The app host and ART root sysd uses (development overrides).
    apps: Option<(PathBuf, PathBuf)>,
    packages: Vec<(String, andruna_sysd::appcontainer::Salt)>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// andruna-app (copied into an install directory under `root`), the ART
/// root and the dex directory, or None.
fn guest(root: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let art = std::env::var_os("ANDRUNA_ART_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\guest\x64\art-root"));
    let dex = std::env::var_os("ANDRUNA_TEST_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\s1\java"));
    let app = std::env::var_os("ANDRUNA_APP_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(LAUNCH).with_file_name("andruna-app.exe"));
    if !art.join(r"lib64\libart.so").is_file()
        || !dex.join("faults.dex").is_file()
        || !app.is_file()
    {
        if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
            panic!("the ART root, the test dex files or andruna-app is missing");
        }
        eprintln!("skipped: the ART root, the test dex files or andruna-app is missing");
        return None;
    }
    let install = root.join("install");
    fs::create_dir_all(&install).unwrap();
    let exe = install.join("andruna-app.exe");
    fs::copy(&app, &exe).unwrap();
    Some((exe, std::path::absolute(art).unwrap(), dex))
}

impl Env {
    fn new(test: &str) -> Self {
        sysd_exe();
        let instance = format!("l{}-{test}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-launch-{instance}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self {
            instance,
            root,
            apps: None,
            packages: vec![],
        }
    }

    /// Installs a package whose main is Faults with `args` (it waits), as
    /// the package manager will. False when the guest is not built.
    fn install_app(&mut self, package: &str, args: &[&str]) -> bool {
        use andruna_sysd::apps::store::{InstalledPackage, Launchable, RecordStore};
        use andruna_sysd::{Layout, appcontainer};
        let Some((exe, art, dex)) = guest(&self.root) else {
            return false;
        };
        let layout = Layout::new(self.root.clone());
        let salt = appcontainer::Salt::generate().unwrap();
        appcontainer::install(&layout, package, &salt).unwrap();
        let code = appcontainer::app_dir(&layout, package).join("faults.dex");
        fs::copy(dex.join("faults.dex"), &code).unwrap();
        RecordStore::write(
            &layout,
            &InstalledPackage {
                name: package.into(),
                salt: salt.clone(),
                launch: Launchable::Main {
                    code: vec![code],
                    class: "Faults".into(),
                    args: args.iter().map(|a| a.to_string()).collect(),
                },
                mode: "jit".into(),
                permissions: vec![],
            },
        )
        .unwrap();
        self.packages.push((package.into(), salt));
        self.apps = Some((exe, art));
        true
    }

    fn launch(&self, package: &str) -> Output {
        self.command(package).output().unwrap()
    }

    fn command(&self, package: &str) -> Command {
        let mut c = Command::new(LAUNCH);
        c.args(["--quiet", "--instance", &self.instance, "--root"])
            .arg(&self.root)
            .arg(package);
        // The sysd the launcher starts inherits these.
        if let Some((exe, art)) = &self.apps {
            c.env("ANDRUNA_APP_EXE", exe).env("ANDRUNA_ART_ROOT", art);
        }
        c
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
        for (p, salt) in &self.packages {
            let _ = andruna_sysd::appcontainer::uninstall(p, salt);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn starts_sysd_then_reuses_it() {
    let mut env = Env::new("start");
    if !env.install_app("com.example.demo", &["wait"]) {
        return;
    }
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

    // The app's own output reaches sysd's log once ART runs it.
    let t = Instant::now();
    let log = loop {
        let log = fs::read_to_string(env.root.join("logs").join("sysd.log")).unwrap();
        if log.contains("faults: waiting") || t.elapsed() > Duration::from_secs(30) {
            break log;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(log.contains("event=host_started"), "{log}");
    assert!(log.contains("faults: waiting"), "{log}");
}

#[test]
fn reports_a_package_that_is_not_installed() {
    let env = Env::new("missing");
    let out = env.launch("com.example.missing");
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("not installed"), "{}", stderr(&out));
}

#[test]
fn uninstalls_quietly() {
    use andruna_pkg::testkit::apk::ApkSpec;
    use andruna_pkg::testkit::sign::{KeyKind, SignConfig, TestKey};
    let env = Env::new("uninstall");
    let uninstall = |package: &str| {
        Command::new(LAUNCH)
            .args([
                "--quiet",
                "--uninstall",
                "--instance",
                &env.instance,
                "--root",
            ])
            .arg(&env.root)
            .arg(package)
            .output()
            .unwrap()
    };
    // Not installed: refused; this also started sysd.
    let out = uninstall("org.example.gone");
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("not installed"), "{}", stderr(&out));

    let key = TestKey::generate(KeyKind::P256, "launch test");
    let apk = env.root.join("app.apk");
    fs::write(
        &apk,
        ApkSpec::new("org.example.gone").build(&SignConfig::new(key)),
    )
    .unwrap();
    let mut c = env.client().unwrap();
    let r = c
        .request(&Request::InstallPackage {
            paths: vec![apk.display().to_string()],
        })
        .unwrap();
    assert!(matches!(r, Response::Installed(_)), "{r:?}");

    let out = uninstall("org.example.gone");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages { packages: vec![] }
    );
    // `<package>/<activity>` is not a valid uninstall target.
    let out = uninstall("org.example.gone/.Main");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
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
    let mut env = Env::new("race");
    if !env.install_app("com.example.a", &["wait"]) || !env.install_app("com.example.b", &["wait"])
    {
        return;
    }
    let a = {
        let mut c = env.command("com.example.a");
        std::thread::spawn(move || c.output().unwrap())
    };
    let b = env.launch("com.example.b");
    let a = a.join().unwrap();
    assert_eq!(a.status.code(), Some(0), "{}", stderr(&a));
    assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
    assert_eq!(env.status().hosts, 2);
}

#[test]
fn starts_one_activity() {
    let mut env = Env::new("activity");
    if !env.install_app("com.example.demo", &["wait"]) {
        return;
    }
    let out = env.launch("com.example.demo/.SettingsActivity");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(env.status().hosts, 1);
    // sysd logs the activity the app was started for (a main-class app
    // has no activities; the framework entry receives it).
    let log = fs::read_to_string(env.root.join("logs").join("sysd.log")).unwrap_or_default();
    assert!(log.contains("activity=.SettingsActivity"), "{log}");
}

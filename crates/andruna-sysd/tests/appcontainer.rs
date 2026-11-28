//! The AppContainer launch path on real processes: profiles, directory
//! security, and andruna-app running Hello World inside a package's
//! container while files outside its grants stay unreadable.
//!
//! The ART tests need the ART root (`ANDRUNA_ART_ROOT`, default
//! `out/guest/x64/art-root`), the test dex files (`ANDRUNA_TEST_DEX`,
//! default `out/s1/java`) and andruna-app (`ANDRUNA_APP_EXE`, default
//! `target/<profile>/andruna-app.exe`, built by `cargo build -p
//! andruna-app`). When one is missing they report that and pass, unless
//! `ANDRUNA_REQUIRE_GUEST=1`. They add an ACE for the `andrunaRuntime`
//! capability to the ART root, as the installer does for the install
//! directory.

use std::io::Read;
use std::os::windows::io::AsHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use andruna_sysd::Layout;
use andruna_sysd::appcontainer::{self, Capability, Launch, Salt, Sandbox};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A test package with its own profile, removed on drop.
struct Pkg {
    layout: Layout,
    name: String,
    salt: Salt,
    sb: Sandbox,
}

impl Pkg {
    fn install(test: &str) -> Pkg {
        let name = format!("org.andruna.test.{test}.p{}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-ac-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let layout = Layout::new(root);
        let salt = Salt::generate().unwrap();
        let sb = appcontainer::install(&layout, &name, &salt).unwrap();
        Pkg {
            layout,
            name,
            salt,
            sb,
        }
    }

    fn root(&self) -> &Path {
        &self.layout.root
    }

    fn data(&self) -> PathBuf {
        appcontainer::data_dir(&self.layout, &self.name)
    }

    fn app(&self) -> PathBuf {
        appcontainer::app_dir(&self.layout, &self.name)
    }
}

impl Drop for Pkg {
    fn drop(&mut self) {
        let _ = appcontainer::uninstall(&self.name, &self.salt);
        let _ = std::fs::remove_dir_all(&self.layout.root);
    }
}

struct Output {
    code: Option<u32>,
    stdout: String,
    stderr: String,
}

/// Runs `exe args` in the package's container and collects its output.
fn run(pkg: &Pkg, exe: &Path, args: &[String], caps: &[Capability]) -> Output {
    let (mut out_r, out_w) = std::io::pipe().unwrap();
    let (mut err_r, err_w) = std::io::pipe().unwrap();
    let temp = pkg.data().join("tmp");
    std::fs::create_dir_all(&temp).unwrap();
    let env = appcontainer::container_environment(&temp);
    let p = appcontainer::launch(&Launch {
        sandbox: &pkg.sb,
        capabilities: caps,
        exe,
        args,
        stdio: [None, Some(out_w.as_handle()), Some(err_w.as_handle())],
        inherit: &[],
        job: None,
        environment: Some(&env),
        handle_env: None,
        current_dir: Some(&pkg.data()),
        suspended: false,
    })
    .unwrap();
    drop((out_w, err_w));
    let t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_r.read_to_string(&mut s);
        s
    });
    let mut stdout = String::new();
    let _ = out_r.read_to_string(&mut stdout);
    let code = p.wait(Duration::from_secs(120)).unwrap();
    if code.is_none() {
        p.kill();
    }
    let o = Output {
        code,
        stdout,
        stderr: t.join().unwrap(),
    };
    eprintln!(
        "{} {args:?}: exit {:?}\n--- stdout\n{}--- stderr\n{}",
        exe.display(),
        o.code,
        o.stdout,
        o.stderr
    );
    o
}

fn cmd_exe() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe")
}

#[test]
fn profile_lifecycle_and_directory_security() {
    let pkg = Pkg::install("life");
    let sid = pkg.sb.sid.to_string_sid();
    assert!(sid.starts_with("S-1-15-2-"), "{sid}");
    // Installing again reuses the profile and keeps the identity.
    let again = appcontainer::install(&pkg.layout, &pkg.name, &pkg.salt).unwrap();
    assert_eq!(again.sid, pkg.sb.sid);
    assert_eq!(
        appcontainer::sandbox(&pkg.name, &pkg.salt).unwrap().sid,
        pkg.sb.sid
    );
    assert!(pkg.data().is_dir() && pkg.app().is_dir());
    // The user still owns and uses the directories.
    std::fs::write(pkg.data().join("x"), b"1").unwrap();
    std::fs::write(pkg.app().join("y"), b"2").unwrap();
    appcontainer::uninstall(&pkg.name, &pkg.salt).unwrap();
    // Deleting a profile that does not exist is not an error.
    appcontainer::uninstall(&pkg.name, &pkg.salt).unwrap();
}

#[test]
fn the_container_reads_its_grants_only() {
    let pkg = Pkg::install("files");
    let secret = pkg.root().join("secret.txt");
    std::fs::write(&secret, b"outside the grants").unwrap();
    let mine = pkg.data().join("mine.txt");
    std::fs::write(&mine, b"package data").unwrap();
    let apk = pkg.app().join("base.txt");
    std::fs::write(&apk, b"package code").unwrap();

    let caps = appcontainer::capabilities_for::<&str>(&[]);
    // cmd.exe does not parse quotes as the C runtime does; the paths are
    // passed unquoted.
    assert!(!pkg.root().display().to_string().contains(' '));
    let typ = |p: &Path| vec!["/d".into(), "/c".into(), format!("type {}", p.display())];
    let o = run(&pkg, &cmd_exe(), &typ(&mine), &caps);
    assert_eq!(o.code, Some(0));
    assert!(o.stdout.contains("package data"));
    let o = run(&pkg, &cmd_exe(), &typ(&apk), &caps);
    assert_eq!(o.code, Some(0));
    assert!(o.stdout.contains("package code"));
    let o = run(&pkg, &cmd_exe(), &typ(&secret), &caps);
    assert_ne!(o.code, Some(0));
    assert!(!o.stdout.contains("outside the grants"));
    // The data directory is writable (low label), the APK directory not.
    let w = |p: &Path| vec!["/d".into(), "/c".into(), format!("echo w>{}", p.display())];
    assert_eq!(
        run(&pkg, &cmd_exe(), &w(&pkg.data().join("new.txt")), &caps).code,
        Some(0)
    );
    assert!(pkg.data().join("new.txt").is_file());
    assert_ne!(
        run(&pkg, &cmd_exe(), &w(&pkg.app().join("new.txt")), &caps).code,
        Some(0)
    );
    assert!(!pkg.app().join("new.txt").exists());

    // Another package cannot read this one's data.
    let other = Pkg::install("other");
    let o = run(&other, &cmd_exe(), &typ(&mine), &caps);
    assert!(!o.stdout.contains("package data"));
}

#[test]
fn peer_identity_comes_from_the_token() {
    let pkg = Pkg::install("peer");
    let (_r, w) = std::io::pipe().unwrap();
    let args: Vec<String> = vec!["/d".into(), "/c".into(), "exit 7".into()];
    let exe = cmd_exe();
    let p = appcontainer::launch(&Launch {
        sandbox: &pkg.sb,
        capabilities: &[Capability::Runtime, Capability::InternetClient],
        exe: &exe,
        args: &args,
        stdio: [None, Some(w.as_handle()), None],
        inherit: &[w.as_handle()],
        job: None,
        environment: None,
        handle_env: None,
        current_dir: None,
        suspended: true,
    })
    .unwrap();
    assert_eq!(p.inherited().len(), 1);
    let by_pid = appcontainer::pid_sid(p.pid).unwrap();
    assert_eq!(by_pid.as_ref(), Some(&pkg.sb.sid));
    assert_eq!(
        appcontainer::process_sid(p.process()).unwrap().as_ref(),
        Some(p.app_container_sid())
    );
    p.resume().unwrap();
    assert_eq!(p.wait(Duration::from_secs(30)).unwrap(), Some(7));
}

#[test]
fn launch_failure_is_reported() {
    let pkg = Pkg::install("fail");
    let missing = pkg.root().join("missing.exe");
    let e = appcontainer::launch(&Launch {
        sandbox: &pkg.sb,
        capabilities: &[Capability::Runtime],
        exe: &missing,
        args: &[],
        stdio: [None, None, None],
        inherit: &[],
        job: None,
        environment: None,
        handle_env: None,
        current_dir: None,
        suspended: false,
    })
    .unwrap_err();
    assert!(e.to_string().contains("missing.exe"), "{e}");
}

/// ART root, hello.dex and andruna-app.exe, or None (skipped).
fn guest() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let root = std::env::var_os("ANDRUNA_ART_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\guest\x64\art-root"));
    let dex = std::env::var_os("ANDRUNA_TEST_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(r"out\s1\java"))
        .join("hello.dex");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let app = std::env::var_os("ANDRUNA_APP_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo().join("target"));
            target.join(profile).join("andruna-app.exe")
        });
    if root.join(r"lib64\libart.so").is_file() && dex.is_file() && app.is_file() {
        return Some((std::path::absolute(root).unwrap(), dex, app));
    }
    if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
        panic!(
            "{}, {} or {} is missing",
            root.display(),
            dex.display(),
            app.display()
        );
    }
    eprintln!("skipped: ART root, hello.dex or andruna-app not built");
    None
}

#[test]
fn hello_world_runs_in_the_container_and_cannot_read_outside() {
    let Some((art, dex, app_src)) = guest() else {
        return;
    };
    let pkg = Pkg::install("hello");
    // The install directory: andruna-app and the ART root are readable
    // through the runtime capability.
    let install = pkg.root().join("install");
    std::fs::create_dir_all(&install).unwrap();
    let app = install.join("andruna-app.exe");
    std::fs::copy(&app_src, &app).unwrap();
    appcontainer::grant_runtime(&install).unwrap();
    appcontainer::grant_runtime(&art).unwrap();
    // The app's code in its APK directory.
    let code = pkg.app().join("hello.dex");
    std::fs::copy(&dex, &code).unwrap();
    // The same code where the package has no grant.
    let outside = pkg.root().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::copy(&dex, outside.join("hello.dex")).unwrap();

    let args = |cp: &Path| -> Vec<String> {
        vec![
            "--art-root".into(),
            art.display().to_string(),
            "--data".into(),
            pkg.data().join("root").display().to_string(),
            "--classpath".into(),
            cp.display().to_string(),
            "Hello".into(),
        ]
    };
    let caps = appcontainer::capabilities_for(&["android.permission.INTERNET"]);
    let o = run(&pkg, &app, &args(&code), &caps);
    assert_eq!(o.code, Some(0));
    assert!(o.stdout.contains("Hello, Andruna"));
    // ART wrote its guest tree into the package's data directory.
    assert!(pkg.data().join(r"root\data\local\tmp").is_dir());

    let o = run(&pkg, &app, &args(&outside.join("hello.dex")), &caps);
    assert_ne!(o.code, Some(0));
    assert!(!o.stdout.contains("Hello, Andruna"));
    assert!(o.stderr.contains("hello.dex"), "the error names the file");
}

//! The app process lifecycle on real processes: the supervisor starts
//! `andruna-app --host` in a package's AppContainer, maps the boot image
//! view, and reports how the app ended.
//!
//! What the tests need and how they skip: see `common`.
//!
//! `ANDRUNA_MEASURE=<runs>` makes `boot_image_start_times` measure start
//! times with and without the view (docs/plan/phase2/app-lifecycle.md).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use andruna_ipc::host::CrashKind;
use andruna_ipc::{ErrorCode, EventKind, Response};
use andruna_sysd::appcontainer::{self, Salt};
use andruna_sysd::apps::store::RecordStore;
use andruna_sysd::{Activity, Layout, Supervisor};

mod common;

use common::guest;

/// A data root with an install directory holding andruna-app.
struct Env {
    layout: Layout,
    sup: Arc<Supervisor>,
    activity: Arc<Activity>,
    packages: Vec<(String, Salt)>,
}

impl Env {
    fn new(test: &str, art_root: &Path, map_boot_image: bool) -> Option<Env> {
        let root = std::env::temp_dir().join(format!("andruna-life-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = common::host_config(&root, art_root, map_boot_image)?;
        let layout = Layout::new(root);
        let activity = Arc::new(Activity::new(Duration::from_secs(3600)));
        let sup = Arc::new(
            Supervisor::with(
                config,
                layout.clone(),
                Box::new(RecordStore::new(layout.clone())),
                activity.clone(),
            )
            .unwrap(),
        );
        Some(Env {
            layout,
            sup,
            activity,
            packages: vec![],
        })
    }

    /// Installs a package that runs `class` from `dex`, as the package
    /// manager will: profile and directories, code, record.
    fn install(&mut self, name: &str, dex: Option<&Path>, class: &str, args: &[&str]) {
        let salt = common::install(&self.layout, name, dex, class, args);
        self.packages.push((name.into(), salt));
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.layout.logs_dir().join("sysd.log")).unwrap_or_default()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.sup.shutdown();
        for (p, s) in &self.packages {
            let _ = appcontainer::uninstall(p, s);
        }
        let _ = std::fs::remove_dir_all(&self.layout.root);
    }
}

fn pkg(test: &str) -> String {
    format!("org.andruna.test.{test}.p{}", std::process::id())
}

/// Logging goes to the test's data root.
fn init_log(env: &Env) {
    let _ = andruna_sysd::log::init(&env.layout.logs_dir(), "sysd");
}

#[test]
fn hello_runs_in_its_container_and_its_exit_is_reported() {
    let Some((art, dex)) = guest() else { return };
    let Some(mut env) = Env::new("hello", &art, true) else {
        return;
    };
    init_log(&env);
    let name = pkg("hello");
    env.install(&name, Some(&dex.join("hello.dex")), "Hello", &[]);
    let t = Instant::now();
    let (pid, again) = env.sup.start(&name).unwrap();
    assert!(!again);
    let r = env
        .sup
        .wait_exit(&name, pid, Duration::from_secs(120))
        .expect("the app ends");
    eprintln!("{r:?} after {:?}", t.elapsed());
    assert_eq!(r.status, Some(0));
    assert_eq!(r.exit_code, 0);
    assert!(r.crash.is_none() && !r.stopped);
    assert!(r.times.running_ms > 0.0);
    // The process ran in the package's container and printed through
    // sysd's pipes; ART used the boot image view.
    let log = env.log();
    assert!(log.contains("Hello, Andruna"), "{log}");
    assert!(log.contains("event=host_created"), "{log}");
    for l in log.lines().filter(|l| l.contains("boot image view")) {
        eprintln!("{l}");
    }
    let mine = format!("package={name} ");
    let adopted = log
        .lines()
        .filter(|l| l.contains(&mine))
        .find_map(|l| {
            l.split("boot image view: ")
                .nth(1)?
                .split(" KiB adopted")
                .next()?
                .parse::<u64>()
                .ok()
        })
        .expect("the host reports the view's use");
    assert!(adopted > 1024, "{adopted} KiB adopted\n{log}");
    assert_eq!(env.sup.host_pid(&name), None);
    assert_eq!(env.activity.counts(), (0, 0));
    // No crash event for a clean exit.
    match env.sup.events().wait(0, 0, Duration::from_millis(10)) {
        Response::Events { events, .. } => assert!(events.is_empty(), "{events:?}"),
        r => panic!("{r:?}"),
    }
}

#[test]
fn an_uncaught_exception_is_a_crash_event() {
    let Some((art, dex)) = guest() else { return };
    let Some(mut env) = Env::new("crash", &art, true) else {
        return;
    };
    let name = pkg("crash");
    env.install(
        &name,
        Some(&dex.join("faults.dex")),
        "Faults",
        &["uncaught"],
    );
    let (pid, _) = env.sup.start(&name).unwrap();
    let r = env
        .sup
        .wait_exit(&name, pid, Duration::from_secs(120))
        .expect("the app ends");
    eprintln!("{r:?}");
    assert_eq!(r.status, Some(1));
    let c = r.crash.expect("a crash report");
    assert_eq!(c.kind, CrashKind::UncaughtException);
    assert!(
        c.summary.starts_with("java.lang.NullPointerException"),
        "{}",
        c.summary
    );
    assert!(c.summary.contains("at Faults.read"), "{}", c.summary);
    match env.sup.events().wait(0, 0, Duration::from_secs(5)) {
        Response::Events { events, .. } => {
            assert_eq!(events.len(), 1, "{events:?}");
            match &events[0].kind {
                EventKind::AppCrashed {
                    package,
                    pid: p,
                    exit_code,
                    reason,
                } => {
                    assert_eq!((package.as_str(), *p, *exit_code), (name.as_str(), pid, 1));
                    assert!(reason.contains("NullPointerException"), "{reason}");
                }
                k => panic!("{k:?}"),
            }
        }
        r => panic!("{r:?}"),
    }
}

#[test]
fn one_process_per_package_and_stop_is_not_a_crash() {
    let Some((art, dex)) = guest() else { return };
    let Some(mut env) = Env::new("stop", &art, true) else {
        return;
    };
    let name = pkg("stop");
    env.install(&name, Some(&dex.join("faults.dex")), "Faults", &["wait"]);
    let (pid, again) = env.sup.start(&name).unwrap();
    assert!(!again);
    assert_eq!(env.sup.start(&name).unwrap(), (pid, true));
    assert_eq!(env.sup.package_for_pid(pid), Some(name.clone()));
    // A host process in the package's container.
    assert_eq!(
        appcontainer::pid_sid(pid)
            .unwrap()
            .map(|s| s.to_string_sid()),
        Some(
            appcontainer::sandbox(&name, &env.packages[0].1)
                .unwrap()
                .sid
                .to_string_sid()
        )
    );
    // It has no window: focusing says so.
    assert_eq!(env.sup.focus(&name).unwrap_err().code, ErrorCode::NoWindow);
    env.sup.stop(&name).unwrap();
    let r = env
        .sup
        .wait_exit(&name, pid, Duration::from_secs(30))
        .expect("stopped");
    assert!(r.stopped && r.crash.is_none(), "{r:?}");
    assert_eq!(env.sup.stop(&name).unwrap_err().code, ErrorCode::NotRunning);
    match env.sup.events().wait(0, 0, Duration::from_millis(10)) {
        Response::Events { events, .. } => assert!(events.is_empty(), "{events:?}"),
        r => panic!("{r:?}"),
    }
    assert_eq!(env.activity.counts(), (0, 0));
}

/// Needs andruna-app but not ART: the framework entry is refused with an
/// honest error, and the refusal is not a crash.
#[test]
fn the_framework_entry_is_refused_honestly() {
    let art = std::env::temp_dir().join("andruna-life-no-art");
    let Some(mut env) = Env::new("framework", &art, true) else {
        return;
    };
    let name = pkg("framework");
    env.install(&name, None, "", &[]);
    let e = env.sup.start(&name).unwrap_err();
    assert_eq!(e.code, ErrorCode::HostStartFailed);
    assert!(e.message.contains("framework"), "{}", e.message);
    assert_eq!(env.sup.host_pid(&name), None);
    // Not installed and invalid names are refused before any process.
    let e = env.sup.start("org.andruna.test.missing").unwrap_err();
    assert_eq!(e.code, ErrorCode::NotInstalled);
    // The refused host ends; nothing stays counted.
    let t = Instant::now();
    while env.activity.counts() != (0, 0) {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    match env.sup.events().wait(0, 0, Duration::from_millis(10)) {
        Response::Events { events, .. } => assert!(events.is_empty(), "{events:?}"),
        r => panic!("{r:?}"),
    }
}

fn median(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[0], v[v.len() - 1])
}

/// Start time with the boot image view mapped by sysd against andruna-app
/// reading the image itself, interleaved.
#[test]
fn boot_image_start_times() {
    let Some(runs) = std::env::var("ANDRUNA_MEASURE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        eprintln!("skipped: set ANDRUNA_MEASURE=<runs>");
        return;
    };
    let Some((art, dex)) = guest() else { return };
    let Some(mut with) = Env::new("measure-view", &art, true) else {
        return;
    };
    let Some(mut without) = Env::new("measure-self", &art, false) else {
        return;
    };
    init_log(&with);
    let name = pkg("measure");
    with.install(&name, Some(&dex.join("hello.dex")), "Hello", &[]);
    without.install(&name, Some(&dex.join("hello.dex")), "Hello", &[]);
    let idle = pkg("measureidle");
    for env in [&mut with, &mut without] {
        env.install(&idle, Some(&dex.join("faults.dex")), "Faults", &["wait"]);
    }
    // (start to Running, start to exit, map)
    let mut rows: Vec<(Vec<f64>, Vec<f64>, Vec<f64>)> = vec![(vec![], vec![], vec![]); 2];
    for i in 0..runs + 1 {
        for (k, env) in [&with, &without].into_iter().enumerate() {
            let t = Instant::now();
            let (pid, _) = env.sup.start(&name).unwrap();
            let r = env
                .sup
                .wait_exit(&name, pid, Duration::from_secs(120))
                .unwrap();
            let total = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(r.status, Some(0), "{r:?}");
            if i == 0 {
                continue; // warm-up: the view file is built, caches fill
            }
            rows[k].0.push(r.times.running_ms);
            rows[k].1.push(total);
            rows[k].2.push(r.times.mapped_ms);
        }
    }
    // Memory of a started app (Faults sleeping after start), both ways,
    // two processes at once each way.
    let mib = |b: usize| b as f64 / (1024.0 * 1024.0);
    for (label, env) in [
        ("view mapped by sysd", &with),
        ("andruna-app reads the image", &without),
    ] {
        let (pid, _) = env.sup.start(&idle).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let m = env.sup.host_memory(&idle).unwrap().unwrap();
        eprintln!(
            "{label}: idle app working set {:.1} MiB, private working set {:.1} MiB, private commit {:.1} MiB",
            mib(m.working_set),
            mib(m.private_working_set),
            mib(m.private_commit)
        );
        env.sup.stop(&idle).unwrap();
        env.sup
            .wait_exit(&idle, pid, Duration::from_secs(30))
            .unwrap();
    }
    if std::env::var_os("ANDRUNA_MEASURE_LOG").is_some() {
        for l in with.log().lines().filter(|l| l.contains("stream=stderr")) {
            eprintln!("{l}");
        }
    }
    for (label, row) in ["view mapped by sysd", "andruna-app reads the image"]
        .iter()
        .zip(rows.iter_mut())
    {
        let (a, b, c) = (median(&mut row.0), median(&mut row.1), median(&mut row.2));
        eprintln!(
            "{label}: start to exit {:.1} ms [{:.1}-{:.1}], start to Running {:.1} ms, map {:.3} ms [{:.3}-{:.3}]",
            b.0, b.1, b.2, a.0, c.0, c.1, c.2
        );
    }
}

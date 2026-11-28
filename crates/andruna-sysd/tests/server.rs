//! The pipe server, in this test process: the real listener, server,
//! supervisor and request handling, with a client on the test thread. The
//! app processes are real (`andruna-app --host` running ART in the
//! package's AppContainer); the tests that start one skip without the ART
//! root (see `common`).

mod common;

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use andruna_ipc::proto;
use andruna_ipc::{
    Client, Endpoint, ErrorCode, PROTOCOL_VERSION, PipeListener, PipeSecurity, Request, Response,
};
use andruna_sysd::appcontainer::Salt;
use andruna_sysd::apps::store::{DatabaseStore, InstalledPackage, Launchable, RecordStore};
use andruna_sysd::{Activity, Context, Layout, Server, Supervisor};

struct Fixture {
    endpoint: Endpoint,
    root: PathBuf,
    ctx: Arc<Context>,
    server: Option<Server>,
    salts: std::sync::Mutex<Vec<(String, Salt)>>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::make(name, false).expect("no app host needed")
    }

    /// A fixture whose supervisor starts andruna-app with the ART root;
    /// `None` (skipped) without them.
    fn with_apps(name: &str) -> Option<Self> {
        Self::make(name, true)
    }

    fn make(name: &str, apps: bool) -> Option<Self> {
        let instance = format!("t{}-{name}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-sysd-{instance}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let endpoint = Endpoint::for_current_user(&instance).unwrap();
        let listener =
            PipeListener::bind(&endpoint, &PipeSecurity::current_user_only(&endpoint)).unwrap();
        let layout = Layout::new(root.clone());
        let activity = Arc::new(Activity::new(Duration::from_secs(3600)));
        let store = Arc::new(andruna_sysd::open_store(&layout, false).unwrap());
        let supervisor = if apps {
            let (art, _) = common::guest()?;
            let config = common::host_config(&root, &art, true)?;
            Supervisor::with(
                config,
                layout.clone(),
                Box::new(DatabaseStore::new(
                    store.clone(),
                    RecordStore::new(layout.clone()),
                )),
                activity.clone(),
            )
        } else {
            // No app host: starts fail before any process is made.
            let exe = root.join("missing").join("andruna-app.exe");
            Supervisor::new(exe, layout.clone(), activity.clone())
        };
        let supervisor = Arc::new(supervisor.unwrap());
        let ctx = Arc::new(Context {
            layout,
            activity,
            supervisor,
            store,
            started: Instant::now(),
        });
        let server = Server::spawn(listener, ctx.clone()).unwrap();
        Some(Self {
            endpoint,
            root,
            ctx,
            server: Some(server),
            salts: std::sync::Mutex::new(vec![]),
        })
    }

    fn client(&self) -> Client {
        Client::connect(&self.endpoint, Duration::from_secs(5)).unwrap()
    }

    /// Makes a package look installed to the supervisor (its directory
    /// exists) without a database record, for the host tests.
    fn install_dir_only(&self, package: &str) {
        fs::create_dir_all(self.root.join("app").join(package)).unwrap();
    }

    /// Installs a package whose main is Faults with `args`.
    fn install_faults(&self, package: &str, args: &[&str]) {
        let (_, dex) = common::guest().unwrap();
        let salt = common::install(
            &self.ctx.layout,
            package,
            Some(&dex.join("faults.dex")),
            "Faults",
            args,
        );
        self.salts.lock().unwrap().push((package.to_string(), salt));
    }

    /// Waits until the activity counts reach `want`.
    fn wait_counts(&self, want: (u32, u32)) {
        let t = Instant::now();
        while self.ctx.activity.counts() != want {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "counts {:?}, want {want:?}",
                self.ctx.activity.counts()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.ctx.supervisor.shutdown();
        if let Some(s) = self.server.take() {
            s.stop();
        }
        for (p, salt) in self.salts.lock().unwrap().iter() {
            let _ = andruna_sysd::appcontainer::uninstall(p, salt);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn ping_status_and_packages() {
    let f = Fixture::new("basic");
    let mut c = f.client();
    assert_eq!(c.server_pid(), std::process::id());
    assert_eq!(
        c.request(&Request::Ping).unwrap(),
        Response::Pong {
            protocol: PROTOCOL_VERSION
        }
    );
    match c.request(&Request::Status).unwrap() {
        Response::Status(s) => {
            assert_eq!(s.pid, std::process::id());
            assert_eq!(s.connections, 1);
            assert_eq!(s.hosts, 0);
            assert_eq!(s.idle_timeout_ms, 3_600_000);
            assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
        }
        r => panic!("{r:?}"),
    }
    // Nothing installed: an empty list, not an invented one.
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages { packages: vec![] }
    );
    // A directory alone is not an installed package: the list comes from
    // the package database.
    f.install_dir_only("org.example.two");
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages { packages: vec![] }
    );
    // A second client is counted while open.
    let mut c2 = f.client();
    match c2.request(&Request::Status).unwrap() {
        Response::Status(s) => assert_eq!(s.connections, 2),
        r => panic!("{r:?}"),
    }
    drop(c2);
    drop(c);
    f.wait_counts((0, 0));
}

#[test]
fn start_refusals() {
    let f = Fixture::new("refuse");
    let mut c = f.client();
    let code = |r: Response| match r {
        Response::Error { code, .. } => code,
        r => panic!("{r:?}"),
    };
    let r = c
        .request(&Request::StartPackage {
            package: "com.example.missing".into(),
        })
        .unwrap();
    assert_eq!(code(r), ErrorCode::NotInstalled);
    let r = c
        .request(&Request::StartPackage {
            package: "..\\x".into(),
        })
        .unwrap();
    assert_eq!(code(r), ErrorCode::InvalidPackageName);
    let r = c
        .request(&Request::FocusWindow {
            package: "com.example.missing".into(),
        })
        .unwrap();
    assert_eq!(code(r), ErrorCode::NotRunning);
}

#[test]
fn start_focus_and_exit_of_a_host() {
    let Some(f) = Fixture::with_apps("host") else {
        return;
    };
    f.install_faults("com.example.demo", &["wait"]);
    let mut c = f.client();
    let start = Request::StartPackage {
        package: "com.example.demo".into(),
    };
    let pid = match c.request(&start).unwrap() {
        Response::Started {
            pid,
            already_running: false,
        } => pid,
        r => panic!("{r:?}"),
    };
    assert_ne!(pid, std::process::id());
    assert_eq!(f.ctx.supervisor.host_pid("com.example.demo"), Some(pid));
    match c.request(&start).unwrap() {
        Response::Started {
            pid: again,
            already_running: true,
        } => assert_eq!(again, pid),
        r => panic!("{r:?}"),
    }
    // The app runs (in ART) but has no window yet: an honest answer.
    let focus = Request::FocusWindow {
        package: "com.example.demo".into(),
    };
    match c.request(&focus).unwrap() {
        Response::Error {
            code: ErrorCode::NoWindow,
            ..
        } => {}
        r => panic!("{r:?}"),
    }
    f.wait_counts((1, 1));
    // Ending the job ends the host; the supervisor notices.
    f.ctx.supervisor.shutdown();
    f.wait_counts((1, 0));
    assert_eq!(f.ctx.supervisor.host_pid("com.example.demo"), None);
}

#[test]
fn start_without_an_app_host_fails_honestly() {
    let f = Fixture::new("nohost");
    f.install_dir_only("com.example.demo");
    RecordStore::write(
        &f.ctx.layout,
        &InstalledPackage {
            name: "com.example.demo".into(),
            salt: Salt::generate().unwrap(),
            launch: Launchable::Framework,
            mode: "jit".into(),
            permissions: vec![],
        },
    )
    .unwrap();
    let mut c = f.client();
    match c
        .request(&Request::StartPackage {
            package: "com.example.demo".into(),
        })
        .unwrap()
    {
        Response::Error {
            code: ErrorCode::HostStartFailed,
            message,
        } => assert!(message.contains("andruna-app.exe"), "{message}"),
        r => panic!("{r:?}"),
    }
    // Installed without a package record: also an honest failure.
    f.install_dir_only("com.example.norecord");
    match c
        .request(&Request::StartPackage {
            package: "com.example.norecord".into(),
        })
        .unwrap()
    {
        Response::Error {
            code: ErrorCode::HostStartFailed,
            message,
        } => assert!(message.contains("record"), "{message}"),
        r => panic!("{r:?}"),
    }
    f.wait_counts((1, 0));
}

/// Opens the pipe as a plain file to send hand-made frames.
fn raw(f: &Fixture) -> fs::File {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.endpoint.pipe_name())
        .unwrap()
}

fn read_response(file: &mut fs::File) -> Option<(u32, Response)> {
    proto::read_frame(file)
        .unwrap()
        .map(|p| Response::decode(&p).unwrap())
}

#[test]
fn protocol_errors() {
    let f = Fixture::new("proto");

    // Another protocol version: answered in ours, then closed.
    let mut p = raw(&f);
    let mut payload = Request::Ping.encode(42);
    payload[0] = 9;
    proto::write_frame(&mut p, &payload).unwrap();
    match read_response(&mut p) {
        Some((
            42,
            Response::Error {
                code: ErrorCode::UnsupportedVersion,
                ..
            },
        )) => {}
        r => panic!("{r:?}"),
    }
    assert!(read_response(&mut p).is_none(), "connection is closed");

    // An unknown request kind: answered, connection stays usable.
    drop(p);
    let mut p = raw(&f);
    let mut payload = Request::Ping.encode(7);
    payload[2] = 0x77;
    proto::write_frame(&mut p, &payload).unwrap();
    match read_response(&mut p) {
        Some((
            7,
            Response::Error {
                code: ErrorCode::UnknownRequest,
                ..
            },
        )) => {}
        r => panic!("{r:?}"),
    }
    proto::write_frame(&mut p, &Request::Ping.encode(8)).unwrap();
    assert!(matches!(
        read_response(&mut p),
        Some((8, Response::Pong { .. }))
    ));

    // A malformed body: answered, then closed.
    drop(p);
    let mut p = raw(&f);
    let mut payload = Request::Ping.encode(3);
    payload.push(0);
    proto::write_frame(&mut p, &payload).unwrap();
    assert!(matches!(
        read_response(&mut p),
        Some((
            3,
            Response::Error {
                code: ErrorCode::Malformed,
                ..
            }
        ))
    ));
    assert!(read_response(&mut p).is_none());

    // An oversized length prefix: closed without reading the body.
    drop(p);
    let mut p = raw(&f);
    p.write_all(&u32::MAX.to_le_bytes()).unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(p.read(&mut buf), Ok(0) | Err(_)));

    f.wait_counts((0, 0));
}

fn started_pid(r: Response) -> u32 {
    match r {
        Response::Started { pid, .. } => pid,
        r => panic!("{r:?}"),
    }
}

#[test]
fn a_crashed_host_is_an_event_and_waiters_are_passive() {
    use andruna_ipc::EventKind;
    let Some(f) = Fixture::with_apps("crash") else {
        return;
    };
    f.install_faults("com.example.crash", &["wait"]);
    let mut waiter = f.client();
    let (epoch, next) = match waiter
        .request(&Request::WaitEvents {
            epoch: 0,
            after: u64::MAX,
            timeout_ms: 0,
        })
        .unwrap()
    {
        Response::Events {
            epoch,
            next,
            events,
            ..
        } => {
            assert!(events.is_empty());
            (epoch, next)
        }
        r => panic!("{r:?}"),
    };
    // The waiting connection does not count as activity.
    f.wait_counts((0, 0));

    let mut c = f.client();
    let pid = started_pid(
        c.request(&Request::StartActivity {
            package: "com.example.crash".into(),
            activity: ".Main".into(),
        })
        .unwrap(),
    );
    drop(c);
    f.wait_counts((0, 1));
    // Ended from outside with a non-zero code: a crash.
    let killed = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output()
        .unwrap();
    assert!(killed.status.success(), "{killed:?}");
    let events = match waiter
        .request(&Request::WaitEvents {
            epoch,
            after: next,
            timeout_ms: 10_000,
        })
        .unwrap()
    {
        Response::Events { events, missed, .. } => {
            assert!(!missed);
            events
        }
        r => panic!("{r:?}"),
    };
    assert_eq!(events.len(), 1);
    match &events[0].kind {
        EventKind::AppCrashed {
            package,
            pid: p,
            exit_code,
            ..
        } => {
            assert_eq!(package, "com.example.crash");
            assert_eq!(*p, pid);
            assert_ne!(*exit_code, 0);
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn missing_functions_are_reported_only_by_hosts() {
    use andruna_ipc::MissingFunction;
    let f = Fixture::new("missing");
    let mut c = f.client();
    // This test process is not a host: refused.
    match c
        .request(&Request::ReportMissingFunctions {
            entries: vec![MissingFunction {
                name: "lx:x".into(),
                calls: 1,
            }],
        })
        .unwrap()
    {
        Response::Error {
            code: ErrorCode::NotPermitted,
            ..
        } => {}
        r => panic!("{r:?}"),
    }
    assert_eq!(
        c.request(&Request::GetMissingFunctions {
            package: "com.example.app".into()
        })
        .unwrap(),
        Response::MissingFunctions {
            updated_ms: 0,
            entries: vec![]
        }
    );
    // What a host reported (written through the same store) is read back.
    andruna_sysd::diagnostics::add(
        &f.root.join("diagnostics"),
        "com.example.app",
        &[MissingFunction {
            name: "service:package.getInstalledModules".into(),
            calls: 3,
        }],
    )
    .unwrap();
    match c
        .request(&Request::GetMissingFunctions {
            package: "com.example.app".into(),
        })
        .unwrap()
    {
        Response::MissingFunctions {
            updated_ms,
            entries,
        } => {
            assert!(updated_ms > 0);
            assert_eq!(entries[0].calls, 3);
        }
        r => panic!("{r:?}"),
    }
    match c
        .request(&Request::GetMissingFunctions {
            package: "..".into(),
        })
        .unwrap()
    {
        Response::Error {
            code: ErrorCode::InvalidPackageName,
            ..
        } => {}
        r => panic!("{r:?}"),
    }
    match c
        .request(&Request::StartActivity {
            package: "com.example.app".into(),
            activity: "a/b".into(),
        })
        .unwrap()
    {
        Response::Error {
            code: ErrorCode::InvalidPackageName,
            ..
        } => {}
        r => panic!("{r:?}"),
    }
}

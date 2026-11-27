//! The pipe server, in this test process: the real listener, server,
//! supervisor and request handling, with a client on the test thread. The
//! host processes are the real placeholder (`andruna-sysd --host`).

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use andruna_ipc::proto;
use andruna_ipc::{
    Client, Endpoint, ErrorCode, PROTOCOL_VERSION, PipeListener, PipeSecurity, Request, Response,
};
use andruna_sysd::{Activity, Context, Layout, Server, Supervisor};

struct Fixture {
    endpoint: Endpoint,
    root: PathBuf,
    ctx: Arc<Context>,
    server: Option<Server>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let instance = format!("t{}-{name}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-sysd-{instance}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let endpoint = Endpoint::for_current_user(&instance).unwrap();
        let listener =
            PipeListener::bind(&endpoint, &PipeSecurity::current_user_only(&endpoint)).unwrap();
        let layout = Layout::new(root.clone());
        let activity = Arc::new(Activity::new(Duration::from_secs(3600)));
        let supervisor = Arc::new(
            Supervisor::new(
                PathBuf::from(env!("CARGO_BIN_EXE_andruna-sysd")),
                layout.clone(),
                activity.clone(),
            )
            .unwrap(),
        );
        let ctx = Arc::new(Context {
            layout,
            activity,
            supervisor,
            started: Instant::now(),
        });
        let server = Server::spawn(listener, ctx.clone()).unwrap();
        Self {
            endpoint,
            root,
            ctx,
            server: Some(server),
        }
    }

    fn client(&self) -> Client {
        Client::connect(&self.endpoint, Duration::from_secs(5)).unwrap()
    }

    fn install(&self, package: &str) {
        fs::create_dir_all(self.root.join("app").join(package)).unwrap();
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
    f.install("org.example.two");
    f.install("com.example.one");
    assert_eq!(
        c.request(&Request::ListPackages).unwrap(),
        Response::Packages {
            packages: vec!["com.example.one".into(), "org.example.two".into()]
        }
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
    let f = Fixture::new("host");
    f.install("com.example.demo");
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
    // The placeholder shows its window shortly after it starts.
    let focus = Request::FocusWindow {
        package: "com.example.demo".into(),
    };
    let t = Instant::now();
    loop {
        match c.request(&focus).unwrap() {
            Response::Focused => break,
            Response::Error {
                code: ErrorCode::NoWindow,
                ..
            } if t.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            r => panic!("{r:?}"),
        }
    }
    f.wait_counts((1, 1));
    // Ending the job ends the host; the supervisor notices.
    f.ctx.supervisor.shutdown();
    f.wait_counts((1, 0));
    assert_eq!(f.ctx.supervisor.host_pid("com.example.demo"), None);
    // The placeholder logged that it is one.
    let log = fs::read_to_string(f.root.join("logs").join("host.log")).unwrap();
    assert!(log.contains("event=placeholder_host"), "{log}");
    assert!(log.contains("package=com.example.demo"), "{log}");
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

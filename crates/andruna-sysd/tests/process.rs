//! The andruna-sysd executable as a process: single instance, logging and
//! idle shutdown.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use andruna_ipc::{Client, Endpoint, Request, Response};

const SYSD: &str = env!("CARGO_BIN_EXE_andruna-sysd");

struct Instance {
    name: String,
    root: PathBuf,
}

impl Instance {
    fn new(test: &str) -> Self {
        let name = format!("p{}-{test}", std::process::id());
        let root = std::env::temp_dir().join(format!("andruna-sysd-{name}"));
        let _ = fs::remove_dir_all(&root);
        Self { name, root }
    }

    fn spawn(&self, idle_ms: u64) -> Child {
        Command::new(SYSD)
            .args(["--instance", &self.name, "--idle-timeout-ms"])
            .arg(idle_ms.to_string())
            .arg("--root")
            .arg(&self.root)
            .spawn()
            .unwrap()
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::for_current_user(&self.name).unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("logs").join("sysd.log")).unwrap_or_default()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_exit(child: &mut Child, limit: Duration) -> Option<i32> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if let Some(s) = child.try_wait().unwrap() {
            return s.code();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn second_instance_exits_and_first_keeps_serving() {
    let inst = Instance::new("single");
    let mut first = inst.spawn(60_000);
    let mut client = Client::connect(&inst.endpoint(), Duration::from_secs(10)).unwrap();
    assert_eq!(client.server_pid(), first.id());

    let mut second = inst.spawn(60_000);
    assert_eq!(
        wait_exit(&mut second, Duration::from_secs(10)),
        Some(andruna_sysd::EXIT_ALREADY_RUNNING)
    );
    // The first one is unaffected.
    match client.request(&Request::Status).unwrap() {
        Response::Status(s) => assert_eq!(s.pid, first.id()),
        r => panic!("{r:?}"),
    }
    assert!(first.try_wait().unwrap().is_none());
    // Another instance name is independent.
    let other = Instance::new("single-other");
    let mut third = other.spawn(60_000);
    let c3 = Client::connect(&other.endpoint(), Duration::from_secs(10)).unwrap();
    assert_eq!(c3.server_pid(), third.id());

    drop(client);
    drop(c3);
    first.kill().unwrap();
    third.kill().unwrap();
    let _ = first.wait();
    let _ = third.wait();
    let log = inst.log();
    assert!(log.contains("event=ready"), "{log}");
    assert!(log.contains("event=already_running"), "{log}");
}

#[test]
fn exits_when_idle() {
    let inst = Instance::new("idle");
    let mut sysd = inst.spawn(300);
    {
        // A connected client keeps it alive past the idle timeout.
        let mut c = Client::connect(&inst.endpoint(), Duration::from_secs(10)).unwrap();
        std::thread::sleep(Duration::from_millis(700));
        assert!(matches!(
            c.request(&Request::Ping).unwrap(),
            Response::Pong { .. }
        ));
        assert!(sysd.try_wait().unwrap().is_none());
    }
    let t = Instant::now();
    assert_eq!(
        wait_exit(&mut sysd, Duration::from_secs(10)),
        Some(andruna_sysd::EXIT_OK)
    );
    assert!(
        t.elapsed() >= Duration::from_millis(250),
        "{:?}",
        t.elapsed()
    );
    let log = inst.log();
    assert!(log.contains("event=idle_shutdown"), "{log}");
    assert!(log.contains("event=stopped"), "{log}");
    // The pipe is gone with it.
    assert!(matches!(
        Client::connect(&inst.endpoint(), Duration::ZERO),
        Err(andruna_ipc::Error::NotRunning)
    ));
}

#[test]
fn rejects_bad_command_line() {
    let s = Command::new(SYSD).arg("--bogus").status().unwrap();
    assert_eq!(s.code(), Some(andruna_sysd::EXIT_USAGE));
}

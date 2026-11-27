//! The transport on real pipes: a listener thread and a client.

use std::thread;
use std::time::{Duration, Instant};

use andruna_ipc::{Client, Endpoint, Error, PipeListener, PipeSecurity, Request, Response, Status};

fn endpoint(name: &str) -> Endpoint {
    Endpoint::for_current_user(&format!("i{}-{name}", std::process::id())).unwrap()
}

#[test]
fn request_response_over_a_pipe() {
    let ep = endpoint("rr");
    let mut listener = PipeListener::bind(&ep, &PipeSecurity::current_user_only(&ep)).unwrap();
    let server = thread::spawn(move || {
        let mut conn = listener.accept().unwrap();
        let mut seen = Vec::new();
        while let Some(p) = conn.read_frame().unwrap() {
            let (id, req) = Request::decode(&p).unwrap();
            let resp = match &req {
                Request::Status => Response::Status(Status {
                    pid: conn.peer_pid(),
                    uptime_ms: 1,
                    connections: 1,
                    hosts: 0,
                    idle_timeout_ms: 2,
                    version: "v".into(),
                }),
                _ => Response::Pong { protocol: 1 },
            };
            seen.push(req);
            conn.write_frame(&resp.encode(id)).unwrap();
        }
        seen
    });
    let mut c = Client::connect(&ep, Duration::from_secs(5)).unwrap();
    assert_eq!(c.server_pid(), std::process::id());
    assert_eq!(
        c.request(&Request::Ping).unwrap(),
        Response::Pong { protocol: 1 }
    );
    match c.request(&Request::Status).unwrap() {
        // The server sees the client's real process id.
        Response::Status(s) => assert_eq!(s.pid, std::process::id()),
        r => panic!("{r:?}"),
    }
    drop(c);
    assert_eq!(server.join().unwrap(), vec![Request::Ping, Request::Status]);
}

#[test]
fn second_listener_on_the_same_name_fails() {
    let ep = endpoint("dup");
    let sec = PipeSecurity::current_user_only(&ep);
    let _first = PipeListener::bind(&ep, &sec).unwrap();
    assert!(PipeListener::bind(&ep, &sec).is_err());
}

#[test]
fn connect_without_server_reports_not_running() {
    let ep = endpoint("none");
    let t = Instant::now();
    assert!(matches!(
        Client::connect(&ep, Duration::ZERO),
        Err(Error::NotRunning)
    ));
    assert!(matches!(
        Client::connect(&ep, Duration::from_millis(100)),
        Err(Error::NotRunning)
    ));
    assert!(t.elapsed() >= Duration::from_millis(100));
}

#[test]
fn waker_unblocks_accept() {
    let ep = endpoint("wake");
    let mut listener = PipeListener::bind(&ep, &PipeSecurity::current_user_only(&ep)).unwrap();
    let waker = listener.waker();
    let t = thread::spawn(move || listener.accept().map(|c| c.peer_pid()));
    thread::sleep(Duration::from_millis(50));
    waker.wake();
    assert_eq!(t.join().unwrap().unwrap(), std::process::id());
}

#[test]
fn many_clients_in_sequence() {
    let ep = endpoint("seq");
    let mut listener = PipeListener::bind(&ep, &PipeSecurity::current_user_only(&ep)).unwrap();
    let server = thread::spawn(move || {
        for _ in 0..20 {
            let mut conn = listener.accept().unwrap();
            thread::spawn(move || {
                while let Some(p) = conn.read_frame().unwrap() {
                    let (id, _) = Request::decode(&p).unwrap();
                    conn.write_frame(&Response::Focused.encode(id)).unwrap();
                }
            });
        }
    });
    let clients: Vec<_> = (0..20)
        .map(|_| {
            let ep = ep.clone();
            thread::spawn(move || {
                let mut c = Client::connect(&ep, Duration::from_secs(5)).unwrap();
                c.request(&Request::ListPackages).unwrap()
            })
        })
        .collect();
    for c in clients {
        assert_eq!(c.join().unwrap(), Response::Focused);
    }
    server.join().unwrap();
}

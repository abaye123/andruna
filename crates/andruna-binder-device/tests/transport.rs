//! The broker's server and the `/dev/binder` device end to end, across
//! processes: this test binary starts itself twice as guest-like
//! processes (a context manager and a client) that use the device
//! through andruna-lx exactly as bionic's system calls do (`openat`,
//! `mmap`, `ioctl`), with the binder protocol written by hand.
//!
//! Measures round trips and checks caller identity, file descriptor
//! passing and death notification.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use andruna_binder::host::Server;
use andruna_binder::transport::PIPE_ENV;
use andruna_binder::uapi::*;
use andruna_lx::UserBuf;
use andruna_lx::fd::FDS;
use andruna_lx::linux::fcntl::*;
use andruna_lx::linux::mman::*;

const ROLE: &str = "ANDRUNA_BINDER_TEST_ROLE";
const CLIENT_UID: u32 = 10_123;
const SM_UID: u32 = 1000;

fn ioctl(fd: i32, cmd: u32, arg: usize) -> i64 {
    let f = FDS.get(fd).unwrap();
    f.ops().ioctl(&f, cmd, arg).map_err(|e| e.0).unwrap()
}

struct Dev {
    fd: i32,
}

impl Dev {
    fn open() -> Self {
        andruna_binder_device::register();
        let fd = andruna_lx::fs::openat(AT_FDCWD, b"/dev/binder", O_RDWR | O_CLOEXEC, 0).unwrap();
        let mut v = 0i32;
        ioctl(fd, BINDER_VERSION, &mut v as *mut i32 as usize);
        assert_eq!(v, BINDER_CURRENT_PROTOCOL_VERSION);
        andruna_lx::mem::mmap(
            0,
            (1 << 20) - 8192,
            PROT_READ,
            MAP_PRIVATE | MAP_NORESERVE,
            fd,
            0,
        )
        .unwrap();
        Self { fd }
    }

    /// One BINDER_WRITE_READ; the read stream.
    fn wr(&self, w: &[u8], read: bool) -> Vec<u8> {
        let mut rb = vec![0u8; 256];
        let mut bwr = binder_write_read {
            write_size: w.len() as u64,
            write_buffer: w.as_ptr() as u64,
            read_size: if read { 256 } else { 0 },
            read_buffer: rb.as_mut_ptr() as u64,
            ..Default::default()
        };
        ioctl(self.fd, BINDER_WRITE_READ, &mut bwr as *mut _ as usize);
        assert_eq!(bwr.write_consumed, w.len() as u64);
        rb.truncate(bwr.read_consumed as usize);
        rb
    }
}

fn cmd(out: &mut Vec<u8>, c: u32) {
    out.extend_from_slice(&c.to_ne_bytes());
}

fn tx(out: &mut Vec<u8>, c: u32, handle: u32, code: u32, flags: u32, data: &[u8], offs: &[u64]) {
    cmd(out, c);
    let tr = binder_transaction_data {
        target: u64::from(handle),
        code,
        flags,
        data_size: data.len() as u64,
        offsets_size: (offs.len() * 8) as u64,
        buffer: data.as_ptr() as u64,
        offsets: offs.as_ptr() as u64,
        ..Default::default()
    };
    push(out, &tr);
}

/// The return commands of a read stream (transactions decoded).
fn parse(r: &[u8]) -> Vec<(u32, Option<binder_transaction_data>, u64)> {
    let mut v = Vec::new();
    let mut pos = 0;
    while pos < r.len() {
        let c: u32 = read_at(r, pos).unwrap();
        let a = pos + 4;
        pos = a + ioc_size(c);
        let tr = matches!(c, BR_TRANSACTION | BR_TRANSACTION_SEC_CTX | BR_REPLY)
            .then(|| read_at::<binder_transaction_data>(r, a).unwrap());
        let arg = if ioc_size(c) >= 8 {
            read_at::<u64>(r, a).unwrap()
        } else {
            0
        };
        v.push((c, tr, arg));
    }
    v
}

fn data_of(tr: &binder_transaction_data) -> &'static [u8] {
    // SAFETY: the receive buffer, valid until freed.
    unsafe { std::slice::from_raw_parts(tr.buffer as *const u8, tr.data_size as usize) }
}

/// Context manager: echoes every call; code 2 reads the passed file and
/// replies with its contents; code 3 replies with the caller's pid and
/// uid; code 99 exits the process.
fn run_context_manager() {
    let d = Dev::open();
    let obj = flat_binder_object {
        flags: FLAT_BINDER_FLAG_ACCEPTS_FDS,
        ..Default::default()
    };
    ioctl(d.fd, BINDER_SET_CONTEXT_MGR_EXT, &obj as *const _ as usize);
    let mut w = Vec::new();
    cmd(&mut w, BC_ENTER_LOOPER);
    let mut pending = w;
    loop {
        let r = d.wr(&pending, true);
        pending = Vec::new();
        for (c, tr, _) in parse(&r) {
            let Some(tr) = tr else { continue };
            if c == BR_REPLY {
                continue;
            }
            let data = data_of(&tr).to_vec();
            let reply: Vec<u8> = match tr.code {
                2 => {
                    // SAFETY: offsets inside the delivered buffer.
                    let off = unsafe { (tr.offsets as *const u64).read_unaligned() } as usize;
                    let obj: flat_binder_object = read_at(&data, off).unwrap();
                    assert_eq!(obj.hdr_type, BINDER_TYPE_FD);
                    let fd = obj.binder as u32 as i32;
                    let mut buf = [0u8; 64];
                    andruna_lx::fs::lseek(fd, 0, SEEK_SET).unwrap();
                    let n = andruna_lx::fs::read(fd, UserBuf::from_mut(&mut buf)).unwrap();
                    andruna_lx::fs::close(fd).unwrap();
                    buf[..n].to_vec()
                }
                3 => {
                    let mut v = tr.sender_pid.to_ne_bytes().to_vec();
                    v.extend_from_slice(&tr.sender_euid.to_ne_bytes());
                    v
                }
                99 => std::process::exit(0),
                _ => data,
            };
            cmd(&mut pending, BC_FREE_BUFFER);
            pending.extend_from_slice(&tr.buffer.to_ne_bytes());
            if tr.flags & TF_ONE_WAY == 0 {
                tx(&mut pending, BC_REPLY, 0, 0, 0, &reply, &[]);
            }
            // The reply buffer must outlive the ioctl that sends it.
            std::mem::forget(reply);
        }
    }
}

/// Calls handle 0 and waits for the reply; its data.
fn call(d: &Dev, code: u32, flags: u32, data: &[u8], offs: &[u64]) -> Vec<u8> {
    let mut w = Vec::new();
    tx(&mut w, BC_TRANSACTION, 0, code, flags, data, offs);
    let mut first = true;
    loop {
        let r = d.wr(if first { &w } else { &[] }, true);
        first = false;
        for (c, tr, _) in parse(&r) {
            if c == BR_DEAD_REPLY || c == BR_FAILED_REPLY {
                panic!("call failed: {c:#x}");
            }
            if c == BR_REPLY {
                let tr = tr.unwrap();
                let v = data_of(&tr).to_vec();
                let mut f = Vec::new();
                cmd(&mut f, BC_FREE_BUFFER);
                f.extend_from_slice(&tr.buffer.to_ne_bytes());
                d.wr(&f, false);
                return v;
            }
        }
    }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn run_client() {
    let d = Dev::open();
    // Identity comes from the broker, not from the caller.
    let r = call(&d, 3, 0, b"", &[]);
    let pid = i32::from_ne_bytes(r[..4].try_into().unwrap());
    let uid = u32::from_ne_bytes(r[4..8].try_into().unwrap());
    assert_eq!(pid as u32, std::process::id());
    assert_eq!(uid, CLIENT_UID);

    // Round trips with 100 bytes.
    let iters: usize = std::env::var("BINDER_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    for size in [100usize, 4096] {
        let payload = vec![0x5au8; size];
        for _ in 0..500 {
            call(&d, 1, 0, &payload, &[]);
        }
        let mut t = Vec::with_capacity(iters);
        for _ in 0..iters {
            let s = Instant::now();
            let r = call(&d, 1, 0, &payload, &[]);
            t.push(s.elapsed().as_secs_f64() * 1e6);
            assert_eq!(r.len(), size);
        }
        let p50 = percentile(&mut t, 0.5);
        let p99 = percentile(&mut t, 0.99);
        println!("ROUNDTRIP bytes={size} iters={iters} p50_us={p50:.1} p99_us={p99:.1}");
    }

    // A file descriptor: the server reads the file through the passed
    // descriptor.
    let root = std::env::temp_dir().join(format!("andruna-binder-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), b"passed through binder").unwrap();
    andruna_lx::fs::mount::mount("/", &root).unwrap();
    let fd = andruna_lx::fs::openat(AT_FDCWD, b"/hello.txt", O_RDONLY, 0).unwrap();
    let obj = flat_binder_object {
        hdr_type: BINDER_TYPE_FD,
        flags: 0,
        binder: u64::from(fd as u32),
        cookie: 0,
    };
    let mut data = Vec::new();
    push(&mut data, &obj);
    let r = call(&d, 2, TF_ACCEPT_FDS, &data, &[0]);
    assert_eq!(r, b"passed through binder");
    println!("FD_PASSING ok");

    // Death notification of the context manager.
    let mut w = Vec::new();
    cmd(&mut w, BC_ACQUIRE);
    w.extend_from_slice(&0u32.to_ne_bytes());
    cmd(&mut w, BC_REQUEST_DEATH_NOTIFICATION);
    w.extend_from_slice(&0u32.to_ne_bytes());
    w.extend_from_slice(&0xfeedu64.to_ne_bytes());
    cmd(&mut w, BC_ENTER_LOOPER);
    tx(&mut w, BC_TRANSACTION, 0, 99, TF_ONE_WAY, b"", &[]);
    let start = Instant::now();
    let mut first = true;
    loop {
        let r = d.wr(if first { &w } else { &[] }, true);
        first = false;
        if parse(&r)
            .iter()
            .any(|(c, _, a)| *c == BR_DEAD_BINDER && *a == 0xfeed)
        {
            break;
        }
    }
    println!(
        "DEATH ok after {:.2} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn child_role() {
    match std::env::var(ROLE).as_deref() {
        Ok("sm") => run_context_manager(),
        Ok("client") => run_client(),
        _ => {}
    }
}

#[test]
fn two_processes_through_the_broker() {
    let name = format!(r"\\.\pipe\andruna-binder-test-{}", std::process::id());
    let roles: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::default();
    let r2 = roles.clone();
    let server = Server::start(
        &name,
        Box::new(move |pid| {
            // A child may connect before its pid is recorded.
            for _ in 0..200 {
                if let Some(&(_, u)) = r2.lock().unwrap().iter().find(|(p, _)| *p == pid) {
                    return Some(u);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            None
        }),
    )
    .unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn = |role: &str| {
        Command::new(&exe)
            .args(["child_role", "--exact", "--nocapture", "--test-threads=1"])
            .env(ROLE, role)
            .env(PIPE_ENV, &name)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let sm = spawn("sm");
    roles.lock().unwrap().push((sm.id(), SM_UID));
    // The context manager must be registered before the client calls.
    let t = Instant::now();
    while server.broker().stats().nodes == 0 {
        assert!(t.elapsed().as_secs() < 20, "context manager did not start");
        std::thread::yield_now();
    }
    // Spawned processes are only accepted once their pid is known; the
    // client connects only after it starts, which is after this push.
    let client = spawn("client");
    roles.lock().unwrap().push((client.id(), CLIENT_UID));
    let out = client.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}");
    assert!(out.status.success(), "client failed");
    assert!(text.contains("FD_PASSING ok"));
    assert!(text.contains("DEATH ok"));
    let sm_out = sm.wait_with_output().unwrap();
    assert!(sm_out.status.success());
    let t = Instant::now();
    while server.broker().stats().procs != 0 {
        assert!(t.elapsed().as_secs() < 10, "processes not cleaned up");
        std::thread::yield_now();
    }
    assert_eq!(server.broker().stats().nodes, 0);
}

//! Client processes. Role "caller" (A) runs the benchmarks; role "server"
//! (B) answers relayed and direct transactions, receives passed handles and
//! subscribes to A's death.

use std::io;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::ring::*;
use crate::win::{self, Owned, Raw, Section};
use crate::winx;
use crate::wire::{self, Hello, Welcome};

pub struct Args {
    pub pipe: String,
    pub role: String,
    pub id: u32,
    pub peer: u32,
    pub iters: u32,
}

struct Conn {
    chan: Channel,
    rings: Vec<Ring>,
    _ev: Vec<Owned>,
    cpu: u32,
    id: u32,
    uid: u32,
    pipe: Owned,
}

fn connect(a: &Args) -> io::Result<Conn> {
    let pipe = winx::pipe_connect(&a.pipe)?;
    // The server deliberately lies about its pid: the broker must ignore it.
    let claimed = if a.role == "server" {
        4
    } else {
        std::process::id()
    };
    let hello = Hello {
        magic: MAGIC,
        claimed_pid: claimed,
        role: (a.role == "server") as u32,
        _r: 0,
    };
    let mut buf = [0u8; 256];
    let n = winx::pipe_transact(pipe.raw(), &wire::to_bytes(&hello), &mut buf)?;
    let w: Welcome = wire::from_bytes(&buf[..n]).ok_or_else(|| io::Error::other("bad welcome"))?;
    if w.magic != MAGIC || w.id != a.id {
        return Err(io::Error::other("unexpected welcome"));
    }
    let sec = Section::map(Owned(Raw(w.section as usize)), w.sec_size as usize)?;
    let chan = Channel { sec };
    let ev: Vec<Owned> = w.events.iter().map(|&e| Owned(Raw(e as usize))).collect();
    let rings = (0..4).map(|i| chan.ring(i, ev[i].raw())).collect();
    // Binder semantics: if the system server dies, the app dies with it.
    let bp = Raw(w.broker_proc as usize);
    std::thread::spawn(move || {
        win::wait_infinite(bp);
        eprintln!("client: broker died, exiting");
        std::process::exit(3);
    });
    Ok(Conn {
        chan,
        rings,
        _ev: ev,
        cpu: w.cpu,
        id: w.id,
        uid: w.uid,
        pipe,
    })
}

/// One calling thread's view of a request/reply ring pair.
struct Caller {
    tx: Ring,
    rx: Ring,
    shared: &'static SharedCfg,
    cfg: Cfg,
    cpu: u32,
    txn: u64,
    hot: Instant,
    reply: Vec<u8>,
    src: Vec<u8>,
}

impl Caller {
    fn new(tx: Ring, rx: Ring, shared: &'static SharedCfg, cpu: u32) -> Caller {
        let mut src = vec![0u8; MAX_PAYLOAD];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        Caller {
            tx,
            rx,
            shared,
            cfg: Cfg::default(),
            cpu,
            txn: 0,
            hot: Instant::now(),
            reply: Vec::with_capacity(MAX_PAYLOAD),
            src,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        code: u32,
        peer: u32,
        len: usize,
        flags: u32,
        handle: u64,
        arg: u64,
        arg2: u64,
    ) -> io::Result<MsgHdr> {
        self.shared.refresh(&mut self.cfg, self.cpu);
        self.txn += 1;
        let hdr = MsgHdr {
            kind: K_TXN,
            flags,
            len: len as u32,
            code,
            peer,
            txn: self.txn,
            handle,
            arg,
            arg2,
            ..Default::default()
        };
        let src = self.src.as_ptr();
        // Writing the parcel into the ring is the sender's one copy.
        let mut pending = self.tx.push(&hdr, |dst| {
            // SAFETY: `dst` has room for `len` bytes; `src` holds MAX_PAYLOAD bytes.
            unsafe { std::ptr::copy_nonoverlapping(src, dst, len) }
        });
        if flags & F_ONEWAY != 0 {
            if let Some(p) = pending {
                win::set_event(p);
            }
            return Ok(hdr);
        }
        loop {
            let m = self
                .rx
                .wait_msg(&self.cfg, &mut pending, &mut self.hot)
                .ok_or_else(|| io::Error::other("channel closed"))?;
            let h = m.hdr;
            if h.kind == K_REPLY && h.txn == self.txn {
                m.copy_payload(&mut self.reply);
                self.rx.consume(&m);
                return Ok(h);
            }
            self.rx.consume(&m);
        }
    }

    fn call_bytes(&mut self, code: u32, data: &[u8]) -> io::Result<MsgHdr> {
        self.src[..data.len()].copy_from_slice(data);
        self.call(code, 0, data.len(), 0, 0, 0, 0)
    }
}

pub fn run(a: Args) -> io::Result<()> {
    let conn: &'static Conn = Box::leak(Box::new(connect(&a)?));
    if a.role == "server" {
        run_server(conn)
    } else {
        run_caller(conn, &a)
    }
}

// ------------------------------------------------------------------ server

enum Event {
    Link(u32),
    Death {
        id: u32,
        exit: i64,
        detect: i64,
        recv: i64,
    },
}

fn run_server(conn: &'static Conn) -> io::Result<()> {
    let (etx, erx) = mpsc::channel::<Event>();
    let shared = &conn.chan.hdr().cfg;
    {
        let (rx, tx, cpu) = (conn.rings[3], conn.rings[2], conn.cpu);
        let etx = etx.clone();
        std::thread::spawn(move || serve(rx, tx, shared, cpu, Some(etx)));
    }
    let mut lane0 = Caller::new(conn.rings[0], conn.rings[1], shared, conn.cpu);
    for ev in erx {
        match ev {
            Event::Link(peer) => {
                lane0.call(C_LINK_DEATH, 0, 0, 0, 0, peer as u64, 0)?;
            }
            Event::Death {
                id,
                exit,
                detect,
                recv,
            } => {
                let text = format!(
                    "# death notice (client {} in AppContainer={} received death of client {id}):\n\
                     #   peer's last timestamp -> broker detected exit : {:8.1} us\n\
                     #   peer's last timestamp -> notice received here  : {:8.1} us",
                    conn.id,
                    conn.uid >= 10000,
                    qpc_to_us(detect - exit),
                    qpc_to_us(recv - exit),
                );
                lane0.call_bytes(C_REPORT, text.as_bytes())?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Server loop for incoming transactions on one ring pair.
fn serve(
    rx: Ring,
    tx: Ring,
    shared: &'static SharedCfg,
    cpu: u32,
    events: Option<mpsc::Sender<Event>>,
) {
    let mut cfg = Cfg::default();
    let mut pending: Option<Raw> = None;
    let mut hot = Instant::now();
    let mut buf = Vec::with_capacity(MAX_PAYLOAD);
    let mut fdbuf = [0u8; 256];
    loop {
        shared.refresh(&mut cfg, cpu);
        let Some(m) = rx.wait_msg(&cfg, &mut pending, &mut hot) else {
            return;
        };
        let h = m.hdr;
        let oneway = h.flags & F_ONEWAY != 0;
        let mut reply = MsgHdr {
            kind: K_REPLY,
            code: h.code,
            txn: h.txn,
            ..Default::default()
        };
        let mut data: &[u8] = &[];
        let mut send = !oneway && h.kind == K_TXN;
        match (h.kind, h.code) {
            (K_TXN, C_ECHO) => {
                m.copy_payload(&mut buf); // unparcel
                reply.len = (h.arg as usize).min(MAX_PAYLOAD) as u32;
            }
            (K_TXN, C_FD_READ) => {
                let mut n = 0;
                if h.handle != 0 {
                    n = winx::pipe_read(Raw(h.handle as usize), &mut fdbuf).unwrap_or(0);
                    win::close(Raw(h.handle as usize));
                }
                reply.arg = winx::handle_count() as u64;
                reply.arg2 = h.sender_pid as u64;
                data = &fdbuf[..n];
                reply.len = n as u32;
            }
            (K_TXN, C_FD_CLOSE) => {
                if h.handle != 0 {
                    win::close(Raw(h.handle as usize));
                }
                reply.arg = winx::handle_count() as u64;
            }
            (K_TXN, C_PAIR_OFFER) => {
                m.copy_payload(&mut buf);
                if let Some(hs) = wire::from_bytes::<[u64; 3]>(&buf) {
                    let peer = h.peer;
                    std::thread::spawn(move || pair_server(hs, shared, cpu));
                    if let Some(e) = &events {
                        let _ = e.send(Event::Link(peer));
                    }
                }
                send = false;
            }
            (K_DEATH, _) => {
                let recv = qpc();
                if let Some(e) = &events {
                    let _ = e.send(Event::Death {
                        id: h.arg as u32,
                        exit: h.arg2 as i64,
                        detect: h.txn as i64,
                        recv,
                    });
                }
                send = false;
            }
            _ => {}
        }
        if send {
            let len = reply.len as usize;
            let dp = data.as_ptr();
            let dl = data.len();
            let w = tx.push(&reply, |dst| {
                // SAFETY: `dst` has room for `len` bytes; we copy at most `dl <= len`.
                unsafe {
                    if dl > 0 {
                        std::ptr::copy_nonoverlapping(dp, dst, dl.min(len))
                    }
                }
            });
            if let Some(w) = w {
                if let Some(old) = pending.replace(w) {
                    win::set_event(old);
                }
            }
        }
        rx.consume(&m);
    }
}

fn pair_server(hs: [u64; 3], shared: &'static SharedCfg, cpu: u32) {
    let size = section_size(2);
    let Ok(sec) = Section::map(Owned(Raw(hs[0] as usize)), size) else {
        return;
    };
    let chan: &'static Channel = Box::leak(Box::new(Channel { sec }));
    let _e0 = Owned(Raw(hs[1] as usize));
    let _e1 = Owned(Raw(hs[2] as usize));
    // Ring 0: caller to server (we consume, event 0); ring 1: server to caller.
    serve(
        chan.ring(0, Raw(hs[1] as usize)),
        chan.ring(1, Raw(hs[2] as usize)),
        shared,
        cpu,
        None,
    );
}

// ------------------------------------------------------------------ caller

#[derive(Clone, Copy, PartialEq)]
enum Path {
    Broker,
    Relay,
    Direct,
}

impl Path {
    fn name(self) -> &'static str {
        match self {
            Path::Broker => "A<->broker",
            Path::Relay => "A->br->B",
            Path::Direct => "A<->B direct",
        }
    }
}

struct Stats {
    p50: f64,
    p99: f64,
    p999: f64,
    max: f64,
    mean: f64,
}

fn stats(mut v: Vec<u64>) -> Stats {
    v.sort_unstable();
    let n = v.len();
    let at = |q: f64| v[((n as f64 * q) as usize).min(n - 1)] as f64 / 1000.0;
    Stats {
        p50: at(0.50),
        p99: at(0.99),
        p999: at(0.999),
        max: v[n - 1] as f64 / 1000.0,
        mean: v.iter().sum::<u64>() as f64 / n as f64 / 1000.0,
    }
}

fn cpu_sample(c: &mut Caller) -> u64 {
    if c.call(C_CPU, 0, 0, 0, 0, 0, 0).is_err() {
        return 0;
    }
    let v: [u64; 4] = wire::from_bytes(&c.reply).unwrap_or_default();
    v.iter().sum()
}

fn config(c: &mut Caller, spin_ns: u32, soaw: bool, pinned: bool) -> io::Result<()> {
    c.call(
        C_CONFIG,
        0,
        0,
        0,
        0,
        spin_ns as u64,
        soaw as u64 | ((pinned as u64) << 1),
    )?;
    Ok(())
}

const MODES: &[(&str, u32, bool)] = &[
    ("event", 0, false),
    ("event+SOAW", 0, true),
    ("spin5us+event", 5_000, false),
    ("spin50us+event", 50_000, false),
    ("spin-while-hot", u32::MAX, false),
];

fn header() {
    println!(
        "{:<6} {:<15} {:<13} {:>6} {:>8} {:>8} {:>8} {:>9} {:>8} {:>10}",
        "pinned",
        "wake",
        "path",
        "bytes",
        "p50 us",
        "p99 us",
        "p99.9 us",
        "max us",
        "mean us",
        "cpu us/rt"
    );
}

fn row(pinned: bool, wake: &str, path: &str, bytes: usize, s: &Stats, cpu_per_rt: f64) {
    println!(
        "{:<6} {:<15} {:<13} {:>6} {:>8.2} {:>8.2} {:>8.2} {:>9.1} {:>8.2} {:>10.2}",
        if pinned { "yes" } else { "no" },
        wake,
        path,
        bytes,
        s.p50,
        s.p99,
        s.p999,
        s.max,
        s.mean,
        cpu_per_rt
    );
}

fn run_caller(conn: &'static Conn, a: &Args) -> io::Result<()> {
    let shared = &conn.chan.hdr().cfg;
    let mut lane0 = Caller::new(conn.rings[0], conn.rings[1], shared, conn.cpu);
    println!(
        "# caller A: id {} uid {} pid {}",
        conn.id,
        conn.uid,
        std::process::id()
    );
    println!("# QPC frequency: {} Hz", qpc_freq());

    // Direct channel to B, set up by the broker.
    let r = lane0.call(C_PAIR, 0, 0, 0, 0, a.peer as u64, 0)?;
    if r.arg != 0 {
        return Err(io::Error::other("pair setup failed"));
    }
    let hs: [u64; 3] =
        wire::from_bytes(&lane0.reply).ok_or_else(|| io::Error::other("bad pair reply"))?;
    let pair_sec = Section::map(Owned(Raw(hs[0] as usize)), section_size(2))?;
    let pair: &'static Channel = Box::leak(Box::new(Channel { sec: pair_sec }));
    let _pe = (Owned(Raw(hs[1] as usize)), Owned(Raw(hs[2] as usize)));
    let mut direct = Caller::new(
        pair.ring(0, Raw(hs[1] as usize)),
        pair.ring(1, Raw(hs[2] as usize)),
        shared,
        conn.cpu,
    );

    let iters = a.iters as usize;
    let warm = (iters / 10).max(200);
    let peer = a.peer;

    println!("\n## Round trips ({iters} iterations after {warm} warm-up; reply is 100 bytes)");
    header();
    let mut idle_rows = vec![];
    for pinned in [true, false] {
        for &(wname, spin, soaw) in MODES {
            config(&mut lane0, spin, soaw, pinned)?;
            for path in [Path::Broker, Path::Relay, Path::Direct] {
                for size in [100usize, 64 * 1024] {
                    let mut lat = Vec::with_capacity(iters);
                    let c0 = cpu_sample(&mut lane0);
                    for i in 0..warm + iters {
                        let t = Instant::now();
                        match path {
                            Path::Broker => lane0.call(C_ECHO, 0, size, 0, 0, 100, 0)?,
                            Path::Relay => lane0.call(C_ECHO, peer, size, 0, 0, 100, 0)?,
                            Path::Direct => direct.call(C_ECHO, 0, size, 0, 0, 100, 0)?,
                        };
                        if i >= warm {
                            lat.push(t.elapsed().as_nanos() as u64);
                        }
                    }
                    let c1 = cpu_sample(&mut lane0);
                    let cpu = (c1 - c0) as f64 / 10.0 / (warm + iters) as f64;
                    row(pinned, wname, path.name(), size, &stats(lat), cpu);
                }
            }
            if pinned {
                // Idle cost: let every ring go cold, then measure 1 s of nothing.
                std::thread::sleep(Duration::from_millis(50));
                let c0 = cpu_sample(&mut lane0);
                std::thread::sleep(Duration::from_secs(1));
                let c1 = cpu_sample(&mut lane0);
                idle_rows.push((wname, (c1 - c0) as f64 / 1e7 * 100.0));
            }
        }
        // Kernel IPC baseline: message-mode named pipe, TransactNamedPipe.
        let mut out = vec![0u8; 128 * 1024];
        let mut req = vec![0u8; 64 * 1024];
        req[..4].copy_from_slice(&100u32.to_le_bytes());
        for size in [100usize, 64 * 1024] {
            let mut lat = Vec::with_capacity(iters);
            let c0 = cpu_sample(&mut lane0);
            for i in 0..warm + iters {
                let t = Instant::now();
                winx::pipe_transact(conn.pipe.raw(), &req[..size], &mut out)?;
                if i >= warm {
                    lat.push(t.elapsed().as_nanos() as u64);
                }
            }
            let c1 = cpu_sample(&mut lane0);
            let cpu = (c1 - c0) as f64 / 10.0 / (warm + iters) as f64;
            row(pinned, "named pipe", "A<->broker", size, &stats(lat), cpu);
        }
    }

    println!("\n## Idle CPU (all 3 processes, 1 s with no traffic, pinned; % of one CPU)");
    for (w, pct) in &idle_rows {
        println!("{w:<15} {pct:6.2} %");
    }

    println!("\n## Oneway throughput (100-byte oneway calls, then one synchronous call to drain; pinned)");
    for &(wname, spin, soaw) in &[MODES[0], MODES[3]] {
        config(&mut lane0, spin, soaw, true)?;
        for (pname, p) in [("A->broker", 0u32), ("A->br->B", peer)] {
            let n = iters * 5;
            let c0 = cpu_sample(&mut lane0);
            let t = Instant::now();
            for _ in 0..n {
                lane0.call(C_ECHO, p, 100, F_ONEWAY, 0, 0, 0)?;
            }
            lane0.call(C_ECHO, p, 100, 0, 0, 100, 0)?;
            let el = t.elapsed();
            let c1 = cpu_sample(&mut lane0);
            println!(
                "{wname:<15} {pname:<10} {n} calls: {:8.0} ns/call, {:6.2} M calls/s, cpu {:.2} us/call",
                el.as_nanos() as f64 / n as f64,
                n as f64 / el.as_secs_f64() / 1e6,
                (c1 - c0) as f64 / 10.0 / n as f64
            );
        }
    }

    println!("\n## Handle (fd) passing through the broker (DuplicateHandle into B)");
    config(&mut lane0, 0, false, true)?;
    {
        let (r, w) = winx::anon_pipe()?;
        let msg = format!(
            "hello through a passed handle from pid {}",
            std::process::id()
        );
        winx::pipe_write(w.raw(), msg.as_bytes())?;
        drop(w);
        let h = lane0.call(C_FD_READ, peer, 0, F_HANDLE, r.raw().0 as u64, 0, 0)?;
        let got = String::from_utf8_lossy(&lane0.reply).to_string();
        println!(
            "# B read from the passed pipe handle: {:?} -> {}",
            got,
            if got == msg { "OK" } else { "MISMATCH" }
        );
        println!(
            "# B saw sender pid {} (stamped by broker; A's pid is {})",
            h.arg2,
            std::process::id()
        );
    }
    for (pinned, (wname, spin, soaw)) in [
        (true, MODES[0]),
        (true, MODES[3]),
        (false, MODES[0]),
        (false, MODES[3]),
    ] {
        config(&mut lane0, spin, soaw, pinned)?;
        let ev = win::event_auto()?;
        let mut lat = Vec::with_capacity(iters);
        let mut first_count = 0;
        let mut last_count = 0;
        for i in 0..warm + iters {
            let t = Instant::now();
            let h = lane0.call(C_FD_CLOSE, peer, 100, F_HANDLE, ev.raw().0 as u64, 0, 0)?;
            if i >= warm {
                lat.push(t.elapsed().as_nanos() as u64);
            }
            if i == 0 {
                first_count = h.arg;
            }
            last_count = h.arg;
        }
        header();
        row(pinned, wname, "A->br->B +fd", 100, &stats(lat), f64::NAN);
        println!("# B handle count after first pass {first_count}, after last pass {last_count} (no leak if equal)");
    }

    // Death notification: B subscribed to our death when it got the pair offer.
    config(&mut lane0, 0, false, false)?;
    std::thread::sleep(Duration::from_millis(100));
    println!("\n## Death notification");
    conn.chan.hdr().exit_qpc.store(qpc(), SeqCst);
    std::process::exit(0);
}

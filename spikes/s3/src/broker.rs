//! The broker (stands in for sysd's Binder module): authenticates clients
//! over a handshake pipe, creates and hands out shared-memory channels,
//! serves broker-local calls, relays transactions between clients with
//! identity stamping and handle translation, and delivers death notices.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::System::Threading::{
    PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use crate::ring::*;
use crate::win::{self, Owned, Raw, Section, TokenInfo};
use crate::winx;
use crate::wire::{self, Hello, Welcome};

pub const AC_NAME: &str = "Andruna.Spike.S3";
const MAX_CLIENTS: usize = 4;

pub struct Client {
    pub id: u32,
    pub pid: u32,
    pub uid: u32,
    pub proc_: Owned,
    /// Verified token identity; kept for per-call permission checks in round 2.
    pub _info: TokenInfo,
    pub chan: Channel,
    /// Rings with this process's event handles: 2*lane = client to broker,
    /// 2*lane+1 = broker to client.
    pub rings: Vec<Ring>,
    _events: Vec<Owned>,
    /// Serializes broker-side producers of the broker-to-client rings.
    pub tx_lock: Vec<Mutex<()>>,
    pub cpu: u32,
    pub death_subs: Mutex<Vec<u32>>,
}

struct Pending {
    caller: u32,
    lane: u32,
    target: u32,
    orig_txn: u64,
}

pub struct Broker {
    clients: [OnceLock<Arc<Client>>; MAX_CLIENTS],
    pending: Mutex<HashMap<u64, Pending>>,
    next_txn: AtomicU64,
    cfg_gen: AtomicU32,
    pub alive: AtomicU32,
}

impl Broker {
    fn client(&self, id: u32) -> Option<&Arc<Client>> {
        self.clients.get(id as usize).and_then(|c| c.get())
    }
}

pub struct Args {
    pub appcontainer: bool,
    pub iters: u32,
    pub cpus: Option<Vec<u32>>,
}

/// CPU plan: (caller A, broker threads for A, broker threads for B, server B).
fn cpu_plan(args: &Args) -> [u32; 4] {
    if let Some(c) = &args.cpus {
        let g = |i: usize| c[i % c.len()];
        return [g(0), g(1), g(2), g(3)];
    }
    let classes = win::cpu_classes();
    let maxc = classes.iter().map(|c| c.1).max().unwrap_or(0);
    let p: Vec<u32> = classes
        .iter()
        .filter(|c| c.1 == maxc)
        .map(|c| c.0)
        .collect();
    let p = if p.is_empty() { vec![0] } else { p };
    let g = |i: usize| p[i % p.len()];
    // Leave CPU 0 (most interrupt work) to the least busy role when possible.
    if p.len() >= 4 {
        [g(1), g(2), g(3), g(0)]
    } else {
        [g(0), g(1 % p.len()), g(2 % p.len()), g(3 % p.len())]
    }
}

pub fn run(args: Args) -> io::Result<()> {
    let classes = win::cpu_classes();
    println!("# S3 broker pid {}", std::process::id());
    println!(
        "# logical CPUs (index:efficiency class): {}",
        classes
            .iter()
            .map(|(i, c)| format!("{i}:{c}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let plan = cpu_plan(&args);
    println!(
        "# pinning plan: caller A -> CPU {}, broker(A) -> CPU {}, broker(B) -> CPU {}, server B -> CPU {}",
        plan[0], plan[1], plan[2], plan[3]
    );

    let me = std::env::current_exe()?;
    let exe = me.to_string_lossy().to_string();
    let user = win::my_user_sid();
    let mut ac_sid = None;
    if args.appcontainer {
        let sid = winx::appcontainer_profile(AC_NAME)?;
        // The AppContainer must be able to read and execute the binary.
        let dir = me.parent().unwrap().to_string_lossy().to_string();
        let st = std::process::Command::new("icacls")
            .args([&dir, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)"])
            .stdout(std::process::Stdio::null())
            .status()?;
        if !st.success() {
            return Err(io::Error::other("icacls failed"));
        }
        println!("# AppContainer profile {AC_NAME}: {sid}");
        ac_sid = Some(sid);
    }

    // Pipe DACL: SYSTEM and this user full, AppContainers read/write; low
    // integrity label so AppContainer (low IL) clients can write.
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user})(A;;GRGW;;;AC)S:(ML;;NW;;;LW)");
    let sa = win::Sddl::new(&sddl)?;
    let pipe_name = format!(r"\\.\pipe\andruna-s3-{}-{:x}", std::process::id(), qpc());

    let broker = Arc::new(Broker {
        clients: Default::default(),
        pending: Mutex::new(HashMap::new()),
        next_txn: AtomicU64::new(1),
        cfg_gen: AtomicU32::new(1),
        alive: AtomicU32::new(0),
    });

    // Client B (id 1): server role, optionally in an AppContainer.
    let mut first = true;
    let b_cmd = format!("\"{exe}\" client --pipe {pipe_name} --role server --id 1");
    let (b_proc, b_pid) = if args.appcontainer {
        winx::spawn_in_appcontainer(AC_NAME, &b_cmd)?
    } else {
        spawn_plain(
            &exe,
            &[
                "client", "--pipe", &pipe_name, "--role", "server", "--id", "1",
            ],
        )?
    };
    accept_client(
        &broker, &pipe_name, &mut first, &sa, 1, b_pid, plan[2], plan[3],
    )?;

    // Client A (id 2): caller role, runs the benchmarks.
    let iters = args.iters.to_string();
    let (a_proc, a_pid) = spawn_plain(
        &exe,
        &[
            "client", "--pipe", &pipe_name, "--role", "caller", "--id", "2", "--peer", "1",
            "--iters", &iters,
        ],
    )?;
    accept_client(
        &broker, &pipe_name, &mut first, &sa, 2, a_pid, plan[1], plan[0],
    )?;

    win::wait_infinite(a_proc.raw());
    win::wait_infinite(b_proc.raw());
    // Let death watchers finish printing.
    while broker.alive.load(SeqCst) != 0 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if ac_sid.is_some() {
        winx::delete_appcontainer_profile(AC_NAME);
    }
    println!("# broker done");
    Ok(())
}

fn spawn_plain(exe: &str, args: &[&str]) -> io::Result<(Owned, u32)> {
    let child = std::process::Command::new(exe).args(args).spawn()?;
    let pid = child.id();
    // Keep our own handle; the Child is dropped without waiting.
    let h = win::open_process(pid, PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION)?;
    drop(child);
    Ok((h, pid))
}

#[allow(clippy::too_many_arguments)]
fn accept_client(
    broker: &Arc<Broker>,
    pipe_name: &str,
    first: &mut bool,
    sa: &win::Sddl,
    id: u32,
    spawned_pid: u32,
    broker_cpu: u32,
    client_cpu: u32,
) -> io::Result<()> {
    let pipe = winx::pipe_server(pipe_name, *first, sa)?;
    *first = false;
    winx::pipe_accept(pipe.raw())?;
    let mut buf = vec![0u8; 256];
    let n = winx::pipe_read(pipe.raw(), &mut buf)?;
    let hello: Hello = wire::from_bytes(&buf[..n]).ok_or_else(|| io::Error::other("bad hello"))?;

    // Identity: never from the message. PID from the pipe, token from
    // impersonation (identification level) and from the process.
    let pipe_pid = winx::pipe_client_pid(pipe.raw())?;
    let (imp_tok, level) = win::impersonation_token_of_pipe_client(pipe.raw())?;
    let imp = win::token_info(imp_tok.raw())?;
    let proc_ = win::open_process(
        pipe_pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_DUP_HANDLE | PROCESS_SYNCHRONIZE,
    )?;
    let ptok = win::process_token(proc_.raw())?;
    let info = win::token_info(ptok.raw())?;
    let consistent = imp.user_sid == info.user_sid && imp.appcontainer_sid == info.appcontainer_sid;
    println!("# client {id} identity:");
    println!("#   claimed pid (untrusted)        : {}", hello.claimed_pid);
    println!("#   pipe client pid                : {pipe_pid}");
    println!(
        "#   pid we spawned                 : {spawned_pid}  match={}",
        pipe_pid == spawned_pid
    );
    println!("#   impersonation level granted    : {level} (0=anonymous 1=identification 2=impersonation 3=delegation)");
    println!("#   user SID (process token)       : {}", info.user_sid);
    println!(
        "#   AppContainer                   : {} {}",
        info.is_appcontainer,
        info.appcontainer_sid.clone().unwrap_or_default()
    );
    println!(
        "#   integrity RID                  : 0x{:x}",
        info.integrity_rid
    );
    println!("#   impersonation token == process token identity: {consistent}");
    if !consistent || pipe_pid != spawned_pid {
        return Err(io::Error::other("identity mismatch, client rejected"));
    }
    let uid = match &info.appcontainer_sid {
        // Spike rule only: production assigns a stable uid per package at install.
        Some(s) => {
            10000
                + (s.bytes()
                    .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
                    % 9000)
        }
        None => 1000 + id,
    };
    println!("#   assigned uid (spike rule)      : {uid}");

    // Channel: 2 lanes (lane 0 = client's outgoing calls, lane 1 = incoming).
    let lanes = 2usize;
    let size = section_size(2 * lanes);
    let sec = Section::create(size)?;
    let chan = Channel { sec };
    {
        // SAFETY: fresh mapping, nobody else sees it yet.
        let h = unsafe { &mut *(chan.sec.base as *mut ChanHdr) };
        h.magic = MAGIC;
        h.id = id;
        h.rings = (2 * lanes) as u32;
    }
    let mut events = vec![];
    let mut rings = vec![];
    for i in 0..2 * lanes {
        let e = win::event_auto()?;
        rings.push(chan.ring(i, e.raw()));
        events.push(e);
    }
    let me = win::current_process();
    let dst = proc_.raw();
    let mut w = Welcome {
        magic: MAGIC,
        id,
        uid,
        cpu: client_cpu,
        broker_pid: std::process::id(),
        section: win::dup_into(me, chan.sec.handle.raw(), dst)?.0 as u64,
        sec_size: size as u64,
        broker_proc: 0,
        events: [0; 4],
        _r: 0,
    };
    for (i, e) in events.iter().enumerate() {
        w.events[i] = win::dup_into(me, e.raw(), dst)?.0 as u64;
    }
    // A SYNCHRONIZE-only handle to ourselves, so the client can notice our death.
    let selfh = win::open_process(std::process::id(), PROCESS_SYNCHRONIZE)?;
    w.broker_proc = win::dup_into(me, selfh.raw(), dst)?.0 as u64;
    winx::pipe_write(pipe.raw(), &wire::to_bytes(&w))?;

    let client = Arc::new(Client {
        id,
        pid: pipe_pid,
        uid,
        proc_,
        _info: info,
        chan,
        rings,
        _events: events,
        tx_lock: (0..lanes).map(|_| Mutex::new(())).collect(),
        cpu: broker_cpu,
        death_subs: Mutex::new(vec![]),
    });
    let _ = broker.clients[id as usize].set(client.clone());
    broker.alive.fetch_add(1, SeqCst);

    for lane in 0..lanes as u32 {
        let (b, c) = (broker.clone(), client.clone());
        std::thread::spawn(move || lane_loop(&b, &c, lane));
    }
    {
        // Named pipe baseline server on the same handshake pipe.
        let c = client.clone();
        std::thread::spawn(move || pipe_loop(pipe, &c));
    }
    {
        let (b, c) = (broker.clone(), client.clone());
        std::thread::spawn(move || death_watch(&b, &c));
    }
    Ok(())
}

/// Named pipe request/reply baseline: first 4 bytes of a request are the
/// requested reply size.
fn pipe_loop(pipe: Owned, c: &Client) {
    let mut cfg = Cfg::default();
    let mut buf = vec![0u8; 128 * 1024];
    let reply = vec![0u8; 64 * 1024];
    loop {
        c.chan.hdr().cfg.refresh(&mut cfg, c.cpu);
        let n = match winx::pipe_read(pipe.raw(), &mut buf) {
            Ok(n) if n >= 4 => n,
            _ => return,
        };
        let _ = n;
        let want = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        if winx::pipe_write(pipe.raw(), &reply[..want.min(reply.len())]).is_err() {
            return;
        }
    }
}

fn lane_loop(b: &Broker, c: &Arc<Client>, lane: u32) {
    let rx = c.rings[2 * lane as usize];
    let mut cfg = Cfg::default();
    let mut pending: Option<Raw> = None;
    let mut buf: Vec<u8> = Vec::with_capacity(MAX_PAYLOAD);
    let mut hot = std::time::Instant::now();
    loop {
        c.chan.hdr().cfg.refresh(&mut cfg, c.cpu);
        let Some(m) = rx.wait_msg(&cfg, &mut pending, &mut hot) else {
            return;
        };
        let h = m.hdr;
        let wake = match h.kind {
            K_TXN if h.peer == 0 => serve_local(b, c, lane, &m, &mut buf),
            K_TXN => relay(b, c, lane, &m),
            K_REPLY => route_reply(b, c, &m),
            _ => None,
        };
        rx.consume(&m);
        if let Some(w) = wake {
            if let Some(old) = pending.replace(w) {
                win::set_event(old);
            }
        }
    }
}

fn reply_hdr(req: &MsgHdr, len: u32) -> MsgHdr {
    MsgHdr {
        kind: K_REPLY,
        len,
        code: req.code,
        txn: req.txn,
        ..Default::default()
    }
}

/// Pushes to client `to`'s broker-to-client ring of `lane`.
fn push_to(to: &Client, lane: u32, hdr: &MsgHdr, fill: impl FnOnce(*mut u8)) -> Option<Raw> {
    let _g = to.tx_lock[lane as usize].lock().unwrap();
    to.rings[2 * lane as usize + 1].push(hdr, fill)
}

fn push_bytes(to: &Client, lane: u32, hdr: &MsgHdr, data: &[u8]) -> Option<Raw> {
    push_to(to, lane, hdr, |dst| {
        // SAFETY: `dst` has room for `hdr.len == data.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) }
    })
}

fn serve_local(b: &Broker, c: &Client, lane: u32, m: &Msg, buf: &mut Vec<u8>) -> Option<Raw> {
    let h = m.hdr;
    // Work on a private copy of the request (the sender may modify shared memory).
    m.copy_payload(buf);
    let oneway = h.flags & F_ONEWAY != 0;
    match h.code {
        C_ECHO => {
            if oneway {
                return None;
            }
            let n = (h.arg as usize).min(MAX_PAYLOAD) as u32;
            push_to(c, lane, &reply_hdr(&h, n), |_| {})
        }
        C_CONFIG => {
            let cfg = Cfg {
                gen: b.cfg_gen.fetch_add(1, SeqCst) + 1,
                spin_ns: h.arg as u32,
                soaw: h.arg2 & 1 != 0,
                pinned: h.arg2 & 2 != 0,
            };
            for cl in b.clients.iter().filter_map(|x| x.get()) {
                cl.chan.hdr().cfg.store(&cfg);
            }
            push_to(c, lane, &reply_hdr(&h, 0), |_| {})
        }
        C_CPU => {
            let mut v = [0u64; MAX_CLIENTS];
            v[0] = win::process_cpu(win::current_process());
            for cl in b.clients.iter().filter_map(|x| x.get()) {
                v[cl.id as usize] = win::process_cpu(cl.proc_.raw());
            }
            push_bytes(
                c,
                lane,
                &reply_hdr(&h, 8 * MAX_CLIENTS as u32),
                &wire::to_bytes(&v),
            )
        }
        C_LINK_DEATH => {
            if let Some(t) = b.client(h.arg as u32) {
                t.death_subs.lock().unwrap().push(c.id);
            }
            push_to(c, lane, &reply_hdr(&h, 0), |_| {})
        }
        C_PAIR => {
            let mut r = reply_hdr(&h, 0);
            match make_pair(c, b.client(h.arg as u32)) {
                Ok((mine, theirs, peer)) => {
                    // Offer the other end to the peer as an incoming oneway transaction.
                    let offer = MsgHdr {
                        kind: K_TXN,
                        flags: F_ONEWAY,
                        code: C_PAIR_OFFER,
                        len: 24,
                        peer: c.id,
                        sender_pid: c.pid,
                        sender_uid: c.uid,
                        ..Default::default()
                    };
                    if let Some(w) = push_bytes(peer, 1, &offer, &wire::to_bytes(&theirs)) {
                        win::set_event(w);
                    }
                    r.len = 24;
                    push_bytes(c, lane, &r, &wire::to_bytes(&mine))
                }
                Err(e) => {
                    eprintln!("pair failed: {e}");
                    r.arg = 1;
                    push_to(c, lane, &r, |_| {})
                }
            }
        }
        C_REPORT => {
            println!("{}", String::from_utf8_lossy(buf));
            if oneway {
                None
            } else {
                push_to(c, lane, &reply_hdr(&h, 0), |_| {})
            }
        }
        _ => {
            let mut r = reply_hdr(&h, 0);
            r.arg = u64::MAX;
            push_to(c, lane, &r, |_| {})
        }
    }
}

/// Creates a direct channel between `a` and `p` (2 rings: a to p, p to a).
/// Returns the handle triples (section, ev0, ev1) valid in a and in p.
fn make_pair<'a>(
    a: &Client,
    p: Option<&'a Arc<Client>>,
) -> io::Result<([u64; 3], [u64; 3], &'a Arc<Client>)> {
    let p = p.ok_or_else(|| io::Error::other("no such peer"))?;
    let size = section_size(2);
    let sec = Section::create(size)?;
    // SAFETY: fresh mapping.
    unsafe {
        let h = &mut *(sec.base as *mut ChanHdr);
        h.magic = MAGIC;
        h.rings = 2;
    }
    let e0 = win::event_auto()?;
    let e1 = win::event_auto()?;
    let me = win::current_process();
    let mut out = [[0u64; 3]; 2];
    for (k, cl) in [a.proc_.raw(), p.proc_.raw()].into_iter().enumerate() {
        out[k][0] = win::dup_into(me, sec.handle.raw(), cl)?.0 as u64;
        out[k][1] = win::dup_into(me, e0.raw(), cl)?.0 as u64;
        out[k][2] = win::dup_into(me, e1.raw(), cl)?.0 as u64;
    }
    Ok((out[0], out[1], p))
}

fn relay(b: &Broker, c: &Client, lane: u32, m: &Msg) -> Option<Raw> {
    let h = m.hdr;
    let Some(t) = b.client(h.peer) else {
        let mut r = reply_hdr(&h, 0);
        r.arg = u64::MAX;
        return push_to(c, lane, &r, |_| {});
    };
    let mut out = h;
    if h.flags & F_HANDLE != 0 {
        // fd passing: duplicate the sender's handle into the target. The
        // sender can only name handles in its own table.
        out.handle = match win::dup_into(c.proc_.raw(), Raw(h.handle as usize), t.proc_.raw()) {
            Ok(r) => r.0 as u64,
            Err(_) => 0,
        };
    }
    out.peer = c.id;
    out.sender_pid = c.pid;
    out.sender_uid = c.uid;
    let oneway = h.flags & F_ONEWAY != 0;
    if !oneway {
        let id = b.next_txn.fetch_add(1, Relaxed);
        b.pending.lock().unwrap().insert(
            id,
            Pending {
                caller: c.id,
                lane,
                target: t.id,
                orig_txn: h.txn,
            },
        );
        out.txn = id;
    }
    let src = m.payload_ptr();
    let len = h.len as usize;
    // The one copy: sender's ring slot straight into the receiver's ring slot.
    push_to(t, 1, &out, |dst| {
        // SAFETY: both ranges are `len` bytes inside their rings.
        unsafe { std::ptr::copy_nonoverlapping(src, dst, len) }
    })
}

fn route_reply(b: &Broker, c: &Client, m: &Msg) -> Option<Raw> {
    let h = m.hdr;
    let p = {
        let mut map = b.pending.lock().unwrap();
        match map.get(&h.txn) {
            // Only the client the transaction was sent to may answer it.
            Some(p) if p.target == c.id => map.remove(&h.txn).unwrap(),
            _ => return None,
        }
    };
    let caller = b.client(p.caller)?;
    let mut out = h;
    out.txn = p.orig_txn;
    out.peer = c.id;
    out.sender_pid = c.pid;
    out.sender_uid = c.uid;
    let src = m.payload_ptr();
    let len = h.len as usize;
    push_to(caller, p.lane, &out, |dst| {
        // SAFETY: both ranges are `len` bytes inside their rings.
        unsafe { std::ptr::copy_nonoverlapping(src, dst, len) }
    })
}

fn death_watch(b: &Broker, c: &Client) {
    win::wait_infinite(c.proc_.raw());
    let t_detect = qpc();
    let exit_qpc = c.chan.hdr().exit_qpc.load(SeqCst);
    for r in &c.rings {
        r.close();
    }
    let subs = c.death_subs.lock().unwrap().clone();
    for s in subs {
        if let Some(t) = b.client(s) {
            let d = MsgHdr {
                kind: K_DEATH,
                flags: F_ONEWAY,
                peer: c.id,
                arg: c.id as u64,
                arg2: exit_qpc as u64,
                txn: t_detect as u64,
                ..Default::default()
            };
            push_to(t, 1, &d, |_| {});
            // Signal unconditionally: cheap, and a death notice must not be lost.
            win::set_event(t.rings[3].ev);
        }
    }
    println!(
        "# broker: client {} (pid {}) exited; detected {:.1} us after its last timestamp; notified {} subscriber(s)",
        c.id,
        c.pid,
        if exit_qpc != 0 { qpc_to_us(t_detect - exit_qpc) } else { f64::NAN },
        c.death_subs.lock().unwrap().len()
    );
    b.alive.fetch_sub(1, SeqCst);
}

//! The broker's server, run in sysd: accepts guest processes on the
//! control pipe and runs one broker thread per guest thread (see
//! [`crate::transport`]).

use std::any::Any;
use std::collections::HashMap;
use std::io;
use std::os::windows::io::OwnedHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::broker::{Broker, BufferMap, ProcEnv, ProcId, errno::*};
use crate::sys;
use crate::transport::*;
use crate::uapi::flat_binder_object;

/// Gives the uid of the guest process `pid`, or `None` to refuse it.
pub type Identity = dyn Fn(u32) -> Option<u32> + Send + Sync;

/// Largest receive buffer a process may map (libbinder maps 1 MiB minus
/// two pages).
pub const MAX_BUFFER: usize = 4 << 20;

/// The server. Dropping it does not stop it; it lives as long as sysd.
pub struct Server {
    broker: Arc<Broker>,
    name: String,
    identity: Box<Identity>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("name", &self.name).finish()
    }
}

/// The environment of one guest process.
#[derive(Debug)]
struct HostEnv {
    /// `PROCESS_DUP_HANDLE` on the guest.
    process: OwnedHandle,
    poll_event: OwnedHandle,
}

impl ProcEnv for HostEnv {
    fn import_handle(&self, from: &dyn ProcEnv, handle: u32) -> Result<u32, i32> {
        let from: &dyn Any = from;
        let from = from.downcast_ref::<HostEnv>().ok_or(EBADF)?;
        let h = windows::Win32::Foundation::HANDLE(handle as usize as *mut _);
        let v = sys::duplicate(sys::raw(&from.process), h, sys::raw(&self.process), None)
            .map_err(|_| EBADF)?;
        u32::try_from(v).map_err(|_| EBADF)
    }

    fn notify_work(&self) {
        sys::set_event(sys::raw(&self.poll_event));
    }
}

/// A lane's broker side.
struct Lane {
    quit: Arc<AtomicBool>,
    req: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

impl Lane {
    fn stop(mut self) {
        self.quit.store(true, Ordering::SeqCst);
        sys::set_event(sys::raw(&*self.req));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A mapped view, unmapped on drop.
struct View(*mut u8);
// SAFETY: plain shared memory; the address is valid in the whole process.
unsafe impl Send for View {}
// SAFETY: as above; users synchronize through the events and the broker
// lock.
unsafe impl Sync for View {}
impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: the view is no longer used once its owner drops it.
        unsafe { sys::unmap_view(self.0) };
    }
}

impl Server {
    /// Starts serving the pipe `name` (`\\.\pipe\...`). Returns once the
    /// first pipe instance exists, so clients can connect.
    pub fn start(name: &str, identity: Box<Identity>) -> io::Result<Arc<Self>> {
        let broker = Broker::new();
        broker.set_spin(crate::transport::spin_budget_when_hot());
        let s = Arc::new(Self {
            broker,
            name: name.to_string(),
            identity,
        });
        let wide = sys::wide(name);
        let first = sys::create_pipe(&wide)?;
        let s2 = s.clone();
        std::thread::Builder::new()
            .name("binder-accept".into())
            .spawn(move || s2.accept_loop(wide, first))?;
        Ok(s)
    }

    pub fn broker(&self) -> &Arc<Broker> {
        &self.broker
    }

    pub fn pipe_name(&self) -> &str {
        &self.name
    }

    fn accept_loop(self: Arc<Self>, name: Vec<u16>, mut next: OwnedHandle) {
        loop {
            if sys::connect_pipe(&next).is_err() {
                // The client went away before we saw it.
                match sys::create_pipe(&name) {
                    Ok(h) => next = h,
                    Err(_) => return,
                }
                continue;
            }
            let conn = next;
            next = match sys::create_pipe(&name) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("binder: cannot create a pipe instance: {e}");
                    return;
                }
            };
            let s = self.clone();
            let _ = std::thread::Builder::new()
                .name("binder-client".into())
                .spawn(move || s.serve(conn));
        }
    }

    /// Serves a private channel for a process sysd starts (ADR 0036: an
    /// AppContainer process gets its sysd connections as inherited
    /// handles, it does not open named pipes): `server_end` from
    /// [`sys::create_channel`], whose client end process `pid` inherits
    /// (and names in [`HANDLE_ENV`]).
    pub fn attach(self: &Arc<Self>, server_end: OwnedHandle, pid: u32, uid: u32) -> io::Result<()> {
        let s = self.clone();
        std::thread::Builder::new()
            .name("binder-client".into())
            .spawn(move || s.serve_as(server_end, pid, uid))?;
        Ok(())
    }

    fn serve(&self, pipe: OwnedHandle) {
        let Ok(pid) = sys::pipe_client_pid(&pipe) else {
            return;
        };
        let Some(uid) = (self.identity)(pid) else {
            return;
        };
        self.serve_as(pipe, pid, uid);
    }

    fn serve_as(&self, pipe: OwnedHandle, pid: u32, uid: u32) {
        let Ok(process) = sys::open_process(pid, sys::PROCESS_DUP_HANDLE) else {
            return;
        };
        let Ok(poll_event) = sys::create_event() else {
            return;
        };
        let env = Arc::new(HostEnv {
            process,
            poll_event,
        });
        let id = self.broker.add_proc(pid as i32, uid, env.clone());
        let mut c = Client {
            broker: self.broker.clone(),
            id,
            env,
            lanes: HashMap::new(),
            buffer: None,
            buffer_view: None,
        };
        let mut buf = [0u8; 64];
        while let Ok(n) = sys::read_msg(&pipe, &mut buf) {
            let reply = match Msg::from_bytes(&buf[..n]) {
                Some(m) => c.handle(m),
                None => Msg {
                    op: -EINVAL,
                    ..Msg::default()
                },
            };
            if sys::write_msg(&pipe, &reply.to_bytes()).is_err() {
                break;
            }
        }
        // The process closed the device or exited.
        self.broker.proc_exit(id);
        for (_, l) in c.lanes.drain() {
            l.stop();
        }
    }
}

struct Client {
    broker: Arc<Broker>,
    id: ProcId,
    env: Arc<HostEnv>,
    lanes: HashMap<i32, Lane>,
    buffer: Option<(OwnedHandle, usize)>,
    buffer_view: Option<View>,
}

fn err(e: i32) -> Msg {
    Msg {
        op: -e,
        ..Msg::default()
    }
}

impl Client {
    /// Duplicates `h` (of sysd) into the guest.
    fn give(&self, h: &OwnedHandle) -> Result<u64, i32> {
        sys::duplicate(
            sys::current_process(),
            sys::raw(h),
            sys::raw(&self.env.process),
            None,
        )
        .map_err(|_| EBADF)
    }

    fn handle(&mut self, m: Msg) -> Msg {
        let r = match m.op as u32 {
            op::HELLO => self.hello(m),
            op::THREAD => self.thread(m.a as i32),
            op::MMAP => self.mmap(m.a as usize),
            op::MAPPED => self.mapped(m.a, m.b as usize),
            op::MAX_THREADS => self
                .broker
                .set_max_threads(self.id, m.a as u32)
                .map(|()| Msg::default()),
            op::CONTEXT_MGR => {
                let obj = (m.c == 1).then_some(flat_binder_object {
                    hdr_type: 0,
                    flags: m.flags,
                    binder: m.a,
                    cookie: m.b,
                });
                self.broker
                    .set_context_mgr(self.id, obj)
                    .map(|()| Msg::default())
            }
            op::THREAD_EXIT => {
                let tid = m.a as i32;
                self.broker.thread_exit(self.id, tid);
                if let Some(l) = self.lanes.remove(&tid) {
                    l.stop();
                }
                Ok(Msg::default())
            }
            op::HAS_WORK => Ok(Msg {
                a: u64::from(self.broker.has_work(self.id, m.a as i32)),
                ..Msg::default()
            }),
            _ => Err(EINVAL),
        };
        r.unwrap_or_else(err)
    }

    fn hello(&self, m: Msg) -> Result<Msg, i32> {
        if m.a != VERSION {
            return Err(EINVAL);
        }
        let me = sys::current_process();
        // SYNCHRONIZE, so the guest notices when sysd is gone.
        let x = sys::duplicate(me, me, sys::raw(&self.env.process), Some(0x0010_0000))
            .map_err(|_| EBADF)?;
        let y = self.give(&self.env.poll_event)?;
        Ok(Msg {
            a: x,
            b: y,
            ..Msg::default()
        })
    }

    fn thread(&mut self, tid: i32) -> Result<Msg, i32> {
        if self.lanes.contains_key(&tid) {
            return Err(EBUSY);
        }
        let section = sys::create_section(LANE_SIZE).map_err(|_| ENOMEM)?;
        let view = View(sys::map_view(&section, LANE_SIZE).map_err(|_| ENOMEM)?);
        let req = Arc::new(sys::create_event().map_err(|_| ENOMEM)?);
        let resp = sys::create_event().map_err(|_| ENOMEM)?;
        let reply = Msg {
            a: self.give(&section)?,
            b: self.give(&req)?,
            c: self.give(&resp)?,
            ..Msg::default()
        };
        let quit = Arc::new(AtomicBool::new(false));
        let (b, id, q, rq) = (self.broker.clone(), self.id, quit.clone(), req.clone());
        let thread = std::thread::Builder::new()
            .name(format!("binder-{tid}"))
            .spawn(move || lane_loop(&b, id, tid, &view, &rq, &resp, &q))
            .map_err(|_| ENOMEM)?;
        self.lanes.insert(
            tid,
            Lane {
                quit,
                req,
                thread: Some(thread),
            },
        );
        Ok(reply)
    }

    fn mmap(&mut self, len: usize) -> Result<Msg, i32> {
        if self.buffer.is_some() {
            return Err(EBUSY);
        }
        if len == 0 || len > MAX_BUFFER {
            return Err(ENOMEM);
        }
        let section = sys::create_section(len).map_err(|_| ENOMEM)?;
        let a = self.give(&section)?;
        self.buffer = Some((section, len));
        Ok(Msg {
            a,
            ..Msg::default()
        })
    }

    fn mapped(&mut self, guest_base: u64, len: usize) -> Result<Msg, i32> {
        let Some((section, size)) = &self.buffer else {
            return Err(EINVAL);
        };
        if len != *size || self.buffer_view.is_some() {
            return Err(EINVAL);
        }
        let view = View(sys::map_view(section, len).map_err(|_| ENOMEM)?);
        self.broker.set_buffer(
            self.id,
            BufferMap {
                ptr: view.0,
                len,
                guest_base,
            },
        )?;
        self.buffer_view = Some(view);
        Ok(Msg::default())
    }
}

/// The broker thread of one guest thread.
fn lane_loop(
    b: &Broker,
    id: ProcId,
    tid: i32,
    view: &View,
    req: &OwnedHandle,
    resp: &OwnedHandle,
    quit: &AtomicBool,
) {
    let base = view.0;
    // SAFETY: the lane stays mapped and the events open while this thread
    // runs (the client joins it before dropping them).
    let (reqw, respw) = unsafe {
        (
            Wake::new(base, true, sys::raw(req)),
            Wake::new(base, false, sys::raw(resp)),
        )
    };
    let hot = Hot::default();
    // A new lane starts at 0; the guest may already have posted.
    let mut seen = 0;
    let alive = || !quit.load(Ordering::SeqCst);
    if !reqw.wait(seen, Duration::ZERO, 1000, &alive) {
        return;
    }
    loop {
        if !alive() {
            return;
        }
        seen = reqw.current();
        // SAFETY: the header is at the start of the lane; copied out once.
        let h = unsafe { base.cast::<LaneHeader>().read_volatile() };
        let wl = (h.write_len as usize).min(LANE_WRITE_MAX);
        let cap = (h.read_cap as usize).min(LANE_READ_MAX);
        // SAFETY: inside the lane. The guest may write it concurrently
        // (only to hurt itself); the broker reads each value once.
        let write = unsafe { std::slice::from_raw_parts(base.add(LANE_WRITE), wl) };
        let r = b.write_read(
            id,
            tid,
            write,
            cap,
            h.flags & LANE_FIRST_READ != 0,
            h.flags & LANE_NONBLOCK != 0,
        );
        let mut out = LaneHeader::default();
        match r {
            Ok(r) => {
                out.write_consumed = r.write_consumed as u64;
                out.read_len = r.read.len() as u64;
                // SAFETY: the read area of the lane holds `cap` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        r.read.as_ptr(),
                        base.add(LANE_READ),
                        r.read.len(),
                    )
                };
            }
            Err((e, consumed)) => {
                out.status = -e;
                out.write_consumed = consumed as u64;
            }
        }
        // SAFETY: the header at the start of the lane.
        unsafe { base.cast::<LaneHeader>().write_volatile(out) };
        hot.mark();
        if !respw.post_then_wait(&reqw, seen, spin_budget(&hot), 1000, &alive) {
            return;
        }
    }
}

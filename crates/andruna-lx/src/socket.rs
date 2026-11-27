//! Unix domain sockets (`AF_UNIX`): stream, sequenced-packet and datagram
//! sockets, `socketpair`, names in the file system and in the abstract
//! namespace, descriptor passing (`SCM_RIGHTS`) and credentials
//! (`SO_PEERCRED`, `SO_PASSCRED`).
//!
//! Sockets are lx objects inside this process: each socket has a receive
//! queue of messages (data with the descriptors sent along), and a
//! connected socket writes into its peer's queue. A stream read never
//! joins data sent with descriptors to data sent before or after it, as on
//! Linux.
//!
//! Other address families (`AF_INET`, `AF_INET6`, `AF_NETLINK`, ...) are
//! not provided by lx yet and fail with `EAFNOSUPPORT`.
//!
//! # Deviations from Linux
//!
//! - Names are known inside this process only: binding a file-system name
//!   creates the file, but a socket bound in another process cannot be
//!   reached through it (`ECONNREFUSED`), and the file reports a regular
//!   file in `stat`.
//! - A socket whose own descriptor is queued in its own receive queue is
//!   not collected while it stays there.
//! - Out-of-band data (`MSG_OOB`) is refused with `EOPNOTSUPP`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use crate::errno::{Errno, SysResult};
use crate::fd::{FDS, FileOps, OpenFile};
use crate::linux::fcntl::{AT_FDCWD, O_CLOEXEC, O_CREAT, O_EXCL, O_NONBLOCK, O_RDWR, O_WRONLY};
use crate::linux::poll::*;
use crate::linux::stat::{S_IFSOCK, StatData};
use crate::pipe::raise_sigpipe;
use crate::util::{UserBuf, lock};
use crate::waitq;

pub const AF_UNIX: i32 = 1;
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_RAW: i32 = 3;
pub const SOCK_SEQPACKET: i32 = 5;
pub const SOCK_TYPE_MASK: i32 = 0xf;
pub const SOCK_NONBLOCK: i32 = O_NONBLOCK as i32;
pub const SOCK_CLOEXEC: i32 = O_CLOEXEC as i32;

pub const MSG_OOB: i32 = 0x1;
pub const MSG_PEEK: i32 = 0x2;
pub const MSG_CTRUNC: i32 = 0x8;
pub const MSG_TRUNC: i32 = 0x20;
pub const MSG_DONTWAIT: i32 = 0x40;
pub const MSG_EOR: i32 = 0x80;
pub const MSG_WAITALL: i32 = 0x100;
pub const MSG_NOSIGNAL: i32 = 0x4000;
pub const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000;

pub const SOL_SOCKET: i32 = 1;
pub const SCM_RIGHTS: i32 = 1;
pub const SCM_CREDENTIALS: i32 = 2;

pub const SO_REUSEADDR: i32 = 2;
pub const SO_TYPE: i32 = 3;
pub const SO_ERROR: i32 = 4;
pub const SO_SNDBUF: i32 = 7;
pub const SO_RCVBUF: i32 = 8;
pub const SO_KEEPALIVE: i32 = 9;
pub const SO_LINGER: i32 = 13;
pub const SO_PASSCRED: i32 = 16;
pub const SO_PEERCRED: i32 = 17;
pub const SO_RCVLOWAT: i32 = 18;
pub const SO_SNDLOWAT: i32 = 19;
pub const SO_RCVTIMEO: i32 = 20;
pub const SO_SNDTIMEO: i32 = 21;
pub const SO_ACCEPTCONN: i32 = 30;
pub const SO_SNDBUFFORCE: i32 = 32;
pub const SO_RCVBUFFORCE: i32 = 33;
pub const SO_PROTOCOL: i32 = 38;
pub const SO_DOMAIN: i32 = 39;

pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;
pub const SHUT_RDWR: i32 = 2;

/// Linux's default socket buffer size (`net.core.wmem_default`).
const DEFAULT_BUF: usize = 212_992;
/// `sizeof(struct sockaddr_un)`.
pub const SOCKADDR_UN_LEN: usize = 110;

/// A socket name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Name {
    /// A normalized absolute Linux path.
    Path(Vec<u8>),
    /// An abstract name (without the leading NUL).
    Abstract(Vec<u8>),
}

impl Name {
    /// The `sockaddr_un` bytes of the name (`None`: unnamed, family only).
    pub fn sockaddr(name: Option<&Name>) -> Vec<u8> {
        let mut v = (AF_UNIX as u16).to_ne_bytes().to_vec();
        match name {
            None => {}
            Some(Name::Path(p)) => {
                v.extend_from_slice(p);
                v.push(0);
            }
            Some(Name::Abstract(a)) => {
                v.push(0);
                v.extend_from_slice(a);
            }
        }
        v
    }
}

/// An address passed to `bind`, `connect` or `sendto`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Addr {
    /// Only the family: autobind for `bind`.
    Unnamed,
    Name(Name),
}

/// Parses a `sockaddr_un` of `addr.len()` bytes (the caller's
/// `addrlen`).
pub fn parse_addr(addr: &[u8]) -> SysResult<Addr> {
    if addr.len() < 2 || addr.len() > SOCKADDR_UN_LEN {
        return Err(Errno::EINVAL);
    }
    let family = i32::from(u16::from_ne_bytes([addr[0], addr[1]]));
    if family != AF_UNIX {
        return Err(Errno::EINVAL);
    }
    let path = &addr[2..];
    if path.is_empty() {
        return Ok(Addr::Unnamed);
    }
    if path[0] == 0 {
        return Ok(Addr::Name(Name::Abstract(path[1..].to_vec())));
    }
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let p = crate::fs::path::normalize(&crate::fs::getcwd(), &path[..end])?;
    Ok(Addr::Name(Name::Path(p.to_bytes())))
}

/// One message in a receive queue.
#[derive(Debug, Default)]
struct Msg {
    data: Vec<u8>,
    /// Bytes of `data` already read (stream sockets).
    read: usize,
    fds: Vec<Arc<OpenFile>>,
    /// The sender's name (datagrams).
    from: Option<Name>,
}

#[derive(Debug)]
struct Queue {
    msgs: VecDeque<Msg>,
    bytes: usize,
    cap: usize,
    /// No more data will arrive: the writer closed or shut down.
    eof: bool,
    /// The reader is gone: writes fail with `EPIPE` (or `ECONNREFUSED`
    /// for datagrams).
    closed: bool,
}

type QueueRef = Arc<Mutex<Queue>>;

fn new_queue() -> QueueRef {
    Arc::new(Mutex::new(Queue {
        msgs: VecDeque::new(),
        bytes: 0,
        cap: DEFAULT_BUF,
        eof: false,
        closed: false,
    }))
}

#[derive(Debug)]
struct Peer {
    rx: QueueRef,
    name: Option<Name>,
    /// A host receiver instead of a socket ([`register_sink`]).
    sink: Option<Sink>,
}

/// A datagram receiver the host process provides at a file-system name,
/// for services that run inside the host (the log writer while there is
/// no log service). It runs on the sending thread.
pub type Sink = fn(&[u8]);

static SINKS: Mutex<Vec<(Name, Sink)>> = Mutex::new(Vec::new());

/// Makes datagrams sent to the absolute Linux path `path` go to `sink`.
pub fn register_sink(path: &str, sink: Sink) -> SysResult<()> {
    let p = crate::fs::path::normalize(b"/", path.as_bytes())?;
    let name = Name::Path(p.to_bytes());
    let mut g = lock(&SINKS);
    g.retain(|(n, _)| *n != name);
    g.push((name, sink));
    Ok(())
}

fn sink_at(n: &Name) -> Option<Sink> {
    lock(&SINKS).iter().find(|(m, _)| m == n).map(|(_, s)| *s)
}

#[derive(Debug, Default)]
struct Opts {
    passcred: bool,
    sndbuf: i32,
    rcvbuf: i32,
    reuseaddr: bool,
    keepalive: bool,
    linger: [i32; 2],
    rcvtimeo: Option<i128>,
    sndtimeo: Option<i128>,
}

#[derive(Debug)]
struct State {
    name: Option<Name>,
    peer: Option<Peer>,
    /// Pending connections of a listening socket.
    backlog: Option<(VecDeque<Arc<Inner>>, usize)>,
    shut_rd: bool,
    shut_wr: bool,
}

#[derive(Debug)]
struct Inner {
    ty: i32,
    ino: u64,
    rx: QueueRef,
    state: Mutex<State>,
    opts: Mutex<Opts>,
}

impl Inner {
    fn new(ty: i32) -> Arc<Self> {
        Arc::new(Self {
            ty,
            ino: crate::pipe::next_ino(),
            rx: new_queue(),
            state: Mutex::new(State {
                name: None,
                peer: None,
                backlog: None,
                shut_rd: false,
                shut_wr: false,
            }),
            opts: Mutex::new(Opts {
                sndbuf: DEFAULT_BUF as i32,
                rcvbuf: DEFAULT_BUF as i32,
                ..Opts::default()
            }),
        })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let st = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        {
            let mut q = lock(&self.rx);
            q.closed = true;
            q.msgs.clear();
            q.bytes = 0;
        }
        if self.ty != SOCK_DGRAM
            && let Some(p) = &st.peer
        {
            lock(&p.rx).eof = true;
        }
        if let Some(n) = st.name.take() {
            let mut r = lock(&NAMES);
            if let Some(m) = r.as_mut()
                && m.get(&n).is_some_and(|w| w.strong_count() == 0)
            {
                m.remove(&n);
            }
        }
        waitq::notify();
    }
}

/// Bound names of this process.
static NAMES: Mutex<Option<HashMap<Name, Weak<Inner>>>> = Mutex::new(None);

fn lookup(n: &Name) -> Option<Arc<Inner>> {
    lock(&NAMES).as_ref()?.get(n)?.upgrade()
}

/// A socket descriptor's object.
#[derive(Debug)]
pub struct Socket {
    inner: Arc<Inner>,
}

/// The ancillary data of `sendmsg`.
#[derive(Debug, Default)]
pub struct SendAnc {
    pub fds: Vec<Arc<OpenFile>>,
}

/// The result of a receive.
#[derive(Debug, Default)]
pub struct Received {
    pub data: Vec<u8>,
    /// The value `recvmsg` returns (the message length with `MSG_TRUNC`).
    pub len: usize,
    pub fds: Vec<Arc<OpenFile>>,
    /// `SCM_CREDENTIALS` to report (pid, uid, gid).
    pub cred: Option<(i32, u32, u32)>,
    pub from: Option<Name>,
    pub flags: i32,
}

fn nonblock(file: &OpenFile, flags: i32) -> bool {
    file.status_flags() & O_NONBLOCK != 0 || flags & MSG_DONTWAIT != 0
}

/// The process's own credentials, which every peer has.
pub fn own_cred() -> (i32, u32, u32) {
    (
        crate::process::getpid(),
        crate::process::uid(),
        crate::process::gid(),
    )
}

impl Socket {
    pub fn ty(&self) -> i32 {
        self.inner.ty
    }

    /// `bind`.
    pub fn bind(&self, addr: Addr) -> SysResult<()> {
        let mut st = lock(&self.inner.state);
        if st.name.is_some() {
            return Err(Errno::EINVAL);
        }
        let mut names = lock(&NAMES);
        let m = names.get_or_insert_with(HashMap::new);
        m.retain(|_, w| w.strong_count() > 0);
        let name = match addr {
            Addr::Unnamed => {
                // Autobind: five hex digits in the abstract namespace.
                let mut n = (crate::process::getpid() as u32).wrapping_mul(2_654_435_761);
                loop {
                    n = n.wrapping_add(self.inner.ino as u32) & 0xfffff;
                    let c = Name::Abstract(format!("{n:05x}").into_bytes());
                    if !m.contains_key(&c) {
                        break c;
                    }
                    n = n.wrapping_add(1);
                }
            }
            Addr::Name(n) => {
                if m.contains_key(&n) {
                    return Err(Errno::EADDRINUSE);
                }
                if let Name::Path(p) = &n {
                    let flags = O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC;
                    let fd = crate::fs::openat(AT_FDCWD, p, flags, 0o777).map_err(|e| {
                        if e == Errno::EEXIST {
                            Errno::EADDRINUSE
                        } else {
                            e
                        }
                    })?;
                    let _ = crate::fs::close(fd);
                    let _ = crate::fs::modes::mark_socket(p);
                }
                n
            }
        };
        m.insert(name.clone(), Arc::downgrade(&self.inner));
        st.name = Some(name);
        Ok(())
    }

    /// `listen`.
    pub fn listen(&self, backlog: i32) -> SysResult<()> {
        if self.inner.ty == SOCK_DGRAM {
            return Err(Errno::EOPNOTSUPP);
        }
        let mut st = lock(&self.inner.state);
        if st.name.is_none() || st.peer.is_some() {
            return Err(Errno::EINVAL);
        }
        let max = backlog.clamp(1, 4096) as usize;
        match &mut st.backlog {
            Some((_, m)) => *m = max,
            None => st.backlog = Some((VecDeque::new(), max)),
        }
        drop(st);
        waitq::notify();
        Ok(())
    }

    /// `connect`.
    pub fn connect(&self, file: &OpenFile, addr: Addr) -> SysResult<()> {
        let Addr::Name(name) = addr else {
            return Err(Errno::EINVAL);
        };
        let target = lookup(&name);
        if self.inner.ty == SOCK_DGRAM {
            if target.is_none()
                && let Some(sink) = sink_at(&name)
            {
                lock(&self.inner.state).peer = Some(Peer {
                    rx: new_queue(),
                    name: Some(name),
                    sink: Some(sink),
                });
                return Ok(());
            }
            let t = target.ok_or_else(|| missing(&name))?;
            if t.ty != SOCK_DGRAM {
                return Err(Errno::EPROTOTYPE);
            }
            let mut st = lock(&self.inner.state);
            st.peer = Some(Peer {
                rx: t.rx.clone(),
                name: Some(name),
                sink: None,
            });
            return Ok(());
        }
        {
            let st = lock(&self.inner.state);
            if st.peer.is_some() {
                return Err(Errno::EISCONN);
            }
            if st.backlog.is_some() {
                return Err(Errno::EINVAL);
            }
        }
        let listener = target.ok_or_else(|| missing(&name))?;
        if listener.ty != self.inner.ty {
            return Err(Errno::EPROTOTYPE);
        }
        let server = Inner::new(self.inner.ty);
        let timeout = lock(&self.inner.opts).sndtimeo;
        waitq::wait_until(waitq::deadline_in(timeout), || {
            let mut ls = lock(&listener.state);
            let Some((q, max)) = ls.backlog.as_mut() else {
                return Err(Errno::ECONNREFUSED);
            };
            if q.len() < *max {
                q.push_back(server.clone());
                return Ok(Some(()));
            }
            if file.status_flags() & O_NONBLOCK != 0 {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })
        .map_err(|e| {
            if e == Errno::ETIMEDOUT {
                Errno::EAGAIN
            } else {
                e
            }
        })?;
        let my_name = lock(&self.inner.state).name.clone();
        {
            let mut ss = lock(&server.state);
            ss.name = Some(name.clone());
            ss.peer = Some(Peer {
                rx: self.inner.rx.clone(),
                name: my_name,
                sink: None,
            });
        }
        lock(&self.inner.state).peer = Some(Peer {
            rx: server.rx.clone(),
            name: Some(name),
            sink: None,
        });
        waitq::notify();
        Ok(())
    }

    /// `accept4`: the new connection and its peer's name.
    pub fn accept(&self, file: &OpenFile) -> SysResult<(Socket, Option<Name>)> {
        if self.inner.ty == SOCK_DGRAM {
            return Err(Errno::EOPNOTSUPP);
        }
        let timeout = lock(&self.inner.opts).rcvtimeo;
        let s = waitq::wait_until(waitq::deadline_in(timeout), || {
            let mut st = lock(&self.inner.state);
            let Some((q, _)) = st.backlog.as_mut() else {
                return Err(Errno::EINVAL);
            };
            if let Some(s) = q.pop_front() {
                return Ok(Some(s));
            }
            if file.status_flags() & O_NONBLOCK != 0 {
                return Err(Errno::EAGAIN);
            }
            Ok(None)
        })
        .map_err(|e| {
            if e == Errno::ETIMEDOUT {
                Errno::EAGAIN
            } else {
                e
            }
        })?;
        let peer_name = lock(&s.state).peer.as_ref().and_then(|p| p.name.clone());
        Ok((Socket { inner: s }, peer_name))
    }

    pub fn name(&self) -> Option<Name> {
        lock(&self.inner.state).name.clone()
    }

    /// `getpeername`.
    pub fn peer_name(&self) -> SysResult<Option<Name>> {
        let st = lock(&self.inner.state);
        let p = st.peer.as_ref().ok_or(Errno::ENOTCONN)?;
        if self.inner.ty != SOCK_DGRAM && lock(&p.rx).closed {
            return Err(Errno::ENOTCONN);
        }
        Ok(p.name.clone())
    }

    /// `sendmsg`: returns the bytes sent.
    pub fn send(
        &self,
        file: &OpenFile,
        data: &[u8],
        anc: SendAnc,
        to: Option<Addr>,
        flags: i32,
    ) -> SysResult<usize> {
        let r = self.send_inner(file, data, anc, to, flags);
        if r == Err(Errno::EPIPE) && flags & MSG_NOSIGNAL == 0 {
            raise_sigpipe();
        }
        r
    }

    fn send_inner(
        &self,
        file: &OpenFile,
        data: &[u8],
        anc: SendAnc,
        to: Option<Addr>,
        flags: i32,
    ) -> SysResult<usize> {
        if flags & MSG_OOB != 0 {
            return Err(Errno::EOPNOTSUPP);
        }
        let ty = self.inner.ty;
        let (rx, from) = {
            let st = lock(&self.inner.state);
            if st.shut_wr {
                return Err(Errno::EPIPE);
            }
            match (ty, to) {
                (SOCK_DGRAM, Some(Addr::Name(n))) => {
                    if lookup(&n).is_none()
                        && let Some(sink) = sink_at(&n)
                    {
                        sink(data);
                        return Ok(data.len());
                    }
                    let t = lookup(&n).ok_or_else(|| missing(&n))?;
                    if t.ty != SOCK_DGRAM {
                        return Err(Errno::EPROTOTYPE);
                    }
                    (t.rx.clone(), st.name.clone())
                }
                (SOCK_DGRAM, Some(Addr::Unnamed)) => return Err(Errno::EINVAL),
                (_, Some(_)) if st.peer.is_some() => return Err(Errno::EISCONN),
                _ => match &st.peer {
                    Some(Peer {
                        sink: Some(sink), ..
                    }) => {
                        sink(data);
                        return Ok(data.len());
                    }
                    Some(p) => (p.rx.clone(), st.name.clone()),
                    None if ty == SOCK_DGRAM => return Err(Errno::ENOTCONN),
                    None => return Err(Errno::ENOTCONN),
                },
            }
        };
        let cap = lock(&rx).cap;
        if ty != SOCK_STREAM && data.len() > cap {
            return Err(Errno::EMSGSIZE);
        }
        let timeout = lock(&self.inner.opts).sndtimeo;
        let deadline = waitq::deadline_in(timeout);
        let mut fds = Some(anc.fds);
        let mut done = 0usize;
        loop {
            let r = waitq::wait_until(deadline, || {
                let mut q = lock(&rx);
                if q.closed {
                    return Err(if ty == SOCK_DGRAM {
                        Errno::ECONNREFUSED
                    } else {
                        Errno::EPIPE
                    });
                }
                let free = q.cap.saturating_sub(q.bytes);
                let left = data.len() - done;
                let fits = if ty == SOCK_STREAM {
                    free > 0 || left == 0
                } else {
                    free >= left || q.msgs.is_empty()
                };
                if fits {
                    let n = if ty == SOCK_STREAM {
                        left.min(free)
                    } else {
                        left
                    };
                    q.msgs.push_back(Msg {
                        data: data[done..done + n].to_vec(),
                        read: 0,
                        fds: fds.take().unwrap_or_default(),
                        from: from.clone(),
                    });
                    q.bytes += n;
                    return Ok(Some(n));
                }
                if nonblock(file, flags) {
                    return Err(Errno::EAGAIN);
                }
                Ok(None)
            });
            match r {
                Ok(n) => {
                    done += n;
                    waitq::notify();
                    if done == data.len() {
                        return Ok(done);
                    }
                }
                Err(Errno::ETIMEDOUT) if done == 0 => return Err(Errno::EAGAIN),
                Err(e) if done > 0 && e != Errno::EPIPE => return Ok(done),
                Err(e) if done > 0 && e == Errno::EPIPE => return Ok(done),
                Err(e) => return Err(e),
            }
        }
    }

    /// `recvmsg` of up to `max` bytes.
    pub fn recv(&self, file: &OpenFile, max: usize, flags: i32) -> SysResult<Received> {
        if flags & MSG_OOB != 0 {
            return Err(Errno::EOPNOTSUPP);
        }
        let ty = self.inner.ty;
        {
            let st = lock(&self.inner.state);
            if st.backlog.is_some() {
                return Err(Errno::EINVAL);
            }
            if ty != SOCK_DGRAM && st.peer.is_none() && lock(&self.inner.rx).msgs.is_empty() {
                return Err(Errno::ENOTCONN);
            }
        }
        let peek = flags & MSG_PEEK != 0;
        let timeout = lock(&self.inner.opts).rcvtimeo;
        let passcred = lock(&self.inner.opts).passcred;
        let mut out = Received::default();
        let r = waitq::wait_until(waitq::deadline_in(timeout), || {
            let shut_rd = lock(&self.inner.state).shut_rd;
            let mut q = lock(&self.inner.rx);
            if q.msgs.is_empty() {
                if q.eof
                    || shut_rd
                    || (ty == SOCK_STREAM && flags & MSG_WAITALL != 0 && !out.data.is_empty())
                {
                    return Ok(Some(()));
                }
                if !out.data.is_empty() {
                    return Ok(Some(()));
                }
                if nonblock(file, flags) {
                    return Err(Errno::EAGAIN);
                }
                return Ok(None);
            }
            if ty == SOCK_STREAM {
                let mut i = 0;
                while i < q.msgs.len() && out.data.len() < max {
                    let m = &mut q.msgs[i];
                    // Descriptors belong to the first byte of their message;
                    // a read ends after the message that carried them.
                    let start = m.read;
                    let n = (m.data.len() - start).min(max - out.data.len());
                    out.data.extend_from_slice(&m.data[start..start + n]);
                    let had_fds = !m.fds.is_empty();
                    if had_fds {
                        out.fds = if peek {
                            m.fds.clone()
                        } else {
                            core::mem::take(&mut m.fds)
                        };
                    }
                    if peek {
                        i += 1;
                    } else {
                        m.read += n;
                        let done = m.read == m.data.len();
                        q.bytes -= n;
                        if done {
                            q.msgs.remove(i);
                        }
                    }
                    if had_fds {
                        break;
                    }
                }
                out.len = out.data.len();
                let more = flags & MSG_WAITALL != 0 && !peek && out.data.len() < max;
                if more {
                    return Ok(None);
                }
                Ok(Some(()))
            } else {
                let m = if peek {
                    let m = &q.msgs[0];
                    Msg {
                        data: m.data.clone(),
                        read: 0,
                        fds: m.fds.clone(),
                        from: m.from.clone(),
                    }
                } else {
                    let m = q.msgs.pop_front().unwrap_or_default();
                    q.bytes -= m.data.len();
                    m
                };
                out.len = m.data.len();
                if m.data.len() > max {
                    out.flags |= MSG_TRUNC;
                    if flags & MSG_TRUNC == 0 {
                        out.len = max;
                    }
                }
                out.data = m.data[..m.data.len().min(max)].to_vec();
                out.fds = m.fds;
                out.from = m.from;
                Ok(Some(()))
            }
        });
        match r {
            Ok(()) => {}
            Err(Errno::ETIMEDOUT) if out.data.is_empty() => return Err(Errno::EAGAIN),
            Err(e) if out.data.is_empty() => return Err(e),
            Err(_) => {}
        }
        if !peek {
            waitq::notify();
        }
        if passcred {
            out.cred = Some(own_cred());
        }
        Ok(out)
    }

    /// `shutdown`.
    pub fn shutdown(&self, how: i32) -> SysResult<()> {
        if !(SHUT_RD..=SHUT_RDWR).contains(&how) {
            return Err(Errno::EINVAL);
        }
        let mut st = lock(&self.inner.state);
        if st.peer.is_none() && self.inner.ty != SOCK_DGRAM {
            return Err(Errno::ENOTCONN);
        }
        if how != SHUT_WR {
            st.shut_rd = true;
        }
        if how != SHUT_RD {
            st.shut_wr = true;
            if self.inner.ty != SOCK_DGRAM
                && let Some(p) = &st.peer
            {
                lock(&p.rx).eof = true;
            }
        }
        drop(st);
        waitq::notify();
        Ok(())
    }

    /// `getsockopt` at `SOL_SOCKET`: the option's bytes.
    pub fn getsockopt(&self, level: i32, opt: i32) -> SysResult<Vec<u8>> {
        if level != SOL_SOCKET {
            return Err(Errno::EOPNOTSUPP);
        }
        let int = |v: i32| Ok(v.to_ne_bytes().to_vec());
        let o = lock(&self.inner.opts);
        let tv = |t: Option<i128>| {
            let us = t.unwrap_or(0) / 1000;
            let mut v = ((us / 1_000_000) as i64).to_ne_bytes().to_vec();
            v.extend_from_slice(&((us % 1_000_000) as i64).to_ne_bytes());
            Ok(v)
        };
        match opt {
            SO_TYPE => int(self.inner.ty),
            SO_DOMAIN => int(AF_UNIX),
            SO_PROTOCOL => int(0),
            SO_ERROR => int(0),
            SO_ACCEPTCONN => int(i32::from(lock(&self.inner.state).backlog.is_some())),
            SO_SNDBUF => int(o.sndbuf),
            SO_RCVBUF => int(o.rcvbuf),
            SO_PASSCRED => int(i32::from(o.passcred)),
            SO_REUSEADDR => int(i32::from(o.reuseaddr)),
            SO_KEEPALIVE => int(i32::from(o.keepalive)),
            SO_RCVLOWAT | SO_SNDLOWAT => int(1),
            SO_LINGER => {
                let mut v = o.linger[0].to_ne_bytes().to_vec();
                v.extend_from_slice(&o.linger[1].to_ne_bytes());
                Ok(v)
            }
            SO_RCVTIMEO => tv(o.rcvtimeo),
            SO_SNDTIMEO => tv(o.sndtimeo),
            SO_PEERCRED => {
                let connected = lock(&self.inner.state).peer.is_some();
                let (pid, uid, gid) = if connected {
                    own_cred()
                } else {
                    (0, u32::MAX, u32::MAX)
                };
                let mut v = pid.to_ne_bytes().to_vec();
                v.extend_from_slice(&uid.to_ne_bytes());
                v.extend_from_slice(&gid.to_ne_bytes());
                Ok(v)
            }
            _ => Err(Errno::ENOPROTOOPT),
        }
    }

    /// `setsockopt` at `SOL_SOCKET`.
    pub fn setsockopt(&self, level: i32, opt: i32, val: &[u8]) -> SysResult<()> {
        if level != SOL_SOCKET {
            return Err(Errno::EOPNOTSUPP);
        }
        let int = || -> SysResult<i32> {
            let b: [u8; 4] = val
                .get(..4)
                .ok_or(Errno::EINVAL)?
                .try_into()
                .map_err(|_| Errno::EINVAL)?;
            Ok(i32::from_ne_bytes(b))
        };
        let tv = || -> SysResult<Option<i128>> {
            let b = val.get(..16).ok_or(Errno::EINVAL)?;
            let s = i64::from_ne_bytes(b[..8].try_into().map_err(|_| Errno::EINVAL)?);
            let us = i64::from_ne_bytes(b[8..].try_into().map_err(|_| Errno::EINVAL)?);
            if !(0..1_000_000).contains(&us) {
                // Linux reports EDOM for too many microseconds.
                return Err(Errno(crate::linux::errno::EDOM));
            }
            let ns = (i128::from(s) * 1_000_000 + i128::from(us)) * 1000;
            Ok(if ns <= 0 { None } else { Some(ns) })
        };
        let mut o = lock(&self.inner.opts);
        match opt {
            SO_SNDBUF | SO_SNDBUFFORCE => {
                // Linux doubles the value for its own bookkeeping.
                o.sndbuf = int()?.clamp(2304, i32::MAX / 2) * 2;
            }
            SO_RCVBUF | SO_RCVBUFFORCE => {
                o.rcvbuf = int()?.clamp(2304, i32::MAX / 2) * 2;
            }
            SO_PASSCRED => o.passcred = int()? != 0,
            SO_REUSEADDR => o.reuseaddr = int()? != 0,
            SO_KEEPALIVE => o.keepalive = int()? != 0,
            SO_LINGER => {
                let a = int()?;
                let b = val.get(4..8).ok_or(Errno::EINVAL)?;
                o.linger = [
                    a,
                    i32::from_ne_bytes(b.try_into().map_err(|_| Errno::EINVAL)?),
                ];
            }
            SO_RCVTIMEO => o.rcvtimeo = tv()?,
            SO_SNDTIMEO => o.sndtimeo = tv()?,
            SO_RCVLOWAT => {
                int()?;
            }
            _ => return Err(Errno::ENOPROTOOPT),
        }
        Ok(())
    }
}

/// The error for connecting to a name nobody listens on.
fn missing(n: &Name) -> Errno {
    match n {
        Name::Path(p) => match crate::fs::fstatat(AT_FDCWD, p, 0) {
            Ok(_) => Errno::ECONNREFUSED,
            Err(e) => e,
        },
        Name::Abstract(_) => Errno::ECONNREFUSED,
    }
}

impl FileOps for Socket {
    fn read(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let r = self.recv(file, buf.len(), 0)?;
        crate::pipe::copy_to_user(buf, 0, &r.data)?;
        Ok(r.data.len())
    }

    fn write(&self, file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let data = crate::pipe::copy_from_user(buf, 0, buf.len())?;
        self.send(file, &data, SendAnc::default(), None, 0)
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            // sockfs.
            dev: 0x8,
            ino: self.inner.ino,
            mode: S_IFSOCK | 0o777,
            nlink: 1,
            uid: crate::process::uid(),
            gid: crate::process::gid(),
            blksize: 4096,
            ..StatData::default()
        })
    }

    fn path(&self) -> Option<Vec<u8>> {
        Some(format!("socket:[{}]", self.inner.ino).into_bytes())
    }

    fn poll(&self) -> u32 {
        let st = lock(&self.inner.state);
        if let Some((q, _)) = &st.backlog {
            return if q.is_empty() { 0 } else { POLLIN | POLLRDNORM };
        }
        let mut ev = 0;
        {
            let q = lock(&self.inner.rx);
            if !q.msgs.is_empty() {
                ev |= POLLIN | POLLRDNORM;
            }
            if q.eof || st.shut_rd {
                ev |= POLLIN | POLLRDNORM | POLLRDHUP;
            }
        }
        match &st.peer {
            Some(p) => {
                let q = lock(&p.rx);
                if q.closed {
                    if self.inner.ty != SOCK_DGRAM {
                        ev |= POLLHUP;
                    }
                    ev |= POLLOUT | POLLWRNORM;
                } else if q.bytes < q.cap && !st.shut_wr {
                    ev |= POLLOUT | POLLWRNORM;
                }
            }
            None if self.inner.ty == SOCK_DGRAM => ev |= POLLOUT | POLLWRNORM,
            None => ev |= POLLOUT | POLLWRNORM | POLLHUP,
        }
        if st.shut_rd && st.shut_wr {
            ev |= POLLHUP;
        }
        ev
    }
}

fn check_type(ty: i32) -> SysResult<i32> {
    match ty & SOCK_TYPE_MASK {
        SOCK_STREAM | SOCK_SEQPACKET | SOCK_DGRAM => Ok(ty & SOCK_TYPE_MASK),
        // AF_UNIX treats raw sockets as datagram sockets.
        SOCK_RAW => Ok(SOCK_DGRAM),
        _ => Err(Errno::ESOCKTNOSUPPORT),
    }
}

fn install(s: Socket, ty: i32) -> SysResult<i32> {
    let f = Arc::new(OpenFile::new(
        Box::new(s),
        O_RDWR | (ty as u32 & O_NONBLOCK),
    ));
    FDS.insert(f, ty & SOCK_CLOEXEC != 0, 0)
}

fn check_args(domain: i32, ty: i32, protocol: i32) -> SysResult<i32> {
    if ty & !(SOCK_TYPE_MASK | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(Errno::EINVAL);
    }
    if domain != AF_UNIX {
        return Err(Errno::EAFNOSUPPORT);
    }
    let base = check_type(ty)?;
    if protocol != 0 && protocol != AF_UNIX {
        return Err(Errno::EPROTONOSUPPORT);
    }
    Ok(base)
}

/// `socket`.
pub fn socket(domain: i32, ty: i32, protocol: i32) -> SysResult<i32> {
    let base = check_args(domain, ty, protocol)?;
    install(
        Socket {
            inner: Inner::new(base),
        },
        ty,
    )
}

/// `socketpair`.
pub fn socketpair(domain: i32, ty: i32, protocol: i32) -> SysResult<[i32; 2]> {
    let base = check_args(domain, ty, protocol)?;
    let (a, b) = (Inner::new(base), Inner::new(base));
    lock(&a.state).peer = Some(Peer {
        rx: b.rx.clone(),
        name: None,
        sink: None,
    });
    lock(&b.state).peer = Some(Peer {
        rx: a.rx.clone(),
        name: None,
        sink: None,
    });
    let fa = install(Socket { inner: a }, ty)?;
    match install(Socket { inner: b }, ty) {
        Ok(fb) => Ok([fa, fb]),
        Err(e) => {
            let _ = FDS.close(fa);
            Err(e)
        }
    }
}

/// The socket behind `fd`, with its description.
pub fn socket_of(fd: i32) -> SysResult<Arc<OpenFile>> {
    let f = FDS.get(fd)?;
    if f.downcast::<Socket>().is_none() {
        return Err(Errno::ENOTSOCK);
    }
    Ok(f)
}

/// `accept4`: the new descriptor and the peer's name.
pub fn accept4(fd: i32, flags: i32) -> SysResult<(i32, Option<Name>)> {
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(Errno::EINVAL);
    }
    let f = socket_of(fd)?;
    let s = f.downcast::<Socket>().ok_or(Errno::ENOTSOCK)?;
    let ty = s.ty();
    let (c, name) = s.accept(&f)?;
    Ok((install(c, ty | flags)?, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sock(fd: i32) -> Arc<OpenFile> {
        socket_of(fd).unwrap()
    }

    fn send(fd: i32, data: &[u8], fds: &[i32]) -> SysResult<usize> {
        let f = sock(fd);
        let anc = SendAnc {
            fds: fds.iter().map(|&d| FDS.get(d).unwrap()).collect(),
        };
        f.downcast::<Socket>().unwrap().send(&f, data, anc, None, 0)
    }

    fn recv(fd: i32, max: usize, flags: i32) -> SysResult<Received> {
        let f = sock(fd);
        f.downcast::<Socket>().unwrap().recv(&f, max, flags)
    }

    #[test]
    fn stream_pair_keeps_descriptor_boundaries() {
        let [a, b] = socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0).unwrap();
        let [p, q] = crate::pipe::pipe2(0).unwrap();
        assert_eq!(send(a, b"ab", &[]), Ok(2));
        assert_eq!(send(a, b"cd", &[p]), Ok(2));
        assert_eq!(send(a, b"ef", &[q]), Ok(2));
        // A read runs into a message with descriptors and ends after it.
        let r = recv(b, 3, MSG_PEEK).unwrap();
        assert_eq!((r.data.as_slice(), r.fds.len()), (b"abc".as_slice(), 1));
        let r = recv(b, 16, 0).unwrap();
        assert_eq!((r.data.as_slice(), r.fds.len()), (b"abcd".as_slice(), 1));
        assert!(Arc::ptr_eq(&r.fds[0], &FDS.get(p).unwrap()));
        // The descriptors stay with the first byte.
        let r = recv(b, 1, 0).unwrap();
        assert_eq!((r.data.as_slice(), r.fds.len()), (b"e".as_slice(), 1));
        let r = recv(b, 16, 0).unwrap();
        assert_eq!((r.data.as_slice(), r.fds.len()), (b"f".as_slice(), 0));
        assert_eq!(
            recv(b, 16, MSG_DONTWAIT).map(|r| r.data),
            Err(Errno::EAGAIN)
        );
        crate::fs::close(a).unwrap();
        assert_eq!(recv(b, 16, 0).map(|r| r.data), Ok(Vec::new()));
        assert_ne!(FDS.get(b).unwrap().ops().poll() & POLLHUP, 0);
        assert_eq!(
            sock(b).downcast::<Socket>().unwrap().send(
                &sock(b),
                b"x",
                SendAnc::default(),
                None,
                MSG_NOSIGNAL
            ),
            Err(Errno::EPIPE)
        );
        for fd in [b, p, q] {
            crate::fs::close(fd).unwrap();
        }
    }

    #[test]
    fn seqpacket_truncates_and_dgram_names() {
        let [a, b] = socketpair(AF_UNIX, SOCK_SEQPACKET, 0).unwrap();
        send(a, b"hello", &[]).unwrap();
        let r = recv(b, 2, 0).unwrap();
        assert_eq!(
            (r.data.as_slice(), r.len, r.flags),
            (b"he".as_slice(), 2, MSG_TRUNC)
        );
        send(a, b"hello", &[]).unwrap();
        let r = recv(b, 2, MSG_TRUNC).unwrap();
        assert_eq!(r.len, 5);
        crate::fs::close(a).unwrap();
        crate::fs::close(b).unwrap();

        let name = Name::Abstract(b"lx-test-dgram".to_vec());
        let s = socket(AF_UNIX, SOCK_DGRAM, 0).unwrap();
        let c = socket(AF_UNIX, SOCK_DGRAM, 0).unwrap();
        sock(s)
            .downcast::<Socket>()
            .unwrap()
            .bind(Addr::Name(name.clone()))
            .unwrap();
        let cs = sock(c);
        let cs = cs.downcast::<Socket>().unwrap();
        assert_eq!(cs.bind(Addr::Name(name.clone())), Err(Errno::EADDRINUSE));
        cs.bind(Addr::Unnamed).unwrap();
        let f = sock(c);
        cs.send(
            &f,
            b"ping",
            SendAnc::default(),
            Some(Addr::Name(name.clone())),
            0,
        )
        .unwrap();
        let r = recv(s, 16, 0).unwrap();
        assert_eq!(r.data, b"ping");
        assert_eq!(r.from, cs.name());
        crate::fs::close(s).unwrap();
        assert_eq!(
            cs.send(&f, b"x", SendAnc::default(), Some(Addr::Name(name)), 0),
            Err(Errno::ECONNREFUSED)
        );
        crate::fs::close(c).unwrap();
        assert_eq!(socket(AF_INET, SOCK_STREAM, 0), Err(Errno::EAFNOSUPPORT));
        assert_eq!(socket(AF_UNIX, 9, 0), Err(Errno::ESOCKTNOSUPPORT));
    }

    #[test]
    fn listen_connect_accept() {
        let name = Name::Abstract(b"lx-test-stream".to_vec());
        let l = socket(AF_UNIX, SOCK_STREAM, 0).unwrap();
        let ls = sock(l);
        let lsock = ls.downcast::<Socket>().unwrap();
        assert_eq!(lsock.listen(1), Err(Errno::EINVAL));
        lsock.bind(Addr::Name(name.clone())).unwrap();
        lsock.listen(4).unwrap();
        let c = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0).unwrap();
        let cf = sock(c);
        let cs = cf.downcast::<Socket>().unwrap();
        assert_eq!(FDS.get(l).unwrap().ops().poll(), 0);
        cs.connect(&cf, Addr::Name(name.clone())).unwrap();
        assert_eq!(
            cs.connect(&cf, Addr::Name(name.clone())),
            Err(Errno::EISCONN)
        );
        assert_eq!(FDS.get(l).unwrap().ops().poll(), POLLIN | POLLRDNORM);
        let (s, peer) = accept4(l, SOCK_CLOEXEC).unwrap();
        assert_eq!(peer, None);
        assert_eq!(cs.peer_name(), Ok(Some(name.clone())));
        send(c, b"hi", &[]).unwrap();
        assert_eq!(recv(s, 8, 0).unwrap().data, b"hi");
        let cred = sock(s)
            .downcast::<Socket>()
            .unwrap()
            .getsockopt(SOL_SOCKET, SO_PEERCRED)
            .unwrap();
        assert_eq!(&cred[..4], &crate::process::getpid().to_ne_bytes());
        sock(s)
            .downcast::<Socket>()
            .unwrap()
            .shutdown(SHUT_WR)
            .unwrap();
        assert_eq!(recv(c, 8, 0).unwrap().data, Vec::<u8>::new());
        drop(cf);
        drop(ls);
        for fd in [l, c, s] {
            crate::fs::close(fd).unwrap();
        }
        let c = socket(AF_UNIX, SOCK_STREAM, 0).unwrap();
        let cf = sock(c);
        assert_eq!(
            cf.downcast::<Socket>()
                .unwrap()
                .connect(&cf, Addr::Name(name)),
            Err(Errno::ECONNREFUSED)
        );
        crate::fs::close(c).unwrap();
    }
}

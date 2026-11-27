//! The binder broker: the logic of the kernel binder driver, run in sysd
//! for every guest process (ADR 0025).
//!
//! The broker is independent of the transport. A guest thread's
//! `BINDER_WRITE_READ` reaches [`Broker::write_read`] on a sysd thread that
//! acts as the guest thread's "kernel half" and blocks there exactly where
//! the kernel would block the thread. All state is under one lock; each
//! guest thread has its own condition variable.
//!
//! # Write stream
//!
//! The write stream is the guest's `BC_*` stream, *packed* by the guest
//! side ([`crate::pack`]): every `BC_TRANSACTION`, `BC_REPLY` and their
//! `_SG` forms is followed by its data (padded to 8 bytes) and its offsets
//! array (padded to 8 bytes) inline, because the broker cannot read guest
//! memory. File descriptor objects carry the sender's Windows handle in
//! the `fd` field.
//!
//! # Copies and trust
//!
//! The data of a transaction is copied once from the write stream into the
//! receiver's buffer. Offsets and objects are read once each from the
//! write stream into broker memory, validated there and the translated
//! objects written into the receiver's buffer; nothing is read back from
//! the receiver's buffer (the receiver can write its own view, see
//! [`crate::device`]), and the references a buffer holds are recorded in
//! the broker, not re-read from the buffer when it is freed.

pub mod alloc;

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::uapi::*;
use alloc::{Allocator, align8};

/// Linux error numbers the broker reports.
pub mod errno {
    pub const EPERM: i32 = 1;
    pub const ESRCH: i32 = 3;
    pub const EINTR: i32 = 4;
    pub const EBADF: i32 = 9;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EFAULT: i32 = 14;
    pub const EBUSY: i32 = 16;
    pub const EINVAL: i32 = 22;
    pub const ENOSPC: i32 = 28;
}
use errno::*;

/// A process known to the broker.
pub type ProcId = u64;
type NodeId = u64;
type TxId = u64;
type DeathId = u64;

/// What the broker needs from the host of one guest process.
pub trait ProcEnv: Send + Sync + Any {
    /// Duplicates the Windows handle `handle` of the process of `from` into
    /// this process; the new handle value. Used for file descriptors in
    /// transactions.
    fn import_handle(&self, from: &dyn ProcEnv, handle: u32) -> Result<u32, i32>;

    /// Work arrived that a thread of the process may not be waiting for
    /// (for `poll` on the binder descriptor).
    fn notify_work(&self) {}
}

/// The receive buffer of a process, as the broker sees it.
#[derive(Debug)]
pub struct BufferMap {
    /// The broker's writable view.
    pub ptr: *mut u8,
    pub len: usize,
    /// The address of the guest's view.
    pub guest_base: u64,
}

// SAFETY: the view is plain shared memory; the broker writes it only under
// its lock.
unsafe impl Send for BufferMap {}

impl BufferMap {
    /// Copies `src` to `off`. The caller checked the bounds.
    fn write(&self, off: usize, src: &[u8]) {
        assert!(off + src.len() <= self.len);
        // SAFETY: in bounds of the mapping (asserted); `src` is broker or
        // lane memory, never the same range.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.add(off), src.len()) };
    }
}

/// The result of [`Broker::write_read`].
#[derive(Debug, Default)]
pub struct WriteRead {
    /// Bytes of the packed write stream consumed.
    pub write_consumed: usize,
    /// The `BR_*` stream for the thread's read buffer.
    pub read: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// `BR_TRANSACTION` or `BR_REPLY`.
    Tx(TxId),
    Complete,
    Node(NodeId),
    Death(DeathId),
}

const LOOPER_REGISTERED: u32 = 1;
const LOOPER_ENTERED: u32 = 2;
const LOOPER_EXITED: u32 = 4;

#[derive(Debug)]
struct Thread {
    todo: VecDeque<Work>,
    stack: Option<TxId>,
    looper: u32,
    need_return: bool,
    /// `BR_TRANSACTION_COMPLETE` of a synchronous transaction, delivered
    /// with the reply.
    deferred_complete: bool,
    return_error: Option<u32>,
    cv: Arc<Condvar>,
    /// Blocked in a read.
    waiting: bool,
    dead: bool,
}

impl Thread {
    fn new() -> Self {
        Self {
            todo: VecDeque::new(),
            stack: None,
            looper: 0,
            need_return: true,
            deferred_complete: false,
            return_error: None,
            cv: Arc::new(Condvar::new()),
            waiting: false,
            dead: false,
        }
    }
}

#[derive(Debug)]
struct Ref {
    node: NodeId,
    strong: u32,
    weak: u32,
    death: Option<DeathId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeathState {
    Armed,
    Queued,
    Delivered,
}

#[derive(Debug)]
struct Death {
    cookie: u64,
    state: DeathState,
    cleared: bool,
    queued: bool,
}

#[derive(Debug, Clone, Copy)]
enum Release {
    /// A reference of the buffer's process.
    Ref { desc: u32, strong: bool },
    /// A temporary reference on a node.
    Node { node: NodeId, strong: bool },
}

#[derive(Debug)]
struct Buf {
    size: usize,
    data_size: usize,
    offsets_size: usize,
    tx: Option<TxId>,
    async_node: Option<NodeId>,
    releases: Vec<Release>,
    is_async: bool,
    user_free: bool,
}

#[derive(Debug)]
struct Proc {
    pid: i32,
    euid: u32,
    env: Arc<dyn ProcEnv>,
    threads: HashMap<i32, Thread>,
    todo: VecDeque<Work>,
    waiting: VecDeque<i32>,
    nodes: HashMap<u64, NodeId>,
    refs: BTreeMap<u32, Ref>,
    ref_by_node: HashMap<NodeId, u32>,
    deaths: HashMap<DeathId, (u32, Death)>,
    buffer: Option<BufferMap>,
    alloc: Allocator,
    async_free: usize,
    buffers: HashMap<usize, Buf>,
    max_threads: u32,
    requested_threads: u32,
    requested_started: u32,
    dead: bool,
}

impl std::fmt::Debug for dyn ProcEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProcEnv")
    }
}

#[derive(Debug)]
struct Node {
    owner: Option<ProcId>,
    ptr: u64,
    cookie: u64,
    accept_fds: bool,
    secctx: bool,
    internal_strong: u32,
    local_strong: u32,
    local_weak: u32,
    refs: HashSet<(ProcId, u32)>,
    has_strong: bool,
    has_weak: bool,
    pending_strong: bool,
    pending_weak: bool,
    queued: bool,
    async_busy: bool,
    async_todo: VecDeque<TxId>,
}

impl Node {
    fn want_strong(&self) -> bool {
        self.internal_strong > 0 || self.local_strong > 0
    }
    fn want_weak(&self) -> bool {
        self.want_strong() || !self.refs.is_empty() || self.local_weak > 0
    }
}

#[derive(Debug)]
struct Tx {
    /// The sending thread, for a synchronous transaction.
    from: Option<(ProcId, i32)>,
    from_parent: Option<TxId>,
    to_proc: ProcId,
    to_thread: Option<i32>,
    to_parent: Option<TxId>,
    node: Option<NodeId>,
    code: u32,
    flags: u32,
    sender_pid: i32,
    sender_euid: u32,
    buffer: Option<usize>,
    is_reply: bool,
}

#[derive(Debug, Default)]
struct State {
    procs: HashMap<ProcId, Proc>,
    nodes: HashMap<NodeId, Node>,
    txs: HashMap<TxId, Tx>,
    next_id: u64,
    ctx_mgr: Option<NodeId>,
    ctx_mgr_uid: Option<u32>,
}

/// The broker. See the module documentation.
#[derive(Debug, Default)]
pub struct Broker {
    state: Mutex<State>,
}

/// Statistics for tests and diagnostics.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub procs: usize,
    pub nodes: usize,
    pub transactions: usize,
    pub refs: usize,
    pub buffers: usize,
}

/// A read's stream, or its error; the lock either way.
type ReadResult<'a> = Result<(MutexGuard<'a, State>, Vec<u8>), (MutexGuard<'a, State>, i32)>;

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const OBJ_SIZE: usize = core::mem::size_of::<flat_binder_object>();
const TR_SIZE: usize = core::mem::size_of::<binder_transaction_data>();

impl Broker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Adds a process with the identity sysd assigned to it: `pid` and
    /// `euid` are what receivers see as the calling pid and uid.
    pub fn add_proc(&self, pid: i32, euid: u32, env: Arc<dyn ProcEnv>) -> ProcId {
        let mut s = lock(&self.state);
        s.next_id += 1;
        let id = s.next_id;
        s.procs.insert(
            id,
            Proc {
                pid,
                euid,
                env,
                threads: HashMap::new(),
                todo: VecDeque::new(),
                waiting: VecDeque::new(),
                nodes: HashMap::new(),
                refs: BTreeMap::new(),
                ref_by_node: HashMap::new(),
                deaths: HashMap::new(),
                buffer: None,
                alloc: Allocator::new(0),
                async_free: 0,
                buffers: HashMap::new(),
                max_threads: 0,
                requested_threads: 0,
                requested_started: 0,
                dead: false,
            },
        );
        id
    }

    /// Sets the receive buffer (the `mmap` of the device). Once per
    /// process, as the kernel allows.
    pub fn set_buffer(&self, p: ProcId, map: BufferMap) -> Result<(), i32> {
        let mut s = lock(&self.state);
        let proc = s.procs.get_mut(&p).ok_or(ESRCH)?;
        if proc.buffer.is_some() {
            return Err(EBUSY);
        }
        proc.alloc = Allocator::new(map.len);
        proc.async_free = proc.alloc.len() / 2;
        proc.buffer = Some(map);
        Ok(())
    }

    /// `BINDER_SET_MAX_THREADS`.
    pub fn set_max_threads(&self, p: ProcId, n: u32) -> Result<(), i32> {
        let mut s = lock(&self.state);
        s.procs.get_mut(&p).ok_or(ESRCH)?.max_threads = n;
        Ok(())
    }

    /// `BINDER_SET_CONTEXT_MGR` (`obj` `None`) and `_EXT`.
    pub fn set_context_mgr(&self, p: ProcId, obj: Option<flat_binder_object>) -> Result<(), i32> {
        let mut s = lock(&self.state);
        let s = &mut *s;
        if let Some(n) = s.ctx_mgr
            && s.nodes.get(&n).is_some_and(|n| n.owner.is_some())
        {
            return Err(EBUSY);
        }
        let proc = s.procs.get(&p).ok_or(ESRCH)?;
        if let Some(uid) = s.ctx_mgr_uid
            && uid != proc.euid
        {
            return Err(EPERM);
        }
        let obj = obj.unwrap_or_default();
        let id = new_id(&mut s.next_id);
        s.nodes.insert(
            id,
            Node {
                owner: Some(p),
                ptr: obj.binder,
                cookie: obj.cookie,
                accept_fds: obj.flags & FLAT_BINDER_FLAG_ACCEPTS_FDS != 0,
                secctx: obj.flags & FLAT_BINDER_FLAG_TXN_SECURITY_CTX != 0,
                internal_strong: 0,
                // The driver's own reference, which keeps the node alive.
                local_strong: 1,
                local_weak: 1,
                refs: HashSet::new(),
                has_strong: true,
                has_weak: true,
                pending_strong: false,
                pending_weak: false,
                queued: false,
                async_busy: false,
                async_todo: VecDeque::new(),
            },
        );
        s.procs.get_mut(&p).unwrap().nodes.insert(obj.binder, id);
        s.ctx_mgr = Some(id);
        s.ctx_mgr_uid = Some(proc_euid(s, p));
        Ok(())
    }

    /// Whether thread `tid` has work it would read now (`poll`).
    pub fn has_work(&self, p: ProcId, tid: i32) -> bool {
        let mut s = lock(&self.state);
        let Some(proc) = s.procs.get_mut(&p) else {
            return true;
        };
        let t = proc.threads.entry(tid).or_insert_with(Thread::new);
        if !t.todo.is_empty() || t.return_error.is_some() || t.need_return {
            return true;
        }
        let for_proc = t.stack.is_none() && t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0;
        for_proc && !proc.todo.is_empty()
    }

    /// `BINDER_THREAD_EXIT`.
    pub fn thread_exit(&self, p: ProcId, tid: i32) {
        let mut s = lock(&self.state);
        release_thread(&mut s, p, tid);
        if let Some(proc) = s.procs.get_mut(&p) {
            proc.threads.remove(&tid);
        }
    }

    /// The process is gone (closed its device or died): its nodes die,
    /// death notifications go out, its references and buffers are
    /// released and every transaction it was part of fails.
    pub fn proc_exit(&self, p: ProcId) {
        let mut s = lock(&self.state);
        let s = &mut *s;
        let Some(proc) = s.procs.get_mut(&p) else {
            return;
        };
        proc.dead = true;
        let tids: Vec<i32> = proc.threads.keys().copied().collect();
        for tid in tids {
            release_thread(s, p, tid);
        }
        // Undelivered work: synchronous transactions fail.
        let todo: Vec<Work> = s.procs.get_mut(&p).unwrap().todo.drain(..).collect();
        for w in todo {
            if let Work::Tx(t) = w {
                fail_tx(s, t, BR_DEAD_REPLY);
            }
        }
        // Buffers release what they hold.
        let bufs: Vec<Buf> = s
            .procs
            .get_mut(&p)
            .unwrap()
            .buffers
            .drain()
            .map(|(_, b)| b)
            .collect();
        for b in bufs {
            if let Some(t) = b.tx
                && let Some(tx) = s.txs.get_mut(&t)
            {
                tx.buffer = None;
            }
            run_releases(s, p, &b.releases);
        }
        // Nodes die.
        let nodes: Vec<NodeId> = s.procs[&p].nodes.values().copied().collect();
        for n in nodes {
            let is_ctx = s.ctx_mgr == Some(n);
            let Some(node) = s.nodes.get_mut(&n) else {
                continue;
            };
            if is_ctx {
                // The driver's own reference.
                node.local_strong -= 1;
                node.local_weak -= 1;
            }
            node.owner = None;
            node.queued = false;
            node.has_strong = false;
            node.has_weak = false;
            let pending: Vec<TxId> = node.async_todo.drain(..).collect();
            let refs: Vec<(ProcId, u32)> = node.refs.iter().copied().collect();
            for t in pending {
                s.txs.remove(&t);
            }
            for (rp, desc) in refs {
                let Some(rproc) = s.procs.get_mut(&rp) else {
                    continue;
                };
                if let Some(d) = rproc.refs.get(&desc).and_then(|r| r.death)
                    && let Some((_, death)) = rproc.deaths.get_mut(&d)
                    && death.state == DeathState::Armed
                {
                    death.state = DeathState::Queued;
                    if !death.queued {
                        death.queued = true;
                        enqueue_proc(s, rp, Work::Death(d));
                    }
                }
            }
            maybe_free_node(s, n);
        }
        if s.ctx_mgr.is_some_and(|n| !s.nodes.contains_key(&n)) {
            s.ctx_mgr = None;
        }
        // References held by the process go away.
        let refs: Vec<(u32, NodeId, u32)> = s.procs[&p]
            .refs
            .iter()
            .map(|(d, r)| (*d, r.node, r.strong))
            .collect();
        for (desc, node, strong) in refs {
            if let Some(n) = s.nodes.get_mut(&node) {
                if strong > 0 {
                    n.internal_strong -= 1;
                }
                n.refs.remove(&(p, desc));
            }
            node_changed(s, node, None);
        }
        let proc = s.procs.remove(&p).unwrap();
        for t in proc.threads.values() {
            t.cv.notify_all();
        }
    }

    /// Counts, for tests.
    pub fn stats(&self) -> Stats {
        let s = lock(&self.state);
        Stats {
            procs: s.procs.len(),
            nodes: s.nodes.len(),
            transactions: s.txs.len(),
            refs: s.procs.values().map(|p| p.refs.len()).sum(),
            buffers: s.procs.values().map(|p| p.buffers.len()).sum(),
        }
    }

    /// Strong and weak counts of reference `desc` of process `p`
    /// (`BINDER_GET_NODE_INFO_FOR_REF` reports the node's; tests use this).
    pub fn ref_counts(&self, p: ProcId, desc: u32) -> Option<(u32, u32)> {
        let s = lock(&self.state);
        s.procs.get(&p)?.refs.get(&desc).map(|r| (r.strong, r.weak))
    }

    /// `BINDER_WRITE_READ`: processes the packed write stream, then, when
    /// `read_cap` is not zero, blocks until there is work and returns the
    /// `BR_*` stream (at most `read_cap` bytes). `first_read` is whether
    /// the read buffer was empty (`read_consumed == 0`): the stream then
    /// starts with `BR_NOOP`, as the kernel's does.
    ///
    /// An error ends the call as the ioctl's error, with the bytes of the
    /// write stream consumed so far.
    pub fn write_read(
        &self,
        p: ProcId,
        tid: i32,
        write: &[u8],
        read_cap: usize,
        first_read: bool,
        nonblock: bool,
    ) -> Result<WriteRead, (i32, usize)> {
        let mut g = lock(&self.state);
        {
            let proc = g.procs.get_mut(&p).ok_or((ESRCH, 0))?;
            if proc.dead {
                return Err((ESRCH, 0));
            }
            let t = proc.threads.entry(tid).or_insert_with(Thread::new);
            if t.looper & LOOPER_EXITED != 0 {
                // A thread that exited the looper may come back.
                t.looper &= !LOOPER_EXITED;
            }
        }
        let mut out = WriteRead::default();
        if !write.is_empty() {
            match thread_write(&mut g, p, tid, write) {
                Ok(n) => out.write_consumed = n,
                Err(e) => {
                    clear_need_return(&mut g, p, tid);
                    return Err(e);
                }
            }
        }
        if read_cap > 0 {
            let r = self.thread_read(g, p, tid, read_cap, first_read, nonblock);
            match r {
                Ok((mut g, read)) => {
                    out.read = read;
                    clear_need_return(&mut g, p, tid);
                }
                Err((mut g, e)) => {
                    clear_need_return(&mut g, p, tid);
                    return Err((e, out.write_consumed));
                }
            }
        } else {
            clear_need_return(&mut g, p, tid);
        }
        Ok(out)
    }

    fn thread_read<'a>(
        &'a self,
        mut g: MutexGuard<'a, State>,
        p: ProcId,
        tid: i32,
        cap: usize,
        first: bool,
        nonblock: bool,
    ) -> ReadResult<'a> {
        let mut out = Vec::new();
        if first {
            if cap < 4 {
                return Ok((g, out));
            }
            out.extend_from_slice(&BR_NOOP.to_ne_bytes());
        }
        loop {
            let Some(proc) = g.procs.get_mut(&p) else {
                return Err((g, ESRCH));
            };
            let t = proc.threads.get_mut(&tid).unwrap();
            if t.dead {
                return Err((g, ESRCH));
            }
            let for_proc = t.stack.is_none()
                && t.todo.is_empty()
                && t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0;
            let has = !t.todo.is_empty()
                || t.return_error.is_some()
                || (for_proc && !proc.todo.is_empty());
            if has {
                let progress = deliver(&mut g, p, tid, for_proc, cap, &mut out);
                // Stale work only: look again.
                if out.len() > usize::from(first) * 4 || !progress {
                    break;
                }
                continue;
            }
            if t.need_return || out.len() > usize::from(first) * 4 {
                break;
            }
            if nonblock {
                return Err((g, EAGAIN));
            }
            t.waiting = true;
            if for_proc {
                proc.waiting.push_back(tid);
            }
            let cv = t.cv.clone();
            g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
            if let Some(proc) = g.procs.get_mut(&p) {
                proc.waiting.retain(|&w| w != tid);
                if let Some(t) = proc.threads.get_mut(&tid) {
                    t.waiting = false;
                }
            }
        }
        // Ask for another looper thread when none is left waiting.
        if let Some(proc) = g.procs.get_mut(&p) {
            let looper = proc.threads[&tid].looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0;
            if proc.requested_threads == 0
                && proc.waiting.is_empty()
                && proc.requested_started < proc.max_threads
                && looper
                && cap - out.len() >= 4
            {
                proc.requested_threads += 1;
                out.extend_from_slice(&BR_SPAWN_LOOPER.to_ne_bytes());
            }
        }
        Ok((g, out))
    }
}

fn new_id(next: &mut u64) -> u64 {
    *next += 1;
    *next
}

fn proc_euid(s: &State, p: ProcId) -> u32 {
    s.procs[&p].euid
}

fn clear_need_return(s: &mut State, p: ProcId, tid: i32) {
    if let Some(t) = s.procs.get_mut(&p).and_then(|p| p.threads.get_mut(&tid)) {
        t.need_return = false;
    }
}

fn wake_thread(s: &State, p: ProcId, tid: i32) {
    if let Some(proc) = s.procs.get(&p)
        && let Some(t) = proc.threads.get(&tid)
    {
        if t.waiting {
            t.cv.notify_one();
        } else {
            proc.env.notify_work();
        }
    }
}

fn enqueue_thread(s: &mut State, p: ProcId, tid: i32, w: Work) {
    if let Some(t) = s.procs.get_mut(&p).and_then(|p| p.threads.get_mut(&tid)) {
        t.todo.push_back(w);
        wake_thread(s, p, tid);
    }
}

/// Queues process work: to a thread waiting for it if there is one (the
/// kernel's thread selection), else to the process queue.
fn enqueue_proc(s: &mut State, p: ProcId, w: Work) {
    let Some(proc) = s.procs.get_mut(&p) else {
        return;
    };
    if let Some(tid) = proc.waiting.pop_front() {
        let t = proc.threads.get_mut(&tid).unwrap();
        t.todo.push_back(w);
        t.cv.notify_one();
    } else {
        proc.todo.push_back(w);
        proc.env.notify_work();
    }
}

/// Re-evaluates what the owner of `n` must be told and frees the node when
/// nothing refers to it. `hint` is the thread to give node work to when it
/// belongs to the owner.
fn node_changed(s: &mut State, n: NodeId, hint: Option<(ProcId, i32)>) {
    let Some(node) = s.nodes.get_mut(&n) else {
        return;
    };
    if let Some(owner) = node.owner {
        let differs = node.want_strong() != node.has_strong || node.want_weak() != node.has_weak;
        if differs && !node.queued {
            node.queued = true;
            match hint {
                Some((hp, ht)) if hp == owner => enqueue_thread(s, hp, ht, Work::Node(n)),
                _ => enqueue_proc(s, owner, Work::Node(n)),
            }
            return;
        }
    }
    maybe_free_node(s, n);
}

fn maybe_free_node(s: &mut State, n: NodeId) {
    let Some(node) = s.nodes.get(&n) else {
        return;
    };
    if node.has_strong
        || node.has_weak
        || node.queued
        || !node.refs.is_empty()
        || node.local_strong > 0
        || node.local_weak > 0
        || node.internal_strong > 0
    {
        return;
    }
    let node = s.nodes.remove(&n).unwrap();
    if let Some(o) = node.owner
        && let Some(proc) = s.procs.get_mut(&o)
    {
        proc.nodes.remove(&node.ptr);
    }
    if s.ctx_mgr == Some(n) {
        s.ctx_mgr = None;
    }
}

/// The reference of `p` to `node`, created when missing; its descriptor.
fn get_ref(s: &mut State, p: ProcId, node: NodeId) -> u32 {
    if let Some(&d) = s.procs[&p].ref_by_node.get(&node) {
        return d;
    }
    let is_ctx = s.ctx_mgr == Some(node);
    let proc = s.procs.get_mut(&p).unwrap();
    let desc = if is_ctx && !proc.refs.contains_key(&0) {
        0
    } else {
        let mut d = 1u32;
        for &k in proc.refs.keys() {
            if k < d {
                continue;
            }
            if k == d {
                d += 1;
            } else {
                break;
            }
        }
        d
    };
    proc.refs.insert(
        desc,
        Ref {
            node,
            strong: 0,
            weak: 0,
            death: None,
        },
    );
    proc.ref_by_node.insert(node, desc);
    s.nodes.get_mut(&node).unwrap().refs.insert((p, desc));
    desc
}

/// Increments reference `desc` of `p`.
fn inc_ref(s: &mut State, p: ProcId, desc: u32, strong: bool, hint: Option<(ProcId, i32)>) {
    let r = s.procs.get_mut(&p).unwrap().refs.get_mut(&desc).unwrap();
    let node = r.node;
    if strong {
        r.strong += 1;
        if r.strong == 1
            && let Some(n) = s.nodes.get_mut(&node)
        {
            n.internal_strong += 1;
        }
    } else {
        r.weak += 1;
    }
    node_changed(s, node, hint);
}

/// Decrements reference `desc` of `p`, deleting it at zero.
fn dec_ref(s: &mut State, p: ProcId, desc: u32, strong: bool) -> Result<(), i32> {
    let proc = s.procs.get_mut(&p).ok_or(ESRCH)?;
    let r = proc.refs.get_mut(&desc).ok_or(EINVAL)?;
    let node = r.node;
    if strong {
        if r.strong == 0 {
            return Err(EINVAL);
        }
        r.strong -= 1;
        if r.strong == 0
            && let Some(n) = s.nodes.get_mut(&node)
        {
            n.internal_strong -= 1;
        }
    } else {
        if r.weak == 0 {
            return Err(EINVAL);
        }
        r.weak -= 1;
    }
    let proc = s.procs.get_mut(&p).unwrap();
    let r = &proc.refs[&desc];
    if r.strong == 0 && r.weak == 0 {
        let r = proc.refs.remove(&desc).unwrap();
        proc.ref_by_node.remove(&node);
        if let Some(d) = r.death
            && let Some((_, death)) = proc.deaths.get(&d)
            && !death.queued
        {
            proc.deaths.remove(&d);
        }
        if let Some(n) = s.nodes.get_mut(&node) {
            n.refs.remove(&(p, desc));
        }
    }
    node_changed(s, node, None);
    Ok(())
}

fn run_releases(s: &mut State, p: ProcId, rel: &[Release]) {
    for r in rel {
        match *r {
            Release::Ref { desc, strong } => {
                let _ = dec_ref(s, p, desc, strong);
            }
            Release::Node { node, strong } => {
                if let Some(n) = s.nodes.get_mut(&node) {
                    if strong {
                        n.local_strong = n.local_strong.saturating_sub(1);
                    } else {
                        n.local_weak = n.local_weak.saturating_sub(1);
                    }
                }
                node_changed(s, node, None);
            }
        }
    }
}

/// Frees buffer `off` of `p`.
fn free_buffer(s: &mut State, p: ProcId, off: usize) {
    let Some(b) = s.procs.get_mut(&p).and_then(|pr| pr.buffers.remove(&off)) else {
        return;
    };
    if let Some(t) = b.tx
        && let Some(tx) = s.txs.get_mut(&t)
    {
        tx.buffer = None;
    }
    if let Some(n) = b.async_node {
        let next = s.nodes.get_mut(&n).and_then(|node| {
            let next = node.async_todo.pop_front();
            node.async_busy = next.is_some();
            next
        });
        if let Some(t) = next {
            enqueue_proc(s, p, Work::Tx(t));
        }
    }
    run_releases(s, p, &b.releases);
    let proc = s.procs.get_mut(&p).unwrap();
    proc.alloc.free(off, b.size);
    if b.is_async {
        proc.async_free += align8(b.size.max(8)).unwrap_or(b.size);
    }
}

/// Fails transaction `t`: its sender gets `code`.
fn fail_tx(s: &mut State, t: TxId, code: u32) {
    let Some(tx) = s.txs.remove(&t) else {
        return;
    };
    if let Some(off) = tx.buffer
        && let Some(b) = s
            .procs
            .get_mut(&tx.to_proc)
            .and_then(|p| p.buffers.get_mut(&off))
    {
        b.tx = None;
        if !b.user_free {
            // Never delivered: the receiver will not free it.
            free_buffer(s, tx.to_proc, off);
        }
    }
    if let Some((fp, ft)) = tx.from
        && let Some(th) = s.procs.get_mut(&fp).and_then(|p| p.threads.get_mut(&ft))
    {
        if th.stack == Some(t) {
            th.stack = tx.from_parent;
        }
        th.return_error = Some(code);
        th.deferred_complete = false;
        wake_thread(s, fp, ft);
    }
}

/// The kernel's `binder_thread_release`: transactions the thread was
/// serving fail with `BR_DEAD_REPLY`; those it was waiting for lose their
/// sender.
fn release_thread(s: &mut State, p: ProcId, tid: i32) {
    let Some(t) = s.procs.get_mut(&p).and_then(|pr| pr.threads.get_mut(&tid)) else {
        return;
    };
    t.dead = true;
    t.cv.notify_all();
    let mut cur = t.stack.take();
    let todo: Vec<Work> = t.todo.drain(..).collect();
    s.procs.get_mut(&p).unwrap().waiting.retain(|&w| w != tid);
    while let Some(id) = cur {
        let Some(tx) = s.txs.get_mut(&id) else {
            break;
        };
        if tx.to_proc == p && tx.to_thread == Some(tid) {
            cur = tx.to_parent;
            tx.to_thread = None;
            fail_tx(s, id, BR_DEAD_REPLY);
        } else if tx.from == Some((p, tid)) {
            cur = tx.from_parent;
            tx.from = None;
        } else {
            break;
        }
    }
    for w in todo {
        match w {
            Work::Tx(t) => {
                let reply = s.txs.get(&t).is_some_and(|tx| tx.is_reply);
                if reply {
                    if let Some(tx) = s.txs.remove(&t)
                        && let Some(off) = tx.buffer
                    {
                        free_buffer(s, tx.to_proc, off);
                    }
                } else {
                    fail_tx(s, t, BR_DEAD_REPLY);
                }
            }
            Work::Node(n) => {
                if let Some(node) = s.nodes.get_mut(&n) {
                    node.queued = false;
                }
                node_changed(s, n, None);
            }
            Work::Death(d) => {
                // Another thread of the process takes it.
                let alive = s.procs.get(&p).is_some_and(|pr| !pr.dead);
                if alive && s.procs[&p].deaths.contains_key(&d) {
                    enqueue_proc(s, p, Work::Death(d));
                }
            }
            Work::Complete => {}
        }
    }
}

/// Reads a value from the packed write stream.
fn rd<T: Pod>(w: &[u8], off: usize) -> Result<T, (i32, usize)> {
    read_at::<T>(w, off).ok_or((EFAULT, off))
}

fn thread_write(s: &mut State, p: ProcId, tid: i32, w: &[u8]) -> Result<usize, (i32, usize)> {
    let mut pos = 0usize;
    while pos < w.len() {
        let cmd: u32 = rd(w, pos)?;
        let start = pos;
        pos += 4;
        if !is_command(cmd) {
            return Err((EINVAL, start));
        }
        let size = ioc_size(cmd);
        if w.len() - pos < size {
            return Err((EFAULT, start));
        }
        let arg = pos;
        pos += size;
        match cmd {
            BC_INCREFS | BC_ACQUIRE | BC_RELEASE | BC_DECREFS => {
                let desc: u32 = rd(w, arg)?;
                let strong = matches!(cmd, BC_ACQUIRE | BC_RELEASE);
                if matches!(cmd, BC_INCREFS | BC_ACQUIRE) {
                    let target = if desc == 0 {
                        // Handle 0 is the context manager.
                        match s.ctx_mgr {
                            Some(n) if !s.procs[&p].refs.contains_key(&0) => Some(get_ref(s, p, n)),
                            _ => s.procs[&p].refs.contains_key(&0).then_some(0),
                        }
                    } else {
                        s.procs[&p].refs.contains_key(&desc).then_some(desc)
                    };
                    if let Some(d) = target {
                        inc_ref(s, p, d, strong, Some((p, tid)));
                    }
                } else {
                    let _ = dec_ref(s, p, desc, strong);
                }
            }
            BC_INCREFS_DONE | BC_ACQUIRE_DONE => {
                let pc: binder_ptr_cookie = rd(w, arg)?;
                if let Some(&n) = s.procs[&p].nodes.get(&pc.ptr) {
                    let node = s.nodes.get_mut(&n).unwrap();
                    if node.cookie == pc.cookie {
                        if cmd == BC_ACQUIRE_DONE {
                            node.pending_strong = false;
                        } else {
                            node.pending_weak = false;
                        }
                        node_changed(s, n, Some((p, tid)));
                    }
                }
            }
            BC_ATTEMPT_ACQUIRE | BC_ACQUIRE_RESULT => return Err((EINVAL, start)),
            BC_FREE_BUFFER => {
                let ptr: u64 = rd(w, arg)?;
                let proc = &s.procs[&p];
                if let Some(map) = &proc.buffer
                    && let Some(off) = ptr.checked_sub(map.guest_base)
                    && proc
                        .buffers
                        .get(&(off as usize))
                        .is_some_and(|b| b.user_free)
                {
                    free_buffer(s, p, off as usize);
                }
            }
            BC_TRANSACTION | BC_REPLY | BC_TRANSACTION_SG | BC_REPLY_SG => {
                let tr: binder_transaction_data = rd(w, arg)?;
                let sg = matches!(cmd, BC_TRANSACTION_SG | BC_REPLY_SG);
                let buffers_size: u64 = if sg { rd(w, arg + TR_SIZE)? } else { 0 };
                let ds = usize::try_from(tr.data_size).map_err(|_| (EINVAL, start))?;
                let os = usize::try_from(tr.offsets_size).map_err(|_| (EINVAL, start))?;
                let dlen = align8(ds).ok_or((EINVAL, start))?;
                let olen = align8(os).ok_or((EINVAL, start))?;
                let data_at = pos;
                let end = pos
                    .checked_add(dlen)
                    .and_then(|v| v.checked_add(olen))
                    .ok_or((EFAULT, start))?;
                if end > w.len() {
                    return Err((EFAULT, start));
                }
                pos = end;
                let reply = matches!(cmd, BC_REPLY | BC_REPLY_SG);
                let data = &w[data_at..data_at + ds];
                let offs = &w[data_at + dlen..data_at + dlen + os];
                if buffers_size != 0 {
                    // Scatter-gather buffers (BINDER_TYPE_PTR) are not
                    // supported; libbinder does not send them.
                    s.procs
                        .get_mut(&p)
                        .unwrap()
                        .threads
                        .get_mut(&tid)
                        .unwrap()
                        .return_error = Some(BR_FAILED_REPLY);
                } else {
                    transaction(s, p, tid, &tr, data, offs, reply);
                }
            }
            BC_REGISTER_LOOPER => {
                let proc = s.procs.get_mut(&p).unwrap();
                let t = proc.threads.get_mut(&tid).unwrap();
                if t.looper & LOOPER_ENTERED == 0 && proc.requested_threads > 0 {
                    proc.requested_threads -= 1;
                    proc.requested_started += 1;
                }
                t.looper |= LOOPER_REGISTERED;
            }
            BC_ENTER_LOOPER => {
                let t = s.procs.get_mut(&p).unwrap().threads.get_mut(&tid).unwrap();
                t.looper |= LOOPER_ENTERED;
            }
            BC_EXIT_LOOPER => {
                let t = s.procs.get_mut(&p).unwrap().threads.get_mut(&tid).unwrap();
                t.looper |= LOOPER_EXITED;
            }
            BC_REQUEST_DEATH_NOTIFICATION | BC_CLEAR_DEATH_NOTIFICATION => {
                let hc: binder_handle_cookie = rd(w, arg)?;
                let (desc, cookie) = (hc.handle, hc.cookie);
                death_cmd(
                    s,
                    p,
                    tid,
                    desc,
                    cookie,
                    cmd == BC_REQUEST_DEATH_NOTIFICATION,
                );
            }
            BC_DEAD_BINDER_DONE => {
                let cookie: u64 = rd(w, arg)?;
                let proc = s.procs.get_mut(&p).unwrap();
                let found = proc
                    .deaths
                    .iter()
                    .find(|(_, (_, d))| d.cookie == cookie && d.state == DeathState::Delivered)
                    .map(|(k, _)| *k);
                if let Some(d) = found {
                    let (_, death) = proc.deaths.get_mut(&d).unwrap();
                    if death.cleared {
                        // The clear was requested meanwhile: confirm it now.
                        death.queued = true;
                        death.state = DeathState::Armed;
                        enqueue_thread_or_proc(s, p, tid, Work::Death(d));
                    } else {
                        proc.deaths.remove(&d);
                    }
                }
            }
            BC_REQUEST_FREEZE_NOTIFICATION
            | BC_CLEAR_FREEZE_NOTIFICATION
            | BC_FREEZE_NOTIFICATION_DONE => {
                // No freezer: nothing is ever frozen.
            }
            _ => return Err((EINVAL, start)),
        }
    }
    Ok(pos)
}

/// Work for a looper thread goes to that thread, else to the process.
fn enqueue_thread_or_proc(s: &mut State, p: ProcId, tid: i32, w: Work) {
    let looper = s.procs[&p]
        .threads
        .get(&tid)
        .is_some_and(|t| t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0);
    if looper {
        enqueue_thread(s, p, tid, w);
    } else {
        enqueue_proc(s, p, w);
    }
}

fn death_cmd(s: &mut State, p: ProcId, tid: i32, desc: u32, cookie: u64, request: bool) {
    let Some(r) = s.procs[&p].refs.get(&desc) else {
        return;
    };
    let node = r.node;
    let existing = r.death;
    if request {
        if existing.is_some() {
            return;
        }
        let id = new_id(&mut s.next_id);
        let dead = s.nodes.get(&node).is_none_or(|n| n.owner.is_none());
        let proc = s.procs.get_mut(&p).unwrap();
        proc.refs.get_mut(&desc).unwrap().death = Some(id);
        proc.deaths.insert(
            id,
            (
                desc,
                Death {
                    cookie,
                    state: if dead {
                        DeathState::Queued
                    } else {
                        DeathState::Armed
                    },
                    cleared: false,
                    queued: dead,
                },
            ),
        );
        if dead {
            enqueue_thread_or_proc(s, p, tid, Work::Death(id));
        }
    } else {
        let Some(id) = existing else {
            return;
        };
        let proc = s.procs.get_mut(&p).unwrap();
        let (_, death) = proc.deaths.get_mut(&id).unwrap();
        if death.cookie != cookie {
            return;
        }
        proc.refs.get_mut(&desc).unwrap().death = None;
        death.cleared = true;
        match death.state {
            DeathState::Armed => {
                if !death.queued {
                    death.queued = true;
                    enqueue_thread_or_proc(s, p, tid, Work::Death(id));
                }
            }
            // BR_DEAD_BINDER first, then the clear is confirmed after
            // BC_DEAD_BINDER_DONE.
            DeathState::Queued | DeathState::Delivered => {}
        }
    }
}

fn set_return_error(s: &mut State, p: ProcId, tid: i32, code: u32) {
    if let Some(t) = s.procs.get_mut(&p).and_then(|pr| pr.threads.get_mut(&tid)) {
        t.return_error = Some(code);
    }
}

/// `BC_TRANSACTION` / `BC_REPLY` from thread `tid` of `p`.
fn transaction(
    s: &mut State,
    p: ProcId,
    tid: i32,
    tr: &binder_transaction_data,
    data: &[u8],
    offs: &[u8],
    reply: bool,
) {
    let oneway = !reply && tr.flags & TF_ONE_WAY != 0;
    let mut in_reply_to = None;
    let (target_proc, target_thread, node, accept_fds) = if reply {
        let t = s.procs[&p].threads[&tid].stack;
        let Some(id) = t else {
            return set_return_error(s, p, tid, BR_FAILED_REPLY);
        };
        let Some(irt) = s.txs.get(&id) else {
            return set_return_error(s, p, tid, BR_FAILED_REPLY);
        };
        if irt.to_proc != p || irt.to_thread != Some(tid) {
            return set_return_error(s, p, tid, BR_FAILED_REPLY);
        }
        let to_parent = irt.to_parent;
        let from = irt.from;
        let accept = irt.flags & TF_ACCEPT_FDS != 0;
        s.procs
            .get_mut(&p)
            .unwrap()
            .threads
            .get_mut(&tid)
            .unwrap()
            .stack = to_parent;
        in_reply_to = Some(id);
        let Some((fp, ft)) = from.filter(|(fp, ft)| {
            s.procs
                .get(fp)
                .and_then(|pr| pr.threads.get(ft))
                .is_some_and(|t| !t.dead)
        }) else {
            s.txs.remove(&id);
            return set_return_error(s, p, tid, BR_DEAD_REPLY);
        };
        (fp, Some(ft), None, accept)
    } else {
        let handle = tr.target as u32;
        let node = if handle == 0 {
            s.ctx_mgr
        } else {
            s.procs[&p]
                .refs
                .get(&handle)
                .filter(|r| r.strong > 0)
                .map(|r| r.node)
        };
        let Some(n) = node else {
            return set_return_error(
                s,
                p,
                tid,
                if handle == 0 {
                    BR_DEAD_REPLY
                } else {
                    BR_FAILED_REPLY
                },
            );
        };
        let nd = &s.nodes[&n];
        let Some(owner) = nd.owner else {
            return set_return_error(s, p, tid, BR_DEAD_REPLY);
        };
        if owner == p {
            return set_return_error(s, p, tid, BR_FAILED_REPLY);
        }
        let accept = nd.accept_fds;
        // A nested call goes to the thread of the target that is waiting
        // in this thread's transaction chain.
        let mut target_thread = None;
        if !oneway {
            let mut cur = s.procs[&p].threads[&tid].stack;
            while let Some(id) = cur {
                let Some(tx) = s.txs.get(&id) else { break };
                if let Some((fp, ft)) = tx.from
                    && fp == owner
                {
                    target_thread = Some(ft);
                }
                cur = tx.from_parent;
            }
        }
        (owner, target_thread, Some(n), accept)
    };
    // A failure from here on also fails the transaction being replied to.
    let fail = |s: &mut State, code: u32| {
        if let Some(id) = in_reply_to {
            fail_tx(s, id, BR_FAILED_REPLY);
        }
        set_return_error(s, p, tid, code);
    };
    if s.procs
        .get(&target_proc)
        .is_none_or(|t| t.dead || t.buffer.is_none())
    {
        return fail(s, BR_DEAD_REPLY);
    }

    // Allocate the buffer in the receiver and copy the data once.
    let ds = data.len();
    let os = offs.len();
    let dlen = align8(ds).unwrap();
    let size = dlen + align8(os).unwrap();
    if !os.is_multiple_of(8) {
        return fail(s, BR_FAILED_REPLY);
    }
    let tp = s.procs.get_mut(&target_proc).unwrap();
    let rounded = align8(size.max(8)).unwrap();
    if oneway && rounded > tp.async_free {
        return set_return_error(s, p, tid, BR_FAILED_REPLY);
    }
    let Some(off) = tp.alloc.alloc(size) else {
        return fail(s, BR_FAILED_REPLY);
    };
    if oneway {
        tp.async_free -= rounded;
    }
    let map = tp.buffer.as_ref().unwrap();
    map.write(off, data);
    // Offsets: read once, validated, written from broker memory.
    let offsets: Vec<u64> = offs
        .chunks_exact(8)
        .map(|c| u64::from_ne_bytes(c.try_into().unwrap()))
        .collect();
    for (i, o) in offsets.iter().enumerate() {
        map.write(off + dlen + i * 8, &o.to_ne_bytes());
    }
    tp.buffers.insert(
        off,
        Buf {
            size,
            data_size: ds,
            offsets_size: os,
            tx: None,
            async_node: None,
            releases: Vec::new(),
            is_async: oneway,
            user_free: false,
        },
    );

    // Translate the objects.
    let mut releases = Vec::new();
    let mut ok = true;
    let mut min = 0usize;
    for &o in &offsets {
        let Ok(o) = usize::try_from(o) else {
            ok = false;
            break;
        };
        if o < min || o % 4 != 0 || o.checked_add(OBJ_SIZE).is_none_or(|e| e > ds) {
            ok = false;
            break;
        }
        min = o + OBJ_SIZE;
        let obj: flat_binder_object = read_at(data, o).unwrap();
        match translate(s, p, tid, target_proc, obj, accept_fds, &mut releases) {
            Some(t) => {
                let mut b = [0u8; OBJ_SIZE];
                write_at(&mut b, 0, &t);
                s.procs[&target_proc]
                    .buffer
                    .as_ref()
                    .unwrap()
                    .write(off + o, &b);
            }
            None => {
                ok = false;
                break;
            }
        }
    }
    if !ok {
        run_releases(s, target_proc, &releases);
        free_buffer(s, target_proc, off);
        return fail(s, BR_FAILED_REPLY);
    }
    if let Some(n) = node {
        // The transaction holds its target node until the buffer is freed.
        s.nodes.get_mut(&n).unwrap().local_strong += 1;
        releases.push(Release::Node {
            node: n,
            strong: true,
        });
    }

    let id = new_id(&mut s.next_id);
    let (pid, euid) = (s.procs[&p].pid, s.procs[&p].euid);
    let from = (!reply && !oneway).then_some((p, tid));
    let from_parent = if from.is_some() {
        s.procs[&p].threads[&tid].stack
    } else {
        None
    };
    s.txs.insert(
        id,
        Tx {
            from,
            from_parent,
            to_proc: target_proc,
            to_thread: target_thread,
            to_parent: None,
            node,
            code: tr.code,
            flags: tr.flags,
            sender_pid: if from.is_some() { pid } else { 0 },
            sender_euid: euid,
            buffer: Some(off),
            is_reply: reply,
        },
    );
    {
        let b = s
            .procs
            .get_mut(&target_proc)
            .unwrap()
            .buffers
            .get_mut(&off)
            .unwrap();
        b.releases = releases;
        b.tx = Some(id);
        if oneway {
            b.async_node = node;
        }
    }

    if reply {
        let irt = in_reply_to.unwrap();
        let tt = target_thread.unwrap();
        if let Some(old) = s.txs.remove(&irt)
            && let Some(th) = s
                .procs
                .get_mut(&target_proc)
                .and_then(|pr| pr.threads.get_mut(&tt))
            && th.stack == Some(irt)
        {
            th.stack = old.from_parent;
        }
        enqueue_thread(s, target_proc, tt, Work::Tx(id));
        enqueue_thread(s, p, tid, Work::Complete);
    } else if oneway {
        let n = node.unwrap();
        let nd = s.nodes.get_mut(&n).unwrap();
        if nd.async_busy {
            nd.async_todo.push_back(id);
        } else {
            nd.async_busy = true;
            enqueue_proc(s, target_proc, Work::Tx(id));
        }
        enqueue_thread(s, p, tid, Work::Complete);
    } else {
        let th = s.procs.get_mut(&p).unwrap().threads.get_mut(&tid).unwrap();
        th.stack = Some(id);
        th.deferred_complete = true;
        match target_thread {
            Some(tt) => enqueue_thread(s, target_proc, tt, Work::Tx(id)),
            None => enqueue_proc(s, target_proc, Work::Tx(id)),
        }
    }
}

/// Translates one object from sender `p` for receiver `tp`.
fn translate(
    s: &mut State,
    p: ProcId,
    tid: i32,
    tp: ProcId,
    obj: flat_binder_object,
    accept_fds: bool,
    rel: &mut Vec<Release>,
) -> Option<flat_binder_object> {
    match obj.hdr_type {
        BINDER_TYPE_BINDER | BINDER_TYPE_WEAK_BINDER => {
            let strong = obj.hdr_type == BINDER_TYPE_BINDER;
            let n = match s.procs[&p].nodes.get(&obj.binder) {
                Some(&n) => {
                    if s.nodes[&n].cookie != obj.cookie {
                        return None;
                    }
                    n
                }
                None => {
                    let id = new_id(&mut s.next_id);
                    s.nodes.insert(
                        id,
                        Node {
                            owner: Some(p),
                            ptr: obj.binder,
                            cookie: obj.cookie,
                            accept_fds: obj.flags & FLAT_BINDER_FLAG_ACCEPTS_FDS != 0,
                            secctx: obj.flags & FLAT_BINDER_FLAG_TXN_SECURITY_CTX != 0,
                            internal_strong: 0,
                            local_strong: 0,
                            local_weak: 0,
                            refs: HashSet::new(),
                            has_strong: false,
                            has_weak: false,
                            pending_strong: false,
                            pending_weak: false,
                            queued: false,
                            async_busy: false,
                            async_todo: VecDeque::new(),
                        },
                    );
                    s.procs.get_mut(&p).unwrap().nodes.insert(obj.binder, id);
                    id
                }
            };
            let desc = get_ref(s, tp, n);
            inc_ref(s, tp, desc, strong, Some((p, tid)));
            rel.push(Release::Ref { desc, strong });
            Some(flat_binder_object {
                hdr_type: if strong {
                    BINDER_TYPE_HANDLE
                } else {
                    BINDER_TYPE_WEAK_HANDLE
                },
                flags: obj.flags,
                binder: u64::from(desc),
                cookie: 0,
            })
        }
        BINDER_TYPE_HANDLE | BINDER_TYPE_WEAK_HANDLE => {
            let strong = obj.hdr_type == BINDER_TYPE_HANDLE;
            let desc = obj.binder as u32;
            let r = s.procs[&p].refs.get(&desc)?;
            if strong && r.strong == 0 {
                return None;
            }
            let n = r.node;
            let node = s.nodes.get_mut(&n)?;
            if node.owner == Some(tp) {
                if strong {
                    node.local_strong += 1;
                } else {
                    node.local_weak += 1;
                }
                let (ptr, cookie) = (node.ptr, node.cookie);
                node_changed(s, n, None);
                rel.push(Release::Node { node: n, strong });
                Some(flat_binder_object {
                    hdr_type: if strong {
                        BINDER_TYPE_BINDER
                    } else {
                        BINDER_TYPE_WEAK_BINDER
                    },
                    flags: obj.flags,
                    binder: ptr,
                    cookie,
                })
            } else {
                let d = get_ref(s, tp, n);
                inc_ref(s, tp, d, strong, None);
                rel.push(Release::Ref { desc: d, strong });
                Some(flat_binder_object {
                    hdr_type: obj.hdr_type,
                    flags: obj.flags,
                    binder: u64::from(d),
                    cookie: 0,
                })
            }
        }
        BINDER_TYPE_FD => {
            if !accept_fds {
                return None;
            }
            let src = s.procs[&p].env.clone();
            let dst = s.procs[&tp].env.clone();
            let h = dst.import_handle(&*src, obj.binder as u32).ok()?;
            Some(flat_binder_object {
                hdr_type: BINDER_TYPE_FD,
                flags: obj.flags,
                binder: u64::from(h),
                cookie: obj.cookie,
            })
        }
        // BINDER_TYPE_FDA and BINDER_TYPE_PTR need scatter-gather buffers.
        _ => None,
    }
}

/// Delivers work to thread `tid` into `out` (at most `cap` bytes).
/// Whether any work was taken (false: the next item did not fit).
fn deliver(
    s: &mut State,
    p: ProcId,
    tid: i32,
    for_proc: bool,
    cap: usize,
    out: &mut Vec<u8>,
) -> bool {
    let mut progress = false;
    let room = |out: &Vec<u8>, n: usize| cap - out.len() >= n;
    {
        let t = s.procs.get_mut(&p).unwrap().threads.get_mut(&tid).unwrap();
        if let Some(e) = t.return_error {
            if !room(out, 4) {
                return false;
            }
            t.deferred_complete = false;
            t.return_error = None;
            out.extend_from_slice(&e.to_ne_bytes());
            return true;
        }
    }
    loop {
        let proc = s.procs.get_mut(&p).unwrap();
        let t = proc.threads.get_mut(&tid).unwrap();
        let (w, from_thread) = if let Some(w) = t.todo.pop_front() {
            (w, true)
        } else if for_proc && let Some(w) = proc.todo.pop_front() {
            (w, false)
        } else {
            return progress;
        };
        let put_back = move |s: &mut State| {
            let proc = s.procs.get_mut(&p).unwrap();
            if from_thread {
                proc.threads.get_mut(&tid).unwrap().todo.push_front(w);
            } else {
                proc.todo.push_front(w);
            }
            progress
        };
        progress = true;
        match w {
            Work::Complete => {
                if !room(out, 4) {
                    return put_back(s);
                }
                out.extend_from_slice(&BR_TRANSACTION_COMPLETE.to_ne_bytes());
            }
            Work::Node(n) => {
                let Some(node) = s.nodes.get_mut(&n) else {
                    continue;
                };
                if !room(out, 4 * 20) {
                    return put_back(s);
                }
                node.queued = false;
                if node.owner != Some(p) {
                    continue;
                }
                let (ws, ww) = (node.want_strong(), node.want_weak());
                let pc = binder_ptr_cookie {
                    ptr: node.ptr,
                    cookie: node.cookie,
                };
                let mut emit = |c: u32| {
                    out.extend_from_slice(&c.to_ne_bytes());
                    push(out, &pc);
                };
                if ww && !node.has_weak {
                    node.has_weak = true;
                    node.pending_weak = true;
                    emit(BR_INCREFS);
                }
                if ws && !node.has_strong {
                    node.has_strong = true;
                    node.pending_strong = true;
                    emit(BR_ACQUIRE);
                }
                if !ws && node.has_strong && !node.pending_strong {
                    node.has_strong = false;
                    emit(BR_RELEASE);
                }
                if !ww && node.has_weak && !node.pending_weak && !node.has_strong {
                    node.has_weak = false;
                    emit(BR_DECREFS);
                }
                maybe_free_node(s, n);
            }
            Work::Death(d) => {
                let Some((_, death)) = proc.deaths.get_mut(&d) else {
                    continue;
                };
                if !room(out, 12) {
                    return put_back(s);
                }
                death.queued = false;
                let cookie = death.cookie;
                if death.state == DeathState::Queued {
                    death.state = DeathState::Delivered;
                    out.extend_from_slice(&BR_DEAD_BINDER.to_ne_bytes());
                    out.extend_from_slice(&cookie.to_ne_bytes());
                    // One death notification per read, as the kernel.
                    return true;
                } else if death.cleared {
                    proc.deaths.remove(&d);
                    out.extend_from_slice(&BR_CLEAR_DEATH_NOTIFICATION_DONE.to_ne_bytes());
                    out.extend_from_slice(&cookie.to_ne_bytes());
                }
            }
            Work::Tx(id) => {
                let Some(tx) = s.txs.get(&id) else { continue };
                let secctx = !tx.is_reply
                    && tx
                        .node
                        .is_some_and(|n| s.nodes.get(&n).is_some_and(|n| n.secctx));
                let need = 4 + TR_SIZE + if secctx { 8 } else { 0 } + 4;
                if !room(out, need) {
                    return put_back(s);
                }
                let t = s.procs.get_mut(&p).unwrap().threads.get_mut(&tid).unwrap();
                if t.deferred_complete {
                    t.deferred_complete = false;
                    out.extend_from_slice(&BR_TRANSACTION_COMPLETE.to_ne_bytes());
                }
                let tx = s.txs.get_mut(&id).unwrap();
                let proc = s.procs.get_mut(&p).unwrap();
                let base = proc.buffer.as_ref().map_or(0, |m| m.guest_base);
                let (data_size, offsets_size, buf_off) = match tx.buffer {
                    Some(off) => {
                        let b = proc.buffers.get_mut(&off).unwrap();
                        b.user_free = true;
                        (b.data_size, b.offsets_size, off)
                    }
                    None => (0, 0, 0),
                };
                let (target, cookie) = match tx.node.and_then(|n| s.nodes.get(&n)) {
                    Some(n) if !tx.is_reply => (n.ptr, n.cookie),
                    _ => (0, 0),
                };
                let buffer = base + buf_off as u64;
                let tr = binder_transaction_data {
                    target,
                    cookie,
                    code: tx.code,
                    flags: tx.flags,
                    sender_pid: tx.sender_pid,
                    sender_euid: tx.sender_euid,
                    data_size: data_size as u64,
                    offsets_size: offsets_size as u64,
                    buffer,
                    offsets: buffer + align8(data_size).unwrap() as u64,
                };
                if tx.is_reply {
                    out.extend_from_slice(&BR_REPLY.to_ne_bytes());
                    push(out, &tr);
                } else if secctx {
                    out.extend_from_slice(&BR_TRANSACTION_SEC_CTX.to_ne_bytes());
                    // No security context: Andruna has no SELinux labels.
                    push(
                        out,
                        &binder_transaction_data_secctx {
                            transaction_data: tr,
                            secctx: 0,
                        },
                    );
                } else {
                    out.extend_from_slice(&BR_TRANSACTION.to_ne_bytes());
                    push(out, &tr);
                }
                let sync = !tx.is_reply && tx.flags & TF_ONE_WAY == 0;
                if sync {
                    let t = proc.threads.get_mut(&tid).unwrap();
                    tx.to_thread = Some(tid);
                    tx.to_parent = t.stack;
                    t.stack = Some(id);
                } else {
                    // Replies and one-way transactions end here; the
                    // buffer lives on until BC_FREE_BUFFER.
                    if let Some(off) = tx.buffer
                        && let Some(b) = proc.buffers.get_mut(&off)
                    {
                        b.tx = None;
                    }
                    s.txs.remove(&id);
                }
                return true;
            }
        }
    }
}

#[cfg(test)]
mod tests;

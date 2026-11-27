//! Broker tests: guest processes simulated in this process, their receive
//! buffers in plain memory, their threads as Rust threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use super::*;
use crate::pack;

#[derive(Debug, Default)]
struct Env {
    imported: AtomicU32,
}

impl ProcEnv for Env {
    fn import_handle(&self, _from: &dyn ProcEnv, handle: u32) -> Result<u32, i32> {
        if handle == 0 {
            return Err(EBADF);
        }
        self.imported.fetch_add(1, Ordering::SeqCst);
        Ok(handle + 1000)
    }
}

/// A simulated guest process.
struct P {
    b: Arc<Broker>,
    id: ProcId,
    env: Arc<Env>,
    _mem: Vec<u64>,
}

const BUF: usize = 64 * 1024;

fn proc(b: &Arc<Broker>, pid: i32, uid: u32) -> P {
    let env = Arc::new(Env::default());
    let id = b.add_proc(pid, uid, env.clone());
    let mut mem = vec![0u64; BUF / 8];
    let ptr = mem.as_mut_ptr().cast::<u8>();
    b.set_buffer(
        id,
        BufferMap {
            ptr,
            len: BUF,
            guest_base: ptr as u64,
        },
    )
    .unwrap();
    P {
        b: b.clone(),
        id,
        env,
        _mem: mem,
    }
}

/// A write stream under construction; keeps transaction data alive.
#[derive(Default)]
struct W {
    bytes: Vec<u8>,
    keep: Vec<Box<[u8]>>,
}

impl W {
    fn new() -> Self {
        Self::default()
    }
    fn cmd(mut self, c: u32) -> Self {
        self.bytes.extend_from_slice(&c.to_ne_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.bytes.extend_from_slice(&v.to_ne_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.bytes.extend_from_slice(&v.to_ne_bytes());
        self
    }
    fn handle_cookie(self, c: u32, h: u32, cookie: u64) -> Self {
        self.cmd(c).u32(h).u64(cookie)
    }
    fn ptr_cookie(self, c: u32, ptr: u64, cookie: u64) -> Self {
        self.cmd(c).u64(ptr).u64(cookie)
    }
    /// A transaction with `data` followed by `objs` (each 24 bytes).
    fn tx(
        mut self,
        cmd: u32,
        handle: u32,
        code: u32,
        flags: u32,
        data: &[u8],
        objs: &[flat_binder_object],
    ) -> Self {
        let mut d = data.to_vec();
        while !d.len().is_multiple_of(8) {
            d.push(0);
        }
        let mut offs = Vec::new();
        for o in objs {
            offs.extend_from_slice(&(d.len() as u64).to_ne_bytes());
            push(&mut d, o);
        }
        let d: Box<[u8]> = d.into();
        let offs: Box<[u8]> = offs.into();
        let tr = binder_transaction_data {
            target: u64::from(handle),
            cookie: 0,
            code,
            flags,
            sender_pid: 0,
            sender_euid: 0,
            data_size: d.len() as u64,
            offsets_size: offs.len() as u64,
            buffer: d.as_ptr() as u64,
            offsets: offs.as_ptr() as u64,
        };
        self = self.cmd(cmd);
        push(&mut self.bytes, &tr);
        self.keep.push(d);
        self.keep.push(offs);
        self
    }
}

fn binder(ptr: u64) -> flat_binder_object {
    flat_binder_object {
        hdr_type: BINDER_TYPE_BINDER,
        flags: FLAT_BINDER_FLAG_ACCEPTS_FDS,
        binder: ptr,
        cookie: ptr + 1,
    }
}

fn handle(h: u32) -> flat_binder_object {
    flat_binder_object {
        hdr_type: BINDER_TYPE_HANDLE,
        flags: 0,
        binder: u64::from(h),
        cookie: 0,
    }
}

fn fd_obj(fd: i32) -> flat_binder_object {
    flat_binder_object {
        hdr_type: BINDER_TYPE_FD,
        flags: 0,
        binder: u64::from(fd as u32),
        cookie: 0,
    }
}

/// A decoded return command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Br {
    Noop,
    Complete,
    Tx {
        tr: binder_transaction_data,
        data: Vec<u8>,
        objs: Vec<flat_binder_object>,
    },
    Reply {
        tr: binder_transaction_data,
        data: Vec<u8>,
        objs: Vec<flat_binder_object>,
    },
    Ref(u32, u64),
    Dead(u64),
    ClearDone(u64),
    Spawn,
    Err(u32),
}

fn decode(r: &[u8]) -> Vec<Br> {
    let mut v = Vec::new();
    let mut pos = 0;
    while pos < r.len() {
        let c: u32 = read_at(r, pos).unwrap();
        let a = pos + 4;
        pos = a + ioc_size(c);
        v.push(match c {
            BR_NOOP => Br::Noop,
            BR_TRANSACTION_COMPLETE => Br::Complete,
            BR_TRANSACTION | BR_TRANSACTION_SEC_CTX | BR_REPLY => {
                let tr: binder_transaction_data = read_at(r, a).unwrap();
                // SAFETY: the broker points into the test's buffer.
                let data = unsafe {
                    std::slice::from_raw_parts(tr.buffer as *const u8, tr.data_size as usize)
                }
                .to_vec();
                let mut objs = Vec::new();
                for i in 0..tr.offsets_size as usize / 8 {
                    // SAFETY: as above.
                    let o = unsafe { ((tr.offsets as usize + i * 8) as *const u64).read() };
                    objs.push(read_at(&data, o as usize).unwrap());
                }
                if c == BR_REPLY {
                    Br::Reply { tr, data, objs }
                } else {
                    Br::Tx { tr, data, objs }
                }
            }
            BR_INCREFS | BR_ACQUIRE | BR_RELEASE | BR_DECREFS => {
                Br::Ref(c, read_at::<u64>(r, a).unwrap())
            }
            BR_DEAD_BINDER => Br::Dead(read_at(r, a).unwrap()),
            BR_CLEAR_DEATH_NOTIFICATION_DONE => Br::ClearDone(read_at(r, a).unwrap()),
            BR_SPAWN_LOOPER => Br::Spawn,
            BR_DEAD_REPLY | BR_FAILED_REPLY => Br::Err(c),
            other => panic!("unexpected return {other:#x}"),
        });
    }
    v
}

impl P {
    /// One `BINDER_WRITE_READ` with a 256-byte read buffer (libbinder's).
    fn wr(&self, tid: i32, w: W) -> Vec<Br> {
        self.wr_cap(tid, w, 256)
    }

    fn wr_cap(&self, tid: i32, w: W, cap: usize) -> Vec<Br> {
        // SAFETY: the data pointers of `w` point into `w.keep`.
        let packed = unsafe { pack::pack(&w.bytes, &mut |fd| Some(fd as u32)) }.unwrap();
        let r = self
            .b
            .write_read(self.id, tid, &packed.bytes, cap, true, false)
            .unwrap();
        assert_eq!(packed.original_consumed(r.write_consumed), w.bytes.len());
        decode(&r.read)
    }

    /// Reads until something other than BR_NOOP arrives.
    fn read(&self, tid: i32) -> Vec<Br> {
        loop {
            let v: Vec<Br> = self
                .wr(tid, W::new())
                .into_iter()
                .filter(|b| *b != Br::Noop)
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
    }

    /// A thread that entered the looper (and consumed its first, empty read).
    fn looper(&self, tid: i32) {
        assert_eq!(self.wr_cap(tid, W::new().cmd(BC_ENTER_LOOPER), 0), vec![]);
    }
}

fn ctx_mgr(b: &Arc<Broker>) -> P {
    let sm = proc(b, 100, 1000);
    b.set_context_mgr(
        sm.id,
        Some(flat_binder_object {
            hdr_type: BINDER_TYPE_BINDER,
            flags: FLAT_BINDER_FLAG_TXN_SECURITY_CTX,
            binder: 0,
            cookie: 0,
        }),
    )
    .unwrap();
    sm
}

/// The kernel's protocol for a service registration: client 2 sends its
/// binder to the context manager, which gets a handle with the caller's
/// identity, takes its own reference, replies; the client is told to
/// hold its object.
#[test]
fn register_and_reply_with_identity() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = proc(&b, 200, 10_050);
    sm.looper(1);
    let t = {
        let a_id = a.id;
        let b2 = b.clone();
        let _ = a_id;
        let _ = b2;
        thread::spawn(move || {
            let r = a.wr(
                7,
                W::new().tx(
                    BC_TRANSACTION,
                    0,
                    3,
                    TF_ACCEPT_FDS,
                    b"addService",
                    &[binder(0x5000)],
                ),
            );
            (a, r)
        })
    };
    let r = sm.read(1);
    let Br::Tx { tr, data, objs } = &r[0] else {
        panic!("{r:?}")
    };
    assert_eq!(tr.code, 3);
    assert_eq!(tr.sender_pid, 200);
    assert_eq!(tr.sender_euid, 10_050);
    assert_eq!(&data[..10], b"addService");
    assert_eq!(objs[0].hdr_type, BINDER_TYPE_HANDLE);
    let h = objs[0].binder as u32;
    assert_eq!(h, 1);
    assert_eq!(b.ref_counts(sm.id, h), Some((1, 0)));
    let buf = tr.buffer;
    // The context manager takes its own strong reference and replies.
    let r2 = sm.wr(
        1,
        W::new()
            .cmd(BC_ACQUIRE)
            .u32(h)
            .cmd(BC_FREE_BUFFER)
            .u64(buf)
            .tx(BC_REPLY, 0, 0, 0, &0i32.to_ne_bytes(), &[]),
    );
    assert_eq!(r2, vec![Br::Noop, Br::Complete]);
    assert_eq!(b.ref_counts(sm.id, h), Some((1, 0)));
    let (a, r) = t.join().unwrap();
    // The client got the references to its object first (from the
    // translation), then, after its next read, the reply.
    let mut all = r;
    while !all.iter().any(|x| matches!(x, Br::Reply { .. })) {
        all.extend(a.read(7));
    }
    assert!(all.contains(&Br::Ref(BR_INCREFS, 0x5000)));
    assert!(all.contains(&Br::Ref(BR_ACQUIRE, 0x5000)));
    let i = all
        .iter()
        .position(|x| matches!(x, Br::Reply { .. }))
        .unwrap();
    assert_eq!(all[i - 1], Br::Complete);
    let Br::Reply { tr, data, .. } = &all[i] else {
        unreachable!()
    };
    assert_eq!(tr.sender_pid, 0);
    assert_eq!(&data[..4], &0i32.to_ne_bytes());
    let reply_buf = tr.buffer;
    a.wr_cap(
        7,
        W::new()
            .cmd(BC_FREE_BUFFER)
            .u64(reply_buf)
            .ptr_cookie(BC_INCREFS_DONE, 0x5000, 0x5001)
            .ptr_cookie(BC_ACQUIRE_DONE, 0x5000, 0x5001),
        0,
    );
    assert_eq!(b.stats().buffers, 0);
    assert_eq!(b.stats().transactions, 0);

    // The context manager drops its reference: the owner is told to
    // release, then to drop its weak reference.
    a.looper(8);
    sm.wr_cap(1, W::new().cmd(BC_RELEASE).u32(h), 0);
    let r = a.read(8);
    assert_eq!(
        r,
        vec![Br::Ref(BR_RELEASE, 0x5000), Br::Ref(BR_DECREFS, 0x5000)]
    );
    assert_eq!(b.stats().nodes, 1);
}

/// A call from A into B that calls back into A is served by A's waiting
/// thread (nested transactions), not by another thread of A.
#[test]
fn nested_call_goes_to_the_waiting_thread() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = Arc::new(proc(&b, 2, 1));
    let bp = proc(&b, 3, 2);
    let _ = &sm;
    // B gets a handle to A's object through a transaction from A to B,
    // which first needs A to hold a handle to B: B registers with the
    // context manager, which hands B's handle to A.
    sm.looper(1);
    bp.looper(1);
    a.looper(2); // another looper thread of A, which must stay idle
    let bt = thread::spawn(move || {
        let r = bp.wr(
            9,
            W::new().tx(BC_TRANSACTION, 0, 1, 0, b"", &[binder(0xb0)]),
        );
        (bp, r)
    });
    let r = sm.read(1);
    let Br::Tx { objs, tr, .. } = &r[0] else {
        panic!()
    };
    let hb = objs[0].binder as u32;
    // The context manager hands B's handle to A in a transaction from A.
    sm.wr(
        1,
        W::new()
            .cmd(BC_ACQUIRE)
            .u32(hb)
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer)
            .tx(BC_REPLY, 0, 0, 0, b"", &[]),
    );
    let (bp, _) = bt.join().unwrap();
    let a2 = a.clone();
    let at = thread::spawn(move || a2.wr(5, W::new().tx(BC_TRANSACTION, 0, 2, 0, b"", &[])));
    let r = sm.read(1);
    let Br::Tx { tr, .. } = &r[0] else { panic!() };
    sm.wr(
        1,
        W::new()
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer)
            .tx(BC_REPLY, 0, 0, 0, b"", &[handle(hb)]),
    );
    let mut r = at.join().unwrap();
    while !r.iter().any(|x| matches!(x, Br::Reply { .. })) {
        r.extend(a.read(5));
    }
    let Br::Reply { objs, tr, .. } = r.iter().find(|x| matches!(x, Br::Reply { .. })).unwrap()
    else {
        unreachable!()
    };
    let ha = objs[0].binder as u32;
    a.wr_cap(
        5,
        W::new()
            .cmd(BC_ACQUIRE)
            .u32(ha)
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer),
        0,
    );

    // A (thread 5) calls B with its own object; B calls back into A.
    let a2 = a.clone();
    let at = thread::spawn(move || {
        let mut r = a2.wr(
            5,
            W::new().tx(BC_TRANSACTION, ha, 10, 0, b"", &[binder(0xa0)]),
        );
        loop {
            if let Some(Br::Tx { tr, .. }) = r.iter().find(|x| matches!(x, Br::Tx { .. })) {
                // Served on the thread waiting for B's reply.
                assert_eq!(tr.code, 11);
                assert_eq!(tr.target, 0xa0);
                let buf = tr.buffer;
                r = a2.wr(
                    5,
                    W::new()
                        .cmd(BC_FREE_BUFFER)
                        .u64(buf)
                        .tx(BC_REPLY, 0, 0, 0, b"cb", &[]),
                );
                continue;
            }
            if let Some(Br::Reply { data, .. }) = r.iter().find(|x| matches!(x, Br::Reply { .. })) {
                return data.clone();
            }
            r = a2.read(5);
        }
    });
    let r = bp.read(1);
    let Br::Tx { tr, objs, .. } = &r[0] else {
        panic!("{r:?}")
    };
    assert_eq!(tr.code, 10);
    let h_a_obj = objs[0].binder as u32;
    let first = tr.buffer;
    let mut r = bp.wr(1, W::new().tx(BC_TRANSACTION, h_a_obj, 11, 0, b"", &[]));
    while !r.iter().any(|x| matches!(x, Br::Reply { .. })) {
        r.extend(bp.read(1));
    }
    let Br::Reply { data, tr, .. } = r.iter().find(|x| matches!(x, Br::Reply { .. })).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(&data[..2], b"cb");
    bp.wr(
        1,
        W::new()
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer)
            .cmd(BC_FREE_BUFFER)
            .u64(first)
            .tx(BC_REPLY, 0, 0, 0, b"done", &[]),
    );
    assert_eq!(&at.join().unwrap()[..4], b"done");
    // A's other looper thread never got a transaction.
    if let Ok(r) = b.write_read(a.id, 2, &[], 256, true, true) {
        assert!(decode(&r.read).iter().all(|x| !matches!(x, Br::Tx { .. })));
    }
}

/// One-way transactions to one node are delivered one at a time, in
/// order; the next after the previous buffer is freed.
#[test]
fn oneway_ordering_per_node() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = proc(&b, 2, 1);
    sm.looper(1);
    sm.looper(2);
    a.wr(
        5,
        W::new().tx(BC_TRANSACTION, 0, 1, TF_ONE_WAY, b"1", &[]).tx(
            BC_TRANSACTION,
            0,
            2,
            TF_ONE_WAY,
            b"2",
            &[],
        ),
    );
    let r = sm.read(1);
    let Br::Tx { tr, .. } = &r[0] else { panic!() };
    assert_eq!(tr.code, 1);
    assert_eq!(tr.sender_pid, 0, "one-way calls carry no pid");
    assert_eq!(tr.sender_euid, 1);
    // The second is held until the first buffer is freed.
    assert!(!b.has_work(sm.id, 2));
    let buf = tr.buffer;
    sm.wr_cap(1, W::new().cmd(BC_FREE_BUFFER).u64(buf), 0);
    let r = sm.read(2);
    let Br::Tx { tr, .. } = &r[0] else { panic!() };
    assert_eq!(tr.code, 2);
}

/// Death notifications: delivered when the owner dies, acknowledged,
/// cleared; and a client waiting for a reply from a dying server gets
/// BR_DEAD_REPLY.
#[test]
fn death_notification_and_dead_reply() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let srv = proc(&b, 3, 2);
    sm.looper(1);
    srv.looper(1);
    let t = thread::spawn(move || {
        let r = srv.wr(
            9,
            W::new().tx(BC_TRANSACTION, 0, 1, 0, b"", &[binder(0xb0)]),
        );
        (srv, r)
    });
    let r = sm.read(1);
    let Br::Tx { objs, tr, .. } = &r[0] else {
        panic!()
    };
    let h = objs[0].binder as u32;
    sm.wr(
        1,
        W::new()
            .cmd(BC_ACQUIRE)
            .u32(h)
            .handle_cookie(BC_REQUEST_DEATH_NOTIFICATION, h, 0xdead)
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer)
            .tx(BC_REPLY, 0, 0, 0, b"", &[]),
    );
    let (srv, _) = t.join().unwrap();

    // The context manager calls the server, which dies before replying.
    let srv_id = srv.id;
    let sm = Arc::new(sm);
    let sm2 = sm.clone();
    let call = thread::spawn(move || {
        let mut r = sm2.wr(1, W::new().tx(BC_TRANSACTION, h, 5, 0, b"", &[]));
        while !r.iter().any(|x| matches!(x, Br::Err(_))) {
            r.extend(sm2.read(1));
        }
        r
    });
    let r = srv.read(1);
    assert!(matches!(r[0], Br::Tx { .. }));
    b.proc_exit(srv_id);
    let r = call.join().unwrap();
    assert!(r.contains(&Br::Err(BR_DEAD_REPLY)), "{r:?}");
    // The death notice.
    sm.looper(2);
    let r = sm.read(2);
    assert_eq!(r, vec![Br::Dead(0xdead)]);
    // Calls to the dead node fail.
    let r = sm.wr(2, W::new().tx(BC_TRANSACTION, h, 5, 0, b"", &[]));
    assert!(r.contains(&Br::Err(BR_DEAD_REPLY)));
    // Clear, then acknowledge: the clear is confirmed after the
    // acknowledgement.
    let r = sm.wr(
        2,
        W::new()
            .handle_cookie(BC_CLEAR_DEATH_NOTIFICATION, h, 0xdead)
            .cmd(BC_DEAD_BINDER_DONE)
            .u64(0xdead),
    );
    assert_eq!(r, vec![Br::Noop, Br::ClearDone(0xdead)]);
    sm.wr_cap(2, W::new().cmd(BC_RELEASE).u32(h), 0);
    let s = b.stats();
    assert_eq!(s.refs, 0);
    assert_eq!(s.nodes, 1, "only the context manager node is left");
    assert_eq!(s.transactions, 0);
}

/// Clearing a death notification of a live node is confirmed at once.
#[test]
fn clear_death_of_live_node() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = proc(&b, 2, 1);
    a.looper(1);
    a.wr_cap(1, W::new().cmd(BC_ACQUIRE).u32(0), 0);
    let r = a.wr(
        1,
        W::new()
            .handle_cookie(BC_REQUEST_DEATH_NOTIFICATION, 0, 7)
            .handle_cookie(BC_CLEAR_DEATH_NOTIFICATION, 0, 7),
    );
    assert_eq!(r, vec![Br::Noop, Br::ClearDone(7)]);
    let _ = sm;
}

/// File descriptors are duplicated into the receiver only when it accepts
/// them; the receiver's environment imports the sender's handle.
#[test]
fn fd_passing() {
    let b = Broker::new();
    let sm = proc(&b, 100, 1000);
    b.set_context_mgr(
        sm.id,
        Some(flat_binder_object {
            flags: FLAT_BINDER_FLAG_ACCEPTS_FDS,
            ..Default::default()
        }),
    )
    .unwrap();
    let a = proc(&b, 2, 1);
    sm.looper(1);
    let t = thread::spawn(move || {
        let mut r = a.wr(5, W::new().tx(BC_TRANSACTION, 0, 1, 0, b"x", &[fd_obj(42)]));
        while !r.iter().any(|x| matches!(x, Br::Reply { .. } | Br::Err(_))) {
            r.extend(a.read(5));
        }
        r
    });
    let r = sm.read(1);
    let Br::Tx { objs, tr, .. } = &r[0] else {
        panic!()
    };
    assert_eq!(objs[0].hdr_type, BINDER_TYPE_FD);
    assert_eq!(objs[0].binder, 1042);
    assert_eq!(sm.env.imported.load(Ordering::SeqCst), 1);
    // The reply carries an fd, but the call did not set TF_ACCEPT_FDS:
    // the reply fails and the caller gets BR_FAILED_REPLY.
    let r = sm.wr(
        1,
        W::new()
            .cmd(BC_FREE_BUFFER)
            .u64(tr.buffer)
            .tx(BC_REPLY, 0, 0, 0, b"", &[fd_obj(3)]),
    );
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)));
    let r = t.join().unwrap();
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)), "{r:?}");
    assert_eq!(b.stats().transactions, 0);
    assert_eq!(b.stats().buffers, 0);
}

/// Invalid input fails the transaction, not the broker.
#[test]
fn invalid_transactions_fail() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = proc(&b, 2, 1);
    // Unknown handle.
    let r = a.wr(5, W::new().tx(BC_TRANSACTION, 9, 1, 0, b"", &[]));
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)));
    // Object past the end of the data.
    let mut w = W::new().tx(BC_TRANSACTION, 0, 1, 0, b"", &[handle(0)]);
    // Patch the offset to point past the data.
    let k = w.keep.len() - 1;
    w.keep[k][..8].copy_from_slice(&100u64.to_ne_bytes());
    let r = a.wr(5, w);
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)));
    // A handle the sender does not hold.
    let r = a.wr(5, W::new().tx(BC_TRANSACTION, 0, 1, 0, b"", &[handle(77)]));
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)));
    // A reply with nothing to reply to.
    let r = a.wr(5, W::new().tx(BC_REPLY, 0, 1, 0, b"", &[]));
    assert!(r.contains(&Br::Err(BR_FAILED_REPLY)));
    // An unknown command ends the call with EINVAL.
    let packed = W::new().cmd(BC_ENTER_LOOPER).cmd(0x1234_5678);
    assert_eq!(
        b.write_read(a.id, 5, &packed.bytes, 0, true, false)
            .unwrap_err(),
        (EINVAL, 4)
    );
    // A truncated stream: EFAULT.
    assert_eq!(
        b.write_read(a.id, 5, &BC_FREE_BUFFER.to_ne_bytes(), 0, true, false)
            .unwrap_err()
            .0,
        EFAULT
    );
    assert_eq!(b.stats().buffers, 0);
    let _ = sm;
}

/// A looper thread that finds no other waiting thread is asked to spawn
/// one, up to the maximum.
#[test]
fn spawn_looper() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    b.set_max_threads(sm.id, 1).unwrap();
    let r = sm.wr_cap(1, W::new().cmd(BC_ENTER_LOOPER), 256);
    assert_eq!(r, vec![Br::Noop, Br::Spawn]);
    let r = sm.wr_cap(2, W::new().cmd(BC_REGISTER_LOOPER), 256);
    // At the maximum: no more requests.
    assert_eq!(r, vec![Br::Noop]);
}

/// A blocked reader is woken by work, and a non-blocking read reports
/// EAGAIN.
#[test]
fn blocking_and_nonblocking_reads() {
    let b = Broker::new();
    let sm = Arc::new(ctx_mgr(&b));
    let a = proc(&b, 2, 1);
    sm.looper(1);
    assert_eq!(
        b.write_read(sm.id, 1, &[], 256, true, true).unwrap_err().0,
        EAGAIN
    );
    let sm2 = sm.clone();
    let t = thread::spawn(move || sm2.read(1));
    thread::sleep(Duration::from_millis(50));
    assert!(!t.is_finished());
    a.wr(5, W::new().tx(BC_TRANSACTION, 0, 4, TF_ONE_WAY, b"", &[]));
    let r = t.join().unwrap();
    assert!(matches!(r[0], Br::Tx { .. }));
}

/// Thread exit while serving a call fails the call; the process exit
/// cleans up everything.
#[test]
fn thread_exit_and_cleanup() {
    let b = Broker::new();
    let sm = ctx_mgr(&b);
    let a = proc(&b, 2, 1);
    sm.looper(1);
    let t = thread::spawn(move || {
        let mut r = a.wr(
            5,
            W::new().tx(BC_TRANSACTION, 0, 1, 0, b"x", &[binder(0x10)]),
        );
        while !r.iter().any(|x| matches!(x, Br::Err(_))) {
            r.extend(a.read(5));
        }
        (a, r)
    });
    let r = sm.read(1);
    assert!(matches!(r[0], Br::Tx { .. }));
    b.thread_exit(sm.id, 1);
    let (a, r) = t.join().unwrap();
    assert!(r.contains(&Br::Err(BR_DEAD_REPLY)));
    b.proc_exit(a.id);
    b.proc_exit(sm.id);
    assert_eq!(b.stats(), Stats::default());
}

/// Many concurrent clients calling the context manager's two looper
/// threads: every call gets its own reply.
#[test]
fn concurrent_calls() {
    concurrent(Duration::ZERO);
}

/// The same with broker threads spinning before they sleep.
#[test]
fn concurrent_calls_spinning() {
    concurrent(Duration::from_micros(30));
}

fn concurrent(spin: Duration) {
    let b = Broker::new();
    b.set_spin(spin);
    let sm = Arc::new(ctx_mgr(&b));
    let mut servers = Vec::new();
    for tid in 1..=2 {
        let sm = sm.clone();
        sm.looper(tid);
        servers.push(thread::spawn(move || {
            loop {
                let r = sm.read(tid);
                for x in r {
                    if let Br::Tx { tr, data, .. } = x {
                        if tr.code == 99 {
                            sm.wr_cap(tid, W::new().cmd(BC_FREE_BUFFER).u64(tr.buffer), 0);
                            return;
                        }
                        sm.wr_cap(
                            tid,
                            W::new().cmd(BC_FREE_BUFFER).u64(tr.buffer).tx(
                                BC_REPLY,
                                0,
                                0,
                                0,
                                &data,
                                &[],
                            ),
                            0,
                        );
                    }
                }
            }
        }));
    }
    let clients: Vec<_> = (0..4)
        .map(|c| {
            let a = proc(&b, 10 + c, 1);
            thread::spawn(move || {
                for i in 0..200u32 {
                    let msg = (c as u32 * 1000 + i).to_ne_bytes();
                    let mut r = a.wr(1, W::new().tx(BC_TRANSACTION, 0, 1, 0, &msg, &[]));
                    while !r.iter().any(|x| matches!(x, Br::Reply { .. })) {
                        r.extend(a.read(1));
                    }
                    let Some(Br::Reply { data, tr, .. }) =
                        r.into_iter().find(|x| matches!(x, Br::Reply { .. }))
                    else {
                        unreachable!()
                    };
                    assert_eq!(&data[..4], &msg);
                    a.wr_cap(1, W::new().cmd(BC_FREE_BUFFER).u64(tr.buffer), 0);
                }
                a
            })
        })
        .collect();
    let clients: Vec<P> = clients.into_iter().map(|t| t.join().unwrap()).collect();
    clients[0].wr(
        1,
        W::new().tx(BC_TRANSACTION, 0, 99, TF_ONE_WAY, b"", &[]).tx(
            BC_TRANSACTION,
            0,
            99,
            TF_ONE_WAY,
            b"",
            &[],
        ),
    );
    for s in servers {
        s.join().unwrap();
    }
    assert_eq!(b.stats().transactions, 0);
}

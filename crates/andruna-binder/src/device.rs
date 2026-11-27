//! `/dev/binder` in a guest process: an andruna-lx device that forwards
//! the binder ioctls to the broker in sysd (see [`crate::transport`]).
//!
//! The host process calls [`register`] before guest code runs.
//!
//! # Deviations from the kernel driver
//!
//! - The receive buffer is mapped writable in the guest (the device writes
//!   the received file descriptors into it); the kernel maps it read-only.
//!   The broker never reads it back, so a process can only corrupt what it
//!   itself received.
//! - A blocked `BINDER_WRITE_READ` is not interrupted by signals.
//! - File descriptors can be passed when they are regular files (their
//!   Windows handle is duplicated); other descriptors arrive as `-1` in the
//!   receiver and the transaction fails in the broker.

use std::any::Any;
use std::collections::HashMap;
use std::os::windows::io::OwnedHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use andruna_lx::errno::{Errno, SysResult};
use andruna_lx::fd::{FDS, FileOps, OpenFile};
use andruna_lx::fs::file::RegularFile;
use andruna_lx::linux::fcntl::O_RDWR;
use andruna_lx::linux::stat::{S_IFCHR, StatData};

use crate::sys;
use crate::transport::*;
use crate::uapi::*;
use crate::{pack, uapi};

/// Registers `/dev/binder` with andruna-lx.
pub fn register() {
    andruna_lx::fs::dev::register(b"binder", open);
}

fn open(_flags: u32) -> SysResult<Box<dyn FileOps>> {
    let name = std::env::var(PIPE_ENV).map_err(|_| Errno::ENOENT)?;
    let pipe = sys::open_pipe(&sys::wide(&name)).map_err(|_| Errno::ENOENT)?;
    let r = call(
        &pipe,
        Msg {
            op: op::HELLO as i32,
            a: VERSION,
            ..Msg::default()
        },
    )?;
    // SAFETY: the broker duplicated these handles into this process for
    // this connection.
    let (broker, poll_event) = unsafe { (sys::own(r.a), sys::own(r.b)) };
    Ok(Box::new(BinderFile {
        conn: Arc::new(Conn {
            pipe: Mutex::new(pipe),
            broker,
            poll_event: Arc::new(poll_event),
            buffer: Mutex::new(None),
            lanes: Mutex::new(HashMap::new()),
            poll_thread: AtomicBool::new(false),
        }),
    }))
}

/// One lane of this process (one per guest thread).
struct Lane {
    view: *mut u8,
    _section: OwnedHandle,
    req: OwnedHandle,
    resp: OwnedHandle,
}

impl Drop for Lane {
    fn drop(&mut self) {
        // SAFETY: the lane is no longer used.
        unsafe { sys::unmap_view(self.view) };
    }
}

// SAFETY: plain shared memory and kernel handles; a lane is used by one
// thread at a time (its guest thread).
unsafe impl Send for Lane {}
// SAFETY: as above.
unsafe impl Sync for Lane {}

struct Conn {
    pipe: Mutex<OwnedHandle>,
    broker: OwnedHandle,
    poll_event: Arc<OwnedHandle>,
    /// The guest view of the receive buffer and its length.
    buffer: Mutex<Option<(usize, usize, OwnedHandle)>>,
    lanes: Mutex<HashMap<i32, Arc<Lane>>>,
    poll_thread: AtomicBool,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("binder connection")
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some((base, _, _)) = self.buffer.get_mut().ok().and_then(|b| b.take()) {
            // SAFETY: the process closed its last descriptor of the device;
            // libbinder unmaps first, as Linux requires nothing here.
            unsafe { sys::unmap_view(base as *mut u8) };
        }
    }
}

/// One control request.
fn call(pipe: &OwnedHandle, m: Msg) -> SysResult<Msg> {
    let mut buf = [0u8; 64];
    let n = sys::transact(pipe, &m.to_bytes(), &mut buf).map_err(|_| Errno::EIO)?;
    let r = Msg::from_bytes(&buf[..n]).ok_or(Errno::EIO)?;
    if r.op < 0 {
        return Err(Errno(-r.op));
    }
    Ok(r)
}

impl Conn {
    fn call(&self, m: Msg) -> SysResult<Msg> {
        call(&self.pipe.lock().unwrap_or_else(|e| e.into_inner()), m)
    }

    fn lane(&self, tid: i32) -> SysResult<Arc<Lane>> {
        if let Some(l) = self
            .lanes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&tid)
        {
            return Ok(l.clone());
        }
        let r = self.call(Msg {
            op: op::THREAD as i32,
            a: tid as u64,
            ..Msg::default()
        })?;
        // SAFETY: duplicated into this process by the broker for this lane.
        let (section, req, resp) = unsafe { (sys::own(r.a), sys::own(r.b), sys::own(r.c)) };
        let view = sys::map_view(&section, LANE_SIZE).map_err(|_| Errno::ENOMEM)?;
        let l = Arc::new(Lane {
            view,
            _section: section,
            req,
            resp,
        });
        self.lanes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tid, l.clone());
        Ok(l)
    }

    fn broker_alive(&self) -> bool {
        !sys::wait(sys::raw(&self.broker), Some(0))
    }

    /// `BINDER_WRITE_READ`.
    fn write_read(&self, bwr_ptr: usize) -> SysResult<i64> {
        if bwr_ptr == 0 {
            return Err(Errno::EFAULT);
        }
        let bwr_p = bwr_ptr as *mut binder_write_read;
        // SAFETY: the caller's argument (as the kernel's copy_from_user; a
        // bad pointer faults in the caller's own process).
        let mut bwr = unsafe { bwr_p.read_unaligned() };
        let tid = andruna_lx::process::gettid();
        let lane = self.lane(tid)?;
        let packed = if bwr.write_size > bwr.write_consumed {
            let len = (bwr.write_size - bwr.write_consumed) as usize;
            // SAFETY: the caller's write buffer.
            let w = unsafe {
                std::slice::from_raw_parts(
                    (bwr.write_buffer + bwr.write_consumed) as usize as *const u8,
                    len,
                )
            };
            // SAFETY: the stream's transaction pointers are the caller's
            // own memory.
            match unsafe { pack::pack(w, &mut fd_handle) } {
                Ok(p) => p,
                Err((e, _)) => return Err(Errno(e)),
            }
        } else {
            pack::Packed::default()
        };
        if packed.bytes.len() > LANE_WRITE_MAX {
            return Err(Errno::EINVAL);
        }
        let read_cap =
            (bwr.read_size.saturating_sub(bwr.read_consumed) as usize).min(LANE_READ_MAX);
        let base = lane.view;
        // SAFETY: the lane holds the header and LANE_WRITE_MAX bytes of
        // write stream; the broker thread does not touch it until woken.
        unsafe {
            core::ptr::copy_nonoverlapping(
                packed.bytes.as_ptr(),
                base.add(LANE_WRITE),
                packed.bytes.len(),
            );
            base.cast::<LaneHeader>().write_volatile(LaneHeader {
                write_len: packed.bytes.len() as u64,
                read_cap: read_cap as u64,
                flags: if bwr.read_consumed == 0 {
                    LANE_FIRST_READ
                } else {
                    0
                },
                ..LaneHeader::default()
            });
        }
        let mut signal = true;
        loop {
            let done = if signal {
                sys::signal_and_wait(sys::raw(&lane.req), sys::raw(&lane.resp), 2000)
            } else {
                sys::wait(sys::raw(&lane.resp), Some(2000))
            };
            signal = false;
            if done {
                break;
            }
            if !self.broker_alive() {
                return Err(Errno::EIO);
            }
        }
        // SAFETY: the broker thread wrote the reply and is waiting again.
        let h = unsafe { base.cast::<LaneHeader>().read_volatile() };
        bwr.write_consumed += packed.original_consumed(h.write_consumed as usize) as u64;
        let rl = (h.read_len as usize).min(read_cap);
        if rl > 0 {
            let dst = (bwr.read_buffer + bwr.read_consumed) as usize as *mut u8;
            // SAFETY: the caller's read buffer has `read_cap` bytes left.
            let read = unsafe {
                core::ptr::copy_nonoverlapping(base.add(LANE_READ), dst, rl);
                std::slice::from_raw_parts(dst, rl)
            };
            // SAFETY: the stream points into this process's receive
            // buffer, writable here.
            unsafe { pack::install_fds(read, &mut install_fd) };
            bwr.read_consumed += rl as u64;
        }
        // SAFETY: the caller's argument.
        unsafe { bwr_p.write_unaligned(bwr) };
        if h.status < 0 {
            return Err(Errno(-h.status));
        }
        Ok(0)
    }

    fn mmap(&self, len: usize) -> SysResult<usize> {
        let mut b = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        if b.is_some() {
            return Err(Errno::EBUSY);
        }
        let r = self.call(Msg {
            op: op::MMAP as i32,
            a: len as u64,
            ..Msg::default()
        })?;
        // SAFETY: duplicated into this process by the broker.
        let section = unsafe { sys::own(r.a) };
        let view = sys::map_view(&section, len).map_err(|_| Errno::ENOMEM)?;
        self.call(Msg {
            op: op::MAPPED as i32,
            a: view as u64,
            b: len as u64,
            ..Msg::default()
        })?;
        *b = Some((view as usize, len, section));
        Ok(view as usize)
    }

    fn start_poll_thread(&self) {
        if self.poll_thread.swap(true, Ordering::SeqCst) {
            return;
        }
        let ev = self.poll_event.clone();
        let _ = std::thread::Builder::new()
            .name("binder-poll".into())
            .spawn(move || {
                while sys::wait(sys::raw(&*ev), None) {
                    andruna_lx::waitq::notify();
                }
            });
    }
}

/// The Windows handle of descriptor `fd`, for passing it in a transaction.
fn fd_handle(fd: i32) -> Option<u32> {
    let f = FDS.get(fd).ok()?;
    let any: &dyn Any = f.ops();
    let rf = any.downcast_ref::<RegularFile>()?;
    u32::try_from(rf.raw_handle().0 as usize).ok()
}

/// A descriptor for a handle the broker duplicated into this process.
fn install_fd(h: u32) -> i32 {
    if h == 0 {
        return -1;
    }
    let handle = windows::Win32::Foundation::HANDLE(h as usize as *mut _);
    // SAFETY: the broker duplicated the handle into this process for this
    // transaction; nothing else owns it.
    let rf = unsafe { RegularFile::from_raw_handle(handle) };
    let file = Arc::new(OpenFile::new(Box::new(rf), O_RDWR));
    FDS.insert(file, true, 0).unwrap_or(-1)
}

/// An open `/dev/binder`.
#[derive(Debug)]
pub struct BinderFile {
    conn: Arc<Conn>,
}

impl FileOps for BinderFile {
    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            dev: 5,
            ino: 0x4000,
            mode: S_IFCHR | 0o666,
            nlink: 1,
            // A misc device: major 10.
            rdev: (10 << 8) | 50,
            blksize: 4096,
            ..StatData::default()
        })
    }

    fn path(&self) -> Option<Vec<u8>> {
        Some(b"/dev/binder".to_vec())
    }

    fn poll(&self) -> u32 {
        use andruna_lx::linux::poll::*;
        self.conn.start_poll_thread();
        let tid = andruna_lx::process::gettid();
        let r = self.conn.call(Msg {
            op: op::HAS_WORK as i32,
            a: tid as u64,
            ..Msg::default()
        });
        match r {
            Ok(m) if m.a != 0 => POLLIN | POLLRDNORM,
            Ok(_) => 0,
            Err(_) => POLLERR,
        }
    }

    fn ioctl(&self, _file: &OpenFile, cmd: u32, arg: usize) -> SysResult<i64> {
        let c = &self.conn;
        let simple = |o: u32, a: u64| {
            c.call(Msg {
                op: o as i32,
                a,
                ..Msg::default()
            })
            .map(|_| 0)
        };
        // SAFETY (for the reads and writes of `arg` below): the caller's
        // argument of the size the command names, as the kernel's
        // copy_from_user / copy_to_user.
        match cmd {
            uapi::BINDER_WRITE_READ => c.write_read(arg),
            BINDER_VERSION => {
                if arg == 0 {
                    return Err(Errno::EFAULT);
                }
                // SAFETY: see above.
                unsafe { (arg as *mut i32).write_unaligned(BINDER_CURRENT_PROTOCOL_VERSION) };
                Ok(0)
            }
            BINDER_SET_MAX_THREADS => {
                // SAFETY: see above.
                let n = unsafe { (arg as *const u32).read_unaligned() };
                simple(op::MAX_THREADS, u64::from(n))
            }
            BINDER_SET_CONTEXT_MGR => c
                .call(Msg {
                    op: op::CONTEXT_MGR as i32,
                    ..Msg::default()
                })
                .map(|_| 0),
            BINDER_SET_CONTEXT_MGR_EXT => {
                // SAFETY: see above.
                let o = unsafe { (arg as *const flat_binder_object).read_unaligned() };
                c.call(Msg {
                    op: op::CONTEXT_MGR as i32,
                    flags: o.flags,
                    a: o.binder,
                    b: o.cookie,
                    c: 1,
                })
                .map(|_| 0)
            }
            BINDER_THREAD_EXIT => {
                let tid = andruna_lx::process::gettid();
                c.lanes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&tid);
                simple(op::THREAD_EXIT, tid as u64)
            }
            BINDER_ENABLE_ONEWAY_SPAM_DETECTION => Ok(0),
            BINDER_GET_EXTENDED_ERROR => {
                // SAFETY: see above.
                unsafe {
                    (arg as *mut binder_extended_error)
                        .write_unaligned(binder_extended_error::default())
                };
                Ok(0)
            }
            _ => Err(Errno::EINVAL),
        }
    }

    fn device_mmap(
        &self,
        _file: &OpenFile,
        _addr: usize,
        len: usize,
        _prot: u32,
        _flags: u32,
        offset: i64,
    ) -> Option<SysResult<usize>> {
        if offset != 0 {
            return Some(Err(Errno::EINVAL));
        }
        Some(self.conn.mmap(len))
    }
}

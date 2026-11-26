//! Shared-memory channel: a header page plus single-producer/single-consumer
//! byte rings, each with a "consumer is sleeping" flag and an auto-reset
//! event. The wake protocol is a Dekker-style handshake:
//!
//! producer: publish tail (SeqCst); if `waiting` is set, clear it and SetEvent.
//! consumer: spin for the configured budget; then set `waiting` (SeqCst),
//!           re-check tail, and only then block on the event.
//!
//! A stale signal (producer clears `waiting` after the consumer already saw
//! data) only causes one spurious wakeup, which the loop absorbs.

use std::sync::atomic::{fence, AtomicI64, AtomicU32, Ordering::*};
use std::time::{Duration, Instant};

/// A ring counts as hot (eligible for spinning) this long after its last message.
pub const HOT_WINDOW: Duration = Duration::from_millis(2);

use crate::win::{self, Raw};

pub const PAGE: usize = 4096;
pub const RING_CAP: usize = 256 * 1024;
pub const RING_STRIDE: usize = PAGE + RING_CAP;
pub const MAX_PAYLOAD: usize = 64 * 1024 + 256;

pub const fn section_size(rings: usize) -> usize {
    PAGE + rings * RING_STRIDE
}

// Message kinds.
pub const K_TXN: u32 = 1;
pub const K_REPLY: u32 = 2;
pub const K_PAD: u32 = 3;
pub const K_DEATH: u32 = 4;

// Flags.
pub const F_ONEWAY: u32 = 1;
pub const F_HANDLE: u32 = 2;

// Codes.
pub const C_ECHO: u32 = 1;
pub const C_CONFIG: u32 = 2;
pub const C_CPU: u32 = 3;
pub const C_LINK_DEATH: u32 = 4;
pub const C_PAIR: u32 = 5;
pub const C_REPORT: u32 = 6;
pub const C_FD_READ: u32 = 10;
pub const C_FD_CLOSE: u32 = 11;
pub const C_PAIR_OFFER: u32 = 12;

/// Wire header of one message in a ring. 64 bytes, so records stay
/// cache-line aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgHdr {
    pub kind: u32,
    pub flags: u32,
    pub len: u32,
    pub code: u32,
    /// On send: destination client id (0 = broker). On delivery: sender id.
    pub peer: u32,
    /// Filled by the broker on delivery; never trusted from the sender.
    pub sender_pid: u32,
    pub sender_uid: u32,
    pub _r: u32,
    pub txn: u64,
    pub handle: u64,
    pub arg: u64,
    pub arg2: u64,
}
const _: () = assert!(std::mem::size_of::<MsgHdr>() == 64);

#[repr(C, align(64))]
pub struct RingCtl {
    pub tail: AtomicU32,
    _p0: [u8; 60],
    pub head: AtomicU32,
    pub waiting: AtomicU32,
    pub closed: AtomicU32,
    _p1: [u8; 52],
}

/// Benchmark configuration shared through each client's header page. The
/// broker writes it; every thread re-reads it when `gen` changes.
#[repr(C, align(64))]
pub struct SharedCfg {
    pub gen: AtomicU32,
    pub spin_ns: AtomicU32,
    pub soaw: AtomicU32,
    pub pinned: AtomicU32,
}

#[repr(C)]
pub struct ChanHdr {
    pub magic: u32,
    pub id: u32,
    pub rings: u32,
    pub _r: u32,
    /// Raw QPC value written by the client right before it exits.
    pub exit_qpc: AtomicI64,
    _p: [u8; 40],
    pub cfg: SharedCfg,
}
const _: () = assert!(std::mem::size_of::<ChanHdr>() <= PAGE);

pub const MAGIC: u32 = 0x5333_4244; // "S3BD"

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cfg {
    pub gen: u32,
    pub spin_ns: u32,
    pub soaw: bool,
    pub pinned: bool,
}

impl SharedCfg {
    pub fn store(&self, c: &Cfg) {
        self.spin_ns.store(c.spin_ns, Relaxed);
        self.soaw.store(c.soaw as u32, Relaxed);
        self.pinned.store(c.pinned as u32, Relaxed);
        self.gen.store(c.gen, Release);
    }
    /// Refreshes `local` if the generation changed; applies pinning for
    /// the calling thread (`cpu` is its designated CPU). Returns true if changed.
    pub fn refresh(&self, local: &mut Cfg, cpu: u32) -> bool {
        let g = self.gen.load(Acquire);
        if g == local.gen {
            return false;
        }
        local.gen = g;
        local.spin_ns = self.spin_ns.load(Relaxed);
        local.soaw = self.soaw.load(Relaxed) != 0;
        local.pinned = self.pinned.load(Relaxed) != 0;
        win::pin_current_thread(if local.pinned { Some(cpu) } else { None });
        true
    }
}

/// A mapped channel section (client channel or pair channel).
pub struct Channel {
    pub sec: win::Section,
}

impl Channel {
    pub fn hdr(&self) -> &ChanHdr {
        // SAFETY: the section starts with a ChanHdr page and lives as long as self.
        unsafe { &*(self.sec.base as *const ChanHdr) }
    }
    /// View of ring `i`; `ev` is the consumer's event as valid in this process.
    pub fn ring(&self, i: usize, ev: Raw) -> Ring {
        assert!(section_size(i + 1) <= self.sec.size);
        // SAFETY: offsets are inside the mapped section (checked above).
        unsafe {
            let base = self.sec.base.add(PAGE + i * RING_STRIDE);
            Ring {
                ctl: base as *const RingCtl,
                data: base.add(PAGE),
                ev,
            }
        }
    }
}

/// Process-local view of one ring.
#[derive(Clone, Copy)]
pub struct Ring {
    ctl: *const RingCtl,
    data: *mut u8,
    pub ev: Raw,
}

// SAFETY: all cross-thread state is in atomics in shared memory; the data
// area is accessed under the SPSC protocol (callers serialize producers).
unsafe impl Send for Ring {}
// SAFETY: as above.
unsafe impl Sync for Ring {}

/// A message seen by the consumer, still in the ring until `consume`.
pub struct Msg {
    pub hdr: MsgHdr,
    payload: *const u8,
    rec: u32,
}

impl Msg {
    /// Copies the payload out (the sender may still scribble on shared
    /// memory, so the receiver must work on its own copy).
    pub fn copy_payload(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(self.hdr.len as usize);
        // SAFETY: `len` was validated against the ring bounds in `peek`.
        unsafe {
            std::ptr::copy_nonoverlapping(self.payload, out.as_mut_ptr(), self.hdr.len as usize);
            out.set_len(self.hdr.len as usize);
        }
    }
    pub fn payload_ptr(&self) -> *const u8 {
        self.payload
    }
}

#[inline]
fn rec_size(len: usize) -> usize {
    64 + len.div_ceil(64) * 64
}

impl Ring {
    fn ctl(&self) -> &RingCtl {
        // SAFETY: points into a mapped section that outlives every Ring copy.
        unsafe { &*self.ctl }
    }

    pub fn close(&self) {
        self.ctl().closed.store(1, SeqCst);
        win::set_event(self.ev);
    }
    pub fn is_closed(&self) -> bool {
        self.ctl().closed.load(Relaxed) != 0
    }

    /// Pushes one message. `fill` writes exactly `hdr.len` payload bytes.
    /// Returns the consumer event to signal if the consumer was sleeping.
    /// Producers of the same ring must be serialized by the caller.
    pub fn push(&self, hdr: &MsgHdr, fill: impl FnOnce(*mut u8)) -> Option<Raw> {
        let c = self.ctl();
        let len = hdr.len as usize;
        assert!(len <= MAX_PAYLOAD);
        let need = rec_size(len);
        let mut tail = c.tail.load(Relaxed);
        let mut spins = 0u32;
        loop {
            let pos = tail as usize % RING_CAP;
            let contig = RING_CAP - pos;
            let total = if need > contig { contig + need } else { need };
            let used = tail.wrapping_sub(c.head.load(Acquire)) as usize;
            if RING_CAP - used >= total {
                if need > contig {
                    // Pad to the end of the ring and start at offset 0.
                    let pad = MsgHdr {
                        kind: K_PAD,
                        len: (contig - 64) as u32,
                        ..Default::default()
                    };
                    // SAFETY: `pos` is 64-aligned and `contig >= 64` bytes remain.
                    unsafe { std::ptr::write_volatile(self.data.add(pos) as *mut MsgHdr, pad) };
                    tail = tail.wrapping_add(contig as u32);
                }
                break;
            }
            if c.closed.load(Relaxed) != 0 {
                return None;
            }
            // Ring full: the consumer is behind (oneway floods). Back off.
            spins += 1;
            if spins < 1000 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        let pos = tail as usize % RING_CAP;
        // SAFETY: [pos, pos+need) is free space owned by the producer.
        unsafe {
            fill(self.data.add(pos + 64));
            std::ptr::write_volatile(self.data.add(pos) as *mut MsgHdr, *hdr);
        }
        c.tail.store(tail.wrapping_add(need as u32), SeqCst);
        if c.waiting.load(SeqCst) != 0 && c.waiting.swap(0, SeqCst) == 1 {
            Some(self.ev)
        } else {
            None
        }
    }

    #[inline]
    fn nonempty(&self) -> bool {
        let c = self.ctl();
        c.tail.load(Acquire) != c.head.load(Relaxed)
    }

    /// Returns the next message if any. Skips padding. A malformed record
    /// closes the ring (the producer is not trusted).
    pub fn peek(&self) -> Option<Msg> {
        let c = self.ctl();
        loop {
            let head = c.head.load(Relaxed);
            let tail = c.tail.load(Acquire);
            if head == tail {
                return None;
            }
            let avail = tail.wrapping_sub(head) as usize;
            let pos = head as usize % RING_CAP;
            // SAFETY: `pos` is inside the data area; the header is copied out.
            let hdr = unsafe { std::ptr::read_volatile(self.data.add(pos) as *const MsgHdr) };
            let rec = rec_size(hdr.len as usize);
            if hdr.len as usize > RING_CAP
                || rec > avail
                || pos + rec > RING_CAP
                || avail > RING_CAP
            {
                c.closed.store(1, SeqCst);
                return None;
            }
            if hdr.kind == K_PAD {
                c.head.store(head.wrapping_add(rec as u32), Release);
                continue;
            }
            if hdr.len as usize > MAX_PAYLOAD {
                c.closed.store(1, SeqCst);
                return None;
            }
            return Some(Msg {
                hdr,
                // SAFETY: inside the data area (checked above).
                payload: unsafe { self.data.add(pos + 64) },
                rec: rec as u32,
            });
        }
    }

    pub fn consume(&self, m: &Msg) {
        let c = self.ctl();
        let head = c.head.load(Relaxed);
        c.head.store(head.wrapping_add(m.rec), Release);
    }

    /// Waits for the next message: spin for `cfg.spin_ns`, then sleep on the
    /// event. `pending` is a wake owed to a peer; with `cfg.soaw` and no spin
    /// it is combined with the wait in one SignalObjectAndWait call.
    /// Returns None when the ring is closed.
    ///
    /// Spinning only happens while the ring is "hot": `hot` holds the time of
    /// the last message, and no thread spins once `HOT_WINDOW` has passed
    /// without traffic. Idle threads therefore always sleep in the kernel.
    pub fn wait_msg(&self, cfg: &Cfg, pending: &mut Option<Raw>, hot: &mut Instant) -> Option<Msg> {
        let c = self.ctl();
        loop {
            if let Some(m) = self.peek() {
                flush(pending);
                if cfg.spin_ns > 0 {
                    *hot = Instant::now();
                }
                return Some(m);
            }
            if c.closed.load(Relaxed) != 0 {
                flush(pending);
                return None;
            }
            if cfg.spin_ns > 0 && hot.elapsed() < HOT_WINDOW {
                flush(pending);
                if self.spin(cfg.spin_ns, *hot) {
                    continue;
                }
            }
            c.waiting.store(1, SeqCst);
            fence(SeqCst);
            if self.nonempty() || c.closed.load(SeqCst) != 0 {
                c.waiting.swap(0, SeqCst);
                continue;
            }
            match pending.take() {
                Some(p) if cfg.soaw => {
                    win::signal_and_wait(p, self.ev);
                }
                Some(p) => {
                    win::set_event(p);
                    win::wait_infinite(self.ev);
                }
                None => {
                    win::wait_infinite(self.ev);
                }
            }
            // Normally the producer already cleared the flag when it woke us;
            // after a stale (spurious) wakeup we clear it ourselves.
            c.waiting.store(0, SeqCst);
        }
    }

    /// Spins until data arrives (true) or the budget runs out (false).
    /// `u32::MAX` means no per-wait budget: spin until the hot window ends.
    fn spin(&self, ns: u32, hot: Instant) -> bool {
        let start = Instant::now();
        let cold_at = hot + HOT_WINDOW;
        let deadline = if ns == u32::MAX {
            cold_at
        } else {
            (start + Duration::from_nanos(ns as u64)).min(cold_at)
        };
        loop {
            for _ in 0..32 {
                if self.nonempty() {
                    return true;
                }
                std::hint::spin_loop();
            }
            if self.is_closed() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }
}

fn flush(pending: &mut Option<Raw>) {
    if let Some(p) = pending.take() {
        win::set_event(p);
    }
}

/// Raw QueryPerformanceCounter value (system-wide, comparable across processes).
pub fn qpc() -> i64 {
    let mut v = 0i64;
    // SAFETY: valid out pointer.
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut v);
    }
    v
}

pub fn qpc_freq() -> i64 {
    let mut v = 0i64;
    // SAFETY: valid out pointer.
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut v);
    }
    v
}

pub fn qpc_to_us(d: i64) -> f64 {
    d as f64 * 1e6 / qpc_freq() as f64
}

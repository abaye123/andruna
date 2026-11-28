//! The input channel between the compositor and an app window: a pair of
//! single-producer, single-consumer byte rings in one shared section, and
//! one event per direction for wakeups.
//!
//! This stands in for Android's `InputChannel` (a socket pair) until
//! Binder hands out real ones. It carries [`wire`](crate::wire) messages:
//! events from the compositor, and "finished" replies from the app, which
//! the compositor counts to know whether the app keeps up (Android's
//! input dispatcher does the same for responsiveness). Replacing it means
//! replacing this module and [`wire`]; the producers of events do not
//! change.
//!
//! The section and the two events are duplicated into the app process
//! ([`ChannelHandles`]); the app opens its end with
//! [`InputChannel::open`]. The app is not trusted: every length read from
//! the shared memory is checked, and a corrupted ring is an error, not a
//! panic.
//!
//! Section layout: the write and read positions of ring 0 (compositor to
//! app) at offsets 64 and 128, of ring 1 (app to compositor) at 192 and
//! 256 (separate cache lines), ring 0's data from 4096, ring 1's after it.

use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::sys::shm::{Event, EventWatch, Handle, Section};
use crate::wire::{self, DecodeError, Message, SurfaceMessage};

const DATA_OFFSET: usize = 4096;
const MAGIC: u64 = 0x414e_4452_494e_5031; // "ANDRINP1"

/// The default capacity of each direction: about 400 motion events.
pub const DEFAULT_CAPACITY: usize = 64 * 1024;

#[derive(Debug)]
pub enum ChannelError {
    /// No room: the reader is behind (the event is not sent).
    Full,
    /// The message is larger than half the ring.
    TooLarge,
    /// The peer wrote inconsistent positions or lengths.
    Corrupted,
    Decode(DecodeError),
    Io(io::Error),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Full => write!(f, "input channel full"),
            ChannelError::TooLarge => write!(f, "message too large for the input channel"),
            ChannelError::Corrupted => write!(f, "input channel corrupted by the peer"),
            ChannelError::Decode(e) => write!(f, "{e}"),
            ChannelError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<io::Error> for ChannelError {
    fn from(e: io::Error) -> Self {
        ChannelError::Io(e)
    }
}

/// The side of the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Compositor,
    App,
}

/// Handle values of a channel end in a target process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelHandles {
    pub section: usize,
    pub capacity: usize,
    /// Signalled when ring 0 (compositor to app) has data.
    pub to_app: usize,
    /// Signalled when ring 1 (app to compositor) has data.
    pub to_compositor: usize,
}

impl ChannelHandles {
    /// The command line form, `section:capacity:to_app:to_compositor`
    /// (hexadecimal handle values), as an app process receives it.
    pub fn to_arg(&self) -> String {
        format!(
            "{:x}:{:x}:{:x}:{:x}",
            self.section, self.capacity, self.to_app, self.to_compositor
        )
    }

    /// Parses [`ChannelHandles::to_arg`].
    pub fn parse(s: &str) -> Option<ChannelHandles> {
        let mut it = s.split(':').map(|p| usize::from_str_radix(p, 16).ok());
        let h = ChannelHandles {
            section: it.next()??,
            capacity: it.next()??,
            to_app: it.next()??,
            to_compositor: it.next()??,
        };
        it.next().is_none().then_some(h)
    }
}

/// Inheritable copies of a channel's handles for a child process started
/// with them in its launch handle list (handle values are the same in the
/// child). Closed in this process on drop, after the child has started.
#[derive(Debug)]
pub struct InheritedChannel {
    _handles: [Handle; 3],
    pub values: ChannelHandles,
}

/// One end of an input channel.
#[derive(Debug)]
pub struct InputChannel {
    section: Section,
    capacity: usize,
    side: Side,
    to_app: Event,
    to_compositor: Event,
}

struct Ring<'a> {
    s: &'a Section,
    write_at: usize,
    read_at: usize,
    data: usize,
    cap: usize,
}

impl Ring<'_> {
    fn write(&self, msg: &[u8]) -> Result<(), ChannelError> {
        if msg.len() > self.cap / 2 {
            return Err(ChannelError::TooLarge);
        }
        let w = self.s.word(self.write_at).load(Ordering::Relaxed);
        let r = self.s.word(self.read_at).load(Ordering::Acquire);
        let used = w.wrapping_sub(r);
        if used > self.cap as u64 {
            return Err(ChannelError::Corrupted);
        }
        if self.cap as u64 - used < msg.len() as u64 {
            return Err(ChannelError::Full);
        }
        let bytes = self.s.bytes();
        for (i, b) in msg.iter().enumerate() {
            let at = self.data + ((w as usize + i) % self.cap);
            bytes[at].store(*b, Ordering::Relaxed);
        }
        self.s
            .word(self.write_at)
            .store(w + msg.len() as u64, Ordering::Release);
        Ok(())
    }

    fn read(&self) -> Result<Option<Vec<u8>>, ChannelError> {
        let r = self.s.word(self.read_at).load(Ordering::Relaxed);
        let w = self.s.word(self.write_at).load(Ordering::Acquire);
        let avail = w.wrapping_sub(r);
        if avail == 0 {
            return Ok(None);
        }
        if avail > self.cap as u64 || avail < wire::HEADER_LEN as u64 {
            return Err(ChannelError::Corrupted);
        }
        let bytes = self.s.bytes();
        let at =
            |i: usize| bytes[self.data + ((r as usize + i) % self.cap)].load(Ordering::Relaxed);
        let len = u32::from_le_bytes([at(0), at(1), at(2), at(3)]) as usize;
        if len < wire::HEADER_LEN || len as u64 > avail || len > self.cap / 2 {
            return Err(ChannelError::Corrupted);
        }
        let msg = (0..len).map(at).collect();
        self.s
            .word(self.read_at)
            .store(r + len as u64, Ordering::Release);
        Ok(Some(msg))
    }
}

impl InputChannel {
    /// Creates a channel and returns both ends in this process. The app's
    /// end is usually exported with [`InputChannel::export`] instead of
    /// being used here; tests and in-process hosts use it directly.
    pub fn pair(capacity: usize) -> Result<(InputChannel, InputChannel), ChannelError> {
        let capacity = capacity.next_power_of_two().max(1024);
        let section = Section::create(DATA_OFFSET + 2 * capacity)?;
        section.word(0).store(MAGIC, Ordering::Relaxed);
        section.word(8).store(capacity as u64, Ordering::Release);
        let to_app = Event::create()?;
        let to_compositor = Event::create()?;
        let app_handles = ChannelHandles {
            section: section.handle().duplicate_into(None)?,
            capacity,
            to_app: to_app.handle().duplicate_into(None)?,
            to_compositor: to_compositor.handle().duplicate_into(None)?,
        };
        let compositor = InputChannel {
            section,
            capacity,
            side: Side::Compositor,
            to_app,
            to_compositor,
        };
        // SAFETY: the handles were just duplicated for this process and are
        // owned by nobody else.
        let app = unsafe { InputChannel::open(app_handles) }?;
        Ok((compositor, app))
    }

    /// Duplicates this channel's handles into `process` (a handle with
    /// `PROCESS_DUP_HANDLE`) for the app to open its end.
    pub fn export(&self, process: usize) -> io::Result<ChannelHandles> {
        Ok(ChannelHandles {
            section: self.section.handle().duplicate_into(Some(process))?,
            capacity: self.capacity,
            to_app: self.to_app.handle().duplicate_into(Some(process))?,
            to_compositor: self.to_compositor.handle().duplicate_into(Some(process))?,
        })
    }

    /// Inheritable copies of the handles, for an app process that this
    /// process starts (the app opens them with [`InputChannel::open`]).
    pub fn export_inheritable(&self) -> io::Result<InheritedChannel> {
        let section = self.section.handle().duplicate_inheritable()?;
        let to_app = self.to_app.handle().duplicate_inheritable()?;
        let to_compositor = self.to_compositor.handle().duplicate_inheritable()?;
        let values = ChannelHandles {
            section: section.raw(),
            capacity: self.capacity,
            to_app: to_app.raw(),
            to_compositor: to_compositor.raw(),
        };
        Ok(InheritedChannel {
            _handles: [section, to_app, to_compositor],
            values,
        })
    }

    /// Posts `msg` to `thread` whenever the peer sends something.
    pub fn watch(&self, thread: u32, msg: u32) -> io::Result<EventWatch> {
        EventWatch::new(self.rx().1, thread, msg)
    }

    /// Opens the app's end from handles duplicated into this process.
    ///
    /// # Safety
    /// The handle values must be valid handles of this process, owned by
    /// the caller (they are closed when the channel is dropped).
    pub unsafe fn open(h: ChannelHandles) -> Result<InputChannel, ChannelError> {
        // SAFETY: the caller passes owned, valid handles.
        let (section, to_app, to_compositor) = unsafe {
            (
                Handle::from_raw(h.section),
                Handle::from_raw(h.to_app),
                Handle::from_raw(h.to_compositor),
            )
        };
        if !h.capacity.is_power_of_two() {
            return Err(ChannelError::Corrupted);
        }
        let section = Section::map(section, DATA_OFFSET + 2 * h.capacity)?;
        if section.word(0).load(Ordering::Relaxed) != MAGIC
            || section.word(8).load(Ordering::Acquire) != h.capacity as u64
        {
            return Err(ChannelError::Corrupted);
        }
        Ok(InputChannel {
            section,
            capacity: h.capacity,
            side: Side::App,
            to_app: Event::from_handle(to_app),
            to_compositor: Event::from_handle(to_compositor),
        })
    }

    fn ring(&self, index: usize) -> Ring<'_> {
        Ring {
            s: &self.section,
            write_at: 64 + index * 128,
            read_at: 128 + index * 128,
            data: DATA_OFFSET + index * self.capacity,
            cap: self.capacity,
        }
    }

    fn tx(&self) -> (Ring<'_>, &Event) {
        match self.side {
            Side::Compositor => (self.ring(0), &self.to_app),
            Side::App => (self.ring(1), &self.to_compositor),
        }
    }

    fn rx(&self) -> (Ring<'_>, &Event) {
        match self.side {
            Side::Compositor => (self.ring(1), &self.to_compositor),
            Side::App => (self.ring(0), &self.to_app),
        }
    }

    pub fn side(&self) -> Side {
        self.side
    }

    /// Sends a message and wakes the peer.
    pub fn send(&self, msg: &Message) -> Result<(), ChannelError> {
        let (ring, ev) = self.tx();
        ring.write(&wire::encode(msg))?;
        ev.set();
        Ok(())
    }

    /// The next message, if one is waiting.
    pub fn try_recv(&self) -> Result<Option<Message>, ChannelError> {
        let (ring, _) = self.rx();
        match ring.read()? {
            Some(b) => wire::decode(&b).map(Some).map_err(ChannelError::Decode),
            None => Ok(None),
        }
    }

    /// The next message, waiting up to `timeout` for one.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<Message>, ChannelError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(m) = self.try_recv()? {
                return Ok(Some(m));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            self.rx().1.wait(deadline - now);
        }
    }

    /// The raw handle of the event signalled when this end has data, for
    /// a caller's own wait loop (the app's Looper).
    pub fn readable_event(&self) -> usize {
        self.rx().1.handle().raw()
    }
}

/// Sends input events with sequence numbers and tracks the app's finished
/// replies, as Android's input publisher and dispatcher do.
#[derive(Debug)]
pub struct InputPublisher {
    channel: InputChannel,
    next_seq: u32,
    /// Sequence numbers sent and not finished, oldest first, with send
    /// times.
    pending: std::collections::VecDeque<(u32, i64)>,
}

/// A finished reply, with the time the event waited for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finished {
    pub seq: u32,
    pub handled: bool,
    /// Send to finished, in nanoseconds on the uptime clock.
    pub latency_ns: i64,
}

impl InputPublisher {
    pub fn new(channel: InputChannel) -> Self {
        Self {
            channel,
            next_seq: 1,
            pending: Default::default(),
        }
    }

    /// Sends an event; returns its sequence number.
    pub fn publish(
        &mut self,
        event: crate::input::InputEvent,
        now_ns: i64,
    ) -> Result<u32, ChannelError> {
        let seq = self.next_seq;
        self.channel.send(&Message::Event { seq, event })?;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.pending.push_back((seq, now_ns));
        Ok(seq)
    }

    /// Reads the app's finished replies (surface messages are dropped; the
    /// compositor reads both with [`InputPublisher::poll`]).
    pub fn poll_finished(&mut self, now_ns: i64) -> Result<Vec<Finished>, ChannelError> {
        Ok(self.poll(now_ns)?.0)
    }

    /// Reads everything the app sent: finished replies and surface
    /// messages, in order.
    pub fn poll(
        &mut self,
        now_ns: i64,
    ) -> Result<(Vec<Finished>, Vec<SurfaceMessage>), ChannelError> {
        let mut out = Vec::new();
        let mut surface = Vec::new();
        while let Some(m) = self.channel.try_recv()? {
            match m {
                Message::Finished { seq, handled, .. } => {
                    if let Some(i) = self.pending.iter().position(|p| p.0 == seq) {
                        let (_, sent) = self.pending.remove(i).unwrap();
                        out.push(Finished {
                            seq,
                            handled,
                            latency_ns: now_ns - sent,
                        });
                    }
                }
                Message::Surface(s) => surface.push(s),
                // Events go the other way; an app that sends them is
                // ignored.
                Message::Event { .. } => {}
            }
        }
        Ok((out, surface))
    }

    /// Sends a surface message to the app.
    pub fn send_surface(&self, msg: SurfaceMessage) -> Result<(), ChannelError> {
        self.channel.send(&Message::Surface(msg))
    }

    /// How long the oldest unfinished event has waited (Android declares
    /// an app not responding after 5 s).
    pub fn oldest_pending_ns(&self, now_ns: i64) -> Option<i64> {
        self.pending.front().map(|p| now_ns - p.1)
    }

    pub fn channel(&self) -> &InputChannel {
        &self.channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keys::system_back;
    use crate::input::{ImeEvent, InputEvent};

    fn key(i: i64) -> InputEvent {
        let [mut k, _] = system_back(i);
        k.repeat_count = i as i32;
        InputEvent::Key(k)
    }

    #[test]
    fn events_and_replies_flow_both_ways() {
        let (c, a) = InputChannel::pair(4096).unwrap();
        let mut p = InputPublisher::new(c);
        for i in 0..10 {
            assert_eq!(p.publish(key(i), 100).unwrap(), i as u32 + 1);
        }
        for i in 0..10 {
            match a.recv_timeout(Duration::from_secs(1)).unwrap().unwrap() {
                Message::Event { seq, event } => {
                    assert_eq!(seq, i + 1);
                    assert_eq!(event, key(i as i64));
                    a.send(&Message::Finished {
                        seq,
                        handled: seq % 2 == 0,
                        consume_time_ns: 0,
                    })
                    .unwrap();
                }
                m => panic!("{m:?}"),
            }
        }
        assert_eq!(a.try_recv().unwrap(), None);
        let f = p.poll_finished(350).unwrap();
        assert_eq!(f.len(), 10);
        assert!(f.iter().all(|f| f.latency_ns == 250));
        assert_eq!(p.oldest_pending_ns(400), None);
    }

    #[test]
    fn surface_messages_flow_with_input() {
        let (c, a) = InputChannel::pair(4096).unwrap();
        let mut p = InputPublisher::new(c);
        p.send_surface(SurfaceMessage::Resize {
            surface: 3,
            width: 10,
            height: 20,
        })
        .unwrap();
        let seq = p.publish(key(1), 0).unwrap();
        assert!(matches!(
            a.try_recv().unwrap(),
            Some(Message::Surface(SurfaceMessage::Resize { surface: 3, .. }))
        ));
        assert!(matches!(a.try_recv().unwrap(), Some(Message::Event { .. })));
        let presented = SurfaceMessage::Presented {
            surface: 3,
            width: 10,
            height: 20,
            bottom_up: true,
        };
        a.send(&Message::Surface(presented)).unwrap();
        a.send(&Message::Finished {
            seq,
            handled: true,
            consume_time_ns: 0,
        })
        .unwrap();
        let (f, s) = p.poll(5).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(s, vec![presented]);
    }

    #[test]
    fn handles_have_a_command_line_form() {
        let h = ChannelHandles {
            section: 0x1a4,
            capacity: 65536,
            to_app: 0x1b0,
            to_compositor: 0x1b4,
        };
        assert_eq!(h.to_arg(), "1a4:10000:1b0:1b4");
        assert_eq!(ChannelHandles::parse(&h.to_arg()), Some(h));
        for bad in ["", "1:2:3", "1:2:3:4:5", "x:1:2:3", "1::2:3"] {
            assert_eq!(ChannelHandles::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn watch_posts_when_the_peer_sends() {
        crate::sys::win::ensure_queue();
        let (c, a) = InputChannel::pair(4096).unwrap();
        let me = crate::sys::win::current_thread_id();
        let _w = c.watch(me, 0x8000 + 0x55).unwrap();
        a.send(&Message::Surface(SurfaceMessage::Detach { surface: 1 }))
            .unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = false;
        while !got && std::time::Instant::now() < end {
            got = crate::sys::win::take_thread_message(0x8000 + 0x55);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(got, "no thread message after the peer sent");
    }

    #[test]
    fn full_ring_reports_full_and_recovers_with_wraparound() {
        let (c, a) = InputChannel::pair(1024).unwrap();
        let msg = Message::Event {
            seq: 1,
            event: InputEvent::Ime(ImeEvent::Commit {
                text: "x".repeat(100),
            }),
        };
        let mut sent = 0;
        loop {
            match c.send(&msg) {
                Ok(()) => sent += 1,
                Err(ChannelError::Full) => break,
                Err(e) => panic!("{e}"),
            }
        }
        // 129-byte messages in a 1024-byte ring.
        assert_eq!(sent, 7);
        // Many rounds: the positions wrap around the ring many times.
        for _ in 0..200 {
            assert_eq!(a.try_recv().unwrap().unwrap(), msg);
            c.send(&msg).unwrap();
        }
        let big = Message::Event {
            seq: 1,
            event: InputEvent::Ime(ImeEvent::Commit {
                text: "x".repeat(600),
            }),
        };
        assert!(matches!(c.send(&big), Err(ChannelError::TooLarge)));
    }

    #[test]
    fn wakes_a_waiting_reader_on_another_thread() {
        let (c, a) = InputChannel::pair(4096).unwrap();
        let t = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let m = a.recv_timeout(Duration::from_secs(5)).unwrap();
            (m, start.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        c.send(&Message::Event {
            seq: 7,
            event: InputEvent::Focus { has_focus: true },
        })
        .unwrap();
        let (m, waited) = t.join().unwrap();
        assert!(matches!(m, Some(Message::Event { seq: 7, .. })));
        assert!(waited < Duration::from_secs(2));
    }

    #[test]
    fn corrupted_positions_are_errors() {
        let (c, a) = InputChannel::pair(1024).unwrap();
        // The app (untrusted) moves the compositor's read position.
        c.section
            .word(128 + 128)
            .store(u64::MAX - 5, Ordering::Release);
        // Ring 1 is app to compositor; its read position is at 256.
        assert!(matches!(c.try_recv(), Err(ChannelError::Corrupted)));
        // A bogus length in the data.
        a.section.word(64 + 128).store(16, Ordering::Release);
        c.section.word(256).store(0, Ordering::Release);
        for i in 0..16 {
            c.section.bytes()[DATA_OFFSET + 1024 + i].store(0xFF, Ordering::Relaxed);
        }
        assert!(matches!(c.try_recv(), Err(ChannelError::Corrupted)));
    }
}

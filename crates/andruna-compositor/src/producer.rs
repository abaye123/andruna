//! The app process's end of a window channel: surfaces handed over by the
//! compositor, the window's configuration, and input.
//!
//! # Handshake
//!
//! The compositor (in sysd, or a test host) and the app process share one
//! [`channel`](crate::channel) per task window. The app gets its end as
//! inherited handles at launch (`andruna-app --compositor <handles>`, the
//! form of [`ChannelHandles::to_arg`]) and opens it with
//! [`Connection::open`]. Then, all as [`SurfaceMessage`]s:
//!
//! ```text
//!  compositor                                app
//!  attach_input(window, publisher)
//!    ── Configuration (changes 0) ─────────►  Connection::configuration
//!  create_surface + offer_surface(process)
//!    DuplicateHandle into the app
//!    ── Attach {surface, handle, w, h} ────►  Connection::next_surface
//!                                             swap chain on the handle,
//!                                             first Present
//!    ◄── Presented {surface, w, h, bottom_up}  RemoteSurface::presented
//!  visual flipped for bottom-up content,
//!  splash fades, Event::SurfacePresented
//!  window resized:
//!    ── Configuration (changes) ───────────►
//!  resize_surface(window, surface, w, h)
//!    ── Resize {surface, w, h} ────────────►  RemoteSurface::size changes;
//!                                             next frame: ResizeBuffers
//!    ◄── Presented {surface, w', h', ..}      new flip height
//!  window closed or surface removed:
//!    ── Detach {surface} ──────────────────►  RemoteSurface::is_open false
//! ```
//!
//! Input events arrive on the same channel; each gets a finished reply
//! (from the handler set with [`Connection::set_input_handler`], else
//! "not handled").
//!
//! # Pacing
//!
//! A producer presents with a sync interval of 1 and waits on its swap
//! chain's frame latency object; both are driven by DWM's composition, the
//! same clock [`crate::vsync`] reads, so the app's frames and the
//! compositor's vsync events share one period
//! ([`WireConfiguration::vsync_period_ns`]).
//!
//! With the `gles` feature, [`RemoteSurface`] is an
//! `andruna_gles::window::WindowTarget`, so an EGL window surface renders
//! into it.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::channel::{ChannelError, ChannelHandles, InputChannel};
use crate::input::InputEvent;
use crate::sys::shm::Handle;
use crate::wire::{Message, SurfaceMessage, WireConfiguration};

/// Decides whether an input event was handled (the finished reply).
pub type InputHandler = Box<dyn FnMut(&InputEvent) -> bool + Send>;

struct State {
    surfaces: HashMap<u64, Arc<RemoteSurface>>,
    /// Attached and not yet taken by [`Connection::next_surface`].
    offered: VecDeque<Arc<RemoteSurface>>,
    config: Option<WireConfiguration>,
    /// Configurations received so far.
    configs: u64,
    input: Option<InputHandler>,
    /// The channel failed (corrupted or closed); nothing more arrives.
    broken: bool,
}

struct Inner {
    channel: InputChannel,
    /// The app's ring has one writer at a time (render threads report
    /// presents, the reader replies to input).
    send_lock: Mutex<()>,
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, m: &Message) -> Result<(), ChannelError> {
        let _g = self.send_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.channel.send(m)
    }

    fn handle(&self, m: Message) {
        match m {
            Message::Surface(SurfaceMessage::Attach {
                surface,
                handle,
                width,
                height,
                z,
            }) => {
                // SAFETY: the compositor duplicated this handle into this
                // process for us; nothing else owns it.
                let handle = unsafe { Handle::from_raw(handle as usize) };
                let s = Arc::new(RemoteSurface {
                    id: surface,
                    handle,
                    z,
                    size: Mutex::new((width.max(1), height.max(1))),
                    open: AtomicBool::new(true),
                    last: Mutex::new(None),
                    resizes: AtomicU64::new(0),
                    conn: Mutex::new(None),
                });
                let mut st = self.lock();
                if st.surfaces.contains_key(&surface) {
                    return; // a repeated id: keep the first
                }
                st.surfaces.insert(surface, s.clone());
                st.offered.push_back(s);
            }
            Message::Surface(SurfaceMessage::Resize {
                surface,
                width,
                height,
            }) => {
                if let Some(s) = self.lock().surfaces.get(&surface) {
                    *s.size.lock().unwrap_or_else(|e| e.into_inner()) =
                        (width.max(1), height.max(1));
                    s.resizes.fetch_add(1, Ordering::AcqRel);
                }
            }
            Message::Surface(SurfaceMessage::Detach { surface }) => {
                let mut st = self.lock();
                if let Some(s) = st.surfaces.remove(&surface) {
                    s.open.store(false, Ordering::Release);
                }
                st.offered.retain(|s| s.id != surface);
            }
            Message::Surface(SurfaceMessage::Configuration(c)) => {
                let mut st = self.lock();
                st.config = Some(c);
                st.configs += 1;
            }
            // Sent by apps, not to them.
            Message::Surface(SurfaceMessage::Presented { .. }) | Message::Finished { .. } => {}
            Message::Event { seq, event } => {
                let handled = self.lock().input.as_mut().is_some_and(|h| h(&event));
                let _ = self.send(&Message::Finished {
                    seq,
                    handled,
                    consume_time_ns: crate::clock::uptime_ns(),
                });
            }
        }
        self.changed.notify_all();
    }
}

/// The app's end of a window channel. Dropping it stops the reader
/// thread; surfaces taken from it stay valid until dropped.
pub struct Connection {
    inner: Arc<Inner>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Connection {
    /// Opens the app's end from inherited or duplicated handles and starts
    /// reading it.
    ///
    /// # Safety
    /// The handle values must be valid handles of this process, owned by
    /// the caller (the connection closes them).
    pub unsafe fn open(h: ChannelHandles) -> Result<Connection, ChannelError> {
        // SAFETY: forwarded from the caller.
        let ch = unsafe { InputChannel::open(h) }?;
        Connection::from_channel(ch)
    }

    /// A connection on an app end in this process (tests, in-process
    /// producers).
    pub fn from_channel(channel: InputChannel) -> Result<Connection, ChannelError> {
        let inner = Arc::new(Inner {
            channel,
            send_lock: Mutex::new(()),
            state: Mutex::new(State {
                surfaces: HashMap::new(),
                offered: VecDeque::new(),
                config: None,
                configs: 0,
                input: None,
                broken: false,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let i = inner.clone();
        let reader = std::thread::Builder::new()
            .name("compositor-channel".into())
            .spawn(move || read_loop(i))?;
        Ok(Connection {
            inner,
            reader: Some(reader),
        })
    }

    /// The next surface the compositor offers, waiting up to `timeout`.
    pub fn next_surface(&self, timeout: Duration) -> Option<Arc<RemoteSurface>> {
        let end = Instant::now() + timeout;
        let mut st = self.inner.lock();
        loop {
            if let Some(s) = st.offered.pop_front() {
                drop(st);
                *s.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.inner.clone());
                return Some(s);
            }
            let left = end.checked_duration_since(Instant::now())?;
            if st.broken {
                return None;
            }
            st = self
                .inner
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The window's latest configuration.
    pub fn configuration(&self) -> Option<WireConfiguration> {
        self.inner.lock().config
    }

    /// Waits until more than `seen` configurations have arrived; returns
    /// the count and the latest.
    pub fn wait_configuration(
        &self,
        seen: u64,
        timeout: Duration,
    ) -> Option<(u64, WireConfiguration)> {
        let end = Instant::now() + timeout;
        let mut st = self.inner.lock();
        loop {
            if st.configs > seen
                && let Some(c) = st.config
            {
                return Some((st.configs, c));
            }
            let left = end.checked_duration_since(Instant::now())?;
            st = self
                .inner
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Who answers input events (default: none, every event is finished
    /// as not handled).
    pub fn set_input_handler(&self, h: Option<InputHandler>) {
        self.inner.lock().input = h;
    }

    /// False once the channel failed (the compositor went away or the
    /// section is corrupted).
    pub fn is_alive(&self) -> bool {
        !self.inner.lock().broken
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

fn read_loop(inner: Arc<Inner>) {
    while !inner.stop.load(Ordering::Acquire) {
        match inner.channel.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(m)) => {
                inner.handle(m);
                // Everything queued, without waiting for another wakeup.
                loop {
                    match inner.channel.try_recv() {
                        Ok(Some(m)) => inner.handle(m),
                        Ok(None) => break,
                        Err(_) => return broken(&inner),
                    }
                }
            }
            Ok(None) => {}
            Err(_) => return broken(&inner),
        }
    }
}

fn broken(inner: &Inner) {
    let mut st = inner.lock();
    st.broken = true;
    for s in st.surfaces.values() {
        s.open.store(false, Ordering::Release);
    }
    drop(st);
    inner.changed.notify_all();
}

/// A surface the compositor handed to this process.
pub struct RemoteSurface {
    id: u64,
    handle: Handle,
    z: i32,
    size: Mutex<(u32, u32)>,
    open: AtomicBool,
    /// The last reported present (width, height, bottom-up).
    last: Mutex<Option<(u32, u32, bool)>>,
    resizes: AtomicU64,
    /// Set when the surface is taken from its connection.
    conn: Mutex<Option<Arc<Inner>>>,
}

impl std::fmt::Debug for RemoteSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSurface")
            .field("id", &self.id)
            .field("size", &self.size())
            .field("open", &self.is_open())
            .finish()
    }
}

impl RemoteSurface {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn z(&self) -> i32 {
        self.z
    }

    /// The composition surface handle (owned by this object).
    pub fn handle(&self) -> usize {
        self.handle.raw()
    }

    /// The buffer size the compositor wants, in pixels.
    pub fn size(&self) -> (u32, u32) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Resize requests received.
    pub fn resizes(&self) -> u64 {
        self.resizes.load(Ordering::Acquire)
    }

    /// False after the compositor detached the surface (window closed).
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// After a present of a `width`x`height` buffer: tells the compositor
    /// when the size or the row order changed (and on the first present).
    pub fn presented(&self, width: u32, height: u32, bottom_up: bool) {
        let now = (width, height, bottom_up);
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if *last == Some(now) {
                return;
            }
            *last = Some(now);
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(c) = conn {
            let r = c.send(&Message::Surface(SurfaceMessage::Presented {
                surface: self.id,
                width,
                height,
                bottom_up,
            }));
            if r.is_err() {
                // Try again at the next present.
                *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }
}

#[cfg(feature = "gles")]
mod gles {
    use windows::Win32::Foundation::HANDLE;

    use super::RemoteSurface;

    /// A compositor surface as an EGL window target: OpenGL content is
    /// bottom-up, so every present is reported as such and the compositor
    /// flips the visual.
    impl andruna_gles::window::WindowTarget for RemoteSurface {
        fn size(&self) -> (u32, u32) {
            RemoteSurface::size(self)
        }

        fn surface_handle(&self) -> HANDLE {
            HANDLE(self.handle() as *mut _)
        }

        fn presented(&self, width: u32, height: u32) {
            RemoteSurface::presented(self, width, height, true);
        }

        fn is_open(&self) -> bool {
            RemoteSurface::is_open(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::InputPublisher;
    use crate::input::keys::system_back;

    fn attach(p: &InputPublisher, id: u64, w: u32, h: u32) {
        // A real handle owned by the receiving side: an event.
        let e = crate::sys::shm::Event::create().unwrap();
        let v = e.handle().duplicate_into(None).unwrap();
        p.send_surface(SurfaceMessage::Attach {
            surface: id,
            handle: v as u64,
            width: w,
            height: h,
            z: 0,
        })
        .unwrap();
    }

    #[test]
    fn surfaces_sizes_presents_and_detach() {
        let (c, a) = InputChannel::pair(4096).unwrap();
        let mut p = InputPublisher::new(c);
        let conn = Connection::from_channel(a).unwrap();
        assert!(conn.next_surface(Duration::from_millis(10)).is_none());
        let cfg = WireConfiguration {
            width_px: 640,
            height_px: 480,
            ..Default::default()
        };
        p.send_surface(SurfaceMessage::Configuration(cfg)).unwrap();
        assert_eq!(
            conn.wait_configuration(0, Duration::from_secs(5)),
            Some((1, cfg))
        );
        attach(&p, 9, 640, 480);
        let s = conn.next_surface(Duration::from_secs(5)).expect("surface");
        assert_eq!((s.id(), s.size(), s.is_open()), (9, (640, 480), true));

        // Presents are reported once per size and orientation.
        s.presented(640, 480, true);
        s.presented(640, 480, true);
        p.send_surface(SurfaceMessage::Resize {
            surface: 9,
            width: 400,
            height: 600,
        })
        .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while s.size() != (400, 600) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!((s.size(), s.resizes()), ((400, 600), 1));
        s.presented(400, 600, true);
        let mut got = vec![];
        let end = Instant::now() + Duration::from_secs(5);
        while got.len() < 2 && Instant::now() < end {
            got.extend(p.poll(0).unwrap().1);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            got,
            vec![
                SurfaceMessage::Presented {
                    surface: 9,
                    width: 640,
                    height: 480,
                    bottom_up: true
                },
                SurfaceMessage::Presented {
                    surface: 9,
                    width: 400,
                    height: 600,
                    bottom_up: true
                },
            ]
        );

        // Input without a handler is finished as not handled.
        let [k, _] = system_back(1);
        let seq = p.publish(InputEvent::Key(k), 0).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        let mut fin = vec![];
        while fin.is_empty() && Instant::now() < end {
            fin = p.poll(1).unwrap().0;
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fin.len(), 1);
        assert_eq!((fin[0].seq, fin[0].handled), (seq, false));

        p.send_surface(SurfaceMessage::Detach { surface: 9 })
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while s.is_open() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!s.is_open());
        drop(conn);
        // The surface outlives its connection.
        assert_eq!(s.size(), (400, 600));
    }
}

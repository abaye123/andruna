//! The compositor: one thread that owns the task windows, their
//! DirectComposition trees and their input.
//!
//! [`Compositor::start`] starts the thread and returns a handle for
//! commands (create a window, add a surface, ...) and a receiver of
//! [`Event`]s (configuration changes, input, close requests). In sysd the
//! activity manager drives the handle; the test host and the tests drive
//! it directly.
//!
//! Windows sends some messages synchronously from inside Win32 calls the
//! compositor makes (resizing sends `WM_GETMINMAXINFO`, `WM_NCCALCSIZE`,
//! `WM_SIZE`). Those are handled without the compositor state (frame and
//! hit testing are stateless; the minimum size is kept aside), and a
//! layout pass is posted for later, so the state is never borrowed twice.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::android::keycode;
use crate::channel::InputPublisher;
use crate::clock;
use crate::config::{
    self, ActivitySpec, CAPTION_HEIGHT_DIP, DeviceTraits, WindowLayout, WindowMetrics,
};
use crate::geom::{Rect, Size};
use crate::input::keys::{self, KeyTracker, SCAN_PAUSE, system_back};
use crate::input::kcm::{self, Mods};
use crate::input::pointer::{PointerFrame, PointerKind, PointerSample, PointerTracker};
use crate::input::{ImeEvent, InputEvent};
use crate::sys::gfx::{Caption, Gfx, Image, Rgb, SurfaceVisual, WindowTree};
use crate::sys::win::{self, Hwnd, Msg};
use crate::sys::{capture, ime, pointer, vsync};

pub use crate::sys::vsync::TargetId;

/// A task window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

/// A surface (a visual) in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u64);

/// The splash shown until the app's first frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Splash {
    /// The app icon, premultiplied BGRA.
    pub icon: Option<Image>,
    /// `windowSplashScreenBackground`, 0xRRGGBB; the backdrop when absent.
    pub background: Option<u32>,
}

/// What a new task window shows.
#[derive(Clone, Debug, Default)]
pub struct WindowSpec {
    pub title: String,
    pub activity: ActivitySpec,
    /// The color the app requests for its status bar (title bar), 0xRRGGBB.
    pub caption_color: Option<u32>,
    pub splash: Option<Splash>,
    /// Client size in pixels instead of the default size (tests).
    pub client_size: Option<Size>,
    /// Show without taking focus.
    pub no_activate: bool,
}

/// What the compositor reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The window's configuration (first layout, or a change). `changes`
    /// is the `ActivityInfo.CONFIG_*` mask, 0 for the first one.
    Configuration {
        window: WindowId,
        layout: WindowLayout,
        changes: u32,
    },
    /// The window moved to another display.
    Display {
        window: WindowId,
        target: Option<TargetId>,
        dpi: u32,
    },
    /// Input for the window (when no input channel is attached).
    /// `delivered_ns` is when the compositor produced it, on the uptime
    /// clock.
    Input {
        window: WindowId,
        event: InputEvent,
        delivered_ns: i64,
    },
    /// The keyboard layout changed: Android key character map overlay.
    KeyboardLayout { layout: isize, kcm: String },
    /// The user asked to close the window (Android decides what happens).
    CloseRequested { window: WindowId },
    Destroyed { window: WindowId },
}

enum Cmd {
    CreateWindow(WindowSpec, Sender<io::Result<WindowId>>),
    CreateSurface {
        window: WindowId,
        z: i32,
        rect: Rect,
        reply: Sender<io::Result<(SurfaceId, crate::surface::SurfaceHandle)>>,
    },
    MoveSurface(WindowId, SurfaceId, i32, i32),
    RemoveSurface(WindowId, SurfaceId),
    SetCaptionColor(WindowId, Option<u32>),
    SetTitle(WindowId, String),
    FirstFrame(WindowId),
    SetImeCaret(WindowId, Rect),
    FinishComposition(WindowId),
    AttachInput(WindowId, Box<InputPublisher>),
    SetClientSize(WindowId, Size),
    Maximize(WindowId, bool),
    Close(WindowId),
    Screenshot(WindowId, Sender<io::Result<Image>>),
    Layout(WindowId, Sender<Option<WindowLayout>>),
    Native(WindowId, Sender<Option<isize>>),
    Quit,
}

const WM_APP_CMD: u32 = 0x8000 + 1;
const WM_APP_RELAYOUT: u32 = 0x8000 + 2;
const SPLASH_TIMER: usize = 1;
const SPLASH_FADE_MS: u32 = 200;

/// The handle to the compositor thread.
pub struct Compositor {
    tx: Sender<Cmd>,
    thread: u32,
    join: Option<std::thread::JoinHandle<()>>,
    /// Whether the thread uses the WARP software renderer.
    pub software: bool,
}

impl Compositor {
    /// Starts the compositor thread.
    pub fn start() -> io::Result<(Compositor, Receiver<Event>)> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (etx, erx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("compositor".into())
            .spawn(move || run(rx, etx, ready_tx))?;
        let (thread, software) = ready_rx
            .recv()
            .map_err(|_| io::Error::other("compositor thread failed"))??;
        Ok((
            Compositor {
                tx,
                thread,
                join: Some(join),
                software,
            },
            erx,
        ))
    }

    fn send(&self, c: Cmd) -> io::Result<()> {
        self.tx
            .send(c)
            .map_err(|_| io::Error::other("compositor stopped"))?;
        if win::post_thread(self.thread, WM_APP_CMD) {
            Ok(())
        } else {
            Err(io::Error::other("compositor stopped"))
        }
    }

    fn call<T>(&self, f: impl FnOnce(Sender<T>) -> Cmd) -> io::Result<T> {
        let (tx, rx) = mpsc::channel();
        self.send(f(tx))?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| io::Error::other("compositor did not answer"))
    }

    /// Opens a task window (shown at once, with its splash if any).
    pub fn create_window(&self, spec: WindowSpec) -> io::Result<WindowId> {
        self.call(|r| Cmd::CreateWindow(spec, r))?
    }

    /// Adds a surface at `rect` (app pixels) with z order `z`. The handle
    /// is a composition surface handle in this process, owned by the
    /// caller: duplicate it into the producer, then close it.
    pub fn create_surface(
        &self,
        window: WindowId,
        z: i32,
        rect: Rect,
    ) -> io::Result<(SurfaceId, crate::surface::SurfaceHandle)> {
        self.call(|reply| Cmd::CreateSurface {
            window,
            z,
            rect,
            reply,
        })?
    }

    pub fn move_surface(&self, w: WindowId, s: SurfaceId, x: i32, y: i32) -> io::Result<()> {
        self.send(Cmd::MoveSurface(w, s, x, y))
    }

    pub fn remove_surface(&self, w: WindowId, s: SurfaceId) -> io::Result<()> {
        self.send(Cmd::RemoveSurface(w, s))
    }

    /// The title bar color (the app's status bar color), or the backdrop.
    pub fn set_caption_color(&self, w: WindowId, rgb: Option<u32>) -> io::Result<()> {
        self.send(Cmd::SetCaptionColor(w, rgb))
    }

    pub fn set_title(&self, w: WindowId, title: &str) -> io::Result<()> {
        self.send(Cmd::SetTitle(w, title.to_owned()))
    }

    /// The app drew its first frame: the splash fades out.
    pub fn first_frame(&self, w: WindowId) -> io::Result<()> {
        self.send(Cmd::FirstFrame(w))
    }

    /// Where the focused text field's cursor is (window pixels), for the
    /// IME's candidate window.
    pub fn set_ime_caret(&self, w: WindowId, caret: Rect) -> io::Result<()> {
        self.send(Cmd::SetImeCaret(w, caret))
    }

    /// Commits the IME composition (`finishComposingText`).
    pub fn finish_composition(&self, w: WindowId) -> io::Result<()> {
        self.send(Cmd::FinishComposition(w))
    }

    /// Delivers the window's input through a channel instead of
    /// [`Event::Input`].
    pub fn attach_input(&self, w: WindowId, p: InputPublisher) -> io::Result<()> {
        self.send(Cmd::AttachInput(w, Box::new(p)))
    }

    pub fn set_client_size(&self, w: WindowId, size: Size) -> io::Result<()> {
        self.send(Cmd::SetClientSize(w, size))
    }

    pub fn maximize(&self, w: WindowId, maximized: bool) -> io::Result<()> {
        self.send(Cmd::Maximize(w, maximized))
    }

    pub fn close(&self, w: WindowId) -> io::Result<()> {
        self.send(Cmd::Close(w))
    }

    /// The client area as displayed (DirectComposition content included).
    pub fn screenshot(&self, w: WindowId) -> io::Result<Image> {
        self.call(|r| Cmd::Screenshot(w, r))?
    }

    /// The Windows window handle (tests, the shell integration).
    pub fn native_window(&self, w: WindowId) -> io::Result<Option<isize>> {
        self.call(|r| Cmd::Native(w, r))
    }

    /// The window's current layout.
    pub fn layout(&self, w: WindowId) -> io::Result<Option<WindowLayout>> {
        self.call(|r| Cmd::Layout(w, r))
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.send(Cmd::Quit);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Win {
    id: WindowId,
    hwnd: Hwnd,
    spec: WindowSpec,
    fixed_dp: Option<Size>,
    tree: WindowTree,
    layout: Option<WindowLayout>,
    dpi: u32,
    monitor: isize,
    origin: (i32, i32),
    surfaces: Vec<(SurfaceId, i32, SurfaceVisual)>,
    mouse: PointerTracker,
    touch: PointerTracker,
    pen: PointerTracker,
    keys: KeyTracker,
    last_touch_frame: Option<u32>,
    back_hover: bool,
    back_pressed: bool,
    /// The `WM_CHAR`s key downs will produce, which the app gets through
    /// the key events (`None`: any, after a dead key), oldest first.
    expect_chars: std::collections::VecDeque<Option<u16>>,
    after_dead: bool,
    high_surrogate: Option<u16>,
    composing: bool,
    publisher: Option<Box<InputPublisher>>,
    caption: Option<Caption>,
}

struct State {
    gfx: Gfx,
    wins: HashMap<isize, Win>,
    ids: HashMap<WindowId, isize>,
    next_id: u64,
    events: Sender<Event>,
    traits: DeviceTraits,
    layout_kcm: Option<isize>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Minimum client sizes, readable while the state is borrowed.
    static MIN_SIZE: RefCell<HashMap<isize, Size>> = RefCell::new(HashMap::new());
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

fn device_traits() -> DeviceTraits {
    DeviceTraits {
        touchscreen: win::has_touch(),
        keyboard: true,
        night: win::apps_use_dark_theme(),
        rtl: false,
        font_scale: 1.0,
    }
}

fn run(rx: Receiver<Cmd>, events: Sender<Event>, ready: Sender<io::Result<(u32, bool)>>) {
    win::init_thread();
    win::ensure_queue();
    let gfx = match Gfx::new() {
        Ok(g) => g,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let software = gfx.warp;
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            gfx,
            wins: HashMap::new(),
            ids: HashMap::new(),
            next_id: 1,
            events,
            traits: device_traits(),
            layout_kcm: None,
        })
    });
    win::set_handler(handle_message);
    let _ = ready.send(Ok((win::current_thread_id(), software)));
    let mut quit = false;
    while !quit
        && win::pump_one(&mut |m| {
            if m == WM_APP_CMD {
                while let Ok(c) = rx.try_recv() {
                    if matches!(c, Cmd::Quit) {
                        quit = true;
                        win::quit();
                        break;
                    }
                    command(c);
                }
            }
        })
    {}
    let hwnds: Vec<isize> = with_state(|s| s.wins.keys().copied().collect()).unwrap_or_default();
    for h in hwnds {
        win::destroy(Hwnd(h));
    }
    win::clear_handler();
    STATE.with(|s| *s.borrow_mut() = None);
}

fn emit(s: &State, e: Event) {
    let _ = s.events.send(e);
}

fn command(c: Cmd) {
    match c {
        Cmd::CreateWindow(spec, reply) => {
            let _ = reply.send(create_window(spec));
        }
        Cmd::CreateSurface {
            window,
            z,
            rect,
            reply,
        } => {
            let r = with_state(|s| {
                let w = s.ids.get(&window).and_then(|h| s.wins.get_mut(h));
                let w = w.ok_or_else(|| io::Error::other("no such window"))?;
                let v = s.gfx.create_surface()?;
                v.set_position(rect.left, rect.top)?;
                let id = SurfaceId(s.next_id);
                s.next_id += 1;
                let handle = crate::surface::SurfaceHandle {
                    handle: v.handle().duplicate_into(None)?,
                    width: rect.width().max(1) as u32,
                    height: rect.height().max(1) as u32,
                };
                w.surfaces.push((id, z, v));
                w.surfaces.sort_by_key(|x| x.1);
                let list: Vec<&SurfaceVisual> = w.surfaces.iter().map(|x| &x.2).collect();
                w.tree.set_surfaces(&list)?;
                s.gfx.commit()?;
                Ok((id, handle))
            })
            .unwrap_or_else(|| Err(io::Error::other("compositor busy")));
            let _ = reply.send(r);
        }
        Cmd::MoveSurface(w, sid, x, y) => {
            with_window(w, |s, win| {
                if let Some(v) = win.surfaces.iter().find(|v| v.0 == sid) {
                    let _ = v.2.set_position(x, y);
                    let _ = s.gfx.commit();
                }
            });
        }
        Cmd::RemoveSurface(w, sid) => {
            with_window(w, |s, win| {
                win.surfaces.retain(|v| v.0 != sid);
                let list: Vec<&SurfaceVisual> = win.surfaces.iter().map(|x| &x.2).collect();
                let _ = win.tree.set_surfaces(&list);
                let _ = s.gfx.commit();
            });
        }
        Cmd::SetCaptionColor(w, c) => {
            let h = with_window(w, |s, win| {
                win.spec.caption_color = c;
                win.caption = None;
                (win.hwnd, frame_dark(&s.traits, c))
            });
            if let Some((h, dark)) = h {
                win::style_frame(h, dark, c);
                relayout(h);
            }
        }
        Cmd::SetTitle(w, t) => {
            if let Some(h) = with_window(w, |_, win| {
                win.spec.title = t.clone();
                win.caption = None;
                win.hwnd
            }) {
                win::set_title(h, &t);
                relayout(h);
            }
        }
        Cmd::FirstFrame(w) => {
            with_window(w, |s, win| {
                if win.tree.has_splash() {
                    let _ = s.gfx.fade_splash(&mut win.tree, SPLASH_FADE_MS);
                    let _ = s.gfx.commit();
                    win::set_timer(win.hwnd, SPLASH_TIMER, SPLASH_FADE_MS + 50);
                }
            });
        }
        Cmd::SetImeCaret(w, r) => {
            if let Some((h, origin)) = with_window(w, |_, win| {
                let l = win.layout.map(|l| (l.content, l.scale));
                (win.hwnd, l)
            }) {
                let (content, scale) = origin.unwrap_or((Rect::default(), 1.0));
                let map = |v: i32, o: i32| o + (v as f32 * scale) as i32;
                ime::set_caret(
                    h,
                    Rect::new(
                        map(r.left, content.left),
                        map(r.top, content.top),
                        map(r.right, content.left),
                        map(r.bottom, content.top),
                    ),
                );
            }
        }
        Cmd::FinishComposition(w) => {
            if let Some(h) = with_window(w, |_, win| win.hwnd) {
                ime::complete(h);
            }
        }
        Cmd::AttachInput(w, p) => {
            with_window(w, |_, win| win.publisher = Some(p));
        }
        Cmd::SetClientSize(w, size) => {
            if let Some(h) = with_window(w, |_, win| win.hwnd) {
                win::set_client_size(h, size);
                relayout(h);
            }
        }
        Cmd::Maximize(w, m) => {
            if let Some(h) = with_window(w, |_, win| win.hwnd) {
                if m {
                    win::maximize(h);
                } else {
                    win::restore(h);
                }
                relayout(h);
            }
        }
        Cmd::Close(w) => {
            if let Some(h) = with_window(w, |_, win| win.hwnd) {
                win::destroy(h);
            }
        }
        Cmd::Screenshot(w, reply) => {
            let r = match with_window(w, |_, win| win.hwnd) {
                Some(h) => screenshot(h),
                None => Err(io::Error::other("no such window")),
            };
            let _ = reply.send(r);
        }
        Cmd::Layout(w, reply) => {
            let _ = reply.send(with_window(w, |_, win| win.layout).flatten());
        }
        Cmd::Native(w, reply) => {
            let _ = reply.send(with_window(w, |_, win| win.hwnd.0));
        }
        Cmd::Quit => {}
    }
}

fn with_window<R>(id: WindowId, f: impl FnOnce(&State, &mut Win) -> R) -> Option<R> {
    with_state(|s| {
        let h = *s.ids.get(&id)?;
        let mut w = s.wins.remove(&h)?;
        let r = f(s, &mut w);
        s.wins.insert(h, w);
        Some(r)
    })
    .flatten()
}

/// Caption buttons are white on dark caption backgrounds.
fn frame_dark(traits: &DeviceTraits, caption: Option<u32>) -> bool {
    match caption {
        Some(c) => Rgb(c).luminance() < 0.4,
        None => traits.night,
    }
}

fn screenshot(h: Hwnd) -> io::Result<Image> {
    let outer = win::window_rect(h);
    let full = capture::window(h, outer.width(), outer.height())?;
    let (ox, oy) = win::client_origin(h);
    let size = win::client_size(h);
    let (dx, dy) = ((ox - outer.left).max(0) as u32, (oy - outer.top).max(0) as u32);
    let w = (size.width as u32).min(full.width.saturating_sub(dx));
    let ht = (size.height as u32).min(full.height.saturating_sub(dy));
    let mut bgra = Vec::with_capacity((w * ht * 4) as usize);
    for y in 0..ht {
        let start = (((y + dy) * full.width + dx) * 4) as usize;
        bgra.extend_from_slice(&full.bgra[start..start + (w * 4) as usize]);
    }
    Ok(Image {
        width: w,
        height: ht,
        bgra,
    })
}

fn create_window(spec: WindowSpec) -> io::Result<WindowId> {
    // Size from the primary work area at the system DPI; the window is
    // re-laid out on its real monitor right after creation.
    let probe = win::create_window(&spec.title, Size::new(100, 100), 96)?;
    let dpi = win::dpi(probe);
    let monitor = win::monitor_of(probe);
    let fixed_dp = (!spec.activity.resizeable)
        .then(|| config::fixed_content_dp(&spec.activity, monitor.work.size(), dpi));
    let client = spec.client_size.unwrap_or_else(|| {
        config::initial_client_size(&spec.activity, fixed_dp, monitor.work.size(), dpi)
    });
    let hwnd = probe;
    let night = with_state(|s| s.traits.night).unwrap_or(false);
    win::style_frame(
        hwnd,
        match spec.caption_color {
            Some(c) => Rgb(c).luminance() < 0.4,
            None => night,
        },
        spec.caption_color,
    );
    if !spec.activity.resizeable {
        win::set_resizable(hwnd, false);
    }
    MIN_SIZE.with(|m| {
        m.borrow_mut().insert(
            hwnd.0,
            config::min_client_size(&spec.activity, fixed_dp, dpi),
        )
    });
    win::set_client_size(hwnd, client);
    win::center_on_monitor(hwnd);

    let id = with_state(|s| -> io::Result<WindowId> {
        let tree = s.gfx.create_tree(hwnd)?;
        let id = WindowId(s.next_id);
        s.next_id += 1;
        s.ids.insert(id, hwnd.0);
        s.wins.insert(
            hwnd.0,
            Win {
                id,
                hwnd,
                spec: spec.clone(),
                fixed_dp,
                tree,
                layout: None,
                dpi,
                monitor: 0,
                origin: (0, 0),
                surfaces: Vec::new(),
                mouse: PointerTracker::new(PointerKind::Mouse),
                touch: PointerTracker::new(PointerKind::Touch),
                pen: PointerTracker::new(PointerKind::Pen),
                keys: KeyTracker::new(),
                last_touch_frame: None,
                back_hover: false,
                back_pressed: false,
                expect_chars: Default::default(),
                after_dead: false,
                high_surrogate: None,
                composing: false,
                publisher: None,
                caption: None,
            },
        );
        Ok(id)
    })
    .unwrap_or_else(|| Err(io::Error::other("compositor busy")));
    let id = match id {
        Ok(id) => id,
        Err(e) => {
            win::destroy(hwnd);
            return Err(e);
        }
    };
    relayout(hwnd);
    with_state(|s| {
        if let Some(w) = s.wins.get_mut(&hwnd.0)
            && let (Some(splash), Some(l)) = (w.spec.splash.clone(), w.layout)
        {
            let px = config::dp_to_px(160, config::density_dpi(w.dpi));
            let _ = s.gfx.show_splash(
                &mut w.tree,
                l.content,
                splash.background.map(Rgb),
                splash.icon.as_ref(),
                px,
            );
            let _ = s.gfx.commit();
        }
        update_keyboard_layout(s);
    });
    win::show(hwnd, !spec.no_activate);
    relayout(hwnd);
    Ok(id)
}

fn update_keyboard_layout(s: &mut State) {
    let layout = ime::current_layout();
    if s.layout_kcm == Some(layout) {
        return;
    }
    s.layout_kcm = Some(layout);
    let kcm = kcm::generate(|scan, mods: Mods| ime::key_output(layout, scan, mods));
    emit(s, Event::KeyboardLayout { layout, kcm });
}

/// The caption height and back button of a window at `dpi`.
fn caption_geometry(dpi: u32) -> (i32, Rect) {
    let h = config::dip_to_px(CAPTION_HEIGHT_DIP, dpi);
    (h, Rect::new(0, 0, config::dip_to_px(44, dpi), h))
}

/// Measures the window and updates its layout, tree and caption; reports
/// configuration changes.
fn relayout(h: Hwnd) {
    let done = with_state(|s| {
        let Some(w) = s.wins.get_mut(&h.0) else {
            return;
        };
        let dpi = win::dpi(h);
        let mon = win::monitor_of(h);
        let (ox, oy) = win::client_origin(h);
        let size = win::client_size(h);
        if size.width <= 0 || size.height <= 0 {
            return; // minimized
        }
        let (caption_px, back) = caption_geometry(dpi);
        let m = WindowMetrics {
            client: Rect::from_size(ox - mon.rect.left, oy - mon.rect.top, size),
            display: Rect::new(0, 0, mon.rect.width(), mon.rect.height()),
            dpi,
            caption_px,
        };
        let layout = config::compute(&m, &w.spec.activity, w.fixed_dp, &s.traits);
        let changes = w.layout.map(|old| config::diff(&old.config, &layout.config));
        w.origin = (ox, oy);
        if w.layout != Some(layout) {
            let _ = w.tree.set_content(layout.content, layout.scale);
            let raw = (
                (m.client.left + layout.content.left) as f32,
                (m.client.top + layout.content.top) as f32,
            );
            for t in [&mut w.mouse, &mut w.touch, &mut w.pen] {
                t.set_raw_offset(raw.0, raw.1);
            }
        }
        if w.dpi != dpi || w.monitor != mon.handle {
            w.dpi = dpi;
            w.monitor = mon.handle;
            let target = vsync::monitor_target(mon.handle);
            MIN_SIZE.with(|ms| {
                ms.borrow_mut().insert(
                    h.0,
                    config::min_client_size(&w.spec.activity, w.fixed_dp, dpi),
                )
            });
            let _ = s.events.send(Event::Display {
                window: w.id,
                target,
                dpi,
            });
        }
        let buttons_left = win::caption_button_bounds(h)
            .map_or(size.width - config::dip_to_px(138, dpi), |b| b.left);
        let fg = match w.spec.caption_color {
            Some(c) if Rgb(c).luminance() < 0.4 => Rgb(0xFFFFFF),
            Some(_) => Rgb(0x000000),
            None if s.traits.night => Rgb(0xFFFFFF),
            None => Rgb(0x000000),
        };
        let caption = Caption {
            width: size.width,
            height: caption_px,
            dpi,
            background: w.spec.caption_color.map(Rgb),
            foreground: fg,
            title: w.spec.title.clone(),
            back,
            back_hover: w.back_hover,
            buttons_left,
        };
        if w.caption.as_ref() != Some(&caption) {
            let _ = s.gfx.draw_caption(&mut w.tree, &caption);
            w.caption = Some(caption);
        }
        if w.tree.has_splash()
            && let (Some(sp), Some(old)) = (w.spec.splash.clone(), w.layout)
            && old.content != layout.content
        {
            let px = config::dp_to_px(160, config::density_dpi(dpi));
            let _ = s.gfx.show_splash(
                &mut w.tree,
                layout.content,
                sp.background.map(Rgb),
                sp.icon.as_ref(),
                px,
            );
        }
        let _ = s.gfx.commit();
        let first = w.layout.is_none();
        w.layout = Some(layout);
        let changes = match changes {
            _ if first => Some(0),
            Some(c) if c != 0 => Some(c),
            _ => None,
        };
        if let Some(changes) = changes {
            let _ = s.events.send(Event::Configuration {
                window: w.id,
                layout,
                changes,
            });
        }
    });
    if done.is_none() {
        // The state is in use further up the stack: lay out later.
        win::post(h, WM_APP_RELAYOUT);
    }
}

/// Delivers an input event to the window's app.
fn deliver(s: &State, w: &mut Win, event: InputEvent) {
    let now = clock::uptime_ns();
    if let Some(p) = w.publisher.as_mut() {
        if p.publish(event, now).is_ok() {
            let _ = p.poll_finished(now);
            return;
        }
        // A full or broken channel drops the event, as Android's
        // dispatcher drops events for an unresponsive window.
        return;
    }
    emit(
        s,
        Event::Input {
            window: w.id,
            event,
            delivered_ns: now,
        },
    );
}

fn lo(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn hi_i16(v: usize) -> i16 {
    ((v >> 16) & 0xFFFF) as u16 as i16
}

fn point_lparam(l: isize) -> (i32, i32) {
    ((l & 0xFFFF) as u16 as i16 as i32, ((l >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// Hit testing of the custom caption, without state.
fn hit_test(m: Msg) -> isize {
    const HTCLIENT: isize = 1;
    const HTCAPTION: isize = 2;
    const HTTOP: isize = 12;
    const HTTOPLEFT: isize = 13;
    const HTTOPRIGHT: isize = 14;
    let (sx, sy) = point_lparam(m.lparam);
    let (x, y) = win::screen_to_client(m.hwnd, sx, sy);
    let dpi = win::dpi(m.hwnd);
    let (caption, back) = caption_geometry(dpi);
    let size = win::client_size(m.hwnd);
    if y < 0 || x < 0 || x >= size.width {
        return win::def_window_proc(m);
    }
    if y >= caption {
        return HTCLIENT;
    }
    let border = win::frame_y(dpi);
    if y < border && !win::is_maximized(m.hwnd) && win::is_resizable(m.hwnd) {
        let corner = win::frame_x(dpi) * 2;
        return if x < corner {
            HTTOPLEFT
        } else if x >= size.width - corner {
            HTTOPRIGHT
        } else {
            HTTOP
        };
    }
    if back.contains(x, y) {
        HTCLIENT
    } else {
        HTCAPTION
    }
}

fn handle_message(m: Msg) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging as wm;
    // Caption buttons (hover, click, snap layouts) first.
    if let Some(r) = win::dwm_def_window_proc(m) {
        return Some(r);
    }
    match m.msg {
        wm::WM_NCCALCSIZE if m.wparam != 0 => Some(win::nccalcsize(m)),
        wm::WM_NCHITTEST => Some(hit_test(m)),
        wm::WM_GETMINMAXINFO => {
            if let Some(min) = MIN_SIZE.with(|s| s.borrow().get(&m.hwnd.0).copied()) {
                win::set_min_track_size(m, min);
            }
            Some(0)
        }
        wm::WM_DPICHANGED => {
            win::set_window_rect(m.hwnd, win::dpi_changed_rect(m));
            relayout(m.hwnd);
            Some(0)
        }
        wm::WM_WINDOWPOSCHANGED | wm::WM_SIZE | WM_APP_RELAYOUT => {
            relayout(m.hwnd);
            None
        }
        wm::WM_PAINT => {
            win::validate(m.hwnd);
            Some(0)
        }
        wm::WM_ERASEBKGND => Some(1),
        wm::WM_CLOSE => {
            with_state(|s| {
                if let Some(w) = s.wins.get(&m.hwnd.0) {
                    emit(s, Event::CloseRequested { window: w.id });
                }
            });
            Some(0)
        }
        wm::WM_DESTROY => {
            with_state(|s| {
                if let Some(w) = s.wins.remove(&m.hwnd.0) {
                    s.ids.remove(&w.id);
                    emit(s, Event::Destroyed { window: w.id });
                }
            });
            MIN_SIZE.with(|s| s.borrow_mut().remove(&m.hwnd.0));
            None
        }
        wm::WM_TIMER if m.wparam == SPLASH_TIMER => {
            win::kill_timer(m.hwnd, SPLASH_TIMER);
            with_state(|s| {
                if let Some(w) = s.wins.get_mut(&m.hwnd.0) {
                    s.gfx.remove_splash(&mut w.tree);
                    let _ = s.gfx.commit();
                }
            });
            Some(0)
        }
        wm::WM_SETTINGCHANGE if win::is_theme_change(m.lparam) => {
            let hs = with_state(|s| {
                s.traits.night = win::apps_use_dark_theme();
                s.wins
                    .values_mut()
                    .map(|w| {
                        w.caption = None;
                        (w.hwnd, frame_dark(&s.traits, w.spec.caption_color), w.spec.caption_color)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
            for (h, dark, c) in hs {
                win::style_frame(h, dark, c);
                relayout(h);
            }
            None
        }
        wm::WM_INPUTLANGCHANGE => {
            with_state(update_keyboard_layout);
            None
        }
        wm::WM_SETFOCUS | wm::WM_KILLFOCUS => {
            let focus = m.msg == wm::WM_SETFOCUS;
            with_state(|s| {
                if let Some(mut w) = s.wins.remove(&m.hwnd.0) {
                    if !focus {
                        let now = clock::uptime_ns();
                        for k in w.keys.cancel_all(now, win::locks()) {
                            deliver(s, &mut w, InputEvent::Key(k));
                        }
                    }
                    deliver(s, &mut w, InputEvent::Focus { has_focus: focus });
                    s.wins.insert(m.hwnd.0, w);
                }
            });
            None
        }
        wm::WM_POINTERDOWN
        | wm::WM_POINTERUP
        | wm::WM_POINTERUPDATE
        | wm::WM_POINTERENTER
        | wm::WM_POINTERLEAVE
        | wm::WM_POINTERCAPTURECHANGED => pointer_message(m),
        wm::WM_POINTERWHEEL | wm::WM_POINTERHWHEEL | wm::WM_MOUSEWHEEL | wm::WM_MOUSEHWHEEL => {
            wheel(m)
        }
        wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN | wm::WM_KEYUP | wm::WM_SYSKEYUP => key_message(m),
        wm::WM_CHAR => char_message(m),
        wm::WM_SYSCHAR if m.wparam != ' ' as usize => Some(0),
        wm::WM_DEADCHAR => Some(0),
        wm::WM_IME_SETCONTEXT => {
            // Hide the default composition window: the app shows the
            // composition in its text field.
            const ISC_SHOWUICOMPOSITIONWINDOW: isize = 0x8000_0000u32 as i32 as isize;
            Some(win::def_window_proc(Msg {
                lparam: m.lparam & !ISC_SHOWUICOMPOSITIONWINDOW,
                ..m
            }))
        }
        wm::WM_IME_STARTCOMPOSITION => Some(0),
        wm::WM_IME_COMPOSITION => {
            let c = ime::composition(m.hwnd, m.lparam as u32);
            with_state(|s| {
                if let Some(mut w) = s.wins.remove(&m.hwnd.0) {
                    if let Some(text) = c.result {
                        w.composing = false;
                        deliver(s, &mut w, InputEvent::Ime(ImeEvent::Commit { text }));
                    }
                    if let Some((text, cursor, target)) = c.composing {
                        w.composing = !text.is_empty();
                        deliver(
                            s,
                            &mut w,
                            InputEvent::Ime(ImeEvent::Composing {
                                text,
                                cursor,
                                target,
                            }),
                        );
                    }
                    s.wins.insert(m.hwnd.0, w);
                }
            });
            Some(0)
        }
        wm::WM_IME_ENDCOMPOSITION => {
            with_state(|s| {
                if let Some(mut w) = s.wins.remove(&m.hwnd.0) {
                    if w.composing {
                        w.composing = false;
                        deliver(s, &mut w, InputEvent::Ime(ImeEvent::FinishComposing));
                    }
                    s.wins.insert(m.hwnd.0, w);
                }
            });
            Some(0)
        }
        _ => None,
    }
}

fn pointer_message(m: Msg) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging as wm;
    let id = lo(m.wparam);
    let kind = pointer::kind(id)?;
    let r = with_state(|s| {
        let mut w = s.wins.remove(&m.hwnd.0)?;
        let (ox, oy) = w.origin;
        let (content, scale) = w.layout.map_or((Rect::default(), 1.0), |l| (l.content, l.scale));
        let map = move |sx: i32, sy: i32| {
            (
                ((sx - ox - content.left) as f32) / scale,
                ((sy - oy - content.top) as f32) / scale,
            )
        };
        let mut frames: Vec<PointerFrame> = match kind {
            PointerKind::Touch => {
                let fid = pointer::frame_id(id);
                if fid.is_some() && fid == w.last_touch_frame {
                    Vec::new()
                } else {
                    w.last_touch_frame = fid;
                    pointer::read(id, kind, &map)
                }
            }
            _ => pointer::read(id, kind, &map),
        };
        if m.msg == wm::WM_POINTERLEAVE || m.msg == wm::WM_POINTERCAPTURECHANGED {
            if let Some(f) = frames.last_mut() {
                for p in &mut f.pointers {
                    if m.msg == wm::WM_POINTERCAPTURECHANGED {
                        p.canceled = true;
                    } else if kind != PointerKind::Touch {
                        p.in_range = false;
                    }
                }
            } else if kind == PointerKind::Mouse {
                frames.push(PointerFrame {
                    time_ns: clock::uptime_ns(),
                    pointers: vec![PointerSample {
                        id,
                        in_range: false,
                        ..Default::default()
                    }],
                });
            }
        }
        // The title bar back button.
        let (_, back) = caption_geometry(w.dpi);
        let in_back = |f: &PointerFrame| {
            f.pointers.first().is_some_and(|p| {
                let cx = (p.x * scale) as i32 + content.left;
                let cy = (p.y * scale) as i32 + content.top;
                back.contains(cx, cy)
            })
        };
        let tracker_down = match kind {
            PointerKind::Mouse => w.mouse.is_down(),
            PointerKind::Touch => w.touch.is_down(),
            PointerKind::Pen => w.pen.is_down(),
        };
        if !tracker_down
            && let Some(last) = frames.last()
        {
            let over = in_back(last) && m.msg != wm::WM_POINTERLEAVE;
            let pressed = last
                .pointers
                .first()
                .is_some_and(|p| p.in_contact || p.buttons & 1 != 0);
            if over || w.back_pressed {
                let mut redraw = w.back_hover != over;
                w.back_hover = over;
                if m.msg == wm::WM_POINTERDOWN && over && pressed {
                    w.back_pressed = true;
                } else if m.msg == wm::WM_POINTERUP && w.back_pressed {
                    w.back_pressed = false;
                    if over {
                        for k in system_back(clock::uptime_ns()) {
                            deliver(s, &mut w, InputEvent::Key(k));
                        }
                    }
                    redraw = true;
                }
                if redraw {
                    w.caption = None;
                }
                s.wins.insert(m.hwnd.0, w);
                return Some(redraw);
            } else if w.back_hover {
                w.back_hover = false;
                w.caption = None;
                s.wins.insert(m.hwnd.0, w);
                return Some(true);
            }
        }
        let meta = w.keys.meta_state(win::locks());
        let tracker = match kind {
            PointerKind::Mouse => &mut w.mouse,
            PointerKind::Touch => &mut w.touch,
            PointerKind::Pen => &mut w.pen,
        };
        let events = tracker.process(&frames, meta);
        for e in events {
            deliver(s, &mut w, e);
        }
        s.wins.insert(m.hwnd.0, w);
        Some(false)
    })
    .flatten();
    if r == Some(true) {
        relayout(m.hwnd);
    }
    // Handled: no mouse emulation. Leave and capture changes go on to the
    // default handling too.
    match m.msg {
        wm::WM_POINTERLEAVE | wm::WM_POINTERCAPTURECHANGED => None,
        _ => r.map(|_| 0),
    }
}

fn wheel(m: Msg) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging as wm;
    let detents = hi_i16(m.wparam) as f32 / 120.0;
    let (sx, sy) = point_lparam(m.lparam);
    let horizontal = matches!(m.msg, wm::WM_POINTERHWHEEL | wm::WM_MOUSEHWHEEL);
    let time = clock::tick_to_uptime_ns(win::message_time());
    with_state(|s| {
        let mut w = s.wins.remove(&m.hwnd.0)?;
        let (ox, oy) = w.origin;
        let (content, scale) = w.layout.map_or((Rect::default(), 1.0), |l| (l.content, l.scale));
        let x = (sx - ox - content.left) as f32 / scale;
        let y = (sy - oy - content.top) as f32 / scale;
        let meta = w.keys.meta_state(win::locks());
        let (v, h) = if horizontal {
            (0.0, detents)
        } else {
            (detents, 0.0)
        };
        let e = w.mouse.scroll(time, x, y, v, h, meta);
        deliver(s, &mut w, InputEvent::Motion(e));
        s.wins.insert(m.hwnd.0, w);
        Some(0)
    })
    .flatten()
}

fn key_message(m: Msg) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging as wm;
    const VK_PROCESSKEY: usize = 0xE5;
    const VK_PACKET: usize = 0xE7;
    const VK_CONTROL: usize = 0x11;
    const VK_F4: usize = 0x73;
    const VK_PAUSE: usize = 0x13;
    const VK_NUMLOCK: usize = 0x90;
    let vk = m.wparam;
    let down = matches!(m.msg, wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN);
    let sys = matches!(m.msg, wm::WM_SYSKEYDOWN | wm::WM_SYSKEYUP);
    if vk == VK_PROCESSKEY || vk == VK_PACKET {
        return None;
    }
    // Alt+F4 closes the window as everywhere in Windows.
    if sys && vk == VK_F4 {
        return None;
    }
    let raw_scan = ((m.lparam >> 16) & 0xFF) as u16;
    let extended = (m.lparam >> 24) & 1 == 1;
    let time_ms = win::message_time();
    if vk == VK_CONTROL && raw_scan == 0x1D && !extended && win::next_is_altgr(time_ms) {
        return Some(0);
    }
    let scan = match vk {
        VK_PAUSE => SCAN_PAUSE,
        VK_NUMLOCK => 0x45,
        _ if extended => 0xE000 | raw_scan,
        _ => raw_scan,
    };
    let time = clock::tick_to_uptime_ns(time_ms);
    with_state(|s| {
        let mut w = s.wins.remove(&m.hwnd.0)?;
        let event = w.keys.key(time, scan, down, win::locks());
        if down {
            let character_key =
                keys::map_scan_code(scan).is_some_and(|(_, c)| keycode::kcm_name(c).is_some());
            if character_key {
                match ime::translated_unit(vk as u32, raw_scan) {
                    // A dead key: the next key's character is composed by
                    // the app's key character map.
                    Some((true, _)) => w.after_dead = true,
                    Some((false, u)) => {
                        if w.expect_chars.len() >= 16 {
                            w.expect_chars.pop_front();
                        }
                        w.expect_chars
                            .push_back(if w.after_dead { None } else { Some(u) });
                        w.after_dead = false;
                    }
                    None => {}
                }
            }
        }
        if let Some(k) = event {
            deliver(s, &mut w, InputEvent::Key(k));
        }
        s.wins.insert(m.hwnd.0, w);
        Some(())
    });
    // System keys are the app's too (Alt combinations), except the
    // window's own (Alt+F4 above, Alt+Space through WM_SYSCHAR).
    Some(0)
}

fn char_message(m: Msg) -> Option<isize> {
    let unit = m.wparam as u16;
    with_state(|s| {
        let mut w = s.wins.remove(&m.hwnd.0)?;
        if let Some(i) = w
            .expect_chars
            .iter()
            .position(|e| e.is_none_or(|e| e == unit))
        {
            // The key event carries it; the app types it through the key
            // character map.
            w.expect_chars.drain(..=i);
        } else if (0xD800..0xDC00).contains(&unit) {
            w.high_surrogate = Some(unit);
        } else {
            let units: Vec<u16> = w.high_surrogate.take().into_iter().chain([unit]).collect();
            let text = String::from_utf16_lossy(&units);
            if !text.chars().all(char::is_control) {
                deliver(s, &mut w, InputEvent::Ime(ImeEvent::Commit { text }));
            }
        }
        s.wins.insert(m.hwnd.0, w);
        Some(0)
    })
    .flatten()
}

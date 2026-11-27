//! Opens real windows on the desktop: draws through DirectComposition,
//! resizes, checks configuration changes and screenshots, and sends input
//! through Windows' input stack.
//!
//! The tests share the desktop, so they run one at a time (a lock).

use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use andruna_compositor::android::config::*;
use andruna_compositor::android::{key, keycode, motion};
use andruna_compositor::clock;
use andruna_compositor::compositor::{Compositor, Event, WindowId, WindowSpec};
use andruna_compositor::config::{self, ActivitySpec, RequestedOrientation};
use andruna_compositor::geom::{Rect, Size};
use andruna_compositor::input::InputEvent;
use andruna_compositor::surface::{BufferFormat, NativeSurface, create_device};
use andruna_compositor::sys::gfx::Image;
use andruna_compositor::sys::{inject, win};

static DESKTOP: Mutex<()> = Mutex::new(());

/// Serializes the tests and makes the test thread per-monitor DPI aware,
/// so its window measurements and injected coordinates are physical
/// pixels like the compositor's.
fn desktop() -> std::sync::MutexGuard<'static, ()> {
    let g = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    win::init_thread();
    g
}

fn wait_for<T>(
    rx: &Receiver<Event>,
    timeout: Duration,
    mut f: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    let end = Instant::now() + timeout;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => {
                if let Some(t) = f(&e) {
                    return Some(t);
                }
            }
            Err(_) => return None,
        }
    }
    None
}

fn config_of(rx: &Receiver<Event>, w: WindowId) -> Option<(config::WindowLayout, u32)> {
    wait_for(rx, Duration::from_secs(5), |e| match e {
        Event::Configuration {
            window,
            layout,
            changes,
        } if *window == w => Some((*layout, *changes)),
        _ => None,
    })
}

/// (b, g, r) at a pixel.
fn pixel(img: &Image, x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * img.width + x) * 4) as usize;
    (img.bgra[i], img.bgra[i + 1], img.bgra[i + 2])
}

fn close(c: (u8, u8, u8), want: (u8, u8, u8)) -> bool {
    let d = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 8;
    d(c.0, want.0) && d(c.1, want.1) && d(c.2, want.2)
}

/// Screenshots until the center has `want` (presents reach the screen
/// on the next composition).
fn wait_for_color(c: &Compositor, w: WindowId, at: (u32, u32), want: (u8, u8, u8)) -> (u8, u8, u8) {
    let mut last = (0, 0, 0);
    for _ in 0..50 {
        let img = c.screenshot(w).expect("screenshot");
        last = pixel(&img, at.0.min(img.width - 1), at.1.min(img.height - 1));
        if close(last, want) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    last
}

#[test]
fn draws_resizes_and_reports_configuration_changes() {
    let _g = desktop();
    let (c, rx) = Compositor::start().expect("compositor");
    let w = c
        .create_window(WindowSpec {
            title: "Andruna compositor test".into(),
            client_size: Some(Size::new(640, 480)),
            ..Default::default()
        })
        .expect("window");
    let (first, changes) = config_of(&rx, w).expect("first configuration");
    assert_eq!(changes, 0);
    let hwnd = win::Hwnd(c.native_window(w).unwrap().unwrap());
    let dpi = win::dpi(hwnd);
    assert_eq!(first.config.density_dpi, config::density_dpi(dpi));
    assert_eq!(first.config.window.bounds.size(), Size::new(640, 480));
    assert_eq!(first.caption_insets.top, config::dip_to_px(32, dpi));
    assert_eq!(first.config.orientation, ORIENTATION_LANDSCAPE);
    assert_eq!(first.config.window.windowing_mode, WINDOWING_MODE_FREEFORM);

    // A surface over the whole window, drawn red by a producer on its own
    // device through the composition surface handle.
    let (_sid, handle) = c
        .create_surface(w, 0, Rect::new(0, 0, 640, 480))
        .expect("surface");
    let device = create_device().expect("device");
    let mut s = NativeSurface::open(&device, &handle, BufferFormat::Rgba8888).expect("swap chain");
    s.clear([1.0, 0.0, 0.0, 1.0]).unwrap();
    s.present(0).unwrap();
    c.first_frame(w).unwrap();
    let got = wait_for_color(&c, w, (320, 300), (0, 0, 255));
    assert!(close(got, (0, 0, 255)), "center is {got:?}, want red");

    // Resize to portrait: screen size, orientation and window bounds
    // change.
    let t0 = Instant::now();
    c.set_client_size(w, Size::new(400, 600)).unwrap();
    let (l, changes) = config_of(&rx, w).expect("configuration change");
    eprintln!(
        "resize: request to configuration event {:.2} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(l.config.window.bounds.size(), Size::new(400, 600));
    assert_eq!(l.config.orientation, ORIENTATION_PORTRAIT);
    for bit in [
        CONFIG_SCREEN_SIZE,
        CONFIG_ORIENTATION,
        CONFIG_SMALLEST_SCREEN_SIZE,
        CONFIG_WINDOW_CONFIGURATION,
    ] {
        assert_ne!(changes & bit, 0, "changes {changes:#x} lack {bit:#x}");
    }
    assert_eq!(changes & CONFIG_DENSITY, 0);
    assert_eq!(
        l.config.screen_width_dp,
        config::px_to_dp(400, config::density_dpi(dpi))
    );

    // The producer follows with a new buffer size and color.
    let t1 = Instant::now();
    s.resize(400, 600).unwrap();
    s.clear([0.0, 1.0, 0.0, 1.0]).unwrap();
    s.present(0).unwrap();
    let got = wait_for_color(&c, w, (200, 400), (0, 255, 0));
    eprintln!(
        "resize: producer resize and present to new pixels in a screenshot {:.2} ms",
        t1.elapsed().as_secs_f64() * 1000.0
    );
    assert!(close(got, (0, 255, 0)), "center is {got:?}, want green");
    let shot = c.screenshot(w).unwrap();
    assert_eq!((shot.width, shot.height), (400, 600));

    // Too small: the minimum size (220 dp plus the caption) holds.
    c.set_client_size(w, Size::new(50, 50)).unwrap();
    let (l, _) = config_of(&rx, w).expect("configuration at minimum");
    let density = config::density_dpi(dpi);
    assert_eq!(
        l.config.window.bounds.width(),
        config::dp_to_px(220, density)
    );
    assert_eq!(
        l.config.window.bounds.height(),
        config::dp_to_px(220, density) + config::dip_to_px(32, dpi)
    );

    c.close(w).unwrap();
    assert!(
        wait_for(&rx, Duration::from_secs(5), |e| {
            matches!(e, Event::Destroyed { window } if *window == w).then_some(())
        })
        .is_some()
    );
}

#[test]
fn non_resizable_app_is_letterboxed_when_maximized() {
    let _g = desktop();
    let (c, rx) = Compositor::start().expect("compositor");
    let w = c
        .create_window(WindowSpec {
            title: "Andruna letterbox test".into(),
            activity: ActivitySpec {
                resizeable: false,
                orientation: RequestedOrientation::Portrait,
                ..Default::default()
            },
            caption_color: Some(0x1565C0),
            ..Default::default()
        })
        .expect("window");
    let (normal, _) = config_of(&rx, w).expect("first configuration");
    assert!(!normal.letterboxed);
    assert_eq!(normal.config.orientation, ORIENTATION_PORTRAIT);
    let (_sid, handle) = c
        .create_surface(
            w,
            0,
            Rect::from_size(0, 0, normal.config.window.bounds.size()),
        )
        .unwrap();
    let device = create_device().unwrap();
    let s = NativeSurface::open(&device, &handle, BufferFormat::Rgbx8888).unwrap();
    s.clear([0.0, 0.0, 1.0, 1.0]).unwrap();
    s.present(0).unwrap();

    // The caption has the app's color (DWM draws its buttons on the
    // same color).
    let caption = c.screenshot(w).unwrap();
    let px = pixel(&caption, 6, (normal.content.top - 4).max(0) as u32);
    assert!(close(px, (0xC0, 0x65, 0x15)), "caption is {px:?}");

    c.maximize(w, true).unwrap();
    let (max, changes) = config_of(&rx, w).expect("maximized configuration");
    assert!(max.letterboxed, "{max:?}");
    // Same app configuration: only the window bounds moved.
    assert_eq!(changes, CONFIG_WINDOW_CONFIGURATION);
    assert_eq!(max.config.screen_width_dp, normal.config.screen_width_dp);
    assert_eq!(max.config.screen_height_dp, normal.config.screen_height_dp);
    // Content centered horizontally.
    let hwnd = win::Hwnd(c.native_window(w).unwrap().unwrap());
    let client = win::client_size(hwnd);
    let mid = max.content.left + max.content.width() / 2;
    assert!(
        (mid - client.width / 2).abs() <= 1,
        "{:?} in {client:?}",
        max.content
    );
    // The content is blue, the side is not.
    let center = (
        (max.content.left + max.content.width() / 2) as u32,
        (max.content.top + max.content.height() / 2) as u32,
    );
    let got = wait_for_color(&c, w, center, (255, 0, 0));
    assert!(close(got, (255, 0, 0)), "content is {got:?}, want blue");
    if max.content.left > 4 {
        let img = c.screenshot(w).unwrap();
        let side = pixel(&img, 2, center.1);
        assert!(
            !close(side, (255, 0, 0)),
            "letterbox is {side:?}, stretched?"
        );
    }
    c.maximize(w, false).unwrap();
    let (back, _) = config_of(&rx, w).expect("restored");
    assert!(!back.letterboxed);
}

#[test]
fn injected_input_reaches_the_app_with_android_semantics() {
    let _g = desktop();
    let (c, rx) = Compositor::start().expect("compositor");
    let w = c
        .create_window(WindowSpec {
            title: "Andruna input test".into(),
            client_size: Some(Size::new(500, 400)),
            ..Default::default()
        })
        .expect("window");
    let (layout, _) = config_of(&rx, w).expect("configuration");
    let hwnd = win::Hwnd(c.native_window(w).unwrap().unwrap());
    let (ox, oy) = win::client_origin(hwnd);

    // Keyboard: only when Windows let the window take the foreground (an
    // interactive desktop).
    let input_events = |rx: &Receiver<Event>, n: usize| {
        let mut v = Vec::new();
        wait_for(rx, Duration::from_secs(3), |e| {
            if let Event::Input {
                event,
                delivered_ns,
                ..
            } = e
            {
                v.push((event.clone(), *delivered_ns));
            }
            (v.len() >= n).then_some(())
        });
        v
    };
    // Keyboard messages through the window's own message path (posted,
    // then translated by the message loop): the key's character (from
    // whatever layout is active, 'ש' with the Hebrew one) is not sent as
    // text, since the app gets it from KEYCODE_A through the key
    // character map; a character with no key (an emoji from the emoji
    // panel, a surrogate pair) is committed text.
    const WM_KEYDOWN: u32 = 0x100;
    const WM_KEYUP: u32 = 0x101;
    const WM_CHAR: u32 = 0x102;
    const WM_SYSKEYDOWN: u32 = 0x104;
    const WM_SYSKEYUP: u32 = 0x105;
    let post = |m: u32, wp: usize, lp: isize| inject::post(hwnd, m, wp, lp).unwrap();
    let posted = clock::uptime_ns();
    post(WM_KEYDOWN, 0x41, inject::key_lparam(0x1E, true));

    post(WM_KEYUP, 0x41, inject::key_lparam(0x1E, false));
    post(WM_CHAR, 0xD83D, 1);
    post(WM_CHAR, 0xDE00, 1);
    // Alt+Left is Back.
    post(
        WM_SYSKEYDOWN,
        0x12,
        inject::key_lparam(0x38, true) | 1 << 29,
    );
    post(
        WM_SYSKEYDOWN,
        0x25,
        inject::key_lparam(0xE04B, true) | 1 << 29,
    );
    post(
        WM_SYSKEYUP,
        0x25,
        inject::key_lparam(0xE04B, false) | 1 << 29,
    );
    post(WM_KEYUP, 0x12, inject::key_lparam(0x38, false));
    let mut got = Vec::new();
    let mut first_delivery = None;
    wait_for(&rx, Duration::from_secs(3), |e| {
        if let Event::Input {
            event,
            delivered_ns,
            ..
        } = e
            && !matches!(event, InputEvent::Focus { .. })
        {
            first_delivery.get_or_insert(*delivered_ns);
            got.push(event.clone());
        }
        (got.len() >= 7).then_some(())
    });
    let summary: Vec<String> = got
        .iter()
        .map(|e| match e {
            InputEvent::Key(k) => format!("key {} {}", k.action, k.key_code),
            InputEvent::Ime(andruna_compositor::input::ImeEvent::Commit { text }) => {
                format!("commit {text}")
            }
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        summary,
        [
            format!("key 0 {}", keycode::A),
            format!("key 1 {}", keycode::A),
            "commit 😀".to_string(),
            format!("key 0 {}", keycode::ALT_LEFT),
            format!("key 0 {}", keycode::BACK),
            format!("key 1 {}", keycode::BACK),
            format!("key 1 {}", keycode::ALT_LEFT),
        ]
    );
    if let InputEvent::Key(k) = &got[4] {
        assert_eq!(k.flags & key::FLAG_FROM_SYSTEM, key::FLAG_FROM_SYSTEM);
    }
    eprintln!(
        "keyboard (posted): post to compositor event {:.3} ms",
        (first_delivery.unwrap() - posted) as f64 / 1e6
    );

    // Through Windows' input stack: only when Windows lets the window take
    // the foreground (an interactive desktop where the test owns input).
    std::thread::sleep(Duration::from_millis(200));
    let sent = clock::uptime_ns();
    inject::key(0x1E, true).unwrap();
    inject::key(0x1E, false).unwrap();
    let keys: Vec<_> = input_events(&rx, 2)
        .into_iter()
        .filter_map(|(e, d)| match e {
            InputEvent::Key(k) => Some((k, d)),
            _ => None,
        })
        .collect();
    if keys.is_empty() {
        eprintln!("keyboard: the test window is not in the foreground; skipped");
    } else {
        assert_eq!(keys[0].0.key_code, keycode::A);
        assert_eq!(keys[0].0.action, key::ACTION_DOWN);
        assert_eq!(keys[0].0.scan_code, 30);
        assert!(keys[0].0.event_time_ns >= sent - 20_000_000);
        eprintln!(
            "keyboard: SendInput to compositor event {:.2} ms",
            (keys[0].1 - sent) as f64 / 1e6
        );
    }

    // Touch through a synthetic touch screen: down, move, up.
    match inject::Touch::new(2) {
        Err(e) => eprintln!("touch injection unavailable: {e}; skipped"),
        Ok(t) => {
            let (x, y) = (ox + 250, oy + 200);
            let t0 = clock::uptime_ns();
            let injected = t
                .frame(&[(1, x, y, inject::Phase::Down)])
                .and_then(|_| t.frame(&[(1, x + 10, y + 5, inject::Phase::Move)]))
                .and_then(|_| t.frame(&[(1, x + 10, y + 5, inject::Phase::Up)]));
            if let Err(e) = injected {
                eprintln!("touch: injection refused ({e}): not the foreground process; skipped");
                return;
            }
            let ev: Vec<_> = input_events(&rx, 3)
                .into_iter()
                .filter_map(|(e, d)| match e {
                    InputEvent::Motion(m) => Some((m, d)),
                    _ => None,
                })
                .collect();
            if ev.is_empty() {
                eprintln!("touch: no events (no interactive desktop); skipped");
            } else {
                let actions: Vec<_> = ev.iter().map(|(m, _)| m.action).collect();
                assert_eq!(actions.first(), Some(&motion::ACTION_DOWN), "{actions:?}");
                assert_eq!(actions.last(), Some(&motion::ACTION_UP), "{actions:?}");
                let down = &ev[0].0;
                assert_eq!(
                    down.source,
                    andruna_compositor::android::source::TOUCHSCREEN
                );
                assert_eq!(down.pointers[0].tool_type, motion::TOOL_TYPE_FINGER);
                let p = &down.current()[0];
                assert!(
                    (p.x() - (250 - layout.content.left) as f32).abs() <= 1.0,
                    "{p:?}"
                );
                assert!(
                    (p.y() - (200 - layout.content.top) as f32).abs() <= 1.0,
                    "{p:?}"
                );
                assert!((p.get(motion::AXIS_PRESSURE) - 0.5).abs() < 0.01);
                eprintln!(
                    "touch: injection to compositor event {:.2} ms; event time to delivery {:.2} ms",
                    (ev[0].1 - t0) as f64 / 1e6,
                    (ev[0].1 - down.event_time_ns()) as f64 / 1e6
                );
            }
        }
    }
}

#[test]
fn vsync_follows_the_compositor_clock() {
    let _g = desktop();
    let v = andruna_compositor::vsync::VsyncSource::start().unwrap();
    let rx = v.subscribe();
    match andruna_compositor::vsync::measure(&rx, 60) {
        None => eprintln!("vsync: no compositor clock ticks (no display); skipped"),
        Some((period, events)) => {
            let ms = period.as_secs_f64() * 1000.0;
            eprintln!(
                "vsync: mean period {ms:.3} ms over 60 ticks, reported {:.3} ms, displays {:?}",
                events[0].period_ns as f64 / 1e6,
                events[0].displays
            );
            // Between 30 Hz and 500 Hz.
            assert!((2.0..34.0).contains(&ms), "{ms}");
            assert!(events.windows(2).all(|p| p[1].frame_id > p[0].frame_id));
            assert!(
                events
                    .windows(2)
                    .all(|p| p[1].timestamp_ns > p[0].timestamp_ns)
            );
            let now = clock::uptime_ns();
            let last = events.last().unwrap().timestamp_ns;
            assert!(
                (now - last).abs() < 1_000_000_000,
                "timestamps on the uptime clock"
            );
        }
    }
}

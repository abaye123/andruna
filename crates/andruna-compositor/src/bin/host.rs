//! `andruna-compositor-host`: a test host for the compositor.
//!
//! Opens a task window whose single surface a producer in this process
//! fills with a color, and prints every compositor event as one logfmt
//! line: configuration changes, input (with its dispatch latency), IME,
//! keyboard layout. Pointer presses change the color, so input and
//! presentation can be checked by eye.
//!
//! ```text
//! andruna-compositor-host [--title T] [--size WxH] [--color RRGGBB]
//!     [--caption-color RRGGBB] [--fixed] [--splash] [--vsync]
//!     [--seconds N] [--screenshot file.bmp]
//! ```

use std::time::{Duration, Instant};

use andruna_compositor::compositor::{Compositor, Event, Splash, WindowSpec};
use andruna_compositor::config::{ActivitySpec, RequestedOrientation};
use andruna_compositor::geom::{Rect, Size};
use andruna_compositor::input::{ImeEvent, InputEvent};
use andruna_compositor::surface::{BufferFormat, NativeSurface, create_device};
use andruna_compositor::sys::gfx::Image;
use andruna_compositor::{clock, vsync};

struct Args {
    title: String,
    size: Option<Size>,
    color: u32,
    caption: Option<u32>,
    fixed: bool,
    splash: bool,
    vsync: bool,
    seconds: Option<u64>,
    screenshot: Option<String>,
}

fn hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s.trim_start_matches('#'), 16).map_err(|_| format!("bad color `{s}`"))
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        title: "Andruna compositor host".into(),
        size: None,
        color: 0x2E7D32,
        caption: None,
        fixed: false,
        splash: false,
        vsync: false,
        seconds: None,
        screenshot: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut v = || it.next().ok_or(format!("`{x}` needs a value"));
        match x.as_str() {
            "--title" => a.title = v()?,
            "--size" => {
                let s = v()?;
                let (w, h) = s.split_once('x').ok_or("size is WxH")?;
                a.size = Some(Size::new(
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                ));
            }
            "--color" => a.color = hex(&v()?)?,
            "--caption-color" => a.caption = Some(hex(&v()?)?),
            "--fixed" => a.fixed = true,
            "--splash" => a.splash = true,
            "--vsync" => a.vsync = true,
            "--seconds" => a.seconds = Some(v()?.parse().map_err(|_| "bad seconds")?),
            "--screenshot" => a.screenshot = Some(v()?),
            o => return Err(format!("unexpected argument `{o}`")),
        }
    }
    Ok(a)
}

fn rgba(c: u32) -> [f32; 4] {
    let f = |s: u32| ((c >> s) & 0xFF) as f32 / 255.0;
    [f(16), f(8), f(0), 1.0]
}

/// A round icon for the splash: the color on transparent.
fn icon(color: u32) -> Image {
    let n = 192u32;
    let mut bgra = Vec::with_capacity((n * n * 4) as usize);
    let r = n as f32 / 2.0;
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 + 0.5 - r).powi(2) + (y as f32 + 0.5 - r).powi(2)).sqrt();
            let a = (r - d).clamp(0.0, 1.0);
            let ch = |s: u32| (((color >> s) & 0xFF) as f32 * a) as u8;
            bgra.extend_from_slice(&[ch(0), ch(8), ch(16), (a * 255.0) as u8]);
        }
    }
    Image {
        width: n,
        height: n,
        bgra,
    }
}

fn save_bmp(path: &str, img: &Image) -> std::io::Result<()> {
    let data = img.bgra.len() as u32;
    let mut b = Vec::with_capacity(54 + data as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(54 + data).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(img.width as i32).to_le_bytes());
    b.extend_from_slice(&(-(img.height as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    b.extend_from_slice(&img.bgra);
    std::fs::write(path, b)
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("andruna-compositor-host: {e}");
            std::process::exit(2);
        }
    };
    andruna_compositor::sys::win::init_thread();
    let (c, rx) = match Compositor::start() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("andruna-compositor-host: {e}");
            std::process::exit(1);
        }
    };
    println!("event=started software={}", c.software);
    let activity = ActivitySpec {
        resizeable: !args.fixed,
        orientation: if args.fixed {
            RequestedOrientation::Portrait
        } else {
            RequestedOrientation::Unspecified
        },
        ..Default::default()
    };
    let w = c
        .create_window(WindowSpec {
            title: args.title.clone(),
            activity,
            caption_color: args.caption,
            splash: args.splash.then(|| Splash {
                icon: Some(icon(args.color)),
                background: None,
            }),
            client_size: args.size,
            no_activate: false,
        })
        .expect("window");

    let device = create_device().expect("device");
    let mut surface: Option<NativeSurface> = None;
    let mut color = args.color;
    let mut presses = 0u32;
    let started = Instant::now();
    let vs = args
        .vsync
        .then(|| vsync::VsyncSource::start().expect("vsync"));
    let vrx = vs.as_ref().map(|v| v.subscribe());
    let mut shot_due = args.screenshot.clone();
    let mut latencies: Vec<i64> = Vec::new();

    loop {
        if let Some(s) = args.seconds
            && started.elapsed() > Duration::from_secs(s)
        {
            break;
        }
        if let Some(v) = &vrx {
            while let Ok(e) = v.try_recv() {
                if e.frame_id % 120 == 0 {
                    println!(
                        "event=vsync frame={} period_ms={:.3} displays={}",
                        e.frame_id,
                        e.period_ns as f64 / 1e6,
                        e.displays.len()
                    );
                }
            }
        }
        let e = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(e) => e,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        match e {
            Event::Configuration {
                layout, changes, ..
            } => {
                let cfg = &layout.config;
                println!(
                    "event=configuration changes={changes:#x} density={} size_dp={}x{} sw_dp={} orientation={} bounds={:?} content={:?} caption={} letterboxed={} scale={:.3}",
                    cfg.density_dpi,
                    cfg.screen_width_dp,
                    cfg.screen_height_dp,
                    cfg.smallest_screen_width_dp,
                    cfg.orientation,
                    cfg.window.bounds,
                    layout.content,
                    layout.caption_insets.top,
                    layout.letterboxed,
                    layout.scale
                );
                let size = cfg.window.bounds.size();
                match &mut surface {
                    None => {
                        let (_, h) = c
                            .create_surface(w, 0, Rect::from_size(0, 0, size))
                            .expect("surface");
                        let s = NativeSurface::open(&device, &h, BufferFormat::Rgba8888)
                            .expect("swap chain");
                        s.clear(rgba(color)).unwrap();
                        s.present(0).unwrap();
                        surface = Some(s);
                        // Let the splash show briefly, as a cold start would.
                        if args.splash {
                            std::thread::sleep(Duration::from_millis(600));
                        }
                        c.first_frame(w).unwrap();
                    }
                    Some(s) => {
                        if s.width() != size.width as u32 || s.height() != size.height as u32 {
                            s.resize(size.width as u32, size.height as u32).unwrap();
                        }
                        s.clear(rgba(color)).unwrap();
                        s.present(0).unwrap();
                    }
                }
                if let Some(p) = shot_due.take() {
                    std::thread::sleep(Duration::from_millis(300));
                    // `--screenshot x.bmp` takes the window as composed
                    // (PrintWindow); a path ending in `.screen.bmp` takes
                    // the screen, with DWM's frame and caption buttons.
                    let shot = if p.ends_with(".screen.bmp") {
                        c.native_window(w).and_then(|h| {
                            let h = andruna_compositor::sys::win::Hwnd(h.unwrap_or(0));
                            andruna_compositor::sys::win::set_topmost(h, true);
                            std::thread::sleep(Duration::from_millis(300));
                            andruna_compositor::sys::capture::screen(
                                andruna_compositor::sys::win::window_rect(h),
                            )
                        })
                    } else {
                        c.screenshot(w)
                    };
                    match shot.and_then(|i| save_bmp(&p, &i)) {
                        Ok(()) => println!("event=screenshot path={p}"),
                        Err(e) => println!("event=screenshot_failed error=\"{e}\""),
                    }
                }
            }
            Event::Input {
                event,
                delivered_ns,
                ..
            } => match &event {
                InputEvent::Motion(m) => {
                    let lat = delivered_ns - m.event_time_ns();
                    latencies.push(lat);
                    let p = &m.current()[0];
                    println!(
                        "event=motion action={:#x} source={:#x} pointers={} x={:.1} y={:.1} buttons={:#x} samples={} latency_us={}",
                        m.action,
                        m.source,
                        m.pointers.len(),
                        p.x(),
                        p.y(),
                        m.button_state,
                        m.samples.len(),
                        lat / 1000
                    );
                    if m.action_masked() == andruna_compositor::android::motion::ACTION_DOWN {
                        presses += 1;
                        color = [0x2E7D32, 0x1565C0, 0xC62828, 0x6A1B9A][presses as usize % 4];
                        if let Some(s) = &surface {
                            s.clear(rgba(color)).unwrap();
                            s.present(0).unwrap();
                        }
                    }
                }
                InputEvent::Key(k) => {
                    let lat = delivered_ns - k.event_time_ns;
                    latencies.push(lat);
                    println!(
                        "event=key action={} code={} scan={} meta={:#x} repeat={} flags={:#x} latency_us={}",
                        k.action,
                        k.key_code,
                        k.scan_code,
                        k.meta_state,
                        k.repeat_count,
                        k.flags,
                        lat / 1000
                    );
                }
                InputEvent::Ime(ImeEvent::Commit { text }) => {
                    println!("event=ime_commit text={text:?}")
                }
                InputEvent::Ime(ImeEvent::Composing {
                    text,
                    cursor,
                    target,
                }) => {
                    println!("event=ime_composing text={text:?} cursor={cursor} target={target:?}")
                }
                InputEvent::Ime(ImeEvent::FinishComposing) => println!("event=ime_finish"),
                InputEvent::Focus { has_focus } => println!("event=focus has_focus={has_focus}"),
            },
            Event::KeyboardLayout { layout, kcm } => {
                println!(
                    "event=keyboard_layout hkl={layout:#x} kcm_keys={}",
                    kcm.matches("\nkey ").count()
                );
            }
            Event::Display { target, dpi, .. } => {
                println!("event=display dpi={dpi} target={target:?}")
            }
            Event::CloseRequested { window } => {
                println!("event=close_requested");
                c.close(window).unwrap();
            }
            Event::Destroyed { .. } => {
                println!("event=destroyed");
                break;
            }
            Event::SurfacePresented {
                surface,
                width,
                height,
                bottom_up,
                ..
            } => {
                println!(
                    "event=surface_presented surface={} size={width}x{height} bottom_up={bottom_up}",
                    surface.0
                )
            }
        }
    }
    if !latencies.is_empty() {
        latencies.sort();
        let p = |q: f64| latencies[((latencies.len() - 1) as f64 * q) as usize] as f64 / 1000.0;
        println!(
            "event=latency count={} p50_us={:.0} p95_us={:.0} max_us={:.0}",
            latencies.len(),
            p(0.5),
            p(0.95),
            p(1.0)
        );
    }
    let _ = clock::uptime_ns();
}

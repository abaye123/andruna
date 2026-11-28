//! GL in an app process, presented by the compositor: this test is the
//! compositor host (what sysd's activity manager will do), `andruna-app`
//! is the app process.
//!
//! The host opens a task window, starts `andruna-app --compositor
//! <handles> --exec gles_window` with the window channel's handles
//! inherited, creates a surface over the window's content and hands it to
//! the app process (the composition surface handle is duplicated into it).
//! The guest renders with OpenGL ES through ANGLE: top half red, bottom
//! half blue. The host checks a screenshot (`PrintWindow` with full
//! content, which works while the session is locked) for upright content,
//! resizes the window, forwards the configuration change as a surface
//! resize, checks that the producer resized its buffers (top half now
//! green, new size) and closes the window; the app exits with 0.
//!
//! Inputs: ANGLE's DLLs (`ANDRUNA_ANGLE_DIR`, default
//! `out/angle/<arch>/bin`) and the guest GLES build (`ANDRUNA_GLES_GUEST`,
//! default `out/gles/<arch>/guest`, from andruna-gles's
//! `tools/build_guest.py`). The test reports that they are missing and
//! passes, unless `ANDRUNA_REQUIRE_GLES=1`. `ANDRUNA_GLES_DEVICE=warp` runs
//! the app's GL on WARP (CI).

use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use andruna_compositor::channel::{DEFAULT_CAPACITY, InputChannel, InputPublisher};
use andruna_compositor::compositor::{Compositor, Event, SurfaceId, WindowId, WindowSpec};
use andruna_compositor::config::WindowLayout;
use andruna_compositor::geom::{Rect, Size};
use andruna_compositor::sys::gfx::Image;
use andruna_compositor::sys::win;

/// The tests share the desktop (window positions, screenshots).
static DESKTOP: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dir_var(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(default.replace("<arch>", arch())))
}

/// (ANGLE directory, guest GLES directory), or None when not built.
fn inputs() -> Option<(PathBuf, PathBuf)> {
    let angle = dir_var("ANDRUNA_ANGLE_DIR", "out/angle/<arch>/bin");
    let guest = dir_var("ANDRUNA_GLES_GUEST", "out/gles/<arch>/guest");
    if angle.join("libGLESv2.dll").is_file() && guest.join("gles_window").is_file() {
        return Some((angle, guest));
    }
    if std::env::var_os("ANDRUNA_REQUIRE_GLES").is_some() {
        panic!(
            "ANGLE ({}) or the guest GLES build ({}) is missing",
            angle.display(),
            guest.display()
        );
    }
    eprintln!(
        "skipped: ANGLE ({}) or gles_window ({}) not built",
        angle.display(),
        guest.display()
    );
    None
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

fn configuration(rx: &Receiver<Event>, w: WindowId) -> Option<(WindowLayout, u32)> {
    wait_for(rx, Duration::from_secs(10), |e| match e {
        Event::Configuration {
            window,
            layout,
            changes,
        } if *window == w => Some((*layout, *changes)),
        _ => None,
    })
}

fn presented(rx: &Receiver<Event>, w: WindowId, timeout: Duration) -> Option<(u32, u32, bool)> {
    wait_for(rx, timeout, |e| match e {
        Event::SurfacePresented {
            window,
            width,
            height,
            bottom_up,
            ..
        } if *window == w => Some((*width, *height, *bottom_up)),
        _ => None,
    })
}

/// (r, g, b) at a pixel.
fn rgb(img: &Image, x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * img.width + x) * 4) as usize;
    (img.bgra[i + 2], img.bgra[i + 1], img.bgra[i])
}

fn near(c: (u8, u8, u8), want: (u8, u8, u8)) -> bool {
    let d = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 8;
    d(c.0, want.0) && d(c.1, want.1) && d(c.2, want.2)
}

/// The guest's picture: the top half's color at the middle of 30% of the
/// height (below the caption), the bottom half's at 10% of the width and
/// 75% of the height, and its yellow quad (a shader draw) at the middle of
/// 75% of the height.
fn probes(img: &Image) -> [(u8, u8, u8); 3] {
    let (w, h) = (img.width, img.height);
    [
        rgb(img, w / 2, h * 3 / 10),
        rgb(img, w / 10, h * 3 / 4),
        rgb(img, w / 2, h * 3 / 4),
    ]
}

const YELLOW: (u8, u8, u8) = (255, 255, 0);

/// Screenshots until the probes show `top`, `bottom` and the yellow quad;
/// returns the last screenshot and whether they matched.
fn wait_for_picture(
    c: &Compositor,
    w: WindowId,
    top: (u8, u8, u8),
    bottom: (u8, u8, u8),
) -> (Image, bool) {
    let mut last = None;
    for _ in 0..150 {
        let img = c.screenshot(w).expect("screenshot");
        let [t, b, q] = probes(&img);
        let ok = near(t, top) && near(b, bottom) && near(q, YELLOW);
        last = Some(img);
        if ok {
            return (last.unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (last.unwrap(), false)
}

fn describe(img: &Image) -> String {
    let [t, b, q] = probes(img);
    format!(
        "{}x{}, top {t:?}, bottom {b:?}, quad {q:?}",
        img.width, img.height
    )
}

fn content_size(l: &WindowLayout) -> (u32, u32) {
    let b = l.config.window.app_bounds;
    (b.width().max(1) as u32, b.height().max(1) as u32)
}

/// One app process session: window, app, surface, checks, resize, close.
/// `app_args` select the program (after `--data` and `--compositor`).
fn session(name: &str, angle: &Path, guest: &Path, app_args: &[std::ffi::OsString]) {
    let _g = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    win::init_thread();
    let (c, rx) = Compositor::start().expect("compositor");
    let w = c
        .create_window(WindowSpec {
            title: format!("Andruna GL app test ({name})"),
            client_size: Some(Size::new(640, 480)),
            ..Default::default()
        })
        .expect("window");
    let (first, _) = configuration(&rx, w).expect("first configuration");

    // The window channel; the app gets its end as inherited handles.
    let (comp_end, app_end) = InputChannel::pair(DEFAULT_CAPACITY).expect("channel");
    let inherited = app_end.export_inheritable().expect("inheritable handles");
    let data = std::env::temp_dir().join(format!("andruna-app-gl-{name}-{}", std::process::id()));
    let t_spawn = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_andruna-app"))
        .arg("--data")
        .arg(&data)
        .arg("--compositor")
        .arg(inherited.values.to_arg())
        .args(app_args)
        .env("ANDRUNA_ANGLE_DIR", angle)
        .env("ANDRUNA_GLES_GUEST", guest)
        .env("ANDRUNA_GLES_STATS", "1")
        .env("ANDRUNA_PROFILE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn andruna-app");
    // Read the app's output as it comes (a full pipe would block it).
    let drain = |r: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let mut r = std::io::BufReader::new(r);
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
            s
        })
    };
    let out_t = drain(Box::new(child.stdout.take().unwrap()));
    let err_t = drain(Box::new(child.stderr.take().unwrap()));
    drop(inherited);
    drop(app_end);
    c.attach_input(w, InputPublisher::new(comp_end)).unwrap();

    // A surface over the content, handed to the app process.
    let (cw, ch) = content_size(&first);
    let (sid, handle): (SurfaceId, _) = c
        .create_surface(w, 0, Rect::new(0, 0, cw as i32, ch as i32))
        .expect("surface");
    // SAFETY: the handle value is this process's copy, owned by the test.
    drop(unsafe { andruna_compositor::sys::shm::Handle::from_raw(handle.handle) });
    c.offer_surface(w, sid, Some(child.as_raw_handle() as usize))
        .expect("offer the surface");

    let first_frame = presented(&rx, w, Duration::from_secs(60));
    let first_ms = t_spawn.elapsed().as_secs_f64() * 1000.0;
    let Some((fw, fh, bottom_up)) = first_frame else {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "no frame from the app process; stdout:\n{}\nstderr:\n{}",
            out_t.join().unwrap(),
            err_t.join().unwrap()
        );
    };
    eprintln!("[{name}] first frame: {first_ms:.1} ms after spawning the app process ({fw}x{fh})");
    assert_eq!((fw, fh, bottom_up), (cw, ch, true));

    // Upright: red on top, blue below with the yellow quad.
    let (img, ok) = wait_for_picture(&c, w, (255, 0, 0), (0, 0, 255));
    assert!(ok, "want red over blue, screenshot {}", describe(&img));

    // Resize: a configuration change, forwarded to the producer.
    let t_resize = Instant::now();
    c.set_client_size(w, Size::new(400, 600)).unwrap();
    let (l, changes) = configuration(&rx, w).expect("configuration change");
    assert_ne!(changes, 0);
    let (nw, nh) = content_size(&l);
    assert_eq!((nw, nh), (400, 600));
    c.resize_surface(w, sid, nw, nh).unwrap();
    let after = presented(&rx, w, Duration::from_secs(10)).expect("present after the resize");
    eprintln!(
        "[{name}] resize: request to the producer's first present at the new size {:.1} ms",
        t_resize.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(after, (nw, nh, true));
    let (img, ok) = wait_for_picture(&c, w, (0, 255, 0), (0, 0, 255));
    assert!(ok, "want green over blue, screenshot {}", describe(&img));
    assert_eq!((img.width, img.height), (400, 600));

    // The app keeps rendering at swap interval 1 while the compositor
    // clock is measured over the same time: both are DWM's composition.
    let hold = std::env::var("ANDRUNA_GL_TEST_HOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000u64);
    let vs = andruna_compositor::vsync::VsyncSource::start().expect("vsync source");
    let ticks = vs.subscribe();
    let t_hold = Instant::now();
    let mut stamps = vec![];
    while t_hold.elapsed() < Duration::from_millis(hold) {
        if let Ok(e) = ticks.recv_timeout(Duration::from_millis(100)) {
            stamps.push((e.timestamp_ns, e.period_ns));
        }
    }
    drop(ticks);
    drop(vs);
    if stamps.len() > 2 {
        let span = stamps.last().unwrap().0 - stamps[0].0;
        eprintln!(
            "[{name}] compositor clock over {hold} ms: {} ticks, mean interval {:.2} ms, reported period {:.2} ms",
            stamps.len(),
            span as f64 / (stamps.len() - 1) as f64 / 1e6,
            stamps.last().unwrap().1 as f64 / 1e6
        );
    }

    // Closing the window detaches the surface; the app ends.
    c.close(w).unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if Instant::now() > end {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let stdout = out_t.join().unwrap();
    let stderr = err_t.join().unwrap();
    for line in stdout.lines().chain(
        stderr
            .lines()
            .filter(|l| l.contains("frames, interval") || l.contains("[andruna-profile]")),
    ) {
        eprintln!("[{name}] app: {line}");
    }
    let code = status.and_then(|s| s.code());
    assert_eq!(code, Some(0), "app exit {code:?}; stderr:\n{stderr}");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn native_gl_program_in_an_app_process_is_presented_by_the_compositor() {
    let Some((angle, guest)) = inputs() else {
        return;
    };
    let args = ["--exec".into(), guest.join("gles_window").into_os_string()];
    session("native", &angle, &guest, &args);
}

/// The Java program (`GlMain` and its JNI library, from
/// `tools/build_gl_java.py`) and the ART root, or None when not built.
fn java_inputs() -> Option<(PathBuf, PathBuf)> {
    let root = dir_var("ANDRUNA_ART_ROOT", "out/guest/<arch>/art-root");
    let gl = dir_var("ANDRUNA_GL_JAVA", "out/app/<arch>/gl");
    if root.join("lib64/libart.so").is_file()
        && gl.join("gljava.dex").is_file()
        && gl.join("libgljni.so").is_file()
    {
        return Some((root, gl));
    }
    if std::env::var_os("ANDRUNA_REQUIRE_GUEST").is_some() {
        panic!(
            "the ART root ({}) or the Java GL program ({}) is missing",
            root.display(),
            gl.display()
        );
    }
    eprintln!(
        "skipped: the ART root ({}) or the Java GL program ({}) not built",
        root.display(),
        gl.display()
    );
    None
}

#[test]
fn java_gl_program_in_an_app_process_is_presented_by_the_compositor() {
    let Some((angle, guest)) = inputs() else {
        return;
    };
    let Some((root, gl)) = java_inputs() else {
        return;
    };
    // The dex file's directory is /data/app/cp0, its JNI library next to it.
    let args = [
        "--art-root".into(),
        root.into_os_string(),
        "--classpath".into(),
        gl.join("gljava.dex").into_os_string(),
        "GlMain".into(),
        "/data/app/cp0/libgljni.so".into(),
    ];
    session("java", &angle, &guest, &args);
}

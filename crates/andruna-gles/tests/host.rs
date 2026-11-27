//! Tests of the host side against ANGLE's DLLs, through the same addresses
//! guest code gets (`andruna_gles::resolve`, guest convention).
//!
//! They need `cargo xtask angle-build` (ANGLE in `out/angle/<arch>/bin`, or
//! `ANDRUNA_ANGLE_DIR`). Without ANGLE they skip, unless
//! `ANDRUNA_REQUIRE_GLES=1`. They run on WARP (Direct3D 11's software
//! rasterizer), so they need no GPU.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Once};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

/// A guest-convention function pointer type.
#[cfg(target_arch = "x86_64")]
macro_rules! g {
    (fn($($t:ty),*) $(-> $r:ty)?) => { unsafe extern "sysv64" fn($($t),*) $(-> $r)? };
}
#[cfg(not(target_arch = "x86_64"))]
macro_rules! g {
    (fn($($t:ty),*) $(-> $r:ty)?) => { unsafe extern "C" fn($($t),*) $(-> $r)? };
}

/// The guest address of `name` as a typed function.
macro_rules! f {
    ($name:literal, $($ty:tt)*) => {{
        let a = andruna_gles::resolve($name).unwrap_or_else(|| panic!("{} unresolved", $name));
        // SAFETY: `resolve` returns a function with the guest convention
        // and the Khronos prototype the caller spells.
        unsafe { core::mem::transmute::<usize, g!($($ty)*)>(a) }
    }};
}

const EGL_NONE: i32 = 0x3038;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_SCISSOR_TEST: u32 = 0x0C11;

fn angle_dir() -> Option<PathBuf> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    };
    let d = std::env::var_os("ANDRUNA_ANGLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../out/angle")
                .join(arch)
                .join("bin")
        });
    d.join("libGLESv2.dll").is_file().then_some(d)
}

/// Loads ANGLE on WARP; false when the test should skip.
fn setup() -> bool {
    static INIT: Once = Once::new();
    let Some(d) = angle_dir() else {
        assert!(
            std::env::var_os("ANDRUNA_REQUIRE_GLES").is_none(),
            "ANGLE not built (cargo xtask angle-build) and ANDRUNA_REQUIRE_GLES is set"
        );
        eprintln!("skipped: ANGLE not built");
        return false;
    };
    INIT.call_once(|| {
        // SAFETY: set before any thread reads them (once, first).
        unsafe {
            std::env::set_var("ANDRUNA_ANGLE_DIR", &d);
            std::env::set_var("ANDRUNA_GLES_DEVICE", "warp");
        }
    });
    andruna_gles::angle::load().expect("ANGLE loads");
    true
}

struct Egl {
    dpy: usize,
    cfg: usize,
}

fn display() -> Egl {
    let get_display = f!("eglGetDisplay", fn(usize) -> usize);
    let initialize = f!("eglInitialize", fn(usize, *mut i32, *mut i32) -> u32);
    let choose = f!(
        "eglChooseConfig",
        fn(usize, *const i32, *mut usize, i32, *mut i32) -> u32
    );
    // SAFETY: EGL calls with valid arguments.
    unsafe {
        let dpy = get_display(0);
        assert_ne!(dpy, 0);
        let (mut ma, mut mi) = (0, 0);
        assert_eq!(initialize(dpy, &mut ma, &mut mi), 1);
        let want = [
            0x3024,
            8,
            0x3023,
            8,
            0x3022,
            8,
            0x3021,
            8, // RGBA8
            0x3033,
            0x0001 | 0x0004, // pbuffer and window surfaces
            0x3040,
            0x0040, // ES 3
            EGL_NONE,
        ];
        let (mut cfg, mut n) = (0usize, 0);
        assert_eq!(choose(dpy, want.as_ptr(), &mut cfg, 1, &mut n), 1);
        assert_eq!(n, 1);
        Egl { dpy, cfg }
    }
}

fn context(e: &Egl) -> usize {
    let create = f!(
        "eglCreateContext",
        fn(usize, usize, usize, *const i32) -> usize
    );
    let attrs = [0x3098, 3, EGL_NONE];
    // SAFETY: EGL call with a terminated list.
    let c = unsafe { create(e.dpy, e.cfg, 0, attrs.as_ptr()) };
    assert_ne!(c, 0);
    c
}

fn read_pixel(x: i32, y: i32) -> [u8; 4] {
    let read = f!(
        "glReadPixels",
        fn(i32, i32, i32, i32, u32, u32, *mut c_void)
    );
    let mut p = [0u8; 4];
    // SAFETY: reads one RGBA pixel into a local.
    unsafe { read(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p.as_mut_ptr().cast()) };
    p
}

fn clear(r: f32, g: f32, b: f32) {
    let color = f!("glClearColor", fn(f32, f32, f32, f32));
    let clear = f!("glClear", fn(u32));
    // SAFETY: GL calls on the current context.
    unsafe {
        color(r, g, b, 1.0);
        clear(GL_COLOR_BUFFER_BIT);
    }
}

/// Every guest thread has its own current context and surface (ANGLE's
/// per-thread state is the guest thread's, one Windows thread each).
#[test]
fn current_context_is_per_thread() {
    if !setup() {
        return;
    }
    let e = Arc::new(display());
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = [(255u8, 0u8), (0, 255)]
        .into_iter()
        .map(|(r, g)| {
            let (e, barrier) = (e.clone(), barrier.clone());
            std::thread::spawn(move || {
                let pbuffer = f!(
                    "eglCreatePbufferSurface",
                    fn(usize, usize, *const i32) -> usize
                );
                let make_current = f!("eglMakeCurrent", fn(usize, usize, usize, usize) -> u32);
                let current = f!("eglGetCurrentContext", fn() -> usize);
                let attrs = [0x3057, 16, 0x3056, 16, EGL_NONE];
                // SAFETY: EGL calls with valid arguments.
                let s = unsafe { pbuffer(e.dpy, e.cfg, attrs.as_ptr()) };
                assert_ne!(s, 0);
                let c = context(&e);
                // SAFETY: as above.
                assert_eq!(unsafe { make_current(e.dpy, s, s, c) }, 1);
                clear(f32::from(r) / 255.0, f32::from(g) / 255.0, 0.0);
                // Both threads are current at the same time here.
                barrier.wait();
                // SAFETY: as above.
                assert_eq!(unsafe { current() }, c);
                assert_eq!(read_pixel(1, 1), [r, g, 0, 255]);
                barrier.wait();
                // SAFETY: as above.
                assert_eq!(unsafe { make_current(e.dpy, 0, 0, 0) }, 1);
                c
            })
        })
        .collect();
    let ctxs: Vec<usize> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_ne!(ctxs[0], ctxs[1]);
}

/// The mechanism of window surfaces: ANGLE renders into a Direct3D 11
/// texture (here one of the test's own, as the back buffer is) in OpenGL's
/// row order, so the content is bottom-up in memory and the compositor
/// flips it (`window::flip_transform`).
#[test]
fn texture_client_buffer_content_is_bottom_up() {
    if !setup() {
        return;
    }
    let e = display();
    let dev = andruna_gles::egl::d3d11_device(e.dpy).expect("ANGLE's D3D11 device");
    let (w, h) = (8u32, 8u32);
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        ..Default::default()
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: D3D11 call with a valid description.
    unsafe { dev.CreateTexture2D(&desc, None, Some(&mut tex)) }.unwrap();
    let tex = tex.unwrap();
    let s = andruna_gles::egl::pbuffer_from_texture(e.dpy, e.cfg, tex.as_raw() as usize, &[])
        .expect("pbuffer on a D3D11 texture");
    let c = context(&e);
    let make_current = f!("eglMakeCurrent", fn(usize, usize, usize, usize) -> u32);
    let enable = f!("glEnable", fn(u32));
    let scissor = f!("glScissor", fn(i32, i32, i32, i32));
    let finish = f!("glFinish", fn());
    // SAFETY: EGL and GL calls with valid arguments.
    unsafe {
        assert_eq!(make_current(e.dpy, s, s, c), 1);
        clear(0.0, 0.0, 1.0);
        // The upper half in OpenGL's coordinates (y up) is red.
        enable(GL_SCISSOR_TEST);
        scissor(0, (h / 2) as i32, w as i32, (h / 2) as i32);
        clear(1.0, 0.0, 0.0);
        finish();
    }
    assert_eq!(read_pixel(0, (h - 1) as i32), [255, 0, 0, 255]);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: D3D11 calls; the staging texture is mapped for reading and
    // has `h` rows of `RowPitch` bytes.
    let (first_row, last_row) = unsafe {
        dev.CreateTexture2D(&desc, None, Some(&mut staging))
            .unwrap();
        let staging = staging.unwrap();
        let ctx = dev.GetImmediateContext().unwrap();
        ctx.CopyResource(&staging, &tex);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
            .unwrap();
        let p = m.pData as *const u8;
        let first = std::slice::from_raw_parts(p, 4).to_vec();
        let last =
            std::slice::from_raw_parts(p.add(m.RowPitch as usize * (h as usize - 1)), 4).to_vec();
        ctx.Unmap(&staging, 0);
        (first, last)
    };
    // Memory row 0 (the top of an upright image) holds OpenGL's bottom row.
    assert_eq!(first_row, [0, 0, 255, 255]);
    assert_eq!(last_row, [255, 0, 0, 255]);
    // SAFETY: as above.
    unsafe { make_current(e.dpy, 0, 0, 0) };
}

/// Cost of a GL call from guest code: the x64 thunk (or on arm64 the
/// direct binding) against a direct call of ANGLE's export.
#[test]
fn call_cost() {
    if !setup() {
        return;
    }
    let e = display();
    let pbuffer = f!(
        "eglCreatePbufferSurface",
        fn(usize, usize, *const i32) -> usize
    );
    let make_current = f!("eglMakeCurrent", fn(usize, usize, usize, usize) -> u32);
    let attrs = [0x3057, 4, 0x3056, 4, EGL_NONE];
    // SAFETY: EGL calls with valid arguments.
    let s = unsafe { pbuffer(e.dpy, e.cfg, attrs.as_ptr()) };
    let c = context(&e);
    // SAFETY: as above.
    unsafe { make_current(e.dpy, s, s, c) };
    let guest = f!("glGetError", fn() -> u32);
    let addr = andruna_gles::thunk::angle_address("glGetError");
    // SAFETY: ANGLE's export with its prototype (Microsoft convention).
    let direct = unsafe { core::mem::transmute::<usize, unsafe extern "C" fn() -> u32>(addr) };
    let n = 2_000_000;
    let time = |f: &dyn Fn() -> u32| {
        let t = std::time::Instant::now();
        let mut sink = 0;
        for _ in 0..n {
            sink += f();
        }
        assert_eq!(sink, 0);
        t.elapsed().as_nanos() as f64 / f64::from(n)
    };
    // SAFETY: calls on the current context.
    let d = time(&|| unsafe { direct() });
    // SAFETY: as above.
    let g = time(&|| unsafe { guest() });
    eprintln!("glGetError: direct {d:.1} ns, through the guest binding {g:.1} ns");
    // SAFETY: as above.
    unsafe { make_current(e.dpy, 0, 0, 0) };
}

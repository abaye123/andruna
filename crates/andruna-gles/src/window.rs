//! Window surfaces: what a guest `eglCreateWindowSurface` makes.
//!
//! # The interface for a compositor
//!
//! A compositor (andruna-compositor, or [`crate::harness`] in tests) owns
//! the Windows window and its DirectComposition tree. For each Android
//! surface it creates a composition surface handle
//! (`DCompositionCreateSurfaceHandle`), sets
//! `IDCompositionDevice::CreateSurfaceFromHandle(handle)` as the content of
//! the surface's visual, and gives the GL side a [`WindowTarget`]. The
//! handle can be duplicated into the app process, so the compositor can
//! live in another process (sysd). [`crate::native_window::create`] turns
//! the target into the guest's `ANativeWindow*`.
//!
//! # What the GL side does with it
//!
//! [`WindowSurface`] creates a flip-model composition swap chain on ANGLE's
//! Direct3D 11 device for that handle
//! (`IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle`), and
//! an ANGLE pbuffer on its back buffer (`EGL_ANGLE_d3d_texture_client_buffer`)
//! that is the guest's default framebuffer. `eglSwapBuffers` presents it
//! (vsync-paced with a frame latency waitable object, like a buffer queue
//! whose `dequeueBuffer` blocks) and follows the target's size.
//!
//! The content is stored bottom-up (OpenGL's row order, as ANGLE renders
//! into textures), so the target shows it flipped vertically: apply
//! [`flip_transform`] for the buffer height to the visual. DirectComposition
//! applies it during composition, at no extra cost.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Multithread, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_FRAME_STATISTICS, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGIFactoryMedia, IDXGISwapChain1, IDXGISwapChain2,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::Interface;

use crate::egl::{self, EglError};

/// What a compositor provides for one window surface.
pub trait WindowTarget: Send + Sync {
    /// The current size of the surface's content in physical pixels. The
    /// swap chain follows it at the next `eglSwapBuffers`.
    fn size(&self) -> (u32, u32);

    /// The composition surface handle the swap chain presents into. The
    /// target owns it and keeps it valid while it exists.
    fn surface_handle(&self) -> HANDLE;

    /// After each present, with the size of the presented buffer. The
    /// target shows the content flipped vertically ([`flip_transform`])
    /// and commits its composition changes if this size changed.
    fn presented(&self, width: u32, height: u32);

    /// False once the window was closed by the user.
    fn is_open(&self) -> bool {
        true
    }
}

/// The DirectComposition transform that shows bottom-up content of
/// `height` pixels upright.
pub fn flip_transform(height: u32) -> windows_numerics::Matrix3x2 {
    windows_numerics::Matrix3x2 {
        M11: 1.0,
        M12: 0.0,
        M21: 0.0,
        M22: -1.0,
        M31: 0.0,
        M32: height as f32,
    }
}

/// Frame timing of a window, for diagnostics and the demo.
#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    pub frames: u64,
    /// Time between successive `eglSwapBuffers` returns (ms).
    pub intervals: VecDeque<f64>,
    /// CPU time inside `eglSwapBuffers` (present and the wait for a free
    /// buffer, ms).
    pub swap_times: VecDeque<f64>,
    /// Present call to the vertical blank it was shown at (ms), from DXGI
    /// frame statistics.
    pub latencies: VecDeque<f64>,
    last_return: i64,
}

const KEEP: usize = 4096;

fn push(q: &mut VecDeque<f64>, v: f64) {
    if q.len() == KEEP {
        q.pop_front();
    }
    q.push_back(v);
}

/// Mean and 99th percentile of samples.
pub fn summary(v: &VecDeque<f64>) -> (f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(f64::total_cmp);
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let p99 = s[((s.len() as f64 * 0.99) as usize).min(s.len() - 1)];
    (mean, p99)
}

pub(crate) fn qpc() -> i64 {
    let mut t = 0;
    // SAFETY: writes the counter into a local.
    let _ = unsafe { QueryPerformanceCounter(&mut t) };
    t
}

pub(crate) fn qpc_ms(d: i64) -> f64 {
    static F: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let f = *F.get_or_init(|| {
        let mut f = 0;
        // SAFETY: writes the frequency into a local.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    });
    d as f64 * 1000.0 / f as f64
}

/// Frames in flight before `eglSwapBuffers` waits (`ANDRUNA_GLES_LATENCY`,
/// 1 to 3, default 2).
fn max_latency() -> u32 {
    std::env::var("ANDRUNA_GLES_LATENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2u32)
        .clamp(1, 3)
}

const SWAP_FLAGS: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

/// A window surface: a composition swap chain and the ANGLE pbuffer on its
/// back buffer.
pub struct WindowSurface {
    pub dpy: usize,
    pub config: usize,
    /// The guest's `ANativeWindow*` (one reference held).
    pub window: usize,
    target: Arc<dyn WindowTarget>,
    stats: Arc<Mutex<FrameStats>>,
    device: ID3D11Device,
    swap: IDXGISwapChain1,
    waitable: HANDLE,
    format: DXGI_FORMAT,
    /// The ANGLE surface the guest renders to.
    pub pbuffer: usize,
    pub width: u32,
    pub height: u32,
    pub interval: u32,
    /// Extra pbuffer attributes from the guest (`EGL_GL_COLORSPACE`).
    attribs: Vec<i32>,
    /// Submit time of recent presents by present count.
    submitted: VecDeque<(u32, i64)>,
}

// SAFETY: the COM objects are used under the surface's mutex; ANGLE's
// device has multithread protection on (`create`), and DXGI swap chains
// may be used from any thread.
unsafe impl Send for WindowSurface {}

fn factory(device: &ID3D11Device) -> windows::core::Result<IDXGIFactoryMedia> {
    // SAFETY: COM calls on valid interfaces.
    unsafe {
        let adapter = device.cast::<IDXGIDevice>()?.GetAdapter()?;
        adapter.GetParent::<IDXGIFactory2>()?.cast()
    }
}

/// A composition swap chain for the target's surface handle and its frame
/// latency waitable object.
fn new_chain(
    device: &ID3D11Device,
    target: &dyn WindowTarget,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<(IDXGISwapChain1, HANDLE), EglError> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: format,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: SWAP_FLAGS.0 as u32,
    };
    let f = factory(device).map_err(|_| EglError::BadAlloc)?;
    // SAFETY: COM call with a valid description; the handle belongs to the
    // target, which outlives the surface.
    let swap = unsafe {
        f.CreateSwapChainForCompositionSurfaceHandle(
            device,
            Some(target.surface_handle()),
            &desc,
            None,
        )
    }
    .map_err(|e| {
        crate::log(format_args!("composition swap chain: {e}"));
        EglError::BadNativeWindow
    })?;
    let sc2: IDXGISwapChain2 = swap.cast().map_err(|_| EglError::BadAlloc)?;
    // SAFETY: COM calls on a valid swap chain made with the waitable flag.
    let waitable = unsafe {
        sc2.SetMaximumFrameLatency(max_latency())
            .map_err(|_| EglError::BadAlloc)?;
        sc2.GetFrameLatencyWaitableObject()
    };
    Ok((swap, waitable))
}

impl WindowSurface {
    /// Creates the surface for a guest window. `attribs` are the
    /// pass-through pbuffer attributes (pairs, without `EGL_NONE`).
    pub fn create(
        dpy: usize,
        config: usize,
        window: usize,
        attribs: Vec<i32>,
    ) -> Result<WindowSurface, EglError> {
        let w = crate::native_window::lookup(window).ok_or(EglError::BadNativeWindow)?;
        let device = egl::d3d11_device(dpy).ok_or(EglError::BadDisplay)?;
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            // SAFETY: a COM call; the swap chain is presented outside
            // ANGLE's locks, so the immediate context must be serialized.
            unsafe {
                let _ = mt.SetMultithreadProtected(true);
            }
        }
        let (width, height) = clamp_size(w.target.size());
        let mut last = EglError::BadMatch;
        // ANGLE accepts the back buffer when its format matches the
        // config's (RGBA8 or BGRA8 configs).
        for format in [DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM] {
            let (swap, waitable) = new_chain(&device, w.target.as_ref(), width, height, format)?;
            // Released by Drop, also when this attempt fails.
            crate::native_window::acquire(window);
            let mut s = WindowSurface {
                dpy,
                config,
                window,
                target: w.target.clone(),
                stats: w.stats.clone(),
                device: device.clone(),
                swap,
                waitable,
                format,
                pbuffer: 0,
                width,
                height,
                interval: w
                    .swap_interval
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .clamp(0, 4) as u32,
                attribs: attribs.clone(),
                submitted: VecDeque::new(),
            };
            match s.make_pbuffer() {
                Ok(p) => {
                    s.pbuffer = p;
                    s.wait_for_buffer();
                    if crate::tracing() {
                        crate::log(format_args!(
                            "window surface {width}x{height} format {} on window {window:#x}",
                            format.0
                        ));
                    }
                    return Ok(s);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn make_pbuffer(&self) -> Result<usize, EglError> {
        // SAFETY: COM call on a valid swap chain.
        let tex: ID3D11Texture2D =
            unsafe { self.swap.GetBuffer(0) }.map_err(|_| EglError::BadAlloc)?;
        egl::pbuffer_from_texture(self.dpy, self.config, tex.as_raw() as usize, &self.attribs)
    }

    fn wait_for_buffer(&self) {
        if !self.waitable.is_invalid() {
            // SAFETY: waits on the swap chain's own waitable object.
            let r = unsafe { WaitForSingleObjectEx(self.waitable, 1000, true) };
            if r != WAIT_OBJECT_0 && crate::tracing() {
                crate::log(format_args!("frame latency wait: {r:?}"));
            }
        }
    }

    /// `eglSwapBuffers`: presents, follows the target's size and waits for
    /// a free buffer.
    pub fn swap(&mut self) -> Result<(), EglError> {
        let t0 = qpc();
        egl::flush();
        // SAFETY: COM call on a valid swap chain; ANGLE's commands for the
        // back buffer are in the same (protected) immediate context.
        let hr = unsafe { self.swap.Present(self.interval, DXGI_PRESENT(0)) };
        if hr.is_err() {
            crate::log(format_args!("Present: {hr:?}"));
            return Err(EglError::BadSurface);
        }
        // SAFETY: as above.
        if let Ok(n) = unsafe { self.swap.GetLastPresentCount() } {
            if self.submitted.len() == 16 {
                self.submitted.pop_front();
            }
            self.submitted.push_back((n, t0));
        }
        self.target.presented(self.width, self.height);
        let latency = self.present_latency();
        let (w, h) = clamp_size(self.target.size());
        if (w, h) != (self.width, self.height) {
            self.resize(w, h)?;
        } else {
            self.wait_for_buffer();
        }
        let t1 = qpc();
        let mut s = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        s.frames += 1;
        push(&mut s.swap_times, qpc_ms(t1 - t0));
        if s.last_return != 0 {
            let d = qpc_ms(t1 - s.last_return);
            push(&mut s.intervals, d);
        }
        s.last_return = t1;
        if let Some(l) = latency {
            push(&mut s.latencies, l);
        }
        Ok(())
    }

    /// The latest DXGI frame statistics matched with a recorded present.
    fn present_latency(&self) -> Option<f64> {
        let mut st = DXGI_FRAME_STATISTICS::default();
        // SAFETY: COM call writing into a local.
        unsafe { self.swap.GetFrameStatistics(&mut st) }.ok()?;
        let (_, t) = self.submitted.iter().find(|(n, _)| *n == st.PresentCount)?;
        (st.SyncQPCTime > *t).then(|| qpc_ms(st.SyncQPCTime - t))
    }

    /// Resizes the buffers to `w`x`h`: the pbuffer is replaced (keeping the
    /// thread's current context and surfaces), then the swap chain's
    /// buffers are resized, or the swap chain recreated when Direct3D still
    /// holds references to the old buffers.
    fn resize(&mut self, w: u32, h: u32) -> Result<(), EglError> {
        let cur = egl::Current::get();
        let (was_draw, was_read) = (cur.draw == self.pbuffer, cur.read == self.pbuffer);
        let uses = was_draw || was_read;
        if uses {
            egl::make_current_raw(cur.dpy, 0, 0, cur.ctx);
        }
        egl::destroy_surface_raw(self.dpy, self.pbuffer);
        self.pbuffer = 0;
        egl::flush();
        self.width = w;
        self.height = h;
        // SAFETY: COM call; the only reference to the buffers was the
        // pbuffer, destroyed above.
        let resized = unsafe {
            self.swap
                .ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, SWAP_FLAGS)
        };
        let r = match resized {
            Ok(()) => self.make_pbuffer(),
            Err(e) => {
                if crate::tracing() {
                    crate::log(format_args!(
                        "ResizeBuffers: {e}; recreating the swap chain"
                    ));
                }
                new_chain(&self.device, self.target.as_ref(), w, h, self.format).and_then(
                    |(swap, waitable)| {
                        self.swap = swap;
                        self.waitable = waitable;
                        self.make_pbuffer()
                    },
                )
            }
        };
        if let Ok(p) = r {
            self.pbuffer = p;
        }
        self.wait_for_buffer();
        if uses {
            let draw = if was_draw { self.pbuffer } else { cur.draw };
            let read = if was_read { self.pbuffer } else { cur.read };
            egl::make_current_raw(cur.dpy, draw, read, cur.ctx);
        }
        if crate::tracing() {
            crate::log(format_args!("window surface resized to {w}x{h}"));
        }
        r.map(|_| ())
    }

    pub fn stats(&self) -> Arc<Mutex<FrameStats>> {
        self.stats.clone()
    }
}

impl Drop for WindowSurface {
    fn drop(&mut self) {
        if self.pbuffer != 0 {
            egl::destroy_surface_raw(self.dpy, self.pbuffer);
        }
        crate::native_window::release(self.window);
    }
}

fn clamp_size((w, h): (u32, u32)) -> (u32, u32) {
    (w.clamp(1, 16384), h.clamp(1, 16384))
}

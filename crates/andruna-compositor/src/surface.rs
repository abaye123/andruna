//! The producer side of an Android `Surface`: what an EGL native window
//! (`ANativeWindow`) is built on. **This is the interface for the GLES
//! track** (`andruna-gles`).
//!
//! # Model
//!
//! On a device, SurfaceFlinger owns layers and an app process produces
//! buffers into them through `BLASTBufferQueue`/`ANativeWindow`. Here the
//! compositor (in sysd) owns one DirectComposition visual per surface and
//! creates a *composition surface handle* for it
//! (`DCompositionCreateSurfaceHandle`). The handle is duplicated into the
//! app process. The app binds a flip-model swap chain to it on its own
//! Direct3D 11 device, and every `Present` of that swap chain shows on
//! screen through DWM without a copy and without a round trip to sysd.
//!
//! ```text
//!  sysd: Compositor::create_surface ──► SurfaceHandle (duplicated into the app)
//!  app:  NativeSurface::open(device, handle, w, h, format)
//!          ├─ back_buffer()  ID3D11Texture2D to render into
//!          ├─ present()      queueBuffer
//!          └─ resize()       ANativeWindow_setBuffersGeometry
//! ```
//!
//! # Use from EGL on ANGLE
//!
//! 1. Create ANGLE's display on the same `ID3D11Device` that is passed to
//!    [`NativeSurface::open`] (`EGL_ANGLE_device_creation_d3d11`:
//!    `eglCreateDeviceANGLE(EGL_D3D11_DEVICE_ANGLE, device)`, then
//!    `eglGetPlatformDisplayEXT(EGL_PLATFORM_DEVICE_EXT, ...)`).
//! 2. For an `ANativeWindow`, wrap [`NativeSurface::back_buffer`] as an EGL
//!    surface: `eglCreatePbufferFromClientBuffer(EGL_D3D_TEXTURE_ANGLE,
//!    texture, config, attribs)` (`EGL_ANGLE_d3d_texture_client_buffer`).
//!    In Direct3D 11, buffer 0 of a flip-model swap chain always names the
//!    current back buffer, so the EGL surface stays valid across presents.
//! 3. `eglSwapBuffers` becomes: `glFlush` (or `eglWaitClient`), then
//!    [`NativeSurface::present`].
//! 4. On `ANativeWindow_setBuffersGeometry` or a window resize: destroy the
//!    EGL surface, [`NativeSurface::resize`], and wrap the back buffer
//!    again (Direct3D requires that no reference to the old buffers is
//!    held during the resize).
//!
//! `ANativeWindow_query` answers come from [`NativeSurface::width`],
//! [`NativeSurface::height`] and [`NativeSurface::format`]; the default
//! dataspace is sRGB. The surface is premultiplied, as Android surfaces
//! are, except [`BufferFormat::Rgbx8888`] (opaque).
//!
//! This module only depends on `windows` types, so the GLES crate can use
//! it without the rest of the compositor.

use std::fmt;
use std::io;

pub use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
pub use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;

use crate::sys::swapchain::SwapChain;

/// Pixel formats of Android surfaces that a composition swap chain can
/// show. The values are Android's `AHARDWAREBUFFER_FORMAT_*` /
/// `HAL_PIXEL_FORMAT_*`. `RGB_565` and `RGB_888` have no flip-model
/// equivalent; Android's `Surface` falls back to RGBA_8888 for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum BufferFormat {
    Rgba8888 = 1,
    Rgbx8888 = 2,
    Bgra8888 = 5,
    RgbaFp16 = 0x16,
    Rgba1010102 = 0x2b,
}

impl BufferFormat {
    /// From an Android format value.
    pub fn from_android(v: i32) -> Option<BufferFormat> {
        Some(match v {
            1 => BufferFormat::Rgba8888,
            2 => BufferFormat::Rgbx8888,
            5 => BufferFormat::Bgra8888,
            0x16 => BufferFormat::RgbaFp16,
            0x2b => BufferFormat::Rgba1010102,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub enum SurfaceError {
    Io(io::Error),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceError::Io(e) => write!(f, "surface: {e}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<io::Error> for SurfaceError {
    fn from(e: io::Error) -> Self {
        SurfaceError::Io(e)
    }
}

/// A Direct3D 11 device on the default adapter (WARP when there is no
/// hardware device), with BGRA support. Producers that have their own
/// device (ANGLE's) use that instead.
pub fn create_device() -> io::Result<ID3D11Device> {
    crate::sys::gfx::create_d3d().map(|d| d.0)
}

/// What the compositor hands to the producer of a surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceHandle {
    /// A composition surface handle, valid in the producer's process.
    pub handle: usize,
    /// The size the window wants, in pixels.
    pub width: u32,
    pub height: u32,
}

/// A producer's view of a surface: a swap chain presenting into a
/// compositor visual. One per `ANativeWindow`.
pub struct NativeSurface {
    chain: SwapChain,
    width: u32,
    height: u32,
    format: BufferFormat,
}

/// Swap chain buffers (double buffering plus one, like Android's default
/// of three buffers for a `BLASTBufferQueue`).
pub const BUFFER_COUNT: u32 = 3;

impl NativeSurface {
    /// Binds a swap chain on `device` to the surface. The handle stays
    /// owned by the caller (close it after this returns, or keep it to
    /// open another producer later).
    pub fn open(
        device: &ID3D11Device,
        handle: &SurfaceHandle,
        format: BufferFormat,
    ) -> Result<NativeSurface, SurfaceError> {
        let chain = SwapChain::create(
            device,
            handle.handle,
            handle.width,
            handle.height,
            format,
            BUFFER_COUNT,
        )?;
        Ok(NativeSurface {
            chain,
            width: handle.width.max(1),
            height: handle.height.max(1),
            format,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> BufferFormat {
        self.format
    }

    /// The texture to render the next frame into.
    pub fn back_buffer(&self) -> Result<ID3D11Texture2D, SurfaceError> {
        Ok(self.chain.back_buffer()?)
    }

    /// The swap chain, for producers that drive it directly (video).
    pub fn swap_chain(&self) -> &IDXGISwapChain1 {
        &self.chain.swap_chain
    }

    /// Queues the back buffer for display. `sync_interval` 0 presents at
    /// the next composition without blocking; 1 waits for a vsync slot.
    pub fn present(&self, sync_interval: u32) -> Result<(), SurfaceError> {
        Ok(self.chain.present(sync_interval)?)
    }

    /// Changes the buffer size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), SurfaceError> {
        self.chain.resize(width, height)?;
        self.width = width.max(1);
        self.height = height.max(1);
        Ok(())
    }

    /// Fills the back buffer with a premultiplied RGBA color (tests, the
    /// test host, and a producer's first frame).
    pub fn clear(&self, rgba: [f32; 4]) -> Result<(), SurfaceError> {
        Ok(self.chain.clear(rgba)?)
    }
}

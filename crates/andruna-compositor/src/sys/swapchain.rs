//! The producer side of a surface: a flip-model swap chain bound to a
//! composition surface handle the compositor created.

use std::io;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactoryMedia, IDXGISwapChain1,
};
use windows::core::Interface;

use crate::surface::{BufferFormat, SurfaceError};

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

pub fn dxgi_format(f: BufferFormat) -> DXGI_FORMAT {
    match f {
        BufferFormat::Rgba8888 | BufferFormat::Rgbx8888 => DXGI_FORMAT_R8G8B8A8_UNORM,
        BufferFormat::Bgra8888 => DXGI_FORMAT_B8G8R8A8_UNORM,
        BufferFormat::RgbaFp16 => DXGI_FORMAT_R16G16B16A16_FLOAT,
        BufferFormat::Rgba1010102 => DXGI_FORMAT_R10G10B10A2_UNORM,
    }
}

fn alpha_mode(f: BufferFormat) -> DXGI_ALPHA_MODE {
    match f {
        BufferFormat::Rgbx8888 => DXGI_ALPHA_MODE_IGNORE,
        _ => DXGI_ALPHA_MODE_PREMULTIPLIED,
    }
}

/// A swap chain presenting into a composition surface.
pub struct SwapChain {
    pub swap_chain: IDXGISwapChain1,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

impl SwapChain {
    /// Creates a swap chain on `device` for the composition surface
    /// `handle` (a handle value of this process, not consumed).
    pub fn create(
        device: &ID3D11Device,
        handle: usize,
        width: u32,
        height: u32,
        format: BufferFormat,
        buffers: u32,
    ) -> Result<SwapChain, SurfaceError> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: dxgi_format(format),
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: buffers.clamp(2, 3),
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: alpha_mode(format),
            Flags: 0,
        };
        // SAFETY: COM calls with valid arguments; `handle` is a
        // composition surface handle of this process.
        unsafe {
            let dxgi: IDXGIDevice = device.cast().map_err(to_io)?;
            let adapter = dxgi.GetAdapter().map_err(to_io)?;
            let factory: IDXGIFactoryMedia = adapter
                .GetParent()
                .or_else(|_| CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)))
                .map_err(to_io)?;
            let swap_chain = factory
                .CreateSwapChainForCompositionSurfaceHandle(
                    device,
                    Some(HANDLE(handle as *mut _)),
                    &desc,
                    None,
                )
                .map_err(to_io)?;
            let context = device.GetImmediateContext().map_err(to_io)?;
            Ok(SwapChain {
                swap_chain,
                device: device.clone(),
                context,
            })
        }
    }

    pub fn back_buffer(&self) -> io::Result<ID3D11Texture2D> {
        // SAFETY: buffer 0 is the current back buffer of a flip-model swap
        // chain in Direct3D 11.
        unsafe { self.swap_chain.GetBuffer(0) }.map_err(to_io)
    }

    /// Clears the back buffer to a premultiplied RGBA color.
    pub fn clear(&self, rgba: [f32; 4]) -> io::Result<()> {
        let tex = self.back_buffer()?;
        // SAFETY: COM calls with valid arguments; the view is used only
        // here.
        unsafe {
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.device
                .CreateRenderTargetView(&tex, None, Some(&mut rtv))
                .map_err(to_io)?;
            let rtv = rtv.ok_or_else(|| io::Error::other("no render target view"))?;
            self.context.ClearRenderTargetView(&rtv, &rgba);
        }
        Ok(())
    }

    pub fn present(&self, sync_interval: u32) -> io::Result<()> {
        // SAFETY: a COM call with plain values.
        unsafe { self.swap_chain.Present(sync_interval, DXGI_PRESENT(0)).ok() }.map_err(to_io)
    }

    pub fn resize(&self, width: u32, height: u32) -> io::Result<()> {
        // SAFETY: no references to the buffers are held by this type;
        // callers drop theirs before resizing (Direct3D requires it).
        unsafe {
            self.context.ClearState();
            self.swap_chain.ResizeBuffers(
                0,
                width.max(1),
                height.max(1),
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
        .map_err(to_io)
    }
}

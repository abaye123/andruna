//! Direct3D, Direct2D and DirectComposition objects of the compositor:
//! one device set per compositor thread, one visual tree per window.
//!
//! The tree of a window:
//!
//! ```text
//! root
//!  ├─ content   offset and scale of the app's area (letterbox, size compat)
//!  │   └─ one visual per Android surface, in z order, each showing a
//!  │      composition surface the app process presents to
//!  ├─ splash    the app icon on the app's background until the first frame
//!  └─ caption   back button and title, drawn with Direct2D
//! ```

use std::io;

use windows::Win32::Foundation::{HANDLE, HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{IUnknown, Interface, w};
use windows_numerics::Matrix3x2;

use super::shm::Handle;
use super::win::Hwnd;
use crate::geom::{Rect, Size};

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

/// A color as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl Rgb {
    fn f(self, alpha: f32) -> D2D1_COLOR_F {
        let c = |s: u32| ((self.0 >> s) & 0xFF) as f32 / 255.0;
        D2D1_COLOR_F {
            r: c(16) * alpha,
            g: c(8) * alpha,
            b: c(0) * alpha,
            a: alpha,
        }
    }

    /// Relative luminance (sRGB, approximate), 0..1.
    pub fn luminance(self) -> f32 {
        let c = |s: u32| {
            let v = ((self.0 >> s) & 0xFF) as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * c(16) + 0.7152 * c(8) + 0.0722 * c(0)
    }
}

/// An image in premultiplied BGRA, rows of `width * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// The device objects of a compositor thread.
pub struct Gfx {
    pub d3d: ID3D11Device,
    _d2d: ID2D1Device,
    dcomp: IDCompositionDesktopDevice,
    dwrite: IDWriteFactory,
    pub warp: bool,
}

impl Gfx {
    /// Creates the devices on the default adapter (WARP when there is no
    /// hardware device, as on some CI machines).
    pub fn new() -> io::Result<Gfx> {
        let (d3d, warp) = create_d3d()?;
        // SAFETY: COM calls on freshly created objects with valid
        // arguments.
        unsafe {
            let dxgi: IDXGIDevice = d3d.cast().map_err(to_io)?;
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(to_io)?;
            let d2d = factory.CreateDevice(&dxgi).map_err(to_io)?;
            let dcomp: IDCompositionDesktopDevice =
                DCompositionCreateDevice2(&d2d).map_err(to_io)?;
            let dwrite: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(to_io)?;
            Ok(Gfx {
                d3d,
                _d2d: d2d.cast().map_err(to_io)?,
                dcomp,
                dwrite,
                warp,
            })
        }
    }

    /// Applies all changes made since the last commit.
    pub fn commit(&self) -> io::Result<()> {
        // SAFETY: a COM call without arguments.
        unsafe { self.dcomp.Commit() }.map_err(to_io)
    }

    /// The compositor's frame statistics (last frame time and the refresh
    /// period, in performance counter units).
    pub fn frame_statistics(&self) -> Option<(i64, i64, i64)> {
        // SAFETY: a COM call returning a struct.
        let s = unsafe { self.dcomp.GetFrameStatistics() }.ok()?;
        let period = if s.currentCompositionRate.Numerator == 0 {
            0
        } else {
            s.timeFrequency * s.currentCompositionRate.Denominator as i64
                / s.currentCompositionRate.Numerator as i64
        };
        Some((s.lastFrameTime, s.nextEstimatedFrameTime, period))
    }

    pub fn create_tree(&self, hwnd: Hwnd) -> io::Result<WindowTree> {
        // SAFETY: COM calls with valid arguments; `hwnd` belongs to this
        // thread.
        unsafe {
            let target = self
                .dcomp
                .CreateTargetForHwnd(hwnd.raw(), true)
                .map_err(to_io)?;
            let root = self.dcomp.CreateVisual().map_err(to_io)?;
            let content = self.dcomp.CreateVisual().map_err(to_io)?;
            let caption = self.dcomp.CreateVisual().map_err(to_io)?;
            // With no reference visual, `insertabove == false` puts the
            // new visual on top of its siblings.
            root.AddVisual(&content, false, None).map_err(to_io)?;
            root.AddVisual(&caption, false, None).map_err(to_io)?;
            target.SetRoot(&root).map_err(to_io)?;
            let clip = self.dcomp.CreateRectangleClip().map_err(to_io)?;
            content.SetClip(&clip).map_err(to_io)?;
            Ok(WindowTree {
                _target: target,
                root,
                content,
                clip,
                caption,
                caption_surface: None,
                splash: None,
            })
        }
    }

    fn surface(&self, size: Size) -> io::Result<IDCompositionSurface> {
        // SAFETY: a COM call with valid arguments.
        unsafe {
            self.dcomp.CreateSurface(
                size.width.max(1) as u32,
                size.height.max(1) as u32,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )
        }
        .map_err(to_io)
    }

    /// Draws into a composition surface with Direct2D.
    fn draw(
        &self,
        surface: &IDCompositionSurface,
        f: impl FnOnce(&ID2D1DeviceContext) -> windows::core::Result<()>,
    ) -> io::Result<()> {
        let mut offset = POINT::default();
        // SAFETY: BeginDraw/EndDraw bracket the drawing; the context is
        // used only between them. `offset` is a valid out pointer.
        unsafe {
            let dc: ID2D1DeviceContext = surface.BeginDraw(None, &mut offset).map_err(to_io)?;
            dc.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            let r = f(&dc);
            surface.EndDraw().map_err(to_io)?;
            r.map_err(to_io)
        }
    }

    fn text_format(
        &self,
        family: windows::core::PCWSTR,
        size: f32,
    ) -> io::Result<IDWriteTextFormat> {
        // SAFETY: COM calls with valid strings.
        unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!(""),
                )
                .map_err(to_io)?;
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            Ok(f)
        }
    }

    /// Draws the caption: the background (the app's color, or nothing so
    /// the backdrop shows), the back button and the title. `buttons_left`
    /// is where DWM's caption buttons start; the background stops there
    /// so they stay visible.
    pub fn draw_caption(&self, tree: &mut WindowTree, c: &Caption) -> io::Result<()> {
        let size = Size::new(c.width.max(1), c.height.max(1));
        let surface = match &tree.caption_surface {
            Some((s, sz)) if *sz == size => s.clone(),
            _ => {
                let s = self.surface(size)?;
                // SAFETY: COM call with a valid surface.
                unsafe { tree.caption.SetContent(&s) }.map_err(to_io)?;
                tree.caption_surface = Some((s.clone(), size));
                s
            }
        };
        let scale = c.dpi as f32 / 96.0;
        let glyph = self.text_format(w!("Segoe Fluent Icons"), 10.0 * scale)?;
        let title_format = self.text_format(w!("Segoe UI Variable Text"), 12.0 * scale)?;
        // SAFETY: COM calls on valid objects inside `draw`.
        unsafe {
            let _ = glyph.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        let title: Vec<u16> = c.title.encode_utf16().collect();
        let back: Vec<u16> = "\u{E72B}".encode_utf16().collect();
        self.draw(&surface, |dc| {
            // SAFETY: drawing calls on a context inside BeginDraw/EndDraw.
            unsafe {
                dc.Clear(Some(&D2D1_COLOR_F::default()));
                if let Some(bg) = c.background {
                    let b = dc.CreateSolidColorBrush(&bg.f(1.0), None)?;
                    let right = c.buttons_left.min(c.width) as f32;
                    dc.FillRectangle(
                        &D2D_RECT_F {
                            left: 0.0,
                            top: 0.0,
                            right,
                            bottom: c.height as f32,
                        },
                        &b,
                    );
                }
                let fg = dc.CreateSolidColorBrush(&c.foreground.f(1.0), None)?;
                let back_rect = D2D_RECT_F {
                    left: c.back.left as f32,
                    top: c.back.top as f32,
                    right: c.back.right as f32,
                    bottom: c.back.bottom as f32,
                };
                if c.back_hover {
                    let h = dc.CreateSolidColorBrush(&c.foreground.f(0.08), None)?;
                    dc.FillRectangle(&back_rect, &h);
                }
                dc.DrawText(
                    &back,
                    &glyph,
                    &back_rect,
                    &fg,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                let title_rect = D2D_RECT_F {
                    left: c.back.right as f32 + 8.0 * scale,
                    top: 0.0,
                    right: (c.buttons_left - 8).max(0) as f32,
                    bottom: c.height as f32,
                };
                dc.DrawText(
                    &title,
                    &title_format,
                    &title_rect,
                    &fg,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        })
    }

    /// Shows the splash: the app's icon centered on its background color
    /// (or the backdrop), covering the content area.
    pub fn show_splash(
        &self,
        tree: &mut WindowTree,
        area: Rect,
        background: Option<Rgb>,
        icon: Option<&Image>,
        icon_px: i32,
    ) -> io::Result<()> {
        // SAFETY: COM calls with valid arguments.
        unsafe {
            let (visual, effect) = match &tree.splash {
                Some((v, e, _)) => (v.clone(), e.clone()),
                None => {
                    let v = self.dcomp.CreateVisual().map_err(to_io)?;
                    let e = self.dcomp.CreateEffectGroup().map_err(to_io)?;
                    v.SetEffect(&e).map_err(to_io)?;
                    tree.root
                        .AddVisual(&v, false, &tree.caption)
                        .map_err(to_io)?;
                    (v, e)
                }
            };
            let surface = self.surface(area.size())?;
            let bitmap = |dc: &ID2D1DeviceContext| -> windows::core::Result<Option<ID2D1Bitmap1>> {
                let Some(i) = icon else { return Ok(None) };
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                };
                dc.CreateBitmap(
                    D2D_SIZE_U {
                        width: i.width,
                        height: i.height,
                    },
                    Some(i.bgra.as_ptr() as *const _),
                    i.width * 4,
                    &props,
                )
                .map(Some)
            };
            self.draw(&surface, |dc| {
                dc.Clear(Some(
                    &background.map_or(D2D1_COLOR_F::default(), |b| b.f(1.0)),
                ));
                if let Some(b) = bitmap(dc)? {
                    let r = Rect::new(0, 0, area.width(), area.height())
                        .center(Size::new(icon_px, icon_px));
                    dc.DrawBitmap(
                        &b,
                        Some(&D2D_RECT_F {
                            left: r.left as f32,
                            top: r.top as f32,
                            right: r.right as f32,
                            bottom: r.bottom as f32,
                        }),
                        1.0,
                        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                        None,
                        None,
                    );
                }
                Ok(())
            })?;
            visual.SetContent(&surface).map_err(to_io)?;
            visual.SetOffsetX2(area.left as f32).map_err(to_io)?;
            visual.SetOffsetY2(area.top as f32).map_err(to_io)?;
            effect.SetOpacity2(1.0).map_err(to_io)?;
            tree.splash = Some((visual, effect, false));
        }
        Ok(())
    }

    /// Fades the splash out over `ms` milliseconds.
    pub fn fade_splash(&self, tree: &mut WindowTree, ms: u32) -> io::Result<()> {
        let Some((_, effect, fading)) = &mut tree.splash else {
            return Ok(());
        };
        if *fading {
            return Ok(());
        }
        *fading = true;
        let secs = ms as f64 / 1000.0;
        // SAFETY: COM calls with valid arguments.
        unsafe {
            let a = self.dcomp.CreateAnimation().map_err(to_io)?;
            a.AddCubic(0.0, 1.0, (-1.0 / secs) as f32, 0.0, 0.0)
                .map_err(to_io)?;
            a.End(secs, 0.0).map_err(to_io)?;
            effect.SetOpacity(&a).map_err(to_io)?;
        }
        Ok(())
    }

    /// Removes the splash visual (after its fade).
    pub fn remove_splash(&self, tree: &mut WindowTree) {
        if let Some((v, _, _)) = tree.splash.take() {
            // SAFETY: the visual is a child of the root.
            unsafe {
                let _ = tree.root.RemoveVisual(&v);
            }
        }
    }

    /// A new surface visual with a composition surface handle a producer
    /// (the app process) presents to.
    pub fn create_surface(&self) -> io::Result<SurfaceVisual> {
        // SAFETY: COM and kernel calls with valid arguments; the handle is
        // owned by the returned value.
        unsafe {
            let h: HANDLE = DCompositionCreateSurfaceHandle(
                (COMPOSITIONOBJECT_READ | COMPOSITIONOBJECT_WRITE) as u32,
                None,
            )
            .map_err(to_io)?;
            let handle = Handle::from_raw(h.0 as usize);
            let content: IUnknown = self.dcomp.CreateSurfaceFromHandle(h).map_err(to_io)?;
            let visual = self.dcomp.CreateVisual().map_err(to_io)?;
            visual.SetContent(&content).map_err(to_io)?;
            Ok(SurfaceVisual {
                visual,
                handle,
                _content: content,
                place: std::cell::Cell::new((0, 0, None)),
            })
        }
    }
}

/// A Direct3D 11 device on the default adapter (WARP if none); the bool
/// is true for WARP.
pub fn create_d3d() -> io::Result<(ID3D11Device, bool)> {
    let mut last = None;
    for (driver, warp) in [
        (D3D_DRIVER_TYPE_HARDWARE, false),
        (D3D_DRIVER_TYPE_WARP, true),
    ] {
        let mut d = None;
        // SAFETY: valid out pointer; no adapter or software module.
        let r = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d),
                None,
                None,
            )
        };
        match (r, d) {
            (Ok(()), Some(d)) => return Ok((d, warp)),
            (Err(e), _) => last = Some(e),
            _ => {}
        }
    }
    Err(last.map_or_else(|| io::Error::other("no Direct3D 11 device"), to_io))
}

/// What the caption shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub background: Option<Rgb>,
    pub foreground: Rgb,
    pub title: String,
    /// The back button, in caption coordinates.
    pub back: Rect,
    pub back_hover: bool,
    pub buttons_left: i32,
}

/// The visual tree of one window.
pub struct WindowTree {
    _target: IDCompositionTarget,
    root: IDCompositionVisual2,
    content: IDCompositionVisual2,
    clip: IDCompositionRectangleClip,
    caption: IDCompositionVisual2,
    caption_surface: Option<(IDCompositionSurface, Size)>,
    splash: Option<(IDCompositionVisual2, IDCompositionEffectGroup, bool)>,
}

impl WindowTree {
    /// Places the app's area: offset in the client, scale of app pixels.
    /// The app's pixels under the caption (client rows above
    /// `caption_px`) are hidden: the caption is opaque, like Android's
    /// caption bar, unless the app asks for a transparent one (`None`).
    pub fn set_content(&self, rect: Rect, scale: f32, caption_px: Option<i32>) -> io::Result<()> {
        let top = caption_px.map_or(-1e6, |c| (c - rect.top) as f32 / scale);
        let m = Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: rect.left as f32,
            M32: rect.top as f32,
        };
        // SAFETY: COM calls with valid arguments.
        unsafe {
            self.content.SetTransform2(&m).map_err(to_io)?;
            self.clip.SetLeft2(-1e6).map_err(to_io)?;
            self.clip.SetRight2(1e6).map_err(to_io)?;
            self.clip.SetBottom2(1e6).map_err(to_io)?;
            self.clip.SetTop2(top).map_err(to_io)?;
            self.content
                .SetBitmapInterpolationMode(if scale == 1.0 {
                    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                } else {
                    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR
                })
                .map_err(to_io)?;
        }
        Ok(())
    }

    /// Re-adds the surface visuals in z order (bottom first).
    pub fn set_surfaces(&self, surfaces: &[&SurfaceVisual]) -> io::Result<()> {
        // SAFETY: COM calls with valid visuals.
        unsafe {
            self.content.RemoveAllVisuals().map_err(to_io)?;
            for s in surfaces {
                // On top of the ones added before.
                self.content
                    .AddVisual(&s.visual, false, None)
                    .map_err(to_io)?;
            }
        }
        Ok(())
    }

    pub fn has_splash(&self) -> bool {
        self.splash.is_some()
    }
}

/// A visual showing a composition surface.
pub struct SurfaceVisual {
    visual: IDCompositionVisual2,
    handle: Handle,
    _content: IUnknown,
    /// Position (app pixels) and, for bottom-up content, the buffer
    /// height it is flipped over.
    place: std::cell::Cell<(i32, i32, Option<u32>)>,
}

/// The transform that places a surface at (`x`, `y`) and, for bottom-up
/// content of `flip` rows (OpenGL row order), shows it upright: row 0 of
/// the buffer lands at `y + flip - 1`.
pub fn surface_transform(x: i32, y: i32, flip: Option<u32>) -> Matrix3x2 {
    match flip {
        None => Matrix3x2::translation(x as f32, y as f32),
        Some(h) => Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: -1.0,
            M31: x as f32,
            M32: (y as i64 + h as i64) as f32,
        },
    }
}

impl SurfaceVisual {
    /// The composition surface handle (to duplicate into the producer).
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    fn apply(&self) -> io::Result<()> {
        let (x, y, flip) = self.place.get();
        // The position is part of the transform (the offsets stay 0), so
        // the flip never depends on the order DirectComposition applies
        // offsets and transforms in.
        // SAFETY: a COM call with a plain matrix.
        unsafe {
            self.visual
                .SetTransform2(&surface_transform(x, y, flip))
                .map_err(to_io)
        }
    }

    /// Position in app pixels inside the content area.
    pub fn set_position(&self, x: i32, y: i32) -> io::Result<()> {
        let (_, _, flip) = self.place.get();
        self.place.set((x, y, flip));
        self.apply()
    }

    /// How the producer's buffers are stored: `Some(height)` for bottom-up
    /// content of that many rows (GL producers), `None` for top-down
    /// content (CPU, video). Returns whether anything changed.
    pub fn set_orientation(&self, flip: Option<u32>) -> io::Result<bool> {
        let (x, y, old) = self.place.get();
        if old == flip {
            return Ok(false);
        }
        self.place.set((x, y, flip));
        self.apply()?;
        Ok(true)
    }

    /// The current flip (see [`SurfaceVisual::set_orientation`]).
    pub fn orientation(&self) -> Option<u32> {
        self.place.get().2
    }
}

/// Waits for DWM's next composition (a vsync of the compositor).
pub fn dwm_flush() {
    // SAFETY: no arguments.
    unsafe {
        let _ = DwmFlush();
    }
}

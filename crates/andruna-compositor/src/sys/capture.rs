//! Screenshots of windows, for tests and diagnostics.

use std::ffi::c_void;
use std::io;

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};

use super::gfx::Image;
use super::win::Hwnd;
use crate::geom::Rect;

/// `PW_RENDERFULLCONTENT`: include DirectComposition content.
const PW_RENDERFULLCONTENT: u32 = 2;

struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> io::Result<Dib> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Top-down rows.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid header and out pointer; the objects are released
        // in Drop.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmp = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .map_err(io::Error::other)?;
            let old = SelectObject(dc, bmp.into());
            Ok(Dib {
                dc,
                bmp,
                old,
                bits: bits as *mut u8,
                w,
                h,
            })
        }
    }

    fn image(&self) -> Image {
        let len = (self.w * self.h * 4) as usize;
        // SAFETY: the DIB section has `w * h` 32-bit pixels and lives
        // until Drop.
        let bgra = unsafe { std::slice::from_raw_parts(self.bits, len) }.to_vec();
        Image {
            width: self.w as u32,
            height: self.h as u32,
            bgra,
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: objects created in `new`, released once.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The window as DWM composes it, through `PrintWindow` with full
/// content (works for occluded windows). The image is the window's outer
/// rectangle.
pub fn window(h: Hwnd, width: i32, height: i32) -> io::Result<Image> {
    let dib = Dib::new(width.max(1), height.max(1))?;
    // SAFETY: a valid window and memory DC.
    let ok = unsafe { PrintWindow(h.raw(), dib.dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)) };
    if !ok.as_bool() {
        return Err(io::Error::other("PrintWindow failed"));
    }
    Ok(dib.image())
}

/// A rectangle of the screen as it is displayed (the window must be
/// visible and on top).
pub fn screen(r: Rect) -> io::Result<Image> {
    let dib = Dib::new(r.width().max(1), r.height().max(1))?;
    // SAFETY: the screen DC is released after the copy.
    unsafe {
        let screen = GetDC(None);
        let res = BitBlt(
            dib.dc,
            0,
            0,
            r.width(),
            r.height(),
            Some(screen),
            r.left,
            r.top,
            SRCCOPY,
        );
        ReleaseDC(None, screen);
        res.map_err(io::Error::other)?;
    }
    Ok(dib.image())
}

//! Text input: the input method's composition and result strings, and the
//! characters of the active keyboard layout.
//!
//! The composition comes through the IMM messages (`WM_IME_*`), which
//! Windows serves from the Text Services Framework for TSF input methods;
//! see docs/plan/phase3/compositor.md for the plan to implement a TSF text
//! store directly.

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, HKL, MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, ToUnicodeEx, VK_CAPITAL,
    VK_CONTROL, VK_LCONTROL, VK_LSHIFT, VK_MENU, VK_RMENU, VK_SHIFT,
};

use super::win::Hwnd;
use crate::input::kcm::{KeyOutput, Mods};

/// A composition: text, caret (UTF-16 units) and the converted clause.
pub type Composing = (String, i32, Option<(i32, i32)>);

/// What one `WM_IME_COMPOSITION` carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composition {
    /// Text the IME committed.
    pub result: Option<String>,
    /// The new composition: text, caret (UTF-16 units) and the clause being
    /// converted.
    pub composing: Option<Composing>,
}

struct Ctx(Hwnd, HIMC);

impl Ctx {
    fn get(h: Hwnd) -> Option<Ctx> {
        // SAFETY: no pointers; released in Drop.
        let c = unsafe { ImmGetContext(h.raw()) };
        (!c.is_invalid()).then_some(Ctx(h, c))
    }

    fn string(&self, kind: IME_COMPOSITION_STRING) -> Vec<u8> {
        // SAFETY: a size query, then a read into a buffer of that size.
        unsafe {
            let n = ImmGetCompositionStringW(self.1, kind, None, 0);
            if n <= 0 {
                return Vec::new();
            }
            let mut buf = vec![0u8; n as usize];
            let n =
                ImmGetCompositionStringW(self.1, kind, Some(buf.as_mut_ptr() as *mut _), n as u32);
            buf.truncate(n.max(0) as usize);
            buf
        }
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        // SAFETY: the context was obtained for this window.
        unsafe {
            let _ = ImmReleaseContext(self.0.raw(), self.1);
        }
    }
}

fn utf16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Reads the strings `WM_IME_COMPOSITION`'s `lParam` flags announce.
pub fn composition(h: Hwnd, flags: u32) -> Composition {
    let Some(c) = Ctx::get(h) else {
        return Composition::default();
    };
    let mut out = Composition::default();
    if flags & GCS_RESULTSTR.0 != 0 {
        out.result = Some(String::from_utf16_lossy(&utf16(&c.string(GCS_RESULTSTR))));
    }
    if flags & GCS_COMPSTR.0 != 0 {
        let text = utf16(&c.string(GCS_COMPSTR));
        // SAFETY: GCS_CURSORPOS returns the position, no buffer.
        let cursor = unsafe { ImmGetCompositionStringW(c.1, GCS_CURSORPOS, None, 0) };
        let attrs = c.string(GCS_COMPATTR);
        let target = attrs
            .iter()
            .position(|a| {
                *a as u32 == ATTR_TARGET_CONVERTED || *a as u32 == ATTR_TARGET_NOTCONVERTED
            })
            .map(|s| {
                let e = attrs[s..]
                    .iter()
                    .position(|a| {
                        *a as u32 != ATTR_TARGET_CONVERTED && *a as u32 != ATTR_TARGET_NOTCONVERTED
                    })
                    .map_or(attrs.len(), |n| s + n);
                (s as i32, e as i32)
            });
        out.composing = Some((
            String::from_utf16_lossy(&text),
            cursor.clamp(0, text.len() as i32),
            target,
        ));
    }
    out
}

/// Places the IME's candidate and composition windows next to the text
/// cursor (client coordinates of the caret's rectangle).
pub fn set_caret(h: Hwnd, caret: crate::geom::Rect) {
    let Some(c) = Ctx::get(h) else { return };
    let r = RECT {
        left: caret.left,
        top: caret.top,
        right: caret.right,
        bottom: caret.bottom,
    };
    let cand = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_EXCLUDE,
        ptCurrentPos: POINT {
            x: caret.left,
            y: caret.bottom,
        },
        rcArea: r,
    };
    let comp = COMPOSITIONFORM {
        dwStyle: CFS_POINT,
        ptCurrentPos: POINT {
            x: caret.left,
            y: caret.top,
        },
        rcArea: r,
    };
    // SAFETY: valid pointers to initialized structs.
    unsafe {
        let _ = ImmSetCandidateWindow(c.1, &cand);
        let _ = ImmSetCompositionWindow(c.1, &comp);
    }
}

/// Ends the composition, committing it (the app's text field lost
/// focus or the app called `finishComposingText`).
pub fn complete(h: Hwnd) {
    let Some(c) = Ctx::get(h) else { return };
    // SAFETY: no pointers.
    unsafe {
        let _ = ImmNotifyIME(c.1, NI_COMPOSITIONSTR, CPS_COMPLETE, 0);
    }
}

/// The active keyboard layout of this thread.
pub fn current_layout() -> isize {
    // SAFETY: no pointers.
    unsafe { GetKeyboardLayout(0) }.0 as isize
}

/// The first UTF-16 unit of the character a key types now (with the
/// current keyboard state), without changing the keyboard state; this is
/// the `WM_CHAR` the key's translation will produce. The bool is true for
/// a dead key (its `WM_DEADCHAR`).
pub fn translated_unit(vk: u32, scan: u16) -> Option<(bool, u16)> {
    let mut state = [0u8; 256];
    // SAFETY: a 256-byte buffer as required.
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState(&mut state) }.ok()?;
    let mut buf = [0u16; 8];
    // SAFETY: valid buffers; flag 0x4 keeps the keyboard state.
    let n = unsafe { ToUnicodeEx(vk, scan as u32, &state, &mut buf, 0x4, None) };
    (n != 0).then_some((n < 0, buf[0]))
}

/// What a key types in the given layout and modifier state, without
/// changing the keyboard state (dead keys stay pending in nobody's
/// state).
pub fn key_output(layout: isize, scan: u16, mods: Mods) -> Option<KeyOutput> {
    let hkl = HKL(layout as *mut _);
    // SAFETY: no pointers.
    let vk = unsafe { MapVirtualKeyExW(scan as u32, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
    if vk == 0 {
        return None;
    }
    let mut state = [0u8; 256];
    if mods.shift {
        state[VK_SHIFT.0 as usize] = 0x80;
        state[VK_LSHIFT.0 as usize] = 0x80;
    }
    if mods.caps {
        state[VK_CAPITAL.0 as usize] = 0x01;
    }
    if mods.altgr {
        for vk in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
            state[vk.0 as usize] = 0x80;
        }
    }
    let mut buf = [0u16; 8];
    // Flag 0x4: do not change the keyboard state (Windows 10 1607 and
    // later), so dead keys are reported without being stored.
    // SAFETY: valid buffers.
    let n = unsafe { ToUnicodeEx(vk, scan as u32, &state, &mut buf, 0x4, Some(hkl)) };
    let first = || char::decode_utf16(buf.iter().copied()).next()?.ok();
    match n {
        1 => first().filter(|c| !c.is_control()).map(KeyOutput::Char),
        -1 => first().map(KeyOutput::Dead),
        _ => None,
    }
}

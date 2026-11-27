//! Synthetic input through Windows' own input stack, for tests and latency
//! measurements: keys with `SendInput`, touch with a synthetic pointer
//! device. The input goes to whatever window Windows routes it to (the
//! foreground window for keys, the window under the contact for touch).

use std::io;

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, SendInput,
};
use windows::Win32::UI::Input::Pointer::*;
use windows::Win32::UI::WindowsAndMessaging::{
    PT_TOUCH, TOUCH_MASK_CONTACTAREA, TOUCH_MASK_PRESSURE,
};

/// Sends a key by scan code (`0xE0xx` for extended keys).
pub fn key(scan: u16, down: bool) -> io::Result<()> {
    let mut flags = KEYEVENTF_SCANCODE;
    if scan & 0xFF00 == 0xE000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: scan & 0xFF,
                dwFlags: KEYBD_EVENT_FLAGS(flags.0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one initialized INPUT of the size passed.
    let n = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if n == 1 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Posts a message to a window, as the keyboard input stack would after
/// translation (tests that cannot take the foreground).
pub fn post(h: super::win::Hwnd, msg: u32, wparam: usize, lparam: isize) -> io::Result<()> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    // SAFETY: no pointers are passed in the parameters.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(h.raw()),
            msg,
            WPARAM(wparam),
            LPARAM(lparam),
        )
    }
    .map_err(io::Error::other)
}

/// The `lParam` of a key message for a scan code (`0xE0xx` extended).
pub fn key_lparam(scan: u16, down: bool) -> isize {
    let mut l = 1isize | (((scan & 0xFF) as isize) << 16);
    if scan & 0xFF00 == 0xE000 {
        l |= 1 << 24;
    }
    if !down {
        l |= (1 << 30) | (1 << 31);
    }
    l
}

/// A contact's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

/// A synthetic touch screen.
pub struct Touch(HSYNTHETICPOINTERDEVICE);

impl Touch {
    pub fn new(max_contacts: u32) -> io::Result<Touch> {
        // SAFETY: no pointers; destroyed in Drop.
        let d =
            unsafe { CreateSyntheticPointerDevice(PT_TOUCH, max_contacts, POINTER_FEEDBACK_NONE) }
                .map_err(io::Error::other)?;
        Ok(Touch(d))
    }

    /// Injects one frame: (contact id, screen x, screen y, phase).
    pub fn frame(&self, contacts: &[(u32, i32, i32, Phase)]) -> io::Result<()> {
        let infos: Vec<POINTER_TYPE_INFO> = contacts
            .iter()
            .map(|&(id, x, y, phase)| {
                let flags = match phase {
                    Phase::Down => {
                        POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
                    }
                    Phase::Move => {
                        POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
                    }
                    Phase::Up => POINTER_FLAG_UP,
                };
                let touch = POINTER_TOUCH_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_TOUCH,
                        pointerId: id,
                        pointerFlags: flags,
                        ptPixelLocation: POINT { x, y },
                        ..Default::default()
                    },
                    touchMask: TOUCH_MASK_CONTACTAREA | TOUCH_MASK_PRESSURE,
                    rcContact: RECT {
                        left: x - 4,
                        top: y - 4,
                        right: x + 4,
                        bottom: y + 4,
                    },
                    pressure: 512,
                    ..Default::default()
                };
                POINTER_TYPE_INFO {
                    r#type: PT_TOUCH,
                    Anonymous: POINTER_TYPE_INFO_0 { touchInfo: touch },
                }
            })
            .collect();
        // SAFETY: a valid device and initialized entries.
        unsafe { InjectSyntheticPointerInput(self.0, &infos) }.map_err(io::Error::other)
    }
}

impl Drop for Touch {
    fn drop(&mut self) {
        // SAFETY: created in `new`, destroyed once.
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}

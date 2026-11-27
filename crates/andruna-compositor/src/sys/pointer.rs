//! Reads Windows pointer input (`WM_POINTER*`) into the neutral frames of
//! [`input::pointer`](crate::input::pointer).

use windows::Win32::UI::Input::Pointer::*;
use windows::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_MOUSE, PT_PEN, PT_TOUCH, PT_TOUCHPAD,
    TOUCH_MASK_CONTACTAREA, TOUCH_MASK_PRESSURE,
};

use crate::android::motion::*;
use crate::clock;
use crate::input::pointer::{PointerFrame, PointerKind, PointerSample};

/// Maps screen pixel coordinates to window coordinates of the app.
pub type MapFn<'a> = &'a dyn Fn(i32, i32) -> (f32, f32);

/// The kind of a pointer, `None` for unknown types.
pub fn kind(pointer_id: u32) -> Option<PointerKind> {
    let mut t = POINTER_INPUT_TYPE::default();
    // SAFETY: valid out pointer.
    unsafe { GetPointerType(pointer_id, &mut t) }.ok()?;
    match t {
        PT_MOUSE | PT_TOUCHPAD => Some(PointerKind::Mouse),
        PT_TOUCH => Some(PointerKind::Touch),
        PT_PEN => Some(PointerKind::Pen),
        _ => None,
    }
}

fn time_ns(i: &POINTER_INFO) -> i64 {
    if i.PerformanceCount != 0 {
        clock::qpc_to_uptime_ns(i.PerformanceCount as i64)
    } else {
        clock::tick_to_uptime_ns(i.dwTime)
    }
}

fn has(f: POINTER_FLAGS, bit: POINTER_FLAGS) -> bool {
    f.0 & bit.0 != 0
}

fn base(i: &POINTER_INFO, map: MapFn) -> PointerSample {
    let (x, y) = map(i.ptPixelLocation.x, i.ptPixelLocation.y);
    let f = i.pointerFlags;
    PointerSample {
        id: i.pointerId,
        x,
        y,
        in_contact: has(f, POINTER_FLAG_INCONTACT),
        in_range: has(f, POINTER_FLAG_INRANGE),
        canceled: has(f, POINTER_FLAG_CANCELED),
        ..Default::default()
    }
}

fn mouse_buttons(f: POINTER_FLAGS) -> i32 {
    let mut b = 0;
    for (flag, button) in [
        (POINTER_FLAG_FIRSTBUTTON, BUTTON_PRIMARY),
        (POINTER_FLAG_SECONDBUTTON, BUTTON_SECONDARY),
        (POINTER_FLAG_THIRDBUTTON, BUTTON_TERTIARY),
        (POINTER_FLAG_FOURTHBUTTON, BUTTON_BACK),
        (POINTER_FLAG_FIFTHBUTTON, BUTTON_FORWARD),
    ] {
        if has(f, flag) {
            b |= button;
        }
    }
    b
}

/// The frames of a pointer message, oldest first: the coalesced history
/// of a mouse or pen, or all contacts of a touch frame.
pub fn read(pointer_id: u32, kind: PointerKind, map: MapFn) -> Vec<PointerFrame> {
    match kind {
        PointerKind::Mouse => read_mouse(pointer_id, map),
        PointerKind::Pen => read_pen(pointer_id, map),
        PointerKind::Touch => read_touch(pointer_id, map),
    }
}

fn read_mouse(id: u32, map: MapFn) -> Vec<PointerFrame> {
    let mut n = 0u32;
    // SAFETY: a size query with a valid out pointer.
    if unsafe { GetPointerInfoHistory(id, &mut n, None) }.is_err() || n == 0 {
        return Vec::new();
    }
    let mut v = vec![POINTER_INFO::default(); n as usize];
    // SAFETY: `v` has room for `n` entries.
    if unsafe { GetPointerInfoHistory(id, &mut n, Some(v.as_mut_ptr())) }.is_err() {
        return Vec::new();
    }
    v.truncate(n as usize);
    v.iter()
        .rev()
        .map(|i| PointerFrame {
            time_ns: time_ns(i),
            pointers: vec![PointerSample {
                buttons: mouse_buttons(i.pointerFlags),
                // A mouse over the window is in range unless it left.
                in_range: !has(i.pointerFlags, POINTER_FLAG_CANCELED),
                ..base(i, map)
            }],
        })
        .collect()
}

fn read_pen(id: u32, map: MapFn) -> Vec<PointerFrame> {
    let mut n = 0u32;
    // SAFETY: a size query with a valid out pointer.
    if unsafe { GetPointerPenInfoHistory(id, &mut n, None) }.is_err() || n == 0 {
        return Vec::new();
    }
    let mut v = vec![POINTER_PEN_INFO::default(); n as usize];
    // SAFETY: `v` has room for `n` entries.
    if unsafe { GetPointerPenInfoHistory(id, &mut n, Some(v.as_mut_ptr())) }.is_err() {
        return Vec::new();
    }
    v.truncate(n as usize);
    v.iter()
        .rev()
        .map(|p| {
            let mut s = base(&p.pointerInfo, map);
            if p.penMask & PEN_MASK_PRESSURE != 0 {
                s.pressure = Some(p.pressure as f32 / 1024.0);
            }
            if p.penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y) != 0 {
                s.tilt = Some((p.tiltX as f32, p.tiltY as f32));
            }
            if p.penFlags & PEN_FLAG_BARREL != 0 {
                s.buttons |= BUTTON_STYLUS_PRIMARY;
            }
            s.eraser = p.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0;
            PointerFrame {
                time_ns: time_ns(&p.pointerInfo),
                pointers: vec![s],
            }
        })
        .collect()
}

fn read_touch(id: u32, map: MapFn) -> Vec<PointerFrame> {
    let mut n = 0u32;
    // SAFETY: a size query with a valid out pointer.
    if unsafe { GetPointerFrameTouchInfo(id, &mut n, None) }.is_err() || n == 0 {
        return Vec::new();
    }
    let mut v = vec![POINTER_TOUCH_INFO::default(); n as usize];
    // SAFETY: `v` has room for `n` entries.
    if unsafe { GetPointerFrameTouchInfo(id, &mut n, Some(v.as_mut_ptr())) }.is_err() {
        return Vec::new();
    }
    v.truncate(n as usize);
    let time = v.first().map_or(0, |t| time_ns(&t.pointerInfo));
    let pointers = v
        .iter()
        .map(|t| {
            let mut s = base(&t.pointerInfo, map);
            if t.touchMask & TOUCH_MASK_PRESSURE != 0 {
                s.pressure = Some(t.pressure as f32 / 1024.0);
            }
            if t.touchMask & TOUCH_MASK_CONTACTAREA != 0 {
                let w = (t.rcContact.right - t.rcContact.left) as f32;
                let h = (t.rcContact.bottom - t.rcContact.top) as f32;
                s.contact_size = Some((w.max(h), w.min(h)));
            }
            s
        })
        .collect();
    vec![PointerFrame {
        time_ns: time,
        pointers,
    }]
}

/// The frame id of a pointer's current message, to process each touch
/// frame once (Windows sends one message per contact).
pub fn frame_id(pointer_id: u32) -> Option<u32> {
    let mut i = POINTER_INFO::default();
    // SAFETY: valid out pointer.
    unsafe { GetPointerInfo(pointer_id, &mut i) }.ok()?;
    Some(i.frameId)
}

/// Skips the pointer's remaining history: it was read with this message.
pub fn skip(pointer_id: u32) {
    // SAFETY: no pointers.
    unsafe {
        let _ = SkipPointerFrameMessages(pointer_id);
    }
}

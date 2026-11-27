//! Pointer input (mouse, touch, pen, touchpad gestures) to `MotionEvent`s.
//!
//! The Windows side reports, for each pointer device, frames of
//! [`PointerSample`]s: one entry per pointer Windows knows at that moment
//! (all touch contacts of a frame, or the one mouse or pen pointer), in the
//! window's coordinates. [`PointerTracker`] turns the changes between
//! frames into the event sequence Android's input reader produces:
//!
//! - **Touch**: `ACTION_DOWN` for the first contact, `ACTION_POINTER_DOWN`
//!   with the pointer index for more, `ACTION_MOVE` with every pointer,
//!   `ACTION_POINTER_UP`, and `ACTION_UP` for the last one. Pointer ids are
//!   the lowest free ids from 0, kept for the life of a contact.
//!   `ACTION_CANCEL` (with `FLAG_CANCELED`) when Windows cancels the
//!   contacts (for example an edge swipe taken by the system).
//! - **Mouse**: hover events while no button is down; on a press
//!   `ACTION_HOVER_EXIT`, `ACTION_DOWN`, `ACTION_BUTTON_PRESS`; on release
//!   `ACTION_BUTTON_RELEASE`, `ACTION_UP`, then hover again. The back and
//!   forward buttons also produce `KEYCODE_BACK`/`KEYCODE_FORWARD` key
//!   events, as Android does for mice. The wheel is `ACTION_SCROLL` with
//!   `AXIS_VSCROLL`/`AXIS_HSCROLL` in detents (fractions from smooth
//!   wheels and touchpads).
//! - **Pen**: like touch while in contact (tool type stylus, or eraser),
//!   hover while in range; pressure, tilt and orientation axes; the barrel
//!   button is `BUTTON_STYLUS_PRIMARY`.
//! - **Touchpad pinch**: two synthetic fingers with
//!   `CLASSIFICATION_PINCH` and `AXIS_GESTURE_PINCH_SCALE_FACTOR`, as
//!   Android reports touchpad pinches ([`PinchConverter`]).
//!
//! Consecutive moves within one call are merged into one event with
//! historical samples, as Android batches them.

use super::{
    InputEvent, KeyEvent, MotionEvent, MotionSample, PointerCoords, PointerProperties, device,
};
use crate::android::motion::*;
use crate::android::{key, keycode, source};

/// The kind of Windows pointer device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Mouse,
    Touch,
    Pen,
}

/// One pointer as Windows reports it in a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerSample {
    /// Windows' pointer id.
    pub id: u32,
    /// Window coordinates in physical pixels.
    pub x: f32,
    pub y: f32,
    /// Touching the surface (touch, pen). For a mouse: computed from the
    /// buttons, this field is ignored.
    pub in_contact: bool,
    /// Detected by the digitizer (pen hover) or over the window (mouse).
    pub in_range: bool,
    /// Windows cancelled the pointer.
    pub canceled: bool,
    /// Android `BUTTON_*` bits held (mouse buttons, pen barrel buttons).
    pub buttons: i32,
    /// Normalized pressure 0..1, when the device reports it.
    pub pressure: Option<f32>,
    /// Contact ellipse (major, minor) in pixels, when reported.
    pub contact_size: Option<(f32, f32)>,
    /// Pen tilt (x, y) in degrees, -90..90, positive to the right and
    /// towards the user.
    pub tilt: Option<(f32, f32)>,
    /// The pen's eraser end is used.
    pub eraser: bool,
}

/// All pointers of one device at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointerFrame {
    /// `CLOCK_MONOTONIC` nanoseconds.
    pub time_ns: i64,
    pub pointers: Vec<PointerSample>,
}

#[derive(Clone, Debug)]
struct Active {
    win_id: u32,
    id: i32,
    tool: i32,
    coords: PointerCoords,
}

/// Turns frames of one pointer device into Android events.
#[derive(Debug)]
pub struct PointerTracker {
    kind: PointerKind,
    active: Vec<Active>,
    hover: Option<(i32, PointerCoords)>,
    buttons: i32,
    down_time: i64,
    raw_offset: (f32, f32),
}

const CONTACT_BUTTONS: i32 = BUTTON_PRIMARY | BUTTON_SECONDARY | BUTTON_TERTIARY;

/// Pen tilt in degrees to Android's (tilt, orientation) in radians, the
/// way Android derives them from a digitizer's tilt axes.
pub fn tilt_orientation(tilt_x_deg: f32, tilt_y_deg: f32) -> (f32, f32) {
    let tx = tilt_x_deg.to_radians();
    let ty = tilt_y_deg.to_radians();
    let tilt = (tx.cos() * ty.cos()).clamp(-1.0, 1.0).acos();
    let orientation = (-tx.sin()).atan2(ty.sin());
    (tilt, orientation)
}

impl PointerTracker {
    pub fn new(kind: PointerKind) -> Self {
        Self {
            kind,
            active: Vec::new(),
            hover: None,
            buttons: 0,
            down_time: 0,
            raw_offset: (0.0, 0.0),
        }
    }

    /// The window's position on its display, for raw coordinates.
    pub fn set_raw_offset(&mut self, x: f32, y: f32) {
        self.raw_offset = (x, y);
    }

    pub fn device_id(&self) -> i32 {
        match self.kind {
            PointerKind::Mouse => device::MOUSE,
            PointerKind::Touch => device::TOUCHSCREEN,
            PointerKind::Pen => device::STYLUS,
        }
    }

    pub fn source(&self) -> u32 {
        match self.kind {
            PointerKind::Mouse => source::MOUSE,
            PointerKind::Touch => source::TOUCHSCREEN,
            PointerKind::Pen => source::TOUCHSCREEN | source::STYLUS,
        }
    }

    /// True while a gesture (contact) is in progress.
    pub fn is_down(&self) -> bool {
        !self.active.is_empty()
    }

    fn tool(&self, s: &PointerSample) -> i32 {
        match self.kind {
            PointerKind::Mouse => TOOL_TYPE_MOUSE,
            PointerKind::Touch => TOOL_TYPE_FINGER,
            PointerKind::Pen if s.eraser => TOOL_TYPE_ERASER,
            PointerKind::Pen => TOOL_TYPE_STYLUS,
        }
    }

    fn contact(&self, s: &PointerSample) -> bool {
        match self.kind {
            PointerKind::Mouse => s.buttons & CONTACT_BUTTONS != 0,
            _ => s.in_contact,
        }
    }

    fn coords(&self, s: &PointerSample, contact: bool) -> PointerCoords {
        let mut c = PointerCoords::xy(s.x, s.y);
        let pressure = match (self.kind, contact) {
            (_, false) => 0.0,
            (PointerKind::Mouse, true) => 1.0,
            (_, true) => s.pressure.unwrap_or(1.0),
        };
        c.set(AXIS_PRESSURE, pressure.clamp(0.0, 1.0));
        if let Some((major, minor)) = s.contact_size {
            c.set(AXIS_TOUCH_MAJOR, major);
            c.set(AXIS_TOUCH_MINOR, minor);
            c.set(AXIS_TOOL_MAJOR, major);
            c.set(AXIS_TOOL_MINOR, minor);
        }
        if let Some((tx, ty)) = s.tilt {
            let (tilt, orientation) = tilt_orientation(tx, ty);
            c.set(AXIS_TILT, tilt);
            c.set(AXIS_ORIENTATION, orientation);
        }
        c
    }

    fn event(
        &self,
        action: i32,
        time: i64,
        meta: i32,
        pointers: Vec<(PointerProperties, PointerCoords)>,
    ) -> MotionEvent {
        let (props, coords): (Vec<_>, Vec<_>) = pointers.into_iter().unzip();
        MotionEvent {
            device_id: self.device_id(),
            source: self.source(),
            action,
            action_button: 0,
            flags: 0,
            meta_state: meta,
            button_state: self.buttons,
            classification: CLASSIFICATION_NONE,
            down_time_ns: self.down_time,
            raw_offset: self.raw_offset,
            pointers: props,
            samples: vec![MotionSample {
                event_time_ns: time,
                coords,
            }],
        }
    }

    fn active_pointers(&self) -> Vec<(PointerProperties, PointerCoords)> {
        self.active
            .iter()
            .map(|a| {
                (
                    PointerProperties {
                        id: a.id,
                        tool_type: a.tool,
                    },
                    a.coords.clone(),
                )
            })
            .collect()
    }

    fn free_id(&self) -> i32 {
        (0..).find(|i| self.active.iter().all(|a| a.id != *i)).unwrap()
    }

    fn button_key(&self, button: i32, down: bool, time: i64, meta: i32) -> Option<KeyEvent> {
        let code = match button {
            BUTTON_BACK => keycode::BACK,
            BUTTON_FORWARD => keycode::FORWARD,
            _ => return None,
        };
        Some(KeyEvent {
            device_id: self.device_id(),
            source: source::KEYBOARD,
            action: if down {
                key::ACTION_DOWN
            } else {
                key::ACTION_UP
            },
            flags: 0,
            key_code: code,
            scan_code: 0,
            meta_state: meta,
            repeat_count: 0,
            down_time_ns: time,
            event_time_ns: time,
        })
    }

    /// Processes frames in order and returns the events, consecutive moves
    /// merged into one event with historical samples.
    pub fn process(&mut self, frames: &[PointerFrame], meta: i32) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for f in frames {
            self.frame(f, meta, &mut out);
        }
        merge_moves(out)
    }

    fn frame(&mut self, f: &PointerFrame, meta: i32, out: &mut Vec<InputEvent>) {
        let t = f.time_ns;
        if f.pointers.iter().any(|s| s.canceled) {
            if self.is_down() {
                let mut e = self.event(ACTION_CANCEL, t, meta, self.active_pointers());
                e.flags |= FLAG_CANCELED;
                out.push(InputEvent::Motion(e));
            }
            self.active.clear();
            self.hover = None;
            self.buttons = 0;
            return;
        }

        let old_buttons = self.buttons;
        let new_buttons = f.pointers.iter().fold(0, |b, s| b | s.buttons);
        let contacts: Vec<&PointerSample> =
            f.pointers.iter().filter(|s| self.contact(s)).collect();

        // Hover exit before the first contact.
        if !contacts.is_empty()
            && let Some((tool, c)) = self.hover.take()
        {
            let s = contacts[0];
            let mut c = c;
            c.set(AXIS_X, s.x);
            c.set(AXIS_Y, s.y);
            let props = PointerProperties {
                id: 0,
                tool_type: tool,
            };
            out.push(InputEvent::Motion(self.event(
                ACTION_HOVER_EXIT,
                t,
                meta,
                vec![(props, c)],
            )));
        }

        // Moves of pointers that were down, with their new coordinates.
        let mut moved = false;
        for i in 0..self.active.len() {
            let win_id = self.active[i].win_id;
            if let Some(s) = f.pointers.iter().find(|s| s.id == win_id) {
                let still = self.contact(s);
                let mut c = self.coords(s, true);
                if !still {
                    // Lifting: keep the last pressure for the up event.
                    c.set(AXIS_PRESSURE, self.active[i].coords.get(AXIS_PRESSURE));
                }
                if c != self.active[i].coords {
                    moved = true;
                    self.active[i].coords = c;
                }
            }
        }
        if moved {
            out.push(InputEvent::Motion(self.event(
                ACTION_MOVE,
                t,
                meta,
                self.active_pointers(),
            )));
        }

        // Button releases (before the up they cause).
        let released = old_buttons & !new_buttons;
        for bit in bits(released) {
            self.buttons &= !bit;
            if self.is_down() || self.kind == PointerKind::Mouse {
                let pointers = self.current_or_hover_pointer(f);
                if !pointers.is_empty() {
                    let mut e = self.event(ACTION_BUTTON_RELEASE, t, meta, pointers);
                    e.action_button = bit;
                    out.push(InputEvent::Motion(e));
                }
            }
            if let Some(k) = self.button_key(bit, false, t, meta) {
                out.push(InputEvent::Key(k));
            }
        }

        // Ups.
        while let Some(i) = self
            .active
            .iter()
            .position(|a| !contacts.iter().any(|s| s.id == a.win_id))
        {
            let action = if self.active.len() == 1 {
                ACTION_UP
            } else {
                with_index(ACTION_POINTER_UP, i)
            };
            out.push(InputEvent::Motion(self.event(
                action,
                t,
                meta,
                self.active_pointers(),
            )));
            self.active.remove(i);
        }

        // Downs.
        for s in &contacts {
            if self.active.iter().any(|a| a.win_id == s.id) {
                continue;
            }
            if self.active.is_empty() {
                self.down_time = t;
            }
            // The down event carries the buttons that caused it.
            self.buttons |= new_buttons & CONTACT_BUTTONS | (new_buttons & !old_buttons);
            let a = Active {
                win_id: s.id,
                id: self.free_id(),
                tool: self.tool(s),
                coords: self.coords(s, true),
            };
            self.active.push(a);
            let action = if self.active.len() == 1 {
                ACTION_DOWN
            } else {
                with_index(ACTION_POINTER_DOWN, self.active.len() - 1)
            };
            out.push(InputEvent::Motion(self.event(
                action,
                t,
                meta,
                self.active_pointers(),
            )));
        }

        // Button presses (after the down they cause).
        let pressed = new_buttons & !old_buttons;
        self.buttons = new_buttons;
        for bit in bits(pressed) {
            let pointers = self.current_or_hover_pointer(f);
            if !pointers.is_empty() {
                let mut e = self.event(ACTION_BUTTON_PRESS, t, meta, pointers);
                e.action_button = bit;
                // Only the buttons pressed so far in this frame.
                e.button_state = (old_buttons & new_buttons) | (pressed & ((bit << 1) - 1));
                out.push(InputEvent::Motion(e));
            }
            if let Some(k) = self.button_key(bit, true, t, meta) {
                out.push(InputEvent::Key(k));
            }
        }

        // Hover.
        if self.kind != PointerKind::Touch && contacts.is_empty() {
            match f.pointers.iter().find(|s| s.in_range) {
                Some(s) => {
                    let tool = self.tool(s);
                    let c = self.coords(s, false);
                    let action = match &self.hover {
                        None => Some(ACTION_HOVER_ENTER),
                        Some((_, old)) if *old != c => Some(ACTION_HOVER_MOVE),
                        Some(_) => None,
                    };
                    if let Some(action) = action {
                        let props = PointerProperties {
                            id: 0,
                            tool_type: tool,
                        };
                        out.push(InputEvent::Motion(self.event(
                            action,
                            t,
                            meta,
                            vec![(props, c.clone())],
                        )));
                    }
                    self.hover = Some((tool, c));
                }
                None => {
                    if let Some((tool, c)) = self.hover.take() {
                        let props = PointerProperties {
                            id: 0,
                            tool_type: tool,
                        };
                        out.push(InputEvent::Motion(self.event(
                            ACTION_HOVER_EXIT,
                            t,
                            meta,
                            vec![(props, c)],
                        )));
                    }
                }
            }
        }
    }

    /// The pointer a button event is about: the down pointers, or the
    /// hovering mouse.
    fn current_or_hover_pointer(&self, f: &PointerFrame) -> Vec<(PointerProperties, PointerCoords)> {
        if self.is_down() {
            return self.active_pointers();
        }
        f.pointers
            .iter()
            .find(|s| s.in_range || self.kind == PointerKind::Mouse)
            .map(|s| {
                (
                    PointerProperties {
                        id: 0,
                        tool_type: self.tool(s),
                    },
                    self.coords(s, false),
                )
            })
            .into_iter()
            .collect()
    }

    /// A wheel or touchpad scroll at (x, y): `ACTION_SCROLL` with the
    /// amounts in detents (positive: up and right).
    pub fn scroll(&self, time_ns: i64, x: f32, y: f32, v: f32, h: f32, meta: i32) -> MotionEvent {
        let mut c = PointerCoords::xy(x, y);
        c.set(AXIS_VSCROLL, v);
        c.set(AXIS_HSCROLL, h);
        let mut e = self.event(
            ACTION_SCROLL,
            time_ns,
            meta,
            vec![(
                PointerProperties {
                    id: 0,
                    tool_type: TOOL_TYPE_MOUSE,
                },
                c,
            )],
        );
        e.down_time_ns = time_ns;
        e
    }
}

fn bits(mask: i32) -> impl Iterator<Item = i32> {
    (0..31).map(|b| 1 << b).filter(move |b| mask & b != 0)
}

/// Merges runs of `ACTION_MOVE` events with the same pointers into one
/// event with historical samples.
pub fn merge_moves(events: Vec<InputEvent>) -> Vec<InputEvent> {
    let mut out: Vec<InputEvent> = Vec::with_capacity(events.len());
    for e in events {
        if let InputEvent::Motion(m) = &e
            && m.action == ACTION_MOVE
            && let Some(InputEvent::Motion(prev)) = out.last_mut()
            && prev.action == ACTION_MOVE
            && prev.pointers == m.pointers
            && prev.device_id == m.device_id
            && prev.button_state == m.button_state
            && prev.meta_state == m.meta_state
        {
            prev.samples.extend(m.samples.iter().cloned());
            continue;
        }
        out.push(e);
    }
    out
}

/// The phase of a touchpad pinch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchPhase {
    Begin,
    Update,
    End,
}

/// Turns a touchpad pinch (focus point and scale) into the two-finger
/// events Android produces for touchpad pinches.
#[derive(Debug, Default)]
pub struct PinchConverter {
    down_time: i64,
    focus: (f32, f32),
    span: f32,
    active: bool,
    raw_offset: (f32, f32),
}

/// The initial distance between the two synthetic fingers, in pixels.
pub const PINCH_INITIAL_SPAN: f32 = 200.0;

impl PinchConverter {
    pub fn set_raw_offset(&mut self, x: f32, y: f32) {
        self.raw_offset = (x, y);
    }

    fn event(&self, action: i32, t: i64, meta: i32, factor: f32) -> MotionEvent {
        let (fx, fy) = self.focus;
        let half = self.span / 2.0;
        let coords = [fx - half, fx + half]
            .into_iter()
            .map(|x| {
                PointerCoords::xy(x, fy)
                    .with(AXIS_PRESSURE, 1.0)
                    .with(AXIS_GESTURE_PINCH_SCALE_FACTOR, factor)
            })
            .collect::<Vec<_>>();
        let n = if action == ACTION_DOWN { 1 } else { 2 };
        MotionEvent {
            device_id: device::TOUCHPAD,
            source: source::MOUSE,
            action,
            action_button: 0,
            flags: 0,
            meta_state: meta,
            button_state: 0,
            classification: CLASSIFICATION_PINCH,
            down_time_ns: self.down_time,
            raw_offset: self.raw_offset,
            pointers: (0..n)
                .map(|id| PointerProperties {
                    id,
                    tool_type: TOOL_TYPE_FINGER,
                })
                .collect(),
            samples: vec![MotionSample {
                event_time_ns: t,
                coords: coords.into_iter().take(n as usize).collect(),
            }],
        }
    }

    /// `scale` is the change since the previous call (1.0 = none).
    pub fn pinch(
        &mut self,
        phase: PinchPhase,
        time_ns: i64,
        focus: (f32, f32),
        scale: f32,
        meta: i32,
    ) -> Vec<MotionEvent> {
        let mut out = Vec::new();
        match phase {
            PinchPhase::Begin => {
                self.down_time = time_ns;
                self.focus = focus;
                self.span = PINCH_INITIAL_SPAN;
                self.active = true;
                out.push(self.event(ACTION_DOWN, time_ns, meta, 1.0));
                out.push(self.event(with_index(ACTION_POINTER_DOWN, 1), time_ns, meta, 1.0));
            }
            PinchPhase::Update if self.active => {
                self.focus = focus;
                self.span *= scale;
                out.push(self.event(ACTION_MOVE, time_ns, meta, scale));
            }
            PinchPhase::End if self.active => {
                self.active = false;
                out.push(self.event(with_index(ACTION_POINTER_UP, 1), time_ns, meta, 1.0));
                let mut up = self.event(ACTION_UP, time_ns, meta, 1.0);
                up.pointers.truncate(1);
                up.samples[0].coords.truncate(1);
                out.push(up);
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(id: u32, x: f32, y: f32) -> PointerSample {
        PointerSample {
            id,
            x,
            y,
            in_contact: true,
            in_range: true,
            ..Default::default()
        }
    }

    fn lifted(id: u32, x: f32, y: f32) -> PointerSample {
        PointerSample {
            in_contact: false,
            ..touch(id, x, y)
        }
    }

    fn frame(t: i64, p: Vec<PointerSample>) -> PointerFrame {
        PointerFrame {
            time_ns: t,
            pointers: p,
        }
    }

    fn motions(ev: &[InputEvent]) -> Vec<&MotionEvent> {
        ev.iter()
            .filter_map(|e| match e {
                InputEvent::Motion(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    /// (action, pointer ids, button state) of each motion event.
    fn summary(ev: &[InputEvent]) -> Vec<(i32, Vec<i32>, i32)> {
        motions(ev)
            .iter()
            .map(|m| {
                (
                    m.action,
                    m.pointers.iter().map(|p| p.id).collect(),
                    m.button_state,
                )
            })
            .collect()
    }

    #[test]
    fn two_finger_touch_sequence() {
        let mut t = PointerTracker::new(PointerKind::Touch);
        let frames = [
            frame(10, vec![touch(101, 10.0, 10.0)]),
            frame(20, vec![touch(101, 12.0, 10.0), touch(102, 50.0, 50.0)]),
            frame(30, vec![touch(101, 14.0, 10.0), touch(102, 52.0, 50.0)]),
            // First finger lifts: pointer up with index 0, id 0.
            frame(40, vec![lifted(101, 14.0, 10.0), touch(102, 54.0, 50.0)]),
            // A new finger gets the free id 0 again, at index 1.
            frame(50, vec![touch(102, 54.0, 50.0), touch(103, 80.0, 80.0)]),
            frame(60, vec![lifted(102, 54.0, 50.0), touch(103, 80.0, 80.0)]),
            frame(70, vec![lifted(103, 80.0, 80.0)]),
        ];
        let ev = t.process(&frames, 0);
        assert_eq!(
            summary(&ev),
            vec![
                (ACTION_DOWN, vec![0], 0),
                (ACTION_MOVE, vec![0], 0),
                (with_index(ACTION_POINTER_DOWN, 1), vec![0, 1], 0),
                (ACTION_MOVE, vec![0, 1], 0),
                (with_index(ACTION_POINTER_UP, 0), vec![0, 1], 0),
                (with_index(ACTION_POINTER_DOWN, 1), vec![1, 0], 0),
                (with_index(ACTION_POINTER_UP, 0), vec![1, 0], 0),
                (ACTION_UP, vec![0], 0),
            ]
        );
        let m = motions(&ev);
        assert!(m.iter().all(|e| e.down_time_ns == 10));
        assert!(m.iter().all(|e| e.source == source::TOUCHSCREEN));
        assert_eq!(m[0].current()[0].get(AXIS_PRESSURE), 1.0);
        assert_eq!(m[0].pointers[0].tool_type, TOOL_TYPE_FINGER);
        // The moves of frames 30 and 40 are one event with two samples.
        assert_eq!(m[3].samples.len(), 2);
        assert_eq!(m[7].current()[0].x(), 80.0);
        assert!(!t.is_down());
    }

    #[test]
    fn consecutive_moves_become_historical_samples() {
        let mut t = PointerTracker::new(PointerKind::Touch);
        let frames: Vec<_> = (0..5)
            .map(|i| frame(i * 4, vec![touch(7, i as f32, 0.0)]))
            .collect();
        let ev = t.process(&frames, 0);
        let m = motions(&ev);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].action, ACTION_MOVE);
        let times: Vec<_> = m[1].samples.iter().map(|s| s.event_time_ns).collect();
        assert_eq!(times, vec![4, 8, 12, 16]);
        assert_eq!(m[1].current()[0].x(), 4.0);
    }

    #[test]
    fn cancel_ends_the_gesture() {
        let mut t = PointerTracker::new(PointerKind::Touch);
        let mut c = touch(1, 5.0, 5.0);
        let ev = t.process(&[frame(1, vec![c])], 0);
        assert_eq!(summary(&ev), vec![(ACTION_DOWN, vec![0], 0)]);
        c.canceled = true;
        let ev = t.process(&[frame(2, vec![c])], 0);
        let m = motions(&ev);
        assert_eq!(m[0].action, ACTION_CANCEL);
        assert_eq!(m[0].flags & FLAG_CANCELED, FLAG_CANCELED);
        assert!(!t.is_down());
    }

    fn mouse(x: f32, buttons: i32) -> PointerSample {
        PointerSample {
            id: 1,
            x,
            y: 0.0,
            in_range: true,
            buttons,
            ..Default::default()
        }
    }

    #[test]
    fn mouse_click_hover_and_buttons() {
        let mut t = PointerTracker::new(PointerKind::Mouse);
        let frames = [
            frame(1, vec![mouse(1.0, 0)]),
            frame(2, vec![mouse(2.0, 0)]),
            frame(3, vec![mouse(2.0, BUTTON_PRIMARY)]),
            frame(4, vec![mouse(3.0, BUTTON_PRIMARY)]),
            frame(5, vec![mouse(3.0, BUTTON_PRIMARY | BUTTON_SECONDARY)]),
            frame(6, vec![mouse(3.0, BUTTON_SECONDARY)]),
            frame(7, vec![mouse(3.0, 0)]),
            frame(8, vec![PointerSample {
                in_range: false,
                ..mouse(3.0, 0)
            }]),
        ];
        let ev = t.process(&frames, 0);
        let p = BUTTON_PRIMARY;
        let s = BUTTON_SECONDARY;
        assert_eq!(
            summary(&ev),
            vec![
                (ACTION_HOVER_ENTER, vec![0], 0),
                (ACTION_HOVER_MOVE, vec![0], 0),
                (ACTION_HOVER_EXIT, vec![0], 0),
                (ACTION_DOWN, vec![0], p),
                (ACTION_BUTTON_PRESS, vec![0], p),
                (ACTION_MOVE, vec![0], p),
                (ACTION_BUTTON_PRESS, vec![0], p | s),
                (ACTION_BUTTON_RELEASE, vec![0], s),
                (ACTION_BUTTON_RELEASE, vec![0], 0),
                (ACTION_UP, vec![0], 0),
                (ACTION_HOVER_ENTER, vec![0], 0),
                (ACTION_HOVER_EXIT, vec![0], 0),
            ]
        );
        let m = motions(&ev);
        assert!(m.iter().all(|e| e.source == source::MOUSE));
        assert!(m.iter().all(|e| e.pointers[0].tool_type == TOOL_TYPE_MOUSE));
        assert_eq!(m[4].action_button, p);
        assert_eq!(m[6].action_button, s);
        assert_eq!(m[7].action_button, p);
        assert_eq!(m[3].current()[0].get(AXIS_PRESSURE), 1.0);
        assert_eq!(m[0].current()[0].get(AXIS_PRESSURE), 0.0);
        assert_eq!(m[3].down_time_ns, 3);
    }

    #[test]
    fn mouse_back_button_is_also_a_back_key() {
        let mut t = PointerTracker::new(PointerKind::Mouse);
        let ev = t.process(
            &[
                frame(1, vec![mouse(1.0, 0)]),
                frame(2, vec![mouse(1.0, BUTTON_BACK)]),
                frame(3, vec![mouse(1.0, 0)]),
            ],
            0,
        );
        let keys: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                InputEvent::Key(k) => Some((k.action, k.key_code)),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (key::ACTION_DOWN, keycode::BACK),
                (key::ACTION_UP, keycode::BACK)
            ]
        );
        // No down: the back button is not a contact button.
        assert!(!summary(&ev).iter().any(|s| s.0 == ACTION_DOWN));
        assert!(summary(&ev).iter().any(|s| s.0 == ACTION_BUTTON_PRESS));
    }

    #[test]
    fn pen_hover_contact_pressure_tilt() {
        let mut t = PointerTracker::new(PointerKind::Pen);
        let pen = |contact: bool, p: f32, buttons: i32| PointerSample {
            id: 9,
            x: 5.0,
            y: 5.0,
            in_contact: contact,
            in_range: true,
            pressure: Some(p),
            tilt: Some((30.0, 0.0)),
            buttons,
            ..Default::default()
        };
        let ev = t.process(
            &[
                frame(1, vec![pen(false, 0.0, 0)]),
                frame(2, vec![pen(true, 0.25, 0)]),
                frame(3, vec![pen(true, 0.75, BUTTON_STYLUS_PRIMARY)]),
                frame(4, vec![pen(false, 0.0, 0)]),
            ],
            0,
        );
        let sp = BUTTON_STYLUS_PRIMARY;
        assert_eq!(
            summary(&ev),
            vec![
                (ACTION_HOVER_ENTER, vec![0], 0),
                (ACTION_HOVER_EXIT, vec![0], 0),
                (ACTION_DOWN, vec![0], 0),
                (ACTION_MOVE, vec![0], 0),
                (ACTION_BUTTON_PRESS, vec![0], sp),
                (ACTION_BUTTON_RELEASE, vec![0], 0),
                (ACTION_UP, vec![0], 0),
                (ACTION_HOVER_ENTER, vec![0], 0),
            ]
        );
        let m = motions(&ev);
        assert_eq!(m[0].source, source::TOUCHSCREEN | source::STYLUS);
        assert_eq!(m[2].pointers[0].tool_type, TOOL_TYPE_STYLUS);
        assert_eq!(m[2].current()[0].get(AXIS_PRESSURE), 0.25);
        assert_eq!(m[3].current()[0].get(AXIS_PRESSURE), 0.75);
        let tilt = m[2].current()[0].get(AXIS_TILT);
        assert!((tilt - 30f32.to_radians()).abs() < 1e-5);
        // Tilted to the right: orientation -90 degrees.
        let o = m[2].current()[0].get(AXIS_ORIENTATION);
        assert!((o + std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn eraser_tool() {
        let mut t = PointerTracker::new(PointerKind::Pen);
        let s = PointerSample {
            in_contact: true,
            in_range: true,
            eraser: true,
            ..Default::default()
        };
        let ev = t.process(&[frame(1, vec![s])], 0);
        assert_eq!(motions(&ev)[0].pointers[0].tool_type, TOOL_TYPE_ERASER);
    }

    #[test]
    fn tilt_table() {
        // (tilt x, tilt y) -> (tilt, orientation) in degrees.
        for (tx, ty, tilt, o) in [
            (0.0, 0.0, 0.0, 0.0),
            (0.0, 45.0, 45.0, 0.0),
            (0.0, -45.0, 45.0, 180.0),
            (45.0, 0.0, 45.0, -90.0),
            (-45.0, 0.0, 45.0, 90.0),
        ] {
            let (a, b) = tilt_orientation(tx, ty);
            assert!((a.to_degrees() - tilt).abs() < 1e-3, "{tx} {ty}");
            assert!(
                (b.to_degrees() - o).abs() < 1e-3 || (b.to_degrees().abs() - 180.0).abs() < 1e-3 && o == 180.0,
                "{tx} {ty}: {}",
                b.to_degrees()
            );
        }
    }

    #[test]
    fn wheel_scroll() {
        let t = PointerTracker::new(PointerKind::Mouse);
        let e = t.scroll(42, 10.0, 20.0, 1.0, -0.5, 0x1000);
        assert_eq!(e.action, ACTION_SCROLL);
        assert_eq!(e.current()[0].get(AXIS_VSCROLL), 1.0);
        assert_eq!(e.current()[0].get(AXIS_HSCROLL), -0.5);
        assert_eq!(e.meta_state, 0x1000);
        assert_eq!(e.event_time_ns(), 42);
    }

    #[test]
    fn pinch_produces_two_synthetic_fingers() {
        let mut p = PinchConverter::default();
        let mut ev = p.pinch(PinchPhase::Begin, 1, (100.0, 100.0), 1.0, 0);
        ev.extend(p.pinch(PinchPhase::Update, 2, (100.0, 100.0), 1.5, 0));
        ev.extend(p.pinch(PinchPhase::End, 3, (100.0, 100.0), 1.0, 0));
        let actions: Vec<_> = ev.iter().map(|e| e.action).collect();
        assert_eq!(
            actions,
            vec![
                ACTION_DOWN,
                with_index(ACTION_POINTER_DOWN, 1),
                ACTION_MOVE,
                with_index(ACTION_POINTER_UP, 1),
                ACTION_UP
            ]
        );
        assert!(ev.iter().all(|e| e.classification == CLASSIFICATION_PINCH));
        let mv = &ev[2];
        assert_eq!(mv.current()[0].get(AXIS_GESTURE_PINCH_SCALE_FACTOR), 1.5);
        let span = mv.current()[1].x() - mv.current()[0].x();
        assert_eq!(span, PINCH_INITIAL_SPAN * 1.5);
        assert_eq!(ev[4].pointers.len(), 1);
    }
}

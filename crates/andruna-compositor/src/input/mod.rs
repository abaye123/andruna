//! Input as Android applications see it.
//!
//! The Windows side reads pointer, keyboard and IME messages and turns
//! them into the neutral samples of [`pointer`] and [`keys`]. The pure
//! state machines in those modules produce [`MotionEvent`]s and
//! [`KeyEvent`]s with Android's field semantics (actions, pointer ids,
//! axes, source, button and meta state, times on the uptime clock), and
//! [`wire`](crate::wire) serializes them for the app process.
//!
//! Devices: each kind of Windows input is one Android input device with a
//! fixed id ([`device`]). Event times are `CLOCK_MONOTONIC` nanoseconds,
//! the clock `SystemClock.uptimeMillis` and `MotionEvent.getEventTime` use
//! ([`clock`](crate::clock)).

pub mod kcm;
pub mod keys;
pub mod pointer;

use crate::android::motion;

/// The input devices the compositor presents, with their Android ids.
pub mod device {
    /// `KeyCharacterMap.VIRTUAL_KEYBOARD`: keys the system generates (the
    /// title bar back button, system shortcuts).
    pub const VIRTUAL_KEYBOARD: i32 = -1;
    pub const KEYBOARD: i32 = 1;
    pub const MOUSE: i32 = 2;
    pub const TOUCHSCREEN: i32 = 3;
    pub const STYLUS: i32 = 4;
    pub const TOUCHPAD: i32 = 5;
}

/// `MotionEvent.PointerProperties`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerProperties {
    pub id: i32,
    pub tool_type: i32,
}

/// `MotionEvent.PointerCoords`: axis values, kept sorted by axis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointerCoords {
    axes: Vec<(u32, f32)>,
}

impl PointerCoords {
    pub fn xy(x: f32, y: f32) -> Self {
        let mut c = Self::default();
        c.set(motion::AXIS_X, x);
        c.set(motion::AXIS_Y, y);
        c
    }

    /// Sets an axis. A zero value is stored like any other (Android drops
    /// zero values from the bit set; the wire format keeps what is set).
    pub fn set(&mut self, axis: u32, value: f32) {
        match self.axes.binary_search_by_key(&axis, |a| a.0) {
            Ok(i) => self.axes[i].1 = value,
            Err(i) => self.axes.insert(i, (axis, value)),
        }
    }

    pub fn with(mut self, axis: u32, value: f32) -> Self {
        self.set(axis, value);
        self
    }

    /// The axis value, 0 when not set (as `PointerCoords.getAxisValue`).
    pub fn get(&self, axis: u32) -> f32 {
        self.axes
            .binary_search_by_key(&axis, |a| a.0)
            .map(|i| self.axes[i].1)
            .unwrap_or(0.0)
    }

    pub fn x(&self) -> f32 {
        self.get(motion::AXIS_X)
    }

    pub fn y(&self) -> f32 {
        self.get(motion::AXIS_Y)
    }

    pub fn axes(&self) -> &[(u32, f32)] {
        &self.axes
    }

    pub fn from_axes(mut axes: Vec<(u32, f32)>) -> Self {
        axes.sort_by_key(|a| a.0);
        axes.dedup_by_key(|a| a.0);
        Self { axes }
    }
}

/// One sample of a motion event: its time and one coordinate set per
/// pointer. Older samples are `MotionEvent`'s historical samples.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionSample {
    pub event_time_ns: i64,
    pub coords: Vec<PointerCoords>,
}

/// `android.view.MotionEvent`. Coordinates are in the window's pixels;
/// `raw_offset` is added to get display coordinates (`getRawX`).
#[derive(Clone, Debug, PartialEq)]
pub struct MotionEvent {
    pub device_id: i32,
    pub source: u32,
    pub action: i32,
    pub action_button: i32,
    pub flags: i32,
    pub meta_state: i32,
    pub button_state: i32,
    pub classification: i32,
    pub down_time_ns: i64,
    pub raw_offset: (f32, f32),
    pub pointers: Vec<PointerProperties>,
    /// At least one; the last is the current sample.
    pub samples: Vec<MotionSample>,
}

impl MotionEvent {
    pub fn action_masked(&self) -> i32 {
        self.action & motion::ACTION_MASK
    }

    pub fn action_index(&self) -> usize {
        ((self.action >> motion::ACTION_POINTER_INDEX_SHIFT) & 0xff) as usize
    }

    pub fn event_time_ns(&self) -> i64 {
        self.samples.last().map_or(0, |s| s.event_time_ns)
    }

    pub fn current(&self) -> &[PointerCoords] {
        self.samples.last().map_or(&[], |s| &s.coords)
    }
}

/// `android.view.KeyEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub device_id: i32,
    pub source: u32,
    pub action: i32,
    pub flags: i32,
    pub key_code: i32,
    /// The Linux (evdev) scan code, as Android reports it.
    pub scan_code: i32,
    pub meta_state: i32,
    pub repeat_count: i32,
    pub down_time_ns: i64,
    pub event_time_ns: i64,
}

/// What a Windows input method does to the focused text field, in
/// `InputConnection` terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// `setComposingText(text, newCursorPosition)`; `cursor` is the caret
    /// inside the composition in UTF-16 units, and `target` the clause the
    /// IME is converting (start, end), if any.
    Composing {
        text: String,
        cursor: i32,
        target: Option<(i32, i32)>,
    },
    /// `commitText(text, 1)`: replaces the composition (if any) with
    /// `text`.
    Commit { text: String },
    /// `finishComposingText()`: keep the composition as it is.
    FinishComposing,
}

/// Everything the compositor delivers to a window of an app.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Motion(MotionEvent),
    Key(KeyEvent),
    Ime(ImeEvent),
    /// The window gained or lost input focus.
    Focus {
        has_focus: bool,
    },
}

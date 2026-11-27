//! Keyboard input to `KeyEvent`s.
//!
//! Key codes come from the key's **position** (its scan code), not from
//! the Windows keyboard layout, as on Android with a hardware keyboard:
//! the scan code maps to a Linux key code, and that to an Android key code
//! by the generic key layout. Characters come from the layout through the
//! key character map ([`kcm`](super::kcm)), which the compositor generates
//! from the active Windows layout. With the Hebrew layout, the key left of
//! `S` is `KEYCODE_A` and types `ש`; `Ctrl+C` is still `KEYCODE_C`, so app
//! shortcuts work in every layout.
//!
//! Scan codes are Windows' set 1 codes with `0xE000` added for extended
//! (E0-prefixed) keys and `0xE11D` for Pause ([`SCAN_PAUSE`]).
//!
//! System keys: `Alt+Left` is Back ([`D-12`](../../../../docs/spec/01-product.md)),
//! sent as `KEYCODE_BACK` from the virtual keyboard with `FLAG_FROM_SYSTEM`.
//! `Esc` reaches the app as `KEYCODE_ESCAPE`; an app that does not handle it
//! gets Back through the framework's fallback key for Escape.

use super::{KeyEvent, device};
use crate::android::key::*;
use crate::android::keycode as kc;
use crate::android::source;

/// The scan code Windows gives Pause (E1 1D 45), as used in this module.
pub const SCAN_PAUSE: u16 = 0xE11D;

/// Scan code (set 1, `0xE0xx` for extended keys) to (Linux key code,
/// Android key code).
pub fn map_scan_code(scan: u16) -> Option<(i32, i32)> {
    let k = match scan {
        0x01 => (1, kc::ESCAPE),
        0x02 => (2, kc::NUM_1),
        0x03 => (3, kc::NUM_2),
        0x04 => (4, kc::NUM_3),
        0x05 => (5, kc::NUM_4),
        0x06 => (6, kc::NUM_5),
        0x07 => (7, kc::NUM_6),
        0x08 => (8, kc::NUM_7),
        0x09 => (9, kc::NUM_8),
        0x0A => (10, kc::NUM_9),
        0x0B => (11, kc::NUM_0),
        0x0C => (12, kc::MINUS),
        0x0D => (13, kc::EQUALS),
        0x0E => (14, kc::DEL),
        0x0F => (15, kc::TAB),
        0x10 => (16, kc::Q),
        0x11 => (17, kc::W),
        0x12 => (18, kc::E),
        0x13 => (19, kc::R),
        0x14 => (20, kc::T),
        0x15 => (21, kc::Y),
        0x16 => (22, kc::U),
        0x17 => (23, kc::I),
        0x18 => (24, kc::O),
        0x19 => (25, kc::P),
        0x1A => (26, kc::LEFT_BRACKET),
        0x1B => (27, kc::RIGHT_BRACKET),
        0x1C => (28, kc::ENTER),
        0x1D => (29, kc::CTRL_LEFT),
        0x1E => (30, kc::A),
        0x1F => (31, kc::S),
        0x20 => (32, kc::D),
        0x21 => (33, kc::F),
        0x22 => (34, kc::G),
        0x23 => (35, kc::H),
        0x24 => (36, kc::J),
        0x25 => (37, kc::K),
        0x26 => (38, kc::L),
        0x27 => (39, kc::SEMICOLON),
        0x28 => (40, kc::APOSTROPHE),
        0x29 => (41, kc::GRAVE),
        0x2A => (42, kc::SHIFT_LEFT),
        0x2B => (43, kc::BACKSLASH),
        0x2C => (44, kc::Z),
        0x2D => (45, kc::X),
        0x2E => (46, kc::C),
        0x2F => (47, kc::V),
        0x30 => (48, kc::B),
        0x31 => (49, kc::N),
        0x32 => (50, kc::M),
        0x33 => (51, kc::COMMA),
        0x34 => (52, kc::PERIOD),
        0x35 => (53, kc::SLASH),
        0x36 => (54, kc::SHIFT_RIGHT),
        0x37 => (55, kc::NUMPAD_MULTIPLY),
        0x38 => (56, kc::ALT_LEFT),
        0x39 => (57, kc::SPACE),
        0x3A => (58, kc::CAPS_LOCK),
        0x3B => (59, kc::F1),
        0x3C => (60, kc::F2),
        0x3D => (61, kc::F3),
        0x3E => (62, kc::F4),
        0x3F => (63, kc::F5),
        0x40 => (64, kc::F6),
        0x41 => (65, kc::F7),
        0x42 => (66, kc::F8),
        0x43 => (67, kc::F9),
        0x44 => (68, kc::F10),
        0x45 => (69, kc::NUM_LOCK),
        0x46 => (70, kc::SCROLL_LOCK),
        0x47 => (71, kc::NUMPAD_7),
        0x48 => (72, kc::NUMPAD_8),
        0x49 => (73, kc::NUMPAD_9),
        0x4A => (74, kc::NUMPAD_SUBTRACT),
        0x4B => (75, kc::NUMPAD_4),
        0x4C => (76, kc::NUMPAD_5),
        0x4D => (77, kc::NUMPAD_6),
        0x4E => (78, kc::NUMPAD_ADD),
        0x4F => (79, kc::NUMPAD_1),
        0x50 => (80, kc::NUMPAD_2),
        0x51 => (81, kc::NUMPAD_3),
        0x52 => (82, kc::NUMPAD_0),
        0x53 => (83, kc::NUMPAD_DOT),
        // The ISO key between left Shift and Z.
        0x56 => (86, kc::BACKSLASH),
        0x57 => (87, kc::F11),
        0x58 => (88, kc::F12),
        0x59 => (117, kc::NUMPAD_EQUALS),
        0x70 => (93, kc::KATAKANA_HIRAGANA),
        0x73 => (89, kc::RO),
        0x79 => (92, kc::HENKAN),
        0x7B => (94, kc::MUHENKAN),
        0x7D => (124, kc::YEN),
        0x7E => (121, kc::NUMPAD_COMMA),
        0xE010 => (165, kc::MEDIA_PREVIOUS),
        0xE019 => (163, kc::MEDIA_NEXT),
        0xE01C => (96, kc::NUMPAD_ENTER),
        0xE01D => (97, kc::CTRL_RIGHT),
        0xE020 => (113, kc::VOLUME_MUTE),
        0xE021 => (140, kc::CALCULATOR),
        0xE022 => (164, kc::MEDIA_PLAY_PAUSE),
        0xE024 => (166, kc::MEDIA_STOP),
        0xE02E => (114, kc::VOLUME_DOWN),
        0xE030 => (115, kc::VOLUME_UP),
        0xE035 => (98, kc::NUMPAD_DIVIDE),
        0xE037 => (99, kc::SYSRQ),
        0xE038 => (100, kc::ALT_RIGHT),
        // Ctrl+Break.
        0xE046 => (119, kc::BREAK),
        0xE047 => (102, kc::MOVE_HOME),
        0xE048 => (103, kc::DPAD_UP),
        0xE049 => (104, kc::PAGE_UP),
        0xE04B => (105, kc::DPAD_LEFT),
        0xE04D => (106, kc::DPAD_RIGHT),
        0xE04F => (107, kc::MOVE_END),
        0xE050 => (108, kc::DPAD_DOWN),
        0xE051 => (109, kc::PAGE_DOWN),
        0xE052 => (110, kc::INSERT),
        0xE053 => (111, kc::FORWARD_DEL),
        0xE05B => (125, kc::META_LEFT),
        0xE05C => (126, kc::META_RIGHT),
        0xE05D => (127, kc::MENU),
        0xE065 => (217, kc::SEARCH),
        0xE066 => (156, kc::BOOKMARK),
        0xE067 => (173, kc::REFRESH),
        0xE069 => (159, kc::FORWARD),
        0xE06A => (158, kc::BACK),
        0xE06C => (155, kc::ENVELOPE),
        SCAN_PAUSE => (119, kc::BREAK),
        _ => return None,
    };
    Some(k)
}

/// The meta state bits a held modifier key contributes.
pub fn modifier_meta(code: i32) -> i32 {
    match code {
        kc::SHIFT_LEFT => META_SHIFT_ON | META_SHIFT_LEFT_ON,
        kc::SHIFT_RIGHT => META_SHIFT_ON | META_SHIFT_RIGHT_ON,
        kc::ALT_LEFT => META_ALT_ON | META_ALT_LEFT_ON,
        kc::ALT_RIGHT => META_ALT_ON | META_ALT_RIGHT_ON,
        kc::CTRL_LEFT => META_CTRL_ON | META_CTRL_LEFT_ON,
        kc::CTRL_RIGHT => META_CTRL_ON | META_CTRL_RIGHT_ON,
        kc::META_LEFT => META_META_ON | META_META_LEFT_ON,
        kc::META_RIGHT => META_META_ON | META_META_RIGHT_ON,
        _ => 0,
    }
}

/// The lock keys' states (global in Windows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Locks {
    pub caps: bool,
    pub num: bool,
    pub scroll: bool,
}

impl Locks {
    pub fn meta(&self) -> i32 {
        (if self.caps { META_CAPS_LOCK_ON } else { 0 })
            | (if self.num { META_NUM_LOCK_ON } else { 0 })
            | (if self.scroll { META_SCROLL_LOCK_ON } else { 0 })
    }
}

#[derive(Clone, Copy, Debug)]
struct Held {
    scan: u16,
    /// The key code sent for the down (Back for Alt+Left).
    code: i32,
    linux: i32,
    down_time: i64,
    repeat: i32,
    system: bool,
}

/// Tracks held keys of one window: repeat counts, down times, meta state.
#[derive(Debug, Default)]
pub struct KeyTracker {
    held: Vec<Held>,
}

/// `KEYCODE_BACK` down and up as the system sends it (title bar back
/// button, system shortcuts).
pub fn system_back(time_ns: i64) -> [KeyEvent; 2] {
    let ev = |action| KeyEvent {
        device_id: device::VIRTUAL_KEYBOARD,
        source: source::KEYBOARD,
        action,
        flags: FLAG_FROM_SYSTEM | FLAG_VIRTUAL_HARD_KEY,
        key_code: kc::BACK,
        scan_code: 0,
        meta_state: 0,
        repeat_count: 0,
        down_time_ns: time_ns,
        event_time_ns: time_ns,
    };
    [ev(ACTION_DOWN), ev(ACTION_UP)]
}

impl KeyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The meta state from the held modifiers and the locks.
    pub fn meta_state(&self, locks: Locks) -> i32 {
        self.held
            .iter()
            .fold(locks.meta(), |m, h| m | modifier_meta(h.code))
    }

    /// A key went down (`down`) or up. Returns the event to deliver, if the
    /// key is one Android knows.
    pub fn key(&mut self, time_ns: i64, scan: u16, down: bool, locks: Locks) -> Option<KeyEvent> {
        let (linux, code) = map_scan_code(scan)?;
        let held = self.held.iter().position(|h| h.scan == scan);
        if down {
            let h = match held {
                Some(i) => {
                    self.held[i].repeat += 1;
                    self.held[i]
                }
                None => {
                    let alt_only = {
                        let m = self.meta_state(Locks::default());
                        m & META_ALT_ON != 0 && m & (META_CTRL_ON | META_META_ON | META_SHIFT_ON) == 0
                    };
                    let system = code == kc::DPAD_LEFT && alt_only;
                    let h = Held {
                        scan,
                        code: if system { kc::BACK } else { code },
                        linux,
                        down_time: time_ns,
                        repeat: 0,
                        system,
                    };
                    self.held.push(h);
                    h
                }
            };
            Some(self.event(&h, ACTION_DOWN, 0, time_ns, locks))
        } else {
            let h = match held {
                Some(i) => self.held.remove(i),
                // An up without a down (pressed before the window had focus).
                None => Held {
                    scan,
                    code,
                    linux,
                    down_time: time_ns,
                    repeat: 0,
                    system: false,
                },
            };
            let mut e = self.event(&h, ACTION_UP, 0, time_ns, locks);
            e.repeat_count = 0;
            Some(e)
        }
    }

    fn event(&self, h: &Held, action: i32, flags: i32, time_ns: i64, locks: Locks) -> KeyEvent {
        if h.system {
            let mut e = system_back(h.down_time)[if action == ACTION_DOWN { 0 } else { 1 }].clone();
            e.event_time_ns = time_ns;
            e.repeat_count = h.repeat;
            e.flags |= flags;
            return e;
        }
        KeyEvent {
            device_id: device::KEYBOARD,
            source: source::KEYBOARD,
            action,
            flags,
            key_code: h.code,
            scan_code: h.linux,
            meta_state: self.meta_state(locks),
            repeat_count: h.repeat,
            down_time_ns: h.down_time,
            event_time_ns: time_ns,
        }
    }

    /// The window lost focus: every held key goes up with `FLAG_CANCELED`.
    pub fn cancel_all(&mut self, time_ns: i64, locks: Locks) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        while let Some(h) = self.held.pop() {
            let mut e = self.event(&h, ACTION_UP, FLAG_CANCELED, time_ns, locks);
            e.repeat_count = 0;
            out.push(e);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_code_table() {
        for (scan, linux, code) in [
            (0x1E, 30, kc::A),
            (0x10, 16, kc::Q),
            (0x0B, 11, kc::NUM_0),
            (0x1C, 28, kc::ENTER),
            (0xE01C, 96, kc::NUMPAD_ENTER),
            (0x0E, 14, kc::DEL),
            (0xE053, 111, kc::FORWARD_DEL),
            (0xE04B, 105, kc::DPAD_LEFT),
            (0xE038, 100, kc::ALT_RIGHT),
            (0xE05B, 125, kc::META_LEFT),
            (0xE05D, 127, kc::MENU),
            (0x56, 86, kc::BACKSLASH),
            (0x01, 1, kc::ESCAPE),
            (0x45, 69, kc::NUM_LOCK),
            (SCAN_PAUSE, 119, kc::BREAK),
            (0x58, 88, kc::F12),
            (0x4F, 79, kc::NUMPAD_1),
            (0xE047, 102, kc::MOVE_HOME),
        ] {
            assert_eq!(map_scan_code(scan), Some((linux, code)), "{scan:#x}");
        }
        assert_eq!(map_scan_code(0xE05E), None); // Power: Windows' own.
        // Every letter of the main block maps to a distinct key code.
        let mut seen = std::collections::HashSet::new();
        for s in 0x01..0x59u16 {
            if let Some((_, c)) = map_scan_code(s) {
                assert!(seen.insert(c) || c == kc::BACKSLASH, "{s:#x}");
            }
        }
    }

    #[test]
    fn hebrew_layout_does_not_change_key_codes() {
        // The key codes depend only on the scan code: with the Hebrew
        // layout the key that types 'ש' is KEYCODE_A, and Ctrl+C is C.
        let mut t = KeyTracker::new();
        let l = Locks::default();
        let ctrl = t.key(1, 0x1D, true, l).unwrap();
        assert_eq!(ctrl.meta_state, META_CTRL_ON | META_CTRL_LEFT_ON);
        let c = t.key(2, 0x2E, true, l).unwrap();
        assert_eq!(c.key_code, kc::C);
        assert_eq!(c.meta_state, META_CTRL_ON | META_CTRL_LEFT_ON);
        t.key(3, 0x2E, false, l);
        let up = t.key(4, 0x1D, false, l).unwrap();
        assert_eq!(up.meta_state, 0);
    }

    #[test]
    fn repeat_and_down_time() {
        let mut t = KeyTracker::new();
        let l = Locks {
            caps: true,
            ..Default::default()
        };
        let a: Vec<_> = [10, 20, 30]
            .into_iter()
            .map(|tm| t.key(tm, 0x1E, true, l).unwrap())
            .collect();
        assert_eq!(
            a.iter().map(|e| e.repeat_count).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(a.iter().all(|e| e.down_time_ns == 10));
        assert_eq!(a[2].event_time_ns, 30);
        assert_eq!(a[0].meta_state, META_CAPS_LOCK_ON);
        assert_eq!(a[0].scan_code, 30);
        assert_eq!(a[0].source, source::KEYBOARD);
        let up = t.key(40, 0x1E, false, l).unwrap();
        assert_eq!((up.action, up.repeat_count, up.down_time_ns), (ACTION_UP, 0, 10));
    }

    #[test]
    fn modifiers_in_meta_state_table() {
        // (scan codes held, meta state of a following key)
        for (held, meta) in [
            (&[0x2A][..], META_SHIFT_ON | META_SHIFT_LEFT_ON),
            (&[0x36], META_SHIFT_ON | META_SHIFT_RIGHT_ON),
            (&[0x2A, 0x36], META_SHIFT_ON | META_SHIFT_LEFT_ON | META_SHIFT_RIGHT_ON),
            (&[0xE038], META_ALT_ON | META_ALT_RIGHT_ON),
            (&[0xE01D, 0x38], META_CTRL_ON | META_CTRL_RIGHT_ON | META_ALT_ON | META_ALT_LEFT_ON),
            (&[0xE05C], META_META_ON | META_META_RIGHT_ON),
        ] {
            let mut t = KeyTracker::new();
            for s in held {
                t.key(0, *s, true, Locks::default());
            }
            let e = t.key(1, 0x39, true, Locks::default()).unwrap();
            assert_eq!(e.meta_state, meta, "{held:x?}");
        }
    }

    #[test]
    fn alt_left_is_back() {
        let mut t = KeyTracker::new();
        let l = Locks::default();
        t.key(0, 0x38, true, l);
        let down = t.key(1, 0xE04B, true, l).unwrap();
        assert_eq!(down.key_code, kc::BACK);
        assert_eq!(down.device_id, device::VIRTUAL_KEYBOARD);
        assert_eq!(down.flags, FLAG_FROM_SYSTEM | FLAG_VIRTUAL_HARD_KEY);
        // Released after Alt: still Back.
        t.key(2, 0x38, false, l);
        let up = t.key(3, 0xE04B, false, l).unwrap();
        assert_eq!((up.key_code, up.action), (kc::BACK, ACTION_UP));
        assert_eq!(up.down_time_ns, 1);
        // Ctrl+Alt+Left and plain Left are not Back.
        assert_eq!(t.key(4, 0xE04B, true, l).unwrap().key_code, kc::DPAD_LEFT);
        t.key(5, 0xE04B, false, l);
        t.key(6, 0x1D, true, l);
        t.key(7, 0x38, true, l);
        assert_eq!(t.key(8, 0xE04B, true, l).unwrap().key_code, kc::DPAD_LEFT);
    }

    #[test]
    fn escape_is_delivered_as_escape() {
        let mut t = KeyTracker::new();
        let e = t.key(0, 0x01, true, Locks::default()).unwrap();
        assert_eq!(e.key_code, kc::ESCAPE);
        assert_eq!(e.device_id, device::KEYBOARD);
    }

    #[test]
    fn focus_loss_cancels_held_keys() {
        let mut t = KeyTracker::new();
        t.key(0, 0x2A, true, Locks::default());
        t.key(1, 0x1E, true, Locks::default());
        let c = t.cancel_all(5, Locks::default());
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|e| e.action == ACTION_UP && e.flags & FLAG_CANCELED != 0));
        assert_eq!(t.meta_state(Locks::default()), 0);
    }

    #[test]
    fn system_back_pair() {
        let [d, u] = system_back(9);
        assert_eq!((d.action, u.action), (ACTION_DOWN, ACTION_UP));
        assert!(d.key_code == kc::BACK && u.key_code == kc::BACK);
        assert_eq!(u.down_time_ns, 9);
    }
}

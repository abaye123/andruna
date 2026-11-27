//! Android API constants the compositor produces: `MotionEvent`,
//! `KeyEvent`, `InputDevice` and `Configuration` values.
//!
//! These are the public values of the Android SDK (they are part of the
//! application ABI and never change). They are written out here so the
//! translation code can be read against the Android documentation.

/// `android.view.MotionEvent` constants.
pub mod motion {
    pub const ACTION_DOWN: i32 = 0;
    pub const ACTION_UP: i32 = 1;
    pub const ACTION_MOVE: i32 = 2;
    pub const ACTION_CANCEL: i32 = 3;
    pub const ACTION_OUTSIDE: i32 = 4;
    pub const ACTION_POINTER_DOWN: i32 = 5;
    pub const ACTION_POINTER_UP: i32 = 6;
    pub const ACTION_HOVER_MOVE: i32 = 7;
    pub const ACTION_SCROLL: i32 = 8;
    pub const ACTION_HOVER_ENTER: i32 = 9;
    pub const ACTION_HOVER_EXIT: i32 = 10;
    pub const ACTION_BUTTON_PRESS: i32 = 11;
    pub const ACTION_BUTTON_RELEASE: i32 = 12;
    pub const ACTION_MASK: i32 = 0xff;
    pub const ACTION_POINTER_INDEX_SHIFT: u32 = 8;

    /// `ACTION_POINTER_DOWN`/`ACTION_POINTER_UP` with the pointer index.
    pub const fn with_index(action: i32, index: usize) -> i32 {
        action | ((index as i32) << ACTION_POINTER_INDEX_SHIFT)
    }

    pub const FLAG_WINDOW_IS_OBSCURED: i32 = 0x1;
    pub const FLAG_IS_GENERATED_GESTURE: i32 = 0x8;
    pub const FLAG_CANCELED: i32 = 0x20;

    pub const AXIS_X: u32 = 0;
    pub const AXIS_Y: u32 = 1;
    pub const AXIS_PRESSURE: u32 = 2;
    pub const AXIS_SIZE: u32 = 3;
    pub const AXIS_TOUCH_MAJOR: u32 = 4;
    pub const AXIS_TOUCH_MINOR: u32 = 5;
    pub const AXIS_TOOL_MAJOR: u32 = 6;
    pub const AXIS_TOOL_MINOR: u32 = 7;
    pub const AXIS_ORIENTATION: u32 = 8;
    pub const AXIS_VSCROLL: u32 = 9;
    pub const AXIS_HSCROLL: u32 = 10;
    pub const AXIS_TILT: u32 = 25;
    pub const AXIS_RELATIVE_X: u32 = 27;
    pub const AXIS_RELATIVE_Y: u32 = 28;
    pub const AXIS_GESTURE_X_OFFSET: u32 = 48;
    pub const AXIS_GESTURE_Y_OFFSET: u32 = 49;
    pub const AXIS_GESTURE_SCROLL_X_DISTANCE: u32 = 50;
    pub const AXIS_GESTURE_SCROLL_Y_DISTANCE: u32 = 51;
    pub const AXIS_GESTURE_PINCH_SCALE_FACTOR: u32 = 52;

    pub const TOOL_TYPE_UNKNOWN: i32 = 0;
    pub const TOOL_TYPE_FINGER: i32 = 1;
    pub const TOOL_TYPE_STYLUS: i32 = 2;
    pub const TOOL_TYPE_MOUSE: i32 = 3;
    pub const TOOL_TYPE_ERASER: i32 = 4;

    pub const BUTTON_PRIMARY: i32 = 1;
    pub const BUTTON_SECONDARY: i32 = 2;
    pub const BUTTON_TERTIARY: i32 = 4;
    pub const BUTTON_BACK: i32 = 8;
    pub const BUTTON_FORWARD: i32 = 16;
    pub const BUTTON_STYLUS_PRIMARY: i32 = 32;
    pub const BUTTON_STYLUS_SECONDARY: i32 = 64;

    pub const CLASSIFICATION_NONE: i32 = 0;
    pub const CLASSIFICATION_TWO_FINGER_SWIPE: i32 = 3;
    pub const CLASSIFICATION_PINCH: i32 = 5;
}

/// `android.view.InputDevice` sources.
pub mod source {
    pub const KEYBOARD: u32 = 0x0000_0101;
    pub const TOUCHSCREEN: u32 = 0x0000_1002;
    pub const MOUSE: u32 = 0x0000_2002;
    pub const STYLUS: u32 = 0x0000_4002;
    pub const TOUCHPAD: u32 = 0x0010_0008;
}

/// `android.view.KeyEvent` constants.
pub mod key {
    pub const ACTION_DOWN: i32 = 0;
    pub const ACTION_UP: i32 = 1;
    pub const ACTION_MULTIPLE: i32 = 2;

    pub const FLAG_SOFT_KEYBOARD: i32 = 0x2;
    pub const FLAG_FROM_SYSTEM: i32 = 0x8;
    pub const FLAG_CANCELED: i32 = 0x20;
    pub const FLAG_VIRTUAL_HARD_KEY: i32 = 0x40;
    pub const FLAG_LONG_PRESS: i32 = 0x80;

    pub const META_SHIFT_ON: i32 = 0x1;
    pub const META_ALT_ON: i32 = 0x2;
    pub const META_SYM_ON: i32 = 0x4;
    pub const META_FUNCTION_ON: i32 = 0x8;
    pub const META_ALT_LEFT_ON: i32 = 0x10;
    pub const META_ALT_RIGHT_ON: i32 = 0x20;
    pub const META_SHIFT_LEFT_ON: i32 = 0x40;
    pub const META_SHIFT_RIGHT_ON: i32 = 0x80;
    pub const META_CTRL_ON: i32 = 0x1000;
    pub const META_CTRL_LEFT_ON: i32 = 0x2000;
    pub const META_CTRL_RIGHT_ON: i32 = 0x4000;
    pub const META_META_ON: i32 = 0x10000;
    pub const META_META_LEFT_ON: i32 = 0x20000;
    pub const META_META_RIGHT_ON: i32 = 0x40000;
    pub const META_CAPS_LOCK_ON: i32 = 0x100000;
    pub const META_NUM_LOCK_ON: i32 = 0x200000;
    pub const META_SCROLL_LOCK_ON: i32 = 0x400000;
}

/// `android.view.KeyEvent` key codes.
#[allow(missing_docs)]
pub mod keycode {
    pub const UNKNOWN: i32 = 0;
    pub const HOME: i32 = 3;
    pub const BACK: i32 = 4;
    pub const NUM_0: i32 = 7;
    pub const NUM_1: i32 = 8;
    pub const NUM_2: i32 = 9;
    pub const NUM_3: i32 = 10;
    pub const NUM_4: i32 = 11;
    pub const NUM_5: i32 = 12;
    pub const NUM_6: i32 = 13;
    pub const NUM_7: i32 = 14;
    pub const NUM_8: i32 = 15;
    pub const NUM_9: i32 = 16;
    pub const DPAD_UP: i32 = 19;
    pub const DPAD_DOWN: i32 = 20;
    pub const DPAD_LEFT: i32 = 21;
    pub const DPAD_RIGHT: i32 = 22;
    pub const VOLUME_UP: i32 = 24;
    pub const VOLUME_DOWN: i32 = 25;
    pub const A: i32 = 29;
    pub const B: i32 = 30;
    pub const C: i32 = 31;
    pub const D: i32 = 32;
    pub const E: i32 = 33;
    pub const F: i32 = 34;
    pub const G: i32 = 35;
    pub const H: i32 = 36;
    pub const I: i32 = 37;
    pub const J: i32 = 38;
    pub const K: i32 = 39;
    pub const L: i32 = 40;
    pub const M: i32 = 41;
    pub const N: i32 = 42;
    pub const O: i32 = 43;
    pub const P: i32 = 44;
    pub const Q: i32 = 45;
    pub const R: i32 = 46;
    pub const S: i32 = 47;
    pub const T: i32 = 48;
    pub const U: i32 = 49;
    pub const V: i32 = 50;
    pub const W: i32 = 51;
    pub const X: i32 = 52;
    pub const Y: i32 = 53;
    pub const Z: i32 = 54;
    pub const COMMA: i32 = 55;
    pub const PERIOD: i32 = 56;
    pub const ALT_LEFT: i32 = 57;
    pub const ALT_RIGHT: i32 = 58;
    pub const SHIFT_LEFT: i32 = 59;
    pub const SHIFT_RIGHT: i32 = 60;
    pub const TAB: i32 = 61;
    pub const SPACE: i32 = 62;
    pub const EXPLORER: i32 = 64;
    pub const ENVELOPE: i32 = 65;
    pub const ENTER: i32 = 66;
    pub const DEL: i32 = 67;
    pub const GRAVE: i32 = 68;
    pub const MINUS: i32 = 69;
    pub const EQUALS: i32 = 70;
    pub const LEFT_BRACKET: i32 = 71;
    pub const RIGHT_BRACKET: i32 = 72;
    pub const BACKSLASH: i32 = 73;
    pub const SEMICOLON: i32 = 74;
    pub const APOSTROPHE: i32 = 75;
    pub const SLASH: i32 = 76;
    pub const MENU: i32 = 82;
    pub const SEARCH: i32 = 84;
    pub const MEDIA_PLAY_PAUSE: i32 = 85;
    pub const MEDIA_STOP: i32 = 86;
    pub const MEDIA_NEXT: i32 = 87;
    pub const MEDIA_PREVIOUS: i32 = 88;
    pub const PAGE_UP: i32 = 92;
    pub const PAGE_DOWN: i32 = 93;
    pub const ESCAPE: i32 = 111;
    pub const FORWARD_DEL: i32 = 112;
    pub const CTRL_LEFT: i32 = 113;
    pub const CTRL_RIGHT: i32 = 114;
    pub const CAPS_LOCK: i32 = 115;
    pub const SCROLL_LOCK: i32 = 116;
    pub const META_LEFT: i32 = 117;
    pub const META_RIGHT: i32 = 118;
    pub const SYSRQ: i32 = 120;
    pub const BREAK: i32 = 121;
    pub const MOVE_HOME: i32 = 122;
    pub const MOVE_END: i32 = 123;
    pub const INSERT: i32 = 124;
    pub const FORWARD: i32 = 125;
    pub const F1: i32 = 131;
    pub const F2: i32 = 132;
    pub const F3: i32 = 133;
    pub const F4: i32 = 134;
    pub const F5: i32 = 135;
    pub const F6: i32 = 136;
    pub const F7: i32 = 137;
    pub const F8: i32 = 138;
    pub const F9: i32 = 139;
    pub const F10: i32 = 140;
    pub const F11: i32 = 141;
    pub const F12: i32 = 142;
    pub const NUM_LOCK: i32 = 143;
    pub const NUMPAD_0: i32 = 144;
    pub const NUMPAD_1: i32 = 145;
    pub const NUMPAD_2: i32 = 146;
    pub const NUMPAD_3: i32 = 147;
    pub const NUMPAD_4: i32 = 148;
    pub const NUMPAD_5: i32 = 149;
    pub const NUMPAD_6: i32 = 150;
    pub const NUMPAD_7: i32 = 151;
    pub const NUMPAD_8: i32 = 152;
    pub const NUMPAD_9: i32 = 153;
    pub const NUMPAD_DIVIDE: i32 = 154;
    pub const NUMPAD_MULTIPLY: i32 = 155;
    pub const NUMPAD_SUBTRACT: i32 = 156;
    pub const NUMPAD_ADD: i32 = 157;
    pub const NUMPAD_DOT: i32 = 158;
    pub const NUMPAD_COMMA: i32 = 159;
    pub const NUMPAD_ENTER: i32 = 160;
    pub const NUMPAD_EQUALS: i32 = 161;
    pub const VOLUME_MUTE: i32 = 164;
    pub const BOOKMARK: i32 = 174;
    pub const CALCULATOR: i32 = 210;
    pub const ZENKAKU_HANKAKU: i32 = 211;
    pub const MUHENKAN: i32 = 213;
    pub const HENKAN: i32 = 214;
    pub const KATAKANA_HIRAGANA: i32 = 215;
    pub const YEN: i32 = 216;
    pub const RO: i32 = 217;
    pub const REFRESH: i32 = 285;

    /// The key's name as used in key character map files
    /// (`KeyEvent.keyCodeToString` without the `KEYCODE_` prefix), for the
    /// keys a generated layout overlay can contain.
    pub fn kcm_name(code: i32) -> Option<&'static str> {
        Some(match code {
            NUM_0 => "0",
            NUM_1 => "1",
            NUM_2 => "2",
            NUM_3 => "3",
            NUM_4 => "4",
            NUM_5 => "5",
            NUM_6 => "6",
            NUM_7 => "7",
            NUM_8 => "8",
            NUM_9 => "9",
            A => "A",
            B => "B",
            C => "C",
            D => "D",
            E => "E",
            F => "F",
            G => "G",
            H => "H",
            I => "I",
            J => "J",
            K => "K",
            L => "L",
            M => "M",
            N => "N",
            O => "O",
            P => "P",
            Q => "Q",
            R => "R",
            S => "S",
            T => "T",
            U => "U",
            V => "V",
            W => "W",
            X => "X",
            Y => "Y",
            Z => "Z",
            COMMA => "COMMA",
            PERIOD => "PERIOD",
            SPACE => "SPACE",
            GRAVE => "GRAVE",
            MINUS => "MINUS",
            EQUALS => "EQUALS",
            LEFT_BRACKET => "LEFT_BRACKET",
            RIGHT_BRACKET => "RIGHT_BRACKET",
            BACKSLASH => "BACKSLASH",
            SEMICOLON => "SEMICOLON",
            APOSTROPHE => "APOSTROPHE",
            SLASH => "SLASH",
            YEN => "YEN",
            RO => "RO",
            _ => return None,
        })
    }
}

/// `android.content.res.Configuration` constants.
pub mod config {
    pub const ORIENTATION_PORTRAIT: i32 = 1;
    pub const ORIENTATION_LANDSCAPE: i32 = 2;

    pub const SCREENLAYOUT_SIZE_SMALL: i32 = 0x01;
    pub const SCREENLAYOUT_SIZE_NORMAL: i32 = 0x02;
    pub const SCREENLAYOUT_SIZE_LARGE: i32 = 0x03;
    pub const SCREENLAYOUT_SIZE_XLARGE: i32 = 0x04;
    pub const SCREENLAYOUT_LONG_NO: i32 = 0x10;
    pub const SCREENLAYOUT_LONG_YES: i32 = 0x20;
    pub const SCREENLAYOUT_LAYOUTDIR_LTR: i32 = 0x40;
    pub const SCREENLAYOUT_LAYOUTDIR_RTL: i32 = 0x80;
    pub const SCREENLAYOUT_ROUND_NO: i32 = 0x100;

    pub const UI_MODE_TYPE_NORMAL: i32 = 0x01;
    pub const UI_MODE_NIGHT_NO: i32 = 0x10;
    pub const UI_MODE_NIGHT_YES: i32 = 0x20;

    pub const TOUCHSCREEN_NOTOUCH: i32 = 1;
    pub const TOUCHSCREEN_FINGER: i32 = 3;
    pub const KEYBOARD_NOKEYS: i32 = 1;
    pub const KEYBOARD_QWERTY: i32 = 2;
    pub const KEYBOARDHIDDEN_NO: i32 = 1;
    pub const KEYBOARDHIDDEN_YES: i32 = 2;
    pub const HARDKEYBOARDHIDDEN_NO: i32 = 1;
    pub const HARDKEYBOARDHIDDEN_YES: i32 = 2;
    pub const NAVIGATION_NONAV: i32 = 1;
    pub const NAVIGATIONHIDDEN_NO: i32 = 1;

    /// `WindowConfiguration.WINDOWING_MODE_FREEFORM`.
    pub const WINDOWING_MODE_FREEFORM: i32 = 5;

    /// `ActivityInfo.CONFIG_*`: the bits of a configuration change.
    pub const CONFIG_TOUCHSCREEN: u32 = 0x0008;
    pub const CONFIG_KEYBOARD: u32 = 0x0010;
    pub const CONFIG_KEYBOARD_HIDDEN: u32 = 0x0020;
    pub const CONFIG_NAVIGATION: u32 = 0x0040;
    pub const CONFIG_ORIENTATION: u32 = 0x0080;
    pub const CONFIG_SCREEN_LAYOUT: u32 = 0x0100;
    pub const CONFIG_UI_MODE: u32 = 0x0200;
    pub const CONFIG_SCREEN_SIZE: u32 = 0x0400;
    pub const CONFIG_SMALLEST_SCREEN_SIZE: u32 = 0x0800;
    pub const CONFIG_DENSITY: u32 = 0x1000;
    pub const CONFIG_LAYOUT_DIRECTION: u32 = 0x2000;
    pub const CONFIG_WINDOW_CONFIGURATION: u32 = 0x2000_0000;
    pub const CONFIG_FONT_SCALE: u32 = 0x4000_0000;

    /// `DisplayMetrics.DENSITY_DEFAULT` (mdpi).
    pub const DENSITY_DEFAULT: u32 = 160;
}

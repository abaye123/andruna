//! Key character maps generated from the Windows keyboard layout.
//!
//! Android turns key codes into characters with a key character map
//! (`.kcm`): the device's base map plus, for a hardware keyboard, the
//! keyboard layout the user chose, which is an `OVERLAY` map. The
//! compositor generates that overlay from the active Windows layout, so an
//! app sees the characters the user's layout types (Hebrew, Russian,
//! French AZERTY, ...) through the normal Android path
//! (`KeyEvent.getUnicodeChar`, `KeyCharacterMap`), and the key codes stay
//! positional ([`keys`](super::keys)).
//!
//! Dead keys of the layout become Android's combining accents, which
//! Android's key character map treats as dead keys.

use super::keys::map_scan_code;
use crate::android::keycode;

/// The modifier states the overlay describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mods {
    pub shift: bool,
    pub caps: bool,
    /// AltGr (right Alt, which Windows reports as Ctrl+Alt).
    pub altgr: bool,
}

/// What a key types in one modifier state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutput {
    Char(char),
    /// A dead key; the spacing form of the accent, as Windows reports it.
    Dead(char),
}

/// The modifier states and their `.kcm` property names, in the order they
/// are written (Android tries later properties first).
const STATES: [(&str, Mods); 6] = [
    (
        "base",
        Mods {
            shift: false,
            caps: false,
            altgr: false,
        },
    ),
    (
        "shift",
        Mods {
            shift: true,
            caps: false,
            altgr: false,
        },
    ),
    (
        "capslock",
        Mods {
            shift: false,
            caps: true,
            altgr: false,
        },
    ),
    (
        "shift+capslock",
        Mods {
            shift: true,
            caps: true,
            altgr: false,
        },
    ),
    (
        "ralt",
        Mods {
            shift: false,
            caps: false,
            altgr: true,
        },
    ),
    (
        "shift+ralt",
        Mods {
            shift: true,
            caps: false,
            altgr: true,
        },
    ),
];

/// The combining accent Android uses as the dead key for a spacing
/// accent.
pub fn combining_accent(spacing: char) -> Option<char> {
    Some(match spacing {
        '`' => '\u{0300}',
        '\u{00B4}' | '\'' => '\u{0301}',
        '^' | '\u{02C6}' => '\u{0302}',
        '~' | '\u{02DC}' => '\u{0303}',
        '\u{00AF}' => '\u{0304}',
        '\u{02D8}' => '\u{0306}',
        '\u{02D9}' => '\u{0307}',
        '\u{00A8}' | '"' => '\u{0308}',
        '\u{02DA}' | '\u{00B0}' => '\u{030A}',
        '\u{02DD}' => '\u{030B}',
        '\u{02C7}' => '\u{030C}',
        '\u{00B8}' => '\u{0327}',
        '\u{02DB}' => '\u{0328}',
        c if ('\u{0300}'..='\u{036F}').contains(&c) => c,
        _ => return None,
    })
}

/// A character literal in `.kcm` syntax.
fn literal(c: char) -> Option<String> {
    match c {
        '\'' => Some("'\\''".into()),
        '\\' => Some("'\\\\'".into()),
        ' '..='~' => Some(format!("'{c}'")),
        c if (c as u32) <= 0xFFFF && !c.is_control() => Some(format!("'\\u{:04x}'", c as u32)),
        _ => None,
    }
}

/// The scan codes of the keys an overlay describes: every key whose
/// Android key code can carry characters.
pub fn character_keys() -> impl Iterator<Item = (u16, i32)> {
    (0x01u16..=0x7E).filter_map(|s| {
        let (_, code) = map_scan_code(s)?;
        keycode::kcm_name(code).map(|_| (s, code))
    })
}

/// Generates an `OVERLAY` key character map. `layout(scan, mods)` says
/// what the key types (`None`: nothing). Keys that type nothing in any
/// state are left out, so the base map applies to them.
pub fn generate(layout: impl Fn(u16, Mods) -> Option<KeyOutput>) -> String {
    let mut out = String::from("# Generated from the Windows keyboard layout.\ntype OVERLAY\n");
    let mut done = std::collections::HashSet::new();
    for (scan, code) in character_keys() {
        // Two scan codes can share a key code (the ISO key and backslash);
        // the first one describes it.
        if !done.insert(code) {
            continue;
        }
        let name = keycode::kcm_name(code).unwrap();
        let mut lines = Vec::new();
        for (prop, mods) in STATES {
            let value = match layout(scan, mods) {
                Some(KeyOutput::Char(c)) => literal(c),
                Some(KeyOutput::Dead(c)) => combining_accent(c).and_then(literal),
                None => None,
            };
            match value {
                Some(v) => lines.push(format!("    {prop}: {v}")),
                // Base must say what the key does when it types nothing.
                None if prop == "base" => lines.push("    base: none".into()),
                None => {}
            }
        }
        if lines.len() == 1 && lines[0].ends_with("none") {
            continue;
        }
        let label = match layout(scan, STATES[0].1) {
            _ if name.len() == 1 => literal(name.chars().next().unwrap()),
            Some(KeyOutput::Char(c)) => c.to_uppercase().next().and_then(literal),
            _ => None,
        };
        out.push_str(&format!("\nkey {name} {{\n"));
        if let Some(l) = label {
            out.push_str(&format!("    label: {l}\n"));
        }
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
        out.push_str("}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part of the Windows Hebrew (standard) layout: Hebrew letters in
    /// the base state, Latin capitals with Shift and with Caps Lock, as
    /// Windows types them.
    fn hebrew(scan: u16, m: Mods) -> Option<KeyOutput> {
        let (heb, latin) = match scan {
            0x1E => ('\u{05E9}', 'A'), // shin
            0x1F => ('\u{05D3}', 'S'), // dalet
            0x2E => ('\u{05D1}', 'C'), // bet
            0x10 => ('/', 'Q'),
            0x33 => ('\u{05EA}', '>'), // tav; Shift: '>'
            0x39 => (' ', ' '),
            0x02 => ('1', '!'),
            _ => return None,
        };
        if m.altgr {
            return None;
        }
        Some(KeyOutput::Char(match (m.shift, m.caps, scan) {
            (false, false, _) => heb,
            (true, _, _) => latin,
            // Caps Lock types Latin lowercase on letter keys.
            (false, true, 0x1E | 0x1F | 0x2E | 0x10) => latin.to_ascii_lowercase(),
            (false, true, _) => heb,
        }))
    }

    #[test]
    fn hebrew_overlay() {
        let kcm = generate(hebrew);
        assert!(kcm.starts_with("# Generated"));
        assert!(kcm.contains("type OVERLAY\n"));
        let a = "\nkey A {\n    label: 'A'\n    base: '\\u05e9'\n    shift: 'A'\n    capslock: 'a'\n    shift+capslock: 'A'\n}\n";
        assert!(kcm.contains(a), "{kcm}");
        assert!(
            kcm.contains(
                "key COMMA {\n    label: '\\u05ea'\n    base: '\\u05ea'\n    shift: '>'\n"
            )
        );
        assert!(
            kcm.contains("key SPACE {\n    label: ' '\n    base: ' '\n"),
            "{kcm}"
        );
        assert!(kcm.contains("key 1 {\n    label: '1'\n    base: '1'\n    shift: '!'\n"));
        // Keys the layout does not type are left to the base map.
        assert!(!kcm.contains("key Z "));
        // Q types '/' unshifted.
        assert!(kcm.contains("key Q {\n    label: 'Q'\n    base: '/'\n"));
    }

    #[test]
    fn altgr_and_dead_keys() {
        // A French-like key: 'e' / 'E', AltGr: euro; a dead circumflex.
        let kcm = generate(|scan, m| match (scan, m.shift, m.altgr) {
            (0x12, false, false) => Some(KeyOutput::Char(if m.caps { 'E' } else { 'e' })),
            (0x12, true, false) => Some(KeyOutput::Char('E')),
            (0x12, false, true) => Some(KeyOutput::Char('\u{20AC}')),
            (0x1A, false, false) => Some(KeyOutput::Dead('^')),
            _ => None,
        });
        assert!(kcm.contains("    ralt: '\\u20ac'\n"), "{kcm}");
        assert!(
            kcm.contains("key LEFT_BRACKET {\n    base: '\\u0302'\n    capslock: '\\u0302'\n}"),
            "{kcm}"
        );
    }

    #[test]
    fn literals() {
        assert_eq!(literal('a').unwrap(), "'a'");
        assert_eq!(literal('\'').unwrap(), "'\\''");
        assert_eq!(literal('\\').unwrap(), "'\\\\'");
        assert_eq!(literal('\u{05D0}').unwrap(), "'\\u05d0'");
        assert_eq!(literal('\u{1F600}'), None);
        assert_eq!(literal('\n'), None);
    }

    #[test]
    fn character_keys_cover_the_main_block() {
        let keys: Vec<_> = character_keys().map(|k| k.1).collect();
        for k in [
            keycode::A,
            keycode::Z,
            keycode::NUM_0,
            keycode::SPACE,
            keycode::SLASH,
            keycode::GRAVE,
        ] {
            assert!(keys.contains(&k));
        }
        assert!(!keys.contains(&keycode::ENTER));
        assert!(!keys.contains(&keycode::NUMPAD_1));
    }
}

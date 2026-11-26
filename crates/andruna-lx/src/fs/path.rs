//! Linux paths: lexical normalization and the reversible escaping of file
//! names that Windows cannot store.
//!
//! # Escaping
//!
//! A Linux file name is any byte string without `/` and NUL. A Windows name
//! is UTF-16 without `< > : " \ | ? *` and control characters, must not end
//! in a dot or space, and must not be a DOS device name (`CON`, `NUL.txt`,
//! ...). Names are converted as follows, so that every Linux name has
//! exactly one Windows form and back:
//!
//! - valid UTF-8 is converted to UTF-16;
//! - a byte that Windows cannot store (a forbidden ASCII character, a byte
//!   that is not valid UTF-8, a final dot or space, or the first letter of a
//!   device name) becomes the private-use character `U+F000 + byte`;
//! - a character that is itself in `U+F000..=U+F100` is prefixed with
//!   `U+F100`, so names that already contain these characters survive.

use crate::errno::{Errno, SysResult};

/// `PATH_MAX` including the terminating NUL.
pub const PATH_MAX: usize = 4096;
/// `NAME_MAX`.
pub const NAME_MAX: usize = 255;

const ESCAPE_BASE: u32 = 0xf000;
const LITERAL_PREFIX: u32 = 0xf100;

/// A normalized absolute Linux path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxPath {
    /// Components, none empty, `.` or `..`.
    pub components: Vec<Vec<u8>>,
    /// Whether the original path ended in `/` (the target must then be a
    /// directory).
    pub trailing_slash: bool,
}

impl LinuxPath {
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
            trailing_slash: false,
        }
    }

    /// The path as bytes, `/` for the root.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.components.is_empty() {
            return b"/".to_vec();
        }
        let mut out = Vec::new();
        for c in &self.components {
            out.push(b'/');
            out.extend_from_slice(c);
        }
        out
    }

    /// The path of the first `n` components.
    pub fn prefix(&self, n: usize) -> Self {
        Self {
            components: self.components[..n].to_vec(),
            trailing_slash: false,
        }
    }
}

/// Resolves `path` against the absolute directory `base` (used when `path`
/// is relative), removing `.`, `..` and repeated slashes.
///
/// `..` is resolved lexically. That matches Linux as long as the directories
/// on the way are not symbolic links, which lx does not create yet.
pub fn normalize(base: &[u8], path: &[u8]) -> SysResult<LinuxPath> {
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    if path.len() >= PATH_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    let mut components: Vec<Vec<u8>> = Vec::new();
    let start: &[u8] = if path[0] == b'/' { b"" } else { base };
    for part in start
        .split(|&b| b == b'/')
        .chain(path.split(|&b| b == b'/'))
    {
        match part {
            b"" | b"." => {}
            b".." => {
                components.pop();
            }
            name => {
                if name.len() > NAME_MAX {
                    return Err(Errno::ENAMETOOLONG);
                }
                components.push(name.to_vec());
            }
        }
    }
    let trailing_slash = path.ends_with(b"/")
        || path.ends_with(b"/.")
        || path.ends_with(b"/..")
        || path == b"."
        || path == b"..";
    Ok(LinuxPath {
        components,
        trailing_slash,
    })
}

fn is_forbidden_ascii(b: u8) -> bool {
    b < 0x20 || matches!(b, b'<' | b'>' | b':' | b'"' | b'\\' | b'|' | b'?' | b'*')
}

/// Whether `name` would be taken by Windows as a DOS device.
fn is_device_name(name: &[u8]) -> bool {
    let stem = name.split(|&b| b == b'.').next().unwrap_or(name);
    let stem = stem.trim_ascii_end();
    let upper: Vec<u8> = stem.to_ascii_uppercase();
    match upper.as_slice() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" | b"CONIN$" | b"CONOUT$" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => d.is_ascii_digit(),
        // COM and LPT followed by a superscript 1, 2 or 3 (UTF-8).
        [b'C', b'O', b'M', 0xc2, s] | [b'L', b'P', b'T', 0xc2, s] => {
            matches!(s, 0xb9 | 0xb2 | 0xb3)
        }
        _ => false,
    }
}

fn push_char(out: &mut Vec<u16>, c: char) {
    let mut buf = [0u16; 2];
    out.extend_from_slice(c.encode_utf16(&mut buf));
}

fn escaped(b: u8) -> u16 {
    (ESCAPE_BASE + u32::from(b)) as u16
}

/// Converts one Linux file name to its Windows form.
pub fn encode_component(name: &[u8]) -> Vec<u16> {
    debug_assert!(!name.is_empty() && !name.contains(&b'/') && !name.contains(&0));
    let mut out = Vec::with_capacity(name.len());
    let device = is_device_name(name);
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            let cp = u32::from(c);
            if c.is_ascii() && is_forbidden_ascii(c as u8) {
                out.push(escaped(c as u8));
            } else if (ESCAPE_BASE..=LITERAL_PREFIX).contains(&cp) {
                out.push(LITERAL_PREFIX as u16);
                out.push(cp as u16);
            } else {
                push_char(&mut out, c);
            }
        }
        out.extend(chunk.invalid().iter().map(|&b| escaped(b)));
    }
    if device {
        // The first character of a device name is always an ASCII letter.
        out[0] = escaped(out[0] as u8);
    }
    if let Some(last) = out.last_mut()
        && (*last == u16::from(b'.') || *last == u16::from(b' '))
    {
        *last = escaped(*last as u8);
    }
    out
}

/// Converts a Windows file name back to the Linux name. Unpaired surrogates
/// (which no Linux name produces) become U+FFFD.
pub fn decode_component(name: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    let mut chars = char::decode_utf16(name.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .peekable();
    while let Some(c) = chars.next() {
        let cp = u32::from(c);
        if cp == LITERAL_PREFIX {
            if let Some(next) = chars.next() {
                let mut buf = [0u8; 4];
                out.extend_from_slice(next.encode_utf8(&mut buf).as_bytes());
            }
        } else if (ESCAPE_BASE..ESCAPE_BASE + 0x100).contains(&cp) {
            out.push((cp - ESCAPE_BASE) as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(base: &str, p: &str) -> Vec<u8> {
        normalize(base.as_bytes(), p.as_bytes()).unwrap().to_bytes()
    }

    #[test]
    fn normalization() {
        assert_eq!(norm("/", "/a/b"), b"/a/b");
        assert_eq!(norm("/x/y", "a//b/./c"), b"/x/y/a/b/c");
        assert_eq!(norm("/x/y", "../z"), b"/x/z");
        assert_eq!(norm("/", "/../.."), b"/");
        assert_eq!(norm("/x", "."), b"/x");
        assert!(normalize(b"/", b"/a/").unwrap().trailing_slash);
        assert!(normalize(b"/", b"/a/.").unwrap().trailing_slash);
        assert!(!normalize(b"/", b"/a").unwrap().trailing_slash);
        assert_eq!(normalize(b"/", b""), Err(Errno::ENOENT));
        let long = vec![b'a'; 256];
        assert_eq!(normalize(b"/", &long), Err(Errno::ENAMETOOLONG));
        let deep = b"/a".repeat(2048);
        assert_eq!(normalize(b"/", &deep), Err(Errno::ENAMETOOLONG));
    }

    fn enc(s: &[u8]) -> String {
        String::from_utf16(&encode_component(s)).unwrap()
    }

    #[test]
    fn plain_names_are_unchanged() {
        assert_eq!(enc(b"hello.txt"), "hello.txt");
        assert_eq!(enc("שלום".as_bytes()), "שלום");
        assert_eq!(enc(b".hidden"), ".hidden");
        assert_eq!(enc(b"CONSOLE"), "CONSOLE");
        assert_eq!(enc(b"COMX"), "COMX");
    }

    #[test]
    fn forbidden_characters_are_escaped() {
        assert_eq!(enc(b"a:b"), "a\u{f03a}b");
        assert_eq!(enc(b"what?"), "what\u{f03f}");
        assert_eq!(enc(b"x\\y"), "x\u{f05c}y");
        assert_eq!(enc(b"tab\there"), "tab\u{f009}here");
        assert_eq!(enc(b"end."), "end\u{f02e}");
        assert_eq!(enc(b"end "), "end\u{f020}");
        assert_eq!(enc(b"..."), "..\u{f02e}");
        assert_eq!(enc(b"con"), "\u{f063}on");
        assert_eq!(enc(b"NUL.txt"), "\u{f04e}UL.txt");
        assert_eq!(enc(b"lpt1"), "\u{f06c}pt1");
        assert_eq!(enc("COM¹".as_bytes()), "\u{f043}OM¹");
        assert_eq!(enc(b"\xff\xfe"), "\u{f0ff}\u{f0fe}");
        assert_eq!(enc("\u{f03a}".as_bytes()), "\u{f100}\u{f03a}");
    }

    #[test]
    fn round_trip() {
        let samples: Vec<Vec<u8>> = vec![
            b"a:b*c?d".to_vec(),
            b"CON".to_vec(),
            b"aux.c".to_vec(),
            b"x.".to_vec(),
            b"  ".to_vec(),
            "\u{f000}\u{f100}\u{f0ff}\u{f101}".as_bytes().to_vec(),
            vec![0x80, b'a', 0xc3, 0xff, b'.'],
            "日本語.txt".as_bytes().to_vec(),
            (1u8..=255).filter(|&b| b != b'/').collect(),
        ];
        for s in samples {
            let w = encode_component(&s);
            assert_eq!(decode_component(&w), s, "{s:?}");
            for &u in &w {
                assert!(u >= 0x20, "{s:?}");
                assert!(!b"<>:\"\\|?*/".contains(&(u as u8)) || u > 0x7f, "{s:?}");
            }
            let last = *w.last().unwrap();
            assert!(last != u16::from(b'.') && last != u16::from(b' '));
        }
    }

    #[test]
    fn exhaustive_two_byte_round_trip() {
        for a in 1u8..=255 {
            for b in 1u8..=255 {
                if a == b'/' || b == b'/' {
                    continue;
                }
                let s = [a, b];
                assert_eq!(decode_component(&encode_component(&s)), s);
            }
        }
    }
}

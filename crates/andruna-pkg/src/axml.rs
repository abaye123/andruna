//! Android binary XML (`ResXMLTree`): `AndroidManifest.xml` and compiled
//! XML resources (adaptive icons, vector drawables).
//!
//! The format is a sequence of chunks (`ResChunk_header`: type, header
//! size, total size): a string pool, an optional resource map (the
//! resource ID of each attribute name, by string index), then namespace and
//! element start/end nodes. Attributes carry a typed value (`Res_value`).
//!
//! The parser builds a small tree. It never panics on malformed input and
//! bounds nesting depth; unknown chunk types are skipped, as Android's own
//! parser does.

use crate::error::{Code, Error, Result};

pub(crate) const RES_STRING_POOL_TYPE: u16 = 0x0001;
pub(crate) const RES_XML_TYPE: u16 = 0x0003;
const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
const RES_XML_END_NAMESPACE_TYPE: u16 = 0x0101;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;

/// The Android namespace URI.
pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

/// Deepest element nesting accepted.
pub const MAX_DEPTH: usize = 256;

const UTF8_FLAG: u32 = 0x100;

/// A little-endian cursor over a byte slice that never panics.
#[derive(Clone, Copy)]
pub(crate) struct Bytes<'a>(pub &'a [u8]);

impl<'a> Bytes<'a> {
    pub fn u8(&self, o: usize) -> Option<u8> {
        self.0.get(o).copied()
    }
    pub fn u16(&self, o: usize) -> Option<u16> {
        self.0
            .get(o..o.checked_add(2)?)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&self, o: usize) -> Option<u32> {
        self.0
            .get(o..o.checked_add(4)?)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn slice(&self, o: usize, n: usize) -> Option<&'a [u8]> {
        self.0.get(o..o.checked_add(n)?)
    }
}

/// A chunk header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Chunk {
    pub ty: u16,
    pub header_size: usize,
    pub size: usize,
}

/// Reads the chunk header at `o` and checks that the chunk fits in `d`.
pub(crate) fn chunk_at(d: Bytes<'_>, o: usize) -> Option<Chunk> {
    let ty = d.u16(o)?;
    let header_size = d.u16(o + 2)? as usize;
    let size = d.u32(o + 4)? as usize;
    if header_size < 8 || size < header_size || o.checked_add(size)? > d.0.len() {
        return None;
    }
    Some(Chunk {
        ty,
        header_size,
        size,
    })
}

/// A resource string pool, decoded lazily.
#[derive(Clone, Debug, Default)]
pub(crate) struct StringPool {
    strings: Vec<String>,
}

fn utf8_len(d: Bytes<'_>, o: &mut usize) -> Option<usize> {
    let a = d.u8(*o)? as usize;
    *o += 1;
    if a & 0x80 != 0 {
        let b = d.u8(*o)? as usize;
        *o += 1;
        Some(((a & 0x7F) << 8) | b)
    } else {
        Some(a)
    }
}

fn utf16_len(d: Bytes<'_>, o: &mut usize) -> Option<usize> {
    let a = d.u16(*o)? as usize;
    *o += 2;
    if a & 0x8000 != 0 {
        let b = d.u16(*o)? as usize;
        *o += 2;
        Some(((a & 0x7FFF) << 16) | b)
    } else {
        Some(a)
    }
}

impl StringPool {
    /// Parses a string pool chunk that starts at `o`.
    pub fn parse(d: Bytes<'_>, o: usize) -> Option<Self> {
        let c = chunk_at(d, o)?;
        if c.ty != RES_STRING_POOL_TYPE || c.header_size < 28 {
            return None;
        }
        let chunk = Bytes(d.slice(o, c.size)?);
        let count = chunk.u32(8)? as usize;
        let flags = chunk.u32(16)?;
        let strings_start = chunk.u32(20)? as usize;
        if count > c.size / 4 {
            return None;
        }
        let utf8 = flags & UTF8_FLAG != 0;
        let mut strings = Vec::with_capacity(count);
        for i in 0..count {
            let off = chunk.u32(c.header_size + i * 4)? as usize;
            let mut p = strings_start.checked_add(off)?;
            let s = if utf8 {
                let _utf16 = utf8_len(chunk, &mut p)?;
                let n = utf8_len(chunk, &mut p)?;
                let b = chunk.slice(p, n)?;
                // aapt wrote modified UTF-8 in some versions; be lenient
                // as Android is.
                String::from_utf8_lossy(b).into_owned()
            } else {
                let n = utf16_len(chunk, &mut p)?;
                let b = chunk.slice(p, n.checked_mul(2)?)?;
                let units: Vec<u16> = b
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            };
            strings.push(s);
        }
        Some(Self { strings })
    }

    pub fn get(&self, i: u32) -> Option<&str> {
        self.strings.get(i as usize).map(String::as_str)
    }
}

/// A typed value (`Res_value`).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    /// A reference to a resource (`@type/name`), by resource ID.
    Reference(u32),
    /// A reference to a theme attribute (`?attr`), by resource ID.
    Attribute(u32),
    String(String),
    Float(f32),
    /// Packed complex dimension (`TYPE_DIMENSION`).
    Dimension(u32),
    /// Packed complex fraction (`TYPE_FRACTION`).
    Fraction(u32),
    /// Decimal or hexadecimal integer.
    Int(u32),
    Bool(bool),
    /// A color as ARGB.
    Color(u32),
    /// A type this parser does not interpret.
    Other {
        ty: u8,
        data: u32,
    },
}

pub(crate) const TYPE_NULL: u8 = 0x00;
pub(crate) const TYPE_REFERENCE: u8 = 0x01;
pub(crate) const TYPE_ATTRIBUTE: u8 = 0x02;
pub(crate) const TYPE_STRING: u8 = 0x03;
pub(crate) const TYPE_FLOAT: u8 = 0x04;
pub(crate) const TYPE_DIMENSION: u8 = 0x05;
pub(crate) const TYPE_FRACTION: u8 = 0x06;
pub(crate) const TYPE_DYNAMIC_REFERENCE: u8 = 0x07;
pub(crate) const TYPE_DYNAMIC_ATTRIBUTE: u8 = 0x08;
pub(crate) const TYPE_INT_DEC: u8 = 0x10;
pub(crate) const TYPE_INT_HEX: u8 = 0x11;
pub(crate) const TYPE_INT_BOOLEAN: u8 = 0x12;
pub(crate) const TYPE_INT_COLOR_ARGB8: u8 = 0x1c;
pub(crate) const TYPE_INT_COLOR_RGB8: u8 = 0x1d;
pub(crate) const TYPE_INT_COLOR_ARGB4: u8 = 0x1e;
pub(crate) const TYPE_INT_COLOR_RGB4: u8 = 0x1f;

fn expand4(c: u32) -> u32 {
    // 0xARGB -> 0xAARRGGBB
    let mut out = 0;
    for i in 0..4 {
        let n = (c >> (i * 4)) & 0xF;
        out |= (n | (n << 4)) << (i * 8);
    }
    out
}

impl Value {
    /// Decodes a `Res_value`'s type and data.
    pub(crate) fn decode(ty: u8, data: u32, strings: &StringPool) -> Self {
        match ty {
            TYPE_NULL => Self::Null,
            TYPE_REFERENCE | TYPE_DYNAMIC_REFERENCE => {
                if data == 0 {
                    Self::Null
                } else {
                    Self::Reference(data)
                }
            }
            TYPE_ATTRIBUTE | TYPE_DYNAMIC_ATTRIBUTE => Self::Attribute(data),
            TYPE_STRING => match strings.get(data) {
                Some(s) => Self::String(s.to_string()),
                None => Self::Other { ty, data },
            },
            TYPE_FLOAT => Self::Float(f32::from_bits(data)),
            TYPE_DIMENSION => Self::Dimension(data),
            TYPE_FRACTION => Self::Fraction(data),
            TYPE_INT_DEC | TYPE_INT_HEX => Self::Int(data),
            TYPE_INT_BOOLEAN => Self::Bool(data != 0),
            TYPE_INT_COLOR_ARGB8 => Self::Color(data),
            TYPE_INT_COLOR_RGB8 => Self::Color(data | 0xFF00_0000),
            TYPE_INT_COLOR_ARGB4 => Self::Color(expand4(data & 0xFFFF)),
            TYPE_INT_COLOR_RGB4 => Self::Color(expand4(data & 0xFFFF) | 0xFF00_0000),
            _ => Self::Other { ty, data },
        }
    }

    /// The value as a string, when it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The value as an integer (decimal, hex or boolean), when it is one.
    pub fn as_int(&self) -> Option<u32> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Bool(b) => Some(*b as u32),
            _ => None,
        }
    }

    /// Decodes a complex dimension to (value, unit): unit 0 px, 1 dp,
    /// 2 sp, 3 pt, 4 in, 5 mm.
    pub fn dimension(&self) -> Option<(f32, u32)> {
        match self {
            Self::Dimension(d) => Some(complex_to_float(*d)),
            _ => None,
        }
    }
}

/// Decodes a packed complex value (`Res_value::COMPLEX_*`) to its number
/// and unit.
pub fn complex_to_float(c: u32) -> (f32, u32) {
    const RADIX_MULT: [f32; 4] = [
        1.0 / (1u32 << 8) as f32,
        1.0 / (1u32 << 15) as f32,
        1.0 / (1u32 << 23) as f32,
        1.0 / (1u32 << 31) as f32,
    ];
    let mantissa = (c & 0xFFFF_FF00) as i32 as f32;
    let radix = ((c >> 4) & 3) as usize;
    (mantissa * RADIX_MULT[radix], c & 0xF)
}

/// One attribute of an element.
#[derive(Clone, Debug, PartialEq)]
pub struct Attr {
    /// Namespace URI, if any.
    pub ns: Option<String>,
    pub name: String,
    /// The attribute's resource ID from the resource map (Android's
    /// parser identifies framework attributes by this, not by name).
    pub res_id: Option<u32>,
    /// The raw string as written in the source, when kept.
    pub raw: Option<String>,
    pub value: Value,
}

/// An element with its attributes and children.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Element {
    pub ns: Option<String>,
    pub name: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Element>,
    pub line: u32,
}

impl Element {
    /// The attribute with the given framework resource ID, or else the
    /// given name in the Android namespace.
    pub fn android_attr(&self, res_id: u32, name: &str) -> Option<&Attr> {
        self.attrs
            .iter()
            .find(|a| a.res_id == Some(res_id))
            .or_else(|| {
                self.attrs.iter().find(|a| {
                    a.name == name && a.ns.as_deref() == Some(ANDROID_NS) && a.res_id.is_none()
                })
            })
    }

    /// An attribute without namespace (such as the manifest's `package`).
    pub fn plain_attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.ns.is_none() && a.name == name)
    }

    /// Direct children with the given name.
    pub fn children_named<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s Element> + 's {
        self.children.iter().filter(move |c| c.name == name)
    }
}

fn bad(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidManifest, m)
}

/// Parses a binary XML document and returns its root element.
pub fn parse(data: &[u8]) -> Result<Element> {
    let d = Bytes(data);
    let root = chunk_at(d, 0).ok_or_else(|| bad("binary XML header is damaged"))?;
    if root.ty != RES_XML_TYPE {
        return Err(bad("not a binary XML document"));
    }
    let end = root.size;
    let mut o = root.header_size;
    let mut strings = StringPool::default();
    let mut res_map: Vec<u32> = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    let mut root_elem: Option<Element> = None;
    let s = |strings: &StringPool, i: u32| -> Option<String> {
        if i == u32::MAX {
            None
        } else {
            strings.get(i).map(str::to_string)
        }
    };
    while o + 8 <= end {
        let c = chunk_at(d, o).ok_or_else(|| bad(format!("chunk at {o} is damaged")))?;
        match c.ty {
            RES_STRING_POOL_TYPE => {
                strings = StringPool::parse(d, o).ok_or_else(|| bad("string pool is damaged"))?;
            }
            RES_XML_RESOURCE_MAP_TYPE => {
                let n = (c.size - c.header_size) / 4;
                res_map = (0..n)
                    .map(|i| d.u32(o + c.header_size + i * 4).unwrap_or(0))
                    .collect();
            }
            RES_XML_START_ELEMENT_TYPE => {
                if stack.len() >= MAX_DEPTH {
                    return Err(bad("elements are nested too deeply"));
                }
                let line = d.u32(o + 8).unwrap_or(0);
                let e = o + c.header_size;
                let ext = Bytes(
                    d.slice(o, c.size)
                        .ok_or_else(|| bad("element is damaged"))?,
                );
                let rel = c.header_size;
                let field = |k: usize| ext.u32(rel + k);
                let (Some(ns), Some(name)) = (field(0), field(4)) else {
                    return Err(bad("element is damaged"));
                };
                let attr_start = ext.u16(rel + 8).unwrap_or(0) as usize;
                let attr_size = ext.u16(rel + 10).unwrap_or(0) as usize;
                let attr_count = ext.u16(rel + 12).unwrap_or(0) as usize;
                let _ = e;
                let name = s(&strings, name).ok_or_else(|| bad("element name is missing"))?;
                let mut el = Element {
                    ns: s(&strings, ns),
                    name,
                    attrs: Vec::with_capacity(attr_count),
                    children: Vec::new(),
                    line,
                };
                if attr_count > 0 && attr_size < 20 {
                    return Err(bad("attribute size is too small"));
                }
                for k in 0..attr_count {
                    let a = rel + attr_start + k * attr_size;
                    let (Some(ans), Some(aname), Some(raw), Some(vty), Some(vdata)) = (
                        ext.u32(a),
                        ext.u32(a + 4),
                        ext.u32(a + 8),
                        ext.u8(a + 15),
                        ext.u32(a + 16),
                    ) else {
                        return Err(bad("attribute runs past its element"));
                    };
                    let res_id = res_map.get(aname as usize).copied().filter(|&id| id != 0);
                    el.attrs.push(Attr {
                        ns: s(&strings, ans),
                        name: s(&strings, aname).unwrap_or_default(),
                        res_id,
                        raw: s(&strings, raw),
                        value: Value::decode(vty, vdata, &strings),
                    });
                }
                stack.push(el);
            }
            RES_XML_END_ELEMENT_TYPE => {
                let el = stack.pop().ok_or_else(|| bad("unbalanced end element"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => {
                        if root_elem.is_some() {
                            return Err(bad("more than one root element"));
                        }
                        root_elem = Some(el);
                    }
                }
            }
            RES_XML_START_NAMESPACE_TYPE | RES_XML_END_NAMESPACE_TYPE => {}
            _ => {}
        }
        o += c.size;
    }
    if !stack.is_empty() {
        return Err(bad("document ends inside an element"));
    }
    root_elem.ok_or_else(|| bad("document has no root element"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::axmlw::{XmlAttr, XmlElement, write_xml};

    #[test]
    fn round_trips_a_manifest_shape() {
        let doc = XmlElement::new("manifest")
            .attr(XmlAttr::plain_str("package", "com.example.app"))
            .attr(XmlAttr::android_int("versionCode", 0x0101021b, 42))
            .child(
                XmlElement::new("application")
                    .attr(XmlAttr::android_ref("label", 0x01010001, 0x7f010000))
                    .child(
                        XmlElement::new("activity")
                            .attr(XmlAttr::android_str("name", 0x01010003, ".Main")),
                    ),
            );
        for utf8 in [false, true] {
            let bytes = write_xml(&doc, utf8);
            let root = parse(&bytes).unwrap();
            assert_eq!(root.name, "manifest");
            assert_eq!(
                root.plain_attr("package").unwrap().value.as_str(),
                Some("com.example.app")
            );
            assert_eq!(
                root.android_attr(0x0101021b, "versionCode").unwrap().value,
                Value::Int(42)
            );
            let app = root.children_named("application").next().unwrap();
            assert_eq!(
                app.android_attr(0x01010001, "label").unwrap().value,
                Value::Reference(0x7f010000)
            );
            let act = app.children_named("activity").next().unwrap();
            assert_eq!(
                act.android_attr(0x01010003, "name").unwrap().value.as_str(),
                Some(".Main")
            );
        }
    }

    #[test]
    fn colors_and_dimensions() {
        let p = StringPool::default();
        assert_eq!(
            Value::decode(TYPE_INT_COLOR_RGB4, 0xF80, &p),
            Value::Color(0xFFFF_8800)
        );
        assert_eq!(
            Value::decode(TYPE_INT_COLOR_ARGB4, 0x8F80, &p),
            Value::Color(0x88FF_8800)
        );
        // 108dp: mantissa 108 << 8, radix 0 (23p0), unit dp.
        let (v, unit) = complex_to_float((108 << 8) | 1);
        assert_eq!((v, unit), (108.0, 1));
    }

    #[test]
    fn damaged_input_never_panics() {
        let doc =
            XmlElement::new("a").child(XmlElement::new("b").attr(XmlAttr::plain_str("k", "v")));
        let bytes = write_xml(&doc, false);
        for cut in 0..bytes.len() {
            let _ = parse(&bytes[..cut]);
        }
        let mut x: u32 = 12345;
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let mut b = bytes.clone();
            let i = x as usize % b.len();
            b[i] ^= (x >> 8) as u8 | 1;
            let _ = parse(&b);
        }
        assert!(parse(b"").is_err());
    }
}

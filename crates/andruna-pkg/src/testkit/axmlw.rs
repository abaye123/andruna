//! A binary XML writer for test APKs, in the layout aapt2 produces:
//! string pool (attribute names with resource IDs first, in resource map
//! order), resource map, namespace, elements.

use crate::axml::ANDROID_NS;

/// A typed attribute value.
#[derive(Clone, Debug)]
pub enum XmlValue {
    Str(String),
    Int(u32),
    Bool(bool),
    Ref(u32),
    Color(u32),
    Dimension(u32),
    Float(f32),
}

#[derive(Clone, Debug)]
pub struct XmlAttr {
    pub android: bool,
    pub name: String,
    pub res_id: Option<u32>,
    pub value: XmlValue,
}

impl XmlAttr {
    pub fn plain_str(name: &str, v: &str) -> Self {
        Self {
            android: false,
            name: name.into(),
            res_id: None,
            value: XmlValue::Str(v.into()),
        }
    }
    pub fn android(name: &str, res_id: u32, value: XmlValue) -> Self {
        Self {
            android: true,
            name: name.into(),
            res_id: Some(res_id),
            value,
        }
    }
    pub fn android_str(name: &str, res_id: u32, v: &str) -> Self {
        Self::android(name, res_id, XmlValue::Str(v.into()))
    }
    pub fn android_int(name: &str, res_id: u32, v: u32) -> Self {
        Self::android(name, res_id, XmlValue::Int(v))
    }
    pub fn android_bool(name: &str, res_id: u32, v: bool) -> Self {
        Self::android(name, res_id, XmlValue::Bool(v))
    }
    pub fn android_ref(name: &str, res_id: u32, v: u32) -> Self {
        Self::android(name, res_id, XmlValue::Ref(v))
    }
}

#[derive(Clone, Debug, Default)]
pub struct XmlElement {
    pub name: String,
    pub attrs: Vec<XmlAttr>,
    pub children: Vec<XmlElement>,
}

impl XmlElement {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
    pub fn attr(mut self, a: XmlAttr) -> Self {
        self.attrs.push(a);
        self
    }
    pub fn child(mut self, c: XmlElement) -> Self {
        self.children.push(c);
        self
    }
}

/// Builds a string pool chunk.
pub fn string_pool(strings: &[String], utf8: bool) -> Vec<u8> {
    let mut offsets = Vec::new();
    let mut data = Vec::new();
    for s in strings {
        offsets.push(data.len() as u32);
        if utf8 {
            let n16 = s.encode_utf16().count();
            let b = s.as_bytes();
            for n in [n16, b.len()] {
                if n > 0x7F {
                    data.push(0x80 | (n >> 8) as u8);
                    data.push(n as u8);
                } else {
                    data.push(n as u8);
                }
            }
            data.extend_from_slice(b);
            data.push(0);
        } else {
            let u: Vec<u16> = s.encode_utf16().collect();
            if u.len() > 0x7FFF {
                data.extend_from_slice(&(0x8000 | (u.len() >> 16) as u16).to_le_bytes());
                data.extend_from_slice(&(u.len() as u16).to_le_bytes());
            } else {
                data.extend_from_slice(&(u.len() as u16).to_le_bytes());
            }
            for c in u {
                data.extend_from_slice(&c.to_le_bytes());
            }
            data.extend_from_slice(&[0, 0]);
        }
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }
    let header = 28u32;
    let strings_start = header + 4 * strings.len() as u32;
    let size = strings_start + data.len() as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&0x0001u16.to_le_bytes());
    out.extend_from_slice(&(header as u16).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(if utf8 { 0x100u32 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&strings_start.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&data);
    out
}

struct Pool {
    strings: Vec<String>,
    res_ids: Vec<u32>,
}

impl Pool {
    fn idx(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.into());
        (self.strings.len() - 1) as u32
    }
    /// Attribute names with resource IDs go first, one string per ID.
    fn attr_idx(&mut self, name: &str, id: u32) -> u32 {
        if let Some(i) = self.res_ids.iter().position(|&x| x == id) {
            return i as u32;
        }
        let i = self.res_ids.len();
        self.res_ids.push(id);
        self.strings.insert(i, name.into());
        i as u32
    }
}

fn collect_attr_ids(e: &XmlElement, pool: &mut Pool) {
    for a in &e.attrs {
        if let Some(id) = a.res_id {
            pool.attr_idx(&a.name, id);
        }
    }
    for c in &e.children {
        collect_attr_ids(c, pool);
    }
}

fn uses_android(e: &XmlElement) -> bool {
    e.attrs.iter().any(|a| a.android) || e.children.iter().any(uses_android)
}

fn node(ty: u16, ext: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(&(16 + ext.len() as u32).to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes()); // line
    v.extend_from_slice(&u32::MAX.to_le_bytes()); // comment
    v.extend_from_slice(ext);
    v
}

fn write_el(e: &XmlElement, pool: &mut Pool, android_ns: u32, out: &mut Vec<u8>) {
    let mut ext = Vec::new();
    ext.extend_from_slice(&u32::MAX.to_le_bytes());
    ext.extend_from_slice(&pool.idx(&e.name).to_le_bytes());
    ext.extend_from_slice(&20u16.to_le_bytes());
    ext.extend_from_slice(&20u16.to_le_bytes());
    ext.extend_from_slice(&(e.attrs.len() as u16).to_le_bytes());
    ext.extend_from_slice(&[0u8; 6]);
    for a in &e.attrs {
        let ns = if a.android { android_ns } else { u32::MAX };
        let name = match a.res_id {
            Some(id) => pool.attr_idx(&a.name, id),
            None => pool.idx(&a.name),
        };
        let (raw, ty, data) = match &a.value {
            XmlValue::Str(s) => {
                let i = pool.idx(s);
                (i, 0x03u8, i)
            }
            XmlValue::Int(v) => (u32::MAX, 0x10, *v),
            XmlValue::Bool(b) => (u32::MAX, 0x12, if *b { u32::MAX } else { 0 }),
            XmlValue::Ref(r) => (u32::MAX, 0x01, *r),
            XmlValue::Color(c) => (u32::MAX, 0x1c, *c),
            XmlValue::Dimension(d) => (u32::MAX, 0x05, *d),
            XmlValue::Float(f) => (u32::MAX, 0x04, f.to_bits()),
        };
        ext.extend_from_slice(&ns.to_le_bytes());
        ext.extend_from_slice(&name.to_le_bytes());
        ext.extend_from_slice(&raw.to_le_bytes());
        ext.extend_from_slice(&8u16.to_le_bytes());
        ext.push(0);
        ext.push(ty);
        ext.extend_from_slice(&data.to_le_bytes());
    }
    out.extend(node(0x0102, &ext));
    for c in &e.children {
        write_el(c, pool, android_ns, out);
    }
    let mut end = Vec::new();
    end.extend_from_slice(&u32::MAX.to_le_bytes());
    end.extend_from_slice(&pool.idx(&e.name).to_le_bytes());
    out.extend(node(0x0103, &end));
}

/// Writes `root` as a binary XML document.
pub fn write_xml(root: &XmlElement, utf8: bool) -> Vec<u8> {
    let mut pool = Pool {
        strings: Vec::new(),
        res_ids: Vec::new(),
    };
    collect_attr_ids(root, &mut pool);
    let android = uses_android(root);
    let (prefix, uri) = (pool.idx("android"), pool.idx(ANDROID_NS));
    let mut body = Vec::new();
    if android {
        let mut ns = Vec::new();
        ns.extend_from_slice(&prefix.to_le_bytes());
        ns.extend_from_slice(&uri.to_le_bytes());
        body.extend(node(0x0100, &ns));
    }
    write_el(root, &mut pool, uri, &mut body);
    if android {
        let mut ns = Vec::new();
        ns.extend_from_slice(&prefix.to_le_bytes());
        ns.extend_from_slice(&uri.to_le_bytes());
        body.extend(node(0x0101, &ns));
    }
    let sp = string_pool(&pool.strings, utf8);
    let mut map = Vec::new();
    if !pool.res_ids.is_empty() {
        map.extend_from_slice(&0x0180u16.to_le_bytes());
        map.extend_from_slice(&8u16.to_le_bytes());
        map.extend_from_slice(&(8 + 4 * pool.res_ids.len() as u32).to_le_bytes());
        for id in &pool.res_ids {
            map.extend_from_slice(&id.to_le_bytes());
        }
    }
    let total = 8 + sp.len() + map.len() + body.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x0003u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend(sp);
    out.extend(map);
    out.extend(body);
    out
}

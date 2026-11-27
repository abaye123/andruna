//! A resource table writer for test APKs.

use super::axmlw::string_pool;

/// A simple resource value.
#[derive(Clone, Debug)]
pub enum Val {
    Str(String),
    Ref(u32),
    Color(u32),
    Int(u32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cfg {
    language: [u8; 2],
    region: [u8; 2],
    density: u16,
    sdk: u16,
    night: bool,
}

/// Parses qualifiers such as `iw`, `fr-rCA`, `xxhdpi`, `anydpi-v26`,
/// `night`, `480dpi`; empty is the default configuration.
fn parse_qualifiers(q: &str) -> Cfg {
    let mut c = Cfg::default();
    for part in q.split('-').filter(|p| !p.is_empty()) {
        let density = match part {
            "ldpi" => Some(120),
            "mdpi" => Some(160),
            "tvdpi" => Some(213),
            "hdpi" => Some(240),
            "xhdpi" => Some(320),
            "xxhdpi" => Some(480),
            "xxxhdpi" => Some(640),
            "anydpi" => Some(0xFFFE),
            "nodpi" => Some(0xFFFF),
            p if p.ends_with("dpi") => p.trim_end_matches("dpi").parse().ok(),
            _ => None,
        };
        if let Some(d) = density {
            c.density = d;
        } else if part == "night" {
            c.night = true;
        } else if let Some(v) = part.strip_prefix('v').and_then(|v| v.parse().ok()) {
            c.sdk = v;
        } else if let Some(r) = part.strip_prefix('r') {
            let b = r.as_bytes();
            c.region = [b[0], b[1]];
        } else {
            let b = part.as_bytes();
            c.language = [b[0], b[1]];
        }
    }
    c
}

fn config_bytes(c: &Cfg) -> Vec<u8> {
    let mut v = vec![0u8; 64];
    v[0..4].copy_from_slice(&64u32.to_le_bytes());
    v[8..10].copy_from_slice(&c.language);
    v[10..12].copy_from_slice(&c.region);
    v[14..16].copy_from_slice(&c.density.to_le_bytes());
    v[24..26].copy_from_slice(&c.sdk.to_le_bytes());
    if c.night {
        v[29] = 0x20;
    }
    v
}

struct Res {
    type_index: usize,
    key: String,
    values: Vec<(Cfg, Val)>,
}

/// Builds a table with one package (ID 0x7f).
pub struct TableBuilder {
    package: String,
    types: Vec<String>,
    res: Vec<Res>,
}

impl TableBuilder {
    pub fn new(package: &str) -> Self {
        Self {
            package: package.into(),
            types: Vec::new(),
            res: Vec::new(),
        }
    }

    /// Adds a resource with values per qualifier string; returns its ID.
    pub fn add(&mut self, ty: &str, name: &str, values: &[(&str, Val)]) -> u32 {
        let ti = match self.types.iter().position(|t| t == ty) {
            Some(i) => i,
            None => {
                self.types.push(ty.into());
                self.types.len() - 1
            }
        };
        let entry = self.res.iter().filter(|r| r.type_index == ti).count();
        self.res.push(Res {
            type_index: ti,
            key: name.into(),
            values: values
                .iter()
                .map(|(q, v)| (parse_qualifiers(q), v.clone()))
                .collect(),
        });
        0x7f00_0000 | (((ti + 1) as u32) << 16) | entry as u32
    }

    pub fn build(&self) -> Vec<u8> {
        let mut global: Vec<String> = Vec::new();
        let gidx = |s: &str, g: &mut Vec<String>| -> u32 {
            if let Some(i) = g.iter().position(|x| x == s) {
                return i as u32;
            }
            g.push(s.into());
            (g.len() - 1) as u32
        };
        let keys: Vec<String> = self.res.iter().map(|r| r.key.clone()).collect();
        let mut body = Vec::new();
        for (ti, _) in self.types.iter().enumerate() {
            let in_type: Vec<(usize, &Res)> = self
                .res
                .iter()
                .enumerate()
                .filter(|(_, r)| r.type_index == ti)
                .collect();
            let n = in_type.len();
            // Type spec.
            let mut spec = Vec::new();
            spec.extend_from_slice(&0x0202u16.to_le_bytes());
            spec.extend_from_slice(&16u16.to_le_bytes());
            spec.extend_from_slice(&(16 + 4 * n as u32).to_le_bytes());
            spec.push((ti + 1) as u8);
            spec.push(0);
            spec.extend_from_slice(&0u16.to_le_bytes());
            spec.extend_from_slice(&(n as u32).to_le_bytes());
            spec.extend(std::iter::repeat_n(0u8, 4 * n));
            body.extend(spec);
            // One type chunk per configuration.
            let mut cfgs: Vec<Cfg> = Vec::new();
            for (_, r) in &in_type {
                for (c, _) in &r.values {
                    if !cfgs.contains(c) {
                        cfgs.push(c.clone());
                    }
                }
            }
            for cfg in cfgs {
                let header = 20 + 64;
                let mut offsets = Vec::new();
                let mut entries = Vec::new();
                for (ri, r) in &in_type {
                    match r.values.iter().find(|(c, _)| *c == cfg) {
                        None => offsets.push(u32::MAX),
                        Some((_, v)) => {
                            offsets.push(entries.len() as u32);
                            let (ty, data) = match v {
                                Val::Str(s) => (0x03u8, gidx(s, &mut global)),
                                Val::Ref(r) => (0x01, *r),
                                Val::Color(c) => (0x1c, *c),
                                Val::Int(i) => (0x10, *i),
                            };
                            entries.extend_from_slice(&8u16.to_le_bytes());
                            entries.extend_from_slice(&0u16.to_le_bytes());
                            entries.extend_from_slice(&(*ri as u32).to_le_bytes());
                            entries.extend_from_slice(&8u16.to_le_bytes());
                            entries.push(0);
                            entries.push(ty);
                            entries.extend_from_slice(&data.to_le_bytes());
                        }
                    }
                }
                let entries_start = header + 4 * n;
                let size = entries_start + entries.len();
                let mut t = Vec::new();
                t.extend_from_slice(&0x0201u16.to_le_bytes());
                t.extend_from_slice(&(header as u16).to_le_bytes());
                t.extend_from_slice(&(size as u32).to_le_bytes());
                t.push((ti + 1) as u8);
                t.push(0);
                t.extend_from_slice(&0u16.to_le_bytes());
                t.extend_from_slice(&(n as u32).to_le_bytes());
                t.extend_from_slice(&(entries_start as u32).to_le_bytes());
                t.extend(config_bytes(&cfg));
                for o in offsets {
                    t.extend_from_slice(&o.to_le_bytes());
                }
                t.extend(entries);
                body.extend(t);
            }
        }
        let type_pool = string_pool(&self.types, false);
        let key_pool = string_pool(&keys, true);
        let pkg_header = 288usize;
        let mut pkg = Vec::new();
        pkg.extend_from_slice(&0x0200u16.to_le_bytes());
        pkg.extend_from_slice(&(pkg_header as u16).to_le_bytes());
        let pkg_size = pkg_header + type_pool.len() + key_pool.len() + body.len();
        pkg.extend_from_slice(&(pkg_size as u32).to_le_bytes());
        pkg.extend_from_slice(&0x7fu32.to_le_bytes());
        let mut name = [0u8; 256];
        for (i, u) in self.package.encode_utf16().take(127).enumerate() {
            name[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        pkg.extend_from_slice(&name);
        pkg.extend_from_slice(&(pkg_header as u32).to_le_bytes()); // type strings
        pkg.extend_from_slice(&(self.types.len() as u32).to_le_bytes());
        pkg.extend_from_slice(&((pkg_header + type_pool.len()) as u32).to_le_bytes());
        pkg.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        pkg.extend_from_slice(&0u32.to_le_bytes()); // typeIdOffset
        pkg.extend(type_pool);
        pkg.extend(key_pool);
        pkg.extend(body);
        let gpool = string_pool(&global, true);
        let total = 12 + gpool.len() + pkg.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&0x0002u16.to_le_bytes());
        out.extend_from_slice(&12u16.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend(gpool);
        out.extend(pkg);
        out
    }
}

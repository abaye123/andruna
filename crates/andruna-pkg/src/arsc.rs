//! The resource table (`resources.arsc`) and resource lookup.
//!
//! Enough of `ResTable` to resolve what an installer needs: the
//! application label in the user's locale and the launcher icon at the
//! best density. The table is a `RES_TABLE_TYPE` chunk with a global string
//! pool and packages; each package has type and key string pools, type
//! specs and types, and each type chunk carries a configuration
//! (`ResTable_config`) and entries (dense, sparse or 16-bit offsets;
//! simple, compact or map entries).
//!
//! Configuration matching is a subset of Android's: locale (language,
//! region, with the legacy codes `iw`, `in`, `ji` equal to `he`, `id`,
//! `yi`), density (with `anydpi` and `nodpi`) and SDK version. A device
//! with no SIM, light mode and an unspecified screen is assumed; entries
//! for other mobile codes or night mode are not chosen.

use std::collections::HashMap;

use crate::axml::{Bytes, StringPool, Value, chunk_at};
use crate::error::{Code, Error, Result};

const RES_TABLE_TYPE: u16 = 0x0002;
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;

const FLAG_SPARSE: u8 = 0x01;
const FLAG_OFFSET16: u8 = 0x02;
const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
const ENTRY_FLAG_COMPACT: u16 = 0x0008;

/// `density` values with special meaning.
pub const DENSITY_DEFAULT: u16 = 0;
pub const DENSITY_ANY: u16 = 0xFFFE;
pub const DENSITY_NONE: u16 = 0xFFFF;

/// The configuration qualifiers this crate evaluates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub mcc: u16,
    pub mnc: u16,
    /// Lowercase language code (two or three letters), empty if none.
    pub language: String,
    /// Uppercase region code, empty if none.
    pub region: String,
    pub density: u16,
    pub sdk: u16,
    /// `uiMode` night bits (0 any, 1 not night, 2 night).
    pub night: u8,
    /// True when the configuration has qualifiers this crate does not
    /// evaluate (screen size, orientation, ...); such entries rank below
    /// entries without them.
    pub other: bool,
}

/// Normalizes legacy language codes to their current form.
pub fn canonical_language(l: &str) -> &str {
    match l {
        "iw" => "he",
        "in" => "id",
        "ji" => "yi",
        _ => l,
    }
}

fn unpack_language(b: [u8; 2], base: u8) -> String {
    if b[0] & 0x80 != 0 {
        // Three letters packed in 15 bits.
        let first = b[1] & 0x1f;
        let second = ((b[1] & 0xe0) >> 5) | ((b[0] & 0x03) << 3);
        let third = (b[0] & 0x7c) >> 2;
        [first, second, third]
            .iter()
            .map(|&c| (c + base) as char)
            .collect()
    } else {
        b.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect()
    }
}

impl Config {
    fn parse(d: Bytes<'_>, o: usize) -> Option<Self> {
        let size = d.u32(o)? as usize;
        let cfg = Bytes(d.slice(o, size)?);
        let get8 = |k: usize| if k < size { cfg.u8(k).unwrap_or(0) } else { 0 };
        let get16 = |k: usize| {
            if k + 2 <= size {
                cfg.u16(k).unwrap_or(0)
            } else {
                0
            }
        };
        let language = unpack_language([get8(8), get8(9)], b'a').to_ascii_lowercase();
        let region = unpack_language([get8(10), get8(11)], b'0').to_ascii_uppercase();
        let ui_mode = get8(29);
        let mut other = false;
        // orientation, touchscreen, keyboard, navigation, input flags,
        // screen size, screen layout, ui mode type, smallest/width/height dp.
        for k in [12usize, 13, 16, 17, 18, 28, 48, 49] {
            if get8(k) != 0 {
                other = true;
            }
        }
        for k in [20usize, 22, 30, 32, 34] {
            if get16(k) != 0 {
                other = true;
            }
        }
        if ui_mode & 0x0f != 0 {
            other = true;
        }
        // Script and variant subtags.
        for k in 36..48 {
            if get8(k) != 0 {
                other = true;
            }
        }
        Some(Self {
            mcc: get16(4),
            mnc: get16(6),
            language: canonical_language(&language).to_string(),
            region,
            density: get16(14),
            sdk: get16(24),
            night: (ui_mode >> 4) & 0x3,
            other,
        })
    }

    /// A short qualifier string, for diagnostics and tests.
    pub fn qualifiers(&self) -> String {
        let mut q = Vec::new();
        if !self.language.is_empty() {
            q.push(self.language.clone());
        }
        if !self.region.is_empty() {
            q.push(format!("r{}", self.region));
        }
        match self.density {
            DENSITY_DEFAULT => {}
            DENSITY_ANY => q.push("anydpi".into()),
            DENSITY_NONE => q.push("nodpi".into()),
            d => q.push(format!("{d}dpi")),
        }
        if self.sdk != 0 {
            q.push(format!("v{}", self.sdk));
        }
        if q.is_empty() {
            "default".into()
        } else {
            q.join("-")
        }
    }
}

/// What the lookup prefers.
#[derive(Clone, Debug)]
pub struct Preferences {
    /// Locales in order of preference, as `(language, region)`; region
    /// may be empty.
    pub locales: Vec<(String, String)>,
    /// Target screen density in dpi (160 = mdpi).
    pub density: u16,
    /// Platform API level.
    pub sdk: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            locales: Vec::new(),
            density: 160,
            sdk: crate::PLATFORM_SDK as u16,
        }
    }
}

impl Preferences {
    /// Preferences for one locale tag such as `he-IL`, `en` or `iw`.
    pub fn for_locale(tag: &str) -> Self {
        let mut it = tag.split(['-', '_']);
        let lang = it.next().unwrap_or("").to_ascii_lowercase();
        let region = it
            .find(|s| s.len() == 2 || (s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit())))
            .unwrap_or("")
            .to_ascii_uppercase();
        Self {
            locales: vec![(canonical_language(&lang).to_string(), region)],
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    config: usize,
    key: u32,
    value: EntryValue,
}

#[derive(Clone, Debug)]
enum EntryValue {
    Simple(u8, u32),
    /// A map (style, array, ...): not resolved by this crate.
    Complex,
}

#[derive(Debug, Default)]
struct Package {
    name: String,
    type_names: StringPool,
    keys: StringPool,
    /// `(type id, entry index)` -> entries across configurations.
    entries: HashMap<(u8, u16), Vec<Entry>>,
}

/// A parsed resource table.
#[derive(Debug)]
pub struct ResTable {
    strings: StringPool,
    packages: HashMap<u8, Package>,
    configs: Vec<Config>,
}

fn bad(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidResources, m)
}

/// One resolved value and the configuration it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub value: Value,
    pub config: Config,
    /// `type/name` of the final resource, for diagnostics.
    pub name: String,
}

impl ResTable {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let d = Bytes(data);
        let top = chunk_at(d, 0).ok_or_else(|| bad("table header is damaged"))?;
        if top.ty != RES_TABLE_TYPE {
            return Err(bad("not a resource table"));
        }
        let mut t = ResTable {
            strings: StringPool::default(),
            packages: HashMap::new(),
            configs: Vec::new(),
        };
        let mut o = top.header_size;
        while o + 8 <= top.size {
            let c = chunk_at(d, o).ok_or_else(|| bad(format!("chunk at {o} is damaged")))?;
            match c.ty {
                crate::axml::RES_STRING_POOL_TYPE => {
                    t.strings = StringPool::parse(d, o)
                        .ok_or_else(|| bad("global string pool is damaged"))?;
                }
                RES_TABLE_PACKAGE_TYPE => t.parse_package(d, o, c.size, c.header_size)?,
                _ => {}
            }
            o += c.size;
        }
        Ok(t)
    }

    fn parse_package(&mut self, d: Bytes<'_>, o: usize, size: usize, header: usize) -> Result<()> {
        let id = d
            .u32(o + 8)
            .ok_or_else(|| bad("package header is damaged"))?;
        if id > 0xff {
            return Err(bad("package id out of range"));
        }
        let name_units: Vec<u16> = (0..128)
            .map_while(|i| d.u16(o + 12 + i * 2).filter(|&c| c != 0))
            .collect();
        let type_strings = d.u32(o + 268).unwrap_or(0) as usize;
        let key_strings = d.u32(o + 276).unwrap_or(0) as usize;
        let type_id_offset = if header >= 288 {
            d.u32(o + 284).unwrap_or(0) as u8
        } else {
            0
        };
        let mut pkg = Package {
            name: String::from_utf16_lossy(&name_units),
            ..Default::default()
        };
        if type_strings != 0 {
            pkg.type_names = StringPool::parse(d, o + type_strings)
                .ok_or_else(|| bad("type string pool is damaged"))?;
        }
        if key_strings != 0 {
            pkg.keys = StringPool::parse(d, o + key_strings)
                .ok_or_else(|| bad("key string pool is damaged"))?;
        }
        let end = o + size;
        let mut p = o + header;
        while p + 8 <= end {
            let c = chunk_at(d, p).ok_or_else(|| bad(format!("chunk at {p} is damaged")))?;
            if c.ty == RES_TABLE_TYPE_TYPE {
                self.parse_type(d, p, c.size, c.header_size, &mut pkg)?;
            }
            // Type specs, libraries, overlayables and staged aliases are
            // not needed for lookup.
            let _ = RES_TABLE_TYPE_SPEC_TYPE;
            p += c.size;
        }
        let _ = type_id_offset;
        self.packages.insert(id as u8, pkg);
        Ok(())
    }

    fn parse_type(
        &mut self,
        d: Bytes<'_>,
        o: usize,
        size: usize,
        header: usize,
        pkg: &mut Package,
    ) -> Result<()> {
        let chunk = Bytes(
            d.slice(o, size)
                .ok_or_else(|| bad("type chunk is damaged"))?,
        );
        let type_id = chunk.u8(8).unwrap_or(0);
        let flags = chunk.u8(9).unwrap_or(0);
        let count = chunk.u32(12).unwrap_or(0) as usize;
        let entries_start = chunk.u32(16).unwrap_or(0) as usize;
        if type_id == 0 || entries_start > size || count > size {
            return Err(bad("type chunk header is damaged"));
        }
        let config = Config::parse(chunk, 20).ok_or_else(|| bad("configuration is damaged"))?;
        let cfg_index = self.configs.len();
        self.configs.push(config);
        let mut offsets: Vec<(u16, usize)> = Vec::new();
        if flags & FLAG_SPARSE != 0 {
            for i in 0..count {
                let (Some(idx), Some(off)) =
                    (chunk.u16(header + i * 4), chunk.u16(header + i * 4 + 2))
                else {
                    return Err(bad("sparse entry table is damaged"));
                };
                offsets.push((idx, off as usize * 4));
            }
        } else if flags & FLAG_OFFSET16 != 0 {
            for i in 0..count.min(0x10000) {
                let off = chunk
                    .u16(header + i * 2)
                    .ok_or_else(|| bad("entry table is damaged"))?;
                if off != 0xFFFF {
                    offsets.push((i as u16, off as usize * 4));
                }
            }
        } else {
            for i in 0..count.min(0x10000) {
                let off = chunk
                    .u32(header + i * 4)
                    .ok_or_else(|| bad("entry table is damaged"))?;
                if off != u32::MAX {
                    offsets.push((i as u16, off as usize));
                }
            }
        }
        for (idx, off) in offsets {
            let e = entries_start + off;
            let (Some(esize), Some(eflags), Some(key)) =
                (chunk.u16(e), chunk.u16(e + 2), chunk.u32(e + 4))
            else {
                return Err(bad("entry is damaged"));
            };
            let (key, value) = if eflags & ENTRY_FLAG_COMPACT != 0 {
                (esize as u32, EntryValue::Simple((eflags >> 8) as u8, key))
            } else if eflags & ENTRY_FLAG_COMPLEX != 0 {
                (key, EntryValue::Complex)
            } else {
                let v = e + esize as usize;
                let (Some(ty), Some(data)) = (chunk.u8(v + 3), chunk.u32(v + 4)) else {
                    return Err(bad("entry value is damaged"));
                };
                (key, EntryValue::Simple(ty, data))
            };
            pkg.entries.entry((type_id, idx)).or_default().push(Entry {
                config: cfg_index,
                key,
                value,
            });
        }
        Ok(())
    }

    /// The package names in the table.
    pub fn package_names(&self) -> Vec<&str> {
        self.packages.values().map(|p| p.name.as_str()).collect()
    }

    /// `type/name` of a resource ID, if it exists.
    pub fn name_of(&self, id: u32) -> Option<String> {
        let pkg = self.packages.get(&((id >> 24) as u8))?;
        let t = ((id >> 16) & 0xff) as u8;
        let entries = pkg.entries.get(&(t, (id & 0xffff) as u16))?;
        let ty = pkg.type_names.get(t as u32 - 1)?;
        let key = pkg.keys.get(entries.first()?.key)?;
        Some(format!("{ty}/{key}"))
    }

    /// All configurations under which `id` has a value.
    pub fn configs_of(&self, id: u32) -> Vec<&Config> {
        self.lookup(id)
            .map(|v| v.iter().map(|e| &self.configs[e.config]).collect())
            .unwrap_or_default()
    }

    fn lookup(&self, id: u32) -> Option<&Vec<Entry>> {
        let pkg = self.packages.get(&((id >> 24) as u8))?;
        pkg.entries
            .get(&(((id >> 16) & 0xff) as u8, (id & 0xffff) as u16))
    }

    /// Chooses the best entry for `prefs` among `entries`, Android style:
    /// eliminate entries whose configuration cannot match, then prefer
    /// the best locale, then the best density, then the highest SDK.
    fn best<'e>(&self, entries: &'e [Entry], prefs: &Preferences) -> Option<&'e Entry> {
        let candidates: Vec<&Entry> = entries
            .iter()
            .filter(|e| {
                let c = &self.configs[e.config];
                c.mcc == 0 && c.mnc == 0 && c.night != 2 && c.sdk <= prefs.sdk
            })
            .collect();
        // Locale rank: 3 language and region, 2 language, 1 no locale,
        // 0 another locale (not a match).
        let locale_rank = |c: &Config| -> (u8, usize) {
            if c.language.is_empty() {
                return (1, usize::MAX);
            }
            for (i, (l, r)) in prefs.locales.iter().enumerate() {
                if c.language == *l {
                    if c.region.is_empty() {
                        return (2, i);
                    }
                    if c.region == *r {
                        return (3, i);
                    }
                }
            }
            (0, usize::MAX)
        };
        let matching: Vec<&Entry> = candidates
            .iter()
            .copied()
            .filter(|e| locale_rank(&self.configs[e.config]).0 > 0)
            .collect();
        // Android has no fallback to another locale; when nothing
        // matches, the entry does not resolve. Tolerate tables without a
        // default by falling back to any entry, so a label is still shown.
        let pool = if matching.is_empty() {
            candidates
        } else {
            matching
        };
        let target = prefs.density.max(1) as i32;
        pool.into_iter().max_by(|a, b| {
            let ca = &self.configs[a.config];
            let cb = &self.configs[b.config];
            let (ra, ia) = locale_rank(ca);
            let (rb, ib) = locale_rank(cb);
            // Earlier preferred locales first.
            let la = (ra > 1, std::cmp::Reverse(ia), ra);
            let lb = (rb > 1, std::cmp::Reverse(ib), rb);
            la.cmp(&lb)
                .then_with(|| (!ca.other).cmp(&!cb.other))
                .then_with(|| {
                    density_score(ca.density, target).cmp(&density_score(cb.density, target))
                })
                .then_with(|| ca.sdk.cmp(&cb.sdk))
        })
    }

    /// Resolves `id` for `prefs`, following references (at most 16).
    pub fn resolve(&self, id: u32, prefs: &Preferences) -> Option<Resolved> {
        let mut id = id;
        for _ in 0..16 {
            let entries = self.lookup(id)?;
            let e = self.best(entries, prefs)?;
            let EntryValue::Simple(ty, data) = e.value else {
                return None;
            };
            let value = Value::decode(ty, data, &self.strings);
            match value {
                Value::Reference(next) => id = next,
                value => {
                    return Some(Resolved {
                        value,
                        config: self.configs[e.config].clone(),
                        name: self.name_of(id).unwrap_or_default(),
                    });
                }
            }
        }
        None
    }

    /// The languages (canonical codes) under which `id` has a value.
    pub fn locales_of(&self, id: u32) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .configs_of(id)
            .into_iter()
            .filter(|c| !c.language.is_empty())
            .map(|c| (c.language.clone(), c.region.clone()))
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Higher is better. `anydpi` beats every bitmap density; otherwise the
/// smallest density at or above the target wins, then the largest below.
fn density_score(d: u16, target: i32) -> i64 {
    match d {
        DENSITY_ANY => i64::MAX,
        DENSITY_NONE => 1,
        _ => {
            let d = if d == DENSITY_DEFAULT { 160 } else { d as i32 };
            if d >= target {
                1_000_000 - (d - target) as i64
            } else {
                d as i64 + 2
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::arscw::{TableBuilder, Val};

    fn table() -> ResTable {
        let mut b = TableBuilder::new("com.example.app");
        let label = b.add(
            "string",
            "app_name",
            &[
                ("", Val::Str("Example".into())),
                (
                    "iw",
                    Val::Str("\u{05d3}\u{05d5}\u{05d2}\u{05de}\u{05d4}".into()),
                ),
                ("fr-rCA", Val::Str("Exemple CA".into())),
                ("fr", Val::Str("Exemple".into())),
            ],
        );
        assert_eq!(label, 0x7f01_0000);
        b.add(
            "mipmap",
            "ic_launcher",
            &[
                ("mdpi", Val::Str("res/mipmap-mdpi/ic_launcher.png".into())),
                ("xhdpi", Val::Str("res/mipmap-xhdpi/ic_launcher.png".into())),
                (
                    "xxxhdpi",
                    Val::Str("res/mipmap-xxxhdpi/ic_launcher.png".into()),
                ),
                (
                    "anydpi-v26",
                    Val::Str("res/mipmap-anydpi-v26/ic_launcher.xml".into()),
                ),
            ],
        );
        b.add("string", "alias", &[("", Val::Ref(label))]);
        b.add("color", "bg", &[("", Val::Color(0xFF3DDC84))]);
        ResTable::parse(&b.build()).unwrap()
    }

    fn label(t: &ResTable, loc: &str) -> String {
        t.resolve(0x7f01_0000, &Preferences::for_locale(loc))
            .unwrap()
            .value
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn locale_matching() {
        let t = table();
        assert_eq!(label(&t, "en-US"), "Example");
        assert_eq!(
            label(&t, "he-IL"),
            "\u{05d3}\u{05d5}\u{05d2}\u{05de}\u{05d4}"
        );
        assert_eq!(label(&t, "iw"), "\u{05d3}\u{05d5}\u{05d2}\u{05de}\u{05d4}");
        assert_eq!(label(&t, "fr-CA"), "Exemple CA");
        assert_eq!(label(&t, "fr-FR"), "Exemple");
        assert_eq!(t.name_of(0x7f01_0000).as_deref(), Some("string/app_name"));
        let locs = t.locales_of(0x7f01_0000);
        assert!(locs.contains(&("he".into(), "".into())));
    }

    #[test]
    fn references_and_colors() {
        let t = table();
        let alias = t
            .resolve(0x7f01_0001, &Preferences::for_locale("he"))
            .unwrap();
        assert_eq!(
            alias.value.as_str(),
            Some("\u{05d3}\u{05d5}\u{05d2}\u{05de}\u{05d4}")
        );
        let c = t.resolve(0x7f03_0000, &Preferences::default()).unwrap();
        assert_eq!(c.value, Value::Color(0xFF3DDC84));
    }

    #[test]
    fn density_matching() {
        let t = table();
        let mut p = Preferences::default();
        p.density = 640;
        let r = t.resolve(0x7f02_0000, &p).unwrap();
        assert_eq!(
            r.value.as_str(),
            Some("res/mipmap-anydpi-v26/ic_launcher.xml")
        );
        p.sdk = 25;
        let r = t.resolve(0x7f02_0000, &p).unwrap();
        assert_eq!(r.value.as_str(), Some("res/mipmap-xxxhdpi/ic_launcher.png"));
        p.density = 300;
        let r = t.resolve(0x7f02_0000, &p).unwrap();
        assert_eq!(r.value.as_str(), Some("res/mipmap-xhdpi/ic_launcher.png"));
        p.density = 120;
        let r = t.resolve(0x7f02_0000, &p).unwrap();
        assert_eq!(r.value.as_str(), Some("res/mipmap-mdpi/ic_launcher.png"));
        assert!(t.resolve(0x7f09_0000, &p).is_none());
    }

    #[test]
    fn damaged_tables_never_panic() {
        let mut b = TableBuilder::new("com.example.app");
        b.add(
            "string",
            "a",
            &[("", Val::Str("x".into())), ("iw", Val::Str("y".into()))],
        );
        let bytes = b.build();
        for cut in 0..bytes.len() {
            if let Ok(t) = ResTable::parse(&bytes[..cut]) {
                let _ = t.resolve(0x7f01_0000, &Preferences::default());
            }
        }
        let mut x: u32 = 99;
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let mut d = bytes.clone();
            let i = x as usize % d.len();
            d[i] ^= (x >> 8) as u8 | 1;
            if let Ok(t) = ResTable::parse(&d) {
                let _ = t.resolve(0x7f01_0000, &Preferences::for_locale("he"));
            }
        }
    }
}

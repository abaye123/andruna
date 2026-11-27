//! System properties as bionic reads them: a read-only property area at
//! `/dev/__properties__` (ADR 0019).
//!
//! bionic maps `/dev/__properties__` when it is a regular file (its
//! "pre-split" layout): a `prop_area` header followed by a trie of
//! `prop_trie_node`s and `prop_info`s, which lx writes here in bionic's
//! own format (`libc/system_properties/prop_area.cpp`). The file belongs
//! to root and is read-only, as bionic checks.
//!
//! The values are Andruna's defaults ([`defaults`]) plus what the host
//! process defines before guest code starts ([`define`]). The area is
//! built on first use and does not change afterwards. Setting a property
//! needs the property service, which does not exist yet: bionic's
//! `__system_property_set` then fails, as it does when the service is
//! unreachable.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// `PROP_VALUE_MAX`.
pub const PROP_VALUE_MAX: usize = 92;
const PROP_AREA_MAGIC: u32 = 0x504f_5250;
const PROP_AREA_VERSION: u32 = 0xfc6e_d0ab;
/// `sizeof(prop_area)`: bytes_used, serial, magic, version, reserved[28].
const HEADER: usize = 128;
/// `sizeof(prop_trie_node)`: namelen, prop, left, right, children.
const NODE: usize = 20;
/// `sizeof(prop_info)`: serial, value[92].
const INFO: usize = 4 + PROP_VALUE_MAX;
/// `prop_info::kLongFlag`.
const LONG_FLAG: u32 = 1 << 16;
const LONG_LEGACY_ERROR: &[u8] = b"Must use __system_property_read_callback() to read\0";
/// Area size: bionic's LP64 size, grown if the properties need more.
const MIN_AREA: usize = 128 * 1024;

static DEFINED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
static AREA: OnceLock<Vec<u8>> = OnceLock::new();

/// Defines properties for this process (host set-up, before guest code
/// reads them; later definitions are not seen once the area exists).
/// Returns false if the area was already built.
pub fn define(props: &[(&str, &str)]) -> bool {
    let mut d = crate::util::lock(&DEFINED);
    for (k, v) in props {
        d.insert((*k).to_owned(), (*v).to_owned());
    }
    AREA.get().is_none()
}

/// Andruna's defaults: facts about the build and the machine. See ADR 0019.
pub fn defaults() -> Vec<(String, String)> {
    let (abi, abilist64) = if cfg!(target_arch = "x86_64") {
        ("x86_64", "x86_64")
    } else {
        ("arm64-v8a", "arm64-v8a")
    };
    [
        ("ro.build.version.sdk", "37"),
        ("ro.build.version.release", "17"),
        ("ro.build.version.codename", "REL"),
        ("ro.build.type", "user"),
        ("ro.build.tags", "release-keys"),
        ("ro.debuggable", "0"),
        ("ro.secure", "1"),
        ("ro.product.cpu.abi", abi),
        ("ro.product.cpu.abilist", abilist64),
        ("ro.product.cpu.abilist64", abilist64),
        ("ro.product.cpu.abilist32", ""),
        ("ro.product.brand", "andruna"),
        ("ro.product.name", "andruna"),
        ("ro.product.device", "andruna"),
        ("ro.hardware", "andruna"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
}

/// The property area file's contents.
pub fn area() -> &'static [u8] {
    AREA.get_or_init(|| {
        let mut all: BTreeMap<String, String> = defaults().into_iter().collect();
        for (k, v) in crate::util::lock(&DEFINED).iter() {
            all.insert(k.clone(), v.clone());
        }
        let props: Vec<(&str, &str)> = all.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        build_area(&props)
    })
}

/// Writes a property area holding `props`, laid out as bionic's
/// `prop_area::add` would have.
pub fn build_area(props: &[(&str, &str)]) -> Vec<u8> {
    let mut w = Writer {
        data: vec![0u8; NODE],
    };
    // The dirty backup area follows the root node.
    w.data.resize(NODE + PROP_VALUE_MAX.next_multiple_of(4), 0);
    for (name, value) in props {
        w.add(name.as_bytes(), value.as_bytes());
    }
    let used = w.data.len();
    let size = (HEADER + used).max(MIN_AREA).next_multiple_of(4096);
    let mut out = vec![0u8; size];
    out[0..4].copy_from_slice(&(used as u32).to_le_bytes());
    // serial (4..8) stays 0.
    out[8..12].copy_from_slice(&PROP_AREA_MAGIC.to_le_bytes());
    out[12..16].copy_from_slice(&PROP_AREA_VERSION.to_le_bytes());
    out[HEADER..HEADER + used].copy_from_slice(&w.data);
    out
}

/// Builds the data part (offsets are from its start, `data_` in bionic).
struct Writer {
    data: Vec<u8>,
}

impl Writer {
    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.data[off..off + 4].try_into().expect("4 bytes"))
    }

    fn set_u32(&mut self, off: usize, v: u32) {
        self.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn alloc(&mut self, size: usize) -> usize {
        let off = self.data.len();
        self.data.resize(off + size.next_multiple_of(4), 0);
        off
    }

    fn new_node(&mut self, name: &[u8]) -> usize {
        let off = self.alloc(NODE + name.len() + 1);
        self.set_u32(off, name.len() as u32);
        self.data[off + NODE..off + NODE + name.len()].copy_from_slice(name);
        off
    }

    fn node_name(&self, off: usize) -> &[u8] {
        let n = self.u32_at(off) as usize;
        &self.data[off + NODE..off + NODE + n]
    }

    /// `find_prop_trie_node` with allocation, in the tree at `root`.
    fn find_node(&mut self, root: usize, name: &[u8]) -> usize {
        let mut cur = root;
        loop {
            let other = self.node_name(cur);
            let ord = name.len().cmp(&other.len()).then_with(|| name.cmp(other));
            let slot = match ord {
                core::cmp::Ordering::Equal => return cur,
                core::cmp::Ordering::Less => cur + 8,
                core::cmp::Ordering::Greater => cur + 12,
            };
            let next = self.u32_at(slot) as usize;
            if next == 0 {
                let n = self.new_node(name);
                self.set_u32(slot, n as u32);
                return n;
            }
            cur = next;
        }
    }

    fn add(&mut self, name: &[u8], value: &[u8]) {
        if name.is_empty() || name.split(|&b| b == b'.').any(<[u8]>::is_empty) {
            return;
        }
        let mut cur = 0usize;
        for part in name.split(|&b| b == b'.') {
            let children = self.u32_at(cur + 16) as usize;
            let root = if children == 0 {
                let n = self.new_node(part);
                self.set_u32(cur + 16, n as u32);
                n
            } else {
                children
            };
            cur = self.find_node(root, part);
        }
        if self.u32_at(cur + 4) != 0 {
            return;
        }
        let info = self.alloc(INFO + name.len() + 1);
        self.data[info + INFO..info + INFO + name.len()].copy_from_slice(name);
        if value.len() >= PROP_VALUE_MAX {
            let long = self.alloc(value.len() + 1);
            self.data[long..long + value.len()].copy_from_slice(value);
            let serial = ((LONG_LEGACY_ERROR.len() as u32 - 1) << 24) | LONG_FLAG;
            self.set_u32(info, serial);
            self.data[info + 4..info + 4 + LONG_LEGACY_ERROR.len()]
                .copy_from_slice(LONG_LEGACY_ERROR);
            // long_property.offset: from the prop_info to the value.
            self.set_u32(info + 4 + 56, (long - info) as u32);
        } else {
            self.set_u32(info, (value.len() as u32) << 24);
            self.data[info + 4..info + 4 + value.len()].copy_from_slice(value);
        }
        self.set_u32(cur + 4, info as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader following bionic's `find_property`.
    fn find(area: &[u8], name: &str) -> Option<String> {
        let data = &area[HEADER..];
        let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let mut cur = 0;
        for part in name.split('.') {
            let mut n = u(cur + 16);
            loop {
                if n == 0 {
                    return None;
                }
                let len = u(n);
                let nm = &data[n + NODE..n + NODE + len];
                let ord = part.len().cmp(&len).then_with(|| part.as_bytes().cmp(nm));
                match ord {
                    core::cmp::Ordering::Equal => break,
                    core::cmp::Ordering::Less => n = u(n + 8),
                    core::cmp::Ordering::Greater => n = u(n + 12),
                }
            }
            cur = n;
        }
        let info = u(cur + 4);
        if info == 0 {
            return None;
        }
        let serial = u(info) as u32;
        let s = if serial & LONG_FLAG != 0 {
            let at = info + u(info + 60);
            let end = data[at..].iter().position(|&b| b == 0).unwrap();
            &data[at..at + end]
        } else {
            &data[info + 4..info + 4 + (serial >> 24) as usize]
        };
        Some(String::from_utf8(s.to_vec()).unwrap())
    }

    #[test]
    fn area_layout_and_lookup() {
        let long = "x".repeat(200);
        let props = [
            ("ro.build.version.sdk", "37"),
            ("ro.build.version.release", "17"),
            ("ro.product.cpu.abi", "x86_64"),
            ("a", "1"),
            ("ro.b", ""),
            ("persist.long", long.as_str()),
            ("bad..name", "no"),
        ];
        let a = build_area(&props);
        assert_eq!(a.len() % 4096, 0);
        assert!(a.len() >= MIN_AREA);
        assert_eq!(&a[8..12], &PROP_AREA_MAGIC.to_le_bytes());
        assert_eq!(&a[12..16], &PROP_AREA_VERSION.to_le_bytes());
        for (k, v) in &props[..6] {
            assert_eq!(find(&a, k).as_deref(), Some(*v), "{k}");
        }
        assert_eq!(find(&a, "bad..name"), None);
        assert_eq!(find(&a, "ro.build"), None);
        assert_eq!(find(&a, "ro.build.version.sdkx"), None);
        // bytes_used covers everything written.
        let used = u32::from_le_bytes(a[0..4].try_into().unwrap()) as usize;
        assert!(a[HEADER + used..].iter().all(|&b| b == 0));
    }

    #[test]
    fn defaults_are_in_the_area() {
        assert!(define(&[("andruna.test.defined", "yes")]) || AREA.get().is_some());
        let a = area();
        assert_eq!(find(a, "ro.build.version.sdk").as_deref(), Some("37"));
        assert!(find(a, "ro.product.cpu.abi").is_some());
    }
}

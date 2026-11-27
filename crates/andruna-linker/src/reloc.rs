//! Relocation table formats: Android's packed relocations (`APS2`) and
//! `RELR`, decoded into plain entries. The decoders are pure functions over
//! bytes so they can be tested without loading anything.

use crate::elf::Rela;
use crate::error::{Error, Result};

/// `RELOCATION_GROUPED_BY_INFO_FLAG` and friends (bionic's
/// `linker_reloc_iterators.h`).
const GROUPED_BY_INFO: u64 = 1;
const GROUPED_BY_OFFSET_DELTA: u64 = 2;
const GROUPED_BY_ADDEND: u64 = 4;
const GROUP_HAS_ADDEND: u64 = 8;

/// Decodes an `APS2` stream. For `DT_ANDROID_REL` (`is_rela == false`)
/// the addends are 0. `limit` bounds the entry count against corrupt input
/// (the table cannot hold more relocations than bytes).
pub fn decode_aps2(data: &[u8], is_rela: bool) -> Result<Vec<Rela>> {
    let bad = |m: &str| Error::new(format!("packed relocations: {m}"));
    if data.len() < 4 || &data[..4] != b"APS2" {
        return Err(bad("missing APS2 magic"));
    }
    let mut r = Sleb { data, pos: 4 };
    let count = r.next()?;
    if count < 0 || count as u64 > data.len() as u64 * 8 {
        return Err(bad("bad relocation count"));
    }
    let count = count as usize;
    let mut cur = Rela {
        r_offset: r.next()? as u64,
        ..Rela::default()
    };
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let group_size = r.next()?;
        if group_size <= 0 || (out.len() as u64 + group_size as u64) > count as u64 {
            return Err(bad("bad group size"));
        }
        let flags = r.next()? as u64;
        let by_delta = flags & GROUPED_BY_OFFSET_DELTA != 0;
        let by_info = flags & GROUPED_BY_INFO != 0;
        let by_addend = flags & GROUPED_BY_ADDEND != 0;
        let has_addend = flags & GROUP_HAS_ADDEND != 0;
        if !is_rela && has_addend {
            return Err(bad("addend in a REL table"));
        }
        let delta = if by_delta { r.next()? as u64 } else { 0 };
        if by_info {
            cur.r_info = r.next()? as u64;
        }
        if has_addend && by_addend {
            cur.r_addend = cur.r_addend.wrapping_add(r.next()?);
        } else if !has_addend {
            cur.r_addend = 0;
        }
        for _ in 0..group_size {
            cur.r_offset =
                cur.r_offset
                    .wrapping_add(if by_delta { delta } else { r.next()? as u64 });
            if !by_info {
                cur.r_info = r.next()? as u64;
            }
            if has_addend && !by_addend {
                cur.r_addend = cur.r_addend.wrapping_add(r.next()?);
            }
            out.push(cur);
        }
    }
    Ok(out)
}

struct Sleb<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Sleb<'_> {
    fn next(&mut self) -> Result<i64> {
        let mut v: i64 = 0;
        let mut shift = 0u32;
        loop {
            let b = *self
                .data
                .get(self.pos)
                .ok_or_else(|| Error::new("packed relocations: truncated"))?;
            self.pos += 1;
            v |= i64::from(b & 0x7f).wrapping_shl(shift);
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    v |= -1i64 << shift;
                }
                return Ok(v);
            }
            if shift >= 70 {
                return Err(Error::new("packed relocations: sleb128 too long"));
            }
        }
    }
}

/// Decodes a `RELR` table into the offsets (relative to the load bias) of
/// the words to relocate, in order.
pub fn decode_relr(words: &[u64], mut relocate: impl FnMut(u64)) {
    let mut base = 0u64;
    for &w in words {
        if w & 1 == 0 {
            relocate(w);
            base = w.wrapping_add(8);
        } else {
            let mut bits = w >> 1;
            let mut addr = base;
            while bits != 0 {
                if bits & 1 != 0 {
                    relocate(addr);
                }
                bits >>= 1;
                addr = addr.wrapping_add(8);
            }
            base = base.wrapping_add(63 * 8);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sleb(out: &mut Vec<u8>, mut v: i64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
            out.push(if done { b } else { b | 0x80 });
            if done {
                break;
            }
        }
    }

    pub fn aps2(values: &[i64]) -> Vec<u8> {
        let mut d = b"APS2".to_vec();
        for &v in values {
            sleb(&mut d, v);
        }
        d
    }

    #[test]
    fn relr() {
        // An address, then a bitmap covering the next 63 words, then a
        // second bitmap continuing 63 words later.
        let words = [0x1000u64, (0b1011 << 1) | 1, (1 << 1) | 1];
        let mut got = vec![];
        decode_relr(&words, |a| got.push(a));
        assert_eq!(got, vec![0x1000, 0x1008, 0x1010, 0x1020, 0x1008 + 63 * 8]);
    }

    #[test]
    fn aps2_grouped() {
        // 3 relocations, start offset 0x100, one group of 3 grouped by
        // info (8 = R_X86_64_RELATIVE) and offset delta 8, addend per
        // entry.
        let d = aps2(&[3, 0x100, 3, 1 | 2 | 8, 8, 8, 0x10, 0x10, 0x10]);
        let r = decode_aps2(&d, true).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].r_offset, 0x108);
        assert_eq!(r[2].r_offset, 0x118);
        assert_eq!(r[2].r_info, 8);
        assert_eq!(r[0].r_addend, 0x10);
        assert_eq!(r[2].r_addend, 0x30);
    }

    #[test]
    fn aps2_ungrouped_and_grouped_addend() {
        // Group 1: two entries with own offset deltas and infos, addend
        // shared (grouped by addend). Group 2: one entry, no addend.
        let d = aps2(&[3, 0, 2, 4 | 8, 5, 0x10, 0x100, 0x20, 0x200, 1, 2, 8, 7]);
        let r = decode_aps2(&d, true).unwrap();
        assert_eq!(
            r,
            vec![
                Rela {
                    r_offset: 0x10,
                    r_info: 0x100,
                    r_addend: 5
                },
                Rela {
                    r_offset: 0x30,
                    r_info: 0x200,
                    r_addend: 5
                },
                Rela {
                    r_offset: 0x38,
                    r_info: 7,
                    r_addend: 0
                },
            ]
        );
    }

    #[test]
    fn aps2_rejects_corrupt_input() {
        assert!(decode_aps2(b"APS1", true).is_err());
        assert!(decode_aps2(&aps2(&[2, 0]), true).is_err(), "truncated");
        assert!(
            decode_aps2(&aps2(&[1, 0, 5, 0]), true).is_err(),
            "group > count"
        );
        assert!(
            decode_aps2(&aps2(&[1, 0, 0, 0]), true).is_err(),
            "empty group"
        );
        assert!(
            decode_aps2(&aps2(&[-1, 0]), true).is_err(),
            "negative count"
        );
        assert!(
            decode_aps2(&aps2(&[1, 0, 1, 8, 0, 0, 0]), false).is_err(),
            "addend in REL"
        );
        let mut d = b"APS2".to_vec();
        d.extend(std::iter::repeat_n(0xffu8, 20));
        assert!(decode_aps2(&d, true).is_err(), "endless sleb");
    }
}

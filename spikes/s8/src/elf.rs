//! ELF64 structures and constants used by the loader (System V gABI, the
//! x86_64 and AArch64 psABIs, and Android's packed relocation format).

#![allow(dead_code)]

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Ehdr {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Shdr {
    pub sh_name: u32,
    pub sh_type: u32,
    pub sh_flags: u64,
    pub sh_addr: u64,
    pub sh_offset: u64,
    pub sh_size: u64,
    pub sh_link: u32,
    pub sh_info: u32,
    pub sh_addralign: u64,
    pub sh_entsize: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Dyn {
    pub d_tag: i64,
    pub d_val: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rela {
    pub r_offset: u64,
    pub r_info: u64,
    pub r_addend: i64,
}

// Layout checks against the ELF64 specification.
const _: () = {
    assert!(core::mem::size_of::<Ehdr>() == 64);
    assert!(core::mem::offset_of!(Ehdr, e_phoff) == 32);
    assert!(core::mem::offset_of!(Ehdr, e_phnum) == 56);
    assert!(core::mem::size_of::<Phdr>() == 56);
    assert!(core::mem::offset_of!(Phdr, p_vaddr) == 16);
    assert!(core::mem::offset_of!(Phdr, p_memsz) == 40);
    assert!(core::mem::size_of::<Shdr>() == 64);
    assert!(core::mem::size_of::<Dyn>() == 16);
    assert!(core::mem::size_of::<Sym>() == 24);
    assert!(core::mem::offset_of!(Sym, st_value) == 8);
    assert!(core::mem::size_of::<Rela>() == 24);
};

pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_EH_FRAME: u32 = 0x6474_e550;
pub const PT_GNU_RELRO: u32 = 0x6474_e552;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub const SHT_SYMTAB: u32 = 2;

pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_STRSZ: i64 = 10;
pub const DT_INIT: i64 = 12;
pub const DT_FINI: i64 = 13;
pub const DT_SONAME: i64 = 14;
pub const DT_REL: i64 = 17;
pub const DT_PLTREL: i64 = 20;
pub const DT_JMPREL: i64 = 23;
pub const DT_INIT_ARRAY: i64 = 25;
pub const DT_FINI_ARRAY: i64 = 26;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_FINI_ARRAYSZ: i64 = 28;
pub const DT_RUNPATH: i64 = 29;
pub const DT_FLAGS: i64 = 30;
pub const DT_PREINIT_ARRAY: i64 = 32;
pub const DT_PREINIT_ARRAYSZ: i64 = 33;
pub const DT_RELRSZ: i64 = 35;
pub const DT_RELR: i64 = 36;
pub const DT_RELRENT: i64 = 37;
pub const DT_GNU_HASH: i64 = 0x6fff_fef5;
pub const DT_VERSYM: i64 = 0x6fff_fff0;
pub const DT_FLAGS_1: i64 = 0x6fff_fffb;
/// Android packed relocations (`APS2`), `DT_LOOS + 2 ..= + 5`.
pub const DT_ANDROID_REL: i64 = 0x6000_000f;
pub const DT_ANDROID_RELSZ: i64 = 0x6000_0010;
pub const DT_ANDROID_RELA: i64 = 0x6000_0011;
pub const DT_ANDROID_RELASZ: i64 = 0x6000_0012;
/// Android's pre-standard RELR tags.
pub const DT_ANDROID_RELR: i64 = 0x6fff_e000;
pub const DT_ANDROID_RELRSZ: i64 = 0x6fff_e001;

pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
pub const STT_FUNC: u8 = 2;
pub const STT_TLS: u8 = 6;
pub const STT_GNU_IFUNC: u8 = 10;
pub const SHN_UNDEF: u16 = 0;
pub const STV_DEFAULT: u8 = 0;

pub mod x86_64 {
    pub const R_NONE: u32 = 0;
    pub const R_64: u32 = 1;
    pub const R_GLOB_DAT: u32 = 6;
    pub const R_JUMP_SLOT: u32 = 7;
    pub const R_RELATIVE: u32 = 8;
    pub const R_DTPMOD64: u32 = 16;
    pub const R_DTPOFF64: u32 = 17;
    pub const R_TPOFF64: u32 = 18;
    pub const R_TLSDESC: u32 = 36;
    pub const R_IRELATIVE: u32 = 37;
}

pub mod aarch64 {
    pub const R_NONE: u32 = 0;
    pub const R_ABS64: u32 = 257;
    pub const R_GLOB_DAT: u32 = 1025;
    pub const R_JUMP_SLOT: u32 = 1026;
    pub const R_RELATIVE: u32 = 1027;
    pub const R_TLS_DTPMOD64: u32 = 1028;
    pub const R_TLS_DTPREL64: u32 = 1029;
    pub const R_TLS_TPREL64: u32 = 1030;
    pub const R_TLSDESC: u32 = 1031;
    pub const R_IRELATIVE: u32 = 1032;
}

#[cfg(target_arch = "x86_64")]
pub const EM_NATIVE: u16 = EM_X86_64;
#[cfg(target_arch = "aarch64")]
pub const EM_NATIVE: u16 = EM_AARCH64;

pub fn r_sym(info: u64) -> u32 {
    (info >> 32) as u32
}

pub fn r_type(info: u64) -> u32 {
    info as u32
}

/// Decodes Android's `APS2` packed relocation stream (bionic's
/// `packed_reloc_iterator`, `RELOCATION_GROUPED_BY_*` flags).
pub fn decode_aps2(data: &[u8], is_rela: bool) -> Result<Vec<Rela>, String> {
    if data.len() < 4 || &data[..4] != b"APS2" {
        return Err("packed relocations without APS2 magic".into());
    }
    let mut r = Sleb { data, pos: 4 };
    const BY_INFO: u64 = 1;
    const BY_OFFSET_DELTA: u64 = 2;
    const BY_ADDEND: u64 = 4;
    const HAS_ADDEND: u64 = 8;
    let count = r.next()? as u64;
    let mut cur = Rela {
        r_offset: r.next()? as u64,
        ..Rela::default()
    };
    let mut out = Vec::with_capacity(count as usize);
    while (out.len() as u64) < count {
        let group_size = r.next()? as u64;
        let flags = r.next()? as u64;
        let offset_delta = if flags & BY_OFFSET_DELTA != 0 {
            r.next()? as u64
        } else {
            0
        };
        if flags & BY_INFO != 0 {
            cur.r_info = r.next()? as u64;
        }
        if is_rela {
            if flags & HAS_ADDEND != 0 && flags & BY_ADDEND != 0 {
                cur.r_addend = cur.r_addend.wrapping_add(r.next()?);
            } else if flags & HAS_ADDEND == 0 {
                cur.r_addend = 0;
            }
        }
        for _ in 0..group_size {
            if flags & BY_OFFSET_DELTA != 0 {
                cur.r_offset = cur.r_offset.wrapping_add(offset_delta);
            } else {
                cur.r_offset = cur.r_offset.wrapping_add(r.next()? as u64);
            }
            if flags & BY_INFO == 0 {
                cur.r_info = r.next()? as u64;
            }
            if is_rela && flags & HAS_ADDEND != 0 && flags & BY_ADDEND == 0 {
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
    fn next(&mut self) -> Result<i64, String> {
        let mut v: i64 = 0;
        let mut shift = 0u32;
        loop {
            let b = *self
                .data
                .get(self.pos)
                .ok_or("truncated packed relocations")?;
            self.pos += 1;
            v |= ((b & 0x7f) as i64) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    v |= -1i64 << shift;
                }
                return Ok(v);
            }
            if shift >= 64 {
                return Err("bad sleb128".into());
            }
        }
    }
}

/// Applies a RELR table: `relocate(addr)` for every encoded address
/// (relative to the load bias), in order.
pub fn decode_relr(words: &[u64], mut relocate: impl FnMut(u64)) {
    let mut base = 0u64;
    for &w in words {
        if w & 1 == 0 {
            relocate(w);
            base = w + 8;
        } else {
            let mut bits = w >> 1;
            let mut addr = base;
            while bits != 0 {
                if bits & 1 != 0 {
                    relocate(addr);
                }
                bits >>= 1;
                addr += 8;
            }
            base += 63 * 8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relr_decoding() {
        // An address, then a bitmap covering the next 63 words.
        let words = [0x1000u64, (0b1011 << 1) | 1];
        let mut got = vec![];
        decode_relr(&words, |a| got.push(a));
        assert_eq!(got, vec![0x1000, 0x1008, 0x1010, 0x1020]);
    }

    #[test]
    fn aps2_decoding() {
        // 3 relocations, start offset 0x100, one group of 3 grouped by
        // info (8 = R_X86_64_RELATIVE) and offset delta 8, with addends.
        let mut d = b"APS2".to_vec();
        for v in [3i64, 0x100, 3, 1 | 2 | 8, 8, 8, 0x10, 0x10, 0x10] {
            let mut v = v;
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
                d.push(if done { b } else { b | 0x80 });
                if done {
                    break;
                }
            }
        }
        let r = decode_aps2(&d, true).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].r_offset, 0x108);
        assert_eq!(r[2].r_offset, 0x118);
        assert_eq!(r[2].r_info, 8);
        assert_eq!(r[2].r_addend, 0x30);
    }
}

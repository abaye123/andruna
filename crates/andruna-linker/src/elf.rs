//! ELF64 structures and constants (System V gABI, the x86_64 and AArch64
//! psABIs, GNU and Android extensions), and validation of the parts of a
//! file the loader reads before mapping it.
//!
//! Only 64-bit little-endian `ET_DYN` objects are accepted: every Android
//! executable and library on x86_64 and arm64 is position independent.

use core::mem::{offset_of, size_of};

use crate::error::{Error, Result};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default)]
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Dyn {
    pub d_tag: i64,
    pub d_val: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

impl Sym {
    pub fn bind(&self) -> u8 {
        self.st_info >> 4
    }

    pub fn kind(&self) -> u8 {
        self.st_info & 0xf
    }

    pub fn visibility(&self) -> u8 {
        self.st_other & 3
    }

    pub fn is_defined(&self) -> bool {
        self.st_shndx != SHN_UNDEF
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rela {
    pub r_offset: u64,
    pub r_info: u64,
    pub r_addend: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rel {
    pub r_offset: u64,
    pub r_info: u64,
}

/// `Elf64_Verdef`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Verdef {
    pub vd_version: u16,
    pub vd_flags: u16,
    pub vd_ndx: u16,
    pub vd_cnt: u16,
    pub vd_hash: u32,
    pub vd_aux: u32,
    pub vd_next: u32,
}

/// `Elf64_Verdaux`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Verdaux {
    pub vda_name: u32,
    pub vda_next: u32,
}

/// `Elf64_Verneed`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Verneed {
    pub vn_version: u16,
    pub vn_cnt: u16,
    pub vn_file: u32,
    pub vn_aux: u32,
    pub vn_next: u32,
}

/// `Elf64_Vernaux`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Vernaux {
    pub vna_hash: u32,
    pub vna_flags: u16,
    pub vna_other: u16,
    pub vna_name: u32,
    pub vna_next: u32,
}

// Layout checks against the ELF64 specification.
const _: () = {
    assert!(size_of::<Ehdr>() == 64);
    assert!(offset_of!(Ehdr, e_type) == 16);
    assert!(offset_of!(Ehdr, e_entry) == 24);
    assert!(offset_of!(Ehdr, e_phoff) == 32);
    assert!(offset_of!(Ehdr, e_shoff) == 40);
    assert!(offset_of!(Ehdr, e_phentsize) == 54);
    assert!(offset_of!(Ehdr, e_phnum) == 56);
    assert!(offset_of!(Ehdr, e_shstrndx) == 62);
    assert!(size_of::<Phdr>() == 56);
    assert!(offset_of!(Phdr, p_offset) == 8);
    assert!(offset_of!(Phdr, p_vaddr) == 16);
    assert!(offset_of!(Phdr, p_filesz) == 32);
    assert!(offset_of!(Phdr, p_memsz) == 40);
    assert!(offset_of!(Phdr, p_align) == 48);
    assert!(size_of::<Shdr>() == 64);
    assert!(offset_of!(Shdr, sh_offset) == 24);
    assert!(offset_of!(Shdr, sh_link) == 40);
    assert!(size_of::<Dyn>() == 16);
    assert!(size_of::<Sym>() == 24);
    assert!(offset_of!(Sym, st_shndx) == 6);
    assert!(offset_of!(Sym, st_value) == 8);
    assert!(offset_of!(Sym, st_size) == 16);
    assert!(size_of::<Rela>() == 24);
    assert!(size_of::<Rel>() == 16);
    assert!(size_of::<Verdef>() == 20);
    assert!(offset_of!(Verdef, vd_hash) == 8);
    assert!(offset_of!(Verdef, vd_next) == 16);
    assert!(size_of::<Verdaux>() == 8);
    assert!(size_of::<Verneed>() == 16);
    assert!(offset_of!(Verneed, vn_next) == 12);
    assert!(size_of::<Vernaux>() == 16);
    assert!(offset_of!(Vernaux, vna_other) == 6);
    assert!(offset_of!(Vernaux, vna_next) == 12);
};

pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const EV_CURRENT: u8 = 1;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;

pub const PT_NULL: u32 = 0;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_EH_FRAME: u32 = 0x6474_e550;
pub const PT_GNU_STACK: u32 = 0x6474_e551;
pub const PT_GNU_RELRO: u32 = 0x6474_e552;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub const SHT_SYMTAB: u32 = 2;

pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_PLTGOT: i64 = 3;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_INIT: i64 = 12;
pub const DT_FINI: i64 = 13;
pub const DT_SONAME: i64 = 14;
pub const DT_RPATH: i64 = 15;
pub const DT_SYMBOLIC: i64 = 16;
pub const DT_REL: i64 = 17;
pub const DT_RELSZ: i64 = 18;
pub const DT_RELENT: i64 = 19;
pub const DT_PLTREL: i64 = 20;
pub const DT_DEBUG: i64 = 21;
pub const DT_TEXTREL: i64 = 22;
pub const DT_JMPREL: i64 = 23;
pub const DT_BIND_NOW: i64 = 24;
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
pub const DT_TLSDESC_PLT: i64 = 0x6fff_fef6;
pub const DT_TLSDESC_GOT: i64 = 0x6fff_fef7;
pub const DT_VERSYM: i64 = 0x6fff_fff0;
pub const DT_RELACOUNT: i64 = 0x6fff_fff9;
pub const DT_RELCOUNT: i64 = 0x6fff_fffa;
pub const DT_FLAGS_1: i64 = 0x6fff_fffb;
pub const DT_VERDEF: i64 = 0x6fff_fffc;
pub const DT_VERDEFNUM: i64 = 0x6fff_fffd;
pub const DT_VERNEED: i64 = 0x6fff_fffe;
pub const DT_VERNEEDNUM: i64 = 0x6fff_ffff;
/// Android packed relocations (`APS2`), `DT_LOOS + 2 ..= + 5`.
pub const DT_ANDROID_REL: i64 = 0x6000_000f;
pub const DT_ANDROID_RELSZ: i64 = 0x6000_0010;
pub const DT_ANDROID_RELA: i64 = 0x6000_0011;
pub const DT_ANDROID_RELASZ: i64 = 0x6000_0012;
/// Android's pre-standard RELR tags.
pub const DT_ANDROID_RELR: i64 = 0x6fff_e000;
pub const DT_ANDROID_RELRSZ: i64 = 0x6fff_e001;
pub const DT_ANDROID_RELRENT: i64 = 0x6fff_e003;

pub const DF_SYMBOLIC: u64 = 0x2;
pub const DF_TEXTREL: u64 = 0x4;
pub const DF_BIND_NOW: u64 = 0x8;
pub const DF_STATIC_TLS: u64 = 0x10;
pub const DF_1_NOW: u64 = 0x1;
pub const DF_1_GLOBAL: u64 = 0x2;
pub const DF_1_NODELETE: u64 = 0x8;
pub const DF_1_PIE: u64 = 0x0800_0000;

pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
pub const STB_GNU_UNIQUE: u8 = 10;
pub const STT_NOTYPE: u8 = 0;
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STT_TLS: u8 = 6;
pub const STT_GNU_IFUNC: u8 = 10;
pub const SHN_UNDEF: u16 = 0;
pub const SHN_ABS: u16 = 0xfff1;
pub const STV_DEFAULT: u8 = 0;
pub const STV_PROTECTED: u8 = 3;

/// `versym` value: local symbol.
pub const VER_NDX_LOCAL: u16 = 0;
/// `versym` value: global, unversioned.
pub const VER_NDX_GLOBAL: u16 = 1;
/// `versym` bit: hidden (not the default version).
pub const VERSYM_HIDDEN: u16 = 0x8000;
pub const VER_FLG_BASE: u16 = 1;

pub mod x86_64 {
    pub const R_NONE: u32 = 0;
    pub const R_64: u32 = 1;
    pub const R_PC32: u32 = 2;
    pub const R_COPY: u32 = 5;
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
    pub const R_COPY: u32 = 1024;
    pub const R_GLOB_DAT: u32 = 1025;
    pub const R_JUMP_SLOT: u32 = 1026;
    pub const R_RELATIVE: u32 = 1027;
    pub const R_TLS_DTPMOD64: u32 = 1028;
    pub const R_TLS_DTPREL64: u32 = 1029;
    pub const R_TLS_TPREL64: u32 = 1030;
    pub const R_TLSDESC: u32 = 1031;
    pub const R_IRELATIVE: u32 = 1032;
}

/// The ELF machine of the running process.
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

pub fn machine_name(m: u16) -> &'static str {
    match m {
        EM_X86_64 => "x86_64",
        EM_AARCH64 => "aarch64",
        _ => "unknown",
    }
}

/// Reads a `T` at `off` in `bytes`, if it fits.
pub fn read_at<T: Copy>(bytes: &[u8], off: u64) -> Option<T> {
    let off = usize::try_from(off).ok()?;
    let end = off.checked_add(size_of::<T>())?;
    if end > bytes.len() {
        return None;
    }
    // SAFETY: `off..end` lies inside `bytes`; `T` is a plain-data ELF
    // structure, valid for every bit pattern; unaligned read.
    Some(unsafe { (bytes.as_ptr().add(off) as *const T).read_unaligned() })
}

/// The header and program headers of an object, validated.
#[derive(Clone, Debug)]
pub struct Headers {
    pub ehdr: Ehdr,
    pub phdrs: Vec<Phdr>,
    /// Lowest page-aligned `p_vaddr` of the `PT_LOAD` segments.
    pub min_vaddr: u64,
    /// End of the highest `PT_LOAD` segment, rounded up to `page`.
    pub max_vaddr: u64,
    /// Highest file offset a `PT_LOAD` segment reads.
    pub load_file_end: u64,
}

impl Headers {
    pub fn loads(&self) -> impl Iterator<Item = &Phdr> {
        self.phdrs.iter().filter(|p| p.p_type == PT_LOAD)
    }

    pub fn find(&self, ty: u32) -> Option<&Phdr> {
        self.phdrs.iter().find(|p| p.p_type == ty)
    }

    pub fn span(&self) -> u64 {
        self.max_vaddr - self.min_vaddr
    }
}

/// Validates the ELF header at the start of `bytes` (which must hold at
/// least the header) for `machine`.
pub fn parse_ehdr(bytes: &[u8], machine: u16, what: &str) -> Result<Ehdr> {
    let bad = |m: &str| Error::invalid(what, m);
    let eh: Ehdr = read_at(bytes, 0).ok_or_else(|| bad("file too short for an ELF header"))?;
    let id = &eh.e_ident;
    if &id[..4] != b"\x7fELF" {
        return Err(bad("not an ELF file"));
    }
    if id[4] != ELFCLASS64 {
        return Err(bad("not a 64-bit ELF file"));
    }
    if id[5] != ELFDATA2LSB {
        return Err(bad("not a little-endian ELF file"));
    }
    if id[6] != EV_CURRENT || eh.e_version != u32::from(EV_CURRENT) {
        return Err(bad("unknown ELF version"));
    }
    match eh.e_type {
        ET_DYN => {}
        ET_EXEC => return Err(bad("ET_EXEC is not supported (Android requires PIE)")),
        t => return Err(bad(&format!("ELF type {t} is not a shared object or PIE"))),
    }
    if eh.e_machine != machine {
        return Err(bad(&format!(
            "e_machine {} ({}) does not match {}",
            eh.e_machine,
            machine_name(eh.e_machine),
            machine_name(machine)
        )));
    }
    if usize::from(eh.e_ehsize) < size_of::<Ehdr>() {
        return Err(bad("e_ehsize is too small"));
    }
    if usize::from(eh.e_phentsize) != size_of::<Phdr>() {
        return Err(bad("e_phentsize is not 56"));
    }
    if eh.e_phnum == 0 || usize::from(eh.e_phnum) > 65536 / 56 {
        return Err(bad("bad e_phnum"));
    }
    Ok(eh)
}

/// Reads and validates the program headers. `bytes` holds the file from
/// offset 0 at least to the end of the program header table.
pub fn parse_phdrs(bytes: &[u8], eh: &Ehdr, page: u64, what: &str) -> Result<Headers> {
    let bad = |m: String| Error::invalid(what, &m);
    let table = u64::from(eh.e_phnum) * 56;
    let end = eh
        .e_phoff
        .checked_add(table)
        .ok_or_else(|| bad("program header table overflows".into()))?;
    if end > bytes.len() as u64 {
        return Err(bad("program header table outside the file".into()));
    }
    let phdrs: Vec<Phdr> = (0..u64::from(eh.e_phnum))
        .map(|i| read_at(bytes, eh.e_phoff + i * 56).expect("bounds checked"))
        .collect();
    let mut min = u64::MAX;
    let mut max = 0u64;
    let mut file_end = 0u64;
    let mut last_vaddr = 0u64;
    let mut any = false;
    for (i, p) in phdrs.iter().enumerate() {
        if p.p_type == PT_LOAD {
            if p.p_filesz > p.p_memsz {
                return Err(bad(format!("PT_LOAD {i}: p_filesz > p_memsz")));
            }
            if p.p_align > 1 && !p.p_align.is_power_of_two() {
                return Err(bad(format!("PT_LOAD {i}: p_align is not a power of two")));
            }
            if p.p_vaddr % page != p.p_offset % page {
                return Err(bad(format!(
                    "PT_LOAD {i}: p_vaddr and p_offset differ modulo the page size"
                )));
            }
            let vend = p
                .p_vaddr
                .checked_add(p.p_memsz)
                .filter(|e| *e < (1u64 << 47))
                .ok_or_else(|| bad(format!("PT_LOAD {i}: segment end overflows")))?;
            let fend = p
                .p_offset
                .checked_add(p.p_filesz)
                .ok_or_else(|| bad(format!("PT_LOAD {i}: file range overflows")))?;
            if any && p.p_vaddr < last_vaddr {
                return Err(bad("PT_LOAD segments are not sorted by p_vaddr".into()));
            }
            last_vaddr = p.p_vaddr;
            any = true;
            min = min.min(p.p_vaddr & !(page - 1));
            max = max.max(vend.div_ceil(page) * page);
            file_end = file_end.max(fend);
        }
    }
    if !any {
        return Err(bad("no PT_LOAD segment".into()));
    }
    if phdrs.iter().filter(|p| p.p_type == PT_DYNAMIC).count() != 1 {
        return Err(bad("exactly one PT_DYNAMIC is required".into()));
    }
    if phdrs.iter().filter(|p| p.p_type == PT_TLS).count() > 1 {
        return Err(bad("more than one PT_TLS".into()));
    }
    if let Some(t) = phdrs.iter().find(|p| p.p_type == PT_TLS) {
        if t.p_filesz > t.p_memsz {
            return Err(bad("PT_TLS: p_filesz > p_memsz".into()));
        }
        if t.p_align > 1 && !t.p_align.is_power_of_two() {
            return Err(bad("PT_TLS: p_align is not a power of two".into()));
        }
    }
    // Every other segment that the loader reads at run time must lie in the
    // mapped span.
    for p in &phdrs {
        if matches!(
            p.p_type,
            PT_DYNAMIC | PT_TLS | PT_GNU_RELRO | PT_GNU_EH_FRAME
        ) && (p.p_vaddr < min || p.p_vaddr.saturating_add(p.p_memsz) > max)
        {
            return Err(bad(format!(
                "segment type {:#x} lies outside the loaded span",
                p.p_type
            )));
        }
    }
    Ok(Headers {
        ehdr: *eh,
        phdrs,
        min_vaddr: min,
        max_vaddr: max,
        load_file_end: file_end,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a minimal valid ELF image in memory: header, `n` program
    /// headers (the caller's), nothing else.
    pub fn image(machine: u16, phdrs: &[Phdr]) -> Vec<u8> {
        let mut eh = Ehdr::default();
        eh.e_ident[..4].copy_from_slice(b"\x7fELF");
        eh.e_ident[4] = ELFCLASS64;
        eh.e_ident[5] = ELFDATA2LSB;
        eh.e_ident[6] = EV_CURRENT;
        eh.e_type = ET_DYN;
        eh.e_machine = machine;
        eh.e_version = 1;
        eh.e_phoff = 64;
        eh.e_ehsize = 64;
        eh.e_phentsize = 56;
        eh.e_phnum = phdrs.len() as u16;
        let mut v = vec![0u8; 64 + 56 * phdrs.len()];
        // SAFETY: plain-data structures written into a buffer sized for them.
        unsafe {
            (v.as_mut_ptr() as *mut Ehdr).write_unaligned(eh);
            for (i, p) in phdrs.iter().enumerate() {
                (v.as_mut_ptr().add(64 + 56 * i) as *mut Phdr).write_unaligned(*p);
            }
        }
        v
    }

    pub fn load(vaddr: u64, off: u64, filesz: u64, memsz: u64, flags: u32) -> Phdr {
        Phdr {
            p_type: PT_LOAD,
            p_flags: flags,
            p_offset: off,
            p_vaddr: vaddr,
            p_paddr: vaddr,
            p_filesz: filesz,
            p_memsz: memsz,
            p_align: 4096,
        }
    }

    fn dynamic(vaddr: u64) -> Phdr {
        Phdr {
            p_type: PT_DYNAMIC,
            p_vaddr: vaddr,
            p_memsz: 16,
            p_filesz: 16,
            ..Phdr::default()
        }
    }

    #[test]
    fn accepts_a_valid_object() {
        let ph = [
            load(0, 0, 0x1000, 0x1000, PF_R | PF_X),
            load(0x2000, 0x1000, 0x100, 0x3000, PF_R | PF_W),
            dynamic(0x2000),
        ];
        let img = image(EM_X86_64, &ph);
        let eh = parse_ehdr(&img, EM_X86_64, "t").unwrap();
        let h = parse_phdrs(&img, &eh, 4096, "t").unwrap();
        assert_eq!(h.min_vaddr, 0);
        assert_eq!(h.max_vaddr, 0x5000);
        assert_eq!(h.load_file_end, 0x1100);
        assert_eq!(h.loads().count(), 2);
    }

    #[test]
    fn rejects_bad_headers() {
        let ph = [load(0, 0, 0x1000, 0x1000, PF_R), dynamic(0)];
        let good = image(EM_X86_64, &ph);
        assert!(parse_ehdr(&good, EM_AARCH64, "t").is_err(), "machine");
        assert!(parse_ehdr(&good[..10], EM_X86_64, "t").is_err(), "short");
        let mut b = good.clone();
        b[4] = 1;
        assert!(parse_ehdr(&b, EM_X86_64, "t").is_err(), "class");
        let mut b = good.clone();
        b[5] = 2;
        assert!(parse_ehdr(&b, EM_X86_64, "t").is_err(), "endianness");
        let mut b = good.clone();
        b[16] = ET_EXEC as u8;
        assert!(parse_ehdr(&b, EM_X86_64, "t").is_err(), "ET_EXEC");
        let mut b = good.clone();
        b[0] = 0;
        assert!(parse_ehdr(&b, EM_X86_64, "t").is_err(), "magic");
        // Program header table cut off.
        let eh = parse_ehdr(&good, EM_X86_64, "t").unwrap();
        assert!(parse_phdrs(&good[..100], &eh, 4096, "t").is_err());
    }

    #[test]
    fn rejects_bad_segments() {
        let check = |ph: &[Phdr]| {
            let img = image(EM_X86_64, ph);
            let eh = parse_ehdr(&img, EM_X86_64, "t").unwrap();
            parse_phdrs(&img, &eh, 4096, "t")
        };
        // filesz > memsz
        assert!(check(&[load(0, 0, 0x2000, 0x1000, PF_R), dynamic(0)]).is_err());
        // vaddr and offset not congruent
        assert!(check(&[load(0x1000, 0x10, 0x100, 0x100, PF_R), dynamic(0x1000)]).is_err());
        // unsorted
        assert!(
            check(&[
                load(0x2000, 0x2000, 0x10, 0x10, PF_R),
                load(0, 0, 0x10, 0x10, PF_R),
                dynamic(0)
            ])
            .is_err()
        );
        // no PT_LOAD, no PT_DYNAMIC
        assert!(check(&[dynamic(0)]).is_err());
        assert!(check(&[load(0, 0, 0x10, 0x10, PF_R)]).is_err());
        // PT_DYNAMIC outside the span
        assert!(check(&[load(0, 0, 0x10, 0x10, PF_R), dynamic(0x9000)]).is_err());
        // end overflows
        assert!(check(&[load(0, 0, 0x10, u64::MAX, PF_R), dynamic(0)]).is_err());
    }

    #[test]
    fn symbol_fields() {
        let s = Sym {
            st_info: (STB_WEAK << 4) | STT_FUNC,
            st_other: STV_PROTECTED,
            st_shndx: 5,
            ..Sym::default()
        };
        assert_eq!(s.bind(), STB_WEAK);
        assert_eq!(s.kind(), STT_FUNC);
        assert_eq!(s.visibility(), STV_PROTECTED);
        assert!(s.is_defined());
        assert_eq!(r_sym(0x0000_0005_0000_0007), 5);
        assert_eq!(r_type(0x0000_0005_0000_0007), 7);
    }
}

//! A loaded object: its mapped image, dynamic section, symbol tables and
//! symbol versions, with bounds-checked access to its memory.

use std::ffi::CString;

use crate::elf::*;
use crate::error::{Error, Result};
use crate::image::Image;

/// Index of an object in the linker's table; stable while it is loaded.
pub type ObjectId = usize;

/// The dynamic section entries the loader uses. Addresses are `p_vaddr`
/// values (add the bias for runtime addresses).
#[derive(Clone, Debug, Default)]
pub struct DynamicInfo {
    pub needed: Vec<u64>,
    pub soname: Option<u64>,
    pub runpath: Option<u64>,
    pub rpath: Option<u64>,
    pub strtab: u64,
    pub strsz: u64,
    pub symtab: u64,
    pub gnu_hash: Option<u64>,
    pub sysv_hash: Option<u64>,
    pub rela: Option<(u64, u64)>,
    pub rel: Option<(u64, u64)>,
    pub android_rela: Option<(u64, u64)>,
    pub android_rel: Option<(u64, u64)>,
    pub relr: Option<(u64, u64)>,
    pub jmprel: Option<(u64, u64)>,
    pub pltrel_is_rela: bool,
    pub init: Option<u64>,
    pub fini: Option<u64>,
    pub init_array: Option<(u64, u64)>,
    pub fini_array: Option<(u64, u64)>,
    pub preinit_array: Option<(u64, u64)>,
    pub flags: u64,
    pub flags_1: u64,
    pub symbolic: bool,
    pub textrel: bool,
    pub versym: Option<u64>,
    pub verdef: Option<(u64, u64)>,
    pub verneed: Option<(u64, u64)>,
}

impl DynamicInfo {
    /// Parses a dynamic section given as its entries.
    pub fn from_entries(entries: &[Dyn], what: &str) -> Result<Self> {
        let mut d = DynamicInfo::default();
        let (mut rela, mut relasz) = (None, None);
        let (mut rel, mut relsz) = (None, None);
        let (mut arela, mut arelasz) = (None, None);
        let (mut arel, mut arelsz) = (None, None);
        let (mut relr, mut relrsz) = (None, None);
        let (mut jmprel, mut pltrelsz) = (None, None);
        let (mut ia, mut iasz) = (None, None);
        let (mut fa, mut fasz) = (None, None);
        let (mut pa, mut pasz) = (None, None);
        let (mut vd, mut vdn) = (None, None);
        let (mut vn, mut vnn) = (None, None);
        let mut strtab = None;
        let mut symtab = None;
        let mut strsz = None;
        let mut pltrel = None;
        let bad = |m: &str| Error::invalid(what, m);
        for e in entries {
            let v = e.d_val;
            match e.d_tag {
                DT_NULL => break,
                DT_NEEDED => d.needed.push(v),
                DT_SONAME => d.soname = Some(v),
                DT_RUNPATH => d.runpath = Some(v),
                DT_RPATH => d.rpath = Some(v),
                DT_STRTAB => strtab = Some(v),
                DT_STRSZ => strsz = Some(v),
                DT_SYMTAB => symtab = Some(v),
                DT_SYMENT if v != 24 => return Err(bad("DT_SYMENT is not 24")),
                DT_RELAENT if v != 24 => return Err(bad("DT_RELAENT is not 24")),
                DT_RELENT if v != 16 => return Err(bad("DT_RELENT is not 16")),
                DT_GNU_HASH => d.gnu_hash = Some(v),
                DT_HASH => d.sysv_hash = Some(v),
                DT_RELA => rela = Some(v),
                DT_RELASZ => relasz = Some(v),
                DT_REL => rel = Some(v),
                DT_RELSZ => relsz = Some(v),
                DT_ANDROID_RELA => arela = Some(v),
                DT_ANDROID_RELASZ => arelasz = Some(v),
                DT_ANDROID_REL => arel = Some(v),
                DT_ANDROID_RELSZ => arelsz = Some(v),
                DT_RELR | DT_ANDROID_RELR => relr = Some(v),
                DT_RELRSZ | DT_ANDROID_RELRSZ => relrsz = Some(v),
                DT_RELRENT | DT_ANDROID_RELRENT if v != 8 => {
                    return Err(bad("DT_RELRENT is not 8"));
                }
                DT_JMPREL => jmprel = Some(v),
                DT_PLTRELSZ => pltrelsz = Some(v),
                DT_PLTREL => pltrel = Some(v),
                DT_INIT => d.init = Some(v),
                DT_FINI => d.fini = Some(v),
                DT_INIT_ARRAY => ia = Some(v),
                DT_INIT_ARRAYSZ => iasz = Some(v),
                DT_FINI_ARRAY => fa = Some(v),
                DT_FINI_ARRAYSZ => fasz = Some(v),
                DT_PREINIT_ARRAY => pa = Some(v),
                DT_PREINIT_ARRAYSZ => pasz = Some(v),
                DT_FLAGS => d.flags = v,
                DT_FLAGS_1 => d.flags_1 = v,
                DT_SYMBOLIC => d.symbolic = true,
                DT_TEXTREL => d.textrel = true,
                DT_VERSYM => d.versym = Some(v),
                DT_VERDEF => vd = Some(v),
                DT_VERDEFNUM => vdn = Some(v),
                DT_VERNEED => vn = Some(v),
                DT_VERNEEDNUM => vnn = Some(v),
                _ => {}
            }
        }
        if d.flags & DF_SYMBOLIC != 0 {
            d.symbolic = true;
        }
        if d.flags & DF_TEXTREL != 0 {
            d.textrel = true;
        }
        let pair = |a: Option<u64>, n: Option<u64>, name: &str| -> Result<Option<(u64, u64)>> {
            match (a, n) {
                (Some(a), Some(n)) => Ok(Some((a, n))),
                (None, None) => Ok(None),
                (Some(_), None) | (None, Some(_)) => Err(bad(&format!("{name} without its size"))),
            }
        };
        d.rela = pair(rela, relasz, "DT_RELA")?;
        d.rel = pair(rel, relsz, "DT_REL")?;
        d.android_rela = pair(arela, arelasz, "DT_ANDROID_RELA")?;
        d.android_rel = pair(arel, arelsz, "DT_ANDROID_REL")?;
        d.relr = pair(relr, relrsz, "DT_RELR")?;
        d.jmprel = pair(jmprel, pltrelsz, "DT_JMPREL")?;
        d.init_array = pair(ia, iasz, "DT_INIT_ARRAY")?;
        d.fini_array = pair(fa, fasz, "DT_FINI_ARRAY")?;
        d.preinit_array = pair(pa, pasz, "DT_PREINIT_ARRAY")?;
        d.verdef = pair(vd, vdn, "DT_VERDEF")?;
        d.verneed = pair(vn, vnn, "DT_VERNEED")?;
        d.pltrel_is_rela = match pltrel {
            None => true,
            Some(t) if t == DT_RELA as u64 => true,
            Some(t) if t == DT_REL as u64 => false,
            Some(_) => return Err(bad("bad DT_PLTREL")),
        };
        d.strtab = strtab.ok_or_else(|| bad("no DT_STRTAB"))?;
        d.strsz = strsz.ok_or_else(|| bad("no DT_STRSZ"))?;
        d.symtab = symtab.ok_or_else(|| bad("no DT_SYMTAB"))?;
        if d.gnu_hash.is_none() && d.sysv_hash.is_none() {
            return Err(bad("no DT_HASH or DT_GNU_HASH"));
        }
        Ok(d)
    }
}

/// The GNU hash of a symbol name (`DT_GNU_HASH`).
pub fn gnu_hash(name: &[u8]) -> u32 {
    name.iter().fold(5381u32, |h, &c| {
        h.wrapping_mul(33).wrapping_add(u32::from(c))
    })
}

/// The System V hash of a name (`DT_HASH`, and version hashes).
pub fn elf_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &c in name {
        h = (h << 4).wrapping_add(u32::from(c));
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// A version a reference asks for (from the referencing object's
/// `DT_VERNEED`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionReq {
    pub name: Vec<u8>,
    pub hash: u32,
}

/// The object's `PT_TLS`, once registered.
#[derive(Clone, Copy, Debug, Default)]
pub struct TlsInfo {
    /// Module id (1-based) for `DTPMOD64` and `__tls_get_addr`.
    pub module: usize,
    /// Offset of the block from the thread pointer, for static TLS.
    pub tp_offset: Option<i64>,
    pub image: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

/// Where an object came from.
#[derive(Clone, Debug)]
pub enum Origin {
    /// A Windows path.
    Host(std::path::PathBuf),
    /// A path in the guest file system (opened through lx).
    Guest(String),
    /// Bytes given by the caller (tests).
    Memory,
}

pub struct Object {
    pub id: ObjectId,
    /// The name it was loaded as (a `DT_NEEDED` name or the file name).
    pub name: String,
    pub soname: Option<String>,
    /// The path reported by `dl_iterate_phdr` and `dladdr`.
    pub path: String,
    pub path_c: CString,
    pub origin: Origin,
    pub machine: u16,
    pub image: Image,
    pub headers: Headers,
    /// Runtime address of the program headers.
    pub phdr_addr: u64,
    pub dynamic: DynamicInfo,
    pub dynamic_addr: u64,
    pub needed: Vec<String>,
    /// `DT_RUNPATH` (or `DT_RPATH`) entries with `$ORIGIN` expanded.
    pub runpath: Vec<String>,
    /// Resolved `DT_NEEDED` objects, in order.
    pub deps: Vec<ObjectId>,
    pub nsyms: usize,
    pub tls: Option<TlsInfo>,
    pub is_main: bool,
    /// In the global group (main executable tree, `RTLD_GLOBAL`).
    pub global: bool,
    pub nodelete: bool,
    pub refcount: usize,
    pub linked: bool,
    pub initialized: bool,
    /// `dlopen` root this object was loaded under (its local group).
    pub group_root: ObjectId,
    /// Function symbols of `.symtab`, sorted, for crash reports.
    pub local_symbols: Vec<(u64, u64, String)>,
}

impl Object {
    /// Checks that `[vaddr, vaddr + len)` lies in the mapped span.
    pub fn check(&self, vaddr: u64, len: u64) -> Result<u64> {
        let end = vaddr.checked_add(len);
        match end {
            Some(e) if vaddr >= self.headers.min_vaddr && e <= self.headers.max_vaddr => {
                Ok(self.image.bias.wrapping_add(vaddr))
            }
            _ => Err(Error::invalid(
                &self.name,
                &format!("address {vaddr:#x}+{len:#x} outside the loaded segments"),
            )),
        }
    }

    pub fn read<T: Copy>(&self, vaddr: u64) -> Result<T> {
        let a = self.check(vaddr, core::mem::size_of::<T>() as u64)?;
        // SAFETY: inside the mapped image (checked); plain-data type;
        // unaligned read.
        Ok(unsafe { (a as *const T).read_unaligned() })
    }

    /// A byte range of the image. The image stays mapped while the object
    /// exists, and the loader holds the object while the slice is used.
    pub fn bytes(&self, vaddr: u64, len: u64) -> Result<&[u8]> {
        let a = self.check(vaddr, len)?;
        // SAFETY: inside the mapped, readable image (checked).
        Ok(unsafe { core::slice::from_raw_parts(a as *const u8, len as usize) })
    }

    /// The NUL-terminated string at `off` in `DT_STRTAB`.
    pub fn string(&self, off: u64) -> Result<&[u8]> {
        if off >= self.dynamic.strsz {
            return Err(Error::invalid(
                &self.name,
                "string offset outside DT_STRTAB",
            ));
        }
        let tab = self.bytes(self.dynamic.strtab, self.dynamic.strsz)?;
        let s = &tab[off as usize..];
        let n = s
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid(&self.name, "unterminated string in DT_STRTAB"))?;
        Ok(&s[..n])
    }

    pub fn string_lossy(&self, off: u64) -> String {
        self.string(off)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default()
    }

    pub fn sym(&self, i: usize) -> Result<Sym> {
        if i >= self.nsyms {
            return Err(Error::invalid(&self.name, "symbol index out of range"));
        }
        self.read(self.dynamic.symtab + i as u64 * 24)
    }

    fn versym(&self, i: usize) -> Option<u16> {
        let v = self.dynamic.versym?;
        self.read::<u16>(v + i as u64 * 2).ok()
    }

    /// Runtime address of `p_vaddr`.
    pub fn addr(&self, vaddr: u64) -> u64 {
        self.image.bias.wrapping_add(vaddr)
    }

    /// The version that the reference of symbol `i` asks for.
    pub fn version_req(&self, i: usize) -> Result<Option<VersionReq>> {
        let Some(ndx) = self.versym(i) else {
            return Ok(None);
        };
        let ndx = ndx & !VERSYM_HIDDEN;
        if ndx == VER_NDX_LOCAL || ndx == VER_NDX_GLOBAL {
            return Ok(None);
        }
        let Some((mut at, count)) = self.dynamic.verneed else {
            return Ok(None);
        };
        for _ in 0..count {
            let vn: Verneed = self.read(at)?;
            let mut aux = at + u64::from(vn.vn_aux);
            for _ in 0..vn.vn_cnt {
                let va: Vernaux = self.read(aux)?;
                if va.vna_other == ndx {
                    return Ok(Some(VersionReq {
                        name: self.string(u64::from(va.vna_name))?.to_vec(),
                        hash: va.vna_hash,
                    }));
                }
                if va.vna_next == 0 {
                    break;
                }
                aux += u64::from(va.vna_next);
            }
            if vn.vn_next == 0 {
                break;
            }
            at += u64::from(vn.vn_next);
        }
        // A versym index that no verneed or verdef names: the reference is
        // to one of this object's own versions (defined symbols), which the
        // relocation does not look up.
        Ok(None)
    }

    /// bionic's `find_verdef_version_index`: the index of the version
    /// `req` names in this object's `DT_VERDEF`, `VER_NDX_GLOBAL` if the
    /// object does not define it, 0 when nothing is required.
    fn verdef_index(&self, req: Option<&VersionReq>) -> u16 {
        let Some(req) = req else { return 0 };
        let Some((mut at, count)) = self.dynamic.verdef else {
            return VER_NDX_GLOBAL;
        };
        for _ in 0..count {
            let Ok(vd) = self.read::<Verdef>(at) else {
                break;
            };
            if let Ok(aux) = self.read::<Verdaux>(at + u64::from(vd.vd_aux))
                && vd.vd_hash == req.hash
                && self.string(u64::from(aux.vda_name)).ok() == Some(&req.name[..])
            {
                return vd.vd_ndx;
            }
            if vd.vd_next == 0 {
                break;
            }
            at += u64::from(vd.vd_next);
        }
        VER_NDX_GLOBAL
    }

    fn version_ok(&self, i: usize, want: u16) -> bool {
        let Some(have) = self.versym(i) else {
            return true;
        };
        if want == 0 {
            have & VERSYM_HIDDEN == 0
        } else {
            want == have & !VERSYM_HIDDEN
        }
    }

    fn exported(s: &Sym) -> bool {
        s.is_defined() && matches!(s.bind(), STB_GLOBAL | STB_WEAK | STB_GNU_UNIQUE)
    }

    /// Looks a name up in this object's exported symbols (GNU hash, else
    /// System V hash), honoring symbol versions as bionic does.
    pub fn lookup(&self, name: &[u8], req: Option<&VersionReq>) -> Option<(usize, Sym)> {
        let want = self.verdef_index(req);
        if let Some(h) = self.dynamic.gnu_hash {
            return self.gnu_lookup(h, name, want).ok().flatten();
        }
        let h = self.dynamic.sysv_hash?;
        self.sysv_lookup(h, name, want).ok().flatten()
    }

    fn gnu_lookup(&self, h: u64, name: &[u8], want: u16) -> Result<Option<(usize, Sym)>> {
        let hash = gnu_hash(name);
        let nbuckets = self.read::<u32>(h)?;
        let symoffset = self.read::<u32>(h + 4)?;
        let bloom_size = self.read::<u32>(h + 8)?;
        let bloom_shift = self.read::<u32>(h + 12)?;
        if nbuckets == 0 || bloom_size == 0 {
            return Ok(None);
        }
        let word = self.read::<u64>(h + 16 + u64::from((hash / 64) % bloom_size) * 8)?;
        let mask = (1u64 << (hash % 64)) | (1u64 << ((hash >> bloom_shift) % 64));
        if word & mask != mask {
            return Ok(None);
        }
        let buckets = h + 16 + u64::from(bloom_size) * 8;
        let chains = buckets + u64::from(nbuckets) * 4;
        let mut n = self.read::<u32>(buckets + u64::from(hash % nbuckets) * 4)?;
        if n == 0 || n < symoffset {
            return Ok(None);
        }
        loop {
            let hv = self.read::<u32>(chains + u64::from(n - symoffset) * 4)?;
            if (hv | 1) == (hash | 1) {
                let s = self.sym(n as usize)?;
                if Self::exported(&s)
                    && self.string(u64::from(s.st_name))? == name
                    && self.version_ok(n as usize, want)
                {
                    return Ok(Some((n as usize, s)));
                }
            }
            if hv & 1 != 0 {
                return Ok(None);
            }
            n += 1;
        }
    }

    fn sysv_lookup(&self, h: u64, name: &[u8], want: u16) -> Result<Option<(usize, Sym)>> {
        let hash = elf_hash(name);
        let nbucket = self.read::<u32>(h)?;
        let nchain = self.read::<u32>(h + 4)?;
        if nbucket == 0 {
            return Ok(None);
        }
        let mut i = self.read::<u32>(h + 8 + u64::from(hash % nbucket) * 4)?;
        let chains = h + 8 + u64::from(nbucket) * 4;
        let mut steps = 0;
        while i != 0 && i < nchain && steps <= nchain {
            let s = self.sym(i as usize)?;
            if Self::exported(&s)
                && self.string(u64::from(s.st_name))? == name
                && self.version_ok(i as usize, want)
            {
                return Ok(Some((i as usize, s)));
            }
            i = self.read::<u32>(chains + u64::from(i) * 4)?;
            steps += 1;
        }
        Ok(None)
    }

    /// The exported dynamic symbol containing (or nearest below, within its
    /// size) the runtime address `addr`, for `dladdr`.
    pub fn symbol_at(&self, addr: u64) -> Option<(Sym, String)> {
        let off = addr.wrapping_sub(self.image.bias);
        let mut best: Option<Sym> = None;
        for i in 1..self.nsyms {
            let Ok(s) = self.sym(i) else { break };
            if !s.is_defined() || s.st_shndx == SHN_ABS || s.kind() == STT_TLS {
                continue;
            }
            let fits = off >= s.st_value
                && (off < s.st_value.saturating_add(s.st_size)
                    || (s.st_size == 0 && off == s.st_value));
            if fits && best.is_none_or(|b| s.st_value > b.st_value) {
                best = Some(s);
            }
        }
        best.map(|s| (s, self.string_lossy(u64::from(s.st_name))))
    }
}

/// Number of entries of the dynamic symbol table, from the hash tables.
pub fn symbol_count(o: &Object) -> Result<usize> {
    if let Some(h) = o.dynamic.sysv_hash {
        return Ok(o.read::<u32>(h + 4)? as usize);
    }
    let h = o.dynamic.gnu_hash.expect("checked in DynamicInfo");
    let nb = u64::from(o.read::<u32>(h)?);
    let symoff = u64::from(o.read::<u32>(h + 4)?);
    let bloom = u64::from(o.read::<u32>(h + 8)?);
    let buckets = h + 16 + bloom * 8;
    let mut max = 0u64;
    for b in 0..nb {
        max = max.max(u64::from(o.read::<u32>(buckets + b * 4)?));
    }
    if max < symoff {
        return Ok(symoff as usize);
    }
    let chains = buckets + nb * 4;
    let mut i = max;
    while o.read::<u32>(chains + (i - symoff) * 4)? & 1 == 0 {
        i += 1;
    }
    Ok(i as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        // Reference values from the GNU hash and System V hash definitions.
        assert_eq!(gnu_hash(b""), 5381);
        assert_eq!(gnu_hash(b"printf"), 0x156b_2bb8);
        assert_eq!(gnu_hash(b"exit"), 0x7c96_7e3f);
        assert_eq!(elf_hash(b"printf"), 0x077905a6);
        assert_eq!(elf_hash(b"LIBC"), 0x0005_0d63);
    }

    #[test]
    fn dynamic_validation() {
        let e = |t: i64, v: u64| Dyn { d_tag: t, d_val: v };
        let base = vec![
            e(DT_STRTAB, 0x100),
            e(DT_STRSZ, 0x10),
            e(DT_SYMTAB, 0x200),
            e(DT_GNU_HASH, 0x300),
        ];
        let d = DynamicInfo::from_entries(&base, "t").unwrap();
        assert_eq!(d.strtab, 0x100);
        assert!(d.pltrel_is_rela);
        let mut v = base.clone();
        v.push(e(DT_RELA, 0x400));
        assert!(
            DynamicInfo::from_entries(&v, "t").is_err(),
            "RELA without size"
        );
        let mut v = base.clone();
        v.push(e(DT_SYMENT, 16));
        assert!(DynamicInfo::from_entries(&v, "t").is_err(), "bad syment");
        assert!(
            DynamicInfo::from_entries(&base[1..], "t").is_err(),
            "no strtab"
        );
        let mut v = base.clone();
        v.push(e(DT_FLAGS, DF_SYMBOLIC | DF_TEXTREL));
        let d = DynamicInfo::from_entries(&v, "t").unwrap();
        assert!(d.symbolic && d.textrel);
        // DT_NULL ends the table.
        let mut v = base.clone();
        v.insert(0, e(DT_NULL, 0));
        assert!(DynamicInfo::from_entries(&v, "t").is_err());
    }
}

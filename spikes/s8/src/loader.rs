//! A minimal ELF loader and dynamic linker for Android ELF objects.
//!
//! What it does, in bionic's order: map every `PT_LOAD` segment of an
//! object and its `DT_NEEDED` dependencies (breadth first) with lx's
//! `mmap`; lay out static TLS for all of them; apply relocations (`RELA`,
//! Android `APS2` packed relocations, `RELR`, `JMPREL`), binding symbols
//! against the global scope (load order) and then the host table; protect
//! segments and `PT_GNU_RELRO`; run `DT_PREINIT_ARRAY`, `DT_INIT` and
//! `DT_INIT_ARRAY`, dependencies first.
//!
//! Not done (spike): `dlopen`/`dlclose`, namespaces, symbol versioning,
//! `DT_RUNPATH`, `TLSDESC`, lazy binding (Android binds eagerly anyway),
//! unloading.

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use andruna_lx::linux::mman::{MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PROT_EXEC, PROT_NONE, PROT_READ, PROT_WRITE};

use crate::elf::*;
use crate::host;
use crate::tls;

const PAGE: u64 = 4096;

fn page_down(v: u64) -> u64 {
    v & !(PAGE - 1)
}

fn page_up(v: u64) -> u64 {
    (v + PAGE - 1) & !(PAGE - 1)
}

/// One loaded object.
pub struct Object {
    pub name: String,
    pub path: PathBuf,
    /// Load bias: runtime address = bias + p_vaddr.
    pub bias: u64,
    pub base: u64,
    pub size: u64,
    pub phdrs: Vec<Phdr>,
    pub dynamic: HashMap<i64, Vec<u64>>,
    pub needed: Vec<String>,
    symtab: u64,
    strtab: u64,
    nsyms: usize,
    exports: HashMap<String, Sym>,
    /// Module id for TLS (1-based), 0 if the object has no `PT_TLS`.
    pub tls_module: usize,
    /// Symbols of `.symtab` (function names for crash reports and the
    /// debugger checks), sorted by address.
    pub local_symbols: Vec<(u64, u64, String)>,
    pub file: Vec<u8>,
}

impl Object {
    pub fn dyn1(&self, tag: i64) -> Option<u64> {
        self.dynamic.get(&tag).and_then(|v| v.first().copied())
    }

    fn sym(&self, i: usize) -> Sym {
        assert!(i < self.nsyms, "symbol index out of range");
        // SAFETY: the symbol table lies in a mapped segment of this object
        // and `i` is below the table size derived from the hash table.
        unsafe { ((self.symtab + (i as u64) * 24) as *const Sym).read_unaligned() }
    }

    fn str_at(&self, off: u32) -> String {
        // SAFETY: the string table is mapped and NUL-terminated (ELF rule).
        unsafe { CStr::from_ptr((self.strtab + off as u64) as *const core::ffi::c_char) }
            .to_string_lossy()
            .into_owned()
    }

    /// The `.symtab` symbol containing `addr`, for reports.
    pub fn symbolize(&self, addr: u64) -> Option<(String, u64)> {
        let off = addr.checked_sub(self.bias)?;
        let i = self.local_symbols.partition_point(|s| s.0 <= off);
        let (start, size, name) = self.local_symbols.get(i.checked_sub(1)?)?;
        if off < start + size.max(&1) || *size == 0 {
            Some((name.clone(), off - start))
        } else {
            None
        }
    }
}

/// Timing of one load, for the report.
#[derive(Default, Debug, Clone)]
pub struct LoadStats {
    pub map: Duration,
    pub relocate: Duration,
    pub init: Duration,
    pub relocations: usize,
    pub symbol_lookups: usize,
    pub host_bindings: usize,
    pub unresolved: Vec<String>,
    pub packed_relocations: usize,
    pub relr_relocations: usize,
}

pub struct Linker {
    pub objects: Vec<Object>,
    pub search: Vec<PathBuf>,
    host: HashMap<&'static str, usize>,
    pub stats: LoadStats,
}

impl Linker {
    pub fn new(search: Vec<PathBuf>) -> Self {
        Linker {
            objects: Vec::new(),
            search,
            host: host::table(),
            stats: LoadStats::default(),
        }
    }

    fn find(&self, name: &str) -> Result<PathBuf, String> {
        for d in &self.search {
            let p = d.join(name);
            if p.exists() {
                return Ok(p);
            }
        }
        Err(format!("library {name} not found in {:?}", self.search))
    }

    /// Loads `path` and all its dependencies, links them and runs their
    /// initializers. Returns the index of the main object.
    pub fn load(&mut self, path: &Path) -> Result<usize, String> {
        let t0 = Instant::now();
        let main = self.objects.len();
        let mut queue = vec![(path.to_path_buf(), path.file_name().unwrap().to_string_lossy().into_owned())];
        let mut qi = 0;
        while qi < queue.len() {
            let (p, name) = queue[qi].clone();
            qi += 1;
            let obj = map_object(&p, &name)?;
            for n in &obj.needed {
                if host::is_host_library(n) {
                    continue;
                }
                if !queue.iter().any(|(_, q)| q == n) {
                    queue.push((self.find(n)?, n.clone()));
                }
            }
            self.objects.push(obj);
        }
        tls::layout(&mut self.objects)?;
        self.stats.map = t0.elapsed();

        let t1 = Instant::now();
        for i in main..self.objects.len() {
            self.relocate(i)?;
        }
        for o in &self.objects[main..] {
            protect(o)?;
        }
        self.stats.relocate = t1.elapsed();
        crate::debug::register_objects(&self.objects[main..]);
        Ok(main)
    }

    /// Runs initializers, dependencies first (reverse load order is a
    /// valid order for this breadth-first tree; bionic does a proper
    /// post-order walk).
    pub fn run_initializers(&mut self, argc: i32, argv: *const *const u8, envp: *const *const u8) {
        let t = Instant::now();
        // The guest libc is initialized before anything else.
        if let Some(init) = self.lookup_global("__andruna_libc_init") {
            // SAFETY: the guest libc's init function, System V convention.
            let f: extern "sysv64" fn(i32, *const *const u8, *const *const u8) = unsafe { core::mem::transmute(init) };
            f(argc, argv, envp);
        }
        for i in (0..self.objects.len()).rev() {
            let o = &self.objects[i];
            let mut fns = vec![];
            if let (Some(a), Some(n)) = (o.dyn1(DT_PREINIT_ARRAY), o.dyn1(DT_PREINIT_ARRAYSZ)) {
                fns.extend(read_array(o.bias + a, n));
            }
            if let Some(f) = o.dyn1(DT_INIT) {
                fns.push(o.bias + f);
            }
            if let (Some(a), Some(n)) = (o.dyn1(DT_INIT_ARRAY), o.dyn1(DT_INIT_ARRAYSZ)) {
                fns.extend(read_array(o.bias + a, n));
            }
            for f in fns {
                if f == 0 || f == u64::MAX {
                    continue;
                }
                // SAFETY: an initializer of the object; bionic calls them
                // with (argc, argv, envp).
                let f: extern "sysv64" fn(i32, *const *const u8, *const *const u8) = unsafe { core::mem::transmute(f) };
                f(argc, argv, envp);
            }
        }
        self.stats.init = t.elapsed();
    }

    /// The address of a global symbol defined by a loaded object.
    pub fn lookup_global(&self, name: &str) -> Option<u64> {
        for o in &self.objects {
            if let Some(s) = o.exports.get(name) {
                return Some(o.bias + s.st_value);
            }
        }
        None
    }

    fn resolve(&mut self, name: &str, weak: bool) -> Result<(u64, Option<(usize, Sym)>), String> {
        self.stats.symbol_lookups += 1;
        for (i, o) in self.objects.iter().enumerate() {
            if let Some(s) = o.exports.get(name) {
                let addr = if s.st_info & 0xf == STT_GNU_IFUNC {
                    // SAFETY: an IFUNC resolver of the object.
                    let r: extern "sysv64" fn() -> u64 = unsafe { core::mem::transmute(o.bias + s.st_value) };
                    r()
                } else {
                    o.bias + s.st_value
                };
                return Ok((addr, Some((i, *s))));
            }
        }
        if let Some(&a) = self.host.get(name) {
            self.stats.host_bindings += 1;
            return Ok((a as u64, None));
        }
        if weak {
            return Ok((0, None));
        }
        self.stats.unresolved.push(name.to_string());
        Ok((host::unresolved_stub(name) as u64, None))
    }

    fn relocate(&mut self, idx: usize) -> Result<(), String> {
        let mut relocs: Vec<Rela> = Vec::new();
        let o = &self.objects[idx];
        let bias = o.bias;
        if let (Some(a), Some(sz)) = (o.dyn1(DT_RELA), o.dyn1(DT_RELASZ)) {
            relocs.extend(read_relas(bias + a, sz));
        }
        if let (Some(a), Some(sz)) = (o.dyn1(DT_ANDROID_RELA), o.dyn1(DT_ANDROID_RELASZ)) {
            // SAFETY: the packed table lies in a mapped segment.
            let data = unsafe { core::slice::from_raw_parts((bias + a) as *const u8, sz as usize) };
            let p = decode_aps2(data, true)?;
            self.stats.packed_relocations += p.len();
            relocs.extend(p);
        }
        if o.dyn1(DT_REL).is_some() || o.dyn1(DT_ANDROID_REL).is_some() {
            return Err(format!("{}: REL relocations are not used on 64-bit Android", o.name));
        }
        let relr = o
            .dyn1(DT_RELR)
            .zip(o.dyn1(DT_RELRSZ))
            .or(o.dyn1(DT_ANDROID_RELR).zip(o.dyn1(DT_ANDROID_RELRSZ)));
        if let Some((a, sz)) = relr {
            // SAFETY: the RELR table lies in a mapped segment.
            let words = unsafe { core::slice::from_raw_parts((bias + a) as *const u64, (sz / 8) as usize) };
            let mut n = 0;
            decode_relr(words, |off| {
                let p = (bias + off) as *mut u64;
                // SAFETY: RELR entries point into the object's writable
                // data, mapped read-write until `protect`.
                unsafe { p.write_unaligned(p.read_unaligned().wrapping_add(bias)) };
                n += 1;
            });
            self.stats.relr_relocations += n;
            self.stats.relocations += n;
        }
        if let (Some(a), Some(sz)) = (o.dyn1(DT_JMPREL), o.dyn1(DT_PLTRELSZ)) {
            relocs.extend(read_relas(bias + a, sz));
        }
        let tls_module = o.tls_module;
        // Symbol cache per object: index -> resolved value.
        let mut cache: HashMap<u32, (u64, Option<(usize, Sym)>)> = HashMap::new();
        let mut irelative = vec![];
        for r in &relocs {
            let ty = r_type(r.r_info);
            let si = r_sym(r.r_info);
            let where_ = (bias + r.r_offset) as *mut u64;
            let (s_addr, def) = if si != 0 {
                if let Some(v) = cache.get(&si) {
                    *v
                } else {
                    let o = &self.objects[idx];
                    let sym = o.sym(si as usize);
                    let name = o.str_at(sym.st_name);
                    let local = sym.st_shndx != SHN_UNDEF && (sym.st_info >> 4) == STB_LOCAL;
                    let v = if local {
                        (bias + sym.st_value, Some((idx, sym)))
                    } else {
                        self.resolve(&name, (sym.st_info >> 4) == STB_WEAK)?
                    };
                    cache.insert(si, v);
                    v
                }
            } else {
                (0, None)
            };
            let a = r.r_addend as u64;
            let val: Option<u64> = match (crate::ARCH_ELF, ty) {
                (EM_X86_64, x86_64::R_NONE) | (EM_AARCH64, aarch64::R_NONE) => None,
                (EM_X86_64, x86_64::R_RELATIVE) | (EM_AARCH64, aarch64::R_RELATIVE) => Some(bias.wrapping_add(a)),
                (EM_X86_64, x86_64::R_64) | (EM_AARCH64, aarch64::R_ABS64) => Some(s_addr.wrapping_add(a)),
                (EM_X86_64, x86_64::R_GLOB_DAT | x86_64::R_JUMP_SLOT)
                | (EM_AARCH64, aarch64::R_GLOB_DAT | aarch64::R_JUMP_SLOT) => Some(s_addr.wrapping_add(a)),
                (EM_X86_64, x86_64::R_IRELATIVE) | (EM_AARCH64, aarch64::R_IRELATIVE) => {
                    irelative.push((where_, bias.wrapping_add(a)));
                    None
                }
                (EM_X86_64, x86_64::R_DTPMOD64) | (EM_AARCH64, aarch64::R_TLS_DTPMOD64) => {
                    let m = match def {
                        Some((i, _)) => self.objects[i].tls_module,
                        None if si == 0 => tls_module,
                        None => return Err("TLS symbol bound to the host".into()),
                    };
                    Some(m as u64)
                }
                (EM_X86_64, x86_64::R_DTPOFF64) | (EM_AARCH64, aarch64::R_TLS_DTPREL64) => {
                    let v = def.map(|(_, s)| s.st_value).unwrap_or(0);
                    Some(v.wrapping_add(a))
                }
                (EM_X86_64, x86_64::R_TPOFF64) | (EM_AARCH64, aarch64::R_TLS_TPREL64) => {
                    let (m, v) = match def {
                        Some((i, s)) => (self.objects[i].tls_module, s.st_value),
                        None => (tls_module, 0),
                    };
                    Some((tls::tp_offset(m) as u64).wrapping_add(v).wrapping_add(a))
                }
                _ => {
                    return Err(format!(
                        "{}: unsupported relocation type {ty} at {:#x}",
                        self.objects[idx].name, r.r_offset
                    ));
                }
            };
            if let Some(v) = val {
                // SAFETY: relocation targets lie in the object's mapped,
                // still writable segments.
                unsafe { where_.write_unaligned(v) };
            }
            self.stats.relocations += 1;
        }
        for (w, resolver) in irelative {
            // SAFETY: IRELATIVE resolvers are functions of the object.
            let f: extern "sysv64" fn() -> u64 = unsafe { core::mem::transmute(resolver) };
            // SAFETY: as above.
            unsafe { w.write_unaligned(f()) };
        }
        Ok(())
    }
}

fn read_array(addr: u64, size: u64) -> Vec<u64> {
    (0..size / 8)
        // SAFETY: the array lies in a mapped segment.
        .map(|i| unsafe { ((addr + i * 8) as *const u64).read_unaligned() })
        .collect()
}

fn read_relas(addr: u64, size: u64) -> Vec<Rela> {
    (0..size / 24)
        // SAFETY: the table lies in a mapped segment.
        .map(|i| unsafe { ((addr + i * 24) as *const Rela).read_unaligned() })
        .collect()
}

fn prot_of(flags: u32) -> u32 {
    let mut p = 0;
    if flags & PF_R != 0 {
        p |= PROT_READ;
    }
    if flags & PF_W != 0 {
        p |= PROT_WRITE;
    }
    if flags & PF_X != 0 {
        p |= PROT_EXEC;
    }
    p
}

fn lx_err(what: &str, e: andruna_lx::errno::Errno) -> String {
    format!("{what}: errno {}", e.0)
}

/// Reads and maps one object; nothing is relocated yet.
fn map_object(path: &Path, name: &str) -> Result<Object, String> {
    let file = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.len() < 64 || &file[..4] != b"\x7fELF" || file[4] != 2 || file[5] != 1 {
        return Err(format!("{}: not a 64-bit little-endian ELF file", path.display()));
    }
    // SAFETY: the file holds at least an ELF header; unaligned read.
    let eh: Ehdr = unsafe { (file.as_ptr() as *const Ehdr).read_unaligned() };
    if eh.e_type != ET_DYN {
        return Err(format!("{}: not ET_DYN (Android objects are PIC)", path.display()));
    }
    if eh.e_machine != crate::ARCH_ELF {
        return Err(format!("{}: e_machine {} is not this architecture", path.display(), eh.e_machine));
    }
    let phdrs: Vec<Phdr> = (0..eh.e_phnum as u64)
        .map(|i| {
            let off = (eh.e_phoff + i * eh.e_phentsize as u64) as usize;
            assert!(off + 56 <= file.len(), "program header outside the file");
            // SAFETY: bounds checked above.
            unsafe { (file.as_ptr().add(off) as *const Phdr).read_unaligned() }
        })
        .collect();
    let loads: Vec<&Phdr> = phdrs.iter().filter(|p| p.p_type == PT_LOAD).collect();
    if loads.is_empty() {
        return Err(format!("{}: no PT_LOAD", path.display()));
    }
    let lo = page_down(loads.iter().map(|p| p.p_vaddr).min().unwrap());
    let hi = page_up(loads.iter().map(|p| p.p_vaddr + p.p_memsz).max().unwrap());
    let span = hi - lo;

    // Reserve the whole span, then map each segment inside it, as bionic's
    // linker does with a PROT_NONE reservation.
    let base = andruna_lx::mem::mmap(0, span as usize, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0)
        .map_err(|e| lx_err("reserve", e))? as u64;
    let bias = base - lo;
    for p in &loads {
        let start = page_down(bias + p.p_vaddr);
        let end = page_up(bias + p.p_vaddr + p.p_memsz);
        andruna_lx::mem::mmap(
            start as usize,
            (end - start) as usize,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            -1,
            0,
        )
        .map_err(|e| lx_err("map segment", e))?;
        let fo = p.p_offset as usize;
        let fs = p.p_filesz as usize;
        if fo + fs > file.len() {
            return Err(format!("{}: segment outside the file", path.display()));
        }
        // SAFETY: the destination was just mapped read-write and is
        // `p_memsz >= p_filesz` bytes long; the source is in the file.
        unsafe {
            core::ptr::copy_nonoverlapping(file.as_ptr().add(fo), (bias + p.p_vaddr) as *mut u8, fs);
        }
    }

    // The dynamic section.
    let mut dynamic: HashMap<i64, Vec<u64>> = HashMap::new();
    if let Some(d) = phdrs.iter().find(|p| p.p_type == PT_DYNAMIC) {
        let mut a = bias + d.p_vaddr;
        loop {
            // SAFETY: PT_DYNAMIC lies in a mapped segment and ends with
            // DT_NULL.
            let e: Dyn = unsafe { (a as *const Dyn).read_unaligned() };
            if e.d_tag == DT_NULL {
                break;
            }
            dynamic.entry(e.d_tag).or_default().push(e.d_val);
            a += 16;
        }
    }
    let get = |t: i64| dynamic.get(&t).and_then(|v| v.first().copied());
    let strtab = bias + get(DT_STRTAB).ok_or("no DT_STRTAB")?;
    let symtab = bias + get(DT_SYMTAB).ok_or("no DT_SYMTAB")?;
    let nsyms = symbol_count(bias, &dynamic)?;
    let mut obj = Object {
        name: name.to_string(),
        path: path.to_path_buf(),
        bias,
        base,
        size: span,
        phdrs: phdrs.clone(),
        needed: vec![],
        dynamic: dynamic.clone(),
        symtab,
        strtab,
        nsyms,
        exports: HashMap::new(),
        tls_module: 0,
        local_symbols: vec![],
        file: vec![],
    };
    obj.needed = dynamic
        .get(&DT_NEEDED)
        .map(|v| v.iter().map(|&o| obj.str_at(o as u32)).collect())
        .unwrap_or_default();
    for i in 1..nsyms {
        let s = obj.sym(i);
        let bind = s.st_info >> 4;
        if s.st_shndx == SHN_UNDEF || bind == STB_LOCAL || (s.st_other & 3) != STV_DEFAULT {
            continue;
        }
        let n = obj.str_at(s.st_name);
        obj.exports.entry(n).or_insert(s);
    }
    obj.local_symbols = read_symtab(&file);
    obj.file = file;
    Ok(obj)
}

/// Number of dynamic symbols, from DT_HASH or DT_GNU_HASH.
fn symbol_count(bias: u64, d: &HashMap<i64, Vec<u64>>) -> Result<usize, String> {
    let get = |t: i64| d.get(&t).and_then(|v| v.first().copied());
    if let Some(h) = get(DT_HASH) {
        // SAFETY: the hash table header (nbucket, nchain) is mapped.
        return Ok(unsafe { ((bias + h + 4) as *const u32).read_unaligned() } as usize);
    }
    let h = bias + get(DT_GNU_HASH).ok_or("no DT_HASH or DT_GNU_HASH")?;
    // SAFETY: the GNU hash table lies in a mapped segment; its layout is
    // nbuckets, symoffset, bloom_size, bloom_shift, bloom[], buckets[],
    // chains[].
    unsafe {
        let r = |o: u64| ((h + o) as *const u32).read_unaligned();
        let (nb, symoff, bloom) = (r(0) as u64, r(4) as u64, r(8) as u64);
        let buckets = 16 + bloom * 8;
        let mut max = 0u64;
        for b in 0..nb {
            max = max.max(r(buckets + b * 4) as u64);
        }
        if max < symoff {
            return Ok(symoff as usize);
        }
        let chains = buckets + nb * 4;
        let mut i = max;
        while r(chains + (i - symoff) * 4) & 1 == 0 {
            i += 1;
        }
        Ok(i as usize + 1)
    }
}

/// Function symbols of `.symtab` (not mapped at run time; read from the
/// file), for symbolization.
fn read_symtab(file: &[u8]) -> Vec<(u64, u64, String)> {
    // SAFETY: validated by `map_object`.
    let eh: Ehdr = unsafe { (file.as_ptr() as *const Ehdr).read_unaligned() };
    let sh = |i: u64| -> Option<Shdr> {
        let off = (eh.e_shoff + i * eh.e_shentsize as u64) as usize;
        if eh.e_shoff == 0 || off + 64 > file.len() {
            return None;
        }
        // SAFETY: bounds checked.
        Some(unsafe { (file.as_ptr().add(off) as *const Shdr).read_unaligned() })
    };
    let mut out = vec![];
    for i in 0..eh.e_shnum as u64 {
        let Some(s) = sh(i) else { break };
        if s.sh_type != SHT_SYMTAB {
            continue;
        }
        let Some(strs) = sh(s.sh_link as u64) else { continue };
        let n = s.sh_size / 24;
        for k in 0..n {
            let off = (s.sh_offset + k * 24) as usize;
            if off + 24 > file.len() {
                break;
            }
            // SAFETY: bounds checked.
            let sym: Sym = unsafe { (file.as_ptr().add(off) as *const Sym).read_unaligned() };
            if sym.st_info & 0xf != STT_FUNC || sym.st_value == 0 {
                continue;
            }
            let so = (strs.sh_offset + sym.st_name as u64) as usize;
            if so >= file.len() {
                continue;
            }
            let end = file[so..].iter().position(|&b| b == 0).unwrap_or(0);
            let name = String::from_utf8_lossy(&file[so..so + end]).into_owned();
            out.push((sym.st_value, sym.st_size, name));
        }
    }
    out.sort();
    out
}

/// Applies the final protections: each segment's flags, then read-only for
/// `PT_GNU_RELRO`.
fn protect(o: &Object) -> Result<(), String> {
    for p in o.phdrs.iter().filter(|p| p.p_type == PT_LOAD) {
        let start = page_down(o.bias + p.p_vaddr);
        let end = page_up(o.bias + p.p_vaddr + p.p_memsz);
        andruna_lx::mem::mprotect(start as usize, (end - start) as usize, prot_of(p.p_flags))
            .map_err(|e| lx_err("mprotect", e))?;
    }
    for p in o.phdrs.iter().filter(|p| p.p_type == PT_GNU_RELRO) {
        let start = page_down(o.bias + p.p_vaddr);
        let end = page_down(o.bias + p.p_vaddr + p.p_memsz);
        if end > start {
            andruna_lx::mem::mprotect(start as usize, (end - start) as usize, PROT_READ)
                .map_err(|e| lx_err("relro", e))?;
        }
    }
    Ok(())
}

/// Finds the object containing `addr`.
pub fn object_at(objs: &[Object], addr: u64) -> Option<&Object> {
    objs.iter().find(|o| addr >= o.base && addr < o.base + o.size)
}

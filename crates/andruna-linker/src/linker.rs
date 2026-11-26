//! The dynamic linker: loading an object and its dependencies, symbol
//! binding, relocation, TLS registration, initializers and finalizers,
//! with bionic's linker semantics.
//!
//! - Dependencies are loaded breadth first (`DT_NEEDED` order), each name
//!   once: a name matches an object loaded under that name, its soname or
//!   its path. A name without `/` is searched in `LD_LIBRARY_PATH`, then
//!   the requesting object's `DT_RUNPATH` (`$ORIGIN` expanded), then the
//!   default library path.
//! - Symbols of a relocation in object X are looked up in X itself when it
//!   is `DT_SYMBOLIC`, then the global group (the executable's tree and
//!   `RTLD_GLOBAL` libraries, in load order), then X's local group (the
//!   tree of the `dlopen` that loaded it, breadth first), then the host
//!   table. Definitions are matched by name and symbol version.
//! - Objects are relocated dependencies first; `IRELATIVE` runs after the
//!   rest of the object's relocations. Binding is eager (`-z now`), as on
//!   Android. Text relocations and copy relocations are rejected.
//! - Initializers run dependencies first (post-order over `DT_NEEDED`),
//!   the executable's `DT_PREINIT_ARRAY` before everything; finalizers run
//!   on unload in reverse.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::bridge::Bridge;
use crate::elf::*;
use crate::error::{Error, Result};
use crate::image::{HostFile, Image, PAGE, SegmentSource};
use crate::object::*;
use crate::reloc::{decode_aps2, decode_relr};
use crate::tls::{self, BuiltinLayout};

/// A guest function called with `(argc, argv, envp)`: initializers.
pub type InitFn = gty!(fn(i32, *const *const u8, *const *const u8));
/// A guest function without arguments: finalizers, x86_64 IFUNC resolvers.
pub type VoidFn = gty!(fn());
/// An IFUNC resolver: x86_64 resolvers take nothing; arm64 resolvers
/// receive `(AT_HWCAP | _IFUNC_ARG_HWCAP, &__ifunc_arg_t)`.
pub type ResolverFn = gty!(fn(u64, *const u64) -> u64);

/// `_IFUNC_ARG_HWCAP` (arm64).
const IFUNC_ARG_HWCAP: u64 = 1 << 62;

#[derive(Clone, Debug)]
pub struct Config {
    /// The default library path (Windows directories).
    pub search: Vec<PathBuf>,
    /// `LD_LIBRARY_PATH`, searched first.
    pub ld_library_path: Vec<PathBuf>,
    /// ELF machine of the objects. Another machine than the process's is
    /// "link-only": objects are mapped and relocated, never run.
    pub machine: u16,
    /// Translate `.eh_frame` into Windows unwind tables (x64).
    pub unwind_tables: bool,
    /// Read `.symtab` for crash reports.
    pub symtab: bool,
    /// Bind undefined non-weak symbols to a stub that reports the call
    /// and fails with `ENOSYS` instead of failing the load (diagnostics
    /// only; off by default, as bionic fails the load).
    pub unresolved_stubs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            search: vec![],
            ld_library_path: vec![],
            machine: EM_NATIVE,
            unwind_tables: cfg!(target_arch = "x86_64"),
            symtab: true,
            unresolved_stubs: false,
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub objects: usize,
    pub map: Duration,
    pub relocate: Duration,
    pub init: Duration,
    pub relocations: usize,
    pub relr: usize,
    pub packed: usize,
    pub tlsdesc: usize,
    pub lookups: usize,
    pub host_bindings: usize,
    pub unresolved: Vec<String>,
    pub bytes_read: u64,
}

/// `RTLD_*` flags (bionic values).
pub mod rtld {
    pub const LAZY: i32 = 0x1;
    pub const NOW: i32 = 0x2;
    pub const NOLOAD: i32 = 0x4;
    pub const LOCAL: i32 = 0;
    pub const GLOBAL: i32 = 0x100;
    pub const NODELETE: i32 = 0x1000;
}

/// A symbol found for a relocation.
#[derive(Clone, Copy, Debug)]
struct Def {
    addr: u64,
    obj: Option<ObjectId>,
    sym: Sym,
}

pub struct Linker {
    pub config: Config,
    objects: Vec<Option<Object>>,
    /// The global group, in order.
    global: Vec<ObjectId>,
    host: HashMap<Vec<u8>, u64>,
    builtin_tls: BuiltinLayout,
    pub bridge: Option<Bridge>,
    /// Static TLS is final once the start-up set is loaded.
    static_tls_done: bool,
    /// TLSDESC arguments for dynamic TLS (`TlsDynamicResolverArg`), owned
    /// per object.
    tlsdesc_args: HashMap<ObjectId, Vec<Box<[u64; 3]>>>,
    pub stats: Stats,
    pub stack_guard: u64,
    /// The `AT_HWCAP` value for arm64 IFUNC resolvers.
    pub hwcap: u64,
    pub hwcap2: u64,
    /// The initial process stack block (argc, argv, envp, auxv) when the
    /// linker starts a program; the bridge reads it.
    pub startup: Option<crate::process::Startup>,
}

/// What `load` produced: the root and the objects to initialize, in order.
pub struct Loaded {
    pub root: ObjectId,
    pub init_order: Vec<ObjectId>,
}

impl Linker {
    pub fn new(config: Config) -> Self {
        let mut guard = [0u8; 8];
        tls::random_bytes(&mut guard);
        Linker {
            config,
            objects: Vec::new(),
            global: Vec::new(),
            host: crate::host::table(),
            builtin_tls: BuiltinLayout::new(),
            bridge: None,
            static_tls_done: false,
            tlsdesc_args: HashMap::new(),
            stats: Stats::default(),
            // The low byte stays 0 so string overflows stop at it.
            stack_guard: u64::from_ne_bytes(guard) & !0xff,
            hwcap: 0,
            hwcap2: 0,
            startup: None,
        }
    }

    /// Runs guest code (the objects are of this process's architecture).
    pub fn runs_code(&self) -> bool {
        self.config.machine == EM_NATIVE
    }

    pub fn get(&self, id: ObjectId) -> Option<&Object> {
        self.objects.get(id).and_then(|o| o.as_ref())
    }

    fn obj(&self, id: ObjectId) -> &Object {
        self.get(id).expect("live object id")
    }

    fn obj_mut(&mut self, id: ObjectId) -> &mut Object {
        self.objects[id].as_mut().expect("live object id")
    }

    /// Live objects in load order.
    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.objects.iter().flatten()
    }

    pub fn builtin_tls(&self) -> &BuiltinLayout {
        &self.builtin_tls
    }

    /// The object whose image contains `addr`.
    pub fn object_at(&self, addr: u64) -> Option<&Object> {
        self.objects().find(|o| o.image.contains(addr))
    }

    pub fn find_by_name(&self, name: &str) -> Option<ObjectId> {
        self.objects()
            .find(|o| o.name == name || o.soname.as_deref() == Some(name) || o.path == name)
            .map(|o| o.id)
    }

    /// Adds or replaces a host symbol.
    pub fn define_host(&mut self, name: &str, addr: u64) {
        self.host.insert(name.as_bytes().to_vec(), addr);
    }

    // ---------------------------------------------------------------
    // Loading
    // ---------------------------------------------------------------

    /// Finds the file for `name`, requested by `by`.
    fn locate(&self, name: &str, by: Option<ObjectId>) -> Result<Origin> {
        if name.contains('/') || Path::new(name).is_absolute() {
            let p = Path::new(name);
            if p.is_file() {
                return Ok(Origin::Host(p.to_path_buf()));
            }
            if name.starts_with('/') && crate::guestfs::exists(name) {
                return Ok(Origin::Guest(name.to_string()));
            }
            return Err(Error::not_found(name));
        }
        let runpath: Vec<PathBuf> = by
            .map(|b| self.obj(b).runpath.iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        for d in self
            .config
            .ld_library_path
            .iter()
            .chain(runpath.iter())
            .chain(self.config.search.iter())
        {
            let p = d.join(name);
            if p.is_file() {
                return Ok(Origin::Host(p));
            }
        }
        Err(Error::not_found(name))
    }

    /// Maps one object; nothing is relocated yet.
    fn map_object(&mut self, name: &str, origin: Origin) -> Result<ObjectId> {
        let (path, mut file): (String, Box<dyn SegmentSource>) = match &origin {
            Origin::Host(p) => {
                let f = std::fs::File::open(p)
                    .map_err(|e| Error::new(format!("cannot open \"{}\": {e}", p.display())))?;
                (p.display().to_string(), Box::new(HostFile { file: f }))
            }
            Origin::Guest(p) => (p.clone(), Box::new(crate::guestfs::GuestFile::open(p)?)),
            Origin::Memory => return Err(Error::new("map_object: memory objects use map_bytes")),
        };
        let head = read_head(&mut *file, name)?;
        let o = self.map_parsed(name, &path, origin, &head, &mut *file)?;
        if self.config.symtab
            && let Origin::Host(p) = &self.obj(o).origin
        {
            let syms = crate::debug::read_symtab(p);
            self.obj_mut(o).local_symbols = syms;
        }
        Ok(o)
    }

    /// Maps an object held in memory (tests).
    pub fn map_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<ObjectId> {
        let mut src = crate::image::Bytes(bytes);
        self.map_parsed(name, name, Origin::Memory, bytes, &mut src)
    }

    fn map_parsed(
        &mut self,
        name: &str,
        path: &str,
        origin: Origin,
        head: &[u8],
        src: &mut dyn SegmentSource,
    ) -> Result<ObjectId> {
        let eh = parse_ehdr(head, self.config.machine, name)?;
        let h = parse_phdrs(head, &eh, PAGE, name)?;
        let image = Image::map(&h, src, name)?;
        self.stats.bytes_read += h.load_file_end;
        let id = self.objects.len();
        let r = self.finish_object(id, name, path, origin, h, image);
        match r {
            Ok(o) => {
                self.objects.push(Some(o));
                self.stats.objects += 1;
                Ok(id)
            }
            Err((e, mut image)) => {
                image.unmap();
                Err(e)
            }
        }
    }

    fn finish_object(
        &self,
        id: ObjectId,
        name: &str,
        path: &str,
        origin: Origin,
        headers: Headers,
        image: Image,
    ) -> core::result::Result<Object, (Error, Image)> {
        let dyn_ph = *headers.find(PT_DYNAMIC).expect("validated");
        let mut o = Object {
            id,
            name: name.to_string(),
            soname: None,
            path: path.to_string(),
            path_c: CString::new(path.replace('\0', "")).unwrap_or_default(),
            origin,
            machine: headers.ehdr.e_machine,
            phdr_addr: 0,
            dynamic: DynamicInfo::default(),
            dynamic_addr: image.bias.wrapping_add(dyn_ph.p_vaddr),
            image,
            headers,
            needed: vec![],
            runpath: vec![],
            deps: vec![],
            nsyms: 0,
            tls: None,
            is_main: false,
            global: false,
            nodelete: false,
            refcount: 0,
            linked: false,
            initialized: false,
            group_root: id,
            local_symbols: vec![],
        };
        if let Err(e) = Self::parse_dynamic(&mut o, dyn_ph) {
            return Err((e, o.image));
        }
        Ok(o)
    }

    fn parse_dynamic(o: &mut Object, dyn_ph: Phdr) -> Result<()> {
        let n = dyn_ph.p_memsz / 16;
        let mut entries = Vec::with_capacity(n as usize);
        for i in 0..n {
            let e: Dyn = o.read(dyn_ph.p_vaddr + i * 16)?;
            entries.push(e);
            if e.d_tag == DT_NULL {
                break;
            }
        }
        o.dynamic = DynamicInfo::from_entries(&entries, &o.name)?;
        if o.dynamic.textrel {
            return Err(Error::invalid(
                &o.name,
                "has text relocations (not supported, as on Android since API 23)",
            ));
        }
        o.check(o.dynamic.strtab, o.dynamic.strsz)?;
        o.nsyms = symbol_count(o)?;
        o.check(o.dynamic.symtab, o.nsyms as u64 * 24)?;
        o.soname = o
            .dynamic
            .soname
            .map(|s| o.string_lossy(s))
            .filter(|s| !s.is_empty());
        o.needed = o
            .dynamic
            .needed
            .clone()
            .into_iter()
            .map(|s| o.string(s).map(|b| String::from_utf8_lossy(b).into_owned()))
            .collect::<Result<_>>()?;
        let origin_dir = match &o.origin {
            Origin::Host(p) => p
                .parent()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            Origin::Guest(p) => p.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default(),
            Origin::Memory => String::new(),
        };
        if let Some(rp) = o.dynamic.runpath.or(o.dynamic.rpath) {
            o.runpath = o
                .string_lossy(rp)
                .split(':')
                .filter(|s| !s.is_empty())
                .map(|s| s.replace("${ORIGIN}", &origin_dir).replace("$ORIGIN", &origin_dir))
                .collect();
        }
        if o.dynamic.flags_1 & DF_1_NODELETE != 0 {
            o.nodelete = true;
        }
        // The program headers in memory: PT_PHDR, else inside the first
        // segment that covers e_phoff.
        let ph_len = u64::from(o.headers.ehdr.e_phnum) * 56;
        o.phdr_addr = if let Some(p) = o.headers.find(PT_PHDR) {
            o.check(p.p_vaddr, ph_len)?
        } else {
            let off = o.headers.ehdr.e_phoff;
            let seg = o
                .headers
                .loads()
                .find(|p| off >= p.p_offset && off + ph_len <= p.p_offset + p.p_filesz)
                .copied()
                .ok_or_else(|| Error::invalid(&o.name, "program headers are not loaded"))?;
            o.check(seg.p_vaddr + (off - seg.p_offset), ph_len)?
        };
        if let Some(t) = o.headers.find(PT_TLS) {
            o.check(t.p_vaddr, t.p_filesz)?;
            o.tls = Some(TlsInfo {
                image: o.addr(t.p_vaddr),
                filesz: t.p_filesz,
                memsz: t.p_memsz,
                align: t.p_align.max(1),
                ..TlsInfo::default()
            });
        }
        Ok(())
    }

    /// Loads `name` (a path, or a library name to search) and its
    /// dependencies, registers TLS, relocates and protects them. Returns the
    /// root and the initializer order; the caller runs [`Self::init_fns`]
    /// without holding the linker lock.
    pub fn load(&mut self, name: &str, by: Option<ObjectId>, flags: i32) -> Result<Loaded> {
        let t0 = Instant::now();
        if let Some(id) = self.find_by_name(name) {
            self.obj_mut(id).refcount += 1;
            if flags & rtld::GLOBAL != 0 {
                self.make_global(id);
            }
            if flags & rtld::NODELETE != 0 {
                self.obj_mut(id).nodelete = true;
            }
            return Ok(Loaded {
                root: id,
                init_order: vec![],
            });
        }
        if flags & rtld::NOLOAD != 0 {
            return Err(Error::not_found(name));
        }
        let first_new = self.objects.len();
        let is_main = self.objects().next().is_none();
        let r = self.load_tree(name, by, is_main, flags);
        let new: Vec<ObjectId> = (first_new..self.objects.len())
            .filter(|&i| self.get(i).is_some())
            .collect();
        match r.and_then(|root| {
            self.stats.map += t0.elapsed();
            self.link_new(root, &new)?;
            Ok(root)
        }) {
            Ok(root) => {
                let init_order = self.init_order(root);
                Ok(Loaded { root, init_order })
            }
            Err(e) => {
                // Undo: nothing of a failed load stays mapped.
                for id in new.into_iter().rev() {
                    self.drop_object(id);
                }
                Err(e)
            }
        }
    }

    fn make_global(&mut self, root: ObjectId) {
        for id in self.local_group(root) {
            if !self.obj(id).global {
                self.obj_mut(id).global = true;
                self.global.push(id);
            }
        }
    }

    /// Breadth-first mapping of `name` and its dependencies.
    fn load_tree(
        &mut self,
        name: &str,
        by: Option<ObjectId>,
        is_main: bool,
        flags: i32,
    ) -> Result<ObjectId> {
        let origin = self.locate(name, by)?;
        let short = Path::new(name)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());
        let root = self.map_object(&short, origin)?;
        {
            let o = self.obj_mut(root);
            o.is_main = is_main;
            o.refcount = 1;
            o.nodelete |= flags & rtld::NODELETE != 0;
        }
        let mut queue = VecDeque::from([root]);
        while let Some(id) = queue.pop_front() {
            let needed = self.obj(id).needed.clone();
            let mut deps = Vec::with_capacity(needed.len());
            for n in needed.iter().filter(|n| *n != crate::host::HOST_LIBRARY) {
                let dep = match self.find_by_name(n) {
                    Some(d) => d,
                    None => {
                        let origin = self.locate(n, Some(id)).map_err(|e| {
                            Error::new(format!("{e} needed by \"{}\"", self.obj(id).name))
                        })?;
                        let d = self.map_object(n, origin)?;
                        self.obj_mut(d).group_root = root;
                        queue.push_back(d);
                        d
                    }
                };
                self.obj_mut(dep).refcount += 1;
                deps.push(dep);
            }
            self.obj_mut(id).deps = deps;
        }
        self.obj_mut(root).group_root = root;
        if is_main || flags & rtld::GLOBAL != 0 {
            self.make_global(root);
        }
        Ok(root)
    }

    /// TLS, relocation, protection and registration of newly mapped objects.
    fn link_new(&mut self, root: ObjectId, new: &[ObjectId]) -> Result<()> {
        let t = Instant::now();
        // The bridge (bionic's loader-side state) links first: it is
        // self-contained and places TLS for everything else.
        if let Some(s) = &self.startup
            && self.obj(root).is_main
        {
            s.set_executable(self.obj(root));
        }
        if self.bridge.is_none() && self.runs_code() {
            if let Some(b) = new
                .iter()
                .copied()
                .find(|&i| crate::bridge::is_bridge(self.obj(i)))
            {
                self.relocate(b)?;
                self.protect_and_register(b)?;
                self.bridge = Some(Bridge::new(self, b)?);
            }
        }
        for &id in new {
            self.register_tls(id)?;
        }
        if !self.static_tls_done && self.obj(root).is_main {
            self.finish_static_tls()?;
        }
        // Dependencies first.
        for &id in new.iter().rev() {
            if !self.obj(id).linked {
                self.relocate(id)?;
                self.protect_and_register(id)?;
            }
        }
        self.stats.relocate += t.elapsed();
        Ok(())
    }

    fn protect_and_register(&mut self, id: ObjectId) -> Result<()> {
        let o = self.obj(id);
        o.image.protect(&o.headers)?;
        if self.config.unwind_tables && self.runs_code() {
            let _ = crate::unwind::register(o);
        }
        crate::debug::register(o);
        self.obj_mut(id).linked = true;
        Ok(())
    }

    fn register_tls(&mut self, id: ObjectId) -> Result<()> {
        let Some(t) = self.obj(id).tls else {
            if self.obj(id).is_main
                && let Some(b) = &self.bridge
            {
                b.tls_exe(None, &self.obj(id).name)?;
            }
            return Ok(());
        };
        if t.module != 0 {
            return Ok(());
        }
        let is_main = self.obj(id).is_main;
        let (module, tp_offset) = if let Some(b) = &self.bridge {
            if is_main {
                b.tls_exe(Some(&t), &self.obj(id).name)?
            } else {
                b.tls_register(&t, id)?
            }
        } else {
            let m = self
                .builtin_tls
                .reserve(t.image, t.filesz, t.memsz, t.align)?;
            (m.id, m.tp_offset)
        };
        let o = self.obj_mut(id);
        let tls = o.tls.as_mut().expect("has TLS");
        tls.module = module;
        tls.tp_offset = tp_offset;
        Ok(())
    }

    fn finish_static_tls(&mut self) -> Result<()> {
        self.static_tls_done = true;
        self.builtin_tls.freeze();
        if let Some(b) = &self.bridge {
            b.tls_finalize()?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // Symbols
    // ---------------------------------------------------------------

    /// Breadth-first tree of `root` over `DT_NEEDED`.
    pub fn local_group(&self, root: ObjectId) -> Vec<ObjectId> {
        let mut seen = HashSet::from([root]);
        let mut out = vec![root];
        let mut i = 0;
        while i < out.len() {
            for &d in &self.obj(out[i]).deps {
                if seen.insert(d) {
                    out.push(d);
                }
            }
            i += 1;
        }
        out
    }

    /// The lookup scope of relocations in `id`.
    fn scope(&self, id: ObjectId) -> Vec<ObjectId> {
        let o = self.obj(id);
        let mut v = Vec::new();
        if o.dynamic.symbolic {
            v.push(id);
        }
        v.extend(self.global.iter().copied());
        for g in self.local_group(o.group_root) {
            if !v.contains(&g) {
                v.push(g);
            }
        }
        v
    }

    fn find_in(&self, scope: &[ObjectId], name: &[u8], req: Option<&VersionReq>) -> Option<Def> {
        for &i in scope {
            let o = self.obj(i);
            if let Some((_, sym)) = o.lookup(name, req) {
                return Some(Def {
                    addr: o.addr(sym.st_value),
                    obj: Some(i),
                    sym,
                });
            }
        }
        None
    }

    /// The runtime value of a definition (IFUNCs resolved).
    fn value(&self, d: &Def) -> u64 {
        if d.sym.kind() == STT_GNU_IFUNC && d.obj.is_some() {
            if !self.runs_code() {
                return d.addr;
            }
            return self.call_resolver(d.addr);
        }
        if d.sym.kind() == STT_TLS {
            return d.sym.st_value;
        }
        d.addr
    }

    fn call_resolver(&self, f: u64) -> u64 {
        // SAFETY: an IFUNC resolver of a loaded, relocated object, called
        // with the convention of its architecture.
        let r: ResolverFn = unsafe { core::mem::transmute(f) };
        let arg = [24u64, self.hwcap, self.hwcap2];
        r(self.hwcap | IFUNC_ARG_HWCAP, arg.as_ptr())
    }

    /// `dlsym` lookup in a scope; host symbols are not visible to dlsym
    /// except through `RTLD_DEFAULT`.
    pub fn dlsym_in(
        &self,
        scope: &[ObjectId],
        name: &str,
        version: Option<&str>,
        with_host: bool,
    ) -> Option<u64> {
        let req = version.map(|v| VersionReq {
            name: v.as_bytes().to_vec(),
            hash: elf_hash(v.as_bytes()),
        });
        if let Some(d) = self.find_in(scope, name.as_bytes(), req.as_ref()) {
            if d.sym.kind() == STT_TLS {
                return self.tls_address(&d);
            }
            return Some(self.value(&d));
        }
        if with_host {
            return self.host.get(name.as_bytes()).copied();
        }
        None
    }

    fn tls_address(&self, d: &Def) -> Option<u64> {
        let t = self.obj(d.obj?).tls?;
        let off = t.tp_offset?;
        Some(
            tls::current()
                .wrapping_add_signed(off)
                .wrapping_add(d.sym.st_value),
        )
    }

    pub fn global_scope(&self) -> Vec<ObjectId> {
        self.global.clone()
    }

    pub fn host_symbol(&self, name: &str) -> Option<u64> {
        self.host.get(name.as_bytes()).copied()
    }

    // ---------------------------------------------------------------
    // Relocation
    // ---------------------------------------------------------------

    fn relocate(&mut self, id: ObjectId) -> Result<()> {
        let mut relas: Vec<Rela> = Vec::new();
        let o = self.obj(id);
        let d = o.dynamic.clone();
        if let Some((a, sz)) = d.android_rela {
            let p = decode_aps2(o.bytes(a, sz)?, true).map_err(|e| Error::invalid(&o.name, e.message()))?;
            self.stats.packed += p.len();
            relas.extend(p);
        }
        if let Some((a, sz)) = d.android_rel {
            let p = decode_aps2(o.bytes(a, sz)?, false).map_err(|e| Error::invalid(&o.name, e.message()))?;
            self.stats.packed += p.len();
            relas.extend(p.into_iter().map(|r| self.rel_addend(id, r)).collect::<Result<Vec<_>>>()?);
        }
        if let Some((a, sz)) = d.rela {
            relas.extend(read_table::<Rela>(o, a, sz)?);
        }
        if let Some((a, sz)) = d.rel {
            for r in read_table::<Rel>(o, a, sz)? {
                let r = Rela {
                    r_offset: r.r_offset,
                    r_info: r.r_info,
                    r_addend: 0,
                };
                relas.push(self.rel_addend(id, r)?);
            }
        }
        if let Some((a, sz)) = d.relr {
            let words: Vec<u64> = read_table::<u64>(o, a, sz)?;
            let bias = o.image.bias;
            let mut targets = Vec::new();
            decode_relr(&words, |off| targets.push(off));
            for off in targets {
                let p = o.check(off, 8)?;
                // SAFETY: RELR targets are words in the object's writable
                // data, mapped read-write until protect (checked above).
                unsafe { (p as *mut u64).write_unaligned((p as *const u64).read_unaligned().wrapping_add(bias)) };
                self.stats.relr += 1;
                self.stats.relocations += 1;
            }
        }
        if let Some((a, sz)) = d.jmprel {
            if d.pltrel_is_rela {
                relas.extend(read_table::<Rela>(o, a, sz)?);
            } else {
                for r in read_table::<Rel>(o, a, sz)? {
                    let r = Rela {
                        r_offset: r.r_offset,
                        r_info: r.r_info,
                        r_addend: 0,
                    };
                    relas.push(self.rel_addend(id, r)?);
                }
            }
        }
        let scope = self.scope(id);
        let mut cache: HashMap<u32, Option<Def>> = HashMap::new();
        let mut irelative = Vec::new();
        for r in &relas {
            self.apply(id, r, &scope, &mut cache, &mut irelative)?;
        }
        if self.runs_code() {
            for (w, f) in irelative {
                let v = self.call_resolver(f);
                // SAFETY: an IRELATIVE target in the object's writable data.
                unsafe { (w as *mut u64).write_unaligned(v) };
            }
        }
        Ok(())
    }

    /// For REL entries, the addend is the word at the target.
    fn rel_addend(&self, id: ObjectId, mut r: Rela) -> Result<Rela> {
        r.r_addend = self.obj(id).read::<i64>(r.r_offset)?;
        Ok(r)
    }

    fn resolve(
        &mut self,
        id: ObjectId,
        si: u32,
        scope: &[ObjectId],
        cache: &mut HashMap<u32, Option<Def>>,
    ) -> Result<Option<Def>> {
        if let Some(d) = cache.get(&si) {
            return Ok(*d);
        }
        let o = self.obj(id);
        let sym = o.sym(si as usize)?;
        let def = if sym.is_defined() && sym.bind() == STB_LOCAL {
            Some(Def {
                addr: o.addr(sym.st_value),
                obj: Some(id),
                sym,
            })
        } else {
            let name = o.string(u64::from(sym.st_name))?.to_vec();
            let req = o.version_req(si as usize)?;
            self.stats.lookups += 1;
            match self.find_in(scope, &name, req.as_ref()) {
                Some(d) => Some(d),
                None => {
                    if let Some(&a) = self.host.get(&name) {
                        self.stats.host_bindings += 1;
                        Some(Def {
                            addr: a,
                            obj: None,
                            sym,
                        })
                    } else if sym.bind() == STB_WEAK {
                        None
                    } else {
                        let n = String::from_utf8_lossy(&name).into_owned();
                        if !self.config.unresolved_stubs {
                            return Err(Error::symbol(&n, &self.obj(id).name));
                        }
                        let stub = crate::host::unresolved_stub(&n)?;
                        self.stats.unresolved.push(n);
                        Some(Def {
                            addr: stub,
                            obj: None,
                            sym,
                        })
                    }
                }
            }
        };
        cache.insert(si, def);
        Ok(def)
    }

    fn apply(
        &mut self,
        id: ObjectId,
        r: &Rela,
        scope: &[ObjectId],
        cache: &mut HashMap<u32, Option<Def>>,
        irelative: &mut Vec<(u64, u64)>,
    ) -> Result<()> {
        let machine = self.obj(id).machine;
        let kind = RelKind::of(machine, r_type(r.r_info));
        let si = r_sym(r.r_info);
        let at = self.obj(id).check(r.r_offset, 8)?;
        let bias = self.obj(id).image.bias;
        let a = r.r_addend as u64;
        let def = if si != 0 && kind.needs_symbol() {
            self.resolve(id, si, scope, cache)?
        } else {
            None
        };
        let s = def.map(|d| self.value(&d)).unwrap_or(0);
        let write = |v: u64| {
            // SAFETY: the target lies in the object's image (checked) and is
            // writable until protect.
            unsafe { (at as *mut u64).write_unaligned(v) }
        };
        match kind {
            RelKind::None => {}
            RelKind::Relative => write(bias.wrapping_add(a)),
            RelKind::Abs64 | RelKind::GlobDat | RelKind::JumpSlot => write(s.wrapping_add(a)),
            RelKind::IRelative => irelative.push((at, bias.wrapping_add(a))),
            RelKind::DtpMod => {
                let m = match def {
                    Some(Def { obj: Some(o), .. }) => self.module_of(o)?,
                    None if si == 0 => self.module_of(id)?,
                    None => 0,
                    Some(_) => return Err(Error::invalid(&self.obj(id).name, "TLS symbol bound to the host")),
                };
                write(m as u64);
            }
            RelKind::DtpOff => write(s.wrapping_add(a)),
            RelKind::TpOff => {
                let (m, v) = self.tls_target(id, si, def)?;
                let off = self.static_offset(m)?;
                write((off as u64).wrapping_add(v).wrapping_add(a));
            }
            RelKind::TlsDesc => {
                self.stats.tlsdesc += 1;
                let (func, arg) = self.tlsdesc(id, si, def, a)?;
                write(func);
                // SAFETY: a TLSDESC slot is two words; the second lies in
                // the same writable data.
                unsafe { ((at + 8) as *mut u64).write_unaligned(arg) };
            }
            RelKind::Copy => {
                return Err(Error::invalid(
                    &self.obj(id).name,
                    "copy relocations are not supported",
                ));
            }
            RelKind::Unknown(t) => {
                return Err(Error::invalid(
                    &self.obj(id).name,
                    &format!("unknown relocation type {t} at {:#x}", r.r_offset),
                ));
            }
        }
        self.stats.relocations += 1;
        Ok(())
    }

    fn module_of(&self, id: ObjectId) -> Result<usize> {
        self.obj(id)
            .tls
            .map(|t| t.module)
            .filter(|&m| m != 0)
            .ok_or_else(|| Error::invalid(&self.obj(id).name, "TLS relocation without PT_TLS"))
    }

    /// The defining object and symbol offset of a TLS relocation.
    fn tls_target(&self, id: ObjectId, si: u32, def: Option<Def>) -> Result<(ObjectId, u64)> {
        match def {
            Some(Def { obj: Some(o), sym, .. }) => Ok((o, sym.st_value)),
            None if si == 0 => Ok((id, 0)),
            _ => Err(Error::invalid(
                &self.obj(id).name,
                "TLS relocation against an undefined or host symbol",
            )),
        }
    }

    fn static_offset(&self, id: ObjectId) -> Result<i64> {
        self.obj(id)
            .tls
            .and_then(|t| t.tp_offset)
            .ok_or_else(|| {
                Error::invalid(
                    &self.obj(id).name,
                    "static TLS relocation (initial-exec) against a dynamically loaded TLS module",
                )
            })
    }

    fn tlsdesc(&mut self, id: ObjectId, si: u32, def: Option<Def>, a: u64) -> Result<(u64, u64)> {
        let machine = self.obj(id).machine;
        let run = self.runs_code();
        if si != 0 && def.is_none() {
            let f = if run { tls::tlsdesc_undefined_weak(machine)? } else { 0 };
            return Ok((f, a));
        }
        let (m, v) = self.tls_target(id, si, def)?;
        let t = self.obj(m).tls.expect("TLS target");
        if let Some(off) = t.tp_offset {
            let f = if run { tls::tlsdesc_static(machine)? } else { 0 };
            return Ok((f, (off as u64).wrapping_add(v).wrapping_add(a)));
        }
        let b = self.bridge.as_ref().ok_or_else(|| Error::new("dynamic TLS needs bionic"))?;
        let (f, generation) = (b.tlsdesc_dynamic(), b.tls_generation(t.module)?);
        let arg = Box::new([generation, t.module as u64, v.wrapping_add(a)]);
        let p = arg.as_ptr() as u64;
        self.tlsdesc_args.entry(id).or_default().push(arg);
        Ok((f, p))
    }

    // ---------------------------------------------------------------
    // Initializers, finalizers, unloading
    // ---------------------------------------------------------------

    /// Post-order over `DT_NEEDED` from `root`, objects not initialized yet.
    fn init_order(&self, root: ObjectId) -> Vec<ObjectId> {
        fn visit(l: &Linker, id: ObjectId, seen: &mut HashSet<ObjectId>, out: &mut Vec<ObjectId>) {
            if !seen.insert(id) || l.obj(id).initialized {
                return;
            }
            for &d in &l.obj(id).deps {
                visit(l, d, seen, out);
            }
            out.push(id);
        }
        let mut out = vec![];
        visit(self, root, &mut HashSet::new(), &mut out);
        out
    }

    /// The initializer functions of `order`, marking the objects
    /// initialized: the executable's preinit array first, then each
    /// object's `DT_INIT` and `DT_INIT_ARRAY`.
    pub fn init_fns(&mut self, order: &[ObjectId]) -> Result<Vec<u64>> {
        let mut fns = vec![];
        for &id in order {
            let o = self.obj(id);
            if o.is_main
                && let Some((a, sz)) = o.dynamic.preinit_array
            {
                fns.splice(0..0, read_fn_array(o, a, sz)?);
            }
        }
        for &id in order {
            let o = self.obj(id);
            if let Some(f) = o.dynamic.init {
                fns.push(o.addr(f));
            }
            if let Some((a, sz)) = o.dynamic.init_array {
                fns.extend(read_fn_array(o, a, sz)?);
            }
            self.obj_mut(id).initialized = true;
        }
        Ok(fns)
    }

    /// Drops a reference to `root`; returns the finalizers to run (in
    /// order) and the objects to unmap afterwards with [`Self::unmap`].
    pub fn release(&mut self, root: ObjectId) -> Result<(Vec<u64>, Vec<ObjectId>)> {
        let mut dead = vec![];
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let o = self.obj_mut(id);
            o.refcount = o.refcount.saturating_sub(1);
            if o.refcount == 0 && !o.nodelete && !o.is_main {
                dead.push(id);
                stack.extend(self.obj(id).deps.iter().copied());
            }
        }
        let mut fns = vec![];
        for &id in &dead {
            let o = self.obj(id);
            if o.initialized {
                if let Some((a, sz)) = o.dynamic.fini_array {
                    let mut v = read_fn_array(o, a, sz)?;
                    v.reverse();
                    fns.extend(v);
                }
                if let Some(f) = o.dynamic.fini {
                    fns.push(o.addr(f));
                }
            }
        }
        Ok((fns, dead))
    }

    /// Unmaps objects released by [`Self::release`].
    pub fn unmap(&mut self, dead: &[ObjectId]) {
        for &id in dead {
            if let (Some(t), Some(b)) = (self.obj(id).tls, &self.bridge)
                && t.module != 0
            {
                let _ = b.tls_unregister(t.module);
            }
            crate::debug::unregister(self.obj(id));
            self.drop_object(id);
        }
    }

    fn drop_object(&mut self, id: ObjectId) {
        if let Some(mut o) = self.objects[id].take() {
            self.global.retain(|&g| g != id);
            self.tlsdesc_args.remove(&id);
            crate::unwind::unregister(&o);
            o.image.unmap();
        }
    }
}

/// How a relocation type is applied, independent of the architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelKind {
    None,
    Relative,
    Abs64,
    GlobDat,
    JumpSlot,
    IRelative,
    DtpMod,
    DtpOff,
    TpOff,
    TlsDesc,
    Copy,
    Unknown(u32),
}

impl RelKind {
    pub fn of(machine: u16, ty: u32) -> RelKind {
        use RelKind::*;
        match machine {
            EM_X86_64 => match ty {
                x86_64::R_NONE => None,
                x86_64::R_RELATIVE => Relative,
                x86_64::R_64 => Abs64,
                x86_64::R_GLOB_DAT => GlobDat,
                x86_64::R_JUMP_SLOT => JumpSlot,
                x86_64::R_IRELATIVE => IRelative,
                x86_64::R_DTPMOD64 => DtpMod,
                x86_64::R_DTPOFF64 => DtpOff,
                x86_64::R_TPOFF64 => TpOff,
                x86_64::R_TLSDESC => TlsDesc,
                x86_64::R_COPY => Copy,
                t => Unknown(t),
            },
            EM_AARCH64 => match ty {
                aarch64::R_NONE => None,
                aarch64::R_RELATIVE => Relative,
                aarch64::R_ABS64 => Abs64,
                aarch64::R_GLOB_DAT => GlobDat,
                aarch64::R_JUMP_SLOT => JumpSlot,
                aarch64::R_IRELATIVE => IRelative,
                aarch64::R_TLS_DTPMOD64 => DtpMod,
                aarch64::R_TLS_DTPREL64 => DtpOff,
                aarch64::R_TLS_TPREL64 => TpOff,
                aarch64::R_TLSDESC => TlsDesc,
                aarch64::R_COPY => Copy,
                t => Unknown(t),
            },
            _ => Unknown(ty),
        }
    }

    fn needs_symbol(self) -> bool {
        !matches!(self, RelKind::None | RelKind::Relative | RelKind::IRelative)
    }
}

fn read_table<T: Copy>(o: &Object, a: u64, sz: u64) -> Result<Vec<T>> {
    let n = core::mem::size_of::<T>() as u64;
    if sz % n != 0 {
        return Err(Error::invalid(&o.name, "relocation table size is not a multiple of its entry"));
    }
    o.check(a, sz)?;
    (0..sz / n).map(|i| o.read::<T>(a + i * n)).collect()
}

fn read_fn_array(o: &Object, a: u64, sz: u64) -> Result<Vec<u64>> {
    Ok(read_table::<u64>(o, a, sz)?
        .into_iter()
        .filter(|&f| f != 0 && f != u64::MAX)
        .collect())
}

/// Reads the start of a file: at least the header and the program headers.
fn read_head(src: &mut dyn SegmentSource, name: &str) -> Result<Vec<u8>> {
    let mut head = vec![0u8; 64];
    // SAFETY: `head` is writable for 64 bytes.
    unsafe { src.copy_to(0, 64, head.as_mut_ptr()) }
        .map_err(|_| Error::invalid(name, "file too short for an ELF header"))?;
    let eh: Ehdr = read_at(&head, 0).expect("64 bytes");
    let end = eh
        .e_phoff
        .checked_add(u64::from(eh.e_phnum) * u64::from(eh.e_phentsize))
        .filter(|&e| e <= 1 << 20)
        .ok_or_else(|| Error::invalid(name, "bad program header table"))?
        .max(64);
    head.resize(end as usize, 0);
    // SAFETY: `head` is writable for `end` bytes.
    unsafe { src.copy_to(0, end, head.as_mut_ptr()) }
        .map_err(|_| Error::invalid(name, "program header table outside the file"))?;
    Ok(head)
}

/// Calls guest functions of the `(argc, argv, envp)` shape.
pub fn run_init_fns(fns: &[u64], argc: i32, argv: *const *const u8, envp: *const *const u8) {
    for &f in fns {
        // SAFETY: an initializer of a loaded, relocated object.
        let f: InitFn = unsafe { core::mem::transmute(f) };
        f(argc, argv, envp);
    }
}

/// Calls guest finalizers.
pub fn run_fini_fns(fns: &[u64]) {
    for &f in fns {
        // SAFETY: a finalizer of a loaded, relocated object.
        let f: VoidFn = unsafe { core::mem::transmute(f) };
        f();
    }
}

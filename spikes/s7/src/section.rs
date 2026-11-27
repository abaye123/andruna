//! Boot image files mapped by the parent into a suspended AppContainer
//! process (strategy 2 of docs/spec/04-security.md), with the real boot
//! image of the ART root.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile3, PAGE_READONLY, VIRTUAL_ALLOCATION_TYPE,
};

use crate::args::Args;
use crate::env::{self, Env, Grant};
use crate::proc::{quote, Proc};
use crate::win::{self, werr, Container, Owned};

/// Where the parent maps the image in the timing runs: away from ART's own
/// boot image base (0x60000000), because andruna-app still maps the image
/// itself.
pub const INJECT_BASE: usize = 0x7_0000_0000;

pub struct BootSections {
    pub files: Vec<(PathBuf, Owned, usize)>,
}

impl BootSections {
    /// One read-only file-backed section per file of `javalib/x86_64`.
    pub fn open(art_root: &Path) -> io::Result<BootSections> {
        let dir = art_root.join(r"javalib\x86_64");
        let mut files = vec![];
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .collect();
        names.sort();
        // boot.art first: it is the one ART expects at the base.
        names.sort_by_key(|p| p.file_name().unwrap() != "boot.art");
        for p in names {
            let len = std::fs::metadata(&p)?.len() as usize;
            let w = win::wpath(&p);
            // SAFETY: NUL-terminated path; handles owned right after; the
            // file handle may close once the section holds the file.
            unsafe {
                let f = Owned(
                    CreateFileW(
                        PCWSTR(w.as_ptr()),
                        GENERIC_READ.0,
                        FILE_SHARE_READ | FILE_SHARE_DELETE,
                        None,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        None,
                    )
                    .map_err(werr)?,
                );
                let s = CreateFileMappingW(f.0, None, PAGE_READONLY, 0, 0, PCWSTR::null())
                    .map_err(werr)?;
                files.push((p, Owned(s), len));
            }
        }
        Ok(BootSections { files })
    }

    pub fn total(&self) -> usize {
        self.files.iter().map(|f| f.2).sum()
    }

    /// Maps every section read-only into `process`, one after another from
    /// `base` at 64 KiB boundaries. Returns (base, size) of each view.
    pub fn map_into(&self, process: &Owned, base: usize) -> io::Result<Vec<(usize, usize)>> {
        let mut at = base;
        let mut out = vec![];
        for (_, s, len) in &self.files {
            // SAFETY: maps a section we hold into a process we created; the
            // range is free in a fresh process (MapViewOfFile3 fails
            // otherwise).
            let v = unsafe {
                MapViewOfFile3(
                    s.0,
                    Some(process.0),
                    Some(at as *const std::ffi::c_void),
                    0,
                    *len,
                    VIRTUAL_ALLOCATION_TYPE(0),
                    PAGE_READONLY.0,
                    None,
                )
            };
            if v.Value.is_null() {
                return Err(io::Error::last_os_error());
            }
            out.push((at, *len));
            at = (at + len + 0xFFFF) & !0xFFFF;
        }
        Ok(out)
    }
}

/// `s7 section`: maps the real boot image into a suspended container
/// process whose token cannot open the files, then lets it read them.
pub fn parent(a: &Args) -> io::Result<()> {
    let env = Env::new(a);
    let p = env::prepare(&env, Grant::Cap, false, "section")?;
    // The container may run s7.exe but not read the ART root.
    win::set_access(&env.art_root, &p.runtime_cap, 0, true)?;
    let boot = BootSections::open(&env.art_root)?;
    let boot_art = env.art_root.join(r"javalib\x86_64\boot.art");
    let caps = [p.runtime_cap.clone()];
    let c = Container {
        sid: &p.sid,
        capabilities: &caps,
        lpac: false,
    };
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..5 {
        let t = Instant::now();
        let mut ch = Proc::spawn(
            &format!(
                "{} section-child --file {}",
                quote(&env.s7().display().to_string()),
                quote(&boot_art.display().to_string())
            ),
            Some(&c),
            true,
            &[],
        )?;
        let created = t.elapsed();
        let tm = Instant::now();
        let views = boot.map_into(&ch.child.process, 0x6000_0000)?;
        let map = tm.elapsed();
        let list: Vec<String> = views.iter().map(|(b, l)| format!("{b:x}:{l:x}")).collect();
        ch.write(format!("{}\n", list.join(",")).as_bytes())?;
        ch.resume();
        let code = ch.wait(Duration::from_secs(30));
        best = best.min(map.as_secs_f64() * 1e3);
        last = Some((code, ch.out(), ch.err(), created, views.len()));
    }
    let (code, out, err, created, n) = last.unwrap();
    println!(
        "{} files, {:.1} MiB mapped into a suspended container process in {:.3} ms (best of 5; CreateProcess suspended {:.1} ms)",
        n,
        boot.total() as f64 / 1048576.0,
        best,
        created.as_secs_f64() * 1e3
    );
    println!("child exit {code:?}\n--- child stdout\n{out}--- child stderr\n{err}");
    // Restore the runtime grant.
    win::set_access(
        &env.art_root,
        &p.runtime_cap,
        win::FILE_GENERIC_READ_EXECUTE,
        false,
    )?;
    Ok(())
}

pub fn child(a: &Args) -> io::Result<()> {
    let (ac, il) = win::own_token();
    println!("token: appcontainer={ac} integrity=0x{il:x}");
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    for (i, v) in line.trim().split(',').enumerate() {
        let (b, l) = v.split_once(':').unwrap();
        let base = usize::from_str_radix(b, 16).unwrap();
        let len = usize::from_str_radix(l, 16).unwrap();
        let t = Instant::now();
        let mut sum = 0u64;
        // SAFETY: the parent mapped [base, base+len) read-only before this
        // process ran.
        unsafe {
            let mut o = 0;
            while o < len {
                sum = sum.wrapping_add(std::ptr::read_volatile((base + o) as *const u8) as u64);
                o += 4096;
            }
            let magic = std::slice::from_raw_parts(base as *const u8, 4);
            println!(
                "view {i} at {base:#x}: magic {:?}, touched {} pages in {:.2} ms (sum {sum})",
                String::from_utf8_lossy(magic),
                len.div_ceil(4096),
                t.elapsed().as_secs_f64() * 1e3
            );
        }
    }
    let f = a.str("--file", "");
    match std::fs::File::open(&f) {
        Ok(_) => println!("open {f}: allowed"),
        Err(e) => println!("open {f}: {e}"),
    }
    let _ = HANDLE::default();
    Ok(())
}

//! The shared boot image (spike S2, ADR 0035).
//!
//! ART maps its boot image into one reservation at the image base
//! (`0x60000000`): each `.art` image at its `image_begin`, each oat file's
//! loadable segments at its `oat_file_begin`. sysd lays those file pages
//! out once in a **view file** (`cache\boot-image\<isa>-<hash>.img`), the
//! reservation as ART fills it, with zeros where ART puts anonymous memory,
//! and holds one file-backed section of it. Every app process gets a
//! read-only view of the section at the image base while it is still
//! suspended; lx in the process lets ART's mappings of those files adopt
//! the view's pages ([`andruna_lx::image`]), so the image is read from
//! disk once and its clean pages are shared by every app. Pages ART writes
//! become private (copy-on-write) in that process only.
//!
//! The view file is rebuilt when an image file changes (its name carries a
//! hash of the files' names, sizes and modification times).

use std::fs;
use std::io;
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use andruna_ipc::host::{BootImage as Wire, Extent as WireExtent};
use windows::Win32::System::Memory::PAGE_EXECUTE_READ;

use super::win;

const PAGE: u64 = 0x1000;
const GRANULARITY: u64 = 0x1_0000;
/// ART's segment alignment for images on this build (`kElfSegmentAlignment`).
const IMAGE_ALIGN: u64 = 0x4000;
/// Version of the view file layout, part of its name.
const FORMAT: u32 = 1;

/// The instruction set directory name of this build.
pub const ISA: &str = if cfg!(target_arch = "x86_64") {
    "x86_64"
} else {
    "arm64"
};

fn round_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

/// One run of file pages in the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    /// Relative to the ART root, `/`-separated.
    pub rel: String,
    pub file: PathBuf,
    pub offset: u64,
    pub addr: u64,
    pub len: u64,
}

/// Where the image files go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub base: u64,
    pub size: u64,
    pub extents: Vec<Extent>,
}

fn u32_at(b: &[u8], off: usize) -> io::Result<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("truncated header"))
}

fn u64_at(b: &[u8], off: usize) -> io::Result<u64> {
    b.get(off..off + 8)
        .map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
        .ok_or_else(|| bad("truncated header"))
}

fn u16_at(b: &[u8], off: usize) -> io::Result<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| bad("truncated header"))
}

fn bad(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

fn read_head(path: &Path, n: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut b = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut b[got..])? {
            0 => break,
            k => got += k,
        }
    }
    b.truncate(got);
    Ok(b)
}

/// `(image_begin, image_size, oat_file_begin, oat_file_end)` of an `.art`
/// file.
fn image_header(path: &Path) -> io::Result<(u64, u64, u64, u64)> {
    let h = read_head(path, 64)?;
    if !h.starts_with(b"art\n") {
        return Err(bad("not an ART image"));
    }
    Ok((
        u64::from(u32_at(&h, 16)?),
        u64::from(u32_at(&h, 20)?),
        u64::from(u32_at(&h, 32)?),
        u64::from(u32_at(&h, 44)?),
    ))
}

/// The loadable segments of an ELF64 file with contents in the file:
/// `(p_offset, p_vaddr, p_filesz)`.
fn load_segments(path: &Path) -> io::Result<Vec<(u64, u64, u64)>> {
    let h = read_head(path, 64 * 1024)?;
    if !h.starts_with(b"\x7fELF") || h.get(4) != Some(&2) {
        return Err(bad("not an ELF64 file"));
    }
    let phoff = u64_at(&h, 0x20)? as usize;
    let phentsize = u16_at(&h, 0x36)? as usize;
    let phnum = u16_at(&h, 0x38)? as usize;
    let mut out = Vec::new();
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if u32_at(&h, p)? != 1 {
            continue;
        }
        let (off, vaddr, filesz) = (u64_at(&h, p + 8)?, u64_at(&h, p + 16)?, u64_at(&h, p + 32)?);
        if filesz > 0 {
            out.push((off, vaddr, filesz));
        }
    }
    Ok(out)
}

/// Reads the boot image in `<art_root>/javalib/<isa>` and computes where
/// its file pages go. `None` when there is no boot image.
pub fn layout(art_root: &Path) -> io::Result<Option<Layout>> {
    let dir = art_root.join("javalib").join(ISA);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut images = Vec::new();
    for e in entries {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "art") {
            let h = image_header(&p)?;
            images.push((p, h));
        }
    }
    if images.is_empty() {
        return Ok(None);
    }
    images.sort_by_key(|(_, h)| h.0);
    let rel = |p: &Path| -> String {
        format!(
            "javalib/{ISA}/{}",
            p.file_name().unwrap_or_default().to_string_lossy()
        )
    };
    let base = images[0].1.0;
    if base % GRANULARITY != 0 {
        return Err(bad("the image base is not 64 KiB aligned"));
    }
    let mut extents = Vec::new();
    let mut end = base;
    for (art, (begin, size, oat_begin, oat_end)) in &images {
        let art_len = round_up(fs::metadata(art)?.len(), PAGE);
        extents.push(Extent {
            rel: rel(art),
            file: art.clone(),
            offset: 0,
            addr: *begin,
            len: round_up(*size, IMAGE_ALIGN).min(art_len),
        });
        let oat = art.with_extension("oat");
        let oat_len = round_up(fs::metadata(&oat)?.len(), PAGE);
        for (off, vaddr, filesz) in load_segments(&oat)? {
            if off % PAGE != 0 || vaddr % PAGE != 0 {
                return Err(bad("an oat segment is not page aligned"));
            }
            let len = round_up(off + filesz, PAGE).min(oat_len) - off;
            extents.push(Extent {
                rel: rel(&oat),
                file: oat.clone(),
                offset: off,
                addr: oat_begin + vaddr,
                len,
            });
        }
        end = end.max(*oat_end).max(begin + round_up(*size, IMAGE_ALIGN));
    }
    extents.sort_by_key(|e| e.addr);
    for w in extents.windows(2) {
        if w[0].addr + w[0].len > w[1].addr {
            return Err(bad("boot image pieces overlap"));
        }
    }
    let end = end.max(extents.iter().map(|e| e.addr + e.len).max().unwrap_or(end));
    Ok(Some(Layout {
        base,
        size: round_up(end - base, GRANULARITY),
        extents,
    }))
}

/// FNV-1a over the inputs that decide the view file's contents.
fn fingerprint(l: &Layout) -> io::Result<u64> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: &[u8]| {
        for x in b {
            h ^= u64::from(*x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&FORMAT.to_le_bytes());
    feed(&l.base.to_le_bytes());
    feed(&l.size.to_le_bytes());
    for e in &l.extents {
        let m = fs::metadata(&e.file)?;
        let t = m
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        feed(e.rel.as_bytes());
        feed(&m.len().to_le_bytes());
        feed(&t.to_le_bytes());
        for v in [e.offset, e.addr, e.len] {
            feed(&v.to_le_bytes());
        }
    }
    Ok(h)
}

/// Writes the view file for `l` into `dir` unless it exists; returns its
/// path.
pub fn build_view_file(l: &Layout, dir: &Path) -> io::Result<PathBuf> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let path = dir.join(format!("{ISA}-{:016x}.img", fingerprint(l)?));
    if fs::metadata(&path).is_ok_and(|m| m.len() == l.size) {
        return Ok(path);
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        "{ISA}-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut out = fs::File::create(&tmp)?;
        out.set_len(l.size)?;
        let mut buf = Vec::new();
        for e in &l.extents {
            let mut f = fs::File::open(&e.file)?;
            f.seek(SeekFrom::Start(e.offset))?;
            buf.clear();
            f.take(e.len).read_to_end(&mut buf)?;
            out.seek(SeekFrom::Start(e.addr - l.base))?;
            out.write_all(&buf)?;
        }
        out.sync_all()?;
    }
    match fs::rename(&tmp, &path) {
        Ok(()) => Ok(path),
        // Another sysd instance wrote it first.
        Err(_) if fs::metadata(&path).is_ok_and(|m| m.len() == l.size) => {
            let _ = fs::remove_file(&tmp);
            Ok(path)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The boot image section sysd holds while it runs.
pub struct BootImage {
    pub layout: Layout,
    pub view_file: PathBuf,
    section: OwnedHandle,
}

impl std::fmt::Debug for BootImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootImage")
            .field("base", &format_args!("{:#x}", self.layout.base))
            .field("size", &self.layout.size)
            .field("view_file", &self.view_file)
            .finish()
    }
}

impl BootImage {
    /// Prepares the section for the boot image of `art_root`, building the
    /// view file in `cache_dir` if needed. `None` when there is no boot
    /// image.
    pub fn open(art_root: &Path, cache_dir: &Path) -> io::Result<Option<Self>> {
        let Some(layout) = layout(art_root)? else {
            return Ok(None);
        };
        let view_file = build_view_file(&layout, cache_dir)?;
        let section = win::file_section(&view_file)?;
        Ok(Some(Self {
            layout,
            view_file,
            section,
        }))
    }

    /// Maps the view into a suspended process at the image base, read-only.
    pub fn map_into(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        win::map_view(
            self.section.as_handle(),
            process,
            self.layout.base as usize,
            self.layout.size as usize,
            // Execute-read so that lx can make oat code executable; it
            // charges no commit (only copy-on-write pages do).
            PAGE_EXECUTE_READ,
        )
    }

    /// What the host needs to know about the view.
    pub fn describe(&self) -> Wire {
        Wire {
            base: self.layout.base,
            size: self.layout.size,
            extents: self
                .layout
                .extents
                .iter()
                .map(|e| WireExtent {
                    path: e.rel.clone(),
                    offset: e.offset,
                    addr: e.addr,
                    len: e.len,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf_with_segments(segs: &[(u64, u64, u64)], file_len: usize) -> Vec<u8> {
        let mut b = vec![0u8; file_len];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        for (i, (off, va, fs)) in segs.iter().enumerate() {
            let p = 64 + i * 56;
            b[p..p + 4].copy_from_slice(&1u32.to_le_bytes());
            b[p + 8..p + 16].copy_from_slice(&off.to_le_bytes());
            b[p + 16..p + 24].copy_from_slice(&va.to_le_bytes());
            b[p + 32..p + 40].copy_from_slice(&fs.to_le_bytes());
        }
        b
    }

    fn art(begin: u32, size: u32, oat_begin: u32, oat_end: u32, len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[..8].copy_from_slice(b"art\n119\0");
        for (o, v) in [(16, begin), (20, size), (32, oat_begin), (44, oat_end)] {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        for (i, x) in b.iter_mut().enumerate().skip(64) {
            *x = (i % 13) as u8;
        }
        b
    }

    #[test]
    fn layout_and_view_file() {
        let root = std::env::temp_dir().join(format!("andruna-sysd-bi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("javalib").join(ISA);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(layout(&root).unwrap(), None);
        fs::write(
            dir.join("boot.art"),
            art(0x6000_0000, 0x5100, 0x6001_0000, 0x6001_8000, 0x7000),
        )
        .unwrap();
        fs::write(
            dir.join("boot.oat"),
            elf_with_segments(&[(0, 0, 0x2000), (0x2000, 0x2000, 0x3100)], 0x5800),
        )
        .unwrap();
        let l = layout(&root).unwrap().unwrap();
        assert_eq!(l.base, 0x6000_0000);
        assert_eq!(l.size, 0x2_0000);
        let at: Vec<(u64, u64, u64)> = l
            .extents
            .iter()
            .map(|e| (e.offset, e.addr, e.len))
            .collect();
        assert_eq!(
            at,
            vec![
                (0, 0x6000_0000, 0x7000),
                (0, 0x6001_0000, 0x2000),
                (0x2000, 0x6001_2000, 0x4000),
            ]
        );
        let cache = root.join("cache");
        let v = build_view_file(&l, &cache).unwrap();
        let blob = fs::read(&v).unwrap();
        assert_eq!(blob.len() as u64, l.size);
        let art_bytes = fs::read(dir.join("boot.art")).unwrap();
        assert_eq!(&blob[..0x7000], &art_bytes[..]);
        let oat = fs::read(dir.join("boot.oat")).unwrap();
        assert_eq!(&blob[0x1_0000..0x1_5800], &oat[..]);
        assert!(blob[0x1_5800..].iter().all(|b| *b == 0));
        // The same inputs give the same file; a changed input a new one.
        assert_eq!(build_view_file(&l, &cache).unwrap(), v);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            dir.join("boot.oat"),
            elf_with_segments(&[(0, 0, 0x2000), (0x2000, 0x2000, 0x3100)], 0x5900),
        )
        .unwrap();
        let l2 = layout(&root).unwrap().unwrap();
        assert_ne!(build_view_file(&l2, &cache).unwrap(), v);
        let b = BootImage::open(&root, &cache).unwrap().unwrap();
        assert_eq!(
            b.describe().extents[1].path,
            format!("javalib/{ISA}/boot.oat")
        );
        drop(b);
        let _ = fs::remove_dir_all(&root);
    }
}

//! Mapping an object's `PT_LOAD` segments into the address space.
//!
//! As bionic's linker does, the whole span of the segments is reserved
//! first (`PROT_NONE`), then each segment is mapped inside it. File-backed
//! mappings are requested from andruna-lx when the object was opened
//! through lx; lx does not have them yet (`ENOSYS`), so the segments are
//! anonymous memory filled from the file. The `.bss` part beyond
//! `p_filesz` is zero because anonymous memory is.
//!
//! After relocation [`Image::protect`] applies each segment's flags (the
//! union for a page two segments share) and makes `PT_GNU_RELRO`
//! read-only.

use andruna_lx::linux::mman::{
    MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PAGE_SIZE, PROT_EXEC, PROT_NONE, PROT_READ, PROT_WRITE,
};

use crate::elf::{Headers, PF_R, PF_W, PF_X, PT_GNU_RELRO};
use crate::error::{Error, Result};

pub const PAGE: u64 = PAGE_SIZE as u64;

/// Reserved after each image for its translated Windows unwind tables,
/// which must lie within 4 GiB above the image base (`crate::unwind`).
pub const UNWIND_SPACE: u64 = 1 << 20;

pub fn page_down(v: u64) -> u64 {
    v & !(PAGE - 1)
}

pub fn page_up(v: u64) -> u64 {
    v.div_ceil(PAGE) * PAGE
}

/// The bytes of a segment in the file: either already in memory or read
/// on demand.
pub trait SegmentSource {
    /// Copies `len` bytes at file offset `off` to `dst`.
    ///
    /// # Safety
    ///
    /// `dst` is writable for `len` bytes.
    unsafe fn copy_to(&mut self, off: u64, len: u64, dst: *mut u8) -> Result<()>;

    /// An lx descriptor for a file-backed mapping, if the file was opened
    /// through lx.
    fn lx_fd(&self) -> Option<i32> {
        None
    }
}

/// A mapped object image.
#[derive(Debug)]
pub struct Image {
    /// Start of the reservation (page aligned).
    pub base: u64,
    /// Bytes of the reservation used by the segments.
    pub size: u64,
    /// Bytes reserved after `size` (unwind tables).
    pub extra: u64,
    /// Runtime address = `bias + p_vaddr`.
    pub bias: u64,
    /// Segments mapped from the file by lx (otherwise copied).
    pub file_mapped: bool,
}

fn prot_of(flags: u32) -> u32 {
    let mut p = PROT_NONE;
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

impl Image {
    /// Reserves the span and maps every `PT_LOAD` segment read-write.
    pub fn map(h: &Headers, src: &mut dyn SegmentSource, what: &str) -> Result<Self> {
        let span = h.span();
        // The base must keep every segment's alignment; reserve enough to
        // align it (p_align can exceed the page size, for example 16 KiB
        // segments of arm64 objects).
        let align = h.loads().map(|p| p.p_align).max().unwrap_or(PAGE).max(PAGE);
        let total = span + UNWIND_SPACE + align - PAGE;
        let raw = andruna_lx::mem::mmap(
            0,
            total as usize,
            PROT_NONE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
        .map_err(|e| {
            Error::new(format!(
                "\"{what}\": cannot reserve {span:#x} bytes: errno {}",
                e.0
            ))
        })? as u64;
        let base = raw.div_ceil(align) * align;
        if base > raw {
            let _ = andruna_lx::mem::munmap(raw as usize, (base - raw) as usize);
        }
        let end = base + span + UNWIND_SPACE;
        let raw_end = raw + total;
        if raw_end > end {
            let _ = andruna_lx::mem::munmap(end as usize, (raw_end - end) as usize);
        }
        let mut img = Image {
            base,
            size: span,
            extra: UNWIND_SPACE,
            bias: base.wrapping_sub(h.min_vaddr),
            file_mapped: false,
        };
        if let Err(e) = img
            .map_segments(h, src, what)
            .and_then(|()| img.protect_segments(h))
        {
            img.unmap();
            return Err(e);
        }
        Ok(img)
    }

    fn map_segments(&mut self, h: &Headers, src: &mut dyn SegmentSource, what: &str) -> Result<()> {
        let mut mapped_to = 0u64;
        for p in h.loads() {
            let start = page_down(self.bias + p.p_vaddr).max(mapped_to);
            let end = page_up(self.bias + p.p_vaddr + p.p_memsz);
            if end > start {
                andruna_lx::mem::mmap(
                    start as usize,
                    (end - start) as usize,
                    PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
                    -1,
                    0,
                )
                .map_err(|e| {
                    Error::new(format!("\"{what}\": cannot map a segment: errno {}", e.0))
                })?;
                mapped_to = end;
            }
            if p.p_filesz > 0
                && let Some(fd) = src.lx_fd()
            {
                // File-backed mapping through lx when it supports it.
                let fstart = page_down(self.bias + p.p_vaddr);
                let fend = page_up(self.bias + p.p_vaddr + p.p_filesz);
                match andruna_lx::mem::mmap(
                    fstart as usize,
                    (fend - fstart) as usize,
                    PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_FIXED,
                    fd,
                    page_down(p.p_offset) as i64,
                ) {
                    Ok(_) => {
                        let tail = self.bias + p.p_vaddr + p.p_filesz;
                        // SAFETY: the rest of the last file page belongs to
                        // this segment (.bss) and is mapped read-write.
                        unsafe {
                            core::ptr::write_bytes(tail as *mut u8, 0, (fend - tail) as usize)
                        };
                        self.file_mapped = true;
                        continue;
                    }
                    Err(andruna_lx::errno::Errno::ENOSYS) => {}
                    Err(e) => {
                        return Err(Error::new(format!(
                            "\"{what}\": file mapping failed: errno {}",
                            e.0
                        )));
                    }
                }
            }
            if p.p_filesz > 0 {
                // SAFETY: the segment's destination was just mapped
                // read-write and spans `p_memsz >= p_filesz` bytes.
                unsafe { src.copy_to(p.p_offset, p.p_filesz, (self.bias + p.p_vaddr) as *mut u8)? };
            }
        }
        Ok(())
    }

    /// Applies `PT_GNU_RELRO` after relocation (the segments already have
    /// their own protections, see [`Self::map`]).
    pub fn protect(&self, h: &Headers) -> Result<()> {
        for p in h.phdrs.iter().filter(|p| p.p_type == PT_GNU_RELRO) {
            let s = page_down(self.bias + p.p_vaddr);
            let e = page_down(self.bias + p.p_vaddr + p.p_memsz);
            if e > s {
                andruna_lx::mem::mprotect(s as usize, (e - s) as usize, PROT_READ)
                    .map_err(|e| Error::new(format!("RELRO mprotect failed: errno {}", e.0)))?;
            }
        }
        Ok(())
    }

    /// Gives each segment its own protection (the union for a page two
    /// segments share), as bionic maps them: code is executable before
    /// relocation, because IFUNC resolvers run during relocation; nothing
    /// writes to non-writable segments (text relocations are rejected).
    fn protect_segments(&self, h: &Headers) -> Result<()> {
        // Union of the protections of every segment touching a page.
        let mut pages: Vec<(u64, u64, u32)> = Vec::new();
        for p in h.loads() {
            let start = page_down(self.bias + p.p_vaddr);
            let end = page_up(self.bias + p.p_vaddr + p.p_memsz);
            let prot = prot_of(p.p_flags);
            let prev = pages.last().copied();
            if let Some(last) = prev
                && last.1 > start
            {
                // The first page is shared with the previous segment.
                let shared_end = last.1.min(end);
                let n = pages.len();
                pages[n - 1].1 = start;
                pages.push((start, shared_end, last.2 | prot));
                if end > shared_end {
                    pages.push((shared_end, end, prot));
                }
                continue;
            }
            pages.push((start, end, prot));
        }
        for (s, e, prot) in pages.into_iter().filter(|r| r.1 > r.0) {
            andruna_lx::mem::mprotect(s as usize, (e - s) as usize, prot)
                .map_err(|e| Error::new(format!("mprotect failed: errno {}", e.0)))?;
        }
        flush_code(self.base, self.size);
        Ok(())
    }

    /// Makes the image writable again (for `dlclose` bookkeeping and tests).
    pub fn unmap(&mut self) {
        if self.size != 0 {
            let _ = andruna_lx::mem::munmap(self.base as usize, (self.size + self.extra) as usize);
            self.size = 0;
        }
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.base + self.size
    }
}

#[cfg(target_arch = "aarch64")]
pub fn flush_code(base: u64, size: u64) {
    use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: flushes the instruction cache for a range of this process.
    let _ = unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(base as *const core::ffi::c_void),
            size as usize,
        )
    };
}

#[cfg(not(target_arch = "aarch64"))]
pub fn flush_code(_base: u64, _size: u64) {
    // x86_64 keeps instruction and data caches coherent.
}

/// A file read with `std::fs` (objects named by a Windows path).
pub struct HostFile {
    pub file: std::fs::File,
}

impl SegmentSource for HostFile {
    unsafe fn copy_to(&mut self, off: u64, len: u64, dst: *mut u8) -> Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        self.file
            .seek(SeekFrom::Start(off))
            .map_err(|e| Error::new(format!("read failed: {e}")))?;
        // SAFETY: the caller guarantees `dst` is writable for `len` bytes.
        let buf = unsafe { core::slice::from_raw_parts_mut(dst, len as usize) };
        self.file
            .read_exact(buf)
            .map_err(|e| Error::new(format!("read failed: {e}")))
    }
}

/// An object in memory (tests, and objects read through lx).
pub struct Bytes<'a>(pub &'a [u8]);

impl SegmentSource for Bytes<'_> {
    unsafe fn copy_to(&mut self, off: u64, len: u64, dst: *mut u8) -> Result<()> {
        let s = usize::try_from(off).ok();
        let e = s.and_then(|s| s.checked_add(len as usize));
        let (Some(s), Some(e)) = (s, e) else {
            return Err(Error::new("segment outside the file"));
        };
        let src = self
            .0
            .get(s..e)
            .ok_or_else(|| Error::new("segment outside the file"))?;
        // SAFETY: the caller guarantees `dst` is writable for `len` bytes,
        // and the source is a separate buffer.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::tests::{image, load};
    use crate::elf::*;

    fn win_protect(a: u64) -> u32 {
        use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
        let mut m = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries the address space only.
        unsafe { VirtualQuery(Some(a as *const _), &mut m, core::mem::size_of_val(&m)) };
        m.Protect.0
    }

    #[test]
    fn maps_copies_and_protects() {
        let ph = [
            load(0, 0, 0x200, 0x200, PF_R | PF_X),
            load(0x1200, 0x200, 0x10, 0x3000, PF_R | PF_W),
            Phdr {
                p_type: PT_DYNAMIC,
                p_vaddr: 0x1200,
                p_memsz: 16,
                ..Phdr::default()
            },
        ];
        let mut file = image(EM_NATIVE, &ph);
        file.resize(0x210, 0);
        file[0x200..0x210].copy_from_slice(b"0123456789abcdef");
        let eh = parse_ehdr(&file, EM_NATIVE, "t").unwrap();
        let h = parse_phdrs(&file, &eh, PAGE, "t").unwrap();
        let mut img = Image::map(&h, &mut Bytes(&file), "t").unwrap();
        assert_eq!(img.size, 0x5000);
        // SAFETY: the image is mapped and readable.
        unsafe {
            assert_eq!(*(img.bias as *const [u8; 4]), *b"\x7fELF");
            let d = (img.bias + 0x1200) as *const u8;
            assert_eq!(core::slice::from_raw_parts(d, 16), b"0123456789abcdef");
            // .bss is zero.
            assert_eq!(*((img.bias + 0x4000) as *const u64), 0);
        }
        img.protect(&h).unwrap();
        assert_eq!(win_protect(img.base), 0x20, "PAGE_EXECUTE_READ");
        assert_eq!(win_protect(img.base + 0x2000), 0x04, "PAGE_READWRITE");
        img.unmap();
    }

    #[test]
    fn segment_outside_the_file_fails() {
        let ph = [
            load(0, 0, 0x2000, 0x2000, PF_R),
            Phdr {
                p_type: PT_DYNAMIC,
                p_vaddr: 0x100,
                p_memsz: 16,
                ..Phdr::default()
            },
        ];
        let file = image(EM_NATIVE, &ph);
        let eh = parse_ehdr(&file, EM_NATIVE, "t").unwrap();
        let h = parse_phdrs(&file, &eh, PAGE, "t").unwrap();
        assert!(Image::map(&h, &mut Bytes(&file), "t").is_err());
    }
}

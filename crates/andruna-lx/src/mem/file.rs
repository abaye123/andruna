//! File mappings: section views of the file where Windows allows them,
//! copies where it does not.
//!
//! # Views
//!
//! A file mapping is a view of a section of the file
//! ([`crate::sys::section`]) when its address and its file offset are equal
//! modulo 64 KiB, which Windows requires of views: the view then starts on
//! the 64 KiB block below the mapping, and the pages of that block before
//! the mapping are *hidden* view pages (inaccessible, unmapped for Linux).
//! lx chooses such an address for every mapping without `MAP_FIXED`, so
//! only fixed mappings at other addresses, or whose 64 KiB block before
//! them is in use, fall back to copies. Semantics are Linux's:
//!
//! - `MAP_SHARED`: the pages are the file's, coherent with `read`, `write`
//!   and every other mapping of the file in any process; `msync` starts or
//!   (`MS_SYNC`) completes writing them.
//! - `MAP_PRIVATE`: copy-on-write. Pages not written yet show later changes
//!   to the file, as on Linux; written pages are the process's own.
//!   `madvise(MADV_DONTNEED)` gives a written page the file's contents
//!   again.
//! - The last page is zero past the end of the file; pages wholly past it
//!   raise `SIGBUS`.
//!
//! # Copies
//!
//! Otherwise the mapping is private memory filled from the file when it is
//! made (`pread`). For `MAP_PRIVATE` this differs from Linux only in that
//! later changes to the file are never seen (Linux leaves that
//! unspecified). For `MAP_SHARED` the copy is written back to the file on
//! `msync`, `munmap` and process exit ([`sync_all`]), and changes made to
//! the file in other ways are not seen: this is the one place where a
//! shared mapping is not coherent with the file.
//!
//! A view becomes copies (its Linux mappings keep their contents) when a
//! fixed mapping needs one of its hidden pages, when a non-executable view
//! is made executable, and when the file is truncated below it (Windows
//! refuses to truncate a mapped file; the pages past the new end of the
//! file then stay readable instead of raising `SIGBUS`).

use std::sync::Arc;

use windows::Win32::System::Memory::{
    PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_NOACCESS,
    PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

use super::*;
use crate::fs::file::RegularFile;
use crate::linux::fcntl::O_RDWR;
use crate::sys::section;
use crate::util::UserBuf;

/// Maps `len` bytes of `file` from `off` (see the module documentation).
pub(super) fn map(
    file: Arc<OpenFile>,
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    off: u64,
    shared: bool,
) -> SysResult<usize> {
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC | PROT_SEM) != 0 {
        return Err(Errno::EINVAL);
    }
    let len_p = match page_round_up(len) {
        Some(0) | None => return Err(Errno::EINVAL),
        Some(l) if l > USER_END => return Err(Errno::ENOMEM),
        Some(l) => l,
    };
    let size = u64::try_from(file.stat()?.size).unwrap_or(0);
    let data = if off < size {
        (size - off).min(len_p as u64) as usize
    } else {
        0
    };
    let rf = file.downcast::<RegularFile>();
    let ident = rf.and_then(|r| r.identity().ok()).unwrap_or((0, 0));
    let no_write = shared && file.access() != O_RDWR;
    let mut s = lock(&SPACE);
    let id = s.next_file;
    s.next_file = s.next_file.wrapping_add(1).max(1);
    s.files.insert(
        id,
        FileMap {
            file: file.clone(),
            addr0: 0,
            off0: off,
            shared,
            ident,
        },
    );
    let m = Request {
        id,
        addr,
        len: len_p,
        data,
        prot,
        flags,
        off,
        shared,
        no_write,
    };
    let r = match rf.filter(|_| data > 0) {
        Some(rf) => match s.map_view(rf, &m) {
            Some(r) => r,
            None => s.map_copy(&file, &m),
        },
        None => s.map_copy(&file, &m),
    };
    match r {
        Ok(start) => {
            let fm = s.files.get_mut(&id).expect("file registered");
            fm.addr0 = start;
            Ok(start)
        }
        Err(e) => {
            s.forget_files(vec![id]);
            Err(e)
        }
    }
}

/// One file mapping request (page-rounded length, `data` bytes of file).
struct Request {
    id: u32,
    addr: usize,
    len: usize,
    data: usize,
    prot: u32,
    flags: u32,
    off: u64,
    shared: bool,
    no_write: bool,
}

impl AddressSpace {
    /// Maps `m` as a view of `rf`'s file. `None` when Windows cannot (the
    /// caller makes a copy), `Some(Err)` for errors Linux reports.
    fn map_view(&mut self, rf: &RegularFile, m: &Request) -> Option<SysResult<usize>> {
        let delta = (m.off % GRANULARITY as u64) as usize;
        let voff = m.off - delta as u64;
        let data_p = page_round_up(m.data)?;
        let view_len = delta + m.data;
        let view_p = delta + data_p;
        let fixed = m.flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        let (v, a) = if fixed {
            let end = match check_fixed(m.addr, m.len) {
                Ok(e) => e,
                Err(e) => return Some(Err(e)),
            };
            if m.addr % GRANULARITY != delta || m.addr - delta < MIN_ADDR {
                return None;
            }
            let noreplace = m.flags & MAP_FIXED_NOREPLACE != 0;
            if let Err(e) = self.prepare_fixed(m.addr, end, noreplace) {
                return Some(Err(e));
            }
            let v = m.addr - delta;
            // The whole view range must be free placeholders of one
            // reservation.
            let r = self.reservations.range(..=v).next_back()?;
            if *r.1 < v + view_p {
                return None;
            }
            let ok = self
                .pieces(v, v + view_p)
                .iter()
                .all(|p| matches!(p, Piece::Own { kind, .. } if kind.is_placeholder()));
            if !ok {
                return None;
            }
            (v, m.addr)
        } else {
            let need = delta + m.len;
            let v = match self.find_free(need, GRANULARITY) {
                Some(v) => v,
                None => match self.new_reservation(None, need.next_multiple_of(GRANULARITY)) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                },
            };
            (v, v + delta)
        };
        let exec = m.prot & PROT_EXEC != 0;
        let writable = m.shared && !m.no_write;
        let Ok(sec) = rf.section(m.shared, writable, exec) else {
            if !fixed {
                self.collect_reservations(v, v + delta + m.len);
            }
            return None;
        };
        let max = match (m.shared, writable, exec) {
            (false, _, false) => PAGE_WRITECOPY,
            (false, _, true) => PAGE_EXECUTE_WRITECOPY,
            (true, false, false) => PAGE_READONLY,
            (true, false, true) => PAGE_EXECUTE_READ,
            (true, true, false) => PAGE_READWRITE,
            (true, true, true) => PAGE_EXECUTE_READWRITE,
        };
        let r = (|| -> SysResult<()> {
            self.split_at(v)?;
            self.split_at(v + view_p)?;
            if self.spans.range(v..v + view_p).count() > 1 {
                section::coalesce(v, view_p)?;
                self.replace_with_placeholder(v, v + view_p);
            }
            section::map(&sec, v, voff, view_len, max)
        })();
        if r.is_err() {
            if !fixed {
                self.collect_reservations(v, v + delta + m.len);
            }
            return None;
        }
        let mk = |hidden: bool, prot: u32| Kind::View {
            view: v,
            prot,
            file: m.id,
            hidden,
            dirty: !m.shared && prot & PROT_WRITE != 0,
            no_write: m.no_write,
            no_exec: !exec,
        };
        self.spans.remove(&v);
        if delta > 0 {
            let _ = os::protect(v, delta, PAGE_NOACCESS);
            self.spans.insert(
                v,
                Span {
                    end: a,
                    kind: mk(true, PROT_NONE),
                },
            );
        }
        self.spans.insert(
            a,
            Span {
                end: a + data_p,
                kind: mk(false, m.prot),
            },
        );
        self.views.insert(v, v + view_p);
        let want = view_protection(m.prot, m.shared);
        if want != max
            && let Err(e) = os::protect(a, data_p, want)
        {
            return Some(Err(e));
        }
        if m.len > data_p
            && let Err(e) = self.map_placeholders(a + data_p, a + m.len, m.prot, Src::PastEof(m.id))
        {
            return Some(Err(e));
        }
        Some(Ok(a))
    }

    /// Maps `m` as private memory filled from the file.
    fn map_copy(&mut self, file: &OpenFile, m: &Request) -> SysResult<usize> {
        let fixed = m.flags & (MAP_FIXED | MAP_FIXED_NOREPLACE);
        let rw = PROT_READ | PROT_WRITE;
        let start = self.map_private(m.addr, m.len, rw, fixed, Src::Copy(m.id))?;
        let data_p = page_round_up(m.data).unwrap_or(m.len);
        let mut done = 0usize;
        while done < m.data {
            // SAFETY: the new mapping is writable and nothing else refers
            // to it yet.
            let buf = unsafe { UserBuf::new((start + done) as *mut u8, m.data - done) };
            match file.pread(buf, (m.off + done as u64) as i64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) => {
                    let _ = self.unmap_range(start, start + m.len);
                    self.collect_reservations(start, start + m.len);
                    return Err(e);
                }
            }
        }
        if m.len > data_p {
            let (s, e) = (start + data_p, start + m.len);
            self.split_at(s)?;
            os::decommit(s, e - s)?;
            self.update(s, e, |k| {
                if let Kind::Mapped { committed, src, .. } = k {
                    *committed = false;
                    *src = Src::PastEof(m.id);
                }
            });
        }
        if m.prot != rw {
            self.mprotect(start, m.len, m.prot)?;
        }
        if m.no_write {
            self.set_no_write(start, start + m.len);
        }
        Ok(start)
    }

    /// Writes the pages `[start, end)` of a shared file copy back to the
    /// file (the part inside the file; a mapping never extends a file).
    pub(super) fn write_back(&self, start: usize, end: usize, id: u32) {
        let Some(fm) = self.files.get(&id) else {
            return;
        };
        if !fm.shared || fm.file.access() != O_RDWR {
            return;
        }
        let Some(rf) = fm.file.downcast::<RegularFile>() else {
            return;
        };
        let size = fm.file.stat().map(|s| s.size as u64).unwrap_or(0);
        let off = fm.offset(start);
        if off >= size {
            return;
        }
        let n = ((size - off) as usize).min(end - start);
        // SAFETY: the pages are an lx mapping; Windows reads them and
        // fails (nothing written) if they are not readable.
        let buf = unsafe { UserBuf::new(start as *mut u8, n) };
        let _ = rf.write_back(buf, off);
    }

    /// Fills `[start, end)` (writable) from the mapping's file; zero past
    /// the end of the file.
    fn refill(&self, start: usize, end: usize, id: u32) -> SysResult<()> {
        let fm = self.files.get(&id).ok_or(Errno::EINVAL)?;
        let mut done = 0usize;
        while done < end - start {
            // SAFETY: the caller made the range writable; it is an lx
            // mapping that nothing else refers to through Rust references.
            let buf = unsafe { UserBuf::new((start + done) as *mut u8, end - start - done) };
            match fm
                .file
                .pread(buf, (fm.offset(start) + done as u64) as i64)?
            {
                0 => break,
                n => done += n,
            }
        }
        // SAFETY: as above.
        unsafe { core::ptr::write_bytes((start + done) as *mut u8, 0, end - start - done) };
        Ok(())
    }

    /// `MADV_DONTNEED` on file pages: a private page gets the file's
    /// contents again; shared pages are the file's already (a shared copy
    /// is written back).
    pub(super) fn discard_file_pages(
        &mut self,
        start: usize,
        end: usize,
        kind: Kind,
    ) -> SysResult<()> {
        match kind {
            Kind::Mapped {
                src: Src::Copy(f),
                prot,
                ..
            } => {
                if self.files.get(&f).is_some_and(|fm| fm.shared) {
                    self.write_back(start, end, f);
                    return Ok(());
                }
                os::protect(start, end - start, PAGE_READWRITE)?;
                let r = self.refill(start, end, f);
                os::protect(start, end - start, page_protection(prot))?;
                r
            }
            Kind::View {
                file,
                prot,
                dirty: true,
                ..
            } if !self.files.get(&file).is_some_and(|fm| fm.shared) => {
                os::protect(start, end - start, PAGE_WRITECOPY)?;
                let r = self.refill(start, end, file);
                os::protect(start, end - start, view_protection(prot, false))?;
                r
            }
            _ => Ok(()),
        }
    }

    /// Turns the view at `view` into copies: each Linux mapping in it
    /// becomes private memory with the same contents, the hidden pages
    /// become placeholders.
    pub(super) fn materialize(&mut self, view: usize) -> SysResult<()> {
        let Some(&vend) = self.views.get(&view) else {
            return Ok(());
        };
        let spans: Vec<(usize, Span)> = self
            .spans
            .range(view..vend)
            .map(|(k, s)| (*k, *s))
            .collect();
        let mut saved = Vec::new();
        for (s, sp) in spans {
            let Kind::View {
                prot,
                file,
                hidden: false,
                no_write,
                ..
            } = sp.kind
            else {
                continue;
            };
            if prot & PROT_READ == 0 {
                os::protect(s, sp.end - s, PAGE_READONLY)?;
            }
            // SAFETY: a readable view range lx owns.
            let data = unsafe { core::slice::from_raw_parts(s as *const u8, sp.end - s) }.to_vec();
            saved.push((s, sp.end, prot, file, no_write, data));
        }
        section::unmap(view)?;
        self.replace_with_placeholder(view, vend);
        self.views.remove(&view);
        for (s, e, prot, file, no_write, data) in saved {
            self.split_at(s)?;
            self.split_at(e)?;
            os::replace_placeholder(s, e - s, Some(PAGE_READWRITE))?;
            // SAFETY: a fresh writable allocation of `e - s` bytes.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), s as *mut u8, e - s) };
            if prot != PROT_READ | PROT_WRITE {
                os::protect(s, e - s, page_protection(prot))?;
            }
            self.spans.insert(
                s,
                Span {
                    end: e,
                    kind: Kind::Mapped {
                        alloc: s,
                        prot,
                        committed: true,
                        no_write,
                        src: Src::Copy(file),
                    },
                },
            );
        }
        Ok(())
    }
}

/// `MS_*` flags of `msync`.
pub const MS_ASYNC: i32 = 1;
pub const MS_INVALIDATE: i32 = 2;
pub const MS_SYNC: i32 = 4;

/// `msync`: shared views start writing their dirty pages
/// (`FlushViewOfFile`), and with `MS_SYNC` the file is flushed to disk;
/// shared copies are written back. Private and anonymous memory has
/// nothing to write. `ENOMEM` if part of the range is not mapped.
pub fn msync(addr: usize, len: usize, flags: i32) -> SysResult<()> {
    if flags & !(MS_ASYNC | MS_INVALIDATE | MS_SYNC) != 0
        || flags & (MS_ASYNC | MS_SYNC) == MS_ASYNC | MS_SYNC
    {
        return Err(Errno::EINVAL);
    }
    let (start, end) = check_range(addr, len)?;
    let s = lock(&SPACE);
    let mut sync = Vec::new();
    for p in s.require_mapped(start, end)? {
        let Piece::Own { start, end, kind } = p else {
            continue;
        };
        let Some(f) = kind.file() else {
            continue;
        };
        let Some(fm) = s.files.get(&f) else {
            continue;
        };
        if !fm.shared {
            continue;
        }
        match kind {
            Kind::View { .. } => section::flush(start, end - start)?,
            Kind::Mapped {
                src: Src::Copy(_), ..
            } => s.write_back(start, end, f),
            _ => {}
        }
        if flags & MS_SYNC != 0 {
            sync.push(fm.file.clone());
        }
    }
    drop(s);
    for f in sync {
        f.sync()?;
    }
    Ok(())
}

/// Writes every shared file copy back to its file (process exit).
pub fn sync_all() {
    let s = lock(&SPACE);
    for (&start, span) in &s.spans {
        if let Kind::Mapped {
            src: Src::Copy(f), ..
        } = span.kind
        {
            s.write_back(start, span.end, f);
        }
    }
}

/// Turns every view of the file `ident` into copies (before a truncation
/// Windows refuses while the file is mapped).
pub fn release_views_of(ident: (u64, u64)) {
    let mut s = lock(&SPACE);
    let views: Vec<usize> = s
        .views
        .iter()
        .filter(|(v, e)| {
            s.spans.range(**v..**e).any(|(_, sp)| {
                sp.kind
                    .file()
                    .and_then(|f| s.files.get(&f))
                    .is_some_and(|fm| fm.ident == ident)
            })
        })
        .map(|(v, _)| *v)
        .collect();
    for v in views {
        let _ = s.materialize(v);
    }
}

/// `mremap` growing or moving a file mapping: the file is mapped again
/// with `new` bytes from the old mapping's offset, a private mapping's
/// changes are copied, and the old range is unmapped.
pub(super) fn remap(
    fm: FileMap,
    shared: bool,
    addr: usize,
    old: usize,
    new: usize,
    prot: u32,
) -> SysResult<usize> {
    let flags = if shared { MAP_SHARED } else { MAP_PRIVATE };
    if !shared && prot & PROT_READ == 0 {
        // Unreadable pages cannot be copied without changing them.
        return Err(Errno::EFAULT);
    }
    let tmp = if shared {
        prot
    } else {
        prot | PROT_READ | PROT_WRITE
    };
    let to = map(fm.file.clone(), 0, new, tmp, flags, fm.off0, shared)?;
    if !shared {
        // The old pages inside the file (those past it are not readable).
        let size = u64::try_from(fm.file.stat()?.size).unwrap_or(0);
        let n = page_round_up(size.saturating_sub(fm.off0).min(old as u64) as usize).unwrap_or(0);
        // SAFETY: both ranges are lx mappings: the old one readable for
        // `n <= old` bytes, the new one writable and longer.
        unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, to as *mut u8, n) };
        if tmp != prot {
            mprotect(to, new, prot)?;
        }
    }
    munmap(addr, old)?;
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{self, tests::test_dir};
    use crate::linux::fcntl::{AT_FDCWD, O_CREAT, O_RDONLY};

    const RW: u32 = PROT_READ | PROT_WRITE;

    /// A file of `pages` pages plus `extra` bytes, page `i` filled with
    /// `i + 1`; returns its path and a read-write descriptor.
    fn file(name: &str, pages: usize, extra: usize) -> (String, i32) {
        let p = format!("{}/f", test_dir(name));
        let fd = fs::openat(AT_FDCWD, p.as_bytes(), O_CREAT | O_RDWR, 0o644).unwrap();
        let data: Vec<u8> = (0..pages * PAGE_SIZE + extra)
            .map(|i| (i / PAGE_SIZE) as u8 + 1)
            .collect();
        assert_eq!(fs::pwrite(fd, UserBuf::from_ref(&data), 0), Ok(data.len()));
        (p, fd)
    }

    fn at(a: usize) -> u8 {
        // SAFETY: tests read pages they mapped readable.
        unsafe { (a as *const u8).read_volatile() }
    }

    fn put(a: usize, v: u8) {
        // SAFETY: tests write pages they mapped writable.
        unsafe { (a as *mut u8).write_volatile(v) }
    }

    fn pread1(fd: i32, off: usize) -> u8 {
        let mut b = [0u8; 1];
        fs::pread(fd, UserBuf::from_mut(&mut b), off as i64).unwrap();
        b[0]
    }

    fn pwrite1(fd: i32, off: usize, v: u8) {
        fs::pwrite(fd, UserBuf::from_ref(&[v]), off as i64).unwrap();
    }

    fn is_view(a: usize) -> bool {
        os::query(a).is_some_and(|r| r.kind.0 == 0x40000 /* MEM_MAPPED */)
    }

    #[test]
    fn shared_view_is_coherent_with_the_file() {
        let (_, fd) = file("view-shared", 20, 0);
        let len = 20 * PAGE_SIZE;
        let a = mmap(0, len, RW, MAP_SHARED, fd, 0).unwrap();
        assert!(is_view(a), "a view, not a copy");
        assert_eq!(at(a + 5 * PAGE_SIZE), 6);
        // write() is seen by the mapping, stores by read().
        pwrite1(fd, 3 * PAGE_SIZE, 0x77);
        assert_eq!(at(a + 3 * PAGE_SIZE), 0x77);
        put(a + 4 * PAGE_SIZE, 0x55);
        assert_eq!(pread1(fd, 4 * PAGE_SIZE), 0x55);
        // A second mapping of the file sees the first one's stores.
        let b = mmap(
            0,
            PAGE_SIZE,
            PROT_READ,
            MAP_SHARED,
            fd,
            4 * PAGE_SIZE as i64,
        )
        .unwrap();
        assert_eq!(at(b), 0x55);
        msync(a, len, MS_SYNC).unwrap();
        assert_eq!(msync(a, len, MS_SYNC | MS_ASYNC), Err(Errno::EINVAL));
        munmap(b, PAGE_SIZE).unwrap();
        munmap(a, len).unwrap();
        assert_eq!(msync(a, len, MS_ASYNC), Err(Errno::ENOMEM));
        fs::close(fd).unwrap();
    }

    #[test]
    fn read_only_shared_view_cannot_be_made_writable() {
        let (p, fd) = file("view-ro", 2, 0);
        let ro = fs::openat(AT_FDCWD, p.as_bytes(), O_RDONLY, 0).unwrap();
        let a = mmap(0, 2 * PAGE_SIZE, PROT_READ, MAP_SHARED, ro, 0).unwrap();
        assert!(is_view(a));
        pwrite1(fd, 10, 9);
        assert_eq!(at(a + 10), 9, "no snapshot: later writes are seen");
        assert_eq!(mprotect(a, PAGE_SIZE, RW), Err(Errno::EACCES));
        munmap(a, 2 * PAGE_SIZE).unwrap();
        fs::close(ro).unwrap();
        fs::close(fd).unwrap();
    }

    #[test]
    fn private_view_is_copy_on_write() {
        let (_, fd) = file("view-private", 4, 100);
        let len = 6 * PAGE_SIZE;
        // An offset that is not a multiple of 64 KiB: a view with hidden
        // pages before it.
        let off = PAGE_SIZE;
        let a = mmap(0, len, RW, MAP_PRIVATE, fd, off as i64).unwrap();
        assert!(is_view(a));
        assert_eq!(a % GRANULARITY, off % GRANULARITY);
        assert_eq!(fault_kind(a - PAGE_SIZE), Some(FaultKind::Unmapped));
        assert_eq!(at(a), 2);
        // Unwritten pages follow the file; written ones are private.
        pwrite1(fd, 2 * PAGE_SIZE, 0x31);
        assert_eq!(at(a + PAGE_SIZE), 0x31);
        put(a, 0xee);
        assert_eq!(pread1(fd, PAGE_SIZE), 2);
        // The last page is zero past the end; the pages after it raise
        // SIGBUS.
        assert_eq!(at(a + 3 * PAGE_SIZE + 99), 5);
        assert_eq!(at(a + 3 * PAGE_SIZE + 100), 0);
        assert_eq!(fault_kind(a + 4 * PAGE_SIZE), Some(FaultKind::PastEof));
        assert_eq!(fault_kind(a + 5 * PAGE_SIZE), Some(FaultKind::PastEof));
        // MADV_DONTNEED brings back the file's contents; MADV_FREE is for
        // anonymous memory only.
        madvise(a, PAGE_SIZE, MADV_DONTNEED).unwrap();
        assert_eq!(at(a), 2);
        assert_eq!(madvise(a, PAGE_SIZE, MADV_FREE), Err(Errno::EINVAL));
        // maps shows the file offset.
        let maps = String::from_utf8(maps()).unwrap();
        // One line for the whole mapping, past the end of the file included.
        let line = format!("{a:08x}-{:08x} rw-p {off:08x}", a + len);
        assert!(maps.lines().any(|l| l.starts_with(&line)), "{maps}");
        munmap(a, len).unwrap();
        assert_ne!(fault_kind(a), Some(FaultKind::Mapped));
        fs::close(fd).unwrap();
    }

    #[test]
    fn fixed_mappings_use_views_when_aligned() {
        let (_, fd) = file("view-fixed", 32, 0);
        // A reservation, as ELF loaders and ART make, then segments mapped
        // into it.
        let anon = MAP_PRIVATE | MAP_ANONYMOUS;
        let r = mmap(0, 64 * PAGE_SIZE, PROT_NONE, anon, -1, 0).unwrap();
        assert_eq!(r % GRANULARITY, 0, "64 KiB mappings start on a block");
        let fixed = MAP_PRIVATE | MAP_FIXED;
        let a = mmap(r, 16 * PAGE_SIZE, PROT_READ, fixed, fd, 0).unwrap();
        assert_eq!(a, r);
        assert!(is_view(a));
        // An offset that does not match the address modulo 64 KiB: a copy.
        let c = mmap(r + 20 * PAGE_SIZE, PAGE_SIZE, PROT_READ, fixed, fd, 0).unwrap();
        assert!(!is_view(c));
        assert_eq!(at(c), 1);
        // MAP_FIXED over part of the view: the rest stays readable.
        let d = mmap(r + 4 * PAGE_SIZE, PAGE_SIZE, RW, anon | MAP_FIXED, -1, 0).unwrap();
        assert_eq!(at(d), 0);
        assert_eq!(at(r + 5 * PAGE_SIZE), 6);
        assert_eq!(at(r), 1);
        munmap(r, 64 * PAGE_SIZE).unwrap();
        fs::close(fd).unwrap();
    }

    #[test]
    fn partial_unmap_hides_pages_until_the_view_goes() {
        let (_, fd) = file("view-partial", 16, 0);
        let len = 16 * PAGE_SIZE;
        let a = mmap(0, len, RW, MAP_PRIVATE, fd, 0).unwrap();
        put(a + 8 * PAGE_SIZE, 0x42);
        munmap(a + 2 * PAGE_SIZE, 2 * PAGE_SIZE).unwrap();
        assert_eq!(fault_kind(a + 2 * PAGE_SIZE), Some(FaultKind::Unmapped));
        assert_eq!(
            mprotect(a + 2 * PAGE_SIZE, PAGE_SIZE, RW),
            Err(Errno::ENOMEM)
        );
        assert_eq!(at(a + 8 * PAGE_SIZE), 0x42);
        // A fixed mapping on the hidden pages turns the view into copies
        // that keep their contents.
        let f = MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED;
        assert_eq!(
            mmap(a + 2 * PAGE_SIZE, PAGE_SIZE, RW, f, -1, 0),
            Ok(a + 2 * PAGE_SIZE)
        );
        assert!(!is_view(a + 8 * PAGE_SIZE));
        assert_eq!(at(a + 8 * PAGE_SIZE), 0x42);
        assert_eq!(at(a + 9 * PAGE_SIZE), 10);
        assert_eq!(at(a), 1);
        munmap(a, len).unwrap();
        fs::close(fd).unwrap();
    }

    #[test]
    fn executable_and_truncated_views() {
        let (_, fd) = file("view-exec", 16, 0);
        let len = 16 * PAGE_SIZE;
        let a = mmap(0, len, PROT_READ | PROT_EXEC, MAP_PRIVATE, fd, 0).unwrap();
        assert!(is_view(a));
        munmap(a, len).unwrap();
        // A non-executable view made executable becomes a copy.
        let b = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0).unwrap();
        mprotect(b, PAGE_SIZE, PROT_READ | PROT_EXEC).unwrap();
        assert_eq!(at(b + PAGE_SIZE), 2);
        munmap(b, len).unwrap();
        // Truncating below a shared view works (the view becomes copies).
        let c = mmap(0, len, RW, MAP_SHARED, fd, 0).unwrap();
        assert!(is_view(c));
        put(c, 0x99);
        fs::ftruncate(fd, (2 * PAGE_SIZE) as i64).unwrap();
        assert_eq!(at(c), 0x99);
        assert_eq!(pread1(fd, 0), 0x99);
        munmap(c, len).unwrap();
        fs::close(fd).unwrap();
    }

    #[test]
    fn shared_copy_is_written_back_and_mremap_moves_file_mappings() {
        let (_, fd) = file("view-copy", 32, 0);
        let anon = MAP_PRIVATE | MAP_ANONYMOUS;
        let r = mmap(0, 64 * PAGE_SIZE, PROT_NONE, anon, -1, 0).unwrap();
        // Writable shared at an address that cannot be a view: a copy,
        // written back on msync and munmap.
        let s = r + PAGE_SIZE;
        let c = mmap(s, 2 * PAGE_SIZE, RW, MAP_SHARED | MAP_FIXED, fd, 0).unwrap();
        assert!(!is_view(c));
        put(c, 0xab);
        msync(c, PAGE_SIZE, MS_SYNC).unwrap();
        assert_eq!(pread1(fd, 0), 0xab);
        put(c + PAGE_SIZE, 0xcd);
        munmap(c, 2 * PAGE_SIZE).unwrap();
        assert_eq!(pread1(fd, PAGE_SIZE), 0xcd);
        munmap(r, 64 * PAGE_SIZE).unwrap();
        // mremap of a private file mapping keeps its changes and maps more
        // of the file.
        let m = mmap(0, 2 * PAGE_SIZE, RW, MAP_PRIVATE, fd, 0).unwrap();
        put(m, 0x11);
        let n = mremap(m, 2 * PAGE_SIZE, 8 * PAGE_SIZE, MREMAP_MAYMOVE).unwrap();
        assert_eq!(at(n), 0x11);
        assert_eq!(at(n + 5 * PAGE_SIZE), 6);
        assert_eq!(pread1(fd, 0), 0xab);
        munmap(n, 8 * PAGE_SIZE).unwrap();
        fs::close(fd).unwrap();
    }
}

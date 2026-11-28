//! A boot image region mapped by the parent process (sysd) before this
//! process started ([ADR 0035](../../../docs/decisions/0035-warm-process-strategy.md),
//! spike S2).
//!
//! sysd maps one read-only view of a file-backed section at the boot image
//! base into the suspended process. The view holds the boot image files at
//! the addresses ART maps them to (its `.art` images and the loadable
//! segments of its oat files) and zeros elsewhere. The host registers the
//! region with [`adopt`] before guest code runs; from then on the region is
//! part of the Linux address space as lx presents it:
//!
//! - Pages start **unmapped**. Their Windows protection does not matter
//!   until a mapping claims them.
//! - A file mapping (`MAP_PRIVATE`, any address request that lands in the
//!   region) whose pages hold exactly the requested file pages
//!   ([`Extent`]), and which no earlier mapping made writable, **adopts**
//!   the view's pages: only the protection changes, nothing is copied. A
//!   writable mapping becomes `PAGE_WRITECOPY`, so this process gets a
//!   private copy of the pages it writes and the shared image stays
//!   unchanged. The first and last requested page are compared with the
//!   file before adopting, so a stale view is never used.
//! - Any other file mapping in the region is filled from the file into
//!   copy-on-write pages, as lx fills file mappings elsewhere.
//! - An anonymous mapping adopts pages that are known to be zero (outside
//!   every extent and never writable); other pages are zeroed (at once, or
//!   when an inaccessible mapping is first made accessible).
//! - `munmap` returns pages to unmapped and makes them inaccessible; the
//!   view stays, since a view cannot be partly unmapped.
//!
//! Requests that straddle the region's end are split: the part outside is
//! ordinary lx memory ([`crate::mem`]). `mremap` of region pages is
//! refused (`EINVAL`), and `MADV_DONTNEED` of a file mapping that was
//! written is refused (`EINVAL`): the original contents cannot be read
//! back without the file description.

use std::collections::BTreeMap;
use std::sync::Mutex;

use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_MAPPED, PAGE_EXECUTE_READ, PAGE_EXECUTE_WRITECOPY, PAGE_NOACCESS,
    PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_WRITECOPY,
};

use crate::errno::{Errno, SysResult};
use crate::linux::mman::*;
use crate::sys::mem as os;
use crate::util::lock;

/// File pages present in the region: bytes `[offset, offset+len)` of the
/// file at guest path `path` are at `[addr, addr+len)`. `offset`, `addr`
/// and `len` are page-aligned; bytes past the end of the file read as
/// zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    pub path: Vec<u8>,
    pub offset: u64,
    pub addr: usize,
    pub len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Unmapped,
    Mapped {
        prot: u32,
        /// An anonymous mapping (zero-filled), as opposed to a file one.
        anon: bool,
        /// Its pages are not known to be zero yet: zero them before they
        /// become accessible.
        zero_pending: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    end: usize,
    state: State,
    /// The pages still hold the view's contents: never writable here.
    pristine: bool,
}

#[derive(Debug)]
struct Region {
    base: usize,
    end: usize,
    extents: Vec<Extent>,
    /// Runs of pages keyed by start; they cover `[base, end)`.
    runs: BTreeMap<usize, Run>,
}

static REGION: Mutex<Option<Region>> = Mutex::new(None);

/// Statistics of the region, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes of file mappings that adopted the view's pages.
    pub adopted: usize,
    /// Bytes of file mappings filled from the file instead.
    pub copied: usize,
}

static STATS: Mutex<Stats> = Mutex::new(Stats {
    adopted: 0,
    copied: 0,
});

pub fn stats() -> Stats {
    *lock(&STATS)
}

/// Registers the view at `[base, base+len)` with its extents. Fails with
/// `EINVAL` if the range is not one mapped, committed view, or the extents
/// are not inside it, page-aligned and disjoint; with `EEXIST` if a region
/// is registered already.
pub fn adopt(base: usize, len: usize, mut extents: Vec<Extent>) -> SysResult<()> {
    let aligned = |v: usize| v.is_multiple_of(PAGE_SIZE);
    let end = base.checked_add(len).ok_or(Errno::EINVAL)?;
    if !base.is_multiple_of(crate::mem::GRANULARITY) || !aligned(len) || len == 0 {
        return Err(Errno::EINVAL);
    }
    extents.sort_by_key(|e| e.addr);
    let mut prev = base;
    for e in &extents {
        let e_end = e.addr.checked_add(e.len).ok_or(Errno::EINVAL)?;
        if !aligned(e.addr) || !aligned(e.len) || !(e.offset as usize).is_multiple_of(PAGE_SIZE) {
            return Err(Errno::EINVAL);
        }
        if e.addr < prev || e_end > end || e.len == 0 || e.path.is_empty() {
            return Err(Errno::EINVAL);
        }
        prev = e_end;
    }
    let mut a = base;
    while a < end {
        match os::query(a) {
            Some(r) if r.state == MEM_COMMIT && r.kind == MEM_MAPPED => a = r.base + r.size,
            _ => return Err(Errno::EINVAL),
        }
    }
    let mut g = lock(&REGION);
    if g.is_some() {
        return Err(Errno::EEXIST);
    }
    // Unmapped pages are inaccessible, as on Linux.
    os::protect(base, len, PAGE_NOACCESS)?;
    let mut runs = BTreeMap::new();
    runs.insert(
        base,
        Run {
            end,
            state: State::Unmapped,
            pristine: true,
        },
    );
    *g = Some(Region {
        base,
        end,
        extents,
        runs,
    });
    Ok(())
}

/// The registered region, `(base, end)`.
pub fn region() -> Option<(usize, usize)> {
    lock(&REGION).as_ref().map(|r| (r.base, r.end))
}

/// Windows protection of region pages for a Linux protection: writable
/// pages are copy-on-write.
fn view_protection(prot: u32) -> PAGE_PROTECTION_FLAGS {
    let r = prot & (PROT_READ | PROT_WRITE | PROT_EXEC) != 0;
    let w = prot & PROT_WRITE != 0;
    let x = prot & PROT_EXEC != 0;
    match (r, w, x) {
        (false, _, _) => PAGE_NOACCESS,
        (true, false, false) => PAGE_READONLY,
        (true, true, false) => PAGE_WRITECOPY,
        (true, false, true) => PAGE_EXECUTE_READ,
        (true, true, true) => PAGE_EXECUTE_WRITECOPY,
    }
}

fn page_round_up(n: usize) -> Option<usize> {
    n.checked_add(PAGE_SIZE - 1).map(|v| v & !(PAGE_SIZE - 1))
}

/// Splits `[start, end)` into the part inside `[base, rend)` and the parts
/// before and after it.
fn split(start: usize, end: usize, base: usize, rend: usize) -> Parts {
    let inside = (start.max(base), end.min(rend));
    Parts {
        before: (start < base).then(|| (start, end.min(base))),
        inside: (inside.0 < inside.1).then_some(inside),
        after: (end > rend).then(|| (start.max(rend), end)),
    }
}

struct Parts {
    before: Option<(usize, usize)>,
    inside: Option<(usize, usize)>,
    after: Option<(usize, usize)>,
}

impl Parts {
    fn outside(&self) -> impl Iterator<Item = (usize, usize)> {
        self.before.into_iter().chain(self.after)
    }
}

/// What a mapping in the region holds.
enum Source {
    Anon,
    File { fd: i32, offset: i64, shared: bool },
}

/// `mmap` when it concerns the region: `None` when it does not (the
/// caller handles it as usual).
pub(crate) fn mmap(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: i64,
) -> Option<SysResult<usize>> {
    let mut g = lock(&REGION);
    let region = g.as_mut()?;
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    if !fixed && addr == 0 {
        return None;
    }
    let size = page_round_up(len).filter(|l| *l > 0)?;
    let start = addr & !(PAGE_SIZE - 1);
    let end = start.checked_add(size)?;
    let parts = split(start, end, region.base, region.end);
    let (is, ie) = parts.inside?;
    if fixed && start != addr {
        return Some(Err(Errno::EINVAL));
    }
    let anon = flags & MAP_ANONYMOUS != 0;
    let source = if anon {
        Source::Anon
    } else {
        let shared = match flags & MAP_TYPE {
            MAP_PRIVATE => false,
            MAP_SHARED | MAP_SHARED_VALIDATE => true,
            _ => return Some(Err(Errno::EINVAL)),
        };
        if offset < 0 || !(offset as u64).is_multiple_of(PAGE_SIZE as u64) {
            return Some(Err(Errno::EINVAL));
        }
        Source::File { fd, offset, shared }
    };
    // A file mapping is placed only inside the region.
    if !anon && parts.outside().next().is_some() {
        return if fixed {
            Some(Err(Errno::ENOMEM))
        } else {
            None
        };
    }
    let noreplace = !fixed || flags & MAP_FIXED_NOREPLACE != 0;
    if noreplace && !region.all_unmapped(is, ie) {
        return if fixed {
            Some(Err(Errno::EEXIST))
        } else {
            // A hint that cannot be honored: placed elsewhere.
            None
        };
    }
    // The parts outside the region are ordinary memory.
    let mut outside_done: Vec<(usize, usize)> = Vec::new();
    for (s, e) in parts.outside() {
        let f = (flags & !(MAP_FIXED | MAP_FIXED_NOREPLACE))
            | if noreplace {
                MAP_FIXED_NOREPLACE
            } else {
                MAP_FIXED
            };
        match crate::mem::mmap_plain(s, e - s, prot, f) {
            Ok(_) => outside_done.push((s, e)),
            Err(err) => {
                for (s, e) in outside_done {
                    let _ = crate::mem::munmap_plain(s, e - s);
                }
                return if fixed { Some(Err(err)) } else { None };
            }
        }
    }
    // The file part starts at `offset` at `start`; inside the region it
    // starts at `is`.
    let source = match source {
        Source::File { fd, offset, shared } => Source::File {
            fd,
            offset: offset + (is - start) as i64,
            shared,
        },
        s => s,
    };
    match region.claim(is, ie, prot, &source) {
        Ok(()) => Some(Ok(start)),
        Err(e) => {
            for (s, e) in outside_done {
                let _ = crate::mem::munmap_plain(s, e - s);
            }
            let _ = region.unmap(is, ie);
            Some(Err(e))
        }
    }
}

/// `munmap`, `mprotect` and `madvise` on the part of `[start, end)` inside
/// the region; returns the parts outside it for the caller, or `None` when
/// the range does not touch the region.
pub(crate) fn munmap(start: usize, end: usize) -> Option<SysResult<Vec<(usize, usize)>>> {
    with_inside(start, end, |r, s, e| r.unmap(s, e))
}

pub(crate) fn mprotect(
    start: usize,
    end: usize,
    prot: u32,
) -> Option<SysResult<Vec<(usize, usize)>>> {
    with_inside(start, end, |r, s, e| r.protect(s, e, prot))
}

pub(crate) fn madvise(
    start: usize,
    end: usize,
    discard: bool,
) -> Option<SysResult<Vec<(usize, usize)>>> {
    with_inside(start, end, |r, s, e| r.advise(s, e, discard))
}

/// `mremap` of region pages is not supported.
pub(crate) fn touches(start: usize, end: usize) -> bool {
    lock(&REGION)
        .as_ref()
        .is_some_and(|r| start < r.end && end > r.base)
}

fn with_inside(
    start: usize,
    end: usize,
    f: impl FnOnce(&mut Region, usize, usize) -> SysResult<()>,
) -> Option<SysResult<Vec<(usize, usize)>>> {
    let mut g = lock(&REGION);
    let region = g.as_mut()?;
    let parts = split(start, end, region.base, region.end);
    let (is, ie) = parts.inside?;
    Some(f(region, is, ie).map(|()| parts.outside().collect()))
}

/// `/proc/self/maps` lines of the region's mappings:
/// `(start, end, prot)`, merged where equal.
pub(crate) fn mapped_runs() -> Vec<(usize, usize, u32)> {
    let g = lock(&REGION);
    let Some(r) = g.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<(usize, usize, u32)> = Vec::new();
    for (&s, run) in &r.runs {
        if let State::Mapped { prot, .. } = run.state {
            match out.last_mut() {
                Some(l) if l.1 == s && l.2 == prot => l.1 = run.end,
                _ => out.push((s, run.end, prot)),
            }
        }
    }
    out
}

impl Region {
    fn split_at(&mut self, a: usize) {
        let Some((&s, &run)) = self.runs.range(..a).next_back() else {
            return;
        };
        if run.end <= a {
            return;
        }
        self.runs.get_mut(&s).expect("run exists").end = a;
        self.runs.insert(a, run);
    }

    /// Makes `[s, e)` run boundaries and returns the runs inside.
    fn runs_in(&mut self, s: usize, e: usize) -> Vec<(usize, Run)> {
        self.split_at(s);
        self.split_at(e);
        self.runs.range(s..e).map(|(k, r)| (*k, *r)).collect()
    }

    fn set(&mut self, s: usize, e: usize, state: State, pristine: Option<bool>) {
        self.split_at(s);
        self.split_at(e);
        for (_, run) in self.runs.range_mut(s..e) {
            run.state = state;
            if let Some(p) = pristine {
                run.pristine = p;
            }
        }
        self.merge(s, e);
    }

    fn merge(&mut self, s: usize, e: usize) {
        let from = self.runs.range(..s).next_back().map_or(s, |(k, _)| *k);
        let keys: Vec<usize> = self.runs.range(from..=e).map(|(k, _)| *k).collect();
        let mut prev: Option<usize> = None;
        for k in keys {
            let Some(cur) = self.runs.get(&k).copied() else {
                continue;
            };
            if let Some(p) = prev {
                let pr = self.runs[&p];
                if pr.end == k && pr.state == cur.state && pr.pristine == cur.pristine {
                    self.runs.remove(&k);
                    self.runs.get_mut(&p).expect("run exists").end = cur.end;
                    continue;
                }
            }
            prev = Some(k);
        }
    }

    fn all_unmapped(&self, s: usize, e: usize) -> bool {
        let first = self.runs.range(..=s).next_back().map_or(s, |(k, _)| *k);
        self.runs
            .range(first..e)
            .filter(|(_, r)| r.end > s)
            .all(|(_, r)| r.state == State::Unmapped)
    }

    fn all_mapped(&self, s: usize, e: usize) -> bool {
        let first = self.runs.range(..=s).next_back().map_or(s, |(k, _)| *k);
        self.runs
            .range(first..e)
            .filter(|(_, r)| r.end > s)
            .all(|(_, r)| matches!(r.state, State::Mapped { .. }))
    }

    fn all_pristine(&self, s: usize, e: usize) -> bool {
        let first = self.runs.range(..=s).next_back().map_or(s, |(k, _)| *k);
        self.runs
            .range(first..e)
            .filter(|(_, r)| r.end > s)
            .all(|(_, r)| r.pristine)
    }

    /// Whether the view is zero in `[s, e)`: no extent overlaps it.
    fn view_is_zero(&self, s: usize, e: usize) -> bool {
        !self
            .extents
            .iter()
            .any(|x| x.addr < e && x.addr + x.len > s)
    }

    /// Whether `[s, e)` holds pages `[offset, ...)` of the file at `path`.
    fn view_holds(&self, path: &[u8], offset: u64, s: usize, e: usize) -> bool {
        let mut a = s;
        while a < e {
            let want = offset + (a - s) as u64;
            let Some(x) = self.extents.iter().find(|x| {
                x.path == path
                    && x.addr <= a
                    && a < x.addr + x.len
                    && x.offset + (a - x.addr) as u64 == want
            }) else {
                return false;
            };
            a = (x.addr + x.len).min(e);
        }
        true
    }

    /// Maps `[s, e)` (inside the region, page-aligned) with `prot`.
    fn claim(&mut self, s: usize, e: usize, prot: u32, source: &Source) -> SysResult<()> {
        match *source {
            Source::Anon => {
                let zero = self.view_is_zero(s, e) && self.all_pristine(s, e);
                if zero {
                    os::protect(s, e - s, view_protection(prot))?;
                    // Pages that are never writable keep the view's (zero)
                    // contents.
                    let pristine = prot & PROT_WRITE == 0;
                    self.set(s, e, mapped(prot, true, false), Some(pristine));
                } else if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0 {
                    os::protect(s, e - s, PAGE_NOACCESS)?;
                    self.set(s, e, mapped(prot, true, true), None);
                } else {
                    zero_fill(s, e)?;
                    os::protect(s, e - s, view_protection(prot))?;
                    self.set(s, e, mapped(prot, true, false), Some(false));
                }
                Ok(())
            }
            Source::File { fd, offset, shared } => {
                let file = crate::fd::FDS.get(fd)?;
                use crate::linux::fcntl::{O_RDONLY, O_RDWR};
                if !matches!(file.access(), O_RDONLY | O_RDWR) {
                    return Err(Errno::EACCES);
                }
                if !file.ops().mappable() {
                    return Err(Errno::ENODEV);
                }
                if shared && prot & PROT_WRITE != 0 {
                    return Err(if file.access() != O_RDWR {
                        Errno::EACCES
                    } else {
                        Errno::ENODEV
                    });
                }
                let path = file.ops().path().unwrap_or_default();
                if self.all_pristine(s, e)
                    && self.view_holds(&path, offset as u64, s, e)
                    && same_as_file(&file, offset, s, e)?
                {
                    os::protect(s, e - s, view_protection(prot))?;
                    let pristine = prot & PROT_WRITE == 0;
                    self.set(s, e, mapped(prot, false, false), Some(pristine));
                    lock(&STATS).adopted += e - s;
                    return Ok(());
                }
                // Filled from the file into private copies.
                os::protect(s, e - s, PAGE_WRITECOPY)?;
                let size = e - s;
                let mut done = 0usize;
                while done < size {
                    // SAFETY: the pages are writable (copy-on-write) view
                    // pages of the region, which lx owns; nothing else
                    // refers to them while they are being mapped.
                    let buf =
                        unsafe { crate::util::UserBuf::new((s + done) as *mut u8, size - done) };
                    match file.pread(buf, offset + done as i64)? {
                        0 => break,
                        n => done += n,
                    }
                }
                if done < size {
                    // SAFETY: as above.
                    unsafe { core::ptr::write_bytes((s + done) as *mut u8, 0, size - done) };
                }
                os::protect(s, e - s, view_protection(prot))?;
                self.set(s, e, mapped(prot, false, false), Some(false));
                lock(&STATS).copied += size;
                Ok(())
            }
        }
    }

    fn unmap(&mut self, s: usize, e: usize) -> SysResult<()> {
        os::protect(s, e - s, PAGE_NOACCESS)?;
        for (k, run) in self.runs_in(s, e) {
            let r = self.runs.get_mut(&k).expect("run exists");
            r.state = State::Unmapped;
            r.pristine = run.pristine;
        }
        self.merge(s, e);
        Ok(())
    }

    fn protect(&mut self, s: usize, e: usize, prot: u32) -> SysResult<()> {
        if !self.all_mapped(s, e) {
            return Err(Errno::ENOMEM);
        }
        let accessible = prot & (PROT_READ | PROT_WRITE | PROT_EXEC) != 0;
        for (k, run) in self.runs_in(s, e) {
            let State::Mapped {
                anon, zero_pending, ..
            } = run.state
            else {
                unreachable!("checked above")
            };
            if zero_pending && accessible {
                zero_fill(k, run.end)?;
            }
            os::protect(k, run.end - k, view_protection(prot))?;
            let r = self.runs.get_mut(&k).expect("run exists");
            r.state = mapped(prot, anon, zero_pending && !accessible);
            if prot & PROT_WRITE != 0 || (zero_pending && accessible) {
                r.pristine = false;
            }
        }
        self.merge(s, e);
        Ok(())
    }

    fn advise(&mut self, s: usize, e: usize, discard: bool) -> SysResult<()> {
        if !self.all_mapped(s, e) {
            return Err(Errno::ENOMEM);
        }
        if !discard {
            return Ok(());
        }
        let runs = self.runs_in(s, e);
        // Check everything before changing anything.
        for (_, run) in &runs {
            if let State::Mapped { anon: false, .. } = run.state
                && !run.pristine
            {
                return Err(Errno::EINVAL);
            }
        }
        for (k, run) in runs {
            let State::Mapped {
                prot,
                anon: true,
                zero_pending: false,
            } = run.state
            else {
                continue;
            };
            if run.pristine {
                continue;
            }
            if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0 {
                let r = self.runs.get_mut(&k).expect("run exists");
                r.state = mapped(prot, true, true);
                continue;
            }
            zero_fill(k, run.end)?;
            os::protect(k, run.end - k, view_protection(prot))?;
        }
        self.merge(s, e);
        Ok(())
    }
}

fn mapped(prot: u32, anon: bool, zero_pending: bool) -> State {
    State::Mapped {
        prot,
        anon,
        zero_pending,
    }
}

/// Zeroes `[s, e)` through copy-on-write (the shared view is unchanged).
fn zero_fill(s: usize, e: usize) -> SysResult<()> {
    os::protect(s, e - s, PAGE_WRITECOPY)?;
    // SAFETY: the pages are writable view pages of the region, which lx
    // owns; the mapping that holds them is being changed under the region
    // lock.
    unsafe { core::ptr::write_bytes(s as *mut u8, 0, e - s) };
    Ok(())
}

/// Compares the first and the last page of `[s, e)` with the file at
/// `offset`, a guard against a view built from other files.
fn same_as_file(file: &crate::fd::OpenFile, offset: i64, s: usize, e: usize) -> SysResult<bool> {
    let mut pages = vec![s];
    if e - s > PAGE_SIZE {
        pages.push(e - PAGE_SIZE);
    }
    os::protect(s, e - s, PAGE_READONLY)?;
    let mut buf = vec![0u8; PAGE_SIZE];
    for p in pages {
        buf.fill(0);
        let mut got = 0;
        while got < PAGE_SIZE {
            match file.pread(
                crate::util::UserBuf::from_mut(&mut buf[got..]),
                offset + (p - s + got) as i64,
            )? {
                0 => break,
                n => got += n,
            }
        }
        // SAFETY: the page is readable (just made read-only) and belongs to
        // the region's view, which stays mapped for the life of the
        // process.
        let view = unsafe { core::slice::from_raw_parts(p as *const u8, PAGE_SIZE) };
        if view != buf.as_slice() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_parts() {
        let p = split(0x1000, 0x9000, 0x4000, 0x6000);
        assert_eq!(p.before, Some((0x1000, 0x4000)));
        assert_eq!(p.inside, Some((0x4000, 0x6000)));
        assert_eq!(p.after, Some((0x6000, 0x9000)));
        let p = split(0x4000, 0x5000, 0x4000, 0x6000);
        assert!(p.before.is_none() && p.after.is_none());
        assert_eq!(p.inside, Some((0x4000, 0x5000)));
        let p = split(0x7000, 0x8000, 0x4000, 0x6000);
        assert!(p.inside.is_none());
    }

    #[test]
    fn protections() {
        assert_eq!(view_protection(PROT_NONE), PAGE_NOACCESS);
        assert_eq!(view_protection(PROT_READ), PAGE_READONLY);
        assert_eq!(view_protection(PROT_READ | PROT_WRITE), PAGE_WRITECOPY);
        assert_eq!(view_protection(PROT_READ | PROT_EXEC), PAGE_EXECUTE_READ);
        assert_eq!(
            view_protection(PROT_READ | PROT_WRITE | PROT_EXEC),
            PAGE_EXECUTE_WRITECOPY
        );
    }

    /// The whole life of a region in this test process: a view of a blob
    /// file holding one guest file's pages, mapped as the parent would.
    #[test]
    fn region_adopts_copies_and_zeroes() {
        use crate::fs::tests::test_dir;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Memory::{
            CreateFileMappingW, MEM_RELEASE, MEM_RESERVE, MapViewOfFile3, VIRTUAL_ALLOCATION_TYPE,
            VirtualAlloc, VirtualFree,
        };

        const LEN: usize = 0x20000;
        const AT: usize = 0x2000;
        let d = test_dir("image");
        let guest = format!("{d}/f");
        let mut data = vec![0u8; 3 * PAGE_SIZE];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 249) as u8 + 1;
        }
        use crate::linux::fcntl::{AT_FDCWD, O_CREAT, O_RDWR};
        let fd = crate::fs::openat(AT_FDCWD, guest.as_bytes(), O_CREAT | O_RDWR, 0o644).unwrap();
        assert_eq!(
            crate::fs::write(fd, crate::util::UserBuf::from_ref(&data)),
            Ok(data.len())
        );
        // The blob: the file's pages at AT, zeros elsewhere.
        let host_dir = std::env::temp_dir().join(format!("andruna-lx-test-{}", std::process::id()));
        let blob_path = host_dir.join("image").join("blob");
        let mut blob = vec![0u8; LEN];
        blob[AT..AT + data.len()].copy_from_slice(&data);
        const AT2: usize = 0x1_0000;
        blob[AT2..AT2 + data.len()].copy_from_slice(&data);
        std::fs::write(&blob_path, &blob).unwrap();
        let blob_file = std::fs::File::open(&blob_path).unwrap();
        // SAFETY: test-only Windows calls on handles and memory this test
        // owns: a section of the blob file, mapped at an address found free
        // by reserving and releasing it.
        let base = unsafe {
            let section = CreateFileMappingW(
                HANDLE(blob_file.as_raw_handle()),
                None,
                PAGE_WRITECOPY,
                0,
                0,
                None,
            )
            .unwrap();
            // Another test thread can take the probed range between the
            // release and the mapping, so probe again a few times.
            let mut mapped = None;
            for _ in 0..16 {
                let probe = VirtualAlloc(None, 2 * LEN, MEM_RESERVE, PAGE_NOACCESS);
                assert!(!probe.is_null());
                VirtualFree(probe, 0, MEM_RELEASE).unwrap();
                let v = MapViewOfFile3(
                    section,
                    None,
                    Some(probe),
                    0,
                    LEN,
                    VIRTUAL_ALLOCATION_TYPE(0),
                    PAGE_READONLY.0,
                    None,
                );
                if v.Value == probe {
                    mapped = Some(probe as usize);
                    break;
                }
                if !v.Value.is_null() {
                    windows::Win32::System::Memory::UnmapViewOfFile(v).unwrap();
                }
            }
            mapped.expect("view mapped where asked")
        };
        adopt(
            base,
            LEN,
            vec![
                Extent {
                    path: guest.as_bytes().to_vec(),
                    offset: 0,
                    addr: base + AT,
                    len: data.len(),
                },
                Extent {
                    path: guest.as_bytes().to_vec(),
                    offset: 0,
                    addr: base + AT2,
                    len: data.len(),
                },
            ],
        )
        .unwrap();
        assert_eq!(adopt(base, LEN, vec![]), Err(Errno::EEXIST));
        let read = |a: usize| -> u8 {
            // SAFETY: the test reads pages it mapped readable.
            unsafe { (a as *const u8).read_volatile() }
        };
        let write = |a: usize, v: u8| {
            // SAFETY: the test writes pages it mapped writable.
            unsafe { (a as *mut u8).write_volatile(v) }
        };
        let anon = MAP_PRIVATE | MAP_ANONYMOUS;
        // A reservation over the region and one more block after it.
        let total = LEN + 0x10000;
        assert_eq!(
            crate::mem::mmap(base, total, PROT_NONE, anon | MAP_FIXED_NOREPLACE, -1, 0),
            Ok(base)
        );
        assert_eq!(
            crate::mem::mmap(
                base,
                PAGE_SIZE,
                PROT_NONE,
                anon | MAP_FIXED_NOREPLACE,
                -1,
                0
            ),
            Err(Errno::EEXIST)
        );
        // The file at its place: adopted, nothing copied.
        let fixed = MAP_PRIVATE | MAP_FIXED;
        assert_eq!(
            crate::mem::mmap(base + AT, data.len(), PROT_READ, fixed, fd, 0),
            Ok(base + AT)
        );
        assert_eq!(stats().adopted, data.len());
        assert_eq!(read(base + AT + 5), data[5]);
        // Elsewhere in the region: filled from the file.
        assert_eq!(
            crate::mem::mmap(base + 0x8000, data.len(), PROT_READ, fixed, fd, 0),
            Ok(base + 0x8000)
        );
        assert_eq!(stats().copied, data.len());
        assert_eq!(read(base + 0x8000 + PAGE_SIZE + 7), data[PAGE_SIZE + 7]);
        // Anonymous memory where the view is zero: zero, writable.
        let rw = PROT_READ | PROT_WRITE;
        assert_eq!(
            crate::mem::mmap(base + 0x6000, PAGE_SIZE, rw, anon | MAP_FIXED, -1, 0),
            Ok(base + 0x6000)
        );
        assert_eq!(read(base + 0x6000), 0);
        write(base + 0x6000, 9);
        // Writing an adopted page changes this process's copy only.
        crate::mem::mprotect(base + AT, PAGE_SIZE, rw).unwrap();
        write(base + AT, 0xEE);
        assert_eq!(read(base + AT), 0xEE);
        assert_eq!(std::fs::read(&blob_path).unwrap()[AT], data[0]);
        // Writes elsewhere do not keep other pages from being adopted.
        let before = stats();
        assert_eq!(
            crate::mem::mmap(base + AT2, data.len(), PROT_READ, fixed, fd, 0),
            Ok(base + AT2)
        );
        assert_eq!(stats().adopted, before.adopted + data.len());
        assert_eq!(read(base + AT2 + 9), data[9]);
        // Mapped again after munmap: the written page is not adopted again.
        crate::mem::munmap(base + AT, data.len()).unwrap();
        let before = stats();
        assert_eq!(
            crate::mem::mmap(base + AT, data.len(), PROT_READ, fixed, fd, 0),
            Ok(base + AT)
        );
        assert_eq!(stats().copied, before.copied + data.len());
        assert_eq!(read(base + AT), data[0]);
        // Anonymous memory over file pages is zero.
        let p1 = base + AT + PAGE_SIZE;
        assert_eq!(
            crate::mem::mmap(p1, PAGE_SIZE, rw, anon | MAP_FIXED, -1, 0),
            Ok(p1)
        );
        assert_eq!(read(p1 + 1), 0);
        // An inaccessible anonymous mapping over file pages reads as zero
        // once it is made accessible.
        let p2 = base + AT + 2 * PAGE_SIZE;
        crate::mem::munmap(p2, PAGE_SIZE).unwrap();
        crate::mem::mmap(p2, PAGE_SIZE, PROT_NONE, anon | MAP_FIXED, -1, 0).unwrap();
        crate::mem::mprotect(p2, PAGE_SIZE, PROT_READ).unwrap();
        assert_eq!(read(p2 + 3), 0);
        // A range across the region's end: both parts change.
        crate::mem::mprotect(base + LEN - PAGE_SIZE, 2 * PAGE_SIZE, rw).unwrap();
        write(base + LEN - 1, 1);
        write(base + LEN, 2);
        assert_eq!((read(base + LEN - 1), read(base + LEN)), (1, 2));
        crate::mem::madvise(base + LEN - PAGE_SIZE, 2 * PAGE_SIZE, MADV_DONTNEED).unwrap();
        assert_eq!((read(base + LEN - 1), read(base + LEN)), (0, 0));
        assert!(crate::mem::maps().len() > 10);
        // mremap of region pages is refused.
        assert_eq!(
            crate::mem::mremap(base + 0x6000, PAGE_SIZE, 2 * PAGE_SIZE, 1),
            Err(Errno::EINVAL)
        );
        // Unmapping everything leaves the region unmapped (and the view in
        // place); the block after it is released.
        crate::mem::munmap(base, total).unwrap();
        assert_eq!(
            crate::mem::mprotect(base + 0x6000, PAGE_SIZE, PROT_READ),
            Err(Errno::ENOMEM)
        );
        assert_eq!(region(), Some((base, base + LEN)));
    }
}

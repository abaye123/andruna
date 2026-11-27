//! Anonymous memory: `mmap`, `munmap`, `mprotect`, `madvise`.
//!
//! # Model
//!
//! lx owns parts of the address space as *reservations*: ranges that start
//! on a 64 KiB boundary (the Windows allocation granularity) and are made of
//! whole 64 KiB blocks. Inside a reservation every page is in one *span*:
//!
//! - a placeholder (`MEM_RESERVE_PLACEHOLDER`): unmapped from the Linux
//!   point of view, free for lx to map at 4 KiB granularity;
//! - a mapping: a private allocation that replaced a placeholder
//!   (`MEM_REPLACE_PLACEHOLDER`). A `PROT_NONE` mapping that was never made
//!   accessible is reserved only and charges no commit, as Linux does not
//!   account `PROT_NONE` private mappings.
//!
//! Each placeholder span is exactly one Windows placeholder, and each mapped
//! span lies inside one Windows allocation, so every Windows call lx makes
//! covers ranges Windows accepts. `munmap` of part of a mapping releases that
//! part back to a placeholder (`MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER`),
//! which splits the Windows allocation; this was verified on Windows 11
//! build 26300 (see the tests) and gives true page-granular partial unmaps.
//! When no mapping is left in a reservation, it is returned to Windows.
//!
//! # Deviations from Linux
//!
//! - Memory lx does not own (Windows heaps, images, thread stacks) cannot be
//!   replaced by `MAP_FIXED` (`ENOMEM`) or unmapped by `munmap` (`EINVAL`).
//!   `mprotect` on it is passed to `VirtualProtect`.
//! - A fixed mapping in memory lx does not own yet needs the 64 KiB blocks
//!   around it to be free, since lx reserves whole blocks; otherwise it
//!   fails (`ENOMEM`, or `EEXIST` with `MAP_FIXED_NOREPLACE`).
//! - Writable private mappings are committed when created, so they count
//!   against the Windows commit limit at once (Linux overcommits).
//! - `madvise(MADV_DONTNEED)` decommits and recommits the pages. Another
//!   thread touching them during that window takes an access violation; the
//!   signal layer waits for the window to close and retries such faults
//!   ([`DISCARDS_ACTIVE`], [`DISCARDS_DONE`], `crate::signal`).
//! - File mappings are private memory filled from the file when the
//!   mapping is made (`pread`). For `MAP_PRIVATE` this is Linux behavior,
//!   except that later changes to the file are never seen (Linux leaves
//!   that unspecified) and pages wholly past the end of the file read as
//!   zero instead of raising `SIGBUS`. A read-only `MAP_SHARED` mapping is
//!   the same snapshot: writes to the file after the mapping was made are
//!   not seen, and `mprotect` to `PROT_WRITE` fails with `EACCES`.
//!   Writable `MAP_SHARED` file mappings fail with `ENODEV` (the error of a
//!   file system that cannot map files), and shared anonymous mappings
//!   with `ENOSYS`. Sharing views between processes (`MapViewOfFile3` with
//!   placeholders, `docs/spec/03-components.md`) is the next step.
//! - `MAP_32BIT` returns `ENOMEM` for now (TODO: `MEM_ADDRESS_REQUIREMENTS`).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use windows::Win32::System::Memory::{
    MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
    PAGE_READONLY, PAGE_READWRITE,
};

use crate::errno::{Errno, SysResult};
use crate::linux::mman::*;
use crate::sys::mem as os;
use crate::util::lock;

/// Windows allocation granularity.
pub const GRANULARITY: usize = 0x1_0000;
/// Lowest address a fixed mapping may use (Windows never maps the first
/// 64 KiB; Linux reports `EPERM` below `mmap_min_addr`).
const MIN_ADDR: usize = GRANULARITY;
/// End of the user address space on x64 and ARM64 Windows (128 TiB).
const USER_END: usize = 0x7fff_ffff_0000;

/// `MAP_32BIT` (x86_64 only).
const MAP_32BIT: u32 = 0x40;

const MADV_FREE: i32 = 8;
/// Advice values that only describe expected use and need no action.
const MADV_HINTS: &[i32] = &[
    MADV_NORMAL,
    MADV_RANDOM,
    MADV_SEQUENTIAL,
    MADV_WILLNEED,
    10, // MADV_DONTFORK
    11, // MADV_DOFORK
    12, // MADV_MERGEABLE
    13, // MADV_UNMERGEABLE
    14, // MADV_HUGEPAGE
    15, // MADV_NOHUGEPAGE
    16, // MADV_DONTDUMP
    17, // MADV_DODUMP
    // No process is ever forked, so there is no child copy to wipe or keep.
    18, // MADV_WIPEONFORK
    19, // MADV_KEEPONFORK
];

/// Translates Linux `PROT_*` bits to a Windows page protection.
pub fn page_protection(prot: u32) -> PAGE_PROTECTION_FLAGS {
    let r = prot & PROT_READ != 0;
    let w = prot & PROT_WRITE != 0;
    let x = prot & PROT_EXEC != 0;
    match (r, w, x) {
        (false, false, false) => PAGE_NOACCESS,
        (true, false, false) => PAGE_READONLY,
        // Write implies read on both architectures.
        (_, true, false) => PAGE_READWRITE,
        // Execute implies read on x86_64; Windows offers no execute-only
        // protection that user code can rely on.
        (_, false, true) => PAGE_EXECUTE_READ,
        (_, true, true) => PAGE_EXECUTE_READWRITE,
    }
}

fn page_round_up(n: usize) -> Option<usize> {
    n.checked_add(PAGE_SIZE - 1).map(|v| v & !(PAGE_SIZE - 1))
}

fn is_page_aligned(n: usize) -> bool {
    n.is_multiple_of(PAGE_SIZE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// One Windows placeholder, part of the reservation starting at
    /// `reservation`.
    Placeholder { reservation: usize },
    /// Part of the Windows private allocation starting at `alloc`.
    Mapped {
        alloc: usize,
        prot: u32,
        committed: bool,
        /// A read-only shared file mapping: `PROT_WRITE` is refused
        /// (`EACCES`), since writes would not reach the file.
        no_write: bool,
    },
}

#[derive(Clone, Copy, Debug)]
struct Span {
    end: usize,
    kind: Kind,
}

/// A piece of a queried range: owned by lx (a span) or not.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Own {
        start: usize,
        end: usize,
        kind: Kind,
    },
    Foreign {
        start: usize,
        end: usize,
    },
}

/// The address-space table of one process (see the module documentation).
#[derive(Debug, Default)]
pub struct AddressSpace {
    /// Spans keyed by start address; they never overlap.
    spans: BTreeMap<usize, Span>,
    /// Reservations, start to end.
    reservations: BTreeMap<usize, usize>,
}

static SPACE: Mutex<AddressSpace> = Mutex::new(AddressSpace::new());

/// `MADV_DONTNEED` windows (decommitted, not yet recommitted) open now.
pub(crate) static DISCARDS_ACTIVE: AtomicU32 = AtomicU32::new(0);
/// `MADV_DONTNEED` windows closed so far.
pub(crate) static DISCARDS_DONE: AtomicU64 = AtomicU64::new(0);

/// Marks a decommit-recommit window for the signal layer's fault retry.
struct DiscardWindow;

impl DiscardWindow {
    fn open() -> Self {
        DISCARDS_ACTIVE.fetch_add(1, Ordering::SeqCst);
        DiscardWindow
    }
}

impl Drop for DiscardWindow {
    fn drop(&mut self) {
        DISCARDS_DONE.fetch_add(1, Ordering::SeqCst);
        DISCARDS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `mmap` on the process address space.
pub fn mmap(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: i64,
) -> SysResult<usize> {
    let a = if flags & MAP_ANONYMOUS == 0 {
        map_file(addr, len, prot, flags, fd, offset)?
    } else {
        lock(&SPACE).mmap(addr, len, prot, flags, fd, offset)?
    };
    if let Some(l) = page_round_up(len) {
        clear_names(&mut lock(&NAMES), a, a + l);
        if flags & MAP_ANONYMOUS == 0
            && let Ok(f) = crate::fd::FDS.get(fd)
            && let Some(p) = f.ops().path()
        {
            lock(&NAMES).insert(a, (a + l, p));
        }
    }
    Ok(a)
}

/// A file mapping: private memory filled from the file (see the module
/// documentation).
fn map_file(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: i64,
) -> SysResult<usize> {
    let shared = match flags & MAP_TYPE {
        MAP_PRIVATE => false,
        MAP_SHARED | MAP_SHARED_VALIDATE => true,
        _ => return Err(Errno::EINVAL),
    };
    if offset < 0 || !(offset as u64).is_multiple_of(PAGE_SIZE as u64) {
        return Err(Errno::EINVAL);
    }
    let file = crate::fd::FDS.get(fd)?;
    if let Some(r) = file
        .ops()
        .device_mmap(&file, addr, len, prot, flags, offset)
    {
        return r;
    }
    use crate::linux::fcntl::{O_RDONLY, O_RDWR};
    if !matches!(file.access(), O_RDONLY | O_RDWR) {
        return Err(Errno::EACCES);
    }
    if !file.ops().mappable() {
        return Err(Errno::ENODEV);
    }
    if shared && prot & PROT_WRITE != 0 {
        if file.access() != O_RDWR {
            return Err(Errno::EACCES);
        }
        // Writable shared file mappings are not supported yet.
        return Err(Errno::ENODEV);
    }
    let anon = (flags & !MAP_TYPE & !MAP_SHARED_VALIDATE) | MAP_PRIVATE | MAP_ANONYMOUS;
    let start = lock(&SPACE).mmap(addr, len, PROT_READ | PROT_WRITE, anon, -1, 0)?;
    let size = page_round_up(len).ok_or(Errno::EINVAL)?;
    let mut done = 0usize;
    while done < size {
        // SAFETY: the new mapping is writable and nothing else refers to
        // it yet.
        let buf = unsafe { crate::util::UserBuf::new((start + done) as *mut u8, size - done) };
        match file.pread(buf, offset + done as i64) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) => {
                let _ = munmap(start, size);
                return Err(e);
            }
        }
    }
    let mut s = lock(&SPACE);
    if prot != PROT_READ | PROT_WRITE {
        s.mprotect(start, size, prot)?;
    }
    if shared {
        s.split_at(start)?;
        s.split_at(start + size)?;
        s.set_no_write(start, start + size);
    }
    Ok(start)
}

/// `munmap` on the process address space.
pub fn munmap(addr: usize, len: usize) -> SysResult<()> {
    lock(&SPACE).munmap(addr, len)?;
    if let Some(l) = page_round_up(len) {
        clear_names(&mut lock(&NAMES), addr, addr.saturating_add(l));
    }
    Ok(())
}

/// `mprotect` on the process address space.
pub fn mprotect(addr: usize, len: usize, prot: u32) -> SysResult<()> {
    lock(&SPACE).mprotect(addr, len, prot)
}

/// `mremap` flags.
pub const MREMAP_MAYMOVE: u32 = 1;
pub const MREMAP_FIXED: u32 = 2;

/// `mremap` on the process address space, for lx's anonymous memory:
/// shrinking unmaps the tail; growing extends in place when the pages after
/// the mapping are free, otherwise (with `MREMAP_MAYMOVE`) maps a new range
/// with the same protection, copies the contents and unmaps the old one.
/// `MREMAP_FIXED` and `MREMAP_DONTUNMAP` are not supported (`EINVAL`).
pub fn mremap(addr: usize, old_len: usize, new_len: usize, flags: u32) -> SysResult<usize> {
    if flags & !MREMAP_MAYMOVE != 0 || !is_page_aligned(addr) {
        return Err(Errno::EINVAL);
    }
    let old = page_round_up(old_len).ok_or(Errno::EINVAL)?;
    let new = match page_round_up(new_len) {
        Some(0) | None => return Err(Errno::EINVAL),
        Some(n) => n,
    };
    let mut s = lock(&SPACE);
    if old == 0 {
        // Duplicating a shared mapping; lx has none.
        return Err(Errno::EINVAL);
    }
    let prot = s.protection_of(addr, old).ok_or(Errno::EFAULT)?;
    if new <= old {
        if new < old {
            s.munmap(addr + new, old - new)?;
        }
        return Ok(addr);
    }
    let tail = addr.checked_add(old).ok_or(Errno::ENOMEM)?;
    let anon = MAP_PRIVATE | MAP_ANONYMOUS;
    if s.mmap(tail, new - old, prot, anon | MAP_FIXED_NOREPLACE, -1, 0)
        .is_ok()
    {
        return Ok(addr);
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return Err(Errno::ENOMEM);
    }
    let to = s.mmap(0, new, PROT_READ | PROT_WRITE, anon, -1, 0)?;
    if prot & PROT_READ == 0 {
        // Unreadable pages cannot be copied without changing them.
        let _ = s.munmap(to, new);
        return Err(Errno::EFAULT);
    }
    // SAFETY: both ranges are lx mappings held under the address-space
    // lock; the old one is readable and `old` bytes long, the new one is
    // writable and longer.
    unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, to as *mut u8, old) };
    if prot != PROT_READ | PROT_WRITE {
        s.mprotect(to, new, prot)?;
    }
    s.munmap(addr, old)?;
    Ok(to)
}

/// Names of mappings for `/proc/self/maps` (start -> (end, name)).
static NAMES: Mutex<BTreeMap<usize, (usize, Vec<u8>)>> = Mutex::new(BTreeMap::new());

/// Names `[addr, addr+len)` (`prctl(PR_SET_VMA_ANON_NAME)`, file paths).
/// An empty name removes it.
pub fn set_name(addr: usize, len: usize, name: &[u8]) -> SysResult<()> {
    let (start, end) = check_range(addr, len)?;
    if lock(&SPACE).require_mapped(start, end).is_err() {
        return Err(Errno::ENOMEM);
    }
    let mut n = lock(&NAMES);
    clear_names(&mut n, start, end);
    if !name.is_empty() {
        n.insert(start, (end, name.to_vec()));
    }
    Ok(())
}

/// Removes names in `[start, end)`, cutting names that overlap it.
fn clear_names(n: &mut BTreeMap<usize, (usize, Vec<u8>)>, start: usize, end: usize) {
    let hit: Vec<usize> = n
        .range(..end)
        .filter(|(_, (e, _))| *e > start)
        .map(|(s, _)| *s)
        .collect();
    for s in hit {
        let (e, name) = n.remove(&s).expect("present");
        if s < start {
            n.insert(s, (start, name.clone()));
        }
        if e > end {
            n.insert(end, (e, name));
        }
    }
}

/// `/proc/self/maps` for lx's mappings: one line per run of pages with
/// the same protection and name.
pub fn maps() -> Vec<u8> {
    let s = lock(&SPACE);
    let names = lock(&NAMES);
    let name_at = |a: usize| {
        names
            .range(..=a)
            .next_back()
            .filter(|(_, (e, _))| *e > a)
            .map(|(_, (_, n))| n.clone())
    };
    // (start, end, protection, shared, name)
    type Line = (usize, usize, u32, bool, Option<Vec<u8>>);
    let mut lines: Vec<Line> = Vec::new();
    for (&start, span) in &s.spans {
        let Kind::Mapped { prot, no_write, .. } = span.kind else {
            continue;
        };
        // Split at name boundaries inside the span.
        let mut cuts = vec![start, span.end];
        for (&ns, (ne, _)) in names.range(start..span.end) {
            cuts.push(ns);
            cuts.push((*ne).min(span.end));
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let name = name_at(a);
            match lines.last_mut() {
                Some(l) if l.1 == a && l.2 == prot && l.3 == no_write && l.4 == name => l.1 = b,
                _ => lines.push((a, b, prot, no_write, name)),
            }
        }
    }
    let mut out = String::new();
    for (a, b, prot, shared, name) in lines {
        let p = |bit: u32, c: char| if prot & bit != 0 { c } else { '-' };
        out.push_str(&format!(
            "{a:08x}-{b:08x} {}{}{}{} 00000000 00:00 0",
            p(PROT_READ, 'r'),
            p(PROT_WRITE, 'w'),
            p(PROT_EXEC, 'x'),
            if shared { 's' } else { 'p' },
        ));
        if let Some(n) = name {
            out.push_str("                          ");
            out.push_str(&String::from_utf8_lossy(&n));
        }
        out.push('\n');
    }
    out.into_bytes()
}

/// Whether lx has a mapping at `addr` (`None` if the table is busy, as it
/// can be when a fault happens while it is locked).
pub fn is_mapped(addr: usize) -> Option<bool> {
    let s = SPACE.try_lock().ok()?;
    Some(matches!(
        s.pieces(addr, addr + 1).first(),
        Some(Piece::Own {
            kind: Kind::Mapped { .. },
            ..
        })
    ))
}

/// `madvise` on the process address space.
pub fn madvise(addr: usize, len: usize, advice: i32) -> SysResult<()> {
    lock(&SPACE).madvise(addr, len, advice)
}

impl AddressSpace {
    pub const fn new() -> Self {
        Self {
            spans: BTreeMap::new(),
            reservations: BTreeMap::new(),
        }
    }

    pub fn mmap(
        &mut self,
        addr: usize,
        len: usize,
        prot: u32,
        flags: u32,
        _fd: i32,
        offset: i64,
    ) -> SysResult<usize> {
        if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC | PROT_SEM) != 0 {
            return Err(Errno::EINVAL);
        }
        if !(offset as u64).is_multiple_of(PAGE_SIZE as u64) {
            return Err(Errno::EINVAL);
        }
        let len = match page_round_up(len) {
            Some(0) | None => return Err(Errno::EINVAL),
            Some(l) if l > USER_END => return Err(Errno::ENOMEM),
            Some(l) => l,
        };
        match flags & MAP_TYPE {
            MAP_PRIVATE => {}
            // Shared anonymous memory (between processes after fork, or
            // through memfd) is not supported yet.
            MAP_SHARED | MAP_SHARED_VALIDATE => return Err(Errno::ENOSYS),
            _ => return Err(Errno::EINVAL),
        }
        if flags & MAP_ANONYMOUS == 0 {
            // File mappings go through `map_file`.
            return Err(Errno::EINVAL);
        }
        if flags & MAP_HUGETLB != 0 || cfg!(target_arch = "x86_64") && flags & MAP_32BIT != 0 {
            return Err(Errno::ENOMEM);
        }

        let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        if fixed {
            if !is_page_aligned(addr) {
                return Err(Errno::EINVAL);
            }
            let end = addr.checked_add(len).ok_or(Errno::ENOMEM)?;
            if end > USER_END {
                return Err(Errno::ENOMEM);
            }
            if addr < MIN_ADDR {
                return Err(Errno::EPERM);
            }
            let noreplace = flags & MAP_FIXED_NOREPLACE != 0;
            self.map_fixed(addr, end, prot, noreplace)?;
            return Ok(addr);
        }

        // A hint is used when the range is free, as on Linux.
        let hint = addr & !(PAGE_SIZE - 1);
        if hint >= MIN_ADDR
            && let Some(end) = hint.checked_add(len)
            && end <= USER_END
            && self.map_fixed(hint, end, prot, true).is_ok()
        {
            return Ok(hint);
        }

        let start = match self.find_free(len) {
            Some(s) => s,
            None => self.new_reservation(None, len.next_multiple_of(GRANULARITY))?,
        };
        if let Err(e) = self.map_placeholders(start, start + len, prot) {
            let _ = self.unmap_range(start, start + len);
            self.collect_reservations(start, start + len);
            return Err(e);
        }
        Ok(start)
    }

    pub fn munmap(&mut self, addr: usize, len: usize) -> SysResult<()> {
        let (start, end) = check_range(addr, len)?;
        if start == end {
            return Err(Errno::EINVAL);
        }
        for p in self.pieces(start, end) {
            if let Piece::Foreign { start, end } = p
                && !is_free(start, end)
            {
                return Err(Errno::EINVAL);
            }
        }
        let r = self.unmap_range(start, end);
        self.collect_reservations(start, end);
        r
    }

    pub fn mprotect(&mut self, addr: usize, len: usize, prot: u32) -> SysResult<()> {
        if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC | PROT_SEM) != 0 {
            // PROT_GROWSDOWN/PROT_GROWSUP need a grows-down mapping, which
            // lx does not create.
            return Err(Errno::EINVAL);
        }
        let (start, end) = check_range(addr, len)?;
        let pieces = self.require_mapped(start, end)?;
        if prot & PROT_WRITE != 0
            && pieces.iter().any(|p| {
                matches!(
                    p,
                    Piece::Own {
                        kind: Kind::Mapped { no_write: true, .. },
                        ..
                    }
                )
            })
        {
            return Err(Errno::EACCES);
        }
        let page_prot = page_protection(prot);
        for p in pieces {
            match p {
                Piece::Own { start, end, kind } => {
                    let Kind::Mapped { committed, .. } = kind else {
                        unreachable!("require_mapped returned a placeholder")
                    };
                    self.split_at(start)?;
                    self.split_at(end)?;
                    let make_accessible = prot != PROT_NONE && !committed;
                    if make_accessible {
                        os::commit(start, end - start, page_prot)?;
                    } else if committed {
                        os::protect(start, end - start, page_prot)?;
                    }
                    self.set_mapped(start, end, prot, committed || make_accessible);
                }
                Piece::Foreign { start, end } => os::protect(start, end - start, page_prot)?,
            }
        }
        self.merge_around(start, end);
        Ok(())
    }

    pub fn madvise(&mut self, addr: usize, len: usize, advice: i32) -> SysResult<()> {
        let (start, end) = check_range(addr, len)?;
        let discard = match advice {
            MADV_DONTNEED | MADV_FREE => true,
            a if MADV_HINTS.contains(&a) => false,
            _ => return Err(Errno::EINVAL),
        };
        let pieces = self.require_mapped(start, end)?;
        if !discard {
            return Ok(());
        }
        // Check everything before changing anything.
        if pieces.iter().any(|p| matches!(p, Piece::Foreign { .. })) {
            // Not lx's anonymous memory; zero-filling it is not possible.
            return Err(Errno::EINVAL);
        }
        for p in pieces {
            if let Piece::Own {
                start,
                end,
                kind: Kind::Mapped {
                    prot, committed, ..
                },
            } = p
            {
                if !committed {
                    continue;
                }
                let _window = DiscardWindow::open();
                os::decommit(start, end - start)?;
                if prot == PROT_NONE {
                    // Stays reserved; the next mprotect commits zero pages.
                    self.split_at(start)?;
                    self.split_at(end)?;
                    self.set_mapped(start, end, prot, false);
                } else {
                    os::commit(start, end - start, page_protection(prot))?;
                }
            }
        }
        self.merge_around(start, end);
        Ok(())
    }

    /// Marks the mapped spans of `[start, end)` as read-only shared file
    /// mappings.
    fn set_no_write(&mut self, start: usize, end: usize) {
        for (_, span) in self.spans.range_mut(start..end) {
            if let Kind::Mapped { no_write, .. } = &mut span.kind {
                *no_write = true;
            }
        }
    }

    /// Whether `[addr, addr+len)` is entirely mapped by lx, and with what
    /// protection if it is uniform. For tests and diagnostics.
    pub fn protection_of(&self, addr: usize, len: usize) -> Option<u32> {
        let mut prot = None;
        for p in self.pieces(addr, addr + len) {
            match p {
                Piece::Own {
                    kind: Kind::Mapped { prot: q, .. },
                    ..
                } => {
                    if prot.is_some_and(|p| p != q) {
                        return None;
                    }
                    prot = Some(q);
                }
                _ => return None,
            }
        }
        prot
    }

    // ---- internals -------------------------------------------------------

    /// Maps `[start, end)` with a fixed address. With `noreplace`, fails
    /// with `EEXIST` if anything is mapped there.
    fn map_fixed(&mut self, start: usize, end: usize, prot: u32, noreplace: bool) -> SysResult<()> {
        let occupied = |p: &Piece| match p {
            Piece::Own { kind, .. } => matches!(kind, Kind::Mapped { .. }),
            Piece::Foreign { start, end } => !is_free(*start, *end),
        };
        let pieces = self.pieces(start, end);
        if noreplace && pieces.iter().any(occupied) {
            return Err(Errno::EEXIST);
        }
        if pieces
            .iter()
            .any(|p| matches!(p, Piece::Foreign { .. }) && occupied(p))
        {
            // Replacing memory lx does not own would break its owner.
            return Err(Errno::ENOMEM);
        }

        // Make the whole range lx-owned: reserve the 64 KiB blocks around
        // the foreign (free) pieces.
        let mut created = Vec::new();
        for p in pieces {
            if let Piece::Foreign { start: s, end: e } = p {
                let rs = s & !(GRANULARITY - 1);
                let re = e.next_multiple_of(GRANULARITY);
                match self.new_reservation(Some(rs), re - rs) {
                    Ok(_) => created.push(rs),
                    Err(_) => {
                        for r in created {
                            self.drop_reservation(r);
                        }
                        return Err(if noreplace {
                            Errno::EEXIST
                        } else {
                            Errno::ENOMEM
                        });
                    }
                }
            }
        }

        // Linux replaces existing mappings in the range.
        self.unmap_range(start, end)?;
        if let Err(e) = self.map_placeholders(start, end, prot) {
            let _ = self.unmap_range(start, end);
            self.collect_reservations(start, end);
            return Err(e);
        }
        Ok(())
    }

    /// Replaces the placeholders that make up `[start, end)` with mappings.
    fn map_placeholders(&mut self, start: usize, end: usize, prot: u32) -> SysResult<()> {
        self.split_at(start)?;
        self.split_at(end)?;
        let keys: Vec<usize> = self.spans.range(start..end).map(|(k, _)| *k).collect();
        let commit = (prot != PROT_NONE).then(|| page_protection(prot));
        for s in keys {
            let span = self.spans[&s];
            debug_assert!(matches!(span.kind, Kind::Placeholder { .. }));
            os::replace_placeholder(s, span.end - s, commit)?;
            self.spans.insert(
                s,
                Span {
                    end: span.end,
                    kind: Kind::Mapped {
                        alloc: s,
                        prot,
                        committed: commit.is_some(),
                        no_write: false,
                    },
                },
            );
        }
        Ok(())
    }

    /// Turns every mapped span in `[start, end)` back into placeholders.
    /// Reservations are kept; see [`Self::collect_reservations`].
    fn unmap_range(&mut self, start: usize, end: usize) -> SysResult<()> {
        let first = self
            .spans
            .range(..end)
            .next_back()
            .filter(|(_, s)| s.end > start)
            .map(|(k, _)| *k);
        if first.is_none() {
            return Ok(());
        }
        self.split_at(start)?;
        self.split_at(end)?;
        let keys: Vec<usize> = self.spans.range(start..end).map(|(k, _)| *k).collect();
        for s in keys {
            let span = self.spans[&s];
            let Kind::Mapped { alloc, .. } = span.kind else {
                continue;
            };
            os::release_to_placeholder(s, span.end - s)?;
            let reservation = self.reservation_of(s);
            self.spans.insert(
                s,
                Span {
                    end: span.end,
                    kind: Kind::Placeholder { reservation },
                },
            );
            // Windows split the allocation: the part after this span is now
            // an allocation of its own, starting at `span.end`.
            let mut next = span.end;
            while let Some(n) = self.spans.get_mut(&next) {
                match &mut n.kind {
                    Kind::Mapped { alloc: a, .. } if *a == alloc => {
                        *a = span.end;
                        next = n.end;
                    }
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// Returns to Windows every reservation overlapping `[start, end)` that
    /// no longer holds a mapping.
    fn collect_reservations(&mut self, start: usize, end: usize) {
        let candidates: Vec<usize> = self
            .reservations
            .range(..end)
            .rev()
            .take_while(|(_, e)| **e > start)
            .map(|(s, _)| *s)
            .collect();
        for r in candidates {
            let rend = self.reservations[&r];
            let in_use = self
                .spans
                .range(r..rend)
                .any(|(_, s)| matches!(s.kind, Kind::Mapped { .. }));
            if !in_use {
                self.drop_reservation(r);
            }
        }
    }

    /// Releases an unused reservation (all its spans are placeholders).
    fn drop_reservation(&mut self, r: usize) {
        let Some(rend) = self.reservations.remove(&r) else {
            return;
        };
        let keys: Vec<usize> = self.spans.range(r..rend).map(|(k, _)| *k).collect();
        for k in keys {
            self.spans.remove(&k);
            // Each placeholder is released on its own. A failure leaves an
            // unused placeholder behind, which is harmless.
            let _ = os::release(k);
        }
    }

    fn new_reservation(&mut self, at: Option<usize>, size: usize) -> SysResult<usize> {
        debug_assert!(size.is_multiple_of(GRANULARITY));
        let start = os::reserve_placeholder(at, size)?;
        debug_assert!(start.is_multiple_of(GRANULARITY));
        self.reservations.insert(start, start + size);
        self.spans.insert(
            start,
            Span {
                end: start + size,
                kind: Kind::Placeholder { reservation: start },
            },
        );
        Ok(start)
    }

    fn reservation_of(&self, addr: usize) -> usize {
        let (s, e) = self
            .reservations
            .range(..=addr)
            .next_back()
            .expect("span outside any reservation");
        debug_assert!(addr < *e);
        *s
    }

    /// First run of adjacent placeholders at least `len` bytes long.
    fn find_free(&self, len: usize) -> Option<usize> {
        let mut run: Option<(usize, usize)> = None;
        for (s, span) in &self.spans {
            if let Kind::Placeholder { .. } = span.kind {
                run = match run {
                    Some((rs, re)) if re == *s => Some((rs, span.end)),
                    _ => Some((*s, span.end)),
                };
                if let Some((rs, re)) = run
                    && re - rs >= len
                {
                    return Some(rs);
                }
            } else {
                run = None;
            }
        }
        None
    }

    /// Makes `addr` a span boundary, splitting a span if needed (a
    /// placeholder is split in Windows too).
    fn split_at(&mut self, addr: usize) -> SysResult<()> {
        let Some((&s, &span)) = self.spans.range(..addr).next_back() else {
            return Ok(());
        };
        if span.end <= addr {
            return Ok(());
        }
        if let Kind::Placeholder { .. } = span.kind {
            os::split_placeholder(s, addr - s)?;
        }
        self.spans.get_mut(&s).expect("span exists").end = addr;
        self.spans.insert(
            addr,
            Span {
                end: span.end,
                kind: span.kind,
            },
        );
        Ok(())
    }

    /// Sets the attributes of the mapped spans in `[start, end)`, which must
    /// be span boundaries.
    fn set_mapped(&mut self, start: usize, end: usize, prot: u32, committed: bool) {
        for (_, span) in self.spans.range_mut(start..end) {
            if let Kind::Mapped {
                prot: p,
                committed: c,
                ..
            } = &mut span.kind
            {
                *p = prot;
                *c = committed;
            }
        }
    }

    /// Merges equal mapped spans of one allocation around `[start, end)` so
    /// that repeated `mprotect` calls do not fragment the table.
    fn merge_around(&mut self, start: usize, end: usize) {
        let from = self
            .spans
            .range(..start)
            .next_back()
            .map_or(start, |(k, _)| *k);
        let keys: Vec<usize> = self.spans.range(from..=end).map(|(k, _)| *k).collect();
        let mut prev: Option<usize> = None;
        for k in keys {
            let Some(cur) = self.spans.get(&k).copied() else {
                continue;
            };
            if let Some(p) = prev {
                let ps = self.spans[&p];
                if ps.end == k && ps.kind == cur.kind && matches!(cur.kind, Kind::Mapped { .. }) {
                    self.spans.remove(&k);
                    self.spans.get_mut(&p).expect("span exists").end = cur.end;
                    continue;
                }
            }
            prev = Some(k);
        }
    }

    /// The pieces of `[start, end)`: lx spans (clipped) and the gaps between
    /// them.
    fn pieces(&self, start: usize, end: usize) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut pos = start;
        let first = self
            .spans
            .range(..=start)
            .next_back()
            .map_or(start, |(k, _)| *k);
        for (&s, span) in self.spans.range(first..end) {
            if span.end <= pos {
                continue;
            }
            if s > pos {
                out.push(Piece::Foreign { start: pos, end: s });
            }
            let ps = s.max(pos);
            let pe = span.end.min(end);
            out.push(Piece::Own {
                start: ps,
                end: pe,
                kind: span.kind,
            });
            pos = pe;
        }
        if pos < end {
            out.push(Piece::Foreign { start: pos, end });
        }
        out
    }

    /// The pieces of `[start, end)` if all of it is mapped (lx mappings or
    /// committed foreign memory), `ENOMEM` otherwise, as Linux reports for
    /// unmapped pages.
    fn require_mapped(&self, start: usize, end: usize) -> SysResult<Vec<Piece>> {
        let pieces = self.pieces(start, end);
        for p in &pieces {
            let ok = match p {
                Piece::Own { kind, .. } => matches!(kind, Kind::Mapped { .. }),
                Piece::Foreign { start, end } => is_committed(*start, *end),
            };
            if !ok {
                return Err(Errno::ENOMEM);
            }
        }
        Ok(pieces)
    }
}

/// Validates an `munmap`/`mprotect`/`madvise` range and returns it rounded
/// to pages.
fn check_range(addr: usize, len: usize) -> SysResult<(usize, usize)> {
    if !is_page_aligned(addr) {
        return Err(Errno::EINVAL);
    }
    let len = page_round_up(len).ok_or(Errno::ENOMEM)?;
    let end = addr.checked_add(len).ok_or(Errno::ENOMEM)?;
    if end > USER_END {
        return Err(Errno::ENOMEM);
    }
    Ok((addr, end))
}

/// Whether every page of `[start, end)` is free in Windows.
fn is_free(start: usize, end: usize) -> bool {
    region_all(start, end, |r| r.state.0 == 0x10000 /* MEM_FREE */)
}

/// Whether every page of `[start, end)` is committed in Windows.
fn is_committed(start: usize, end: usize) -> bool {
    region_all(start, end, |r| r.state == MEM_COMMIT)
}

fn region_all(start: usize, end: usize, f: impl Fn(&os::Region) -> bool) -> bool {
    let mut a = start;
    while a < end {
        match os::query(a) {
            Some(r) if f(&r) => a = r.base + r.size,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const RW: u32 = PROT_READ | PROT_WRITE;
    const ANON: u32 = MAP_PRIVATE | MAP_ANONYMOUS;

    fn write(addr: usize, v: u8) {
        // SAFETY: tests only write to pages they mapped read-write.
        unsafe { (addr as *mut u8).write_volatile(v) }
    }

    fn read(addr: usize) -> u8 {
        // SAFETY: tests only read pages they mapped readable.
        unsafe { (addr as *const u8).read_volatile() }
    }

    fn state(addr: usize) -> (u32, u32) {
        let r = os::query(addr).unwrap();
        (r.state.0, r.protect.0)
    }

    #[test]
    fn protection_table() {
        assert_eq!(page_protection(PROT_NONE), PAGE_NOACCESS);
        assert_eq!(page_protection(PROT_READ), PAGE_READONLY);
        assert_eq!(page_protection(PROT_WRITE), PAGE_READWRITE);
        assert_eq!(page_protection(RW), PAGE_READWRITE);
        assert_eq!(page_protection(PROT_EXEC), PAGE_EXECUTE_READ);
        assert_eq!(page_protection(PROT_READ | PROT_EXEC), PAGE_EXECUTE_READ);
        assert_eq!(page_protection(RW | PROT_EXEC), PAGE_EXECUTE_READWRITE);
    }

    #[test]
    fn anonymous_mapping_is_zeroed_and_writable() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 3 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        assert!(is_page_aligned(a));
        for i in 0..3 {
            assert_eq!(read(a + i * PAGE_SIZE), 0);
            write(a + i * PAGE_SIZE, i as u8 + 1);
        }
        assert_eq!(s.protection_of(a, 3 * PAGE_SIZE), Some(RW));
        s.munmap(a, 3 * PAGE_SIZE).unwrap();
        assert!(s.spans.is_empty() && s.reservations.is_empty());
        assert_eq!(state(a).0, 0x10000, "reservation returned to Windows");
    }

    #[test]
    fn argument_errors() {
        let mut s = AddressSpace::new();
        assert_eq!(s.mmap(0, 0, RW, ANON, -1, 0), Err(Errno::EINVAL));
        assert_eq!(
            s.mmap(0, 4096, RW, MAP_ANONYMOUS, -1, 0),
            Err(Errno::EINVAL)
        );
        assert_eq!(s.mmap(0, 4096, RW, ANON, -1, 1), Err(Errno::EINVAL));
        assert_eq!(s.mmap(0, 4096, 0x10, ANON, -1, 0), Err(Errno::EINVAL));
        assert_eq!(
            s.mmap(0x1001, 4096, RW, ANON | MAP_FIXED, -1, 0),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            s.mmap(0x1000, 4096, RW, ANON | MAP_FIXED, -1, 0),
            Err(Errno::EPERM)
        );
        assert_eq!(
            s.mmap(0, 4096, RW, MAP_SHARED | MAP_ANONYMOUS, -1, 0),
            Err(Errno::ENOSYS)
        );
        // File mappings go through `mmap`, not the table.
        assert_eq!(s.mmap(0, 4096, RW, MAP_PRIVATE, 3, 0), Err(Errno::EINVAL));
        assert_eq!(s.munmap(0x10001, 4096), Err(Errno::EINVAL));
        assert_eq!(s.munmap(0x10000, 0), Err(Errno::EINVAL));
        assert_eq!(s.mprotect(0x10001, 4096, RW), Err(Errno::EINVAL));
        assert_eq!(s.madvise(0x10000, 4096, 1234), Err(Errno::EINVAL));
        assert_eq!(
            s.mmap(0, usize::MAX - 10, RW, ANON, -1, 0),
            Err(Errno::EINVAL)
        );
        assert_eq!(s.mmap(0, 1 << 60, RW, ANON, -1, 0), Err(Errno::ENOMEM));
    }

    #[test]
    fn partial_unmap_keeps_the_rest() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 4 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        for i in 0..4 {
            write(a + i * PAGE_SIZE, 10 + i as u8);
        }
        // Hole in the middle, then the first page.
        s.munmap(a + PAGE_SIZE, PAGE_SIZE).unwrap();
        // Placeholders show as reserved.
        assert_eq!(state(a + PAGE_SIZE).0, 0x2000, "placeholder again");
        assert_eq!(read(a), 10);
        assert_eq!(read(a + 2 * PAGE_SIZE), 12);
        assert_eq!(read(a + 3 * PAGE_SIZE), 13);
        s.munmap(a, PAGE_SIZE).unwrap();
        assert_eq!(read(a + 3 * PAGE_SIZE), 13);
        // Protection change across what is now a separate allocation.
        s.mprotect(a + 2 * PAGE_SIZE, 2 * PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(state(a + 3 * PAGE_SIZE).1, PAGE_READONLY.0);
        // Unmapping a range with holes in it is fine.
        s.munmap(a, 4 * PAGE_SIZE).unwrap();
        assert!(s.spans.is_empty());
    }

    #[test]
    fn small_mappings_share_a_reservation() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        let b = s.mmap(0, PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        assert_eq!(b, a + PAGE_SIZE);
        assert_eq!(s.reservations.len(), 1);
        s.munmap(a, PAGE_SIZE).unwrap();
        // The freed page is reused first.
        let c = s.mmap(0, PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        assert_eq!(c, a);
        s.munmap(a, 2 * PAGE_SIZE).unwrap();
        assert!(s.reservations.is_empty());
    }

    #[test]
    fn fixed_mappings() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 4 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        write(a + PAGE_SIZE, 7);
        write(a + 2 * PAGE_SIZE, 8);
        // MAP_FIXED over an existing mapping replaces it with zero pages.
        let r = s
            .mmap(a + PAGE_SIZE, PAGE_SIZE, PROT_READ, ANON | MAP_FIXED, -1, 0)
            .unwrap();
        assert_eq!(r, a + PAGE_SIZE);
        assert_eq!(read(a + PAGE_SIZE), 0);
        assert_eq!(read(a + 2 * PAGE_SIZE), 8);
        assert_eq!(state(a + PAGE_SIZE).1, PAGE_READONLY.0);
        // MAP_FIXED_NOREPLACE over it fails.
        assert_eq!(
            s.mmap(a, PAGE_SIZE, RW, ANON | MAP_FIXED_NOREPLACE, -1, 0),
            Err(Errno::EEXIST)
        );
        s.munmap(a, 4 * PAGE_SIZE).unwrap();
        assert_eq!(state(a).0, 0x10000);

        // MAP_FIXED into free memory at a page (not 64 KiB) boundary.
        let f = a + 3 * PAGE_SIZE;
        let r = match s.mmap(f, 2 * PAGE_SIZE, RW, ANON | MAP_FIXED_NOREPLACE, -1, 0) {
            Ok(r) => r,
            // Another test thread mapped the block that was just freed:
            // nothing left to check here.
            Err(Errno::EEXIST) => return,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(r, f);
        write(f + PAGE_SIZE, 1);
        assert_eq!(state(f - PAGE_SIZE).0, 0x2000, "rest stays placeholder");
        s.munmap(f, 2 * PAGE_SIZE).unwrap();
        assert!(s.reservations.is_empty());
    }

    #[test]
    fn hint_is_used_when_free() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        s.munmap(a, PAGE_SIZE).unwrap();
        let b = s
            .mmap(a + 5 * PAGE_SIZE, PAGE_SIZE, RW, ANON, -1, 0)
            .unwrap();
        assert_eq!(b, a + 5 * PAGE_SIZE);
        // A hint on occupied memory is ignored.
        let c = s.mmap(b, PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        assert_ne!(c, b);
        s.munmap(b, PAGE_SIZE).unwrap();
        s.munmap(c, PAGE_SIZE).unwrap();
        assert!(s.reservations.is_empty());
    }

    #[test]
    fn fixed_over_foreign_memory_is_refused() {
        let mut s = AddressSpace::new();
        let v = vec![0u8; 1 << 20];
        let p = (v.as_ptr() as usize).next_multiple_of(PAGE_SIZE);
        assert_eq!(
            s.mmap(p, PAGE_SIZE, RW, ANON | MAP_FIXED, -1, 0),
            Err(Errno::ENOMEM)
        );
        assert_eq!(
            s.mmap(p, PAGE_SIZE, RW, ANON | MAP_FIXED_NOREPLACE, -1, 0),
            Err(Errno::EEXIST)
        );
        assert_eq!(s.munmap(p, PAGE_SIZE), Err(Errno::EINVAL));
        // mprotect on committed foreign memory is passed through.
        s.mprotect(p, PAGE_SIZE, PROT_READ).unwrap();
        s.mprotect(p, PAGE_SIZE, RW).unwrap();
        drop(v);
    }

    #[test]
    fn prot_none_is_reserved_until_made_accessible() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 2 * PAGE_SIZE, PROT_NONE, ANON, -1, 0).unwrap();
        assert_eq!(state(a).0, 0x2000, "reserved, not committed");
        s.mprotect(a, PAGE_SIZE, RW).unwrap();
        assert_eq!(state(a).0, 0x1000);
        write(a, 5);
        // Back to PROT_NONE keeps the data, then readable again.
        s.mprotect(a, PAGE_SIZE, PROT_NONE).unwrap();
        assert_eq!(state(a).1, PAGE_NOACCESS.0);
        s.mprotect(a, PAGE_SIZE, PROT_READ).unwrap();
        assert_eq!(read(a), 5);
        assert_eq!(s.protection_of(a, PAGE_SIZE), Some(PROT_READ));
        assert_eq!(s.protection_of(a, 2 * PAGE_SIZE), None);
        s.munmap(a, 2 * PAGE_SIZE).unwrap();
    }

    #[test]
    fn mprotect_and_madvise_need_mapped_pages() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 2 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        s.munmap(a + PAGE_SIZE, PAGE_SIZE).unwrap();
        assert_eq!(s.mprotect(a, 2 * PAGE_SIZE, PROT_READ), Err(Errno::ENOMEM));
        assert_eq!(
            s.madvise(a, 2 * PAGE_SIZE, MADV_DONTNEED),
            Err(Errno::ENOMEM)
        );
        assert_eq!(s.mprotect(a, PAGE_SIZE, PROT_GROWSDOWN), Err(Errno::EINVAL));
        s.munmap(a, PAGE_SIZE).unwrap();
    }

    #[test]
    fn dontneed_zeroes_pages() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 3 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        for i in 0..3 {
            write(a + i * PAGE_SIZE, 0x5a);
        }
        s.madvise(a + PAGE_SIZE, PAGE_SIZE, MADV_DONTNEED).unwrap();
        assert_eq!(read(a), 0x5a);
        assert_eq!(read(a + PAGE_SIZE), 0);
        assert_eq!(read(a + 2 * PAGE_SIZE), 0x5a);
        write(a + PAGE_SIZE, 1);
        s.madvise(a, PAGE_SIZE, MADV_WILLNEED).unwrap();
        assert_eq!(read(a), 0x5a);
        s.munmap(a, 3 * PAGE_SIZE).unwrap();
    }

    #[test]
    fn repeated_mprotect_does_not_fragment() {
        let mut s = AddressSpace::new();
        let a = s.mmap(0, 8 * PAGE_SIZE, RW, ANON, -1, 0).unwrap();
        for i in 0..8 {
            s.mprotect(a + i * PAGE_SIZE, PAGE_SIZE, PROT_READ).unwrap();
        }
        for i in 0..8 {
            s.mprotect(a + i * PAGE_SIZE, PAGE_SIZE, RW).unwrap();
        }
        assert_eq!(s.spans.range(a..a + 8 * PAGE_SIZE).count(), 1);
        s.munmap(a, 8 * PAGE_SIZE).unwrap();
    }

    #[test]
    fn large_mapping_spans_reservations() {
        let mut s = AddressSpace::new();
        let len = 3 * GRANULARITY + PAGE_SIZE;
        let a = s.mmap(0, len, RW, ANON, -1, 0).unwrap();
        write(a + len - 1, 1);
        s.munmap(a + GRANULARITY, GRANULARITY).unwrap();
        assert_eq!(read(a + len - 1), 1);
        s.munmap(a, len).unwrap();
        assert!(s.reservations.is_empty());
    }
}

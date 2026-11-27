//! Windows primitives for a `userfaultfd` substitute, as ART's concurrent
//! mark-compact (CMC) collector would need, and baselines to compare with.
//!
//! The model of one CMC compaction cycle on Windows:
//!
//! - The moving space lives in a pagefile-backed section mapped at the heap
//!   address H (inside an lx placeholder).
//! - "mremap(MREMAP_DONTUNMAP)" substitute (in the pause): map a second view
//!   of the old section elsewhere (the from-space F), replace the view at H
//!   by a view of a fresh section (the to-space) with no access, and map an
//!   alias view A of the fresh section that only the collector writes.
//! - "UFFDIO_COPY" substitute: compute a to-space page into A, then
//!   `VirtualProtect` the page at H to read-write. Mutators never see a
//!   partly filled page: until the protection changes they fault.
//! - "SIGBUS on a missing page" substitute: a vectored exception handler
//!   that claims the page, fills it (or waits for the collector to finish
//!   it) and resumes the faulting instruction.

use std::cell::Cell;
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering::*};
use std::sync::Barrier;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, RemoveVectoredExceptionHandler, EXCEPTION_POINTERS,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, DiscardVirtualMemory, MapViewOfFile3, UnmapViewOfFile, UnmapViewOfFile2,
    VirtualAlloc, VirtualAlloc2, VirtualFree, VirtualProtect, MEMORY_MAPPED_VIEW_ADDRESS,
    MEM_COMMIT, MEM_PRESERVE_PLACEHOLDER, MEM_RELEASE, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE,
    MEM_RESERVE_PLACEHOLDER, MEM_RESET, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
    SEC_COMMIT, VIRTUAL_ALLOCATION_TYPE,
};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::args::Args;
use crate::win::{self, mib, pct, Owned, PAGE};

/// Every fourth 32-bit word of a page is a "reference" that compaction
/// updates by `DELTA`.
const DELTA: u32 = 0x40;

static TO_BASE: AtomicUsize = AtomicUsize::new(0);
static TO_LEN: AtomicUsize = AtomicUsize::new(0);
static ALIAS: AtomicUsize = AtomicUsize::new(0);
static FROM: AtomicUsize = AtomicUsize::new(0);
static STATE: AtomicUsize = AtomicUsize::new(0);
/// 1: fault handler only unprotects; 2: fault handler fills the page.
static MODE: AtomicUsize = AtomicUsize::new(0);
static WAITS: AtomicUsize = AtomicUsize::new(0);
static BY_MUTATOR: AtomicUsize = AtomicUsize::new(0);
/// Stand-in for ART's `Thread::is_gc_marking` flag.
static mut IS_MARKING: u32 = 0;

thread_local! {
    static FAULTS: Cell<u64> = const { Cell::new(0) };
}

fn state(page: usize) -> &'static AtomicU8 {
    // SAFETY: STATE points to a leaked array of TO_LEN / PAGE atomics.
    unsafe { &*(STATE.load(Relaxed) as *const AtomicU8).add(page) }
}

/// Fills `n` to-space pages starting at `first` through the alias view and
/// makes them accessible at the heap address.
fn fill_run(first: usize, n: usize) {
    let from = FROM.load(Relaxed);
    let alias = ALIAS.load(Relaxed);
    let to = TO_BASE.load(Relaxed);
    for p in first..first + n {
        // SAFETY: both ranges are inside live mappings of TO_LEN bytes.
        unsafe {
            let s = (from + p * PAGE) as *const u32;
            let d = (alias + p * PAGE) as *mut u32;
            std::ptr::copy_nonoverlapping(s, d, PAGE / 4);
            let mut i = 0;
            while i < PAGE / 4 {
                *d.add(i) = (*d.add(i)).wrapping_sub(DELTA);
                i += 4;
            }
        }
    }
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: the range is inside the to-space view.
    unsafe {
        VirtualProtect(
            (to + first * PAGE) as *const c_void,
            n * PAGE,
            PAGE_READWRITE,
            &mut old,
        )
        .expect("VirtualProtect");
    }
}

/// Resolves a mutator fault on `page` (the SIGBUS handler's job).
fn resolve(page: usize) {
    let st = state(page);
    let mut waited = false;
    loop {
        match st.load(Acquire) {
            2 => break,
            0 => {
                if st.compare_exchange(0, 1, AcqRel, Acquire).is_ok() {
                    fill_run(page, 1);
                    st.store(2, Release);
                    BY_MUTATOR.fetch_add(1, Relaxed);
                    break;
                }
            }
            _ => {
                waited = true;
                std::hint::spin_loop();
            }
        }
    }
    if waited {
        WAITS.fetch_add(1, Relaxed);
    }
}

unsafe extern "system" fn veh(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception pointers.
    let rec = unsafe { &*(*info).ExceptionRecord };
    if rec.ExceptionCode != EXCEPTION_ACCESS_VIOLATION {
        return 0; // EXCEPTION_CONTINUE_SEARCH
    }
    let addr = rec.ExceptionInformation[1];
    let base = TO_BASE.load(Relaxed);
    if addr < base || addr >= base + TO_LEN.load(Relaxed) {
        return 0;
    }
    let page = (addr - base) / PAGE;
    match MODE.load(Relaxed) {
        1 => {
            let mut old = PAGE_PROTECTION_FLAGS(0);
            // SAFETY: the page is inside the registered range.
            unsafe {
                let _ = VirtualProtect(
                    (base + page * PAGE) as *const c_void,
                    PAGE,
                    PAGE_READWRITE,
                    &mut old,
                );
            }
        }
        _ => resolve(page),
    }
    FAULTS.with(|f| f.set(f.get() + 1));
    -1 // EXCEPTION_CONTINUE_EXECUTION
}

fn faults() -> u64 {
    FAULTS.with(|f| f.get())
}

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

fn permutation(n: usize, seed: u64) -> Vec<u32> {
    let mut v: Vec<u32> = (0..n as u32).collect();
    let mut s = seed | 1;
    for i in (1..n).rev() {
        let j = (xorshift(&mut s) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

fn alloc_rw(len: usize) -> usize {
    // SAFETY: plain private allocation.
    let p = unsafe { VirtualAlloc(None, len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
    assert!(!p.is_null());
    p as usize
}

fn free(p: usize) {
    // SAFETY: releases an allocation from alloc_rw.
    unsafe {
        let _ = VirtualFree(p as *mut c_void, 0, MEM_RELEASE);
    }
}

fn fill_from(from: usize, len: usize) {
    // SAFETY: the buffer is ours and len bytes long.
    let s = unsafe { std::slice::from_raw_parts_mut(from as *mut u32, len / 4) };
    for (i, w) in s.iter_mut().enumerate() {
        *w = (i as u32).wrapping_mul(2654435761).wrapping_add(DELTA);
    }
}

fn expected(word: usize) -> u32 {
    let v = (word as u32).wrapping_mul(2654435761).wrapping_add(DELTA);
    if word % 4 == 0 {
        v.wrapping_sub(DELTA)
    } else {
        v
    }
}

struct Space {
    section: Owned,
    to: MEMORY_MAPPED_VIEW_ADDRESS,
    alias: MEMORY_MAPPED_VIEW_ADDRESS,
    states: Vec<AtomicU8>,
    setup_us: f64,
    protect_us: f64,
}

/// Creates the to-space section with its heap view (no access) and alias.
fn setup_space(len: usize, from: usize) -> Space {
    setup_space_opt(len, from, false)
}

fn setup_space_opt(len: usize, from: usize, prefault: bool) -> Space {
    let t = Instant::now();
    // SAFETY: pagefile-backed section and two views of it.
    let (section, to, alias) = unsafe {
        let h = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE | SEC_COMMIT,
            (len >> 32) as u32,
            len as u32,
            PCWSTR::null(),
        )
        .expect("section");
        let to = MapViewOfFile3(
            h,
            None,
            None,
            0,
            len,
            VIRTUAL_ALLOCATION_TYPE(0),
            PAGE_READWRITE.0,
            None,
        );
        let alias = MapViewOfFile3(
            h,
            None,
            None,
            0,
            len,
            VIRTUAL_ALLOCATION_TYPE(0),
            PAGE_READWRITE.0,
            None,
        );
        assert!(!to.Value.is_null() && !alias.Value.is_null());
        (Owned(h), to, alias)
    };
    let setup_us = t.elapsed().as_secs_f64() * 1e6;
    if prefault {
        // SAFETY: the alias view is ours and len bytes long.
        unsafe { std::ptr::write_bytes(alias.Value as *mut u8, 0, len) };
    }
    let t = Instant::now();
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: protects the whole heap view.
    unsafe { VirtualProtect(to.Value, len, PAGE_NOACCESS, &mut old).expect("protect") };
    let protect_us = t.elapsed().as_secs_f64() * 1e6;
    let states: Vec<AtomicU8> = (0..len / PAGE).map(|_| AtomicU8::new(0)).collect();
    TO_BASE.store(to.Value as usize, Relaxed);
    TO_LEN.store(len, Relaxed);
    ALIAS.store(alias.Value as usize, Relaxed);
    FROM.store(from, Relaxed);
    STATE.store(states.as_ptr() as usize, Relaxed);
    WAITS.store(0, Relaxed);
    BY_MUTATOR.store(0, Relaxed);
    Space {
        section,
        to,
        alias,
        states,
        setup_us,
        protect_us,
    }
}

fn check_space(s: &Space, len: usize) -> usize {
    let to = s.to.Value as *const u32;
    let mut bad = 0;
    for p in 0..len / PAGE {
        if s.states[p].load(Relaxed) != 2 {
            bad += 1;
            continue;
        }
        for w in [p * PAGE / 4, p * PAGE / 4 + 1, p * PAGE / 4 + 1023] {
            // SAFETY: the page is accessible (state 2).
            if unsafe { *to.add(w) } != expected(w) {
                bad += 1;
            }
        }
    }
    bad
}

fn teardown(s: Space) {
    TO_LEN.store(0, Relaxed);
    // SAFETY: unmaps our views; the section closes on drop.
    unsafe {
        let _ = UnmapViewOfFile(s.to);
        let _ = UnmapViewOfFile(s.alias);
    }
    drop(s.section);
}

struct Lat {
    lat_us: Vec<f64>,
}

/// One mutator: pops page indices from the shared permutation and reads a
/// word of each page; records the latency of accesses that faulted.
fn mutator(perm: &[u32], next: &AtomicUsize, to: usize, compute_ns: u64) -> Lat {
    let mut lat = Vec::new();
    let mut sink = 0u32;
    loop {
        let i = next.fetch_add(1, Relaxed);
        if i >= perm.len() {
            break;
        }
        let p = perm[i] as usize;
        let f0 = faults();
        let t = Instant::now();
        // SAFETY: inside the to-space; a fault is resolved by the handler.
        sink = sink.wrapping_add(unsafe { ((to + p * PAGE + 64) as *const u32).read_volatile() });
        let dt = t.elapsed();
        if faults() != f0 {
            lat.push(dt.as_secs_f64() * 1e6);
        }
        if compute_ns > 0 {
            let t = Instant::now();
            while (t.elapsed().as_nanos() as u64) < compute_ns {
                std::hint::spin_loop();
            }
        }
    }
    std::hint::black_box(sink);
    Lat { lat_us: lat }
}

fn collector(pages: usize, batch: usize) {
    let mut p = 0;
    while p < pages {
        // Claim a run of up to `batch` free pages.
        let mut n = 0;
        while p + n < pages
            && n < batch
            && state(p + n).compare_exchange(0, 1, AcqRel, Acquire).is_ok()
        {
            n += 1;
        }
        if n == 0 {
            p += 1;
            continue;
        }
        fill_run(p, n);
        for q in p..p + n {
            state(q).store(2, Release);
        }
        p += n;
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    pct(&mut v, 0.5)
}

pub fn run(a: &Args) -> io::Result<()> {
    let len: usize = a.num::<usize>("--mib", 64) << 20;
    let reps: usize = a.num("--reps", 3);
    let threads = a.list("--threads", &[1, 2, 4, 7]);
    let pages = len / PAGE;
    // SAFETY: registers our handler first in the chain.
    let h = unsafe { AddVectoredExceptionHandler(1, Some(veh)) };
    assert!(!h.is_null());
    let from = alloc_rw(len);
    fill_from(from, len);
    println!(
        "# GC primitives: space {} MiB ({pages} pages of 4 KiB), {reps} reps, median shown",
        len >> 20
    );

    // 1. Stop-the-world baseline: copy + reference update into plain memory.
    {
        let dst = alloc_rw(len);
        // SAFETY: prefault the destination.
        unsafe { std::ptr::write_bytes(dst as *mut u8, 0, len) };
        let mut v = Vec::new();
        for _ in 0..reps {
            let t = Instant::now();
            for p in 0..pages {
                // SAFETY: both buffers are len bytes.
                unsafe {
                    let s = (from + p * PAGE) as *const u32;
                    let d = (dst + p * PAGE) as *mut u32;
                    std::ptr::copy_nonoverlapping(s, d, PAGE / 4);
                    let mut i = 0;
                    while i < PAGE / 4 {
                        *d.add(i) = (*d.add(i)).wrapping_sub(DELTA);
                        i += 4;
                    }
                }
            }
            v.push(t.elapsed().as_secs_f64());
        }
        let s = median(v);
        println!("\n## stop-the-world copy+update, 1 thread, resident destination");
        println!(
            "{:.1} ms for {} MiB = {:.2} GiB/s = {:.3} us/page",
            s * 1e3,
            len >> 20,
            len as f64 / s / 1073741824.0,
            s * 1e6 / pages as f64
        );
        free(dst);
    }

    // 2. Bare fault round trip: VEH that only unprotects the page.
    println!(
        "\n## fault round trip, handler = VirtualProtect(4 KiB, RW) only, resident private pages"
    );
    println!("| threads | faults | us per fault (wall / threads) | faults/s total |");
    println!("| ---: | ---: | ---: | ---: |");
    for &t in &threads {
        let mut v = Vec::new();
        for _ in 0..reps {
            let region = alloc_rw(len);
            // SAFETY: prefault, then protect the region.
            unsafe {
                std::ptr::write_bytes(region as *mut u8, 1, len);
                let mut old = PAGE_PROTECTION_FLAGS(0);
                VirtualProtect(region as *const c_void, len, PAGE_NOACCESS, &mut old).unwrap();
            }
            TO_BASE.store(region, Relaxed);
            TO_LEN.store(len, Relaxed);
            MODE.store(1, Relaxed);
            let bar = Barrier::new(t);
            let t0 = Instant::now();
            std::thread::scope(|sc| {
                for k in 0..t {
                    let bar = &bar;
                    sc.spawn(move || {
                        bar.wait();
                        let mut p = k;
                        let mut sink = 0u8;
                        while p < pages {
                            // SAFETY: in the region; the fault is resolved.
                            sink ^= unsafe { ((region + p * PAGE) as *const u8).read_volatile() };
                            p += t;
                        }
                        std::hint::black_box(sink);
                    });
                }
            });
            v.push(t0.elapsed().as_secs_f64());
            TO_LEN.store(0, Relaxed);
            free(region);
        }
        let s = median(v);
        println!(
            "| {t} | {pages} | {:.2} | {:.0} |",
            s * 1e6 / pages as f64 * t as f64,
            pages as f64 / s
        );
    }
    MODE.store(2, Relaxed);

    // 3. Collector alone (no mutator faults): concurrent compaction speed.
    println!("\n## collector alone: fill via alias + VirtualProtect per run of pages");
    println!("| batch pages | to-space pages already allocated | setup (section + 2 views) us | protect to-space NOACCESS us | compaction ms | us/page | GiB/s | bad |");
    println!("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for &(batch, prefault) in &[
        (1usize, false),
        (16, false),
        (64, false),
        (1, true),
        (16, true),
    ] {
        let mut v = Vec::new();
        let mut su = Vec::new();
        let mut pu = Vec::new();
        let mut bad = 0;
        for _ in 0..reps {
            let s = setup_space_opt(len, from, prefault);
            su.push(s.setup_us);
            pu.push(s.protect_us);
            let t = Instant::now();
            collector(pages, batch);
            v.push(t.elapsed().as_secs_f64());
            bad += check_space(&s, len);
            teardown(s);
        }
        let s = median(v);
        println!(
            "| {batch} | {} | {:.0} | {:.0} | {:.1} | {:.3} | {:.2} | {bad} |",
            if prefault { "yes" } else { "no (demand-zero)" },
            median(su),
            median(pu),
            s * 1e3,
            s * 1e6 / pages as f64,
            len as f64 / s / 1073741824.0
        );
    }

    // 4. Mutators only: every page is filled by a mutator fault.
    println!("\n## mutators only: every to-space page filled in the fault handler");
    println!("| threads | wall ms | us per fault (wall / threads) | fault latency p50 us | p99 us | p99.9 us | max us | bad |");
    println!("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for &t in &threads {
        let mut walls = Vec::new();
        let mut lats = Vec::new();
        let mut bad = 0;
        for r in 0..reps {
            let perm = permutation(pages, 0x9e37 + r as u64);
            let s = setup_space(len, from);
            let to = s.to.Value as usize;
            let next = AtomicUsize::new(0);
            let t0 = Instant::now();
            let res: Vec<Lat> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..t)
                    .map(|_| sc.spawn(|| mutator(&perm, &next, to, 0)))
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            walls.push(t0.elapsed().as_secs_f64());
            for l in res {
                lats.extend(l.lat_us);
            }
            bad += check_space(&s, len);
            teardown(s);
        }
        let w = median(walls);
        println!(
            "| {t} | {:.1} | {:.2} | {:.2} | {:.1} | {:.1} | {:.0} | {bad} |",
            w * 1e3,
            w * 1e6 / pages as f64 * t as f64,
            pct(&mut lats, 0.5),
            pct(&mut lats, 0.99),
            pct(&mut lats, 0.999),
            pct(&mut lats, 1.0)
        );
    }

    // 5. Concurrent compaction: collector plus mutators that touch random
    //    pages with some work between accesses.
    let compute_ns: u64 = a.num("--compute-ns", 2000);
    println!("\n## concurrent: collector (batch 16) + mutators touching random pages, {compute_ns} ns of work between touches");
    println!("| mutators | compaction ms | pages filled by mutators | waits on busy page | mutator faults | fault latency p50 us | p99 us | p99.9 us | max us | bad |");
    println!("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for &t in &threads {
        let mut gcs = Vec::new();
        let mut lats = Vec::new();
        let mut bym = Vec::new();
        let mut waits = Vec::new();
        let mut nf = Vec::new();
        let mut bad = 0;
        for r in 0..reps {
            let perm = permutation(pages, 0x51ed + r as u64);
            let s = setup_space(len, from);
            let to = s.to.Value as usize;
            let next = AtomicUsize::new(0);
            let mut gc_s = 0.0;
            let res: Vec<Lat> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..t)
                    .map(|_| sc.spawn(|| mutator(&perm, &next, to, compute_ns)))
                    .collect();
                let t0 = Instant::now();
                collector(pages, 16);
                gc_s = t0.elapsed().as_secs_f64();
                // Stop the mutators once compaction is over.
                next.store(usize::MAX / 2, Relaxed);
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            gcs.push(gc_s);
            let mut n = 0;
            for l in res {
                n += l.lat_us.len();
                lats.extend(l.lat_us);
            }
            nf.push(n as f64);
            bym.push(BY_MUTATOR.load(Relaxed) as f64);
            waits.push(WAITS.load(Relaxed) as f64);
            bad += check_space(&s, len);
            teardown(s);
        }
        println!(
            "| {t} | {:.1} | {:.0} | {:.0} | {:.0} | {:.2} | {:.1} | {:.1} | {:.0} | {bad} |",
            median(gcs) * 1e3,
            median(bym),
            median(waits),
            median(nf),
            pct(&mut lats, 0.5),
            pct(&mut lats, 0.99),
            pct(&mut lats, 0.999),
            pct(&mut lats, 1.0)
        );
    }
    MODE.store(0, Relaxed);

    flip(len, reps);
    read_barrier(a);

    // SAFETY: removes the handler we added.
    unsafe { RemoveVectoredExceptionHandler(h) };
    free(from);
    Ok(())
}

/// The pause-time part of the cycle: the `mremap(MREMAP_DONTUNMAP)`
/// substitute on a resident heap, and freeing the from-space afterwards.
fn flip(len: usize, reps: usize) {
    println!(
        "\n## flip (mremap substitute) on a resident {} MiB heap at a fixed placeholder",
        len >> 20
    );
    println!("| step | median us |");
    println!("| --- | ---: |");
    let mut t_alias = Vec::new();
    let mut t_unmap = Vec::new();
    let mut t_new = Vec::new();
    let mut t_prot = Vec::new();
    let mut t_alias2 = Vec::new();
    let mut t_reset = Vec::new();
    let mut t_discard = Vec::new();
    let mut t_free = Vec::new();
    let mut ws_after_reset = 0usize;
    let mut ws_after_discard = 0usize;
    let mut reset_ok = true;
    let mut discard_rc = 0u32;
    // SAFETY: pseudo handle.
    let me = unsafe { GetCurrentProcess() };
    for _ in 0..reps {
        // SAFETY: the sequence below only touches mappings created here.
        unsafe {
            let mk = || {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    None,
                    PAGE_READWRITE | SEC_COMMIT,
                    (len >> 32) as u32,
                    len as u32,
                    PCWSTR::null(),
                )
                .expect("section")
            };
            let s0 = Owned(mk());
            let s1 = Owned(mk());
            let hp = VirtualAlloc2(
                None,
                None,
                len,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS.0,
                None,
            );
            assert!(!hp.is_null());
            let hv = MapViewOfFile3(
                s0.0,
                None,
                Some(hp),
                0,
                len,
                MEM_REPLACE_PLACEHOLDER,
                PAGE_READWRITE.0,
                None,
            );
            assert!(!hv.Value.is_null());
            std::ptr::write_bytes(hv.Value as *mut u8, 7, len);

            let t = Instant::now();
            let fv = MapViewOfFile3(
                s0.0,
                None,
                None,
                0,
                len,
                VIRTUAL_ALLOCATION_TYPE(0),
                PAGE_READWRITE.0,
                None,
            );
            t_alias.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            UnmapViewOfFile2(me, hv, MEM_PRESERVE_PLACEHOLDER).expect("unmap preserve");
            t_unmap.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            let nv = MapViewOfFile3(
                s1.0,
                None,
                Some(hp),
                0,
                len,
                MEM_REPLACE_PLACEHOLDER,
                PAGE_READWRITE.0,
                None,
            );
            assert!(!nv.Value.is_null());
            t_new.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            let mut old = PAGE_PROTECTION_FLAGS(0);
            VirtualProtect(nv.Value, len, PAGE_NOACCESS, &mut old).unwrap();
            t_prot.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            let av = MapViewOfFile3(
                s1.0,
                None,
                None,
                0,
                len,
                VIRTUAL_ALLOCATION_TYPE(0),
                PAGE_READWRITE.0,
                None,
            );
            assert!(!av.Value.is_null());
            t_alias2.push(t.elapsed().as_secs_f64() * 1e6);
            // The from-space still holds the old data.
            assert_eq!(*(fv.Value as *const u8).add(len - 1), 7);

            // Releasing from-space memory after compaction.
            let t = Instant::now();
            let r = VirtualAlloc(Some(fv.Value), len / 2, MEM_RESET, PAGE_NOACCESS);
            t_reset.push(t.elapsed().as_secs_f64() * 1e6);
            reset_ok &= !r.is_null();
            ws_after_reset = win::range_ws(me, fv.Value as usize, len / 2).valid;
            let t = Instant::now();
            let half = std::slice::from_raw_parts_mut((fv.Value as *mut u8).add(len / 2), len / 2);
            discard_rc = DiscardVirtualMemory(half);
            t_discard.push(t.elapsed().as_secs_f64() * 1e6);
            ws_after_discard = win::range_ws(me, fv.Value as usize + len / 2, len / 2).valid;
            let t = Instant::now();
            let _ = UnmapViewOfFile(fv);
            drop(s0);
            t_free.push(t.elapsed().as_secs_f64() * 1e6);

            let _ = UnmapViewOfFile(av);
            let _ = UnmapViewOfFile2(me, nv, MEM_PRESERVE_PLACEHOLDER);
            let _ = VirtualFree(hp, 0, MEM_RELEASE);
            drop(s1);
        }
    }
    let total = median(t_alias.clone())
        + median(t_unmap.clone())
        + median(t_new.clone())
        + median(t_prot.clone())
        + median(t_alias2.clone());
    println!(
        "| map from-space alias of old section | {:.0} |",
        median(t_alias)
    );
    println!(
        "| unmap heap view, keep placeholder | {:.0} |",
        median(t_unmap)
    );
    println!(
        "| map fresh section at heap address | {:.0} |",
        median(t_new)
    );
    println!(
        "| VirtualProtect heap view NOACCESS | {:.0} |",
        median(t_prot)
    );
    println!(
        "| map collector alias of fresh section | {:.0} |",
        median(t_alias2)
    );
    println!("| **total flip (in the pause)** | **{total:.0}** |");
    println!("| MEM_RESET half of from-space view (ok={reset_ok}, pages still in WS after: {ws_after_reset}) | {:.0} |", median(t_reset));
    println!("| DiscardVirtualMemory other half (rc={discard_rc}, pages still in WS after: {ws_after_discard}) | {:.0} |", median(t_discard));
    println!(
        "| unmap from-space and close old section | {:.0} |",
        median(t_free)
    );
}

/// Fast-path cost of a Baker-style read barrier when the collector is not
/// marking: one load and compare of a flag per reference load.
fn read_barrier(a: &Args) {
    let nobj: usize = a.num("--rb-objects", 1 << 22);
    let steps: usize = a.num("--rb-steps", 20_000_000);
    // Objects of 16 bytes: [ref u32][payload u32][pad u64].
    let mut objs = vec![[0u32; 4]; nobj];
    let perm = permutation(nobj, 77);
    for i in 0..nobj {
        objs[perm[i] as usize][0] = perm[(i + 1) % nobj];
        objs[i][1] = i as u32;
    }
    let refs: Vec<u32> = (0..nobj).map(|i| perm[i]).collect();
    let flag = std::ptr::addr_of!(IS_MARKING);
    #[inline(never)]
    fn slow(r: u32) -> u32 {
        std::hint::black_box(r)
    }
    let chase = |barrier: bool| {
        let t = Instant::now();
        let mut r = perm[0];
        for _ in 0..steps {
            r = objs[r as usize][0];
            // SAFETY: plain read of a static.
            if barrier && unsafe { flag.read_volatile() } != 0 {
                r = slow(r);
            }
        }
        std::hint::black_box(r);
        t.elapsed().as_secs_f64() * 1e9 / steps as f64
    };
    let scan = |barrier: bool| {
        let t = Instant::now();
        let mut sum = 0u64;
        for _ in 0..4 {
            for &r in &refs {
                let mut r = r;
                // SAFETY: plain read of a static.
                if barrier && unsafe { flag.read_volatile() } != 0 {
                    r = slow(r);
                }
                sum += objs[r as usize][1] as u64;
            }
        }
        std::hint::black_box(sum);
        t.elapsed().as_secs_f64() * 1e9 / (4 * nobj) as f64
    };
    let mut c0 = Vec::new();
    let mut c1 = Vec::new();
    let mut s0 = Vec::new();
    let mut s1 = Vec::new();
    for _ in 0..5 {
        c0.push(chase(false));
        c1.push(chase(true));
        s0.push(scan(false));
        s1.push(scan(true));
    }
    let (c0, c1, s0, s1) = (median(c0), median(c1), median(s0), median(s1));
    println!("\n## read-barrier fast path (flag check per reference load, not marking), {} MiB of objects", (nobj * 16) >> 20);
    println!("| workload | no barrier ns/load | barrier ns/load | overhead |");
    println!("| --- | ---: | ---: | ---: |");
    println!(
        "| dependent pointer chase (cache-miss bound) | {c0:.2} | {c1:.2} | {:+.1}% |",
        (c1 / c0 - 1.0) * 100.0
    );
    println!(
        "| independent reference scan | {s0:.2} | {s1:.2} | {:+.1}% |",
        (s1 / s0 - 1.0) * 100.0
    );
    let _ = mib(0);
    let _ = AtomicU32::new(0);
}

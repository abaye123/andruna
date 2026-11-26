//! Shared boot image: a broker builds one section holding a synthetic boot
//! image whose 32-bit references are valid only at a fixed base address
//! (as ART's are), and child processes map it at that address into a
//! placeholder, either themselves at the start of `main` ("self") or from
//! the broker while the child is still suspended ("inject").

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile3, MemExtendedParameterAddressRequirements,
    PrefetchVirtualMemory, UnmapViewOfFile, VirtualAlloc2, VirtualFree, VirtualProtect,
    VirtualProtectEx, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_ADDRESS_REQUIREMENTS,
    MEM_EXTENDED_PARAMETER, MEM_MAPPED, MEM_RELEASE, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE,
    MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE,
    PAGE_WRITECOPY, SEC_COMMIT, VIRTUAL_ALLOCATION_TYPE, WIN32_MEMORY_RANGE_ENTRY,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, ResumeThread, SetEvent, TerminateProcess, WaitForSingleObject,
    INFINITE,
};

use crate::args::Args;
use crate::win::{self, mib, pct, Owned, PAGE};

/// Size of one synthetic object.
const OBJ: usize = 256;
/// First page of the region the children dirty (after a 64 KiB header).
const DIRTY_FIRST_PAGE: usize = 16;
const MAGIC: u32 = 0x5332_4249; // "S2BI"

// ------------------------------------------------------------------ image

fn lcg(i: u64) -> u64 {
    i.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
        >> 17
}

/// Fills `buf` (part of the image starting at image offset `off`) with
/// objects whose references are absolute 32-bit addresses for `base`.
fn generate(buf: &mut [u8], off: usize, base: usize, size: usize) {
    let nobj = size / OBJ;
    for (k, obj) in buf.chunks_exact_mut(OBJ).enumerate() {
        let i = off / OBJ + k;
        let w = |j: usize, v: u32, obj: &mut [u8]| {
            obj[j * 4..j * 4 + 4].copy_from_slice(&v.to_le_bytes())
        };
        let addr = (base + i * OBJ) as u32;
        w(0, addr, obj); // self reference
        w(
            1,
            (base + (lcg(i as u64) as usize % nobj) * OBJ) as u32,
            obj,
        ); // next
        w(2, (base + (i % 4096) * OBJ) as u32, obj); // class
        w(3, 0, obj); // a "static field" children write to
        w(4, MAGIC, obj);
        w(5, base as u32, obj);
        w(6, (size / PAGE) as u32, obj);
        for j in 7..OBJ / 4 {
            w(j, (i as u32).wrapping_mul(2654435761) ^ j as u32, obj);
        }
    }
}

/// Number of 32-bit reference fields at the start of each object.
const REFS: usize = 3;

// ---------------------------------------------------------------- section

fn create_pagefile_section(size: usize, base: usize) -> io::Result<Owned> {
    // SAFETY: pagefile-backed section; the broker's view is unmapped below.
    unsafe {
        let h = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE | SEC_COMMIT,
            (size >> 32) as u32,
            size as u32,
            PCWSTR::null(),
        )
        .map_err(win::werr)?;
        let h = Owned(h);
        let v = MapViewOfFile3(
            h.0,
            None,
            None,
            0,
            size,
            VIRTUAL_ALLOCATION_TYPE(0),
            PAGE_READWRITE.0,
            None,
        );
        if v.Value.is_null() {
            return Err(win::last_err());
        }
        let s = std::slice::from_raw_parts_mut(v.Value as *mut u8, size);
        generate(s, 0, base, size);
        UnmapViewOfFile(v).map_err(win::werr)?;
        Ok(h)
    }
}

fn file_ok(path: &str, size: usize, base: usize) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    if f.metadata().map(|m| m.len() as usize).unwrap_or(0) != size {
        return false;
    }
    let mut hdr = [0u8; 28];
    if f.read_exact(&mut hdr).is_err() {
        return false;
    }
    let r = |j: usize| u32::from_le_bytes(hdr[j * 4..j * 4 + 4].try_into().unwrap());
    r(4) == MAGIC && r(5) == base as u32 && r(6) == (size / PAGE) as u32
}

fn create_file_section(path: &str, size: usize, base: usize) -> io::Result<(Owned, bool)> {
    let mut wrote = false;
    if !file_ok(path, size, base) {
        let mut f = std::fs::File::create(path)?;
        let chunk = 16 << 20;
        let mut buf = vec![0u8; chunk];
        let mut off = 0;
        while off < size {
            let n = chunk.min(size - off);
            generate(&mut buf[..n], off, base, size);
            f.write_all(&buf[..n])?;
            off += n;
        }
        f.sync_all()?;
        wrote = true;
    }
    let w = win::wide(path);
    // SAFETY: opens the file read-only; the section keeps its own reference.
    unsafe {
        let fh = CreateFileW(
            PCWSTR(w.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(win::werr)?;
        let fh = Owned(fh);
        let h = CreateFileMappingW(fh.0, None, PAGE_READONLY, 0, 0, PCWSTR::null())
            .map_err(win::werr)?;
        Ok((Owned(h), wrote))
    }
}

// ----------------------------------------------------------------- broker

struct ChildRec {
    child: win::Child,
    exit: Owned,
    label: String,
    report: BTreeMap<String, String>,
    spawn_ms: f64,
    inject_us: f64,
    inject_free: bool,
}

fn read_line(h: HANDLE) -> io::Result<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let mut n = 0u32;
        // SAFETY: reads one byte into a local buffer.
        unsafe {
            windows::Win32::Storage::FileSystem::ReadFile(h, Some(&mut b), Some(&mut n), None)
                .map_err(win::werr)?;
        }
        if n == 0 || b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

fn parse_report(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn reserve_and_map(
    process: Option<HANDLE>,
    section: HANDLE,
    base: usize,
    size: usize,
    prot: PAGE_PROTECTION_FLAGS,
) -> io::Result<usize> {
    // SAFETY: reserves a placeholder at `base` and replaces it with a view.
    unsafe {
        let p = VirtualAlloc2(
            process,
            Some(base as *const c_void),
            size,
            MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
            PAGE_NOACCESS.0,
            None,
        );
        if p.is_null() {
            return Err(win::last_err());
        }
        let v = MapViewOfFile3(
            section,
            process,
            Some(p),
            0,
            size,
            MEM_REPLACE_PLACEHOLDER,
            prot.0,
            None,
        );
        if v.Value.is_null() {
            let e = win::last_err();
            let _ = windows::Win32::System::Memory::VirtualFreeEx(
                process.unwrap_or(GetCurrentProcess()),
                p,
                0,
                MEM_RELEASE,
            );
            return Err(e);
        }
        Ok(v.Value as usize)
    }
}

pub fn broker(a: &Args) -> io::Result<()> {
    let procs: usize = a.num("--procs", 24);
    let size: usize = a.num("--size-mib", 320) << 20;
    let base: usize = a.hex("--base", 0x6000_0000);
    let backing = a.str("--backing", "pagefile");
    let mode = a.str("--mode", "self");
    let cow = a.str("--cow", "part");
    let dirty: usize = a.num("--dirty-pages", 1024);
    let n_ac: usize = a.num("--appcontainer", 2);
    let n_reloc: usize = a.num("--force-reloc", 0);
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let file = a.str(
        "--file",
        &std::env::temp_dir()
            .join("andruna-s2-bootimage.bin")
            .to_string_lossy(),
    );
    assert!(base % 0x10000 == 0, "base must be 64 KiB aligned");
    assert!(
        base + size <= 0x1_0000_0000,
        "image must end below 4 GiB (32-bit references)"
    );

    println!("# boot image broker: {procs} children, image {} MiB at {base:#x}, backing {backing}, mode {mode}, cow {cow}, dirty {dirty} pages, {n_ac} in AppContainer, {n_reloc} forced to relocate", size >> 20);
    let commit0 = win::commit_total();
    let t = Instant::now();
    let (section, note) = if backing == "file" {
        let (s, wrote) = create_file_section(&file, size, base)?;
        (
            s,
            if wrote {
                "file written"
            } else {
                "file reused (in page cache)"
            },
        )
    } else {
        (
            create_pagefile_section(size, base)?,
            "pagefile section filled",
        )
    };
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    let commit1 = win::commit_total();
    println!(
        "# section built in {build_ms:.1} ms ({note}); system commit +{:.1} MiB",
        mib(commit1.saturating_sub(commit0))
    );

    let ac_name = "andruna.s2.bootimage";
    if n_ac > 0 {
        win::appcontainer_profile(ac_name)?;
        let dir = std::env::current_exe()?
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let st = std::process::Command::new("icacls")
            .args([&dir, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)"])
            .stdout(std::process::Stdio::null())
            .status()?;
        if !st.success() {
            return Err(io::Error::other("icacls failed"));
        }
    }

    // Read-only, inheritable handle for children that map it themselves.
    let child_section = win::dup_inheritable(section.0, 0x4 | 0x1)?; // SECTION_MAP_READ | SECTION_QUERY

    let mut kids: Vec<ChildRec> = Vec::new();
    // Child 0 is a baseline without the image.
    for i in 0..=procs {
        let (cmode, label) = if i == 0 {
            ("none", "baseline".to_string())
        } else {
            let m = match mode.as_str() {
                "mixed" => {
                    if i % 2 == 0 {
                        "inject"
                    } else {
                        "self"
                    }
                }
                m => m,
            };
            (m, m.to_string())
        };
        let in_ac = i > 0 && i <= n_ac;
        let force_reloc = i > n_ac && i <= n_ac + n_reloc;
        let label = format!(
            "{label}{}{}",
            if in_ac { "+AC" } else { "" },
            if force_reloc { "+reloc" } else { "" }
        );

        let mut rd = HANDLE::default();
        let mut wr = HANDLE::default();
        // SAFETY: anonymous pipe for the child's report line.
        unsafe { CreatePipe(&mut rd, &mut wr, None, 0).map_err(win::werr)? };
        let rd = Owned(rd);
        let wr = Owned(wr);
        win::set_inherit(wr.0, true);
        // SAFETY: manual-reset event, initially clear.
        let ev = unsafe { CreateEventW(None, true, false, PCWSTR::null()).map_err(win::werr)? };
        let ev = Owned(ev);
        let ev_child = win::dup_inheritable(ev.0, 0)?;

        let mut inherit = vec![ev_child.0];
        let mut cmd = format!(
            "\"{exe}\" child --mode {cmode} --base {base:#x} --size {size} --exit {} --dirty-pages {dirty} --cow {cow}",
            ev_child.0 .0 as usize
        );
        if cmode == "self" {
            cmd += &format!(" --section {}", child_section.0 .0 as usize);
            inherit.push(child_section.0);
        }
        if force_reloc {
            cmd += " --force-reloc";
        }
        if a.flag("--prefetch") {
            cmd += " --prefetch";
        }
        let t = Instant::now();
        let child = win::spawn(
            &cmd,
            if in_ac { Some(ac_name) } else { None },
            cmode == "inject",
            wr.0,
            &inherit,
        )?;
        drop(wr);
        drop(ev_child);
        let mut inject_us = 0.0;
        let mut inject_free = true;
        if cmode == "inject" {
            let ti = Instant::now();
            inject_free = win::range_free(child.process.0, base, size);
            let prot = if cow == "all" {
                PAGE_WRITECOPY
            } else {
                PAGE_READONLY
            };
            let r = if force_reloc || !inject_free {
                Err(io::Error::other("fixed range unavailable"))
            } else {
                reserve_and_map(Some(child.process.0), section.0, base, size, prot)
            };
            match r {
                Ok(_) => {
                    if cow == "part" && dirty > 0 {
                        let mut old = PAGE_PROTECTION_FLAGS(0);
                        // SAFETY: changes protection of part of the view in the child.
                        unsafe {
                            VirtualProtectEx(
                                child.process.0,
                                (base + DIRTY_FIRST_PAGE * PAGE) as *const c_void,
                                dirty * PAGE,
                                PAGE_WRITECOPY,
                                &mut old,
                            )
                            .map_err(win::werr)?;
                        }
                    }
                }
                Err(e) => {
                    println!("# child {i}: inject failed ({e}); child will map itself elsewhere");
                }
            }
            inject_us = ti.elapsed().as_secs_f64() * 1e6;
            // SAFETY: resumes the primary thread of the child we created.
            unsafe { ResumeThread(child.thread.0) };
        }
        let line = read_line(rd.0)?;
        let spawn_ms = t.elapsed().as_secs_f64() * 1e3;
        let report = parse_report(&line);
        if !line.starts_with("READY") {
            println!("# child {i} ({label}) failed: {line}");
        }
        kids.push(ChildRec {
            child,
            exit: ev,
            label,
            report,
            spawn_ms,
            inject_us,
            inject_free,
        });
    }
    let commit2 = win::commit_total();

    // Measure every child while all of them are alive.
    println!();
    println!("| # | kind | pid | spawn->ready ms | fixed addr free | relocated | map us | prefetch ms | touch ms | touch us/page | dirty ms | reloc ms | bad refs | image WS MiB | image shared MiB | image private MiB | process WS MiB | process private WS MiB | private commit MiB | rw map denied |");
    println!("| ---: | --- | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |");
    let mut rows = Vec::new();
    for (i, k) in kids.iter().enumerate() {
        let r = &k.report;
        let g = |key: &str| r.get(key).cloned().unwrap_or_else(|| "-".into());
        let gf = |key: &str| {
            r.get(key)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        let addr = r
            .get("addr")
            .and_then(|v| usize::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .unwrap_or(base);
        let ws = win::range_ws(k.child.process.0, addr, size);
        let pm = win::proc_mem(k.child.process.0);
        let (wsp, wss) = win::ws_pages(k.child.process.0);
        let map_us = if k.inject_us > 0.0 {
            k.inject_us
        } else {
            gf("t_res_us") + gf("t_map_us") + gf("t_prot_us")
        };
        let free = if k.label.starts_with("inject") {
            k.inject_free.to_string()
        } else {
            g("free")
        };
        println!(
            "| {i} | {} | {} | {:.1} | {} | {} | {:.0} | {:.1} | {:.1} | {:.2} | {:.2} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {} |",
            k.label,
            k.child.pid,
            k.spawn_ms,
            free,
            g("reloc"),
            map_us,
            gf("t_prefetch_us") / 1e3,
            gf("t_touch_us") / 1e3,
            gf("t_touch_us") / (size / PAGE) as f64,
            gf("t_dirty_us") / 1e3,
            r.get("t_reloc_us").map(|v| format!("{:.1}", v.parse::<f64>().unwrap_or(0.0) / 1e3)).unwrap_or("-".into()),
            g("bad"),
            mib(ws.valid * PAGE),
            mib(ws.shared * PAGE),
            mib(ws.private * PAGE),
            mib(pm.working_set),
            mib(pm.private_ws),
            mib(pm.private_commit),
            g("rw_denied"),
        );
        let _ = (wsp, wss);
        rows.push((
            k.label.clone(),
            k.spawn_ms,
            map_us,
            gf("t_touch_us") / 1e3,
            gf("t_dirty_us") / 1e3,
            ws,
            pm,
        ));
    }

    // Summary over the image children of the main kind (no relocation).
    let base_pm = rows[0].6;
    let mut sel: Vec<_> = rows
        .iter()
        .skip(1)
        .filter(|r| !r.0.contains("reloc"))
        .collect();
    if !sel.is_empty() {
        let col =
            |f: &dyn Fn(&&(String, f64, f64, f64, f64, win::RangeWs, win::ProcMem)) -> f64| {
                let mut v: Vec<f64> = sel.iter().map(f).collect();
                let med = pct(&mut v, 0.5);
                (med, v[0], v[v.len() - 1])
            };
        let s = col(&|r| r.1);
        let m = col(&|r| r.2);
        let tc = col(&|r| r.3);
        let d = col(&|r| r.4);
        let pws = col(&|r| mib(r.6.private_ws) - mib(base_pm.private_ws));
        let pc = col(&|r| mib(r.6.private_commit) - mib(base_pm.private_commit));
        let ip = col(&|r| mib(r.5.private * PAGE));
        println!();
        println!(
            "summary over {} image children (median [min-max]):",
            sel.len()
        );
        println!(
            "  spawn->ready           {:.1} ms [{:.1}-{:.1}]",
            s.0, s.1, s.2
        );
        println!(
            "  reserve+map(+protect)  {:.0} us [{:.0}-{:.0}]",
            m.0, m.1, m.2
        );
        println!(
            "  touch all pages        {:.1} ms [{:.1}-{:.1}]",
            tc.0, tc.1, tc.2
        );
        println!(
            "  dirty {dirty} pages      {:.2} ms [{:.2}-{:.2}]",
            d.0, d.1, d.2
        );
        println!(
            "  image private WS       {:.1} MiB [{:.1}-{:.1}]",
            ip.0, ip.1, ip.2
        );
        println!(
            "  private WS vs baseline {:.1} MiB [{:.1}-{:.1}]",
            pws.0, pws.1, pws.2
        );
        println!(
            "  private commit vs base {:.1} MiB [{:.1}-{:.1}]",
            pc.0, pc.1, pc.2
        );
        sel.clear();
    }
    println!(
        "  baseline child: WS {:.1} MiB, private WS {:.1} MiB, private commit {:.1} MiB",
        mib(base_pm.working_set),
        mib(base_pm.private_ws),
        mib(base_pm.private_commit)
    );
    println!(
        "  system commit: section +{:.1} MiB, all children +{:.1} MiB ({:.1} MiB per child incl. baseline)",
        mib(commit1.saturating_sub(commit0)),
        mib(commit2.saturating_sub(commit1)),
        mib(commit2.saturating_sub(commit1)) / (procs + 1) as f64
    );

    for k in &kids {
        // SAFETY: signals and waits on handles we own.
        unsafe {
            let _ = SetEvent(k.exit.0);
        }
    }
    for k in &kids {
        // SAFETY: waits on a process handle we own.
        unsafe {
            if WaitForSingleObject(k.child.process.0, 10_000) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(k.child.process.0, 1);
            }
        }
    }
    if n_ac > 0 {
        win::delete_appcontainer_profile(ac_name);
    }
    Ok(())
}

// ------------------------------------------------------------------ child

fn low4g_placeholder(size: usize) -> io::Result<usize> {
    let mut req = MEM_ADDRESS_REQUIREMENTS {
        LowestStartingAddress: 0x1000_0000 as *mut c_void,
        HighestEndingAddress: 0xffff_ffff as *mut c_void,
        Alignment: 0,
    };
    let mut p = MEM_EXTENDED_PARAMETER::default();
    p.Anonymous1._bitfield = MemExtendedParameterAddressRequirements.0 as u64;
    p.Anonymous2.Pointer = &mut req as *mut _ as *mut c_void;
    // SAFETY: reserves a placeholder anywhere below 4 GiB.
    let a = unsafe {
        VirtualAlloc2(
            None,
            None,
            size,
            MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
            PAGE_NOACCESS.0,
            Some(std::slice::from_mut(&mut p)),
        )
    };
    if a.is_null() {
        Err(win::last_err())
    } else {
        Ok(a as usize)
    }
}

pub fn child(a: &Args) -> io::Result<()> {
    let t_main = Instant::now();
    let mode = a.str("--mode", "self");
    let base: usize = a.hex("--base", 0);
    let size: usize = a.num("--size", 0);
    let exit = HANDLE(a.num::<usize>("--exit", 0) as *mut c_void);
    let dirty: usize = a.num("--dirty-pages", 0);
    let cow = a.str("--cow", "part");
    let force_reloc = a.flag("--force-reloc");
    let mut out = String::from("READY");
    // SAFETY: pseudo handle of this process.
    let me = unsafe { GetCurrentProcess() };

    if mode != "none" {
        let free = win::range_free(me, base, size);
        let mut addr = base;
        let mut reloc = false;
        let mut t_res = 0.0;
        let mut t_map = 0.0;
        let mut t_prot = 0.0;
        if mode == "self" {
            let section = HANDLE(a.num::<usize>("--section", 0) as *mut c_void);
            // A writable view must be refused: the handle is read-only.
            // SAFETY: attempts a mapping that is expected to fail.
            let rw = unsafe {
                MapViewOfFile3(
                    section,
                    None,
                    None,
                    0,
                    0x10000,
                    VIRTUAL_ALLOCATION_TYPE(0),
                    PAGE_READWRITE.0,
                    None,
                )
            };
            if rw.Value.is_null() {
                out += &format!(" rw_denied=err{}", unsafe { GetLastError().0 });
            } else {
                out += " rw_denied=NO";
                // SAFETY: unmaps the view we just made.
                unsafe {
                    let _ = UnmapViewOfFile(rw);
                }
            }
            let prot = if cow == "all" || force_reloc {
                PAGE_WRITECOPY
            } else {
                PAGE_READONLY
            };
            let t = Instant::now();
            // SAFETY: reserve a placeholder at the fixed base.
            let p = if force_reloc || !free {
                std::ptr::null_mut()
            } else {
                unsafe {
                    VirtualAlloc2(
                        None,
                        Some(base as *const c_void),
                        size,
                        MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                        PAGE_NOACCESS.0,
                        None,
                    )
                }
            };
            let p = if p.is_null() {
                reloc = true;
                low4g_placeholder(size)?
            } else {
                p as usize
            };
            t_res = t.elapsed().as_secs_f64() * 1e6;
            let t = Instant::now();
            // SAFETY: replaces our placeholder with a view of the section.
            let v = unsafe {
                MapViewOfFile3(
                    section,
                    None,
                    Some(p as *const c_void),
                    0,
                    size,
                    MEM_REPLACE_PLACEHOLDER,
                    prot.0,
                    None,
                )
            };
            if v.Value.is_null() {
                let e = win::last_err();
                // SAFETY: releases our placeholder.
                unsafe {
                    let _ = VirtualFree(p as *mut c_void, 0, MEM_RELEASE);
                }
                println!("FAIL map {e}");
                return Ok(());
            }
            t_map = t.elapsed().as_secs_f64() * 1e6;
            addr = v.Value as usize;
            if cow == "part" && dirty > 0 && !reloc {
                let t = Instant::now();
                let mut old = PAGE_PROTECTION_FLAGS(0);
                // SAFETY: protection change on part of our own view.
                unsafe {
                    VirtualProtect(
                        (addr + DIRTY_FIRST_PAGE * PAGE) as *const c_void,
                        dirty * PAGE,
                        PAGE_WRITECOPY,
                        &mut old,
                    )
                    .map_err(win::werr)?;
                }
                t_prot = t.elapsed().as_secs_f64() * 1e6;
            }
        } else {
            // inject: the broker mapped the view before we started; if it
            // could not, fall back to a relocated self mapping is not
            // possible without a handle, so report it.
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: plain query into a local struct.
            unsafe {
                VirtualQuery(
                    Some(base as *const c_void),
                    &mut mbi,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if mbi.Type != MEM_MAPPED {
                println!("FAIL inject: no view at {base:#x}");
                return Ok(());
            }
        }

        let nobj = size / OBJ;
        let img = addr as *mut u32;
        let delta = addr.wrapping_sub(base) as u32;
        let mut t_reloc = None;
        if reloc {
            // Relocate every reference, as ART does when the boot image
            // cannot be mapped at its preferred address.
            let t = Instant::now();
            for i in 0..nobj {
                for j in 0..REFS {
                    // SAFETY: within the view, which is copy-on-write.
                    unsafe {
                        let p = img.add(i * OBJ / 4 + j);
                        p.write_volatile(p.read_volatile().wrapping_add(delta));
                    }
                }
            }
            t_reloc = Some(t.elapsed().as_secs_f64() * 1e6);
        }

        // Optionally ask the memory manager to bring the range in at once.
        let mut t_prefetch = 0.0;
        if a.flag("--prefetch") {
            let t = Instant::now();
            let e = WIN32_MEMORY_RANGE_ENTRY {
                VirtualAddress: addr as *mut c_void,
                NumberOfBytes: size,
            };
            // SAFETY: the range is our view.
            unsafe {
                let _ = PrefetchVirtualMemory(me, &[e], 0);
            }
            t_prefetch = t.elapsed().as_secs_f64() * 1e6;
        }
        // Touch every page and check that references are valid here.
        let t = Instant::now();
        let mut bad = 0usize;
        for pg in 0..size / PAGE {
            // SAFETY: within the view.
            let v = unsafe { img.add(pg * PAGE / 4).read_volatile() };
            if v as usize != addr + pg * PAGE {
                bad += 1;
            }
        }
        let t_touch = t.elapsed().as_secs_f64() * 1e6;
        out += &format!(" t_prefetch_us={t_prefetch:.1}");
        let t = Instant::now();
        let mut o = 0usize;
        for _ in 0..200_000 {
            // SAFETY: within the view; `next` is validated before use.
            let next = unsafe { img.add(o / 4 + 1).read_volatile() } as usize;
            if next < addr || next >= addr + size || (next - addr) % OBJ != 0 {
                bad += 1;
                break;
            }
            o = next - addr;
        }
        out += &format!(" t_chase_us={:.1}", t.elapsed().as_secs_f64() * 1e6);

        // Dirty some pages (class statics, as ART does in its boot image).
        let t = Instant::now();
        for pg in DIRTY_FIRST_PAGE..DIRTY_FIRST_PAGE + dirty {
            // SAFETY: within the copy-on-write part of the view.
            unsafe { img.add(pg * PAGE / 4 + 3).write_volatile(pg as u32) };
        }
        let t_dirty = t.elapsed().as_secs_f64() * 1e6;

        out += &format!(
            " free={free} reloc={reloc} addr={addr:#x} t_res_us={t_res:.1} t_map_us={t_map:.1} t_prot_us={t_prot:.1} t_touch_us={t_touch:.1} t_dirty_us={t_dirty:.1} bad={bad}"
        );
        if let Some(r) = t_reloc {
            out += &format!(" t_reloc_us={r:.1}");
        }
    }
    out += &format!(" t_main_us={:.1}", t_main.elapsed().as_secs_f64() * 1e6);
    println!("{out}");
    io::stdout().flush()?;
    // SAFETY: waits on the inherited exit event.
    unsafe { WaitForSingleObject(exit, INFINITE) };
    Ok(())
}

// ------------------------------------------------------------------- ASLR

/// Starts many processes and records which parts of the low 4 GiB are in
/// use, both at `main` entry and while the process is still suspended.
pub fn aslr(a: &Args) -> io::Result<()> {
    let runs: usize = a.num("--runs", 100);
    let base: usize = a.hex("--base", 0x6000_0000);
    let size: usize = a.num("--size-mib", 320) << 20;
    let n_ac_every: usize = a.num("--appcontainer-every", 4);
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let ac_name = "andruna.s2.aslr";
    win::appcontainer_profile(ac_name)?;
    let dir = std::env::current_exe()?
        .parent()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let _ = std::process::Command::new("icacls")
        .args([&dir, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)"])
        .stdout(std::process::Stdio::null())
        .status()?;
    // Occupancy per 16 MiB bucket below 4 GiB, at main and while suspended.
    let mut occ_main = vec![0usize; 256];
    let mut occ_susp = vec![0usize; 256];
    let mut free_main = 0;
    let mut free_susp = 0;
    let mut low_bytes_main = Vec::new();
    let mut ac_runs = 0;
    for i in 0..runs {
        let in_ac = n_ac_every > 0 && i % n_ac_every == 0;
        ac_runs += in_ac as usize;
        let mut rd = HANDLE::default();
        let mut wr = HANDLE::default();
        // SAFETY: anonymous pipe for the report.
        unsafe { CreatePipe(&mut rd, &mut wr, None, 0).map_err(win::werr)? };
        let rd = Owned(rd);
        let wr = Owned(wr);
        win::set_inherit(wr.0, true);
        let cmd = format!("\"{exe}\" probe --base {base:#x} --size {size}");
        let c = win::spawn(
            &cmd,
            if in_ac { Some(ac_name) } else { None },
            true,
            wr.0,
            &[],
        )?;
        drop(wr);
        if win::range_free(c.process.0, base, size) {
            free_susp += 1;
        }
        let (_, _, hits) = win::low_regions(c.process.0, 0, 0x1_0000_0000);
        mark(&mut occ_susp, &hits);
        // SAFETY: resumes the child we created.
        unsafe { ResumeThread(c.thread.0) };
        let line = read_line(rd.0)?;
        let r = parse_report(&line);
        if r.get("free").map(|v| v == "true").unwrap_or(false) {
            free_main += 1;
        }
        let regions: Vec<(usize, usize)> = r
            .get("regions")
            .map(|s| {
                s.split(',')
                    .filter_map(|e| e.split_once(':'))
                    .filter_map(|(b, l)| {
                        Some((
                            usize::from_str_radix(b, 16).ok()?,
                            usize::from_str_radix(l, 16).ok()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        low_bytes_main.push(regions.iter().map(|r| r.1).sum::<usize>() as f64 / 1048576.0);
        mark(&mut occ_main, &regions);
        // SAFETY: waits on our child.
        unsafe { WaitForSingleObject(c.process.0, 5000) };
    }
    win::delete_appcontainer_profile(ac_name);
    println!("# ASLR probe: {runs} launches ({ac_runs} in an AppContainer), fixed range {base:#x}+{} MiB", size >> 20);
    println!(
        "fixed range free while suspended: {free_susp}/{runs}; at main entry: {free_main}/{runs}"
    );
    println!(
        "bytes allocated below 4 GiB at main entry: median {:.1} MiB, max {:.1} MiB",
        pct(&mut low_bytes_main.clone(), 0.5),
        pct(&mut low_bytes_main, 1.0)
    );
    println!("16 MiB buckets below 4 GiB used in at least one launch (bucket start: launches at main / while suspended):");
    for b in 0..256 {
        if occ_main[b] > 0 || occ_susp[b] > 0 {
            println!("  {:#010x}: {} / {}", b << 24, occ_main[b], occ_susp[b]);
        }
    }
    Ok(())
}

fn mark(occ: &mut [usize], regions: &[(usize, usize)]) {
    let mut seen = [false; 256];
    for &(b, l) in regions {
        let first = b >> 24;
        let last = ((b + l - 1) >> 24).min(255);
        for s in seen.iter_mut().take(last + 1).skip(first) {
            *s = true;
        }
    }
    for (o, s) in occ.iter_mut().zip(seen) {
        *o += s as usize;
    }
}

pub fn probe(a: &Args) -> io::Result<()> {
    let base: usize = a.hex("--base", 0);
    let size: usize = a.num("--size", 0);
    // SAFETY: pseudo handle of this process.
    let me = unsafe { GetCurrentProcess() };
    let free = win::range_free(me, base, size);
    let (_, _, hits) = win::low_regions(me, 0, 0x1_0000_0000);
    let regions: Vec<String> = hits.iter().map(|(b, l)| format!("{b:x}:{l:x}")).collect();
    println!("READY free={free} regions={}", regions.join(","));
    io::stdout().flush()
}

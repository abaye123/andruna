//! Exceptions and debugging support for guest ELF code.
//!
//! - The first vectored exception handler: lazy FS-base restore (x64), then
//!   a crash report for faults in guest code (object, offset, symbol, and a
//!   frame-pointer walk), since Windows debuggers know nothing about ELF.
//! - `lldb-load.txt`: the `target modules add/load` commands that let LLDB
//!   (llvm-mingw's `lldb.exe`) show guest symbols, DWARF lines and unwind
//!   guest frames from `.eh_frame` (S8.md, "Debugging").
//! - Optional: Windows unwind tables for guest code translated from
//!   `.eh_frame` (`crate::unwind`), so `RtlVirtualUnwind` and Windows
//!   debuggers can walk through guest frames.

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::Once;

use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

use crate::loader::Object;

#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

/// Address ranges and symbols of the loaded guest objects, for the
/// handler (which must not take locks that guest code may hold; this one
/// is only written during loading).
struct Range {
    base: u64,
    end: u64,
    bias: u64,
    name: String,
    syms: Vec<(u64, u64, String)>,
}

static RANGES: Mutex<Vec<Range>> = Mutex::new(Vec::new());

pub fn register_objects(objs: &[Object]) {
    let mut r = RANGES.lock().unwrap();
    let mut cmds = String::new();
    for o in objs {
        r.push(Range {
            base: o.base,
            end: o.base + o.size,
            bias: o.bias,
            name: o.name.clone(),
            syms: o.local_symbols.clone(),
        });
        let p = o.path.display().to_string().replace('\\', "/");
        cmds.push_str(&format!("target modules add \"{p}\"\n"));
        cmds.push_str(&format!(
            "target modules load --file \"{}\" --slide {:#x}\n",
            o.name, o.bias
        ));
    }
    if let Ok(dir) = std::env::var("S8_LLDB_FILE") {
        let _ = std::fs::write(dir, cmds);
    }
    // The same for WinDbg/cdb: `.reload` of each object at its base, for
    // the debugger's ELF/DWARF symbol support (S8.md, "Debugging").
    if let Ok(f) = std::env::var("S8_WINDBG_FILE") {
        let mut w = String::new();
        for o in objs {
            let dir = o
                .path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            w.push_str(&format!(
                ".sympath+ {dir}\n.reload /f /i {}={:#x},{:#x}\n",
                o.name, o.base, o.size
            ));
        }
        let _ = std::fs::write(f, w);
    }
    if std::env::var_os("S8_UNWIND").is_some() {
        for o in objs {
            match crate::unwind::register(o) {
                Ok(n) => eprintln!("[s8] {}: {n} Windows unwind entries registered", o.name),
                Err(e) => eprintln!("[s8] {}: unwind registration failed: {e}", o.name),
            }
        }
    }
}

/// `object+offset (symbol+off)` for an address in guest code.
pub fn describe(addr: u64) -> Option<String> {
    let r = RANGES.try_lock().ok()?;
    let o = r.iter().find(|o| addr >= o.base && addr < o.end)?;
    let off = addr - o.bias;
    let i = o.syms.partition_point(|s| s.0 <= off);
    let sym = i
        .checked_sub(1)
        .and_then(|i| o.syms.get(i))
        .filter(|s| off < s.0 + s.1.max(1))
        .map(|s| format!(" ({}+{:#x})", s.2, off - s.0))
        .unwrap_or_default();
    Some(format!("{}+{:#x}{}", o.name, off, sym))
}

static INSTALL: Once = Once::new();

pub fn install_handler() {
    INSTALL.call_once(|| {
        // SAFETY: a valid handler that is never removed.
        let h = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
        assert!(!h.is_null(), "AddVectoredExceptionHandler failed");
    });
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid records.
    let (rec, ctx) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    let code = rec.ExceptionCode.0 as u32;
    #[cfg(target_arch = "x86_64")]
    let (pc, fp, sp) = (ctx.Rip, ctx.Rbp, ctx.Rsp);
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the ARM64 CONTEXT's named registers.
    let (pc, fp, sp) = (ctx.Pc, unsafe { ctx.Anonymous.Anonymous.Fp }, ctx.Sp);
    #[cfg(target_arch = "x86_64")]
    if code == 0xC000_0005 && crate::tls::lazy_restore(pc) {
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: the system passes a valid, writable context record.
        let ctx = unsafe { &mut *(*info).ContextRecord };
        let mut p = [0usize; 15];
        for (i, v) in rec.ExceptionInformation.iter().enumerate() {
            p[i] = *v;
        }
        if crate::tls::current_tp() != 0 && crate::host::signal::deliver(code, &p, rec.NumberParameters, ctx) {
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }
    // Only report faults in guest code, and only real faults (debuggers
    // and the C runtime use other codes).
    let fatal = matches!(code, 0xC000_0005 | 0xC000_001D | 0xC000_0094 | 0xC000_00FD);
    if fatal && std::env::var_os("S8_NO_CRASH_REPORT").is_none() {
        if let Some(d) = describe(pc) {
            let mut e = std::io::stderr().lock();
            let _ = writeln!(e, "[s8] guest fault {code:#x} at {pc:#x} {d}");
            if code == 0xC000_0005 {
                let _ = writeln!(
                    e,
                    "[s8]   {} address {:#x}",
                    if rec.ExceptionInformation[0] == 1 {
                        "write to"
                    } else {
                        "read from"
                    },
                    rec.ExceptionInformation[1]
                );
            }
            let _ = writeln!(e, "[s8] guest frames (frame-pointer walk):");
            walk(&mut e, pc, fp, sp);
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

fn walk(e: &mut impl std::io::Write, pc: u64, mut fp: u64, _sp: u64) {
    let _ = writeln!(
        e,
        "[s8]   #0 {pc:#x} {}",
        describe(pc).unwrap_or_else(|| "(host)".into())
    );
    for i in 1..32 {
        if fp == 0 || fp & 7 != 0 {
            break;
        }
        // Only follow frame pointers that look like stack addresses of this
        // thread: read through a guarded helper.
        let Some((next, ret)) = read_frame(fp) else {
            break;
        };
        let _ = writeln!(
            e,
            "[s8]   #{i} {ret:#x} {}",
            describe(ret).unwrap_or_else(|| "(host)".into())
        );
        if next <= fp {
            break;
        }
        fp = next;
    }
}

fn read_frame(fp: u64) -> Option<(u64, u64)> {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: queries the address space only.
    let n = unsafe { VirtualQuery(Some(fp as *const _), &mut mbi, core::mem::size_of_val(&mbi)) };
    if n == 0 || mbi.State != MEM_COMMIT || fp + 16 > mbi.BaseAddress as u64 + mbi.RegionSize as u64
    {
        return None;
    }
    // SAFETY: committed memory, checked above.
    unsafe { Some((*(fp as *const u64), *((fp + 8) as *const u64))) }
}

/// Prints the guest frames of the current thread (for `abort`).
pub fn print_guest_backtrace() {
    let fp: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads rbp.
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) fp)
    };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: reads x29.
    unsafe {
        core::arch::asm!("mov {}, x29", out(reg) fp)
    };
    let mut e = std::io::stderr().lock();
    walk(&mut e, 0, fp, 0);
}

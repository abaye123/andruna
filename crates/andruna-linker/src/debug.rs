//! Exceptions and debugging support for guest ELF code.
//!
//! - The first vectored exception handler: the lazy FS-base restore (x64,
//!   [`crate::tls::lazy_restore`]), then a crash report for faults in guest
//!   code (object, offset, nearest `.symtab` symbol, frame-pointer walk),
//!   since Windows debuggers know nothing about ELF.
//! - LLDB: when `ANDRUNA_LLDB_FILE` names a file, the loader keeps the
//!   `target modules add` / `target modules load --slide` commands for every
//!   loaded object in it; `command source <file>` in llvm-mingw's
//!   `lldb.exe` then shows guest symbols, DWARF lines and unwinds guest
//!   frames (S8.md, "Debugging").

use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, Once};

use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

use crate::object::Object;

const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

struct Range {
    base: u64,
    end: u64,
    bias: u64,
    name: String,
    path: String,
    syms: Vec<(u64, u64, String)>,
}

/// Address ranges of loaded objects, for the handler (which only uses
/// `try_lock`, so a fault while the list is being changed cannot
/// deadlock).
static RANGES: Mutex<Vec<Range>> = Mutex::new(Vec::new());

/// Prints a loader diagnostic on standard error.
pub fn report(args: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "[andruna-linker] {args}");
}

/// `ANDRUNA_TRACE`: loader progress on standard error.
pub fn tracing() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("ANDRUNA_TRACE").is_some())
}

pub fn trace(args: std::fmt::Arguments<'_>) {
    if tracing() {
        report(args);
    }
}

pub fn register(o: &Object) {
    let mut r = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    r.push(Range {
        base: o.image.base,
        end: o.image.base + o.image.size,
        bias: o.image.bias,
        name: o.name.clone(),
        path: o.path.clone(),
        syms: o.local_symbols.clone(),
    });
    write_lldb_file(&r);
}

pub fn unregister(o: &Object) {
    let mut r = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|x| x.base != o.image.base);
    write_lldb_file(&r);
}

fn write_lldb_file(r: &[Range]) {
    let Some(f) = std::env::var_os("ANDRUNA_LLDB_FILE") else {
        return;
    };
    let mut cmds = String::new();
    for o in r {
        let p = o.path.replace('\\', "/");
        cmds.push_str(&format!("target modules add \"{p}\"\n"));
        cmds.push_str(&format!(
            "target modules load --file \"{}\" --slide {:#x}\n",
            Path::new(&p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| o.name.clone()),
            o.bias
        ));
    }
    let _ = std::fs::write(f, cmds);
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

/// Installs the first vectored exception handler (once per process).
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
    let (pc, fp) = (ctx.Rip, ctx.Rbp);
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the ARM64 CONTEXT's named registers.
    let (pc, fp) = (ctx.Pc, unsafe { ctx.Anonymous.Anonymous.Fp });
    #[cfg(target_arch = "x86_64")]
    if code == 0xC000_0005 && crate::tls::lazy_restore(pc) {
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    // Guest signal handlers (andruna-lx) come before the crash report.
    // SAFETY: called from the vectored handler with the system's records.
    if unsafe { andruna_lx::signal::handle_exception(info) } {
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    let fatal = matches!(
        code,
        0xC000_0005 | 0xC000_001D | 0xC000_0094 | 0xC000_00FD | 0xC000_0096
    );
    if fatal
        && std::env::var_os("ANDRUNA_NO_CRASH_REPORT").is_none()
        && let Some(d) = describe(pc)
    {
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, "[andruna-linker] guest fault {code:#x} at {pc:#x} {d}");
        if code == 0xC000_0005 {
            let _ = writeln!(
                e,
                "[andruna-linker]   {} address {:#x}",
                if rec.ExceptionInformation[0] == 1 {
                    "write to"
                } else {
                    "read from"
                },
                rec.ExceptionInformation[1]
            );
        }
        walk(&mut e, fp);
    } else if fatal && tracing() {
        // A fault in host code (ANDRUNA_TRACE): the guest frames above it
        // are still worth seeing.
        let mut e = std::io::stderr().lock();
        let addr = rec.ExceptionInformation[1];
        let _ = writeln!(
            e,
            "[andruna-linker] host fault {code:#x} at {pc:#x} (address {addr:#x}) on thread {}",
            andruna_lx::process::gettid()
        );
        walk(&mut e, fp);
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Guest code keeps frame pointers (Android's default on both
/// architectures); follows them while they look like stack frames.
fn walk(e: &mut impl std::io::Write, mut fp: u64) {
    for i in 1..32 {
        if fp == 0 || fp & 7 != 0 {
            break;
        }
        let Some((next, ret)) = read_frame(fp) else {
            break;
        };
        let _ = writeln!(
            e,
            "[andruna-linker]   #{i} {ret:#x} {}",
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
    if n == 0
        || mbi.State != MEM_COMMIT
        || mbi.Protect.0 & 0x66 == 0
        || fp + 16 > mbi.BaseAddress as u64 + mbi.RegionSize as u64
    {
        return None;
    }
    // SAFETY: committed, readable memory (checked above).
    unsafe { Some((*(fp as *const u64), *((fp + 8) as *const u64))) }
}

/// Function symbols of `.symtab` (not loaded at run time; read from the
/// file), sorted by address, for crash reports.
pub fn read_symtab(path: &Path) -> Vec<(u64, u64, String)> {
    use crate::elf::*;
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut fh) = std::fs::File::open(path) else {
        return vec![];
    };
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if len > 256 << 20 {
            return None;
        }
        let mut v = vec![0u8; len];
        fh.seek(SeekFrom::Start(off)).ok()?;
        fh.read_exact(&mut v).ok()?;
        Some(v)
    };
    let Some(head) = read_at(0, 64) else {
        return vec![];
    };
    let Some(eh) = read_at_t::<Ehdr>(&head, 0) else {
        return vec![];
    };
    if eh.e_shoff == 0 || eh.e_shentsize as usize != 64 {
        return vec![];
    }
    let Some(sh) = read_at(eh.e_shoff, eh.e_shnum as usize * 64) else {
        return vec![];
    };
    let shdr = |i: usize| read_at_t::<Shdr>(&sh, (i * 64) as u64);
    let mut out = vec![];
    for i in 0..eh.e_shnum as usize {
        let Some(s) = shdr(i) else { break };
        if s.sh_type != SHT_SYMTAB {
            continue;
        }
        let Some(strs) = shdr(s.sh_link as usize) else {
            continue;
        };
        let (Some(syms), Some(names)) = (
            read_at(s.sh_offset, s.sh_size as usize),
            read_at(strs.sh_offset, strs.sh_size as usize),
        ) else {
            continue;
        };
        for k in 0..syms.len() / 24 {
            let Some(sym) = read_at_t::<Sym>(&syms, (k * 24) as u64) else {
                break;
            };
            if sym.kind() != STT_FUNC || sym.st_value == 0 {
                continue;
            }
            let so = sym.st_name as usize;
            if so >= names.len() {
                continue;
            }
            let end = names[so..].iter().position(|&b| b == 0).unwrap_or(0);
            out.push((
                sym.st_value,
                sym.st_size,
                String::from_utf8_lossy(&names[so..so + end]).into_owned(),
            ));
        }
    }
    out.sort();
    out
}

fn read_at_t<T: Copy>(b: &[u8], off: u64) -> Option<T> {
    crate::elf::read_at(b, off)
}

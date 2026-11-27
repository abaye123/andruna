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
    if code == 0x8000_0004 && WATCH_ON.load(core::sync::atomic::Ordering::Relaxed) {
        // A data breakpoint of S1_WATCH_THREADS: report writes that leave
        // a value no suspend count can have.
        let dr6 = ctx.Dr6;
        for i in 0..4 {
            if dr6 & (1 << i) != 0 {
                let addr = [ctx.Dr0, ctx.Dr1, ctx.Dr2, ctx.Dr3][i];
                // SAFETY: a watched, live ART Thread field.
                let v = unsafe { core::ptr::read_volatile(addr as *const i32) };
                if !(0..=64).contains(&v) {
                    let mut e = std::io::stderr().lock();
                    let _ = writeln!(
                        e,
                        "[s1] watch: {addr:#x} = {v:#x} written at {pc:#x} {}",
                        describe(pc)
                            .unwrap_or_else(|| "(not in a loaded object: JIT code?)".into())
                    );
                    walk(&mut e, pc, fp, sp);
                    dump_code(&mut e, pc);
                }
            }
        }
        // SAFETY: the system passes a valid, writable context record.
        unsafe { (*(*info).ContextRecord).Dr6 = 0 };
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    #[cfg(target_arch = "aarch64")]
    if code == 0xC000_001D {
        // SAFETY: the system passes a valid, writable context record.
        let ctx = unsafe { &mut *(*info).ContextRecord };
        if emulate_cache_op(ctx) {
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }
    {
        // SAFETY: the system passes a valid, writable context record.
        let ctx = unsafe { &mut *(*info).ContextRecord };
        let mut p = [0usize; 15];
        for (i, v) in rec.ExceptionInformation.iter().enumerate() {
            p[i] = *v;
        }
        if crate::tls::current_tp() != 0
            && crate::host::signal::deliver(code, &p, rec.NumberParameters, ctx)
        {
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }
    // Only report faults in guest code, and only real faults (debuggers
    // and the C runtime use other codes).
    let fatal = matches!(code, 0xC000_0005 | 0xC000_001D | 0xC000_0094 | 0xC000_00FD);
    // A fault on a guest thread is reported even when the PC is in host
    // code or in JIT code (no loaded object): the unclaimed fault is about
    // to end the process.
    let guest_thread = crate::tls::current_tp() != 0;
    if fatal
        && std::env::var_os("S8_NO_CRASH_REPORT").is_none()
        && let Some(d) =
            describe(pc).or_else(|| guest_thread.then(|| "(host or JIT code)".to_string()))
    {
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
        // SAFETY: the base of this executable (for host PCs).
        let exe = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
            .map(|h| h.0 as u64)
            .unwrap_or(0);
        let _ = writeln!(e, "[s8]   runner image at {exe:#x}, sp {sp:#x}");
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: the ARM64 CONTEXT's named registers.
            let lr = unsafe { ctx.Anonymous.Anonymous.Lr };
            let _ = writeln!(e, "[s8]   lr {lr:#x} {}", describe(lr).unwrap_or_default());
        }
        let _ = writeln!(e, "[s8] guest frames (frame-pointer walk):");
        walk(&mut e, pc, fp, sp);
    }
    EXCEPTION_CONTINUE_SEARCH
}

#[cfg(target_arch = "x86_64")]
static WATCH_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// 32 bytes before `pc` (the writing instruction ends at `pc`).
#[cfg(target_arch = "x86_64")]
fn dump_code(e: &mut impl std::io::Write, pc: u64) {
    let mut line = String::new();
    for a in (pc - 32..pc + 8).step_by(8) {
        if let Some((w, _)) = read_frame(a) {
            for b in w.to_le_bytes() {
                line.push_str(&format!("{b:02x} "));
            }
        }
    }
    let _ = writeln!(e, "[s1]   code from {:#x}: {line}", pc - 32);
}

/// Debug aid (`S1_WATCH_THREADS=1`, x64): hardware write breakpoints on
/// `tls32_.suspend_count` of the first four ART threads, set on every
/// thread of the process and refreshed every 20 ms; the exception handler
/// reports writes of implausible values with the writing PC.
#[cfg(target_arch = "x86_64")]
pub fn start_thread_watch() {
    use windows::Win32::System::Diagnostics::Debug::{
        CONTEXT, CONTEXT_DEBUG_REGISTERS_AMD64, GetThreadContext, SetThreadContext,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, GetCurrentThreadId, OpenThread, ResumeThread, SuspendThread,
        THREAD_ALL_ACCESS,
    };
    if std::env::var_os("S1_WATCH_THREADS").is_none() {
        return;
    }
    WATCH_ON.store(true, core::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(|| {
        let me = unsafe { GetCurrentThreadId() };
        let pid = unsafe { GetCurrentProcessId() };
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let mut addrs = vec![];
            for (_, tp) in crate::host::threads::GUEST_TPS.lock().unwrap().iter() {
                if let Some((t, _)) = read_frame(tp + 7 * 8)
                    && t != 0
                    && addrs.len() < 4
                {
                    addrs.push(t + 4);
                }
            }
            if addrs.is_empty() {
                continue;
            }
            // SAFETY: snapshot and thread APIs on threads of this process;
            // only the debug registers are changed.
            unsafe {
                let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
                    continue;
                };
                let mut te = THREADENTRY32 {
                    dwSize: core::mem::size_of::<THREADENTRY32>() as u32,
                    ..Default::default()
                };
                let mut ok = Thread32First(snap, &mut te).is_ok();
                while ok {
                    if te.th32OwnerProcessID == pid
                        && te.th32ThreadID != me
                        && let Ok(h) = OpenThread(THREAD_ALL_ACCESS, false, te.th32ThreadID)
                    {
                        SuspendThread(h);
                        #[repr(C, align(16))]
                        struct A(CONTEXT);
                        let mut a = A(CONTEXT {
                            ContextFlags: CONTEXT_DEBUG_REGISTERS_AMD64,
                            ..Default::default()
                        });
                        if GetThreadContext(h, &mut a.0).is_ok() {
                            let c = &mut a.0;
                            let mut dr7 = 0u64;
                            for (i, addr) in addrs.iter().enumerate() {
                                match i {
                                    0 => c.Dr0 = *addr,
                                    1 => c.Dr1 = *addr,
                                    2 => c.Dr2 = *addr,
                                    _ => c.Dr3 = *addr,
                                }
                                // Local enable, RW = 01 (writes), LEN = 11 (4 bytes).
                                dr7 |= 1 << (2 * i);
                                dr7 |= 0b1101 << (16 + 4 * i);
                            }
                            c.Dr7 = dr7;
                            let _ = SetThreadContext(h, c);
                        }
                        ResumeThread(h);
                    }
                    ok = Thread32Next(snap, &mut te).is_ok();
                }
            }
        }
    });
}

/// Guest code built for Linux keeps the instruction cache coherent itself
/// (compiler-rt's `__clear_cache`: `mrs ctr_el0`, `dc cvau`, `ic ivau`).
/// Should Windows trap these at EL0, they are emulated here: the cache
/// type register reads as 64-byte lines with IDC/DIC clear, and the
/// maintenance operations become `FlushInstructionCache` of that line.
/// Returns false for any other instruction.
#[cfg(target_arch = "aarch64")]
fn emulate_cache_op(ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT) -> bool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows::Win32::System::Threading::GetCurrentProcess;
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if describe(ctx.Pc).is_none() {
        return false;
    }
    // SAFETY: the faulting instruction of guest code, mapped and readable.
    let insn = unsafe { *(ctx.Pc as *const u32) };
    let rt = (insn & 31) as usize;
    // SAFETY: the X array view of the register union.
    let x = unsafe { &mut ctx.Anonymous.X };
    match insn & !31 {
        // mrs Xt, ctr_el0: DminLine = IminLine = 4 (16 words = 64 bytes).
        0xD53B_0020 => {
            if rt != 31 {
                x[rt] = 0x8444_c004;
            }
        }
        // dc cvau, Xt / ic ivau, Xt
        0xD50B_7B20 | 0xD50B_7520 => {
            let a = if rt == 31 { 0 } else { x[rt] & !63 };
            // SAFETY: flushing a range of this process.
            let _ = unsafe { FlushInstructionCache(GetCurrentProcess(), Some(a as *const _), 64) };
        }
        _ => return false,
    }
    if COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
        eprintln!("[s1] emulating an EL0 cache instruction ({insn:#010x}) trapped by Windows");
    }
    ctx.Pc += 4;
    true
}

/// For each guest thread: ART's `Thread*` from bionic's
/// `TLS_SLOT_ART_THREAD_SELF` (slot 7) and the head of its `tls32_`
/// (state_and_flags: flags in the low bits, state in the top byte;
/// suspend_count; thin lock id; tid).
fn dump_art_threads(e: &mut impl std::io::Write) {
    let tps = crate::host::threads::GUEST_TPS.lock().unwrap().clone();
    for (tid, tp) in tps {
        let Some((t, _)) = read_frame(tp + 7 * 8) else {
            continue;
        };
        if t == 0 {
            continue;
        }
        let (Some((w0, w1)), Some((w2, _))) = (read_frame(t), read_frame(t + 16)) else {
            continue;
        };
        let _ = writeln!(
            e,
            "[s1] art thread tid {tid} Thread*={t:#x} state_and_flags={:#010x} suspend_count={} thin_id={} tid={} daemon={} oom={}",
            w0 as u32,
            (w0 >> 32) as i32,
            w1 as u32,
            (w1 >> 32) as u32,
            w2 as u32,
            (w2 >> 32) as u32
        );
    }
}

/// A NUL-terminated printable string pointed to by the pointer at `at`,
/// when both are readable (diagnostics only).
pub fn guest_cstr_at(at: usize) -> Option<String> {
    let (p, _) = read_frame(at as u64)?;
    let mut out = String::new();
    for i in 0..64u64 {
        let (w, _) = read_frame((p + i) & !7)?;
        let b = (w >> (((p + i) & 7) * 8)) as u8;
        if b == 0 {
            return (!out.is_empty()).then_some(out);
        }
        if !(0x20..0x7f).contains(&b) {
            return None;
        }
        out.push(b as char);
    }
    None
}

pub fn walk(e: &mut impl std::io::Write, pc: u64, mut fp: u64, _sp: u64) {
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

/// Debug aid (`S1_WATCHDOG=<seconds>`): after the delay, prints the guest
/// frames of every other thread of the process (suspend, read the context,
/// frame-pointer walk) and exits with 124. For hangs.
pub fn start_watchdog() {
    #[cfg(target_arch = "x86_64")]
    use windows::Win32::System::Diagnostics::Debug::CONTEXT_FULL_AMD64 as CONTEXT_FULL;
    #[cfg(target_arch = "aarch64")]
    use windows::Win32::System::Diagnostics::Debug::CONTEXT_FULL_ARM64 as CONTEXT_FULL;
    use windows::Win32::System::Diagnostics::Debug::{CONTEXT, GetThreadContext};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, GetCurrentThreadId, OpenThread, SuspendThread, THREAD_ALL_ACCESS,
    };
    let Some(secs) = std::env::var("S1_WATCHDOG")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        // S1_WATCHDOG_QUIT=<s>: first send SIGQUIT, so that ART's signal
        // catcher prints its own thread dump (states, suspend counts,
        // held locks, Java stacks), and wait that long.
        if let Some(q) = std::env::var("S1_WATCHDOG_QUIT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            eprintln!("[s1] watchdog after {secs} s: SIGQUIT to the signal catcher");
            crate::host::signal::queue_signal(3);
            std::thread::sleep(std::time::Duration::from_secs(q));
        }
        let me = unsafe { GetCurrentThreadId() };
        let pid = unsafe { GetCurrentProcessId() };
        let mut e = std::io::stderr().lock();
        crate::host::lx::dump_futex_waits(&mut e);
        dump_art_threads(&mut e);
        let _ = writeln!(e, "[s1] watchdog after {secs} s: thread stacks");
        // SAFETY: snapshot APIs with valid arguments; threads of this process.
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
                return;
            };
            let mut te = THREADENTRY32 {
                dwSize: core::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut ok = Thread32First(snap, &mut te).is_ok();
            while ok {
                if te.th32OwnerProcessID == pid
                    && te.th32ThreadID != me
                    && let Ok(h) = OpenThread(THREAD_ALL_ACCESS, false, te.th32ThreadID)
                {
                    SuspendThread(h);
                    #[repr(C, align(16))]
                    struct A(CONTEXT);
                    let mut a = A(CONTEXT {
                        ContextFlags: CONTEXT_FULL,
                        ..Default::default()
                    });
                    if GetThreadContext(h, &mut a.0).is_ok() {
                        let c = &a.0;
                        #[cfg(target_arch = "x86_64")]
                        let (pc, fp, sp) = (c.Rip, c.Rbp, c.Rsp);
                        #[cfg(target_arch = "aarch64")]
                        let (pc, fp, sp) = (c.Pc, c.Anonymous.Anonymous.Fp, c.Sp);
                        let _ = writeln!(
                            e,
                            "[s1] thread {} pc {:#x} {}",
                            te.th32ThreadID,
                            pc,
                            describe(pc).unwrap_or_else(|| "(host or system)".into())
                        );
                        #[cfg(target_arch = "aarch64")]
                        {
                            let lr = c.Anonymous.Anonymous.Lr;
                            let _ = writeln!(
                                e,
                                "[s1]   lr {lr:#x} {}",
                                describe(lr).unwrap_or_default()
                            );
                        }
                        walk(&mut e, pc, fp, sp);
                        // Stack scan: guest code addresses on the stack
                        // (when the thread is stopped in host code).
                        let mut n = 0;
                        let mut p = sp & !7;
                        while n < 24 && p < sp + (256 << 10) {
                            let Some((v, _)) = read_frame(p) else { break };
                            if let Some(d) = describe(v) {
                                let _ = writeln!(e, "[s1]   stack {:#x}: {v:#x} {d}", p);
                                n += 1;
                            }
                            p += 8;
                        }
                    }
                }
                ok = Thread32Next(snap, &mut te).is_ok();
            }
        }
        std::process::exit(124);
    });
}

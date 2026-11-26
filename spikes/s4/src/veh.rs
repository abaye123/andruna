//! A tiny Vectored Exception Handler used to probe instructions that may
//! fault (#UD, #GP, access violations) and to measure trap-and-emulate cost.
//!
//! Protocol: a probe sequence loads the address of its resume label into a
//! scratch register (r11 on x64, x17 on arm64) and then executes the
//! instruction under test. If that instruction faults while the current
//! thread is armed, the handler records the exception code and resumes at
//! the label.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

const MODE_OFF: u8 = 0;
const MODE_PROBE: u8 = 1;
const MODE_EMULATE: u8 = 2;

thread_local! {
    static MODE: Cell<u8> = const { Cell::new(MODE_OFF) };
    static LAST_CODE: Cell<Option<u32>> = const { Cell::new(None) };
    /// Guest thread pointer for lazy FS restore (x64); 0 when disabled.
    static GUEST_TP: Cell<u64> = const { Cell::new(0) };
    static LAZY_RESTORES: Cell<u64> = const { Cell::new(0) };
}

/// Enables lazy FS restore on the current thread: FS base is set to `tp`
/// now, and every access violation raised by an FS-prefixed instruction while
/// FS base differs from `tp` rewrites FS base and re-executes the instruction.
#[cfg(target_arch = "x86_64")]
pub fn lazy_fs_enable(tp: u64) {
    install();
    GUEST_TP.with(|c| c.set(tp));
    crate::x64::wrfsbase(tp);
}

#[cfg(target_arch = "x86_64")]
pub fn lazy_fs_disable() {
    GUEST_TP.with(|c| c.set(0));
}

/// Number of lazy restores performed on the current thread.
#[cfg(target_arch = "x86_64")]
pub fn lazy_restores_here() -> u64 {
    LAZY_RESTORES.with(|c| c.get())
}

/// True when the instruction at `rip` carries an FS segment override prefix.
#[cfg(target_arch = "x86_64")]
fn has_fs_prefix(rip: u64) -> bool {
    for i in 0..15 {
        // SAFETY: rip points at the faulting instruction, which is mapped
        // executable memory; at most 15 bytes (the maximum length) are read.
        let b = unsafe { *((rip + i) as *const u8) };
        match b {
            0x64 => return true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 | 0x40..=0x4f => {
                continue
            }
            _ => return false,
        }
    }
    false
}

/// Value written into the result register by the emulate mode.
pub static EMULATE_VALUE: AtomicU64 = AtomicU64::new(0);
/// Number of exceptions the handler resolved.
pub static HANDLED: AtomicU64 = AtomicU64::new(0);

static INSTALL: Once = Once::new();

pub fn install() {
    INSTALL.call_once(|| {
        // SAFETY: registering a handler with a valid function pointer; it is
        // never removed, so it outlives every call.
        let h = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
        assert!(!h.is_null(), "AddVectoredExceptionHandler failed");
    });
}

/// Arms the current thread for one probe.
pub fn arm_probe() {
    install();
    LAST_CODE.with(|c| c.set(None));
    MODE.with(|m| m.set(MODE_PROBE));
}

/// Disarms and returns the exception code seen by the probe, if any.
pub fn take_probe() -> Option<u32> {
    MODE.with(|m| m.set(MODE_OFF));
    LAST_CODE.with(|c| c.take())
}

/// Arms the current thread to emulate faulting loads until `disarm`.
pub fn arm_emulate(value: u64) {
    install();
    EMULATE_VALUE.store(value, Ordering::Relaxed);
    MODE.with(|m| m.set(MODE_EMULATE));
}

pub fn disarm() {
    MODE.with(|m| m.set(MODE_OFF));
}

/// Formats an exception code for reports.
pub fn code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "STATUS_ACCESS_VIOLATION",
        0xC000_001D => "STATUS_ILLEGAL_INSTRUCTION",
        0xC000_0096 => "STATUS_PRIVILEGED_INSTRUCTION",
        0x8000_0003 => "STATUS_BREAKPOINT",
        _ => "other",
    }
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception and context records.
    let (rec, ctx) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
    let code = rec.ExceptionCode.0 as u32;

    #[cfg(target_arch = "x86_64")]
    if code == 0xC000_0005 {
        let tp = GUEST_TP.with(|c| c.get());
        if tp != 0 && crate::x64::rdfsbase() != tp && has_fs_prefix(ctx.Rip) {
            crate::x64::wrfsbase(tp);
            LAZY_RESTORES.with(|c| c.set(c.get() + 1));
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }

    let mode = MODE.with(|m| m.get());
    if mode == MODE_OFF {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    match mode {
        MODE_PROBE => {
            LAST_CODE.with(|c| c.set(Some(code)));
            MODE.with(|m| m.set(MODE_OFF));
            resume_at_label(ctx, None);
        }
        _ => {
            if code != 0xC000_0005 {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            resume_at_label(ctx, Some(EMULATE_VALUE.load(Ordering::Relaxed)));
        }
    }
    HANDLED.fetch_add(1, Ordering::Relaxed);
    EXCEPTION_CONTINUE_EXECUTION
}

#[cfg(target_arch = "x86_64")]
fn resume_at_label(
    ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT,
    rax: Option<u64>,
) {
    if let Some(v) = rax {
        ctx.Rax = v;
    }
    ctx.Rip = ctx.R11;
}

#[cfg(target_arch = "aarch64")]
fn resume_at_label(ctx: &mut windows::Win32::System::Diagnostics::Debug::CONTEXT, x0: Option<u64>) {
    // SAFETY: CONTEXT_0 is a plain union of two views of the same registers.
    unsafe {
        if let Some(v) = x0 {
            ctx.Anonymous.X[0] = v;
        }
        ctx.Pc = ctx.Anonymous.X[17];
    }
}

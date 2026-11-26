//! ARM64: hand-written machine code and Windows CONTEXT <-> Linux ucontext.
//!
//! Same structure as `arch_x64`. Written and compile-checked only; round 1
//! did not run it on ARM64 hardware.

use crate::linux_abi::aarch64 as lx;
use crate::linux_abi::{siginfo_t, stack_t};
use core::arch::global_asm;
use windows::Win32::System::Diagnostics::Debug::{
    RtlLookupFunctionEntry, RtlVirtualUnwind, CONTEXT, CONTEXT_CONTROL_ARM64, CONTEXT_FLAGS,
    CONTEXT_FULL_ARM64, UNW_FLAG_NHANDLER,
};

pub const CONTEXT_CONTROL: CONTEXT_FLAGS = CONTEXT_CONTROL_ARM64;
pub const CONTEXT_FULL: CONTEXT_FLAGS = CONTEXT_FULL_ARM64;

// The injection trampoline reserves 0x390 bytes for the CONTEXT at sp+0x20.
const _: () = assert!(core::mem::size_of::<CONTEXT>() == 0x390);

// ---------------------------------------------------------------------------
// Generated-like code blob (AAPCS64, as ART's arm64 code).
// ---------------------------------------------------------------------------
global_asm!(
    ".text",
    ".p2align 4",
    ".globl s1_jit_a64_begin",
    "s1_jit_a64_begin:",
    // u32 null_load(obj: x0)
    ".globl s1_jit_null_load",
    "s1_jit_null_load:",
    "    stp x29, x30, [sp, #-16]!",
    "    mov x29, sp",
    ".globl s1_jit_null_site",
    "s1_jit_null_site:",
    "    ldr w0, [x0, #8]",
    ".globl s1_jit_null_load_after",
    "s1_jit_null_load_after:",
    "    ldp x29, x30, [sp], #16",
    "    ret",
    ".Ls1_null_catch:",
    "    mov w0, #-1",
    "    ldp x29, x30, [sp], #16",
    "    ret",
    // NPE stub: the handler set LR = fault pc + 4 (as ART's arm64 handler
    // does) and PC here. Record LR, continue in the catch block.
    ".globl s1_jit_npe_stub",
    "s1_jit_npe_stub:",
    "    ldr x9, s1_jit_slot_npe",
    "    str x30, [x9]",
    "    b .Ls1_null_catch",
    // u64 so_entry(depth: x0, gap: x1): recursion with ART's arm64 implicit
    // stack overflow probe (`sub x16, sp, #reserved; ldr wzr, [x16]`). The
    // reserved gap comes in x1 (ART uses 8 KiB) so that other gaps can be
    // measured; x15 holds it through the recursion.
    ".globl s1_jit_so_entry",
    "s1_jit_so_entry:",
    "    mov x15, x1",
    "    stp x29, x30, [sp, #-16]!",
    "    mov x29, sp",
    "    ldr x9, s1_jit_slot_sp",
    "    mov x10, sp",
    "    str x10, [x9]",
    "    bl .Ls1_so_rec",
    "    ldp x29, x30, [sp], #16",
    "    ret",
    ".Ls1_so_rec:",
    "    sub x16, sp, x15",
    "    ldr wzr, [x16]",
    "    stp x29, x30, [sp, #-128]!",
    "    add x0, x0, #1",
    "    bl .Ls1_so_rec",
    "    ldp x29, x30, [sp], #128",
    "    ret",
    // u64 wso_entry(depth: x0): recursion without probes, 512-byte frames.
    ".globl s1_jit_wso_entry",
    "s1_jit_wso_entry:",
    "    stp x29, x30, [sp, #-16]!",
    "    mov x29, sp",
    "    ldr x9, s1_jit_slot_sp",
    "    mov x10, sp",
    "    str x10, [x9]",
    "    bl .Ls1_wso_rec",
    "    ldp x29, x30, [sp], #16",
    "    ret",
    ".Ls1_wso_rec:",
    "    stp x29, x30, [sp, #-512]!",
    "    add x0, x0, #1",
    "    bl .Ls1_wso_rec",
    "    ldp x29, x30, [sp]",
    "    add sp, sp, #512",
    "    ret",
    // Stack overflow stub: back to the entry frame, return depth (x0).
    ".globl s1_jit_so_stub",
    "s1_jit_so_stub:",
    "    ldr x9, s1_jit_slot_sp",
    "    ldr x10, [x9]",
    "    mov sp, x10",
    "    ldp x29, x30, [sp], #16",
    "    ret",
    // u32 unw_func(ptr: x0, callback: x1): canonical frame for packed
    // unwind data (stp x29, x30, [sp, #-32]!; mov x29, sp).
    ".p2align 4",
    ".globl s1_jit_unw_func",
    "s1_jit_unw_func:",
    "    stp x29, x30, [sp, #-32]!",
    "    mov x29, sp",
    ".globl s1_jit_unw_prolog_end",
    "s1_jit_unw_prolog_end:",
    "    add x9, sp, #32",
    "    ldr x10, s1_jit_slot_entry_sp",
    "    str x9, [x10]",
    "    str x0, [sp, #16]",
    "    cbz x1, .Ls1_unw_load",
    "    blr x1",
    ".Ls1_unw_load:",
    "    ldr x9, [sp, #16]",
    "    ldr w0, [x9]",
    ".globl s1_jit_unw_after",
    "s1_jit_unw_after:",
    "    ldp x29, x30, [sp], #32",
    "    ret",
    ".globl s1_jit_unw_func_end",
    "s1_jit_unw_func_end:",
    ".p2align 3",
    ".globl s1_jit_slot_npe",
    "s1_jit_slot_npe: .quad 0",
    ".globl s1_jit_slot_sp",
    "s1_jit_slot_sp: .quad 0",
    ".globl s1_jit_slot_entry_sp",
    "s1_jit_slot_entry_sp: .quad 0",
    ".globl s1_jit_a64_end",
    "s1_jit_a64_end:",
);

extern "C" {
    static s1_jit_a64_begin: u8;
    static s1_jit_a64_end: u8;
    static s1_jit_null_load: u8;
    static s1_jit_null_site: u8;
    static s1_jit_null_load_after: u8;
    static s1_jit_npe_stub: u8;
    static s1_jit_so_entry: u8;
    static s1_jit_wso_entry: u8;
    static s1_jit_so_stub: u8;
    static s1_jit_unw_func: u8;
    static s1_jit_unw_prolog_end: u8;
    static s1_jit_unw_after: u8;
    static s1_jit_unw_func_end: u8;
    static s1_jit_slot_npe: u8;
    static s1_jit_slot_sp: u8;
    static s1_jit_slot_entry_sp: u8;
}

pub use crate::arch_common::BlobLayout;

pub fn blob() -> (*const u8, BlobLayout) {
    // SAFETY: only the addresses of the assembler labels are taken.
    unsafe {
        let b = &s1_jit_a64_begin as *const u8;
        let off = |p: &u8| (p as *const u8 as usize) - (b as usize);
        (
            b,
            BlobLayout {
                len: off(&s1_jit_a64_end),
                null_load: off(&s1_jit_null_load),
                null_site: off(&s1_jit_null_site),
                null_load_after: off(&s1_jit_null_load_after),
                npe_stub: off(&s1_jit_npe_stub),
                so_entry: off(&s1_jit_so_entry),
                wso_entry: off(&s1_jit_wso_entry),
                so_stub: off(&s1_jit_so_stub),
                unw_func: off(&s1_jit_unw_func),
                unw_prolog_end: off(&s1_jit_unw_prolog_end),
                unw_after: off(&s1_jit_unw_after),
                unw_func_end: off(&s1_jit_unw_func_end),
                slot_npe: off(&s1_jit_slot_npe),
                slot_sp: off(&s1_jit_slot_sp),
                slot_entry_sp: off(&s1_jit_slot_entry_sp),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Image-resident helpers. x18 points to the TEB on Windows ARM64; NT_TIB
// StackBase is at +8 and StackLimit at +16.
// ---------------------------------------------------------------------------
global_asm!(
    ".text",
    ".p2align 4",
    ".globl s1_call_on_stack",
    ".seh_proc s1_call_on_stack",
    "s1_call_on_stack:",
    "    stp x29, x30, [sp, #-32]!",
    "    .seh_save_fplr_x 32",
    "    stp x19, x20, [sp, #16]",
    "    .seh_save_regp x19, 16",
    "    mov x29, sp",
    "    .seh_set_fp",
    "    .seh_endprologue",
    "    ldr x19, [x18, #8]",
    "    ldr x20, [x18, #16]",
    "    mov sp, x3",
    "    str x3, [x18, #8]",
    "    str x4, [x18, #16]",
    "    blr x2",
    "    str x19, [x18, #8]",
    "    str x20, [x18, #16]",
    "    .seh_startepilogue",
    "    mov sp, x29",
    "    .seh_set_fp",
    "    ldp x19, x20, [sp, #16]",
    "    .seh_save_regp x19, 16",
    "    ldp x29, x30, [sp], #32",
    "    .seh_save_fplr_x 32",
    "    .seh_endepilogue",
    "    ret",
    ".seh_endproc",
    // Injection trampoline: SP was moved down and aligned by the requester.
    // RtlCaptureContext is reached with blr, which clobbers x16 and x30, and
    // x0 carries the context pointer: save those three (and x17) first.
    ".p2align 4",
    ".globl s1_inject_tramp",
    "s1_inject_tramp:",
    "    sub sp, sp, #0x3b0",
    "    stp x0, x30, [sp]",
    "    stp x16, x17, [sp, #16]",
    "    add x0, sp, #0x20",
    "    adrp x16, {cap}",
    "    ldr x16, [x16, :lo12:{cap}]",
    "    blr x16",
    "    add x0, sp, #0x20",
    "    mov x1, sp",
    "    bl {entry}",
    "    brk #0xf000",
    cap = sym crate::suspend::S1_RTL_CAPTURE_CONTEXT,
    entry = sym crate::suspend::s1_inject_entry,
);

extern "C" {
    pub fn s1_call_on_stack(
        a0: *mut core::ffi::c_void,
        a1: usize,
        f: unsafe extern "C" fn(*mut core::ffi::c_void, usize),
        top: usize,
        bottom: usize,
    );
    pub static s1_inject_tramp: u8;
}

// ---------------------------------------------------------------------------
// CONTEXT <-> ucontext_t
// ---------------------------------------------------------------------------

pub type UContext = lx::ucontext_t;

pub fn pc(ctx: &CONTEXT) -> usize {
    ctx.Pc as usize
}
pub fn sp(ctx: &CONTEXT) -> usize {
    ctx.Sp as usize
}
pub fn uc_pc(uc: &UContext) -> usize {
    uc.uc_mcontext.pc as usize
}
pub fn uc_set_pc(uc: &mut UContext, pc: usize) {
    uc.uc_mcontext.pc = pc as u64;
}
pub fn uc_sp(uc: &UContext) -> usize {
    uc.uc_mcontext.sp as usize
}
pub fn uc_set_ret(uc: &mut UContext, v: u64) {
    uc.uc_mcontext.regs[0] = v;
}

const ESR_EC_DABT_LOW: u64 = 0x24 << 26;
const ESR_IL: u64 = 1 << 25;
const ESR_WNR: u64 = 1 << 6;
const DFSC_TRANSLATION_L3: u64 = 0x07;
const DFSC_PERMISSION_L3: u64 = 0x0f;

pub fn context_to_ucontext(
    ctx: &CONTEXT,
    info: &siginfo_t,
    fault: Option<(usize, bool, bool)>,
    altstack: stack_t,
    uc: &mut UContext,
) {
    let mc = &mut uc.uc_mcontext;
    // SAFETY: the X array view of the register union is always valid.
    mc.regs = unsafe { ctx.Anonymous.X };
    mc.sp = ctx.Sp;
    mc.pc = ctx.Pc;
    mc.pstate = ctx.Cpsr as u64;
    mc.fault_address = info.si_addr as u64;
    let mut fp = lx::fpsimd_context {
        head: lx::aarch64_ctx {
            magic: lx::FPSIMD_MAGIC,
            size: core::mem::size_of::<lx::fpsimd_context>() as u32,
        },
        fpsr: ctx.Fpsr,
        fpcr: ctx.Fpcr,
        vregs: [0; 32],
    };
    for (i, v) in ctx.V.iter().enumerate() {
        // SAFETY: the Low/High view of a NEON register is always valid.
        let (lo, hi) = unsafe { (v.Anonymous.Low, v.Anonymous.High as u64) };
        fp.vregs[i] = (lo as u128) | ((hi as u128) << 64);
    }
    let esr = fault.map(|(_, write, present)| lx::esr_context {
        head: lx::aarch64_ctx {
            magic: lx::ESR_MAGIC,
            size: 16,
        },
        esr: ESR_EC_DABT_LOW
            | ESR_IL
            | if write { ESR_WNR } else { 0 }
            | if present {
                DFSC_PERMISSION_L3
            } else {
                DFSC_TRANSLATION_L3
            },
    });
    let r = &mut mc.reserved.0;
    let fsz = core::mem::size_of::<lx::fpsimd_context>();
    // SAFETY: the records are written inside the 4096-byte reserved area at
    // 16-byte aligned offsets, as the kernel lays them out; a zero header
    // terminates the list (the area was zeroed by the caller).
    unsafe {
        core::ptr::write(r.as_mut_ptr() as *mut lx::fpsimd_context, fp);
        if let Some(e) = esr {
            core::ptr::write(r.as_mut_ptr().add(fsz) as *mut lx::esr_context, e);
        }
    }
    uc.uc_flags = 0;
    uc.uc_link = core::ptr::null_mut();
    uc.uc_stack = altstack;
    uc.uc_sigmask = 0;
}

pub fn ucontext_to_context(uc: &UContext, ctx: &mut CONTEXT) {
    let mc = &uc.uc_mcontext;
    ctx.Anonymous.X = mc.regs;
    ctx.Sp = mc.sp;
    ctx.Pc = mc.pc;
    // Only NZCV is user-modifiable.
    ctx.Cpsr = (ctx.Cpsr & 0x0fff_ffff) | (mc.pstate as u32 & 0xf000_0000);
    // SAFETY: reads the first record of the reserved area.
    let fp = unsafe { core::ptr::read(mc.reserved.0.as_ptr() as *const lx::fpsimd_context) };
    if fp.head.magic == lx::FPSIMD_MAGIC {
        ctx.Fpsr = fp.fpsr;
        ctx.Fpcr = fp.fpcr;
        for (i, v) in ctx.V.iter_mut().enumerate() {
            v.Anonymous.Low = fp.vregs[i] as u64;
            v.Anonymous.High = (fp.vregs[i] >> 64) as u64 as i64;
        }
    }
}

// ---------------------------------------------------------------------------

/// Simulates a call from the faulting instruction: LR = `ret`, PC =
/// `target` (what ART's arm64 fault handler does).
pub fn redirect_call(ctx: &mut CONTEXT, ret: usize, target: usize) {
    // SAFETY: writing the X array view of the register union.
    unsafe { ctx.Anonymous.X[30] = ret as u64 };
    ctx.Pc = target as u64;
}

pub fn uc_redirect_call(uc: &mut UContext, ret: usize, target: usize) {
    uc.uc_mcontext.regs[30] = ret as u64;
    uc.uc_mcontext.pc = target as u64;
}

/// `udf #0` (SIGILL test). Integer division by zero does not trap on ARM64.
pub fn trap_insn_len(pc: usize) -> Option<usize> {
    // SAFETY: `pc` is the faulting instruction, mapped and readable.
    let w = unsafe { *(pc as *const u32) };
    (w == 0).then_some(4)
}

pub fn exec_ud2() -> u64 {
    let r: u64;
    // SAFETY: faults; the SIGILL handler skips it and sets x0.
    unsafe { core::arch::asm!("mov x0, #0", "udf #0", out("x0") r) };
    r
}

/// ARM64 integer division by zero returns 0 without a trap (on Linux too,
/// so there is nothing to deliver). Returns u64::MAX to mean "not
/// applicable".
pub fn exec_div0() -> u64 {
    u64::MAX
}

pub fn virtual_unwind(ctx: &CONTEXT) -> (bool, usize, usize) {
    let mut c = *ctx;
    let mut base = 0usize;
    // SAFETY: lookup only reads the registered function tables.
    let fe = unsafe { RtlLookupFunctionEntry(c.Pc as usize, &mut base, None) };
    if fe.is_null() {
        // SAFETY: reading the X array view.
        return (false, unsafe { c.Anonymous.X[30] } as usize, c.Sp as usize);
    }
    let mut hd: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut ef = 0usize;
    // SAFETY: `fe` belongs to `c.Pc`; `c` is a private copy.
    unsafe {
        RtlVirtualUnwind(
            UNW_FLAG_NHANDLER,
            base,
            c.Pc as usize,
            fe,
            &mut c,
            &mut hd,
            &mut ef,
            None,
        );
    }
    (true, c.Pc as usize, c.Sp as usize)
}

#[inline(never)]
pub fn manual_walk(out: &mut [usize]) -> usize {
    // SAFETY: CONTEXT is plain data; RtlCaptureContext fills it.
    let mut c: crate::suspend::AlignedContext = unsafe { core::mem::zeroed() };
    unsafe { windows::Win32::System::Diagnostics::Debug::RtlCaptureContext(&mut c.0) };
    let mut n = 0;
    while n < out.len() && c.0.Pc != 0 {
        out[n] = c.0.Pc as usize;
        n += 1;
        let mut base = 0usize;
        // SAFETY: as in `virtual_unwind`.
        let fe = unsafe { RtlLookupFunctionEntry(c.0.Pc as usize, &mut base, None) };
        if fe.is_null() {
            // SAFETY: reading the X array view.
            c.0.Pc = unsafe { c.0.Anonymous.X[30] };
        } else {
            let mut hd: *mut core::ffi::c_void = core::ptr::null_mut();
            let mut ef = 0usize;
            // SAFETY: `fe` belongs to `c.Pc`.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    base,
                    c.0.Pc as usize,
                    fe,
                    &mut c.0,
                    &mut hd,
                    &mut ef,
                    None,
                );
            }
        }
    }
    n
}

/// For `s1_jit_unw_func`: LR was saved at [entry_sp - 32 + 8].
pub fn expected_caller(entry_sp: usize) -> (usize, usize) {
    // SAFETY: the frame record is still on the live stack of the caller.
    let ret = unsafe { *((entry_sp - 24) as *const u64) } as usize;
    (ret, entry_sp)
}

pub fn inject_redirect(ctx: &mut CONTEXT, tramp: usize) -> (usize, usize) {
    let orig = (ctx.Pc as usize, ctx.Sp as usize);
    ctx.Sp = ((ctx.Sp as usize - 16) & !15) as u64;
    ctx.Pc = tramp as u64;
    orig
}

/// `extra` points at the trampoline's saved {x0, x30, x16, x17}.
pub fn inject_fixup(ctx: &mut CONTEXT, extra: usize, orig_pc: usize, orig_sp: usize) {
    // SAFETY: `extra` points into the trampoline frame (4 saved registers).
    let s = unsafe { core::slice::from_raw_parts(extra as *const u64, 4) };
    // SAFETY: writing the X array view.
    unsafe {
        ctx.Anonymous.X[0] = s[0];
        ctx.Anonymous.X[30] = s[1];
        ctx.Anonymous.X[16] = s[2];
        ctx.Anonymous.X[17] = s[3];
    }
    ctx.Pc = orig_pc as u64;
    ctx.Sp = orig_sp as u64;
}

/// The instruction before `pc` is `svc #imm`.
pub fn in_syscall(pc: usize) -> bool {
    // SAFETY: the word before a system-call return address is in the stub.
    let w = unsafe { *((pc - 4) as *const u32) };
    w & 0xffe0_001f == 0xd400_0001
}

pub fn ret_reg(ctx: &CONTEXT) -> u64 {
    // SAFETY: reading the X array view.
    unsafe { ctx.Anonymous.X[0] }
}

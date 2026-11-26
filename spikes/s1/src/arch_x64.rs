//! x86_64: hand-written machine code and Windows CONTEXT <-> Linux ucontext.
//!
//! Two kinds of code live here:
//! - A "generated-like" code blob (`s1_jit_x64_begin` .. `s1_jit_x64_end`)
//!   that is copied into a JIT buffer at run time, the way a JIT emits code.
//!   It uses the System V calling convention, as ART's x86_64 code does, and
//!   references only blob-internal labels (PC-relative), so it can be moved.
//! - Image-resident helpers (stack switch, injection trampoline, ABI
//!   trampolines, register-check harness) with SEH unwind directives.

use crate::linux_abi::x86_64 as lx;
use crate::linux_abi::{siginfo_t, stack_t};
use core::arch::global_asm;
use windows::Win32::System::Diagnostics::Debug::CONTEXT;

// ---------------------------------------------------------------------------
// Generated-like code blob (copied into the JIT buffer).
// ---------------------------------------------------------------------------
global_asm!(
    ".text",
    ".p2align 4",
    ".globl s1_jit_x64_begin",
    "s1_jit_x64_begin:",
    // u32 null_load(obj: rdi): the load is an implicit null check.
    ".globl s1_jit_null_load",
    "s1_jit_null_load:",
    "    mov eax, dword ptr [rdi + 8]",
    ".globl s1_jit_null_load_after",
    "s1_jit_null_load_after:",
    "    ret",
    ".Ls1_null_catch:",
    "    mov eax, -1",
    "    ret",
    // NPE stub: the fault handler pushed (fault pc + insn length) and jumped
    // here, as ART's x86_64 handler does before entering its throw stub. We
    // record the pushed address (ART uses it to find the dex pc) and jump to
    // the catch block of the same method.
    ".globl s1_jit_npe_stub",
    "s1_jit_npe_stub:",
    "    pop rcx",
    "    mov rax, qword ptr [rip + s1_jit_slot_npe]",
    "    mov qword ptr [rax], rcx",
    "    jmp .Ls1_null_catch",
    // u64 so_entry(depth: rdi): recursion with an ART-style implicit stack
    // overflow probe 8 KiB below SP in every frame.
    ".globl s1_jit_so_entry",
    "s1_jit_so_entry:",
    "    push rbx",
    "    mov rax, qword ptr [rip + s1_jit_slot_sp]",
    "    mov qword ptr [rax], rsp",
    "    call .Ls1_so_rec",
    "    pop rbx",
    "    ret",
    ".Ls1_so_rec:",
    "    test dword ptr [rsp - 0x2000], eax",
    "    sub rsp, 0x78",
    "    inc rdi",
    "    call .Ls1_so_rec",
    "    add rsp, 0x78",
    "    ret",
    // u64 wso_entry(depth: rdi): recursion without probes; each frame touches
    // its lowest word, so the stack grows through Windows' guard page until
    // the kernel raises STATUS_STACK_OVERFLOW.
    ".globl s1_jit_wso_entry",
    "s1_jit_wso_entry:",
    "    push rbx",
    "    mov rax, qword ptr [rip + s1_jit_slot_sp]",
    "    mov qword ptr [rax], rsp",
    "    call .Ls1_wso_rec",
    "    pop rbx",
    "    ret",
    ".Ls1_wso_rec:",
    "    sub rsp, 0x200",
    "    mov qword ptr [rsp], rdi",
    "    inc rdi",
    "    call .Ls1_wso_rec",
    "    add rsp, 0x200",
    "    ret",
    // Stack overflow stub: "throw StackOverflowError" to the entry frame by
    // restoring the SP saved at entry; returns the depth reached (rdi).
    ".globl s1_jit_so_stub",
    "s1_jit_so_stub:",
    "    mov rax, qword ptr [rip + s1_jit_slot_sp]",
    "    mov rsp, qword ptr [rax]",
    "    mov rax, rdi",
    "    pop rbx",
    "    ret",
    // u32 unw_func(ptr: rdi, callback: rsi): a framed function with a Windows
    // x64 prologue (push rbp; push rbx; sub rsp, 0x28). Calls the Windows-ABI
    // callback (if any) with rcx = ptr, then loads [ptr] (faults if null).
    ".p2align 4",
    ".globl s1_jit_unw_func",
    "s1_jit_unw_func:",
    "    push rbp",
    "    push rbx",
    "    sub rsp, 0x28",
    ".globl s1_jit_unw_prolog_end",
    "s1_jit_unw_prolog_end:",
    "    lea rax, [rsp + 0x38]",
    "    mov rcx, qword ptr [rip + s1_jit_slot_entry_sp]",
    "    mov qword ptr [rcx], rax",
    "    mov rbx, rdi",
    "    test rsi, rsi",
    "    jz .Ls1_unw_load",
    "    mov rcx, rdi",
    "    call rsi",
    ".Ls1_unw_load:",
    "    mov eax, dword ptr [rbx]",
    ".globl s1_jit_unw_after",
    "s1_jit_unw_after:",
    "    add rsp, 0x28",
    "    pop rbx",
    "    pop rbp",
    "    ret",
    ".globl s1_jit_unw_func_end",
    "s1_jit_unw_func_end:",
    // Pointer slots (patched before the buffer becomes executable). They hold
    // addresses of writable statics; the code itself is never written.
    ".p2align 3",
    ".globl s1_jit_slot_npe",
    "s1_jit_slot_npe: .quad 0",
    ".globl s1_jit_slot_sp",
    "s1_jit_slot_sp: .quad 0",
    ".globl s1_jit_slot_entry_sp",
    "s1_jit_slot_entry_sp: .quad 0",
    ".globl s1_jit_x64_end",
    "s1_jit_x64_end:",
);

extern "C" {
    pub static s1_jit_x64_begin: u8;
    pub static s1_jit_x64_end: u8;
    pub static s1_jit_null_load: u8;
    pub static s1_jit_null_load_after: u8;
    pub static s1_jit_npe_stub: u8;
    pub static s1_jit_so_entry: u8;
    pub static s1_jit_wso_entry: u8;
    pub static s1_jit_so_stub: u8;
    pub static s1_jit_unw_func: u8;
    pub static s1_jit_unw_prolog_end: u8;
    pub static s1_jit_unw_after: u8;
    pub static s1_jit_unw_func_end: u8;
    pub static s1_jit_slot_npe: u8;
    pub static s1_jit_slot_sp: u8;
    pub static s1_jit_slot_entry_sp: u8;
}

pub use crate::arch_common::BlobLayout;

pub fn blob() -> (*const u8, BlobLayout) {
    // SAFETY: only the addresses of the assembler labels are taken; nothing
    // is read through these references here.
    unsafe {
        let b = &s1_jit_x64_begin as *const u8;
        let off = |p: &u8| (p as *const u8 as usize) - (b as usize);
        (
            b,
            BlobLayout {
                len: off(&s1_jit_x64_end),
                null_load: off(&s1_jit_null_load),
                null_site: off(&s1_jit_null_load),
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

/// Windows x64 UNWIND_INFO for `s1_jit_unw_func`:
/// `push rbp` (offset 1), `push rbx` (offset 2), `sub rsp, 0x28` (offset 6).
pub fn unw_func_unwind_info(prolog_size: u8) -> [u8; 12] {
    const UWOP_PUSH_NONVOL: u8 = 0;
    const UWOP_ALLOC_SMALL: u8 = 2;
    const RBX: u8 = 3;
    const RBP: u8 = 5;
    [
        1,           // Version 1, no flags
        prolog_size, // SizeOfProlog
        3,           // CountOfCodes
        0,           // no frame register
        // Codes in reverse prologue order.
        prolog_size,
        UWOP_ALLOC_SMALL | (((0x28 / 8) - 1) << 4),
        2,
        UWOP_PUSH_NONVOL | (RBX << 4),
        1,
        UWOP_PUSH_NONVOL | (RBP << 4),
        0,
        0, // padding to an even number of slots
    ]
}

// ---------------------------------------------------------------------------
// Image-resident helpers.
// ---------------------------------------------------------------------------
global_asm!(
    ".text",
    // void s1_call_on_stack(a0, a1, fn, stack_top, stack_bottom)
    // Calls fn(a0, a1) on another stack. The TEB stack bounds are switched
    // for the duration so that exception dispatch and unwinding on the
    // alternate stack pass the dispatcher's stack-bounds checks.
    ".p2align 4",
    ".globl s1_call_on_stack",
    ".seh_proc s1_call_on_stack",
    "s1_call_on_stack:",
    "    push rbp",
    "    .seh_pushreg rbp",
    "    push rbx",
    "    .seh_pushreg rbx",
    "    push rsi",
    "    .seh_pushreg rsi",
    "    push rdi",
    "    .seh_pushreg rdi",
    "    mov rbp, rsp",
    "    .seh_setframe rbp, 0",
    "    .seh_endprologue",
    "    mov rbx, qword ptr gs:[0x8]",
    "    mov rsi, qword ptr gs:[0x10]",
    "    mov rdi, qword ptr [rbp + 0x48]",
    "    mov rsp, r9",
    "    mov qword ptr gs:[0x8], r9",
    "    mov qword ptr gs:[0x10], rdi",
    "    sub rsp, 0x20",
    "    call r8",
    "    mov qword ptr gs:[0x8], rbx",
    "    mov qword ptr gs:[0x10], rsi",
    "    lea rsp, [rbp]",
    "    pop rdi",
    "    pop rsi",
    "    pop rbx",
    "    pop rbp",
    "    ret",
    ".seh_endproc",
    // Injection trampoline. The suspending thread set RIP here and moved RSP
    // below the red zone (16-byte aligned). All other registers are the
    // interrupted code's live values (or a system call's results). Capture
    // them, then call s1_inject_entry(ctx, live_rcx), which never returns.
    ".p2align 4",
    ".globl s1_inject_tramp",
    "s1_inject_tramp:",
    "    lea rsp, [rsp - 0x520]",
    "    mov qword ptr [rsp + 0x510], rcx",
    "    lea rcx, [rsp + 0x20]",
    "    call qword ptr [rip + {cap}]",
    "    lea rcx, [rsp + 0x20]",
    "    mov rdx, qword ptr [rsp + 0x510]",
    "    call {entry}",
    "    ud2",
    // u64 s1_win64_to_sysv(a1, a2, a3, a4): Windows-ABI entry to a System V
    // target (in S1_SYSV_TARGET). System V code may clobber rsi, rdi and
    // xmm6-xmm15, which Windows callers expect preserved, so save them.
    ".p2align 4",
    ".globl s1_win64_to_sysv",
    ".seh_proc s1_win64_to_sysv",
    "s1_win64_to_sysv:",
    "    push rsi",
    "    .seh_pushreg rsi",
    "    push rdi",
    "    .seh_pushreg rdi",
    "    sub rsp, 0xa8",
    "    .seh_stackalloc 0xa8",
    "    movaps xmmword ptr [rsp + 0x00], xmm6",
    "    .seh_savexmm xmm6, 0x00",
    "    movaps xmmword ptr [rsp + 0x10], xmm7",
    "    .seh_savexmm xmm7, 0x10",
    "    movaps xmmword ptr [rsp + 0x20], xmm8",
    "    .seh_savexmm xmm8, 0x20",
    "    movaps xmmword ptr [rsp + 0x30], xmm9",
    "    .seh_savexmm xmm9, 0x30",
    "    movaps xmmword ptr [rsp + 0x40], xmm10",
    "    .seh_savexmm xmm10, 0x40",
    "    movaps xmmword ptr [rsp + 0x50], xmm11",
    "    .seh_savexmm xmm11, 0x50",
    "    movaps xmmword ptr [rsp + 0x60], xmm12",
    "    .seh_savexmm xmm12, 0x60",
    "    movaps xmmword ptr [rsp + 0x70], xmm13",
    "    .seh_savexmm xmm13, 0x70",
    "    movaps xmmword ptr [rsp + 0x80], xmm14",
    "    .seh_savexmm xmm14, 0x80",
    "    movaps xmmword ptr [rsp + 0x90], xmm15",
    "    .seh_savexmm xmm15, 0x90",
    "    .seh_endprologue",
    "    mov rdi, rcx",
    "    mov rsi, rdx",
    "    mov rdx, r8",
    "    mov rcx, r9",
    "    call qword ptr [rip + {sysv_target}]",
    "    movaps xmm6, xmmword ptr [rsp + 0x00]",
    "    movaps xmm7, xmmword ptr [rsp + 0x10]",
    "    movaps xmm8, xmmword ptr [rsp + 0x20]",
    "    movaps xmm9, xmmword ptr [rsp + 0x30]",
    "    movaps xmm10, xmmword ptr [rsp + 0x40]",
    "    movaps xmm11, xmmword ptr [rsp + 0x50]",
    "    movaps xmm12, xmmword ptr [rsp + 0x60]",
    "    movaps xmm13, xmmword ptr [rsp + 0x70]",
    "    movaps xmm14, xmmword ptr [rsp + 0x80]",
    "    movaps xmm15, xmmword ptr [rsp + 0x90]",
    "    add rsp, 0xa8",
    "    pop rdi",
    "    pop rsi",
    "    ret",
    ".seh_endproc",
    // u64 s1_sysv_to_win64(a1..a6): System V entry (how ART's JNI stub calls
    // a native) to a Windows-ABI target (in S1_WIN64_TARGET). Arguments 1-4
    // move to rcx, rdx, r8, r9; 5-6 go to the stack above the 32-byte shadow
    // space. Windows callees preserve a superset of what System V requires,
    // so nothing else needs saving.
    ".p2align 4",
    ".globl s1_sysv_to_win64",
    ".seh_proc s1_sysv_to_win64",
    "s1_sysv_to_win64:",
    "    sub rsp, 0x38",
    "    .seh_stackalloc 0x38",
    "    .seh_endprologue",
    "    mov qword ptr [rsp + 0x20], r8",
    "    mov qword ptr [rsp + 0x28], r9",
    "    mov r9, rcx",
    "    mov r8, rdx",
    "    mov rdx, rsi",
    "    mov rcx, rdi",
    "    call qword ptr [rip + {win64_target}]",
    "    add rsp, 0x38",
    "    ret",
    ".seh_endproc",
    // u64 s1_sysv_hand(a: rdi, b: rsi, cb: rdx) = cb(2a, 3b) + 1, where cb
    // is Windows-ABI code. A hand-made System V function calling Windows code.
    ".p2align 4",
    ".globl s1_sysv_hand",
    ".seh_proc s1_sysv_hand",
    "s1_sysv_hand:",
    "    sub rsp, 0x28",
    "    .seh_stackalloc 0x28",
    "    .seh_endprologue",
    "    mov rax, rdx",
    "    lea rcx, [rdi + rdi]",
    "    lea rdx, [rsi + rsi * 2]",
    "    call rax",
    "    add rax, 1",
    "    add rsp, 0x28",
    "    ret",
    ".seh_endproc",
    // System V leaf that clobbers every register System V lets it clobber,
    // including rsi, rdi and xmm6-xmm15. Returns 7.
    ".p2align 4",
    ".globl s1_sysv_clobber",
    "s1_sysv_clobber:",
    "    mov rsi, -1",
    "    mov rdi, -1",
    "    mov r8, -1",
    "    mov r9, -1",
    "    mov r10, -1",
    "    mov r11, -1",
    "    pcmpeqd xmm6, xmm6",
    "    pcmpeqd xmm7, xmm7",
    "    pcmpeqd xmm8, xmm8",
    "    pcmpeqd xmm9, xmm9",
    "    pcmpeqd xmm10, xmm10",
    "    pcmpeqd xmm11, xmm11",
    "    pcmpeqd xmm12, xmm12",
    "    pcmpeqd xmm13, xmm13",
    "    pcmpeqd xmm14, xmm14",
    "    pcmpeqd xmm15, xmm15",
    "    mov eax, 7",
    "    ret",
    // u32 s1_check_callee_saved(fn): Windows-ABI harness. Loads sentinels
    // into every Windows callee-saved register (rbx, rbp, rsi, rdi, r12-r15,
    // xmm6-xmm15), calls fn() as Windows-ABI code, and returns a bit mask of
    // registers whose value changed (bit 0 rbx .. bit 7 r15, bit 8+i xmm(6+i)).
    ".p2align 4",
    ".globl s1_check_callee_saved",
    ".seh_proc s1_check_callee_saved",
    "s1_check_callee_saved:",
    "    push rbx",
    "    .seh_pushreg rbx",
    "    push rbp",
    "    .seh_pushreg rbp",
    "    push rsi",
    "    .seh_pushreg rsi",
    "    push rdi",
    "    .seh_pushreg rdi",
    "    push r12",
    "    .seh_pushreg r12",
    "    push r13",
    "    .seh_pushreg r13",
    "    push r14",
    "    .seh_pushreg r14",
    "    push r15",
    "    .seh_pushreg r15",
    "    sub rsp, 0xc8",
    "    .seh_stackalloc 0xc8",
    "    movaps xmmword ptr [rsp + 0x20], xmm6",
    "    .seh_savexmm xmm6, 0x20",
    "    movaps xmmword ptr [rsp + 0x30], xmm7",
    "    .seh_savexmm xmm7, 0x30",
    "    movaps xmmword ptr [rsp + 0x40], xmm8",
    "    .seh_savexmm xmm8, 0x40",
    "    movaps xmmword ptr [rsp + 0x50], xmm9",
    "    .seh_savexmm xmm9, 0x50",
    "    movaps xmmword ptr [rsp + 0x60], xmm10",
    "    .seh_savexmm xmm10, 0x60",
    "    movaps xmmword ptr [rsp + 0x70], xmm11",
    "    .seh_savexmm xmm11, 0x70",
    "    movaps xmmword ptr [rsp + 0x80], xmm12",
    "    .seh_savexmm xmm12, 0x80",
    "    movaps xmmword ptr [rsp + 0x90], xmm13",
    "    .seh_savexmm xmm13, 0x90",
    "    movaps xmmword ptr [rsp + 0xa0], xmm14",
    "    .seh_savexmm xmm14, 0xa0",
    "    movaps xmmword ptr [rsp + 0xb0], xmm15",
    "    .seh_savexmm xmm15, 0xb0",
    "    .seh_endprologue",
    "    mov rax, rcx",
    "    mov rcx, 0x5a5a000000000000",
    "    lea rbx, [rcx + 1]",
    "    lea rbp, [rcx + 2]",
    "    lea rsi, [rcx + 3]",
    "    lea rdi, [rcx + 4]",
    "    lea r12, [rcx + 5]",
    "    lea r13, [rcx + 6]",
    "    lea r14, [rcx + 7]",
    "    lea r15, [rcx + 8]",
    ".irp n, 6,7,8,9,10,11,12,13,14,15",
    "    lea rdx, [rcx + 0x10 + \\n]",
    "    movq xmm\\n, rdx",
    ".endr",
    "    call rax",
    "    xor eax, eax",
    "    mov rcx, 0x5a5a000000000000",
    "    lea rdx, [rcx + 1]",
    "    cmp rbx, rdx",
    "    je 21f",
    "    or eax, 0x1",
    "21:",
    "    lea rdx, [rcx + 2]",
    "    cmp rbp, rdx",
    "    je 22f",
    "    or eax, 0x2",
    "22:",
    "    lea rdx, [rcx + 3]",
    "    cmp rsi, rdx",
    "    je 23f",
    "    or eax, 0x4",
    "23:",
    "    lea rdx, [rcx + 4]",
    "    cmp rdi, rdx",
    "    je 24f",
    "    or eax, 0x8",
    "24:",
    "    lea rdx, [rcx + 5]",
    "    cmp r12, rdx",
    "    je 25f",
    "    or eax, 0x10",
    "25:",
    "    lea rdx, [rcx + 6]",
    "    cmp r13, rdx",
    "    je 26f",
    "    or eax, 0x20",
    "26:",
    "    lea rdx, [rcx + 7]",
    "    cmp r14, rdx",
    "    je 27f",
    "    or eax, 0x40",
    "27:",
    "    lea rdx, [rcx + 8]",
    "    cmp r15, rdx",
    "    je 28f",
    "    or eax, 0x80",
    "28:",
    ".irp n, 6,7,8,9,10,11,12,13,14,15",
    "    lea rdx, [rcx + 0x10 + \\n]",
    "    movq r8, xmm\\n",
    "    cmp r8, rdx",
    "    je 3\\n\\()f",
    "    or eax, (1 << (\\n + 2))",
    "3\\n\\():",
    ".endr",
    "    movaps xmm6, xmmword ptr [rsp + 0x20]",
    "    movaps xmm7, xmmword ptr [rsp + 0x30]",
    "    movaps xmm8, xmmword ptr [rsp + 0x40]",
    "    movaps xmm9, xmmword ptr [rsp + 0x50]",
    "    movaps xmm10, xmmword ptr [rsp + 0x60]",
    "    movaps xmm11, xmmword ptr [rsp + 0x70]",
    "    movaps xmm12, xmmword ptr [rsp + 0x80]",
    "    movaps xmm13, xmmword ptr [rsp + 0x90]",
    "    movaps xmm14, xmmword ptr [rsp + 0xa0]",
    "    movaps xmm15, xmmword ptr [rsp + 0xb0]",
    "    add rsp, 0xc8",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rdi",
    "    pop rsi",
    "    pop rbp",
    "    pop rbx",
    "    ret",
    ".seh_endproc",
    cap = sym crate::suspend::S1_RTL_CAPTURE_CONTEXT,
    entry = sym crate::suspend::s1_inject_entry,
    sysv_target = sym crate::abi::S1_SYSV_TARGET,
    win64_target = sym crate::abi::S1_WIN64_TARGET,
);

extern "C" {
    /// Calls `f(a0, a1)` on the stack `[bottom, top)`.
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
    ctx.Rip as usize
}
pub fn sp(ctx: &CONTEXT) -> usize {
    ctx.Rsp as usize
}
pub fn uc_pc(uc: &UContext) -> usize {
    uc.uc_mcontext.gregs[lx::REG_RIP] as usize
}
pub fn uc_set_pc(uc: &mut UContext, pc: usize) {
    uc.uc_mcontext.gregs[lx::REG_RIP] = pc as i64;
}
pub fn uc_sp(uc: &UContext) -> usize {
    uc.uc_mcontext.gregs[lx::REG_RSP] as usize
}
pub fn uc_set_sp(uc: &mut UContext, sp: usize) {
    uc.uc_mcontext.gregs[lx::REG_RSP] = sp as i64;
}
pub fn uc_set_ret(uc: &mut UContext, v: u64) {
    uc.uc_mcontext.gregs[lx::REG_RAX] = v as i64;
}

/// Linux x86 trap numbers and page-fault error code bits.
const TRAP_DE: i64 = 0;
const TRAP_UD: i64 = 6;
const TRAP_PF: i64 = 14;
const PF_PROT: i64 = 1;
const PF_WRITE: i64 = 2;
const PF_USER: i64 = 4;

/// Fills `uc` from `ctx`. `fault` is (address, is_write, page_present) for
/// memory faults. The FXSAVE image is copied as is: `XMM_SAVE_AREA32` and
/// `_libc_fpstate` have the same layout.
pub fn context_to_ucontext(
    ctx: &CONTEXT,
    info: &siginfo_t,
    fault: Option<(usize, bool, bool)>,
    altstack: stack_t,
    uc: &mut UContext,
) {
    let g = &mut uc.uc_mcontext.gregs;
    g[lx::REG_R8] = ctx.R8 as i64;
    g[lx::REG_R9] = ctx.R9 as i64;
    g[lx::REG_R10] = ctx.R10 as i64;
    g[lx::REG_R11] = ctx.R11 as i64;
    g[lx::REG_R12] = ctx.R12 as i64;
    g[lx::REG_R13] = ctx.R13 as i64;
    g[lx::REG_R14] = ctx.R14 as i64;
    g[lx::REG_R15] = ctx.R15 as i64;
    g[lx::REG_RDI] = ctx.Rdi as i64;
    g[lx::REG_RSI] = ctx.Rsi as i64;
    g[lx::REG_RBP] = ctx.Rbp as i64;
    g[lx::REG_RBX] = ctx.Rbx as i64;
    g[lx::REG_RDX] = ctx.Rdx as i64;
    g[lx::REG_RAX] = ctx.Rax as i64;
    g[lx::REG_RCX] = ctx.Rcx as i64;
    g[lx::REG_RSP] = ctx.Rsp as i64;
    g[lx::REG_RIP] = ctx.Rip as i64;
    g[lx::REG_EFL] = ctx.EFlags as i64;
    // Linux packs cs, gs, fs, ss into one word.
    g[lx::REG_CSGSFS] = (ctx.SegCs as i64)
        | ((ctx.SegGs as i64) << 16)
        | ((ctx.SegFs as i64) << 32)
        | ((ctx.SegSs as i64) << 48);
    let (trapno, err, cr2) = match (info.si_signo, fault) {
        (crate::linux_abi::SIGSEGV, Some((addr, write, present))) => (
            TRAP_PF,
            PF_USER | if write { PF_WRITE } else { 0 } | if present { PF_PROT } else { 0 },
            addr as i64,
        ),
        (crate::linux_abi::SIGFPE, _) => (TRAP_DE, 0, 0),
        (crate::linux_abi::SIGILL, _) => (TRAP_UD, 0, 0),
        _ => (0, 0, 0),
    };
    g[lx::REG_ERR] = err;
    g[lx::REG_TRAPNO] = trapno;
    g[lx::REG_OLDMASK] = 0;
    g[lx::REG_CR2] = cr2;
    // SAFETY: FltSave is the 512-byte FXSAVE area of CONTEXT, valid for the
    // context record's whole lifetime; libc_fpstate is the same 512 bytes.
    unsafe {
        let src = &ctx.Anonymous.FltSave as *const _ as *const lx::libc_fpstate;
        uc.fpregs_mem = *src;
    }
    uc.fpregs_mem.mxcsr = ctx.MxCsr;
    uc.uc_mcontext.fpregs = &mut uc.fpregs_mem;
    uc.uc_flags = 0;
    uc.uc_link = core::ptr::null_mut();
    uc.uc_stack = altstack;
    uc.uc_sigmask = 0;
}

/// Writes the (possibly modified) ucontext back into the Windows context.
pub fn ucontext_to_context(uc: &UContext, ctx: &mut CONTEXT) {
    let g = &uc.uc_mcontext.gregs;
    ctx.R8 = g[lx::REG_R8] as u64;
    ctx.R9 = g[lx::REG_R9] as u64;
    ctx.R10 = g[lx::REG_R10] as u64;
    ctx.R11 = g[lx::REG_R11] as u64;
    ctx.R12 = g[lx::REG_R12] as u64;
    ctx.R13 = g[lx::REG_R13] as u64;
    ctx.R14 = g[lx::REG_R14] as u64;
    ctx.R15 = g[lx::REG_R15] as u64;
    ctx.Rdi = g[lx::REG_RDI] as u64;
    ctx.Rsi = g[lx::REG_RSI] as u64;
    ctx.Rbp = g[lx::REG_RBP] as u64;
    ctx.Rbx = g[lx::REG_RBX] as u64;
    ctx.Rdx = g[lx::REG_RDX] as u64;
    ctx.Rax = g[lx::REG_RAX] as u64;
    ctx.Rcx = g[lx::REG_RCX] as u64;
    ctx.Rsp = g[lx::REG_RSP] as u64;
    ctx.Rip = g[lx::REG_RIP] as u64;
    // Only the arithmetic and direction flags are user-modifiable, as on
    // Linux sigreturn.
    const USER_FLAGS: u32 = 0x0000_0cd5 | 0x0000_0100 | 0x0004_0000;
    ctx.EFlags = (ctx.EFlags & !USER_FLAGS) | (g[lx::REG_EFL] as u32 & USER_FLAGS);
    if !uc.uc_mcontext.fpregs.is_null() {
        // SAFETY: fpregs points either into `uc` itself or to a 512-byte
        // fpstate the handler supplied, as on Linux; FltSave is 512 bytes.
        unsafe {
            let dst = &mut ctx.Anonymous.FltSave as *mut _ as *mut lx::libc_fpstate;
            *dst = *uc.uc_mcontext.fpregs;
            ctx.MxCsr = (*uc.uc_mcontext.fpregs).mxcsr;
        }
    }
}

// ---------------------------------------------------------------------------
// Small per-architecture operations used by the portable modules.
// ---------------------------------------------------------------------------

use windows::Win32::System::Diagnostics::Debug::{
    RtlLookupFunctionEntry, RtlVirtualUnwind, CONTEXT_CONTROL_AMD64, CONTEXT_FLAGS,
    CONTEXT_FULL_AMD64, UNW_FLAG_NHANDLER,
};

pub const CONTEXT_CONTROL: CONTEXT_FLAGS = CONTEXT_CONTROL_AMD64;
pub const CONTEXT_FULL: CONTEXT_FLAGS = CONTEXT_FULL_AMD64;

/// System V reserves 128 bytes below RSP (red zone) that leaf code may use;
/// an injected frame must skip it, as Linux signal delivery does.
const RED_ZONE: usize = 128;

/// Simulates a call from the faulting instruction: push `ret`, jump to
/// `target` (what ART's x86_64 fault handler does before its throw stub).
pub fn redirect_call(ctx: &mut CONTEXT, ret: usize, target: usize) {
    ctx.Rsp -= 8;
    // SAFETY: the new slot is just below the interrupted SP on the
    // thread's own committed stack.
    unsafe { *(ctx.Rsp as *mut u64) = ret as u64 };
    ctx.Rip = target as u64;
}

pub fn uc_redirect_call(uc: &mut UContext, ret: usize, target: usize) {
    let sp = uc_sp(uc) - 8;
    // SAFETY: as in `redirect_call`.
    unsafe { *(sp as *mut u64) = ret as u64 };
    uc_set_sp(uc, sp);
    uc_set_pc(uc, target);
}

/// Returns the length of the instruction at `pc` if it is one the trap tests
/// execute: `ud2` (SIGILL test) or `div rcx` (SIGFPE test).
pub fn trap_insn_len(pc: usize) -> Option<usize> {
    // SAFETY: `pc` is the faulting instruction of the current thread, which
    // is mapped and readable.
    let b = unsafe { core::slice::from_raw_parts(pc as *const u8, 3) };
    match b {
        [0x0f, 0x0b, _] => Some(2),
        [0x48, 0xf7, 0xf1] => Some(3),
        _ => None,
    }
}

/// Executes `ud2` and returns the value the SIGILL handler left in rax.
pub fn exec_ud2() -> u64 {
    let r: u64;
    // SAFETY: the instruction faults; the installed SIGILL handler skips it
    // and sets rax. Without a handler the process terminates, as on Linux.
    unsafe { core::arch::asm!("xor eax, eax", "ud2", out("rax") r) };
    r
}

/// Executes an integer division by zero; returns the handler's rax.
pub fn exec_div0() -> u64 {
    let r: u64;
    // SAFETY: as above, for SIGFPE.
    unsafe {
        core::arch::asm!(
            "xor eax, eax", "xor edx, edx", "xor ecx, ecx", "div rcx",
            out("rax") r, out("rdx") _, out("rcx") _,
        )
    };
    r
}

/// One step of RtlVirtualUnwind from `ctx`. Returns (function entry found,
/// caller pc, caller sp). Without an entry the frame is treated as a leaf,
/// as the OS unwinder does.
pub fn virtual_unwind(ctx: &CONTEXT) -> (bool, usize, usize) {
    let mut c = *ctx;
    let mut base = 0u64;
    // SAFETY: lookup only reads the registered function tables.
    let fe = unsafe { RtlLookupFunctionEntry(c.Rip, &mut base, None) };
    if fe.is_null() {
        // SAFETY: RSP points at the return address of a leaf frame.
        let ret = unsafe { *(c.Rsp as *const u64) };
        return (false, ret as usize, (c.Rsp + 8) as usize);
    }
    let mut hd: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut ef = 0u64;
    // SAFETY: `fe` was returned for `c.Rip`; `c` is a private copy.
    unsafe {
        RtlVirtualUnwind(
            UNW_FLAG_NHANDLER,
            base,
            c.Rip,
            fe,
            &mut c,
            &mut hd,
            &mut ef,
            None,
        );
    }
    (true, c.Rip as usize, c.Rsp as usize)
}

/// Walks the current stack with RtlLookupFunctionEntry + RtlVirtualUnwind
/// (what a debugger or a crash reporter does). Returns the number of pcs.
#[inline(never)]
pub fn manual_walk(out: &mut [usize]) -> usize {
    // SAFETY: CONTEXT is plain data; RtlCaptureContext fills it.
    let mut c: crate::suspend::AlignedContext = unsafe { core::mem::zeroed() };
    unsafe { windows::Win32::System::Diagnostics::Debug::RtlCaptureContext(&mut c.0) };
    let mut n = 0;
    while n < out.len() && c.0.Rip != 0 {
        out[n] = c.0.Rip as usize;
        n += 1;
        let (_, pc, sp) = virtual_unwind(&c.0);
        // virtual_unwind works on a copy; redo it in place for the other
        // non-volatile registers.
        let mut base = 0u64;
        // SAFETY: as in `virtual_unwind`.
        let fe = unsafe { RtlLookupFunctionEntry(c.0.Rip, &mut base, None) };
        if fe.is_null() {
            c.0.Rip = pc as u64;
            c.0.Rsp = sp as u64;
        } else {
            let mut hd: *mut core::ffi::c_void = core::ptr::null_mut();
            let mut ef = 0u64;
            // SAFETY: `fe` belongs to `c.Rip`.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    base,
                    c.0.Rip,
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

/// For `s1_jit_unw_func`: the caller's pc and sp given the SP at entry.
pub fn expected_caller(entry_sp: usize) -> (usize, usize) {
    // SAFETY: entry_sp was recorded by the JIT function and points at its
    // return address, still on the live stack of the calling frame.
    let ret = unsafe { *(entry_sp as *const u64) } as usize;
    (ret, entry_sp + 8)
}

/// Redirects a suspended thread to `tramp`. Returns (orig pc, orig sp).
pub fn inject_redirect(ctx: &mut CONTEXT, tramp: usize) -> (usize, usize) {
    let orig = (ctx.Rip as usize, ctx.Rsp as usize);
    ctx.Rsp = ((ctx.Rsp as usize - RED_ZONE) & !15) as u64;
    ctx.Rip = tramp as u64;
    orig
}

/// Repairs the context captured by the trampoline: rcx (used to pass the
/// context pointer), the original pc and sp.
pub fn inject_fixup(ctx: &mut CONTEXT, extra: usize, orig_pc: usize, orig_sp: usize) {
    ctx.Rcx = extra as u64;
    ctx.Rip = orig_pc as u64;
    ctx.Rsp = orig_sp as u64;
}

/// True if the bytes before `pc` are `syscall` (0f 05): the thread was
/// suspended inside a system call.
pub fn in_syscall(pc: usize) -> bool {
    // SAFETY: `pc` is inside a mapped system-call stub; the two bytes
    // before the return address belong to the same stub.
    let b = unsafe { core::slice::from_raw_parts((pc - 2) as *const u8, 2) };
    b == [0x0f, 0x05]
}

pub fn ret_reg(ctx: &CONTEXT) -> u64 {
    ctx.Rax
}

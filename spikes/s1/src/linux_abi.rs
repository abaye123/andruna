//! Linux/bionic signal ABI types for x86_64 and aarch64.
//!
//! Both layouts are always compiled (they are plain data), so the size and
//! offset assertions for both architectures are checked on every build.
//! Layouts follow bionic's `libc/include/sys/ucontext.h`,
//! `libc/include/bits/signal_types.h` and the arm64/x86 UAPI `sigcontext.h`
//! and `asm-generic/siginfo.h` (bionic main branch, read 2026-11-26).

#![allow(non_camel_case_types, dead_code)]

use core::mem::{offset_of, size_of};

pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGSEGV: i32 = 11;

pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
pub const ILL_ILLOPC: i32 = 1;
pub const FPE_INTDIV: i32 = 1;
pub const FPE_INTOVF: i32 = 2;
pub const TRAP_BRKPT: i32 = 1;

pub const SA_SIGINFO: i32 = 0x4;
pub const SA_ONSTACK: i32 = 0x0800_0000;

pub const SS_ONSTACK: i32 = 1;
pub const SS_DISABLE: i32 = 2;

/// `sigset_t` on LP64 bionic: one 64-bit word.
pub type sigset_t = u64;

/// `stack_t` (same on x86_64 and arm64).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct stack_t {
    pub ss_sp: *mut core::ffi::c_void,
    pub ss_flags: i32,
    pub ss_size: usize,
}

/// `siginfo_t`: 128 bytes, `si_signo`, `si_errno`, `si_code`, then the
/// union at offset 16 (8-byte aligned on LP64). Only `_sigfault` is named.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct siginfo_t {
    pub si_signo: i32,
    pub si_errno: i32,
    pub si_code: i32,
    _pad0: i32,
    /// `_sifields._sigfault._addr`
    pub si_addr: *mut core::ffi::c_void,
    _rest: [u64; 13],
}

impl siginfo_t {
    pub const fn zeroed() -> Self {
        Self {
            si_signo: 0,
            si_errno: 0,
            si_code: 0,
            _pad0: 0,
            si_addr: core::ptr::null_mut(),
            _rest: [0; 13],
        }
    }
}

/// bionic LP64 userspace `struct sigaction` (differs from the kernel's).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sigaction {
    pub sa_flags: i32,
    pub sa_sigaction: usize,
    pub sa_mask: sigset_t,
    pub sa_restorer: usize,
}

const _: () = {
    assert!(size_of::<sigset_t>() == 8);
    assert!(size_of::<stack_t>() == 24);
    assert!(offset_of!(stack_t, ss_flags) == 8);
    assert!(offset_of!(stack_t, ss_size) == 16);
    assert!(size_of::<siginfo_t>() == 128);
    assert!(offset_of!(siginfo_t, si_errno) == 4);
    assert!(offset_of!(siginfo_t, si_code) == 8);
    assert!(offset_of!(siginfo_t, si_addr) == 16);
    assert!(size_of::<sigaction>() == 32);
    assert!(offset_of!(sigaction, sa_sigaction) == 8);
    assert!(offset_of!(sigaction, sa_mask) == 16);
    assert!(offset_of!(sigaction, sa_restorer) == 24);
};

pub mod x86_64 {
    use super::{sigset_t, stack_t};
    use core::mem::{align_of, offset_of, size_of};

    pub const REG_R8: usize = 0;
    pub const REG_R9: usize = 1;
    pub const REG_R10: usize = 2;
    pub const REG_R11: usize = 3;
    pub const REG_R12: usize = 4;
    pub const REG_R13: usize = 5;
    pub const REG_R14: usize = 6;
    pub const REG_R15: usize = 7;
    pub const REG_RDI: usize = 8;
    pub const REG_RSI: usize = 9;
    pub const REG_RBP: usize = 10;
    pub const REG_RBX: usize = 11;
    pub const REG_RDX: usize = 12;
    pub const REG_RAX: usize = 13;
    pub const REG_RCX: usize = 14;
    pub const REG_RSP: usize = 15;
    pub const REG_RIP: usize = 16;
    pub const REG_EFL: usize = 17;
    pub const REG_CSGSFS: usize = 18;
    pub const REG_ERR: usize = 19;
    pub const REG_TRAPNO: usize = 20;
    pub const REG_OLDMASK: usize = 21;
    pub const REG_CR2: usize = 22;
    pub const NGREG: usize = 23;

    /// `struct _libc_fpstate`: the FXSAVE image, identical in layout to
    /// Windows' `XMM_SAVE_AREA32`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct libc_fpstate {
        pub cwd: u16,
        pub swd: u16,
        pub ftw: u16,
        pub fop: u16,
        pub rip: u64,
        pub rdp: u64,
        pub mxcsr: u32,
        pub mxcr_mask: u32,
        pub st: [[u16; 8]; 8],
        pub xmm: [[u32; 4]; 16],
        pub padding: [u32; 24],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct mcontext_t {
        pub gregs: [i64; NGREG],
        pub fpregs: *mut libc_fpstate,
        pub reserved1: [u64; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ucontext_t {
        pub uc_flags: u64,
        pub uc_link: *mut ucontext_t,
        pub uc_stack: stack_t,
        pub uc_mcontext: mcontext_t,
        pub uc_sigmask: sigset_t,
        pub fpregs_mem: libc_fpstate,
    }

    const _: () = {
        assert!(size_of::<libc_fpstate>() == 512);
        assert!(offset_of!(libc_fpstate, mxcsr) == 24);
        assert!(offset_of!(libc_fpstate, st) == 32);
        assert!(offset_of!(libc_fpstate, xmm) == 160);
        assert!(size_of::<mcontext_t>() == 256);
        assert!(offset_of!(mcontext_t, fpregs) == 184);
        assert!(offset_of!(ucontext_t, uc_stack) == 16);
        assert!(offset_of!(ucontext_t, uc_mcontext) == 40);
        assert!(offset_of!(ucontext_t, uc_sigmask) == 296);
        assert!(offset_of!(ucontext_t, fpregs_mem) == 304);
        assert!(size_of::<ucontext_t>() == 816);
        assert!(align_of::<ucontext_t>() == 8);
        // RIP lives at uc_mcontext.gregs[REG_RIP]: 40 + 16 * 8.
        assert!(offset_of!(ucontext_t, uc_mcontext) + REG_RIP * 8 == 168);
    };
}

pub mod aarch64 {
    use super::{sigset_t, stack_t};
    use core::mem::{align_of, offset_of, size_of};

    pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
    pub const ESR_MAGIC: u32 = 0x4553_5201;

    /// `struct sigcontext` (= `mcontext_t` in bionic).
    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct sigcontext {
        pub fault_address: u64,
        pub regs: [u64; 31],
        pub sp: u64,
        pub pc: u64,
        pub pstate: u64,
        pub reserved: Reserved,
    }

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct Reserved(pub [u8; 4096]);

    pub type mcontext_t = sigcontext;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct aarch64_ctx {
        pub magic: u32,
        pub size: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct fpsimd_context {
        pub head: aarch64_ctx,
        pub fpsr: u32,
        pub fpcr: u32,
        pub vregs: [u128; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct esr_context {
        pub head: aarch64_ctx,
        pub esr: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ucontext_t {
        pub uc_flags: u64,
        pub uc_link: *mut ucontext_t,
        pub uc_stack: stack_t,
        pub uc_sigmask: sigset_t,
        pub padding: [u8; 128 - 8],
        pub uc_mcontext: mcontext_t,
    }

    const _: () = {
        assert!(offset_of!(sigcontext, regs) == 8);
        assert!(offset_of!(sigcontext, sp) == 256);
        assert!(offset_of!(sigcontext, pc) == 264);
        assert!(offset_of!(sigcontext, pstate) == 272);
        assert!(offset_of!(sigcontext, reserved) == 288);
        assert!(size_of::<sigcontext>() == 4384);
        // u128 is 16-byte aligned on aarch64 Linux; vregs start at 16.
        assert!(offset_of!(fpsimd_context, vregs) == 16);
        assert!(size_of::<fpsimd_context>() == 528);
        assert!(size_of::<esr_context>() == 16);
        assert!(offset_of!(ucontext_t, uc_sigmask) == 40);
        assert!(offset_of!(ucontext_t, uc_mcontext) == 176);
        assert!(size_of::<ucontext_t>() == 4560);
        assert!(align_of::<ucontext_t>() == 16);
        // PC lives at 176 + 264.
        assert!(offset_of!(ucontext_t, uc_mcontext) + offset_of!(sigcontext, pc) == 440);
    };
}

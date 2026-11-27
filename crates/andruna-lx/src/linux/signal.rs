//! Signals: numbers, flags and the structures the kernel exchanges with
//! user code (`asm-generic/signal.h`, `asm-generic/siginfo.h`,
//! `asm-x86/asm/sigcontext.h`, `asm-arm64/asm/sigcontext.h`,
//! `asm-arm64/asm/ucontext.h`, bionic's `sys/ucontext.h`).
//!
//! The kernel's `struct sigaction` (what `rt_sigaction` takes) is the same
//! on x86_64 and arm64: handler, flags, restorer, mask. Both ucontext
//! layouts are compiled and checked on every build.

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGABRT: i32 = 6;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;
pub const SIGSTKFLT: i32 = 16;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTTIN: i32 = 21;
pub const SIGTTOU: i32 = 22;
pub const SIGURG: i32 = 23;
pub const SIGXCPU: i32 = 24;
pub const SIGXFSZ: i32 = 25;
pub const SIGVTALRM: i32 = 26;
pub const SIGPROF: i32 = 27;
pub const SIGWINCH: i32 = 28;
pub const SIGIO: i32 = 29;
pub const SIGPWR: i32 = 30;
pub const SIGSYS: i32 = 31;
/// First real-time signal number of the kernel (bionic reserves a few).
pub const SIGRTMIN: i32 = 32;
/// Highest signal number (`_NSIG - 1`).
pub const SIGRTMAX: i32 = 64;
pub const NSIG: usize = 65;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

pub const SA_NOCLDSTOP: u64 = 0x1;
pub const SA_NOCLDWAIT: u64 = 0x2;
pub const SA_SIGINFO: u64 = 0x4;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

pub const SIG_BLOCK: i32 = 0;
pub const SIG_UNBLOCK: i32 = 1;
pub const SIG_SETMASK: i32 = 2;

pub const SS_ONSTACK: i32 = 1;
pub const SS_DISABLE: i32 = 2;
pub const SS_AUTODISARM: i32 = 1 << 31;

/// `MINSIGSTKSZ`: x86_64 2048, arm64 5120 (its signal frame is larger).
#[cfg(target_arch = "x86_64")]
pub const MINSIGSTKSZ: usize = 2048;
#[cfg(target_arch = "aarch64")]
pub const MINSIGSTKSZ: usize = 5120;

// si_code values.
pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_QUEUE: i32 = -1;
pub const SI_TIMER: i32 = -2;
pub const SI_TKILL: i32 = -6;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
pub const BUS_ADRERR: i32 = 2;
pub const ILL_ILLOPC: i32 = 1;
pub const ILL_PRVOPC: i32 = 5;
pub const FPE_INTDIV: i32 = 1;
pub const FPE_INTOVF: i32 = 2;
pub const TRAP_BRKPT: i32 = 1;

/// The signal set of the system calls: one bit per signal, bit `n - 1` for
/// signal `n` (`sigsetsize` must be 8).
pub type SigSet = u64;

pub const fn sigbit(sig: i32) -> SigSet {
    1u64 << (sig - 1)
}

/// Signals that cannot be caught, blocked or ignored.
pub const UNBLOCKABLE: SigSet = sigbit(SIGKILL) | sigbit(SIGSTOP);

/// The kernel's `struct sigaction` (`rt_sigaction`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelSigaction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: SigSet,
}

/// `stack_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StackT {
    pub ss_sp: u64,
    pub ss_flags: i32,
    pub _pad: i32,
    pub ss_size: u64,
}

/// `siginfo_t` (128 bytes on LP64). The union starts at offset 16; the
/// fields used here are `si_addr` (faults) and `si_pid`, `si_uid`,
/// `si_value` (kill, queue).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigInfo {
    pub si_signo: i32,
    pub si_errno: i32,
    pub si_code: i32,
    pub _pad: i32,
    pub fields: [u64; 14],
}

impl SigInfo {
    /// A fault at `addr`.
    pub fn fault(sig: i32, code: i32, addr: u64) -> Self {
        let mut i = Self {
            si_signo: sig,
            si_code: code,
            ..Self::default()
        };
        i.fields[0] = addr;
        i
    }

    /// A signal sent by `kill`/`tgkill` (`si_pid`, `si_uid`).
    pub fn kill(sig: i32, code: i32, pid: i32, uid: u32) -> Self {
        let mut i = Self {
            si_signo: sig,
            si_code: code,
            ..Self::default()
        };
        i.fields[0] = u64::from(pid as u32) | (u64::from(uid) << 32);
        i
    }

    pub fn addr(&self) -> u64 {
        self.fields[0]
    }
}

/// x86_64 `ucontext_t` as bionic declares it (816 bytes): flags, link,
/// stack, `mcontext_t` (23 general registers, the `fpregs` pointer, 8
/// reserved words), an 8-byte signal mask, and the 512-byte FXSAVE image
/// that `fpregs` points to.
pub mod x86_64 {
    use super::{SigSet, StackT};

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

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct UContext {
        pub uc_flags: u64,
        pub uc_link: u64,
        pub uc_stack: StackT,
        pub gregs: [i64; NGREG],
        pub fpregs: u64,
        pub reserved: [u64; 8],
        pub uc_sigmask: SigSet,
        pub fpregs_mem: [u8; 512],
    }

    const _: () = {
        assert!(core::mem::size_of::<UContext>() == 816);
        assert!(core::mem::offset_of!(UContext, uc_stack) == 16);
        assert!(core::mem::offset_of!(UContext, gregs) == 40);
        assert!(core::mem::offset_of!(UContext, fpregs) == 224);
        assert!(core::mem::offset_of!(UContext, uc_sigmask) == 296);
        assert!(core::mem::offset_of!(UContext, fpregs_mem) == 304);
    };

    /// Offset of `mxcsr` in the FXSAVE image.
    pub const FX_MXCSR: usize = 24;
}

/// arm64 `ucontext_t` (4560 bytes): flags, link, stack, an 8-byte signal
/// mask padded to 128 bytes, then `struct sigcontext` (fault address,
/// x0-x30, sp, pc, pstate, and a 4096-byte record area holding an
/// `fpsimd_context` and, for data aborts, an `esr_context`).
pub mod aarch64 {
    use super::{SigSet, StackT};

    pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
    pub const ESR_MAGIC: u32 = 0x4553_5201;

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct UContext {
        pub uc_flags: u64,
        pub uc_link: u64,
        pub uc_stack: StackT,
        pub uc_sigmask: SigSet,
        pub _pad: [u8; 128 - 8],
        pub _pad2: u64,
        pub fault_address: u64,
        pub regs: [u64; 31],
        pub sp: u64,
        pub pc: u64,
        pub pstate: u64,
        pub _pad3: u64,
        pub reserved: [u8; 4096],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct FpsimdContext {
        pub magic: u32,
        pub size: u32,
        pub fpsr: u32,
        pub fpcr: u32,
        pub vregs: [[u64; 2]; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct EsrContext {
        pub magic: u32,
        pub size: u32,
        pub esr: u64,
    }

    const _: () = {
        assert!(core::mem::offset_of!(UContext, uc_stack) == 16);
        assert!(core::mem::offset_of!(UContext, uc_sigmask) == 40);
        assert!(core::mem::offset_of!(UContext, fault_address) == 176);
        assert!(core::mem::offset_of!(UContext, regs) == 184);
        assert!(core::mem::offset_of!(UContext, sp) == 432);
        assert!(core::mem::offset_of!(UContext, pc) == 440);
        assert!(core::mem::offset_of!(UContext, pstate) == 448);
        assert!(core::mem::offset_of!(UContext, reserved) == 464);
        assert!(core::mem::size_of::<UContext>() == 4560);
        assert!(core::mem::size_of::<FpsimdContext>() == 528);
        assert!(core::mem::size_of::<EsrContext>() == 16);
    };
}

const _: () = {
    assert!(core::mem::size_of::<KernelSigaction>() == 32);
    assert!(core::mem::size_of::<StackT>() == 24);
    assert!(core::mem::offset_of!(StackT, ss_flags) == 8);
    assert!(core::mem::offset_of!(StackT, ss_size) == 16);
    assert!(core::mem::size_of::<SigInfo>() == 128);
    assert!(core::mem::offset_of!(SigInfo, fields) == 16);
};

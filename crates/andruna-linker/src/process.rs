//! Starting a program: the initial stack block, loading, the main thread,
//! initializers and the entry point, as the kernel and bionic's linker do.
//!
//! The guest main thread is a Windows thread running on a committed guest
//! stack. The block at the stack top is what the kernel gives a Linux
//! process (`argc`, `argv`, `envp`, the auxiliary vector, the strings and
//! `AT_RANDOM` bytes). With bionic (the program links `libc.so`, and
//! `ld-android.so` is the bridge) the loader jumps to the executable's
//! entry point (`_start`, which calls `__libc_init`) with the stack
//! pointer at that block; without bionic it calls `main` and exits with its
//! result.

use std::ffi::CString;
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::linker::{Config, Linker, run_init_fns};
use crate::object::Object;
use crate::thread::GuestStack;

/// Default stack of the guest main thread (Android's main thread limit).
pub const MAIN_STACK: u64 = 8 << 20;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;
const AT_EXECFN: u64 = 31;

/// The initial stack block of a program.
pub struct Startup {
    /// Address of `argc`.
    pub block: u64,
    phdr_slot: u64,
    phnum_slot: u64,
    entry_slot: u64,
    pub argc: i32,
    pub argv: u64,
    pub envp: u64,
}

impl Startup {
    /// Fills `AT_PHDR`, `AT_PHNUM` and `AT_ENTRY` once the executable is
    /// mapped.
    pub fn set_executable(&self, o: &Object) {
        // SAFETY: the slots lie in the block this value describes, which
        // stays mapped for the life of the program.
        unsafe {
            (self.phdr_slot as *mut u64).write(o.phdr_addr);
            (self.phnum_slot as *mut u64).write(u64::from(o.headers.ehdr.e_phnum));
            (self.entry_slot as *mut u64).write(o.addr(o.headers.ehdr.e_entry));
        }
    }
}

struct Args {
    argc: i32,
    argv: u64,
    envp: u64,
}

static ARGS: OnceLock<Args> = OnceLock::new();

/// `(argc, argv, envp)` of the program, for initializers of `dlopen`ed
/// objects (bionic passes the same values).
pub fn args() -> (i32, *const *const u8, *const *const u8) {
    match ARGS.get() {
        Some(a) => (
            a.argc,
            a.argv as *const *const u8,
            a.envp as *const *const u8,
        ),
        None => (0, core::ptr::null(), core::ptr::null()),
    }
}

/// `exit_group`: ends the process.
pub fn exit_process(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code & 0xff)
}

#[cfg(target_arch = "x86_64")]
fn hwcaps() -> (u64, u64) {
    // SAFETY: CPUID is always available on x86_64.
    let r = unsafe { core::arch::x86_64::__cpuid(1) };
    (u64::from(r.edx), 0)
}

#[cfg(target_arch = "aarch64")]
fn hwcaps() -> (u64, u64) {
    use windows::Win32::System::Threading::{
        IsProcessorFeaturePresent, PF_ARM_V8_CRC32_INSTRUCTIONS_AVAILABLE,
        PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE, PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE,
        PF_ARM_V82_DP_INSTRUCTIONS_AVAILABLE,
    };
    // SAFETY: feature queries.
    let has = |f| unsafe { IsProcessorFeaturePresent(f) }.as_bool();
    // FP and ASIMD are mandatory on Windows ARM64.
    let mut h = 1 | 2;
    if has(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE) {
        h |= (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
    }
    if has(PF_ARM_V8_CRC32_INSTRUCTIONS_AVAILABLE) {
        h |= 1 << 7;
    }
    if has(PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE) {
        h |= 1 << 8;
    }
    if has(PF_ARM_V82_DP_INSTRUCTIONS_AVAILABLE) {
        h |= 1 << 20;
    }
    (h, 0)
}

/// Writes the initial stack block below `top`; returns it and the new
/// top (below the block).
fn build_block(top: u64, args: &[String], env: &[String], hwcap: (u64, u64)) -> Startup {
    let mut strings: Vec<u8> = Vec::new();
    let mut offs = Vec::new();
    for s in args.iter().chain(env.iter()) {
        offs.push(strings.len() as u64);
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
    }
    let platform_off = strings.len() as u64;
    strings.extend_from_slice(if cfg!(target_arch = "x86_64") {
        b"x86_64\0"
    } else {
        b"aarch64\0"
    });
    let random_off = strings.len() as u64;
    let mut rnd = [0u8; 16];
    crate::tls::random_bytes(&mut rnd);
    strings.extend_from_slice(&rnd);
    let str_base = (top - strings.len() as u64) & !15;
    // SAFETY: the strings lie below `top` in the committed guest stack.
    unsafe { core::ptr::copy_nonoverlapping(strings.as_ptr(), str_base as *mut u8, strings.len()) };
    let uid = u64::from(andruna_lx::process::uid());
    let gid = u64::from(andruna_lx::process::gid());
    let aux: Vec<(u64, u64)> = vec![
        (AT_PHDR, 0),
        (AT_PHENT, 56),
        (AT_PHNUM, 0),
        (AT_PAGESZ, crate::image::PAGE),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, 0),
        (AT_UID, uid),
        (AT_EUID, uid),
        (AT_GID, gid),
        (AT_EGID, gid),
        (AT_PLATFORM, str_base + platform_off),
        (AT_HWCAP, hwcap.0),
        (AT_HWCAP2, hwcap.1),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, str_base + random_off),
        (AT_EXECFN, str_base + offs.first().copied().unwrap_or(0)),
        (AT_NULL, 0),
    ];
    let mut words: Vec<u64> = vec![args.len() as u64];
    words.extend(offs[..args.len()].iter().map(|o| str_base + o));
    words.push(0);
    let envp_index = words.len();
    words.extend(offs[args.len()..].iter().map(|o| str_base + o));
    words.push(0);
    let aux_index = words.len();
    for (k, v) in &aux {
        words.push(*k);
        words.push(*v);
    }
    let block = (str_base - words.len() as u64 * 8) & !15;
    // SAFETY: below the strings in the committed guest stack.
    unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), block as *mut u64, words.len()) };
    let slot = |key: u64| {
        let i = aux.iter().position(|(k, _)| *k == key).expect("aux key");
        block + (aux_index + 2 * i + 1) as u64 * 8
    };
    Startup {
        block,
        phdr_slot: slot(AT_PHDR),
        phnum_slot: slot(AT_PHNUM),
        entry_slot: slot(AT_ENTRY),
        argc: args.len() as i32,
        argv: block + 8,
        envp: block + envp_index as u64 * 8,
    }
}

/// What a program run needs.
pub struct Program {
    pub config: Config,
    pub exe: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
}

struct MainCtx {
    program: Program,
    stack: GuestStack,
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "sysv64" fn jump_to_entry(entry: u64, sp: u64) -> ! {
    core::arch::naked_asm!("mov rsp, rsi", "xor ebp, ebp", "xor edx, edx", "jmp rdi")
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn jump_to_entry(entry: u64, sp: u64) -> ! {
    core::arch::naked_asm!(
        "mov x16, x0",
        "mov sp, x1",
        "mov x29, xzr",
        "mov x30, xzr",
        "mov x0, xzr",
        "br x16"
    )
}

type MainFn = gty!(fn(i32, *const *const u8, *const *const u8) -> i32);

gfn! {
    /// Runs on the guest stack of the new main thread.
    unsafe fn main_thread(ctx: u64) -> ! {
        // SAFETY: the Box made by `run`, passed once.
        let ctx = unsafe { Box::from_raw(ctx as *mut MainCtx) };
        match start(*ctx) {
            Ok(n) => match n {},
            Err(e) => {
                crate::debug::report(format_args!("CANNOT LINK EXECUTABLE: {e}"));
                exit_process(1)
            }
        }
    }
}

fn start(ctx: MainCtx) -> Result<std::convert::Infallible> {
    let MainCtx { program, stack } = ctx;
    let (hwcap, hwcap2) = hwcaps();
    // The block sits at the top of the guest stack; this frame runs below
    // it and is discarded when the entry point starts.
    let s = build_block(stack.top(), &program.args, &program.env, (hwcap, hwcap2));
    let _ = ARGS.set(Args {
        argc: s.argc,
        argv: s.argv,
        envp: s.envp,
    });
    let (argc, argv, envp) = (s.argc, s.argv, s.envp);
    let block = s.block;
    let mut l = Linker::new(program.config);
    l.hwcap = hwcap;
    l.hwcap2 = hwcap2;
    l.startup = Some(s);
    let loaded = l.load(&program.exe, None, crate::linker::rtld::NOW)?;
    let main = l.get(loaded.root).expect("main");
    let entry = main.addr(main.headers.ehdr.e_entry);
    let main_fn = main
        .lookup(b"main", None)
        .map(|(_, s)| main.addr(s.st_value));
    let bionic = l.bridge.is_some();
    if let Some(b) = &l.bridge {
        b.finish_main_thread();
    } else {
        // No bionic: a built-in TLS area for the main thread.
        let area = crate::tls::ThreadArea::new(l.builtin_tls(), l.stack_guard);
        crate::tls::set_thread_pointer(area.tp);
        core::mem::forget(area);
    }
    let fns = l.init_fns(&loaded.init_order)?;
    crate::dl::install(l);
    run_init_fns(
        &fns,
        argc,
        argv as *const *const u8,
        envp as *const *const u8,
    );
    if bionic {
        // SAFETY: the executable's entry point with the stack at the
        // initial block, as the kernel starts a process; it never returns.
        unsafe { jump_to_entry(entry, block) }
    }
    let f = main_fn.ok_or_else(|| Error::new("no bionic and no exported main"))?;
    // SAFETY: the program's `main`, guest convention.
    let f: MainFn = unsafe { core::mem::transmute(f) };
    let rc = f(argc, argv as *const *const u8, envp as *const *const u8);
    exit_process(rc)
}

/// Starts `program` on a new guest main thread and waits forever: the
/// program ends the process (`exit_group`).
pub fn run(program: Program) -> Result<std::convert::Infallible> {
    crate::tls::supported()?;
    crate::debug::install_handler();
    let stack = GuestStack::new(MAIN_STACK)?;
    let top = stack.top();
    let ctx = Box::into_raw(Box::new(MainCtx { program, stack }));
    crate::thread::spawn_on_stack(top - 4096 * 4, main_thread as usize as u64, ctx as u64)?;
    loop {
        std::thread::park();
    }
}

/// The guest-visible path strings of `args`, checked for NUL bytes.
pub fn c_strings(v: &[String]) -> Result<Vec<CString>> {
    v.iter()
        .map(|s| CString::new(s.as_str()).map_err(|_| Error::new("argument contains NUL")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_layout() {
        let st = GuestStack::new(1 << 16).unwrap();
        let s = build_block(
            st.top(),
            &["prog".into(), "a".into()],
            &["X=1".into()],
            (5, 0),
        );
        // SAFETY: reads the block just written.
        unsafe {
            let w = s.block as *const u64;
            assert_eq!(*w, 2);
            let a0 = core::ffi::CStr::from_ptr(*w.add(1) as *const core::ffi::c_char);
            assert_eq!(a0.to_str().unwrap(), "prog");
            assert_eq!(*w.add(3), 0);
            let e0 = core::ffi::CStr::from_ptr(*w.add(4) as *const core::ffi::c_char);
            assert_eq!(e0.to_str().unwrap(), "X=1");
            assert_eq!(*w.add(5), 0);
            // auxv follows; AT_PHDR first.
            assert_eq!(*w.add(6), AT_PHDR);
            assert_eq!(s.phdr_slot, s.block + 7 * 8);
        }
        assert_eq!(s.block % 16, 0);
        assert_eq!(s.envp, s.block + 4 * 8);
    }
}

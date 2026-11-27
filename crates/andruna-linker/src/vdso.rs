//! The vDSO: clock functions guest code calls without a system call.
//!
//! Linux maps a small shared object (the vDSO) into every process and
//! passes its address as `AT_SYSINFO_EHDR`; bionic's `__libc_init_vdso`
//! looks up `clock_gettime`, `clock_getres`, `gettimeofday` (and `time` on
//! x86_64) in its dynamic symbol table, and libc's functions of those names
//! call them instead of entering the kernel. The loader does the same: it
//! builds a minimal ELF image in memory (ELF header, a `PT_LOAD` and a
//! `PT_DYNAMIC` program header, a `.dynsym` section header, the dynamic
//! table, the symbols and their names) whose symbols are host functions.
//!
//! The functions use the guest convention and run on the calling guest
//! stack, as the Linux vDSO does (they use a few hundred bytes of it). They
//! return what the system call would: 0 or `-errno`. The clocks are
//! andruna-lx's ([`andruna_lx::time::clock_gettime`]), so the values and
//! errors are exactly those of the `clock_gettime` system call; what is
//! saved is the system call path (the stack switch of `__andruna_syscall`,
//! the dispatch, lx's `errno` and its pending-signal check). As with Linux,
//! no signal is delivered at a vDSO call.
//!
//! Symbol names (bionic's `private/bionic_vdso.h`): `__vdso_*` on x86_64,
//! `__kernel_*` on arm64.

use std::sync::OnceLock;

use andruna_lx::linux::errno::{EFAULT, EIO};
use andruna_lx::linux::time::{CLOCK_REALTIME, Timespec};

/// `struct timeval` (bionic LP64).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `struct timezone`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timezone {
    pub tz_minuteswest: i32,
    pub tz_dsttime: i32,
}

const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<Timeval>() == 16);
    assert!(offset_of!(Timeval, tv_sec) == 0);
    assert!(offset_of!(Timeval, tv_usec) == 8);
    assert!(size_of::<Timezone>() == 8);
    assert!(offset_of!(Timezone, tz_minuteswest) == 0);
    assert!(offset_of!(Timezone, tz_dsttime) == 4);
};

fn neg(e: i32) -> i32 {
    -e
}

/// `clock_gettime` as the system call computes it: 0 or `-errno`.
///
/// # Safety
///
/// `tp` is NULL or writable for one `struct timespec`.
pub unsafe fn clock_gettime(clk: i32, tp: *mut Timespec) -> i32 {
    match andruna_lx::time::clock_gettime(clk) {
        Ok(_) if tp.is_null() => neg(EFAULT),
        Ok(t) => {
            // SAFETY: the caller's pointer (contract), not NULL.
            unsafe { tp.write_unaligned(t) };
            0
        }
        Err(e) => neg(e.0),
    }
}

/// `clock_getres`: 0 or `-errno`; a NULL `res` is allowed.
///
/// # Safety
///
/// `res` is NULL or writable for one `struct timespec`.
pub unsafe fn clock_getres(clk: i32, res: *mut Timespec) -> i32 {
    match andruna_lx::time::clock_getres(clk) {
        Ok(t) => {
            if !res.is_null() {
                // SAFETY: the caller's pointer (contract), not NULL.
                unsafe { res.write_unaligned(t) };
            }
            0
        }
        Err(e) => neg(e.0),
    }
}

/// `gettimeofday`: `CLOCK_REALTIME` in microseconds; the time zone (when
/// asked for) is UTC with no daylight saving time, Linux's default
/// `sys_tz`. 0 or `-errno`.
///
/// # Safety
///
/// `tv` and `tz` are NULL or writable for their structure.
pub unsafe fn gettimeofday(tv: *mut Timeval, tz: *mut Timezone) -> i32 {
    if !tv.is_null() {
        let t = match andruna_lx::time::clock_gettime(CLOCK_REALTIME) {
            Ok(t) => t,
            Err(e) => return neg(e.0),
        };
        let v = Timeval {
            tv_sec: t.tv_sec,
            tv_usec: t.tv_nsec / 1000,
        };
        // SAFETY: the caller's pointer (contract), not NULL.
        unsafe { tv.write_unaligned(v) };
    }
    if !tz.is_null() {
        // SAFETY: as above.
        unsafe { tz.write_unaligned(Timezone::default()) };
    }
    0
}

/// `time`: seconds of `CLOCK_REALTIME`, also stored through `t` when it is
/// not NULL.
///
/// # Safety
///
/// `t` is NULL or writable for one `time_t`.
pub unsafe fn time(t: *mut i64) -> i64 {
    let s = match andruna_lx::time::clock_gettime(CLOCK_REALTIME) {
        Ok(v) => v.tv_sec,
        Err(e) => return i64::from(neg(e.0)),
    };
    if !t.is_null() {
        // SAFETY: the caller's pointer (contract), not NULL.
        unsafe { t.write_unaligned(s) };
    }
    s
}

gfn! {
    unsafe fn vdso_clock_gettime(clk: i32, tp: *mut Timespec) -> i32 {
        crate::profile::vdso_call();
        // SAFETY: guest code calls with the system call's contract.
        crate::host::guard(neg(EIO), || unsafe { clock_gettime(clk, tp) })
    }
}

gfn! {
    unsafe fn vdso_clock_getres(clk: i32, res: *mut Timespec) -> i32 {
        crate::profile::vdso_call();
        // SAFETY: as above.
        crate::host::guard(neg(EIO), || unsafe { clock_getres(clk, res) })
    }
}

gfn! {
    unsafe fn vdso_gettimeofday(tv: *mut Timeval, tz: *mut Timezone) -> i32 {
        crate::profile::vdso_call();
        // SAFETY: as above.
        crate::host::guard(neg(EIO), || unsafe { gettimeofday(tv, tz) })
    }
}

#[cfg(target_arch = "x86_64")]
gfn! {
    unsafe fn vdso_time(t: *mut i64) -> i64 {
        crate::profile::vdso_call();
        // SAFETY: as above.
        crate::host::guard(i64::from(neg(EIO)), || unsafe { time(t) })
    }
}

/// The exported functions: (name, address).
fn functions() -> Vec<(&'static str, u64)> {
    #[cfg(target_arch = "x86_64")]
    {
        vec![
            ("__vdso_clock_gettime", vdso_clock_gettime as usize as u64),
            ("__vdso_clock_getres", vdso_clock_getres as usize as u64),
            ("__vdso_gettimeofday", vdso_gettimeofday as usize as u64),
            ("__vdso_time", vdso_time as usize as u64),
        ]
    }
    #[cfg(target_arch = "aarch64")]
    {
        vec![
            ("__kernel_clock_gettime", vdso_clock_gettime as usize as u64),
            ("__kernel_clock_getres", vdso_clock_getres as usize as u64),
            ("__kernel_gettimeofday", vdso_gettimeofday as usize as u64),
        ]
    }
}

const EHDR: usize = 64;
const PHDR: usize = 56;
const SHDR: usize = 64;
const SYM: usize = 24;
const DYN: usize = 16;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_DYNSYM: u32 = 11;
const DT_NULL: u64 = 0;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;
const SHN_ABS: u16 = 0xfff1;
#[cfg(target_arch = "x86_64")]
const EM: u16 = 62;
#[cfg(target_arch = "aarch64")]
const EM: u16 = 183;

/// Writes the ELF image for `funcs` into `out` (8-byte aligned, at
/// address `base`). Offsets equal virtual addresses (one `PT_LOAD` at 0),
/// so symbol values are the functions' addresses minus `base`.
fn build(base: u64, funcs: &[(&str, u64)]) -> Vec<u8> {
    let mut strtab = vec![0u8];
    let mut names = Vec::new();
    for (n, _) in funcs {
        names.push(strtab.len() as u32);
        strtab.extend_from_slice(n.as_bytes());
        strtab.push(0);
    }
    let nsym = funcs.len() + 1;
    let phoff = EHDR;
    let shoff = phoff + 2 * PHDR;
    let symoff = shoff + 3 * SHDR;
    let dynoff = symoff + nsym * SYM;
    let ndyn = 5;
    let stroff = dynoff + ndyn * DYN;
    let total = (stroff + strtab.len()).next_multiple_of(8);
    let mut b = vec![0u8; total];
    let put = |b: &mut Vec<u8>, at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
    // ELF header.
    put(&mut b, 0, b"\x7fELF");
    b[4] = 2; // ELFCLASS64
    b[5] = 1; // ELFDATA2LSB
    b[6] = 1; // EV_CURRENT
    put(&mut b, 16, &3u16.to_le_bytes()); // ET_DYN
    put(&mut b, 18, &EM.to_le_bytes());
    put(&mut b, 20, &1u32.to_le_bytes());
    put(&mut b, 32, &(phoff as u64).to_le_bytes());
    put(&mut b, 40, &(shoff as u64).to_le_bytes());
    put(&mut b, 52, &(EHDR as u16).to_le_bytes());
    put(&mut b, 54, &(PHDR as u16).to_le_bytes());
    put(&mut b, 56, &2u16.to_le_bytes());
    put(&mut b, 58, &(SHDR as u16).to_le_bytes());
    put(&mut b, 60, &3u16.to_le_bytes());
    // Program headers: the whole image, then the dynamic table.
    let phdr = |b: &mut Vec<u8>, at: usize, ty: u32, off: usize, size: usize| {
        put(b, at, &ty.to_le_bytes());
        put(b, at + 4, &4u32.to_le_bytes()); // PF_R
        put(b, at + 8, &(off as u64).to_le_bytes());
        put(b, at + 16, &(off as u64).to_le_bytes());
        put(b, at + 24, &(off as u64).to_le_bytes());
        put(b, at + 32, &(size as u64).to_le_bytes());
        put(b, at + 40, &(size as u64).to_le_bytes());
        put(b, at + 48, &8u64.to_le_bytes());
    };
    phdr(&mut b, phoff, PT_LOAD, 0, total);
    phdr(&mut b, phoff + PHDR, PT_DYNAMIC, dynoff, ndyn * DYN);
    // Section headers: null, .dynsym (linked to .dynstr), .dynstr.
    let shdr =
        |b: &mut Vec<u8>, at: usize, ty: u32, off: usize, size: usize, link: u32, ent: usize| {
            put(b, at + 4, &ty.to_le_bytes());
            put(b, at + 8, &2u64.to_le_bytes()); // SHF_ALLOC
            put(b, at + 16, &(off as u64).to_le_bytes());
            put(b, at + 24, &(off as u64).to_le_bytes());
            put(b, at + 32, &(size as u64).to_le_bytes());
            put(b, at + 40, &link.to_le_bytes());
            put(b, at + 44, &1u32.to_le_bytes()); // sh_info: first global symbol
            put(b, at + 48, &8u64.to_le_bytes());
            put(b, at + 56, &(ent as u64).to_le_bytes());
        };
    shdr(&mut b, shoff + SHDR, SHT_DYNSYM, symoff, nsym * SYM, 2, SYM);
    shdr(
        &mut b,
        shoff + 2 * SHDR,
        SHT_STRTAB,
        stroff,
        strtab.len(),
        0,
        0,
    );
    // Symbols (the first is the null symbol).
    for (i, (_, addr)) in funcs.iter().enumerate() {
        let at = symoff + (i + 1) * SYM;
        put(&mut b, at, &names[i].to_le_bytes());
        b[at + 4] = 0x12; // STB_GLOBAL, STT_FUNC
        put(&mut b, at + 6, &SHN_ABS.to_le_bytes());
        put(&mut b, at + 8, &addr.wrapping_sub(base).to_le_bytes());
    }
    // Dynamic table.
    for (i, (tag, val)) in [
        (DT_STRTAB, stroff as u64),
        (DT_SYMTAB, symoff as u64),
        (DT_STRSZ, strtab.len() as u64),
        (DT_SYMENT, SYM as u64),
        (DT_NULL, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut b, dynoff + i * DYN, &tag.to_le_bytes());
        put(&mut b, dynoff + i * DYN + 8, &val.to_le_bytes());
    }
    put(&mut b, stroff, &strtab);
    b
}

/// The address of the vDSO image (`AT_SYSINFO_EHDR`), built on first use
/// and kept for the life of the process.
pub fn image() -> u64 {
    static IMAGE: OnceLock<u64> = OnceLock::new();
    *IMAGE.get_or_init(|| {
        let funcs = functions();
        // The size does not depend on the base; build once to learn it,
        // then again at the final address.
        let len = build(0, &funcs).len();
        let mem: &'static mut [u64] = Box::leak(vec![0u64; len / 8].into_boxed_slice());
        let base = mem.as_ptr() as u64;
        let img = build(base, &funcs);
        // SAFETY: `mem` is `len` bytes, owned and leaked here.
        unsafe { core::ptr::copy_nonoverlapping(img.as_ptr(), mem.as_mut_ptr().cast(), len) };
        base
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use andruna_lx::linux::errno::EINVAL;
    use andruna_lx::linux::time::CLOCK_MONOTONIC;

    /// Looks a symbol up the way bionic's `__libc_init_vdso` does.
    fn lookup(ehdr: u64, name: &str) -> Option<u64> {
        // SAFETY: reads the image `image()` built.
        unsafe {
            let rd64 = |a: u64| (a as *const u64).read_unaligned();
            let rd32 = |a: u64| (a as *const u32).read_unaligned();
            let rd16 = |a: u64| u64::from((a as *const u16).read_unaligned());
            let (phoff, shoff) = (rd64(ehdr + 32), rd64(ehdr + 40));
            let (phnum, shnum) = (rd16(ehdr + 56), rd16(ehdr + 60));
            let mut count = 0;
            for i in 0..shnum {
                let s = ehdr + shoff + i * 64;
                if rd32(s + 4) == SHT_DYNSYM {
                    count = rd64(s + 32) / 24;
                }
            }
            let (mut addr, mut dynamic) = (0, 0);
            for i in 0..phnum {
                let p = ehdr + phoff + i * 56;
                match rd32(p) {
                    PT_DYNAMIC => dynamic = ehdr + rd64(p + 8),
                    PT_LOAD => addr = ehdr + rd64(p + 8) - rd64(p + 16),
                    _ => {}
                }
            }
            let (mut strtab, mut symtab) = (0, 0);
            let mut d = dynamic;
            while rd64(d) != DT_NULL {
                match rd64(d) {
                    DT_STRTAB => strtab = addr + rd64(d + 8),
                    DT_SYMTAB => symtab = addr + rd64(d + 8),
                    _ => {}
                }
                d += 16;
            }
            for j in 0..count {
                let s = symtab + j * 24;
                let n = core::ffi::CStr::from_ptr((strtab + u64::from(rd32(s))) as *const _);
                if n.to_bytes() == name.as_bytes() {
                    return Some(addr.wrapping_add(rd64(s + 8)));
                }
            }
            None
        }
    }

    #[test]
    fn image_exports_the_functions_bionic_looks_up() {
        let e = image();
        // SAFETY: the image starts with the ELF magic.
        assert_eq!(unsafe { *(e as *const [u8; 4]) }, *b"\x7fELF");
        for (n, a) in functions() {
            assert_eq!(lookup(e, n), Some(a), "{n}");
        }
        assert_eq!(lookup(e, "__vdso_getcpu"), None);
    }

    #[test]
    fn clocks_match_the_system_call() {
        let mut t = Timespec::default();
        // SAFETY: valid pointers or NULL.
        unsafe {
            assert_eq!(clock_gettime(CLOCK_MONOTONIC, &mut t), 0);
            let lx = andruna_lx::time::clock_gettime(CLOCK_MONOTONIC).unwrap();
            assert!(lx.as_nanos() >= t.as_nanos());
            assert!(lx.as_nanos() - t.as_nanos() < 1_000_000_000);
            assert_eq!(
                clock_gettime(CLOCK_MONOTONIC, core::ptr::null_mut()),
                -EFAULT
            );
            assert_eq!(clock_gettime(12345, &mut t), -EINVAL);
            // CPU-time clocks go through lx too.
            assert_eq!(
                clock_gettime(andruna_lx::linux::time::CLOCK_THREAD_CPUTIME_ID, &mut t),
                0
            );
            assert_eq!(clock_getres(CLOCK_MONOTONIC, core::ptr::null_mut()), 0);
            assert_eq!(clock_getres(CLOCK_MONOTONIC, &mut t), 0);
            assert_eq!(
                t,
                Timespec {
                    tv_sec: 0,
                    tv_nsec: 100
                }
            );
            assert_eq!(clock_getres(12345, &mut t), -EINVAL);
        }
    }

    #[test]
    fn wall_clock_functions() {
        let mut tv = Timeval::default();
        let mut tz = Timezone {
            tz_minuteswest: 7,
            tz_dsttime: 1,
        };
        let mut s = 0i64;
        // SAFETY: valid pointers or NULL.
        unsafe {
            assert_eq!(gettimeofday(&mut tv, &mut tz), 0);
            assert_eq!(tz, Timezone::default());
            assert!((0..1_000_000).contains(&tv.tv_usec));
            assert_eq!(
                gettimeofday(core::ptr::null_mut(), core::ptr::null_mut()),
                0
            );
            let r = time(&mut s);
            assert_eq!(r, s);
            assert!(r >= tv.tv_sec && r - tv.tv_sec < 5);
            assert!(time(core::ptr::null_mut()) >= r);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((tv.tv_sec - now).abs() < 5);
    }
}

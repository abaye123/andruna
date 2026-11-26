//! x64 calling-convention experiments: System V (ART managed code, JNI)
//! against Microsoft x64 (Windows and our AOSP C/C++ code).

use crate::util;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Target of `s1_win64_to_sysv` (a System V function).
pub static S1_SYSV_TARGET: AtomicUsize = AtomicUsize::new(0);
/// Target of `s1_sysv_to_win64` (a Windows-ABI function).
pub static S1_WIN64_TARGET: AtomicUsize = AtomicUsize::new(0);

extern "sysv64" {
    fn s1_sysv_hand(a: u64, b: u64, cb: extern "win64" fn(u64, u64) -> u64) -> u64;
    fn s1_sysv_to_win64(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64;
    fn s1_sysv_clobber() -> u64;
}
extern "win64" {
    fn s1_win64_to_sysv(a: u64, b: u64, c: u64, d: u64) -> u64;
    fn s1_check_callee_saved(f: usize) -> u32;
}

extern "win64" fn win_cb(a: u64, b: u64) -> u64 {
    a * 1000 + b
}
extern "win64" fn win_sum6(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64 {
    a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f
}
extern "sysv64" fn sysv_digits(a: u64, b: u64, c: u64, d: u64) -> u64 {
    a * 1000 + b * 100 + c * 10 + d
}
#[inline(never)]
extern "win64" fn win_add(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}
#[inline(never)]
extern "sysv64" fn sysv_add(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}
/// Windows-ABI function whose body calls System V code; rustc must save
/// rsi, rdi and xmm6-xmm15 around the call.
#[inline(never)]
extern "win64" fn win_calls_sysv_clobber() -> u64 {
    // SAFETY: s1_sysv_clobber is a System V leaf with no arguments.
    unsafe { s1_sysv_clobber() }
}
#[inline(never)]
extern "win64" fn win_wraps_sysv_add(a: u64, b: u64) -> u64 {
    let f: extern "sysv64" fn(u64, u64) -> u64 = core::hint::black_box(sysv_add);
    f(a, b)
}

pub struct AbiResult {
    pub hand_ok: bool,
    pub sysv_to_win64_ok: bool,
    pub win64_to_sysv_ok: bool,
    pub mask_raw: u32,
    pub mask_trampoline: u32,
    pub mask_rustc: u32,
    pub ns_win_win: f64,
    pub ns_win_sysv_rustc: f64,
    pub ns_win_sysv_tramp: f64,
    pub ns_sysv_win_tramp: f64,
}

pub fn run() -> AbiResult {
    // SAFETY (all calls): the assembly functions follow the conventions
    // declared above; the targets are set before the trampolines run.
    unsafe {
        let hand_ok = s1_sysv_hand(5, 7, win_cb) == 10 * 1000 + 21 + 1;
        S1_WIN64_TARGET.store(win_sum6 as usize, Ordering::Release);
        let sysv_to_win64_ok = s1_sysv_to_win64(1, 2, 3, 4, 5, 6) == 91;
        S1_SYSV_TARGET.store(sysv_digits as usize, Ordering::Release);
        let win64_to_sysv_ok = s1_win64_to_sysv(1, 2, 3, 4) == 1234;

        let mask_raw = s1_check_callee_saved(s1_sysv_clobber as usize);
        S1_SYSV_TARGET.store(s1_sysv_clobber as usize, Ordering::Release);
        let mask_trampoline = s1_check_callee_saved(s1_win64_to_sysv as usize);
        let mask_rustc = s1_check_callee_saved(win_calls_sysv_clobber as usize);

        let n = 20_000_000;
        let ww: extern "win64" fn(u64, u64) -> u64 = core::hint::black_box(win_add);
        let ns_win_win = util::time_ns(n, || ww(1, 2));
        let wr: extern "win64" fn(u64, u64) -> u64 = core::hint::black_box(win_wraps_sysv_add);
        let ns_win_sysv_rustc = util::time_ns(n, || wr(1, 2));
        S1_SYSV_TARGET.store(sysv_add as usize, Ordering::Release);
        let wt: unsafe extern "win64" fn(u64, u64, u64, u64) -> u64 =
            core::hint::black_box(s1_win64_to_sysv);
        let ns_win_sysv_tramp = util::time_ns(n, || wt(1, 2, 0, 0));
        S1_WIN64_TARGET.store(win_sum6 as usize, Ordering::Release);
        let st: unsafe extern "sysv64" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            core::hint::black_box(s1_sysv_to_win64);
        let ns_sysv_win_tramp = util::time_ns(n, || st(1, 2, 3, 4, 5, 6));

        AbiResult {
            hand_ok,
            sysv_to_win64_ok,
            win64_to_sysv_ok,
            mask_raw,
            mask_trampoline,
            mask_rustc,
            ns_win_win,
            ns_win_sysv_rustc,
            ns_win_sysv_tramp,
            ns_sysv_win_tramp,
        }
    }
}

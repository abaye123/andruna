//! Guest threads on Windows threads.
//!
//! bionic creates threads itself (`pthread_create` allocates the stack and
//! the static TLS, then calls `__bionic_clone`); in Andruna's build
//! `__bionic_clone` calls [`clone_thread`] (`__andruna_clone_thread`),
//! which starts a Windows thread that sets the thread pointer, switches to
//! bionic's stack and calls bionic's `__start_thread(fn, arg)`. The kernel
//! parts of `clone` are done here: `CLONE_PARENT_SETTID`,
//! `CLONE_CHILD_SETTID`, `CLONE_SETTLS`, and at exit
//! `CLONE_CHILD_CLEARTID` (store 0, futex wake), which `pthread_join`
//! waits on.
//!
//! While a thread runs on a guest stack, the TEB's stack bounds describe
//! that stack, so Windows exception dispatch and stack walks accept it.
//! Guest stacks are fully committed memory: Linux code does not probe the
//! stack page by page, so a lazily committed Windows stack could be skipped
//! by a large frame.

use core::ffi::c_void;
use std::cell::Cell;
use std::sync::atomic::{AtomicI32, Ordering};

use andruna_lx::linux::errno::*;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateThread, ExitThread, GetThreadId, ResumeThread,
    STACK_SIZE_PARAM_IS_A_RESERVATION, THREAD_CREATION_FLAGS,
};

pub const CLONE_VM: i32 = 0x100;
pub const CLONE_THREAD: i32 = 0x10000;
pub const CLONE_SETTLS: i32 = 0x80000;
pub const CLONE_PARENT_SETTID: i32 = 0x100000;
pub const CLONE_CHILD_CLEARTID: i32 = 0x200000;
pub const CLONE_CHILD_SETTID: i32 = 0x1000000;

/// Windows stack of a guest thread's host side (the guest runs on its own
/// stack; this one only holds the start frame).
const HOST_STACK: usize = 256 << 10;

thread_local! {
    /// `CLONE_CHILD_CLEARTID` / `set_tid_address` word.
    static CLEAR_TID: Cell<u64> = const { Cell::new(0) };
    /// Stack pointer on the Windows stack, saved when switching to the
    /// guest stack.
    static HOST_SP: Cell<u64> = const { Cell::new(0) };
    /// TEB stack bounds before switching (base, limit).
    static SAVED_BOUNDS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// The TEB of the current thread.
fn teb() -> *mut u64 {
    let t: *mut u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: gs:[0x30] is the TEB's self pointer on x64 Windows.
    unsafe {
        core::arch::asm!("mov {}, gs:[0x30]", out(reg) t, options(nostack, readonly, preserves_flags))
    };
    #[cfg(target_arch = "aarch64")]
    // SAFETY: x18 holds the TEB in user mode on Windows ARM64.
    unsafe {
        core::arch::asm!("mov {}, x18", out(reg) t, options(nomem, nostack, preserves_flags))
    };
    t
}

/// The TEB's `NT_TIB` stack base and limit.
fn stack_bounds() -> (u64, u64) {
    let t = teb();
    // SAFETY: NT_TIB.StackBase and StackLimit are the second and third
    // words of the TEB.
    unsafe { (*t.add(1), *t.add(2)) }
}

fn set_stack_bounds(base: u64, limit: u64) {
    let t = teb();
    // SAFETY: as in `stack_bounds`; the thread writes only its own TEB.
    unsafe {
        t.add(1).write_volatile(base);
        t.add(2).write_volatile(limit);
    }
}

/// The committed region around a guest stack top.
fn region_of(top: u64) -> (u64, u64) {
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: queries the address space only.
    let n = unsafe {
        VirtualQuery(
            Some((top - 16) as *const c_void),
            &mut mbi,
            core::mem::size_of_val(&mbi),
        )
    };
    if n == 0 {
        return (top, top - 4096);
    }
    let lo = mbi.BaseAddress as u64;
    (lo + mbi.RegionSize as u64, lo)
}

/// Switches the current thread to the guest stack `top` and calls the
/// guest function `entry(a0, a1)`; never returns.
fn enter_stack(top: u64, entry: u64, a0: u64, a1: u64) -> ! {
    SAVED_BOUNDS.with(|c| c.set(stack_bounds()));
    let (base, limit) = region_of(top);
    set_stack_bounds(base, limit);
    let host_sp = HOST_SP.with(|c| c.as_ptr());
    // SAFETY: `top` is the top of a committed stack owned by this thread
    // from now on; `entry` is a guest function with two arguments that does
    // not return (it ends the thread or the process).
    unsafe { switch_and_call(top, entry, a0, a1, host_sp) }
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "sysv64" fn switch_and_call(
    top: u64,
    entry: u64,
    a0: u64,
    a1: u64,
    save: *mut u64,
) -> ! {
    core::arch::naked_asm!(
        "mov [r8], rsp",
        "mov rsp, rdi",
        "and rsp, -16",
        "xor ebp, ebp",
        "mov rax, rsi",
        "mov rdi, rdx",
        "mov rsi, rcx",
        "call rax",
        "ud2",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn switch_and_call(top: u64, entry: u64, a0: u64, a1: u64, save: *mut u64) -> ! {
    core::arch::naked_asm!(
        "mov x9, sp",
        "str x9, [x4]",
        "and x9, x0, #-16",
        "mov sp, x9",
        "mov x16, x1",
        "mov x0, x2",
        "mov x1, x3",
        "mov x29, xzr",
        "mov x30, xzr",
        "blr x16",
        "brk #0",
    )
}

/// Runs `f(a0, a1)` on the saved Windows stack of this thread; never
/// returns.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "sysv64" fn on_host_stack(sp: u64, f: u64, a0: u64, a1: u64) -> ! {
    core::arch::naked_asm!(
        "mov rsp, rdi",
        "and rsp, -16",
        "mov rax, rsi",
        "mov rdi, rdx",
        "mov rsi, rcx",
        "call rax",
        "ud2",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn on_host_stack(sp: u64, f: u64, a0: u64, a1: u64) -> ! {
    core::arch::naked_asm!(
        "and x9, x0, #-16",
        "mov sp, x9",
        "mov x16, x1",
        "mov x0, x2",
        "mov x1, x3",
        "blr x16",
        "brk #0",
    )
}

gfn! {
    /// The end of a guest thread, on its Windows stack: unmaps the guest
    /// stack if asked, then clears the tid word and wakes the joiner. As
    /// with the kernel, the word is cleared only once the thread no longer
    /// uses its guest stack, because the joiner may free that stack.
    unsafe fn teardown(stack: u64, size: u64) -> () {
        if size != 0 {
            let _ = andruna_lx::mem::munmap(stack as usize, size as usize);
        }
        clear_tid();
        // SAFETY: ends the current thread; nothing on the guest stack is
        // used afterwards.
        unsafe { ExitThread(0) }
    }
}

fn clear_tid() {
    crate::signal::thread_exited(andruna_lx::process::gettid());
    let ctid = CLEAR_TID.with(|c| c.replace(0));
    if ctid != 0 && ctid.is_multiple_of(4) {
        // SAFETY: the word registered by clone or set_tid_address, an
        // aligned i32 the thread's owner keeps alive until it is cleared.
        unsafe { (*(ctid as *const AtomicI32)).store(0, Ordering::Release) };
        andruna_lx::futex::wake(ctid as usize, 1);
    }
}

/// `set_tid_address`: remembers the word to clear at exit.
pub fn set_tid_address(p: u64) -> i64 {
    CLEAR_TID.with(|c| c.set(p));
    i64::from(andruna_lx::process::gettid())
}

/// Ends the current thread (`exit`, `_exit_with_stack_teardown`).
pub fn exit_current(status: i32, unmap: Option<(u64, u64)>) -> ! {
    let (base, limit) = SAVED_BOUNDS.with(|c| c.get());
    let host_sp = HOST_SP.with(|c| c.get());
    if base != 0 {
        set_stack_bounds(base, limit);
    }
    if host_sp != 0 {
        let (stack, size) = unmap.unwrap_or((0, 0));
        // SAFETY: `host_sp` is this thread's Windows stack, unused since the
        // switch; `teardown` never returns.
        unsafe { on_host_stack(host_sp, teardown as usize as u64, stack, size) }
    }
    clear_tid();
    // SAFETY: ends the current thread.
    unsafe { ExitThread(status as u32) }
}

struct Start {
    flags: i32,
    stack: u64,
    tls: u64,
    ctid: u64,
    entry: u64,
    func: u64,
    arg: u64,
}

unsafe extern "system" fn thread_start(p: *mut c_void) -> u32 {
    // SAFETY: `p` is the Box made by `clone_thread`, passed once.
    let s = unsafe { Box::from_raw(p as *mut Start) };
    let Start {
        flags,
        stack,
        tls,
        ctid,
        entry,
        func,
        arg,
    } = *s;
    if flags & CLONE_SETTLS != 0 {
        crate::tls::set_thread_pointer(tls);
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        CLEAR_TID.with(|c| c.set(ctid));
    }
    enter_stack(stack, entry, func, arg)
}

/// `__andruna_clone_thread`: the thread part of `clone` (the kernel's
/// argument list).
#[allow(clippy::too_many_arguments)]
fn clone_thread(
    flags: i32,
    stack: u64,
    ptid: *mut i32,
    tls: u64,
    ctid: *mut i32,
    entry: u64,
    func: u64,
    arg: u64,
) -> i32 {
    if flags & (CLONE_VM | CLONE_THREAD) != (CLONE_VM | CLONE_THREAD) {
        // A new process: Windows cannot fork.
        return -ENOSYS;
    }
    if stack == 0 || entry == 0 {
        return -EINVAL;
    }
    let start = Box::into_raw(Box::new(Start {
        flags,
        stack,
        tls,
        ctid: ctid as u64,
        entry,
        func,
        arg,
    }));
    let mut tid = 0u32;
    // SAFETY: a valid start routine and parameter; the thread starts
    // suspended so the ids are stored before it runs.
    let h = unsafe {
        CreateThread(
            None,
            HOST_STACK,
            Some(thread_start),
            Some(start as *const c_void),
            THREAD_CREATION_FLAGS(CREATE_SUSPENDED.0 | STACK_SIZE_PARAM_IS_A_RESERVATION.0),
            Some(&mut tid),
        )
    };
    let h: HANDLE = match h {
        Ok(h) => h,
        Err(_) => {
            // SAFETY: the thread was not created; the Box is still ours.
            drop(unsafe { Box::from_raw(start) });
            return -EAGAIN;
        }
    };
    // SAFETY: querying the new thread's id.
    let tid = unsafe { GetThreadId(h) } as i32;
    crate::signal::inherit(tid);
    if flags & CLONE_PARENT_SETTID != 0 && !ptid.is_null() {
        // SAFETY: the caller's tid word (bionic's pthread_internal_t).
        unsafe { ptid.write_volatile(tid) };
    }
    if flags & CLONE_CHILD_SETTID != 0 && !ctid.is_null() {
        // SAFETY: as above.
        unsafe { ctid.write_volatile(tid) };
    }
    // The new thread starts with the creator's signal mask, as on Linux.
    andruna_lx::signal::inherit_to(tid);
    // SAFETY: resumes and releases the thread handle.
    unsafe {
        ResumeThread(h);
        let _ = CloseHandle(h);
    }
    tid
}

gfn! {
    #[allow(clippy::too_many_arguments)]
    unsafe fn h_clone_thread(
        flags: i32,
        stack: u64,
        ptid: *mut i32,
        tls: u64,
        ctid: *mut i32,
        entry: u64,
        func: u64,
        arg: u64,
    ) -> i32 {
        crate::host::guard(-EAGAIN, || clone_thread(flags, stack, ptid, tls, ctid, entry, func, arg))
    }
}

gfn! {
    unsafe fn h_exit_with_stack_teardown(stack: u64, size: u64) -> () {
        exit_current(0, Some((stack, size)))
    }
}

gfn! {
    unsafe fn h_set_tls(tp: u64) -> () {
        crate::tls::set_thread_pointer(tp)
    }
}

/// A committed guest stack for a thread the loader starts itself.
pub struct GuestStack {
    pub base: u64,
    pub size: u64,
}

impl GuestStack {
    /// Maps `size` bytes read-write with a `PROT_NONE` guard page below.
    pub fn new(size: u64) -> crate::Result<Self> {
        use andruna_lx::linux::mman::*;
        let g = PAGE_SIZE as u64;
        let a = andruna_lx::mem::mmap(
            0,
            (size + g) as usize,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )? as u64;
        andruna_lx::mem::mprotect(a as usize, g as usize, PROT_NONE)?;
        Ok(Self { base: a + g, size })
    }

    pub fn top(&self) -> u64 {
        self.base + self.size
    }
}

/// Starts a Windows thread that switches to `stack` (top `top`, which
/// may be below data the caller placed on it) and calls the host function
/// `f(arg)` there; `f` never returns (it enters guest code or ends the
/// process). Returns once the thread runs.
pub fn spawn_on_stack(top: u64, f: u64, arg: u64) -> crate::Result<()> {
    let start = Box::into_raw(Box::new(Start {
        flags: 0,
        stack: top,
        tls: 0,
        ctid: 0,
        entry: f,
        func: arg,
        arg: 0,
    }));
    // SAFETY: as in `clone_thread`.
    let h = unsafe {
        CreateThread(
            None,
            HOST_STACK,
            Some(thread_start),
            Some(start as *const c_void),
            STACK_SIZE_PARAM_IS_A_RESERVATION,
            None,
        )
    }
    .map_err(|e| crate::Error::new(format!("CreateThread failed: {e}")))?;
    // SAFETY: releases the handle; the thread keeps running.
    let _ = unsafe { CloseHandle(h) };
    Ok(())
}

pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![
        ("__andruna_clone_thread", h_clone_thread as usize as u64),
        (
            "__andruna_exit_with_stack_teardown",
            h_exit_with_stack_teardown as usize as u64,
        ),
        ("__andruna_set_tls", h_set_tls as usize as u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static SEEN: AtomicU64 = AtomicU64::new(0);

    gfn! {
        unsafe fn child(func: u64, arg: u64) -> () {
            // Runs on the guest stack with the thread pointer set.
            let local = 0u8;
            let sp = &local as *const u8 as u64;
            SEEN.store(func ^ arg ^ crate::tls::current(), Ordering::SeqCst);
            assert!(sp < arg && sp > arg - (1 << 20));
            let (base, _) = stack_bounds();
            assert!(base >= arg);
            exit_current(0, None)
        }
    }

    #[test]
    fn clone_runs_on_the_given_stack_and_clears_tid() {
        let stack = GuestStack::new(1 << 20).unwrap();
        let tls = Box::new([0u64; 8]);
        let tid = Box::new(AtomicI32::new(-1));
        let ctid = tid.as_ptr();
        let flags =
            CLONE_VM | CLONE_THREAD | CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID;
        let r = clone_thread(
            flags,
            stack.top(),
            ctid,
            tls.as_ptr() as u64,
            ctid,
            child as usize as u64,
            0x55,
            stack.top(),
        );
        assert!(r > 0);
        // Wait for CLONE_CHILD_CLEARTID.
        for _ in 0..2000 {
            if tid.load(Ordering::Acquire) == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(tid.load(Ordering::Acquire), 0);
        assert_eq!(
            SEEN.load(Ordering::SeqCst),
            0x55 ^ stack.top() ^ tls.as_ptr() as u64
        );
    }

    #[test]
    fn processes_cannot_be_cloned() {
        assert_eq!(
            clone_thread(
                0,
                1,
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                1,
                0,
                0
            ),
            -ENOSYS
        );
    }
}

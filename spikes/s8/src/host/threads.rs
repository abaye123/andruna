//! Threads for guest code: pthread on Windows threads, with bionic's
//! object layouts and lx's futex for waiting.
//!
//! A real build uses bionic's pthread over lx (`clone` and `futex`); this
//! is the spike's shim. Layouts (LP64 bionic): `pthread_mutex_t` 40 bytes,
//! `pthread_cond_t` 48, `pthread_rwlock_t` 56, `pthread_attr_t` 56,
//! `pthread_once_t` and `pthread_key_t` 4, `pthread_t` a pointer.

use core::ffi::c_void;
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

use andruna_lx::futex::Deadline;
use andruna_lx::linux::errno::*;
use andruna_lx::linux::time::Timespec;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    AcquireSRWLockExclusive, AcquireSRWLockShared, CreateThread, INFINITE, ReleaseSRWLockExclusive,
    ReleaseSRWLockShared, SRWLOCK, THREAD_CREATION_FLAGS, TryAcquireSRWLockExclusive, TryAcquireSRWLockShared,
    WaitForSingleObject,
};

use super::guard;
use crate::tls::ThreadArea;

#[cfg(target_arch = "x86_64")]
pub type StartFn = extern "sysv64" fn(*mut c_void) -> *mut c_void;
#[cfg(not(target_arch = "x86_64"))]
pub type StartFn = extern "C" fn(*mut c_void) -> *mut c_void;
#[cfg(target_arch = "x86_64")]
pub type DtorFn = extern "sysv64" fn(*mut c_void);
#[cfg(not(target_arch = "x86_64"))]
pub type DtorFn = extern "C" fn(*mut c_void);

/// Committed stack of guest threads: guest code is compiled for Linux and
/// does not probe the stack page by page, so a lazily committed Windows
/// stack (guard page) can be skipped by a large frame. Committing it all
/// avoids that (S8.md, "Stacks").
pub const GUEST_STACK: usize = 8 << 20;

/// `pthread_t` points to this.
pub struct GuestThread {
    handle: HANDLE,
    tid: AtomicI32,
    ret: AtomicUsize,
    detached: AtomicU32,
    start: Option<StartFn>,
    arg: usize,
}

// SAFETY: the handle is only used for waiting and closing.
unsafe impl Send for GuestThread {}
// SAFETY: as above; the other fields are atomics or set before start.
unsafe impl Sync for GuestThread {}

thread_local! {
    static SELF: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static KEY_VALUES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static THREAD_DTORS: RefCell<Vec<(DtorFn, usize)>> = const { RefCell::new(Vec::new()) };
}

const MAX_KEYS: usize = 256;
static KEYS: Mutex<[(bool, usize); MAX_KEYS]> = Mutex::new([(false, 0); MAX_KEYS]);

/// Prepares the current Windows thread to run guest code: TLS area,
/// thread pointer, `pthread_self`.
pub fn enter_guest_thread(t: *mut GuestThread) -> ThreadArea {
    let area = ThreadArea::new();
    area.activate();
    SELF.with(|c| c.set(t as usize));
    // SAFETY: `t` is a live thread record.
    unsafe { (*t).tid.store(andruna_lx::process::gettid(), Ordering::Relaxed) };
    area
}

/// Runs the C++ `thread_local` destructors and the key destructors of the
/// current thread, as bionic does at thread exit.
pub fn run_thread_exit() {
    loop {
        let d = THREAD_DTORS.with(|v| v.borrow_mut().pop());
        let Some((f, obj)) = d else { break };
        f(obj as *mut c_void);
    }
    for _round in 0..4 {
        let mut any = false;
        for k in 0..MAX_KEYS {
            let (used, dtor) = KEYS.lock().unwrap()[k];
            if !used || dtor == 0 {
                continue;
            }
            let v = KEY_VALUES.with(|v| v.borrow().get(k).copied().unwrap_or(0));
            if v != 0 {
                KEY_VALUES.with(|vals| vals.borrow_mut()[k] = 0);
                // SAFETY: a destructor the guest registered with
                // pthread_key_create.
                let f: DtorFn = unsafe { core::mem::transmute(dtor) };
                f(v as *mut c_void);
                any = true;
            }
        }
        if !any {
            break;
        }
    }
}

/// A record for a thread that already exists (the guest main thread).
pub fn new_record() -> *mut GuestThread {
    Box::into_raw(Box::new(GuestThread {
        handle: HANDLE::default(),
        tid: AtomicI32::new(0),
        ret: AtomicUsize::new(0),
        detached: AtomicU32::new(1),
        start: None,
        arg: 0,
    }))
}

/// Runs `f` on a new Windows thread with a committed guest stack, and
/// waits for it.
pub fn run_on_guest_stack<F: FnOnce() + Send>(f: F) {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(GUEST_STACK)
            .spawn_scoped(s, || {
                commit_stack();
                f()
            })
            .expect("spawn")
            .join()
            .expect("guest thread panicked");
    });
}

/// Commits the current thread's stack from here down to `GUEST_STACK - 64
/// KiB` by touching each page in order (the Windows guard page moves down).
#[inline(never)]
fn commit_stack() {
    let mut probe = 0u8;
    let top = &mut probe as *mut u8 as usize;
    let mut a = top & !0xfff;
    let low = top - (GUEST_STACK - (256 << 10));
    while a > low {
        // SAFETY: an address inside this thread's reserved stack, above the
        // last 256 KiB; touched in descending order so each access hits the
        // guard page.
        unsafe { core::ptr::read_volatile(a as *const u8) };
        a -= 4096;
    }
}

unsafe extern "system" fn thread_main(p: *mut c_void) -> u32 {
    let t = p as *mut GuestThread;
    commit_stack();
    let area = enter_guest_thread(t);
    // SAFETY: the record outlives the thread (joined or detached).
    let (start, arg) = unsafe { ((*t).start.unwrap(), (*t).arg) };
    let r = start(arg as *mut c_void);
    // SAFETY: as above.
    unsafe { (*t).ret.store(r as usize, Ordering::Release) };
    run_thread_exit();
    drop(area);
    0
}

gfn! {
    pub unsafe fn h_pthread_create(out: *mut usize, attr: *const u8, start: StartFn, arg: *mut c_void) -> i32 {
        guard(|| {
            // pthread_attr_t: flags at 0 (bit 0 = detached).
            // SAFETY: NULL or the guest's attribute object.
            let detached = !attr.is_null() && unsafe { *(attr as *const u32) } & 1 != 0;
            let t = Box::into_raw(Box::new(GuestThread {
                handle: HANDLE::default(),
                tid: AtomicI32::new(0),
                ret: AtomicUsize::new(0),
                detached: AtomicU32::new(detached as u32),
                start: Some(start),
                arg: arg as usize,
            }));
            // SAFETY: valid start routine and parameter; the stack size is
            // committed (no STACK_SIZE_PARAM_IS_A_RESERVATION).
            let h = unsafe {
                CreateThread(None, GUEST_STACK, Some(thread_main), Some(t as *const c_void), THREAD_CREATION_FLAGS(0), None)
            };
            match h {
                Ok(h) => {
                    // SAFETY: the record is not freed while the thread runs.
                    unsafe { (*t).handle = h };
                    // SAFETY: the guest's out parameter.
                    unsafe { *out = t as usize };
                    0
                }
                Err(_) => EAGAIN,
            }
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_join(t: usize, ret: *mut *mut c_void) -> i32 {
        guard(|| {
            let t = t as *mut GuestThread;
            // SAFETY: a pthread_t from pthread_create.
            let h = unsafe { (*t).handle };
            if h.is_invalid() {
                return EINVAL;
            }
            // SAFETY: a live thread handle.
            if unsafe { WaitForSingleObject(h, INFINITE) } != WAIT_OBJECT_0 {
                return EINVAL;
            }
            if !ret.is_null() {
                // SAFETY: the guest's out parameter.
                unsafe { *ret = (*t).ret.load(Ordering::Acquire) as *mut c_void };
            }
            // SAFETY: the thread has exited; the record and handle are ours.
            unsafe {
                let _ = CloseHandle(h);
                drop(Box::from_raw(t));
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_detach(t: usize) -> i32 {
        // SAFETY: a pthread_t; the record is leaked once detached (spike).
        guard(|| unsafe {
            (*(t as *mut GuestThread)).detached.store(1, Ordering::Relaxed);
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_self() -> usize {
        guard(|| SELF.with(|c| c.get()))
    }
}

gfn! {
    pub unsafe fn h_pthread_equal(a: usize, b: usize) -> i32 {
        guard(|| (a == b) as i32)
    }
}

gfn! {
    pub unsafe fn h_pthread_gettid_np(t: usize) -> i32 {
        // SAFETY: a pthread_t.
        guard(|| unsafe { (*(t as *const GuestThread)).tid.load(Ordering::Relaxed) })
    }
}

// --- mutex -----------------------------------------------------------------
//
// Word 0 (atomic): bits 0-1 lock state (0 free, 1 locked, 2 contended),
// bits 14-15 the type, as bionic's static initializers set it. Word 1: the
// owner tid; word 2: the recursion count.

const TYPE_SHIFT: u32 = 14;
const NORMAL: u32 = 0;
const RECURSIVE: u32 = 1;
const ERRORCHECK: u32 = 2;

fn mutex_word<'a>(m: *mut u32) -> &'a AtomicU32 {
    // SAFETY: the guest's pthread_mutex_t, at least 40 bytes, 4-aligned.
    unsafe { AtomicU32::from_ptr(m) }
}

fn mtype(w: u32) -> u32 {
    (w >> TYPE_SHIFT) & 3
}

fn tid() -> u32 {
    andruna_lx::process::gettid() as u32
}

fn lock_word(word: &AtomicU32, deadline: Option<&Timespec>) -> i32 {
    let mut w = word.load(Ordering::Relaxed);
    loop {
        let ty = w & !3;
        if w & 3 == 0 {
            match word.compare_exchange_weak(w, ty | 1, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => return 0,
                Err(x) => {
                    w = x;
                    continue;
                }
            }
        }
        if w & 3 == 1 && word.compare_exchange(w, ty | 2, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            w = word.load(Ordering::Relaxed);
            continue;
        }
        let d = match deadline {
            Some(ts) => match Deadline::absolute(ts, true) {
                Ok(d) => Some(d),
                Err(e) => return e.0,
            },
            None => None,
        };
        match andruna_lx::futex::wait(word, ty | 2, d) {
            Err(e) if e.0 == ETIMEDOUT => return ETIMEDOUT,
            _ => {}
        }
        w = word.load(Ordering::Relaxed);
        // After a wait, take the lock as contended so the unlock wakes the
        // next waiter.
        if w & 3 == 0 {
            if word.compare_exchange(w, ty | 2, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            w = word.load(Ordering::Relaxed);
        }
    }
}

unsafe fn mutex_lock(m: *mut u32, deadline: Option<&Timespec>, try_only: bool) -> i32 {
    let word = mutex_word(m);
    let ty = mtype(word.load(Ordering::Relaxed));
    // SAFETY: words 1 and 2 of the 40-byte object.
    let (owner, count) = unsafe { (AtomicU32::from_ptr(m.add(1)), &mut *m.add(2)) };
    if ty != NORMAL && owner.load(Ordering::Relaxed) == tid() {
        if ty == ERRORCHECK {
            return EDEADLK;
        }
        *count += 1;
        return 0;
    }
    let r = if try_only {
        let w = word.load(Ordering::Relaxed);
        if w & 3 != 0 || word.compare_exchange(w, (w & !3) | 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            EBUSY
        } else {
            0
        }
    } else {
        lock_word(word, deadline)
    };
    if r == 0 && ty != NORMAL {
        owner.store(tid(), Ordering::Relaxed);
        *count = 1;
    }
    r
}

unsafe fn mutex_unlock(m: *mut u32) -> i32 {
    let word = mutex_word(m);
    let ty = mtype(word.load(Ordering::Relaxed));
    if ty != NORMAL {
        // SAFETY: words 1 and 2.
        let (owner, count) = unsafe { (AtomicU32::from_ptr(m.add(1)), &mut *m.add(2)) };
        if owner.load(Ordering::Relaxed) != tid() {
            return EPERM;
        }
        *count -= 1;
        if *count > 0 {
            return 0;
        }
        owner.store(0, Ordering::Relaxed);
    }
    let old = word.fetch_and(!3, Ordering::Release);
    if old & 3 == 2 {
        andruna_lx::futex::wake(m as usize, 1);
    }
    0
}

gfn! {
    pub unsafe fn h_pthread_mutex_init(m: *mut u32, attr: *const u32) -> i32 {
        guard(|| {
            // SAFETY: the guest's mutex (40 bytes) and attribute (an int,
            // whose low bits are the type).
            unsafe {
                core::ptr::write_bytes(m as *mut u8, 0, 40);
                let ty = if attr.is_null() { 0 } else { *attr & 3 };
                *m = ty << TYPE_SHIFT;
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutex_destroy(_m: *mut u32) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_pthread_mutex_lock(m: *mut u32) -> i32 {
        // SAFETY: the guest's mutex.
        guard(|| unsafe { mutex_lock(m, None, false) })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutex_timedlock(m: *mut u32, ts: *const Timespec) -> i32 {
        // SAFETY: the guest's mutex and timespec.
        guard(|| unsafe { mutex_lock(m, Some(&*ts), false) })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutex_trylock(m: *mut u32) -> i32 {
        // SAFETY: the guest's mutex.
        guard(|| unsafe { mutex_lock(m, None, true) })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutex_unlock(m: *mut u32) -> i32 {
        // SAFETY: the guest's mutex.
        guard(|| unsafe { mutex_unlock(m) })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutexattr_init(a: *mut u32) -> i32 {
        // SAFETY: the guest's attribute.
        guard(|| unsafe { *a = NORMAL; 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_mutexattr_destroy(_a: *mut u32) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_pthread_mutexattr_settype(a: *mut u32, ty: i32) -> i32 {
        guard(|| {
            if !(0..=2).contains(&ty) {
                return EINVAL;
            }
            // SAFETY: the guest's attribute.
            unsafe { *a = (*a & !3) | ty as u32 };
            0
        })
    }
}

// --- condition variables -----------------------------------------------------
//
// Word 0: a sequence number; word 1: the clock (0 realtime, 1 monotonic).

gfn! {
    pub unsafe fn h_pthread_cond_init(c: *mut u32, attr: *const u32) -> i32 {
        guard(|| {
            // SAFETY: the guest's 48-byte condition variable and attribute.
            unsafe {
                core::ptr::write_bytes(c as *mut u8, 0, 48);
                if !attr.is_null() && (*attr & 1) != 0 {
                    *c.add(1) = 1;
                }
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_cond_destroy(_c: *mut u32) -> i32 {
        guard(|| 0)
    }
}

unsafe fn cond_wait(c: *mut u32, m: *mut u32, ts: Option<&Timespec>) -> i32 {
    // SAFETY: the guest's condition variable.
    let seq = unsafe { AtomicU32::from_ptr(c) };
    // SAFETY: as above.
    let realtime = unsafe { *c.add(1) } == 0;
    let s = seq.load(Ordering::Relaxed);
    // SAFETY: the guest's mutex, locked by the caller.
    unsafe { mutex_unlock(m) };
    let mut r = 0;
    let d = match ts {
        Some(ts) => match Deadline::absolute(ts, realtime) {
            Ok(d) => Some(d),
            Err(e) => {
                r = e.0;
                None
            }
        },
        None => None,
    };
    if r == 0 {
        if let Err(e) = andruna_lx::futex::wait(seq, s, d) {
            if e.0 == ETIMEDOUT {
                r = ETIMEDOUT;
            }
        }
    }
    // SAFETY: as above.
    unsafe { mutex_lock(m, None, false) };
    r
}

gfn! {
    pub unsafe fn h_pthread_cond_wait(c: *mut u32, m: *mut u32) -> i32 {
        // SAFETY: guest objects.
        guard(|| unsafe { cond_wait(c, m, None) })
    }
}

gfn! {
    pub unsafe fn h_pthread_cond_timedwait(c: *mut u32, m: *mut u32, ts: *const Timespec) -> i32 {
        // SAFETY: guest objects.
        guard(|| unsafe { cond_wait(c, m, Some(&*ts)) })
    }
}

gfn! {
    pub unsafe fn h_pthread_cond_signal(c: *mut u32) -> i32 {
        guard(|| {
            // SAFETY: the guest's condition variable.
            unsafe { AtomicU32::from_ptr(c) }.fetch_add(1, Ordering::Release);
            andruna_lx::futex::wake(c as usize, 1);
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_cond_broadcast(c: *mut u32) -> i32 {
        guard(|| {
            // SAFETY: the guest's condition variable.
            unsafe { AtomicU32::from_ptr(c) }.fetch_add(1, Ordering::Release);
            andruna_lx::futex::wake(c as usize, i32::MAX);
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_condattr_init(a: *mut u32) -> i32 {
        // SAFETY: the guest's attribute.
        guard(|| unsafe { *a = 0; 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_condattr_setclock(a: *mut u32, clk: i32) -> i32 {
        // SAFETY: the guest's attribute.
        guard(|| unsafe { *a = (clk == 1) as u32; 0 })
    }
}

// --- rwlock: an SRWLOCK in the first 8 bytes, the writer tid at 8 -------------

fn srw<'a>(l: *mut u8) -> &'a mut SRWLOCK {
    // SAFETY: the guest's 56-byte rwlock; SRWLOCK is pointer-sized and its
    // initial value is 0, like PTHREAD_RWLOCK_INITIALIZER.
    unsafe { &mut *(l as *mut SRWLOCK) }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_init(l: *mut u8, _a: *const c_void) -> i32 {
        // SAFETY: the guest's rwlock.
        guard(|| unsafe { core::ptr::write_bytes(l, 0, 56); 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_destroy(_l: *mut u8) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_rdlock(l: *mut u8) -> i32 {
        // SAFETY: a valid SRWLOCK.
        guard(|| unsafe { AcquireSRWLockShared(srw(l)); 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_wrlock(l: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: a valid SRWLOCK.
            unsafe { AcquireSRWLockExclusive(srw(l)) };
            // SAFETY: the writer field at offset 8.
            unsafe { AtomicU32::from_ptr(l.add(8) as *mut u32) }.store(tid(), Ordering::Relaxed);
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_tryrdlock(l: *mut u8) -> i32 {
        // SAFETY: a valid SRWLOCK.
        guard(|| if unsafe { TryAcquireSRWLockShared(srw(l)) } { 0 } else { EBUSY })
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_trywrlock(l: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: a valid SRWLOCK.
            if !unsafe { TryAcquireSRWLockExclusive(srw(l)) } {
                return EBUSY;
            }
            // SAFETY: the writer field.
            unsafe { AtomicU32::from_ptr(l.add(8) as *mut u32) }.store(tid(), Ordering::Relaxed);
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_rwlock_unlock(l: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: the writer field.
            let w = unsafe { AtomicU32::from_ptr(l.add(8) as *mut u32) };
            if w.load(Ordering::Relaxed) == tid() {
                w.store(0, Ordering::Relaxed);
                // SAFETY: held exclusively by this thread.
                unsafe { ReleaseSRWLockExclusive(srw(l)) };
            } else {
                // SAFETY: held shared by this thread.
                unsafe { ReleaseSRWLockShared(srw(l)) };
            }
            0
        })
    }
}

// --- once, keys -------------------------------------------------------------

gfn! {
    pub unsafe fn h_pthread_once(o: *mut u32, f: extern "C" fn()) -> i32 {
        guard(|| {
            // SAFETY: the guest's pthread_once_t (an int).
            let w = unsafe { AtomicU32::from_ptr(o) };
            loop {
                match w.compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire) {
                    Ok(_) => {
                        // SAFETY: the guest's init routine (no arguments, so
                        // the convention does not matter for the call).
                        let g: GuestVoidFn = unsafe { core::mem::transmute(f) };
                        g();
                        w.store(2, Ordering::Release);
                        andruna_lx::futex::wake(o as usize, i32::MAX);
                        return 0;
                    }
                    Err(2) => return 0,
                    Err(_) => {
                        let _ = andruna_lx::futex::wait(w, 1, None);
                    }
                }
            }
        })
    }
}

#[cfg(target_arch = "x86_64")]
type GuestVoidFn = extern "sysv64" fn();
#[cfg(not(target_arch = "x86_64"))]
type GuestVoidFn = extern "C" fn();

gfn! {
    pub unsafe fn h_pthread_key_create(k: *mut u32, dtor: usize) -> i32 {
        guard(|| {
            let mut keys = KEYS.lock().unwrap();
            for (i, e) in keys.iter_mut().enumerate() {
                if !e.0 {
                    *e = (true, dtor);
                    // SAFETY: the guest's out parameter.
                    unsafe { *k = i as u32 };
                    return 0;
                }
            }
            EAGAIN
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_key_delete(k: u32) -> i32 {
        guard(|| {
            if let Some(e) = KEYS.lock().unwrap().get_mut(k as usize) {
                *e = (false, 0);
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_getspecific(k: u32) -> usize {
        guard(|| KEY_VALUES.with(|v| v.borrow().get(k as usize).copied().unwrap_or(0)))
    }
}

gfn! {
    pub unsafe fn h_pthread_setspecific(k: u32, val: usize) -> i32 {
        guard(|| {
            KEY_VALUES.with(|v| {
                let mut v = v.borrow_mut();
                if v.len() <= k as usize {
                    v.resize(k as usize + 1, 0);
                }
                v[k as usize] = val;
            });
            0
        })
    }
}

gfn! {
    pub unsafe fn h_cxa_thread_atexit_impl(dtor: DtorFn, obj: *mut c_void, _dso: *mut c_void) -> i32 {
        guard(|| {
            THREAD_DTORS.with(|v| v.borrow_mut().push((dtor, obj as usize)));
            0
        })
    }
}

gfn! {
    pub unsafe fn h_libc_thread_exit() -> () {
        guard(run_thread_exit)
    }
}

gfn! {
    /// `__tls_get_addr(tls_index*)`; `ti` is `{module, offset}`.
    pub unsafe fn h_tls_get_addr(ti: *const [u64; 2]) -> u64 {
        guard(|| {
            // SAFETY: a GOT entry pair the loader filled (DTPMOD64, DTPOFF64).
            let [m, off] = unsafe { *ti };
            crate::tls::tls_get_addr(m, off)
        })
    }
}

// --- attributes (stored but mostly ignored) ----------------------------------

gfn! {
    pub unsafe fn h_pthread_attr_init(a: *mut u8) -> i32 {
        guard(|| {
            // SAFETY: the guest's 56-byte attribute: flags, stack base,
            // stack size, guard size.
            unsafe {
                core::ptr::write_bytes(a, 0, 56);
                *(a.add(16) as *mut usize) = GUEST_STACK;
                *(a.add(24) as *mut usize) = 4096;
            }
            0
        })
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_destroy(_a: *mut u8) -> i32 {
        guard(|| 0)
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_setdetachstate(a: *mut u8, s: i32) -> i32 {
        // SAFETY: the guest's attribute flags word.
        guard(|| unsafe { *(a as *mut u32) = (*(a as *mut u32) & !1) | (s == 1) as u32; 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_setstacksize(a: *mut u8, s: usize) -> i32 {
        // SAFETY: the guest's attribute.
        guard(|| unsafe { *(a.add(16) as *mut usize) = s; 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_attr_getstacksize(a: *const u8, s: *mut usize) -> i32 {
        // SAFETY: the guest's attribute and out parameter.
        guard(|| unsafe { *s = *(a.add(16) as *const usize); 0 })
    }
}

gfn! {
    pub unsafe fn h_pthread_setname_np(_t: usize, _n: *const u8) -> i32 {
        guard(|| 0)
    }
}

pub fn symbols() -> Vec<(&'static str, usize)> {
    vec![
        ("pthread_create", h_pthread_create as usize),
        ("pthread_join", h_pthread_join as usize),
        ("pthread_detach", h_pthread_detach as usize),
        ("pthread_self", h_pthread_self as usize),
        ("pthread_equal", h_pthread_equal as usize),
        ("pthread_gettid_np", h_pthread_gettid_np as usize),
        ("pthread_mutex_init", h_pthread_mutex_init as usize),
        ("pthread_mutex_destroy", h_pthread_mutex_destroy as usize),
        ("pthread_mutex_lock", h_pthread_mutex_lock as usize),
        ("pthread_mutex_timedlock", h_pthread_mutex_timedlock as usize),
        ("pthread_mutex_trylock", h_pthread_mutex_trylock as usize),
        ("pthread_mutex_unlock", h_pthread_mutex_unlock as usize),
        ("pthread_mutexattr_init", h_pthread_mutexattr_init as usize),
        ("pthread_mutexattr_destroy", h_pthread_mutexattr_destroy as usize),
        ("pthread_mutexattr_settype", h_pthread_mutexattr_settype as usize),
        ("pthread_cond_init", h_pthread_cond_init as usize),
        ("pthread_cond_destroy", h_pthread_cond_destroy as usize),
        ("pthread_cond_wait", h_pthread_cond_wait as usize),
        ("pthread_cond_timedwait", h_pthread_cond_timedwait as usize),
        ("pthread_cond_signal", h_pthread_cond_signal as usize),
        ("pthread_cond_broadcast", h_pthread_cond_broadcast as usize),
        ("pthread_condattr_init", h_pthread_condattr_init as usize),
        ("pthread_condattr_setclock", h_pthread_condattr_setclock as usize),
        ("pthread_rwlock_init", h_pthread_rwlock_init as usize),
        ("pthread_rwlock_destroy", h_pthread_rwlock_destroy as usize),
        ("pthread_rwlock_rdlock", h_pthread_rwlock_rdlock as usize),
        ("pthread_rwlock_wrlock", h_pthread_rwlock_wrlock as usize),
        ("pthread_rwlock_tryrdlock", h_pthread_rwlock_tryrdlock as usize),
        ("pthread_rwlock_trywrlock", h_pthread_rwlock_trywrlock as usize),
        ("pthread_rwlock_unlock", h_pthread_rwlock_unlock as usize),
        ("pthread_once", h_pthread_once as usize),
        ("pthread_key_create", h_pthread_key_create as usize),
        ("pthread_key_delete", h_pthread_key_delete as usize),
        ("pthread_getspecific", h_pthread_getspecific as usize),
        ("pthread_setspecific", h_pthread_setspecific as usize),
        ("pthread_attr_init", h_pthread_attr_init as usize),
        ("pthread_attr_destroy", h_pthread_attr_destroy as usize),
        ("pthread_attr_setdetachstate", h_pthread_attr_setdetachstate as usize),
        ("pthread_attr_setstacksize", h_pthread_attr_setstacksize as usize),
        ("pthread_attr_getstacksize", h_pthread_attr_getstacksize as usize),
        ("pthread_setname_np", h_pthread_setname_np as usize),
        ("__cxa_thread_atexit_impl", h_cxa_thread_atexit_impl as usize),
        ("__andruna_libc_thread_exit", h_libc_thread_exit as usize),
        ("__tls_get_addr", h_tls_get_addr as usize),
    ]
}

//! x64 probes: can user mode own a per-thread FS base on Windows 11?
//!
//! Every instruction that may fault runs under the probe protocol from
//! `veh.rs`: `lea r11, [rip + 2f]` precedes it and label `2:` follows it.

use std::arch::asm;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Wdk::System::Threading::{NtQueryInformationThread, ThreadDescriptorTableEntry};
use windows::Win32::Foundation::{HANDLE, NTSTATUS};
use windows::Win32::System::Diagnostics::Debug::{
    GetThreadContext, SetThreadContext, CONTEXT, CONTEXT_FULL_AMD64, CONTEXT_SEGMENTS_AMD64,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, QueueUserAPC, ResumeThread, Sleep, SleepEx, SuspendThread,
    SwitchToThread, TlsAlloc, TlsGetValue, TlsSetValue,
};

use crate::{bench, hogs, veh};

windows_link::link!("ntdll.dll" "system" fn NtSetLdtEntries(selector0: u32, entry0_low: u32, entry0_hi: u32, selector1: u32, entry1_low: u32, entry1_hi: u32) -> NTSTATUS);
windows_link::link!("ntdll.dll" "system" fn NtSetInformationProcess(process: HANDLE, class: u32, info: *const core::ffi::c_void, len: u32) -> NTSTATUS);
windows_link::link!("ntdll.dll" "system" fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> NTSTATUS);

/// Offset of `NT_TIB.Self` in the x64 TEB.
/// Offset of `TlsSlots[0]` in the x64 TEB.
const TEB_TLS_SLOTS: u64 = 0x1480;

// ---------------------------------------------------------------------------
// Raw instruction wrappers
// ---------------------------------------------------------------------------

fn teb() -> u64 {
    let v: u64;
    // SAFETY: gs:[0x30] is the TEB self pointer on Windows x64.
    unsafe {
        asm!("mov {}, qword ptr gs:[0x30]", out(reg) v, options(nostack, readonly, preserves_flags))
    };
    v
}

fn fs_selector() -> u16 {
    let v: u16;
    // SAFETY: reading a segment register has no side effects.
    unsafe { asm!("mov {0:x}, fs", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

fn gs_selector() -> u16 {
    let v: u16;
    // SAFETY: reading a segment register has no side effects.
    unsafe { asm!("mov {0:x}, gs", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

type Probe = Result<u64, u32>;

fn finish(v: u64) -> Probe {
    match veh::take_probe() {
        Some(code) => Err(code),
        None => Ok(v),
    }
}

/// `rdfsbase rax` under the probe handler.
fn probe_rdfsbase() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: the instruction either reads FS base or raises #UD, which the
    // armed handler turns into a jump to label 2.
    unsafe {
        asm!("xor eax, eax", "lea r11, [rip + 2f]", ".byte 0xf3, 0x48, 0x0f, 0xae, 0xc0", "2:",
             out("rax") v, out("r11") _, options(nostack))
    };
    finish(v)
}

/// `rdgsbase rax` under the probe handler.
fn probe_rdgsbase() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: as for `probe_rdfsbase`.
    unsafe {
        asm!("xor eax, eax", "lea r11, [rip + 2f]", ".byte 0xf3, 0x48, 0x0f, 0xae, 0xc8", "2:",
             out("rax") v, out("r11") _, options(nostack))
    };
    finish(v)
}

/// `wrfsbase rax` under the probe handler.
fn probe_wrfsbase(base: u64) -> Probe {
    veh::arm_probe();
    // SAFETY: Windows x64 code does not use FS, so changing its base only
    // affects code in this spike that reads FS explicitly. A fault is caught.
    unsafe {
        asm!("lea r11, [rip + 2f]", ".byte 0xf3, 0x48, 0x0f, 0xae, 0xd0", "2:",
             in("rax") base, out("r11") _, options(nostack))
    };
    finish(0)
}

/// Unchecked `rdfsbase`; only call after a successful probe.
#[inline(always)]
pub(crate) fn rdfsbase() -> u64 {
    let v: u64;
    // SAFETY: caller verified that the instruction is enabled.
    unsafe {
        asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xc0", out("rax") v, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Unchecked `wrfsbase`; only call after a successful probe.
#[inline(always)]
pub(crate) fn wrfsbase(base: u64) {
    // SAFETY: caller verified that the instruction is enabled; see `probe_wrfsbase`.
    unsafe {
        asm!(".byte 0xf3, 0x48, 0x0f, 0xae, 0xd0", in("rax") base, options(nomem, nostack, preserves_flags))
    };
}

/// `mov rax, fs:[off]` under the probe handler.
fn probe_read_fs(off: u64) -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: the load either succeeds or faults into the armed handler.
    unsafe {
        asm!("xor eax, eax", "lea r11, [rip + 2f]", "mov rax, qword ptr fs:[rcx]", "2:",
             in("rcx") off, out("rax") v, out("r11") _, options(nostack))
    };
    finish(v)
}

/// `mov fs, sel` under the probe handler.
fn probe_load_fs(sel: u16) -> Probe {
    veh::arm_probe();
    // SAFETY: loading FS either succeeds or raises #GP into the armed handler.
    unsafe {
        asm!("lea r11, [rip + 2f]", "mov fs, {0:x}", "2:", in(reg) sel, out("r11") _, options(nostack))
    };
    finish(0)
}

/// Faulting null read, used to raise an access violation on purpose.
fn probe_null_read() -> Probe {
    veh::arm_probe();
    let v: u64;
    // SAFETY: address 0 is never mapped; the fault is caught by the handler.
    unsafe {
        asm!("xor eax, eax", "lea r11, [rip + 2f]", "mov rax, qword ptr [r10]", "2:",
             in("r10") 0u64, out("rax") v, out("r11") _, options(nostack))
    };
    finish(v)
}

fn show(p: Probe) -> String {
    match p {
        Ok(v) => format!("ok 0x{v:016x}"),
        Err(c) => format!("fault 0x{c:08x} {}", veh::code_name(c)),
    }
}

fn status(s: NTSTATUS) -> String {
    format!("0x{:08x}", s.0 as u32)
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

pub fn probe() {
    veh::install();
    println!("== x64 FS/GS probe ==");
    // SAFETY: CPUID leaf 7 subleaf 0 exists on every x64 CPU Windows 11 runs on.
    let l7 = unsafe { std::arch::x86_64::__cpuid_count(7, 0) };
    println!(
        "cpuid.7.0.ebx[0] FSGSBASE supported by CPU: {}",
        l7.ebx & 1 != 0
    );
    println!("TEB (gs:[0x30])             : 0x{:016x}", teb());
    println!("FS selector                 : 0x{:04x}", fs_selector());
    println!("GS selector                 : 0x{:04x}", gs_selector());
    println!("rdgsbase                    : {}", show(probe_rdgsbase()));
    println!("rdfsbase                    : {}", show(probe_rdfsbase()));
    for off in [0u64, 0x28] {
        println!(
            "read fs:[0x{off:x}]               : {}",
            show(probe_read_fs(off))
        );
    }

    // Data selectors: what base does each give FS when loaded explicitly?
    let original = fs_selector();
    for sel in [0x2bu16, 0x53, 0x00, 0x07, original] {
        let r = probe_load_fs(sel);
        let now = fs_selector();
        let base = probe_rdfsbase();
        let read0 = probe_read_fs(0);
        println!(
            "mov fs,0x{sel:02x}: {} -> sel 0x{now:04x}, rdfsbase {}, fs:[0] {}",
            if r.is_ok() { "ok" } else { "fault" },
            show(base),
            show(read0)
        );
    }

    nt_probes();
    let wr_ok = wrfsbase_probe();
    set_context_probe();
    if wr_ok {
        survival();
        lazy_restore_test();
    } else {
        println!("WRFSBASE not usable: survival tests skipped");
    }
}

#[repr(C)]
#[derive(Default)]
struct DescriptorTableEntry {
    selector: u32,
    low: u32,
    high: u32,
}

fn descriptor_base(low: u32, high: u32) -> u32 {
    (low >> 16) | ((high & 0xff) << 16) | (high & 0xff00_0000)
}

/// Builds a present, ring-3, read/write, 4 GiB data descriptor with `base`.
fn data_descriptor(base: u32) -> (u32, u32) {
    let limit = 0xFFFFFu32;
    let low = (limit & 0xffff) | ((base & 0xffff) << 16);
    let high = ((base >> 16) & 0xff)
        | (0xF2 << 8)
        | (((limit >> 16) & 0xf) << 16)
        | (0xC << 20)
        | (base & 0xff00_0000);
    (low, high)
}

fn nt_probes() {
    println!("-- NT interfaces --");
    for sel in [0x53u32, 0x2b, 0x33, 0x50, 0x07] {
        let mut e = DescriptorTableEntry {
            selector: sel,
            ..Default::default()
        };
        let mut ret = 0u32;
        // SAFETY: the buffer is a valid, correctly sized DESCRIPTOR_TABLE_ENTRY.
        let s = unsafe {
            NtQueryInformationThread(
                GetCurrentThread(),
                ThreadDescriptorTableEntry,
                &mut e as *mut _ as *mut _,
                std::mem::size_of::<DescriptorTableEntry>() as u32,
                &mut ret,
            )
        };
        println!(
            "NtQueryInformationThread(ThreadDescriptorTableEntry, sel 0x{sel:02x}) = {} desc {:08x}:{:08x} base 0x{:08x}",
            status(s),
            e.high,
            e.low,
            descriptor_base(e.low, e.high)
        );
    }

    // A low buffer, so a 32-bit descriptor base could address it.
    let block = low_block();
    let (lo, hi) = data_descriptor(block as u32);
    // SAFETY: plain system call with scalar arguments.
    let s = unsafe { NtSetLdtEntries(0x07, lo, hi, 0, 0, 0) };
    println!(
        "NtSetLdtEntries(sel 0x07, base 0x{block:x}) = {}",
        status(s)
    );

    // PROCESS_LDT_INFORMATION { Start, Length, LdtEntries[1] }.
    let info: [u32; 4] = [0, 8, lo, hi];
    // SAFETY: the buffer matches PROCESS_LDT_INFORMATION with one entry.
    let s =
        unsafe { NtSetInformationProcess(GetCurrentProcess(), 10, info.as_ptr() as *const _, 16) };
    println!(
        "NtSetInformationProcess(ProcessLdtInformation) = {}",
        status(s)
    );
    let mut q: [u32; 4] = [0, 8, 0, 0];
    let mut ret = 0u32;
    // SAFETY: same buffer layout, used for output.
    let s = unsafe {
        NtQueryInformationProcess(
            GetCurrentProcess(),
            10,
            q.as_mut_ptr() as *mut _,
            16,
            &mut ret,
        )
    };
    println!(
        "NtQueryInformationProcess(ProcessLdtInformation) = {}",
        status(s)
    );
    let size: u32 = 8;
    // SAFETY: ProcessLdtSize takes a 32-bit length.
    let s = unsafe {
        NtSetInformationProcess(GetCurrentProcess(), 11, &size as *const u32 as *const _, 4)
    };
    println!("NtSetInformationProcess(ProcessLdtSize) = {}", status(s));

    let r = probe_load_fs(0x07);
    println!(
        "mov fs,0x07 after LDT attempts: {}",
        if r.is_ok() { "ok" } else { "fault" }
    );
    let _ = probe_load_fs(0x53);
}

/// Returns a page-aligned buffer below 4 GiB whose first qword is its own address.
fn low_block() -> u64 {
    use windows::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
    for addr in (0x1000_0000u64..0x8000_0000).step_by(0x100_0000) {
        // SAFETY: requesting a fresh private mapping at a hint address.
        let p = unsafe {
            VirtualAlloc(
                Some(addr as *const _),
                0x1000,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if !p.is_null() {
            // SAFETY: the page was just committed read/write.
            unsafe { *(p as *mut u64) = p as u64 };
            return p as u64;
        }
    }
    0
}

/// A heap block standing in for a bionic TLS slot array: slot 0 points to itself.
fn tls_block(tag: u64) -> u64 {
    let b: &'static mut [u64; 16] = Box::leak(Box::new([0u64; 16]));
    b[0] = b.as_ptr() as u64;
    b[5] = 0x5EED_0000_0000_0000 | tag; // stack guard slot
    b.as_ptr() as u64
}

fn wrfsbase_probe() -> bool {
    println!("-- WRFSBASE --");
    let block = tls_block(1);
    let before = probe_rdfsbase();
    let w = probe_wrfsbase(block);
    println!("wrfsbase(0x{block:x}) : {}", show(w));
    if w.is_err() {
        return false;
    }
    let after = probe_rdfsbase();
    let r0 = probe_read_fs(0);
    let r28 = probe_read_fs(0x28);
    println!("rdfsbase after         : {}", show(after));
    println!("fs:[0], fs:[0x28]      : {} / {}", show(r0), show(r28));
    println!("FS selector after write: 0x{:04x}", fs_selector());
    if let Ok(b) = before {
        let _ = probe_wrfsbase(b);
    }
    matches!(r0, Ok(v) if v == block)
}

/// Does SetThreadContext with a different SegFs change the FS base the thread sees?
fn set_context_probe() {
    println!("-- SetThreadContext(SegFs) --");
    let stop = Arc::new(AtomicBool::new(false));
    let seen_sel = Arc::new(AtomicU64::new(0));
    let seen_base = Arc::new(AtomicU64::new(u64::MAX));
    let (s2, sel2, base2) = (stop.clone(), seen_sel.clone(), seen_base.clone());
    let t = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            sel2.store(fs_selector() as u64, Ordering::Relaxed);
            base2.store(probe_rdfsbase().unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    });
    std::thread::sleep(Duration::from_millis(50));
    let h = HANDLE(t.as_raw_handle());
    for sel in [0x2bu32, 0x00, 0x53] {
        let mut ctx = Context::new(CONTEXT_FULL_AMD64.0 | CONTEXT_SEGMENTS_AMD64.0);
        // SAFETY: valid thread handle; the thread is suspended while its context is edited.
        let (g, s) = unsafe {
            SuspendThread(h);
            let g = GetThreadContext(h, ctx.as_mut());
            let before = ctx.0.SegFs;
            ctx.0.SegFs = sel as u16;
            let s = SetThreadContext(h, ctx.as_mut());
            ResumeThread(h);
            (g.map(|_| before), s)
        };
        std::thread::sleep(Duration::from_millis(30));
        println!(
            "SegFs {:?} -> 0x{sel:02x}: set {:?}; thread now sees sel 0x{:04x} rdfsbase 0x{:x}",
            g.map(|b| format!("0x{b:02x}")),
            s.is_ok(),
            seen_sel.load(Ordering::Relaxed),
            seen_base.load(Ordering::Relaxed)
        );
    }
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
}

/// CONTEXT must be 16-byte aligned; the windows crate type carries that alignment.
struct Context(Box<CONTEXT>);
impl Context {
    fn new(flags: u32) -> Self {
        // SAFETY: CONTEXT is plain data; all-zero is a valid value.
        let mut c: Box<CONTEXT> = Box::new(unsafe { std::mem::zeroed() });
        c.ContextFlags.0 = flags;
        Context(c)
    }
    fn as_mut(&mut self) -> *mut CONTEXT {
        &mut *self.0
    }
}

// ---------------------------------------------------------------------------
// Survival of a user-written FS base
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct Tally {
    checks: u64,
    /// Base found reset to zero.
    lost: u64,
    /// Base found holding a value that is neither ours nor zero.
    foreign: u64,
    last_bad: u64,
}

impl Tally {
    #[inline(always)]
    fn check(&mut self, want: u64) {
        self.checks += 1;
        let got = rdfsbase();
        if got != want {
            if got == 0 {
                self.lost += 1;
            } else {
                self.foreign += 1;
            }
            self.last_bad = got;
            wrfsbase(want);
        }
    }
    fn line(&self, name: &str) {
        println!(
            "{name:<46}: {} checks, {} reset to 0, {} foreign (last seen 0x{:x})",
            self.checks, self.lost, self.foreign, self.last_bad
        );
    }
}

unsafe extern "system" fn apc(_: usize) {}

fn survival() {
    println!("-- survival of a WRFSBASE value, idle machine --");
    let me = tls_block(7);
    wrfsbase(me);
    let mut t = Tally::default();
    for _ in 0..3000 {
        // SAFETY: plain yield.
        unsafe { Sleep(0) };
        t.check(me);
    }
    t.line("Sleep(0) x3000, no load");
    let mut t = Tally::default();
    let dir = std::env::temp_dir();
    for _ in 0..3000 {
        let _ = std::fs::metadata(&dir);
        t.check(me);
    }
    t.line("file system calls x3000, no load");
    let mut t = Tally::default();
    for _ in 0..300 {
        // SAFETY: plain wait.
        unsafe { Sleep(1) };
        t.check(me);
    }
    t.line("Sleep(1) x300, no load");

    println!("-- survival of a WRFSBASE value (CPU hogs on every core) --");
    let _h = hogs::Hogs::start(2);

    let mut t = Tally::default();
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        for _ in 0..1000 {
            t.check(me);
        }
    }
    t.line("preemption (2 s busy loop)");

    let mut t = Tally::default();
    for i in 0..3000 {
        match i % 3 {
            // SAFETY: plain waits.
            0 => unsafe { Sleep(0) },
            1 => unsafe { Sleep(1) },
            _ => unsafe {
                let _ = SwitchToThread();
            },
        }
        t.check(me);
    }
    t.line("Sleep(0) / Sleep(1) / SwitchToThread x3000");

    let mut t = Tally::default();
    for _ in 0..3000 {
        assert!(probe_null_read().is_err());
        t.check(me);
    }
    t.line("access violation handled by VEH x3000");

    let mut t = Tally::default();
    for _ in 0..3000 {
        // SAFETY: queues a no-op APC to the current thread and drains it.
        unsafe {
            QueueUserAPC(Some(apc), GetCurrentThread(), 0);
            SleepEx(0, true);
        }
        t.check(me);
    }
    t.line("user APC delivery x3000");

    let mut t = Tally::default();
    let dir = std::env::temp_dir();
    for i in 0..2000 {
        let _ = std::fs::metadata(&dir);
        let v = vec![i as u8; 1 << 16];
        std::hint::black_box(&v);
        t.check(me);
    }
    t.line("file system calls + heap x2000");

    // Another thread suspends us, reads and writes our full context.
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicU64::new(0));
    let checks = Arc::new(AtomicU64::new(0));
    let (s2, l2, c2) = (stop.clone(), lost.clone(), checks.clone());
    let worker = std::thread::spawn(move || {
        let mine = tls_block(9);
        wrfsbase(mine);
        let mut t = Tally::default();
        while !s2.load(Ordering::Relaxed) {
            t.check(mine);
        }
        l2.store(t.lost + t.foreign, Ordering::Relaxed);
        c2.store(t.checks, Ordering::Relaxed);
    });
    let h = HANDLE(worker.as_raw_handle());
    for _ in 0..2000 {
        let mut ctx = Context::new(CONTEXT_FULL_AMD64.0 | CONTEXT_SEGMENTS_AMD64.0);
        // SAFETY: valid handle; context is read and written back while suspended.
        unsafe {
            SuspendThread(h);
            GetThreadContext(h, ctx.as_mut()).unwrap();
            SetThreadContext(h, ctx.as_mut()).unwrap();
            ResumeThread(h);
        }
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    println!(
        "{:<44}: {} checks, {} lost",
        "Suspend+Get/SetThreadContext x2000 (other thr)",
        checks.load(Ordering::Relaxed),
        lost.load(Ordering::Relaxed)
    );

    // Independence: 16 threads, each with its own base.
    let ths: Vec<_> = (0..16u64)
        .map(|i| {
            std::thread::spawn(move || {
                let mine = tls_block(100 + i);
                wrfsbase(mine);
                let mut t = Tally::default();
                let end = Instant::now() + Duration::from_secs(1);
                let mut n = 0u32;
                while Instant::now() < end {
                    t.check(mine);
                    n = n.wrapping_add(1);
                    if n.is_multiple_of(4096) {
                        // SAFETY: yield to force switches between FS-owning threads.
                        unsafe { Sleep(0) };
                    }
                }
                t
            })
        })
        .collect();
    let (mut checks, mut lost, mut foreign) = (0, 0, 0);
    for t in ths {
        let tally = t.join().unwrap();
        checks += tally.checks;
        lost += tally.lost;
        foreign += tally.foreign;
    }
    println!(
        "{:<46}: {checks} checks, {lost} reset to 0, {foreign} foreign",
        "16 threads, own base each, 1 s"
    );
    println!("FS selector at end: 0x{:04x}", fs_selector());
}

// ---------------------------------------------------------------------------
// Lazy restore: the VEH rewrites FS base on the first FS fault after a switch
// ---------------------------------------------------------------------------

/// Plain `mov rax, fs:[0x28]`, as bionic's stack protector emits it.
#[inline(always)]
fn read_fs28() -> u64 {
    let v: u64;
    // SAFETY: with lazy restore enabled, a fault here is resolved by the VEH,
    // which rewrites FS base and re-executes this instruction.
    unsafe {
        asm!("mov rax, qword ptr fs:[0x28]", out("rax") v, options(nostack, readonly, preserves_flags))
    };
    v
}

fn lazy_restore_test() {
    // Variant II ELF TLS (x86_64) uses negative offsets: with FS base 0 those
    // addresses are non-canonical and raise #GP, which must restore too.
    let words: &'static mut [u64; 8] = Box::leak(Box::new([0x77u64; 8]));
    let tp = &words[4] as *const u64 as u64;
    veh::lazy_fs_enable(tp);
    wrfsbase(0);
    let v: u64;
    // SAFETY: fs:[-16] faults while FS base is 0; the VEH restores `tp` and
    // the retried load reads words[2].
    unsafe {
        asm!("mov rax, qword ptr fs:[rcx]", in("rcx") -16i64, out("rax") v, options(nostack, readonly))
    };
    println!(
        "lazy restore, fs:[-16] with FS base 0: value 0x{v:x} (want 0x77), restores {}",
        veh::lazy_restores_here()
    );
    veh::lazy_fs_disable();

    println!("-- lazy restore under load: 16 threads, 2 s, CPU hogs, Sleep(0) every 8000 reads --");
    let _h = hogs::Hogs::start(2);
    let ths: Vec<_> = (0..16u64)
        .map(|i| {
            std::thread::spawn(move || {
                let mine = tls_block(200 + i);
                let want = 0x5EED_0000_0000_0000 | (200 + i);
                veh::lazy_fs_enable(mine);
                let (mut reads, mut bad) = (0u64, 0u64);
                let end = Instant::now() + Duration::from_secs(2);
                let mut n = 0u32;
                while Instant::now() < end {
                    for _ in 0..1000 {
                        reads += 1;
                        if read_fs28() != want {
                            bad += 1;
                        }
                    }
                    n += 1;
                    if n.is_multiple_of(8) {
                        // SAFETY: plain yield.
                        unsafe { Sleep(0) };
                    }
                }
                (reads, bad, veh::lazy_restores_here())
            })
        })
        .collect();
    let (mut reads, mut bad, mut restores) = (0, 0, 0);
    for t in ths {
        let (r, b, x) = t.join().unwrap();
        reads += r;
        bad += b;
        restores += x;
    }
    println!(
        "reads {reads}, wrong values {bad}, lazy restores {restores} ({:.0} per thread per second)",
        restores as f64 / 32.0
    );
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

#[inline(never)]
fn rust_tls_read() -> u64 {
    thread_local! { static V: std::cell::Cell<u64> = const { std::cell::Cell::new(3) }; }
    V.with(|v| v.get())
}

pub fn benches() {
    veh::install();
    println!("== x64 TLS access cost ==");
    let n: u64 = 200_000_000;

    let empty = bench::run("empty loop", n, |n| {
        // SAFETY: register-only loop.
        unsafe { asm!("2:", "dec rcx", "jnz 2b", inout("rcx") n => _, options(nomem, nostack)) };
    });

    // TEB TLS slot holding a thread pointer, and a slot array behind it.
    // SAFETY: TlsAlloc/TlsSetValue on the current thread.
    let idx = unsafe { TlsAlloc() };
    assert!(idx < 64, "expected a TEB-resident TLS slot");
    let block = tls_block(3);
    // SAFETY: idx was just allocated.
    unsafe { TlsSetValue(idx, Some(block as *const _)).unwrap() };
    let slot_off = TEB_TLS_SLOTS + 8 * idx as u64;

    let r = bench::run("gs:[TlsSlots+8*i] (in-place patch of fs:disp32)", n, |n| {
        // SAFETY: gs-relative read of the current TEB's TLS slot.
        unsafe {
            asm!("2:", "mov rax, qword ptr gs:[rdx]", "add r8, rax", "dec rcx", "jnz 2b",
                 inout("rcx") n => _, in("rdx") slot_off, out("rax") _, out("r8") _, options(nostack, readonly))
        };
    });
    bench::report(&r, &empty);

    let r = bench::run("gs:[slot] -> [tp+0x28] (two dependent loads)", n, |n| {
        // SAFETY: reads the slot array that the TLS slot points to.
        unsafe {
            asm!("2:", "mov rax, qword ptr gs:[rdx]", "mov rax, qword ptr [rax + 0x28]", "add r8, rax", "dec rcx", "jnz 2b",
                 inout("rcx") n => _, in("rdx") slot_off, out("rax") _, out("r8") _, options(nostack, readonly))
        };
    });
    bench::report(&r, &empty);

    let r = bench::run(
        "call stub {gs:[slot]; [tp+0x28]; ret} (5-byte patch)",
        n,
        |n| {
            // SAFETY: the stub is local to this asm block and uses only rax.
            unsafe {
                asm!("2:", "call 3f", "add r8, rax", "dec rcx", "jnz 2b", "jmp 4f",
                 "3:", "mov rax, qword ptr gs:[rdx]", "mov rax, qword ptr [rax + 0x28]", "ret", "4:",
                 inout("rcx") n => _, in("rdx") slot_off, out("rax") _, out("r8") _)
            };
        },
    );
    bench::report(&r, &empty);

    let r = bench::run("TlsGetValue()", n / 4, |n| {
        for _ in 0..n {
            // SAFETY: idx is a valid TLS index.
            std::hint::black_box(unsafe { TlsGetValue(idx) });
        }
    });
    bench::report(&r, &empty);

    let r = bench::run("Rust thread_local! (non-inlined fn)", n / 4, |n| {
        for _ in 0..n {
            std::hint::black_box(rust_tls_read());
        }
    });
    bench::report(&r, &empty);

    let have_wr = probe_wrfsbase(block).is_ok();
    if have_wr {
        // A context switch during the loop resets FS base; lazy restore repairs it.
        veh::lazy_fs_enable(block);
        let r = bench::run("fs:[0x28] with FS base set by WRFSBASE", n, |n| {
            // SAFETY: FS base points at `block` (wrfsbase above).
            unsafe {
                asm!("2:", "mov rax, qword ptr fs:[0x28]", "add r8, rax", "dec rcx", "jnz 2b",
                     inout("rcx") n => _, out("rax") _, out("r8") _, options(nostack, readonly))
            };
        });
        bench::report(&r, &empty);
        let r = bench::run("wrfsbase (switch cost at a thunk)", n / 4, |n| {
            // SAFETY: rewrites the same base each time.
            unsafe {
                asm!("2:", ".byte 0xf3, 0x48, 0x0f, 0xae, 0xd0", "dec rcx", "jnz 2b",
                     inout("rcx") n => _, in("rax") block, options(nomem, nostack))
            };
        });
        bench::report(&r, &empty);
        let r = bench::run("rdfsbase", n / 4, |n| {
            // SAFETY: reads FS base.
            unsafe {
                asm!("2:", ".byte 0xf3, 0x48, 0x0f, 0xae, 0xc0", "dec rcx", "jnz 2b",
                     inout("rcx") n => _, out("rax") _, options(nomem, nostack))
            };
        });
        bench::report(&r, &empty);

        let before = veh::lazy_restores_here();
        let m = 200_000u64;
        let r = bench::run(
            "lazy restore: FS reset + fault + VEH wrfsbase + retry",
            m,
            |n| {
                for _ in 0..n {
                    wrfsbase(0);
                    std::hint::black_box(read_fs28());
                }
            },
        );
        bench::report(&r, &empty);
        let restores = veh::lazy_restores_here() - before;
        println!("  (restores counted: {restores})");
        veh::lazy_fs_disable();
    } else {
        println!("(WRFSBASE unavailable: no FS-based measurements)");
    }

    // Trap and emulate: each iteration faults and the VEH supplies the value.
    let m = 200_000u64;
    veh::arm_emulate(0x1234);
    let r = bench::run("fault + VEH emulation per access", m, |n| {
        for _ in 0..n {
            let v: u64;
            // SAFETY: the null read faults and the armed handler emulates it.
            unsafe {
                asm!("lea r11, [rip + 2f]", "mov rax, qword ptr [r10]", "2:",
                     in("r10") 0u64, out("rax") v, out("r11") _, options(nostack))
            };
            std::hint::black_box(v);
        }
    });
    veh::disarm();
    bench::report(&r, &empty);
}

//! Probes of wake primitives across processes: WaitOnAddress and keyed
//! events. Keyed events are not exposed by the `windows` crate, so they are
//! declared here directly from ntdll (spike only).

use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, FILE_MAP_ALL_ACCESS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{WaitOnAddress, WakeByAddressAll, PROCESS_DUP_HANDLE};

use crate::win::{self, Raw};

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateKeyedEvent(h: *mut HANDLE, access: u32, oa: *const c_void, flags: u32) -> i32;
    fn NtWaitForKeyedEvent(
        h: HANDLE,
        key: *const c_void,
        alertable: u8,
        timeout: *const i64,
    ) -> i32;
    fn NtReleaseKeyedEvent(
        h: HANDLE,
        key: *const c_void,
        alertable: u8,
        timeout: *const i64,
    ) -> i32;
}

#[repr(C)]
struct Shared {
    stage: AtomicU32,
    word: AtomicU32,
    woa_ms: AtomicU64,
    keyed: AtomicU64,
    keyed_status: AtomicU32,
}

fn map(name: &str, create: bool) -> io::Result<&'static Shared> {
    let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: named 4 KiB section; the view is leaked for the process lifetime.
    unsafe {
        let h = if create {
            CreateFileMappingW(
                windows::Win32::Foundation::INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                4096,
                PCWSTR(w.as_ptr()),
            )
        } else {
            OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(w.as_ptr()))
        }
        .map_err(|e| io::Error::other(e.to_string()))?;
        let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, 4096);
        if v.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(&*(v.Value as *const Shared))
    }
}

fn wait_stage(s: &Shared, v: u32) {
    while s.stage.load(SeqCst) < v {
        std::thread::sleep(Duration::from_millis(1));
    }
}

const KEY: usize = 0x1000;

pub fn run_parent() -> io::Result<()> {
    let name = format!("Local\\andruna-s3-probe-{}", std::process::id());
    let s = map(&name, true)?;
    let exe = std::env::current_exe()?;
    let mut child = std::process::Command::new(exe)
        .args(["probe-child", &name])
        .spawn()?;
    let cproc = win::open_process(child.id(), PROCESS_DUP_HANDLE)?;

    // 1. WaitOnAddress across processes on the same shared page.
    wait_stage(s, 1);
    std::thread::sleep(Duration::from_millis(100));
    s.word.store(1, SeqCst);
    // SAFETY: address of a live atomic in our mapping.
    unsafe { WakeByAddressAll(&s.word as *const _ as *const c_void) };
    wait_stage(s, 2);
    let ms = s.woa_ms.load(SeqCst);
    println!(
        "WaitOnAddress cross-process: child woke after {ms} ms (wake sent at ~100 ms, timeout 1000 ms) -> {}",
        if ms >= 900 { "NOT woken: in-process only" } else { "woken" }
    );

    // 2. Keyed event handle shared with the child.
    let mut h = HANDLE::default();
    // SAFETY: valid out pointer; 0x3 = wait|wake access.
    let st = unsafe { NtCreateKeyedEvent(&mut h, 0x3, std::ptr::null(), 0) };
    if st != 0 {
        println!("NtCreateKeyedEvent failed: 0x{st:x}");
    } else {
        let ch = win::dup_into(win::current_process(), Raw::from(h), cproc.raw())?;
        s.keyed.store(ch.0 as u64, SeqCst);
        s.stage.store(3, SeqCst);
        wait_stage(s, 4); // child is about to wait
        std::thread::sleep(Duration::from_millis(100));
        let timeout: i64 = -10_000_000; // 1 s relative
        let t = Instant::now();
        // SAFETY: valid keyed event handle and timeout pointer.
        let rs = unsafe { NtReleaseKeyedEvent(h, KEY as *const c_void, 0, &timeout) };
        let el = t.elapsed().as_millis();
        wait_stage(s, 5);
        println!(
            "Keyed event cross-process: release status 0x{rs:x} after {el} ms, child wait status 0x{:x} -> {}",
            s.keyed_status.load(SeqCst),
            if rs == 0 { "matched a waiter in another process" } else { "no cross-process match (0x102 = timeout)" }
        );
        win::close(Raw::from(h));
    }
    s.stage.store(6, SeqCst);
    let _ = child.wait();
    Ok(())
}

pub fn run_child(name: &str) -> io::Result<()> {
    let s = map(name, false)?;
    let zero = 0u32;
    s.stage.store(1, SeqCst);
    let t = Instant::now();
    // SAFETY: address of a live atomic in the mapping; compare value on our stack.
    let _ = unsafe {
        WaitOnAddress(
            &s.word as *const _ as *const c_void,
            &zero as *const _ as *const c_void,
            4,
            Some(1000),
        )
    };
    s.woa_ms.store(t.elapsed().as_millis() as u64, SeqCst);
    s.stage.store(2, SeqCst);

    wait_stage(s, 3);
    let h = HANDLE(s.keyed.load(SeqCst) as usize as *mut c_void);
    s.stage.store(4, SeqCst);
    let timeout: i64 = -20_000_000; // 2 s relative
                                    // SAFETY: keyed event handle duplicated into us by the parent.
    let st = unsafe { NtWaitForKeyedEvent(h, KEY as *const c_void, 0, &timeout) };
    s.keyed_status.store(st as u32, SeqCst);
    s.stage.store(5, SeqCst);
    wait_stage(s, 6);
    Ok(())
}

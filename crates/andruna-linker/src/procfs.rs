//! `/proc/self` files the loader writes while andruna-lx has no `/proc`
//! (an `open` that lx fails is retried here; see [`crate::interim`]).
//!
//! - `stat`: the fields bionic and libbase read (pid, name, state, thread
//!   count, `startstack`, the address of the initial stack block, which is
//!   how bionic finds the main thread's stack).
//! - `maps`: the address space from `VirtualQuery`, one line per region
//!   with Linux permissions; the guest main thread's stack is `[stack]`.
//! - `cmdline`: the guest `argv`.
//!
//! Each open takes a snapshot, as reading a Linux `/proc` file does.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use andruna_lx::UserBuf;
use andruna_lx::errno::{Errno, SysResult};
use andruna_lx::fd::{FileOps, OpenFile};
use andruna_lx::linux::stat::{S_IFREG, StatData};

/// Address of the initial stack block (`startstack`), set at start-up.
pub static START_STACK: AtomicU64 = AtomicU64::new(0);
/// The guest `argv`, NUL-separated, set at start-up.
pub static CMDLINE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// A read-only file held in memory.
#[derive(Debug)]
pub struct MemFile {
    data: Vec<u8>,
    pos: Mutex<u64>,
}

impl MemFile {
    pub fn new(data: Vec<u8>) -> Self {
        MemFile {
            data,
            pos: Mutex::new(0),
        }
    }

    fn copy_out(&self, buf: &UserBuf, off: u64) -> usize {
        let off = (off as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - off);
        // SAFETY: the caller's buffer is writable for `buf.len() >= n`
        // bytes (UserBuf contract).
        unsafe { core::ptr::copy_nonoverlapping(self.data[off..].as_ptr(), buf.ptr(), n) };
        n
    }
}

impl FileOps for MemFile {
    fn read(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        let mut p = self.pos.lock().unwrap_or_else(|e| e.into_inner());
        let n = self.copy_out(&buf, *p);
        *p += n as u64;
        Ok(n)
    }

    fn pread(&self, buf: UserBuf, offset: u64) -> SysResult<usize> {
        Ok(self.copy_out(&buf, offset))
    }

    fn seek(&self, offset: i64, whence: i32) -> SysResult<u64> {
        let mut p = self.pos.lock().unwrap_or_else(|e| e.into_inner());
        let base = match whence {
            0 => 0,
            1 => *p as i64,
            2 => self.data.len() as i64,
            _ => return Err(Errno::EINVAL),
        };
        let new = base.checked_add(offset).ok_or(Errno::EINVAL)?;
        if new < 0 {
            return Err(Errno::EINVAL);
        }
        *p = new as u64;
        Ok(new as u64)
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            mode: S_IFREG | 0o444,
            nlink: 1,
            blksize: 4096,
            // Linux reports 0 for /proc files.
            size: 0,
            ..Default::default()
        })
    }
}

/// The contents of `path` when it names a `/proc` file the loader
/// provides for this process.
pub fn contents(path: &[u8]) -> Option<Vec<u8>> {
    let pid = crate::process::guest_pid().to_string();
    let rest = path.strip_prefix(b"/proc/self/").or_else(|| {
        path.strip_prefix(b"/proc/")
            .and_then(|r| r.strip_prefix(pid.as_bytes()))
            .and_then(|r| r.strip_prefix(b"/"))
    })?;
    match rest {
        b"stat" => Some(stat()),
        b"maps" => Some(maps()),
        b"cmdline" => Some(CMDLINE.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        _ => None,
    }
}

fn comm() -> String {
    let c = CMDLINE.lock().unwrap_or_else(|e| e.into_inner());
    let first = c.split(|&b| b == 0).next().unwrap_or(b"");
    let name = first.rsplit(|&b| b == b'/').next().unwrap_or(b"");
    String::from_utf8_lossy(&name[..name.len().min(15)]).into_owned()
}

fn stat() -> Vec<u8> {
    let pid = crate::process::guest_pid();
    // Fields 1 to 52 of proc(5); everything not known is 0.
    let mut f: Vec<String> = vec![pid.to_string(), format!("({})", comm()), "R".into()];
    f.resize(52, "0".into());
    f[3] = "1".into(); // ppid
    f[4] = pid.to_string(); // pgrp
    f[5] = pid.to_string(); // session
    f[17] = "20".into(); // priority
    f[19] = "1".into(); // num_threads (not tracked)
    f[24] = u64::MAX.to_string(); // rsslim
    f[27] = START_STACK.load(Ordering::Relaxed).to_string(); // startstack
    let mut s = f.join(" ");
    s.push('\n');
    s.into_bytes()
}

fn perms(protect: u32) -> &'static str {
    match protect & 0xff {
        0x02 => "r--p",
        0x04 | 0x08 => "rw-p",
        0x10 => "--xp",
        0x20 => "r-xp",
        0x40 | 0x80 => "rwxp",
        _ => "---p",
    }
}

fn maps() -> Vec<u8> {
    use windows::Win32::System::Memory::{
        MEM_COMMIT, MEM_FREE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, VirtualQuery,
    };
    let start_stack = START_STACK.load(Ordering::Relaxed);
    let mut out = String::new();
    let mut addr: u64 = 0x10000;
    let end: u64 = if cfg!(target_arch = "x86_64") {
        0x7fff_ffff_0000
    } else {
        0xffff_ffff_0000
    };
    while addr < end {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries the address space only.
        let n = unsafe {
            VirtualQuery(
                Some(addr as *const core::ffi::c_void),
                &mut m,
                core::mem::size_of_val(&m),
            )
        };
        if n == 0 {
            break;
        }
        let lo = m.BaseAddress as u64;
        let hi = lo + m.RegionSize as u64;
        if m.State != MEM_FREE {
            let p = if m.State == MEM_COMMIT && m.Protect.0 & PAGE_GUARD.0 == 0 {
                perms(m.Protect.0)
            } else {
                "---p"
            };
            let name = if (lo..hi).contains(&start_stack) {
                "[stack]"
            } else {
                ""
            };
            out.push_str(&format!("{lo:x}-{hi:x} {p} 00000000 00:00 0 {name}\n"));
        }
        if hi <= addr {
            break;
        }
        addr = hi;
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lists_this_stack() {
        let local = 0u64;
        let a = &local as *const u64 as u64;
        let m = String::from_utf8(maps()).unwrap();
        assert!(m.lines().any(|l| {
            let (r, _) = l.split_once(' ').unwrap();
            let (lo, hi) = r.split_once('-').unwrap();
            let (lo, hi) = (
                u64::from_str_radix(lo, 16).unwrap(),
                u64::from_str_radix(hi, 16).unwrap(),
            );
            lo <= a && a < hi && l.contains("rw-p")
        }));
    }

    #[test]
    fn stat_has_startstack_as_field_28() {
        START_STACK.store(0x1234, Ordering::Relaxed);
        let s = String::from_utf8(stat()).unwrap();
        let after = s.rsplit_once(')').unwrap().1;
        let fields: Vec<&str> = after.split_whitespace().collect();
        // Field 3 is the first after the name; startstack is field 28.
        assert_eq!(fields[28 - 3], "4660");
        assert!(contents(b"/proc/self/stat").is_some());
        assert!(contents(b"/proc/self/environ").is_none());
        assert!(contents(b"/proc/1/stat").is_none() || crate::process::guest_pid() == 1);
    }

    #[test]
    fn memfile_reads_and_seeks() {
        let f = MemFile::new(b"hello".to_vec());
        let mut b = [0u8; 3];
        assert_eq!(f.pread(UserBuf::from_mut(&mut b), 2).unwrap(), 3);
        assert_eq!(&b, b"llo");
        assert_eq!(f.seek(-1, 2).unwrap(), 4);
        assert_eq!(f.pread(UserBuf::from_mut(&mut b), 9).unwrap(), 0);
    }
}

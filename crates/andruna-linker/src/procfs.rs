//! `/proc` and `/sys` files that andruna-lx does not provide yet (an
//! `open` that lx fails is retried here; see [`crate::interim`]):
//!
//! - `/proc/self/stat` (and `/proc/<pid>/stat`): pid, name, state and
//!   `startstack`, the address of the initial stack block, which is how
//!   bionic finds the main thread's stack (`pthread_getattr_np`, which ART
//!   calls at start-up); the other fields are 0.
//! - `/sys/devices/system/cpu/{online,possible,present}`: the processor
//!   list that bionic's `get_nprocs` reads (`sysconf(_SC_NPROCESSORS_*)`,
//!   `std::thread::hardware_concurrency`; without it dex2oat runs on one
//!   thread).
//!
//! Each open takes a snapshot, as reading a Linux `/proc` file does.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Address of the initial stack block (`startstack`), set at start-up.
pub static START_STACK: AtomicU64 = AtomicU64::new(0);
/// The program's name (`comm`, at most 15 bytes), set at start-up.
pub static COMM: Mutex<String> = Mutex::new(String::new());

use andruna_lx::UserBuf;
use andruna_lx::errno::{Errno, SysResult};
use andruna_lx::fd::{FileOps, OpenFile};
use andruna_lx::linux::stat::{S_IFREG, StatData};

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

/// The contents of `path` when it names a file the loader provides.
pub fn contents(path: &[u8]) -> Option<Vec<u8>> {
    if let Some(f) = path.strip_prefix(b"/sys/devices/system/cpu/") {
        return matches!(f, b"online" | b"possible" | b"present").then(cpu_list);
    }
    let pid = andruna_lx::process::getpid().to_string();
    let rest = path.strip_prefix(b"/proc/self/").or_else(|| {
        path.strip_prefix(b"/proc/")
            .and_then(|r| r.strip_prefix(pid.as_bytes()))
            .and_then(|r| r.strip_prefix(b"/"))
    })?;
    (rest == b"stat").then(stat)
}

/// Records what `stat` reports about the program.
pub fn set_program(start_stack: u64, argv0: &str) {
    START_STACK.store(start_stack, Ordering::Relaxed);
    let name = argv0.rsplit('/').next().unwrap_or("");
    let end = name
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|&e| e <= 15)
        .last()
        .unwrap_or(0);
    *COMM.lock().unwrap_or_else(|e| e.into_inner()) = name[..end].to_string();
}

fn stat() -> Vec<u8> {
    let pid = andruna_lx::process::getpid();
    let comm = COMM.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Fields 1 to 52 of proc(5); everything not known is 0.
    let mut f: Vec<String> = vec![pid.to_string(), format!("({comm})"), "R".into()];
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

fn cpu_list() -> Vec<u8> {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if n == 1 {
        b"0\n".to_vec()
    } else {
        format!("0-{}\n", n - 1).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_lists() {
        let cpus = contents(b"/sys/devices/system/cpu/online").unwrap();
        assert!(cpus.starts_with(b"0") && cpus.ends_with(b"\n"));
        assert!(contents(b"/sys/devices/system/cpu/cpu0").is_none());
        assert!(contents(b"/proc/self/maps").is_none());
    }

    #[test]
    fn stat_has_startstack_as_field_28() {
        set_program(0x1234, "/apex/com.android.art/bin/dalvikvm");
        let s = String::from_utf8(contents(b"/proc/self/stat").unwrap()).unwrap();
        assert!(s.contains("(dalvikvm)"));
        let after = s.rsplit_once(')').unwrap().1;
        let fields: Vec<&str> = after.split_whitespace().collect();
        // Field 3 is the first after the name; startstack is field 28.
        assert_eq!(fields[28 - 3], "4660");
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

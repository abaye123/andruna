//! A stand-in for logd's write socket while there is no log service.
//!
//! liblog sends each record as one datagram to the Unix socket
//! `/dev/socket/logdw`. lx has no sockets yet, so the loader answers
//! `socket(AF_UNIX, SOCK_DGRAM)` with a descriptor of its own
//! ([`crate::interim`]); `connect` to `/dev/socket/logdw` succeeds on it, and
//! every datagram written to it (`writev`, `write`, `sendmsg`) is printed on
//! standard error as `P/tag(tid): message`, for records of the main, system
//! and crash buffers at or above `ANDRUNA_LOG_PRIORITY` (a letter of
//! `VDIWEF` or a number; default `W`). Binary buffers (events, stats,
//! security) are dropped. sysd's log service replaces this in phase 2.

use std::collections::HashSet;
use std::io::Write as _;
use std::sync::Mutex;

use andruna_lx::UserBuf;
use andruna_lx::errno::SysResult;
use andruna_lx::linux::errno::ENOTSOCK;

const ECONNREFUSED: i32 = 111;
use andruna_lx::fd::{FileOps, OpenFile};
use andruna_lx::linux::stat::{S_IFSOCK, StatData};

/// Descriptors that are logd sockets; true once connected.
static SOCKETS: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());

/// The logd socket file type (datagrams written with `write`).
#[derive(Debug)]
pub struct LogSocket;

impl FileOps for LogSocket {
    fn write(&self, _file: &OpenFile, buf: UserBuf) -> SysResult<usize> {
        // SAFETY: the caller's buffer is readable for its length.
        let b = unsafe { core::slice::from_raw_parts(buf.ptr(), buf.len()) };
        record(b);
        Ok(buf.len())
    }

    fn stat(&self) -> SysResult<StatData> {
        Ok(StatData {
            mode: S_IFSOCK | 0o777,
            nlink: 1,
            blksize: 4096,
            ..Default::default()
        })
    }
}

/// Creates a logd socket descriptor.
pub fn socket(cloexec: bool) -> i64 {
    let f = OpenFile::new(Box::new(LogSocket), 2);
    match andruna_lx::fd::FDS.insert(std::sync::Arc::new(f), cloexec, 0) {
        Ok(fd) => {
            let mut s = SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
            s.retain(|x| x.0 != fd);
            s.push((fd, false));
            i64::from(fd)
        }
        Err(e) => -i64::from(e.0),
    }
}

pub fn is_socket(fd: i32) -> bool {
    SOCKETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|x| x.0 == fd)
}

/// `connect`: only logd's write socket exists.
pub fn connect(fd: i32, path: &[u8]) -> i64 {
    let mut s = SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(e) = s.iter_mut().find(|x| x.0 == fd) else {
        return -i64::from(ENOTSOCK);
    };
    if path == b"/dev/socket/logdw" {
        e.1 = true;
        0
    } else {
        -i64::from(ECONNREFUSED)
    }
}

/// The descriptor was closed.
pub fn closed(fd: i32) {
    SOCKETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|x| x.0 != fd);
}

/// A datagram gathered from an `iovec` array.
///
/// # Safety
///
/// `v` points to `n` readable `struct iovec`, each a readable buffer.
pub unsafe fn gather(v: *const [u64; 2], n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        // SAFETY: the function's contract.
        let [base, len] = unsafe { v.add(i).read_unaligned() };
        if len != 0 && base != 0 {
            // SAFETY: as above.
            out.extend_from_slice(unsafe {
                core::slice::from_raw_parts(base as *const u8, len as usize)
            });
        }
    }
    out
}

fn min_priority() -> u8 {
    static P: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *P.get_or_init(|| {
        let v = std::env::var("ANDRUNA_LOG_PRIORITY").unwrap_or_default();
        match v.as_str() {
            "V" | "v" => 2,
            "D" | "d" => 3,
            "I" | "i" => 4,
            "W" | "w" | "" => 5,
            "E" | "e" => 6,
            "F" | "f" => 7,
            n => n.parse().unwrap_or(5),
        }
    })
}

/// Parsed text record: (priority, tid, tag, message).
pub fn parse(b: &[u8]) -> Option<(u8, u16, &[u8], &[u8])> {
    // android_log_header_t: id (1), tid (2), realtime (8); then the
    // priority byte, the tag and the message, each NUL-terminated.
    const LOG_ID_MAIN: u8 = 0;
    const LOG_ID_SYSTEM: u8 = 3;
    const LOG_ID_CRASH: u8 = 4;
    const LOG_ID_DEFAULT: u8 = 7;
    if b.len() < 12
        || !matches!(
            b[0],
            LOG_ID_MAIN | LOG_ID_SYSTEM | LOG_ID_CRASH | LOG_ID_DEFAULT
        )
    {
        return None;
    }
    let tid = u16::from_le_bytes([b[1], b[2]]);
    let prio = b[11];
    let rest = &b[12..];
    let t_end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    let tag = &rest[..t_end];
    let msg = rest.get(t_end + 1..).unwrap_or(b"");
    let m_end = msg.iter().position(|&c| c == 0).unwrap_or(msg.len());
    Some((prio, tid, tag, &msg[..m_end]))
}

/// Prints one record.
pub fn record(b: &[u8]) {
    let Some((prio, tid, tag, msg)) = parse(b) else {
        return;
    };
    if prio < min_priority() {
        return;
    }
    let letter = match prio {
        2 => 'V',
        3 => 'D',
        4 => 'I',
        5 => 'W',
        6 => 'E',
        7 => 'F',
        _ => '?',
    };
    let tag = String::from_utf8_lossy(tag);
    let mut e = std::io::stderr().lock();
    for line in String::from_utf8_lossy(msg).lines() {
        let _ = writeln!(e, "{letter}/{tag}({tid}): {line}");
    }
}

/// Descriptors of the closed set, for tests.
pub fn sockets() -> HashSet<i32> {
    SOCKETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|x| x.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_text_record() {
        let mut b = vec![0u8, 0x34, 0x12];
        b.extend_from_slice(&[0; 8]);
        b.push(6);
        b.extend_from_slice(b"art\0boom\0");
        let (p, tid, tag, msg) = parse(&b).unwrap();
        assert_eq!((p, tid, tag, msg), (6, 0x1234, &b"art"[..], &b"boom"[..]));
        b[0] = 2; // events: binary
        assert!(parse(&b).is_none());
    }

    #[test]
    fn socket_connects_only_to_logd() {
        let fd = socket(true) as i32;
        assert!(fd >= 0);
        assert!(is_socket(fd));
        assert_eq!(connect(fd, b"/dev/socket/other"), -i64::from(ECONNREFUSED));
        assert_eq!(connect(fd, b"/dev/socket/logdw"), 0);
        let _ = andruna_lx::fd::FDS.close(fd);
        closed(fd);
        assert!(!sockets().contains(&fd));
    }
}

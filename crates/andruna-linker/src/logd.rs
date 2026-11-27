//! A stand-in for logd's write socket while there is no log service.
//!
//! liblog sends each record as one datagram to the Unix socket
//! `/dev/socket/logdw`. The loader receives the datagrams sent to that name
//! in the process ([`install`]), and every one is written to
//! the guest's standard error (descriptor 2) as `P/tag(tid): message`, for records of the main, system
//! and crash buffers at or above `ANDRUNA_LOG_PRIORITY` (a letter of
//! `VDIWEF` or a number; default `W`). Binary buffers (events, stats,
//! security) are dropped. sysd's log service replaces this in phase 2.

use std::io::Write as _;

/// Receives liblog's datagrams at `/dev/socket/logdw` through andruna-lx's
/// Unix sockets ([`andruna_lx::socket::register_sink`]).
pub fn install() {
    let _ = andruna_lx::socket::register_sink("/dev/socket/logdw", record);
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
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(msg).lines() {
        let _ = writeln!(out, "{letter}/{tag}({tid}): {line}");
    }
    // The guest's descriptor 2, so a redirected stderr gets the records.
    let mut done = 0usize;
    while done < out.len() {
        // SAFETY: writes from a local buffer.
        let n = unsafe {
            crate::syscall::lx_raw(
                WRITE,
                [
                    2,
                    out[done..].as_ptr() as i64,
                    (out.len() - done) as i64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if n <= 0 {
            break;
        }
        done += n as usize;
    }
}

#[cfg(target_arch = "x86_64")]
const WRITE: i64 = 1;
#[cfg(target_arch = "aarch64")]
const WRITE: i64 = 64;

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
}

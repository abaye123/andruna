//! Messages of the kinds 0x0010..=0x001F: the event stream (notifications,
//! app crashes), the missing-function counters and activity starts.
//!
//! The event stream is a long poll: a client sends
//! [`Request::WaitEvents`](super::Request::WaitEvents) with the sequence
//! number of the last event it saw, and sysd answers as soon as a newer
//! event exists or the wait times out. sysd keeps the most recent events in
//! a ring; a client that fell behind gets `missed = true`.
//!
//! Every event is
//!
//! ```text
//! u64 sequence number (1, 2, ... per sysd instance)
//! u64 time, milliseconds since the Unix epoch
//! u16 event type
//! u32 body length
//! ...  body
//! ```
//!
//! so a client skips the types it does not know.

use super::{DecodeError, Reader, Writer};

/// Longest wait a [`Request::WaitEvents`](super::Request::WaitEvents) may
/// ask for; longer requests are cut to this.
pub const MAX_WAIT_MS: u32 = 30_000;

/// Most missing-function entries in one message.
pub const MAX_MISSING_FUNCTIONS: usize = 4096;

mod event_type {
    pub const NOTIFICATION_POSTED: u16 = 1;
    pub const NOTIFICATION_REMOVED: u16 = 2;
    pub const APP_CRASHED: u16 = 3;
}

/// One entry of the missing-function counters
/// ([03-components.md](../../../../docs/spec/03-components.md#cross-cutting-missing-function-counters)).
/// `name` is `<area>:<entry point>`, for example `lx:epoll_pwait2` or
/// `service:package.getInstalledModules`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFunction {
    pub name: String,
    /// How often the app called it.
    pub calls: u64,
}

/// An Android notification as the Windows notification bridge needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub package: String,
    /// The app's notification id (Android `int`).
    pub id: i32,
    /// The app's notification tag; empty when it has none.
    pub tag: String,
    /// The notification channel id.
    pub channel: String,
    pub title: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// An app posted or updated a notification (type 1).
    NotificationPosted(Notification),
    /// An app cancelled a notification, or it was removed with the app
    /// (type 2).
    NotificationRemoved {
        package: String,
        id: i32,
        tag: String,
    },
    /// An app's host process ended abnormally (type 3).
    AppCrashed {
        package: String,
        /// Process id of the host that ended.
        pid: u32,
        /// Its exit code.
        exit_code: u32,
        /// What the runtime reported (for example the uncaught exception);
        /// empty when the process ended without a report.
        reason: String,
    },
    /// An event type this version does not know; its body was skipped.
    Unknown(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub time_ms: u64,
    pub kind: EventKind,
}

/// Checks an activity class name as written in a manifest: dot-separated
/// Java identifiers (ASCII letters, digits, `_` and `$`, not starting with
/// a digit), optionally starting with `.` (relative to the package), at
/// most 255 bytes.
pub fn is_valid_activity_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    let rest = name.strip_prefix('.').unwrap_or(name);
    !rest.is_empty()
        && rest.split('.').all(|seg| {
            let mut chars = seg.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        })
}

pub(super) fn write_missing(w: &mut Writer, entries: &[MissingFunction]) {
    let n = entries.len().min(MAX_MISSING_FUNCTIONS);
    w.u32(n as u32);
    for e in &entries[..n] {
        w.str(&e.name);
        w.u64(e.calls);
    }
}

pub(super) fn read_missing(r: &mut Reader<'_>) -> Result<Vec<MissingFunction>, DecodeError> {
    let n = r.u32()? as usize;
    // Each entry needs at least 10 bytes (string length and count).
    if n > MAX_MISSING_FUNCTIONS || n > r.0.len() / 10 {
        return Err(DecodeError::Truncated);
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(MissingFunction {
            name: r.str()?,
            calls: r.u64()?,
        });
    }
    Ok(out)
}

pub(super) fn write_event(w: &mut Writer, e: &Event) {
    let mut body = Writer(Vec::new());
    let ty = match &e.kind {
        EventKind::NotificationPosted(n) => {
            body.str(&n.package);
            body.i32(n.id);
            body.str(&n.tag);
            body.str(&n.channel);
            body.str(&n.title);
            body.str(&n.text);
            event_type::NOTIFICATION_POSTED
        }
        EventKind::NotificationRemoved { package, id, tag } => {
            body.str(package);
            body.i32(*id);
            body.str(tag);
            event_type::NOTIFICATION_REMOVED
        }
        EventKind::AppCrashed {
            package,
            pid,
            exit_code,
            reason,
        } => {
            body.str(package);
            body.u32(*pid);
            body.u32(*exit_code);
            body.str(reason);
            event_type::APP_CRASHED
        }
        // Never produced by a server; written with an empty body.
        EventKind::Unknown(t) => *t,
    };
    w.u64(e.seq);
    w.u64(e.time_ms);
    w.u16(ty);
    w.u32(body.0.len() as u32);
    w.0.extend_from_slice(&body.0);
}

pub(super) fn read_events(r: &mut Reader<'_>) -> Result<Vec<Event>, DecodeError> {
    let n = r.u32()? as usize;
    // Each event needs at least its 22-byte head.
    if n > r.0.len() / 22 {
        return Err(DecodeError::Truncated);
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let seq = r.u64()?;
        let time_ms = r.u64()?;
        let ty = r.u16()?;
        let len = r.u32()? as usize;
        let mut b = Reader(r.take(len)?);
        let kind = match ty {
            event_type::NOTIFICATION_POSTED => EventKind::NotificationPosted(Notification {
                package: b.str()?,
                id: b.i32()?,
                tag: b.str()?,
                channel: b.str()?,
                title: b.str()?,
                text: b.str()?,
            }),
            event_type::NOTIFICATION_REMOVED => EventKind::NotificationRemoved {
                package: b.str()?,
                id: b.i32()?,
                tag: b.str()?,
            },
            event_type::APP_CRASHED => EventKind::AppCrashed {
                package: b.str()?,
                pid: b.u32()?,
                exit_code: b.u32()?,
                reason: b.str()?,
            },
            t => {
                b.0 = &[];
                EventKind::Unknown(t)
            }
        };
        b.end()?;
        out.push(Event { seq, time_ms, kind });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{Request, Response};
    use super::*;

    fn sample_events() -> Vec<Event> {
        vec![
            Event {
                seq: 1,
                time_ms: 1_700_000_000_000,
                kind: EventKind::NotificationPosted(Notification {
                    package: "com.example.chat".into(),
                    id: -5,
                    tag: String::new(),
                    channel: "messages".into(),
                    title: "\u{5e9}\u{5dc}\u{5d5}\u{5dd}".into(),
                    text: "hi".into(),
                }),
            },
            Event {
                seq: 2,
                time_ms: 1,
                kind: EventKind::NotificationRemoved {
                    package: "com.example.chat".into(),
                    id: 7,
                    tag: "t".into(),
                },
            },
            Event {
                seq: 3,
                time_ms: 2,
                kind: EventKind::AppCrashed {
                    package: "org.example.app".into(),
                    pid: 4242,
                    exit_code: 0xC000_0005,
                    reason: "java.lang.IllegalStateException".into(),
                },
            },
        ]
    }

    #[test]
    fn events_and_counters_round_trip() {
        let resp = Response::Events {
            epoch: 99,
            next: 3,
            missed: true,
            events: sample_events(),
        };
        assert_eq!(Response::decode(&resp.encode(4)), Ok((4, resp)));
        let resp = Response::MissingFunctions {
            updated_ms: 5,
            entries: vec![MissingFunction {
                name: "lx:epoll_pwait2".into(),
                calls: 12,
            }],
        };
        assert_eq!(Response::decode(&resp.encode(5)), Ok((5, resp)));
        for req in [
            Request::WaitEvents {
                epoch: 1,
                after: u64::MAX,
                timeout_ms: 0,
            },
            Request::GetMissingFunctions {
                package: "a.b".into(),
            },
            Request::ReportMissingFunctions { entries: vec![] },
            Request::StartActivity {
                package: "a.b".into(),
                activity: ".Main".into(),
            },
        ] {
            assert_eq!(Request::decode(&req.encode(6)), Ok((6, req)));
        }
    }

    #[test]
    fn unknown_event_types_are_skipped() {
        let mut w = Writer::new(super::super::kind::EVENTS, 1);
        w.u64(1);
        w.u64(2);
        w.u8(0);
        w.u32(2);
        // Type 99 with a 3-byte body, then a known event.
        w.u64(1);
        w.u64(0);
        w.u16(99);
        w.u32(3);
        w.0.extend_from_slice(&[1, 2, 3]);
        write_event(&mut w, &sample_events()[1]);
        let (_, resp) = Response::decode(&w.0).unwrap();
        let Response::Events { events, .. } = resp else {
            panic!("{resp:?}")
        };
        assert_eq!(events[0].kind, EventKind::Unknown(99));
        assert_eq!(events[1], sample_events()[1]);
    }

    #[test]
    fn rejects_bad_bodies_and_counts() {
        // A known event whose body is longer than its fields.
        let mut w = Writer::new(super::super::kind::EVENTS, 1);
        w.u64(1);
        w.u64(2);
        w.u8(0);
        w.u32(1);
        w.u64(1);
        w.u64(0);
        w.u16(event_type::NOTIFICATION_REMOVED);
        w.u32(10);
        w.str("a.b");
        w.i32(1);
        w.u16(0);
        w.u8(0);
        assert!(Response::decode(&w.0).is_err());

        let mut bytes = Response::MissingFunctions {
            updated_ms: 0,
            entries: vec![],
        }
        .encode(1);
        let at = super::super::HEADER_LEN + 8;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Response::decode(&bytes), Err(DecodeError::Truncated));

        let full = Response::Events {
            epoch: 1,
            next: 3,
            missed: false,
            events: sample_events(),
        }
        .encode(1);
        for cut in 0..full.len() {
            assert!(Response::decode(&full[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn activity_names() {
        for ok in [".Main", "com.example.Main", "Main", "a.b$Inner", "_x.Y1"] {
            assert!(is_valid_activity_name(ok), "{ok}");
        }
        for bad in [
            "", ".", "..Main", "a..b", "1a", "a.1b", "a/b", "a b", "a.", "\u{5d0}",
        ] {
            assert!(!is_valid_activity_name(bad), "{bad}");
        }
        assert!(!is_valid_activity_name(&"a".repeat(256)));
    }
}

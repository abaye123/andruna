//! The event stream to clients: notifications and app crashes.
//!
//! Producers (the supervisor for crashes, the notification service once it
//! exists) call [`Events::publish`]. Clients long-poll with
//! `WaitEvents` ([andruna-ipc](../../andruna-ipc/README.md)); the most
//! recent [`CAPACITY`] events are kept so that a client that reconnects
//! gets what it missed.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use andruna_ipc::{Event, EventKind, Response};

/// Events kept for clients that are behind.
pub const CAPACITY: usize = 256;

struct Ring {
    /// Sequence number of the next event; the first is 1.
    next_seq: u64,
    items: VecDeque<Event>,
    stopped: bool,
}

pub struct Events {
    epoch: u64,
    ring: Mutex<Ring>,
    changed: Condvar,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    pub fn new() -> Self {
        // Start time and process id: distinct for every sysd instance, so a
        // client notices a restart even when sequence numbers overlap.
        let epoch = (now_ms() << 16) ^ u64::from(std::process::id());
        Self {
            epoch: epoch.max(1),
            ring: Mutex::new(Ring {
                next_seq: 1,
                items: VecDeque::new(),
                stopped: false,
            }),
            changed: Condvar::new(),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Adds an event and wakes every waiting client. Returns its sequence
    /// number.
    pub fn publish(&self, kind: EventKind) -> u64 {
        let mut r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let seq = r.next_seq;
        r.next_seq += 1;
        if r.items.len() == CAPACITY {
            r.items.pop_front();
        }
        r.items.push_back(Event {
            seq,
            time_ms: now_ms(),
            kind,
        });
        self.changed.notify_all();
        seq
    }

    /// Wakes every waiting client with what it has; later waits return at
    /// once.
    pub fn stop(&self) {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
        self.changed.notify_all();
    }

    /// Answers a `WaitEvents` request.
    pub fn wait(&self, epoch: u64, after: u64, timeout: Duration) -> Response {
        let deadline = Instant::now() + timeout;
        let mut r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let latest = r.next_seq - 1;
        if after == u64::MAX {
            return Response::Events {
                epoch: self.epoch,
                next: latest,
                missed: false,
                events: Vec::new(),
            };
        }
        // Another sysd instance's position (or one from the future) starts
        // over from the oldest event kept.
        let restarted = epoch != self.epoch || after > latest;
        let after = if restarted { 0 } else { after };
        loop {
            let first_kept = r.items.front().map(|e| e.seq);
            let events: Vec<Event> = r.items.iter().filter(|e| e.seq > after).cloned().collect();
            let now = Instant::now();
            if !events.is_empty() || r.stopped || now >= deadline {
                let missed = restarted && after == 0 && epoch != 0 && epoch != self.epoch
                    || first_kept.is_some_and(|f| f > after + 1);
                let next = events.last().map_or(after, |e| e.seq);
                return Response::Events {
                    epoch: self.epoch,
                    next,
                    missed,
                    events,
                };
            }
            r = self
                .changed
                .wait_timeout(r, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn crash(pid: u32) -> EventKind {
        EventKind::AppCrashed {
            package: "a.b".into(),
            pid,
            exit_code: 1,
            reason: String::new(),
        }
    }

    fn unpack(r: Response) -> (u64, u64, bool, Vec<Event>) {
        match r {
            Response::Events {
                epoch,
                next,
                missed,
                events,
            } => (epoch, next, missed, events),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn current_position_then_new_events() {
        let ev = Events::new();
        ev.publish(crash(1));
        let (epoch, next, missed, events) = unpack(ev.wait(0, u64::MAX, Duration::ZERO));
        assert_eq!(
            (epoch, next, missed, events.len()),
            (ev.epoch(), 1, false, 0)
        );
        ev.publish(crash(2));
        let (_, next, missed, events) = unpack(ev.wait(epoch, next, Duration::ZERO));
        assert_eq!((next, missed), (2, false));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crash(2));
    }

    #[test]
    fn long_poll_wakes_on_publish_and_times_out() {
        let ev = Arc::new(Events::new());
        let e2 = ev.clone();
        let t = thread::spawn(move || {
            let start = Instant::now();
            let r = unpack(e2.wait(e2.epoch(), 0, Duration::from_secs(10)));
            (r, start.elapsed())
        });
        thread::sleep(Duration::from_millis(50));
        ev.publish(crash(3));
        let ((_, next, _, events), took) = t.join().unwrap();
        assert_eq!((next, events.len()), (1, 1));
        assert!(took < Duration::from_secs(5), "{took:?}");

        let start = Instant::now();
        let (_, next, _, events) = unpack(ev.wait(ev.epoch(), 1, Duration::from_millis(100)));
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert_eq!((next, events.len()), (1, 0));
    }

    #[test]
    fn behind_and_other_epoch_are_reported_as_missed() {
        let ev = Events::new();
        for pid in 0..(CAPACITY as u32 + 10) {
            ev.publish(crash(pid));
        }
        let (_, next, missed, events) = unpack(ev.wait(ev.epoch(), 3, Duration::ZERO));
        assert!(missed);
        assert_eq!(events.len(), CAPACITY);
        assert_eq!(next, CAPACITY as u64 + 10);

        let (_, _, missed, events) = unpack(ev.wait(ev.epoch() + 1, 5, Duration::ZERO));
        assert!(missed);
        assert_eq!(events.len(), CAPACITY);
    }

    #[test]
    fn stop_releases_waiters() {
        let ev = Arc::new(Events::new());
        let e2 = ev.clone();
        let t = thread::spawn(move || e2.wait(e2.epoch(), 0, Duration::from_secs(3600)));
        thread::sleep(Duration::from_millis(30));
        ev.stop();
        assert_eq!(unpack(t.join().unwrap()).3.len(), 0);
    }
}

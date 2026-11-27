//! Vsync for Choreographer, from the compositor clock.
//!
//! On a device, SurfaceFlinger sends `DisplayEventReceiver` vsync events
//! with the vsync time, a frame id and the expected presentation time. On
//! Windows 11 the compositor (DWM) has a clock of its own:
//! `DCompositionWaitForCompositorClock` returns at each composition, and
//! `DCompositionGetStatistics` gives the frame's start, its target
//! (present) time and period, plus each display's refresh period
//! (displays may run at different rates; the compositor clock runs at the
//! fastest).
//!
//! [`VsyncSource`] runs one thread that waits on the clock while at least
//! one subscriber is listening, and sends each tick as a [`VsyncEvent`] on
//! the uptime clock. With no subscriber it sleeps on a condition variable:
//! no wakeups when no app draws.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::clock;
use crate::sys::vsync::{self as os, TargetId};

/// The refresh period of one display in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayTiming {
    pub target: TargetId,
    pub period_ns: i64,
}

/// One compositor clock tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VsyncEvent {
    /// The vsync id: counts compositor clock ticks since the source
    /// started, including ticks the thread missed.
    pub frame_id: u64,
    /// When the composition started (the vsync time), uptime clock.
    pub timestamp_ns: i64,
    /// When the frame is expected on screen.
    pub expected_present_ns: i64,
    /// The compositor clock's period.
    pub period_ns: i64,
    /// Per display periods, to pace a window at its own monitor's rate.
    pub displays: Vec<DisplayTiming>,
}

impl VsyncEvent {
    /// The period of `target`, or the compositor's.
    pub fn period_for(&self, target: Option<TargetId>) -> i64 {
        target
            .and_then(|t| self.displays.iter().find(|d| d.target == t))
            .map_or(self.period_ns, |d| d.period_ns)
    }
}

#[derive(Default)]
struct Shared {
    subscribers: Vec<Sender<VsyncEvent>>,
    stop: bool,
}

/// The vsync thread.
pub struct VsyncSource {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl VsyncSource {
    pub fn start() -> std::io::Result<VsyncSource> {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("vsync".into())
            .spawn(move || run(s))?;
        Ok(VsyncSource {
            shared,
            join: Some(join),
        })
    }

    /// A stream of vsync events; dropping the receiver unsubscribes.
    pub fn subscribe(&self) -> Receiver<VsyncEvent> {
        let (tx, rx) = channel();
        let (m, cv) = &*self.shared;
        m.lock().unwrap().subscribers.push(tx);
        cv.notify_all();
        rx
    }
}

impl Drop for VsyncSource {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap().stop = true;
        cv.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The vsync a clock tick at `now` belongs to, from the last frame the
/// compositor created. DWM creates no frame while nothing on screen
/// changes, but its clock keeps ticking: the tick's time is the last
/// frame's start plus whole periods. Returns (vsync time, present time,
/// periods since that frame), in performance counter units.
pub fn extrapolate(start: i64, target: i64, period: i64, now: i64) -> (i64, i64, i64) {
    if period <= 0 || now <= start {
        return (start, target, 0);
    }
    // The nearest tick: the wait returns just after a tick, a frame's
    // start may sit a little off the grid.
    let n = (now - start + period / 2) / period;
    (start + n * period, target + n * period, n)
}

fn run(shared: Arc<(Mutex<Shared>, Condvar)>) {
    let (m, cv) = &*shared;
    let mut last_ts = i64::MIN;
    let mut vsync_id = 0u64;
    loop {
        {
            let mut g = m.lock().unwrap();
            while g.subscribers.is_empty() && !g.stop {
                g = cv.wait(g).unwrap();
            }
            if g.stop {
                return;
            }
        }
        if !os::wait_for_compositor_clock(100) {
            continue;
        }
        let Some((f, targets)) = os::latest_frame() else {
            continue;
        };
        let (now, _) = crate::sys::time::qpc();
        let (start, target, n) = extrapolate(f.start, f.target, f.period, now);
        // One event per tick: a new frame's start lands near a tick
        // already sent.
        let period = f.period.max(1);
        let _ = n;
        if last_ts != i64::MIN && start - last_ts < period / 2 {
            continue;
        }
        // Ticks count from the first one, including ticks this thread did
        // not see (it was descheduled): ids follow time.
        vsync_id += if last_ts == i64::MIN {
            1
        } else {
            ((start - last_ts + period / 2) / period).max(1) as u64
        };
        last_ts = start;
        let e = VsyncEvent {
            frame_id: vsync_id,
            timestamp_ns: clock::qpc_to_uptime_ns(start),
            expected_present_ns: clock::qpc_to_uptime_ns(target),
            period_ns: clock::qpc_duration_ns(f.period),
            displays: targets
                .iter()
                .filter(|t| t.vblank > 0)
                .map(|t| DisplayTiming {
                    target: t.id,
                    period_ns: clock::qpc_duration_ns(t.vblank),
                })
                .collect(),
        };
        let mut g = m.lock().unwrap();
        g.subscribers.retain(|s| s.send(e.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::extrapolate;

    #[test]
    fn ticks_without_new_frames_are_extrapolated() {
        // Last frame started at 1000 with an 8.33 ms (83 333 ticks) period.
        let p = 83_333;
        assert_eq!(extrapolate(1000, 1000 + p, p, 1000), (1000, 1000 + p, 0));
        assert_eq!(extrapolate(1000, 1000 + p, p, 1000 + p + 5), (1000 + p, 1000 + 2 * p, 1));
        assert_eq!(
            extrapolate(1000, 1000 + p, p, 1000 + 10 * p - 1),
            (1000 + 10 * p, 1000 + 11 * p, 10)
        );
        assert_eq!(extrapolate(1000, 1100, 0, 5000), (1000, 1100, 0));
    }
}

/// Waits for `n` events and returns their mean period, for tests and the
/// host.
pub fn measure(rx: &Receiver<VsyncEvent>, n: usize) -> Option<(Duration, Vec<VsyncEvent>)> {
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        v.push(rx.recv_timeout(Duration::from_secs(2)).ok()?);
    }
    let span = v.last()?.timestamp_ns - v.first()?.timestamp_ns;
    Some((
        Duration::from_nanos((span / (n as i64 - 1).max(1)) as u64),
        v,
    ))
}

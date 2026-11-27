//! What keeps sysd alive, and the idle shutdown.
//!
//! sysd runs while a client is connected (the launcher for the length of
//! its request, the manager while it is open) or a host process runs. When
//! neither has been the case for the idle timeout, sysd exits, so nothing
//! runs when no app and no manager is open
//! ([architecture](../../docs/spec/02-architecture.md#processes)).

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct State {
    connections: u32,
    hosts: u32,
    /// When the last connection or host ended (or sysd started).
    idle_since: Instant,
    stopped: bool,
}

#[derive(Debug)]
pub struct Activity {
    state: Mutex<State>,
    changed: Condvar,
    timeout: Duration,
}

impl Activity {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                connections: 0,
                hosts: 0,
                idle_since: Instant::now(),
                stopped: false,
            }),
            changed: Condvar::new(),
            timeout,
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let was_busy = s.connections + s.hosts > 0;
        f(&mut s);
        if was_busy && s.connections + s.hosts == 0 {
            s.idle_since = Instant::now();
        }
        self.changed.notify_all();
    }

    pub fn connection_opened(&self) {
        self.update(|s| s.connections += 1);
    }

    pub fn connection_closed(&self) {
        self.update(|s| s.connections = s.connections.saturating_sub(1));
    }

    /// An open connection stops counting as activity: it only waits for
    /// events (a notifier), which must not keep sysd running. Its close is
    /// then not reported with [`Activity::connection_closed`].
    pub fn connection_passive(&self) {
        self.update(|s| s.connections = s.connections.saturating_sub(1));
    }

    pub fn host_started(&self) {
        self.update(|s| s.hosts += 1);
    }

    pub fn host_exited(&self) {
        self.update(|s| s.hosts = s.hosts.saturating_sub(1));
    }

    /// (connections, hosts)
    pub fn counts(&self) -> (u32, u32) {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (s.connections, s.hosts)
    }

    /// Ends [`Activity::wait_idle`] without an idle result.
    pub fn stop(&self) {
        self.update(|s| s.stopped = true);
    }

    /// Blocks until sysd has been idle for the timeout (true) or
    /// [`Activity::stop`] was called (false).
    pub fn wait_idle(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if s.stopped {
                return false;
            }
            if s.connections + s.hosts > 0 {
                s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let idle_for = s.idle_since.elapsed();
            if idle_for >= self.timeout {
                return true;
            }
            s = self
                .changed
                .wait_timeout(s, self.timeout - idle_for)
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

    #[test]
    fn idle_after_timeout_when_nothing_runs() {
        let a = Activity::new(Duration::from_millis(50));
        let t = Instant::now();
        assert!(a.wait_idle());
        assert!(t.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn activity_restarts_the_idle_clock() {
        let a = Arc::new(Activity::new(Duration::from_millis(150)));
        a.connection_opened();
        let b = a.clone();
        let waiter = thread::spawn(move || {
            assert!(b.wait_idle());
            Instant::now()
        });
        thread::sleep(Duration::from_millis(200));
        a.host_started();
        a.connection_closed();
        thread::sleep(Duration::from_millis(100));
        let last_busy = Instant::now();
        a.host_exited();
        let idle_at = waiter.join().unwrap();
        // The full idle timeout counts from the end of the last activity.
        let waited = idle_at - last_busy;
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        assert_eq!(a.counts(), (0, 0));
    }

    #[test]
    fn stop_ends_the_wait() {
        let a = Arc::new(Activity::new(Duration::from_secs(3600)));
        let b = a.clone();
        let waiter = thread::spawn(move || b.wait_idle());
        thread::sleep(Duration::from_millis(20));
        a.stop();
        assert!(!waiter.join().unwrap());
    }
}

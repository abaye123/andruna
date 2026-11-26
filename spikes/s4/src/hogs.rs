//! Busy threads that keep every core loaded, so the thread under test is
//! preempted and migrated often.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub struct Hogs {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Hogs {
    /// Starts `per_core` busy threads per logical processor.
    pub fn start(per_core: usize) -> Self {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            * per_core;
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..n)
            .map(|_| {
                let s = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 0u64;
                    while !s.load(Ordering::Relaxed) {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                })
            })
            .collect();
        Hogs { stop, threads }
    }
}

impl Drop for Hogs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

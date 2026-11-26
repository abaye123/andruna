//! Timing helpers.

use std::time::Instant;

/// Runs `f` `n` times and returns nanoseconds per call (best of 3 runs).
pub fn time_ns<R>(n: usize, mut f: impl FnMut() -> R) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..n {
            core::hint::black_box(f());
        }
        best = best.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
    }
    best
}

/// (median, 99th percentile) of the samples.
pub fn median_p99(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = v[v.len() / 2];
    let p = v[((v.len() as f64 * 0.99) as usize).min(v.len() - 1)];
    (m, p)
}

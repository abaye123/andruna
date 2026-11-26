//! Minimal timing helpers. Each measurement runs once to warm up, then the
//! best of five runs is kept.

use std::time::Instant;

pub struct Result {
    pub name: &'static str,
    pub ns_per_iter: f64,
}

pub fn run(name: &'static str, iters: u64, f: impl Fn(u64)) -> Result {
    f(iters / 10 + 1);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        f(iters);
        let ns = t.elapsed().as_nanos() as f64 / iters as f64;
        best = best.min(ns);
    }
    Result {
        name,
        ns_per_iter: best,
    }
}

/// Prints a result and the same value with the empty-loop cost removed.
pub fn report(r: &Result, empty: &Result) {
    println!(
        "{:<58} {:>9.3} ns/iter  ({:>8.3} ns above empty loop)",
        r.name,
        r.ns_per_iter,
        r.ns_per_iter - empty.ns_per_iter
    );
}

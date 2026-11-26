//! Signals (not implemented yet; this module marks the boundary).
//!
//! The next step builds here, from spike S1 (`docs/plan/spike-results/S1.md`)
//! and S4:
//!
//! - `sigaction`, `sigprocmask`/`pthread_sigmask`, `sigaltstack`, and
//!   synchronous signals (SIGSEGV, SIGBUS, SIGILL, SIGFPE, SIGTRAP) delivered
//!   from a first vectored exception handler with bionic `siginfo_t` and
//!   `ucontext_t` layouts, always on the alternate stack.
//! - The x64 FS-base lazy restore (S4) runs in the same handler, before any
//!   signal delivery.
//! - Faults on lx-owned pages that are transiently decommitted by
//!   `madvise(MADV_DONTNEED)` ([`crate::mem`]) must be retried, not
//!   delivered.
//! - `SIGSEGV` codes (`SEGV_MAPERR` / `SEGV_ACCERR`) come from lx's
//!   address-space table rather than `VirtualQuery`.
//! - Process-directed signals and `sigwait`/`sigtimedwait` for ART's signal
//!   catcher; thread suspension stays cooperative (S1 recommendation).
//! - Blocking calls (`futex` waits, `nanosleep`) gain an interruption point
//!   so that a delivered signal makes them return `EINTR`.
//!
//! Until then no lx call returns `EINTR`.

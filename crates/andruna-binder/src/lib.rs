//! Binder without a kernel driver (ADR 0025).
//!
//! - [`uapi`]: the Linux binder interface (`linux/android/binder.h`).
//! - [`broker`]: the driver logic, run in sysd for all guest processes.
//! - [`pack`]: the guest side of the write and read streams.
//! - [`transport`]: the control pipe and the shared-memory lanes between a
//!   guest thread and its broker thread.
//! - [`host`]: the broker's server in sysd.
//! - [`sys`]: the Windows calls of both sides.
//!
//! `/dev/binder` in guest processes is the crate `andruna-binder-device`,
//! kept apart so that sysd does not link andruna-lx.

pub mod broker;
pub mod host;
pub mod pack;
pub mod sys;
pub mod transport;
pub mod uapi;

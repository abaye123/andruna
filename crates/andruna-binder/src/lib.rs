//! Binder without a kernel driver (ADR 0025).
//!
//! - [`uapi`]: the Linux binder interface (`linux/android/binder.h`).
//! - [`broker`]: the driver logic, run in sysd for all guest processes.
//! - [`pack`]: the guest side of the write and read streams.
//! - [`transport`]: the control pipe and the shared-memory lanes between a
//!   guest thread and its broker thread.
//! - [`host`]: the broker's server in sysd.
//! - `device` (feature `device`): `/dev/binder` for andruna-lx in guest
//!   processes.

pub mod broker;
#[cfg(feature = "device")]
pub mod device;
pub mod host;
pub mod pack;
mod sys;
pub mod transport;
pub mod uapi;

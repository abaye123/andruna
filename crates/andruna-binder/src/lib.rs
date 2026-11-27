//! Binder without a kernel driver (ADR 0025).
//!
//! - [`uapi`]: the Linux binder interface (`linux/android/binder.h`).
//! - [`broker`]: the driver logic, run in sysd for all guest processes.
//! - [`pack`]: the guest side of the write and read streams.

pub mod broker;
pub mod pack;
pub mod uapi;

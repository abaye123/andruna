//! `andruna-lx`: the Linux layer.
//!
//! Provides the Linux system call and process semantics that AOSP code (ART,
//! libcore natives, bionic) expects, implemented on Windows. See
//! `docs/spec/03-components.md` (andruna-lx) for the design.
//!
//! # Layout
//!
//! - [`linux`]: the Linux/bionic ABI (constants, struct layouts with
//!   compile-time checks for x86_64 and aarch64, system call numbers).
//! - [`errno`]: per-thread `errno` and Windows error mapping.
//! - [`fd`]: the descriptor table and open file descriptions.
//! - [`fs`]: paths, the mount table, files and directories.
//! - [`mem`]: anonymous memory on Windows placeholders.
//! - [`futex`], [`time`], [`process`]: synchronization, clocks, identity.
//! - [`signal`]: the boundary for signal delivery (not implemented yet).
//! - `ffi`: the C exports with bionic names, each catching panics and
//!   reporting errors through `errno`.
//! - `sys`: the wrappers around Windows APIs, where the `unsafe` calls
//!   into Windows live.
//!
//! The Rust API ([`fs::mount::mount`], [`process::set_ids`], ...) is for
//! the host process that sets lx up.

pub mod errno;
pub mod fd;
mod ffi;
pub mod fs;
pub mod futex;
pub mod linux;
pub mod mem;
pub mod process;
pub mod properties;
pub mod sched;
pub mod signal;
mod sys;
pub mod time;
mod util;

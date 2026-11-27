//! `andruna-ipc`: the named-pipe protocol between clients (andruna-launch,
//! later the manager) and andruna-sysd.
//!
//! [architecture](../../docs/spec/02-architecture.md#ipc) puts a minimal
//! named-pipe protocol between the launcher and sysd. This crate holds both
//! halves of it so they cannot drift apart:
//!
//! - [`proto`]: versioned, length-prefixed messages and their encoding.
//! - [`pipe`]: endpoint names, the pipe's security descriptor, the server
//!   listener used by sysd and the client used by the launcher.
//! - `sys`: the Windows API wrappers; all `unsafe` code of the crate.
//!
//! See README.md for the message list.

pub mod pipe;
pub mod proto;
mod sys;

pub use pipe::{
    Client, Connection, DEFAULT_INSTANCE, Endpoint, Error, PipeListener, PipeSecurity, Waker,
};
pub use proto::{
    ErrorCode, Event, EventKind, MissingFunction, Notification, PROTOCOL_VERSION, Request,
    Response, Status, is_valid_activity_name, is_valid_package_name,
};
pub use sys::Sid;

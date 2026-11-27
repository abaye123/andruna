//! `andruna-linker`: the ELF loader and dynamic linker for Android ELF code
//! (`<arch>-linux-android`) in a Windows process (ADR 0013).
//!
//! It plays the role of bionic's `linker64`: it maps executables and
//! shared objects, binds and relocates them with bionic's semantics, lays
//! out ELF TLS on the guest thread pointer (S4), runs initializers, and
//! implements the `dlopen` family for guest code (`__loader_*`, which
//! bionic's `libdl.so` calls). Guest code reaches the host through a small
//! table of functions with the Android calling convention (System V on
//! x64, AAPCS64 on arm64), most importantly `__andruna_syscall`, which
//! bionic's system call stubs call instead of the `syscall`/`svc`
//! instruction and which forwards to andruna-lx.
//!
//! # Layout
//!
//! - [`elf`], [`reloc`], [`object`]: formats, validation, symbol lookup.
//! - [`image`]: mapping segments; [`linker`]: loading, binding,
//!   relocation, initializers.
//! - [`tls`]: thread pointer and TLS modules; [`bridge`]: the
//!   loader-side bionic state in `ld-android.so`.
//! - [`host`]: the host symbol table; [`syscall`], [`thread`], [`dl`],
//!   [`icu`]: the host functions; [`interim`], [`procfs`]: system calls
//!   the loader answers while andruna-lx does not implement them.
//! - [`process`]: starting a program; [`debug`], [`unwind`]: crash
//!   reports, debugger registration and Windows unwind tables.

/// The function-pointer type of a guest function: `gty!(fn(i32) -> i32)`.
#[cfg(target_arch = "x86_64")]
macro_rules! gty {
    ($($t:tt)*) => { extern "sysv64" $($t)* };
}
#[cfg(not(target_arch = "x86_64"))]
macro_rules! gty {
    ($($t:tt)*) => { extern "C" $($t)* };
}

/// Defines a host function that guest code calls, with the guest
/// convention (System V on x64; AAPCS64, which is Windows ARM64's
/// convention for these arguments, on arm64).
#[cfg(target_arch = "x86_64")]
macro_rules! gfn {
    ($(#[$m:meta])* $v:vis unsafe fn $name:ident($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)? $body:block) => {
        $(#[$m])* $v unsafe extern "sysv64" fn $name($($a: $t),*) $(-> $r)? $body
    };
}
#[cfg(not(target_arch = "x86_64"))]
macro_rules! gfn {
    ($(#[$m:meta])* $v:vis unsafe fn $name:ident($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)? $body:block) => {
        $(#[$m])* $v unsafe extern "C" fn $name($($a: $t),*) $(-> $r)? $body
    };
}

pub mod bridge;
pub mod debug;
pub mod dl;
pub mod elf;
pub mod error;
pub mod guestfs;
pub mod host;
pub mod icu;
pub mod image;
pub mod interim;
pub mod linker;
pub mod logd;
pub mod object;
pub mod process;
pub mod procfs;
pub mod reloc;
pub mod syscall;
pub mod thread;
pub mod tls;
pub mod unwind;

pub use error::{Error, Result};
pub use linker::{Config, Linker};

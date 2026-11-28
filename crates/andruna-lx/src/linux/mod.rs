//! The Linux and bionic ABI as seen by callers: constants, structure
//! layouts and system call numbers.
//!
//! Values come from the Linux UAPI headers that bionic ships
//! (`asm-generic/*.h`, `asm-x86/asm/*.h`, `asm-arm64/asm/*.h`) and from
//! bionic's own `libc/include` (read 2026-11-26). Where the two supported
//! architectures differ, both variants are always compiled so the layout
//! assertions for x86_64 and aarch64 are checked on every build, whichever
//! architecture is being built.
//!
//! All `long`, `size_t`, `off_t` and `time_t` values are 64-bit here, as in
//! bionic's LP64 ABI. Windows C code is LLP64 (`long` is 32-bit), so C
//! callers must reach these interfaces through declarations that use
//! 64-bit types where bionic uses `long`.

pub mod errno;
pub mod fcntl;
pub mod futex;
pub mod info;
pub mod mman;
pub mod poll;
pub mod signal;
pub mod stat;
pub mod syscall_nr;
pub mod time;

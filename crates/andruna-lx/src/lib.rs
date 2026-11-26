//! `andruna-lx`: the Linux layer.
//!
//! Provides the Linux system call and process semantics that AOSP code (ART,
//! libcore natives, bionic) expects, implemented on Windows. See
//! `docs/spec/03-components.md` for its interfaces. This crate is empty until
//! phase 1; spike S1 determines the first set of exports.

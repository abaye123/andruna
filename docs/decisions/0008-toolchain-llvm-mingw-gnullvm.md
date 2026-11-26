# 0008 - llvm-mingw for C/C++, Rust gnullvm targets

**Status:** accepted, 2026-11-26

## Context
AOSP is built with clang and assumes a GNU-like environment. Mixing ABIs
between AOSP code and Rust code invites subtle bugs.

## Decision
AOSP code is built with llvm-mingw (clang, UCRT). Rust uses the
`x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm` targets with the
same linker. Debug info is CodeView so PDBs work with Windows debuggers.

## Consequences
- One ABI across the process.
- A move to the MSVC ABI is reconsidered only if a spike shows a concrete
  problem.

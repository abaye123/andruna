# 0008 - llvm-mingw for C/C++, Rust gnullvm targets

**Status:** accepted, 2026-11-26; amended by [ADR 0013](0013-aosp-binary-format.md)

## Context
AOSP is built with clang and assumes a GNU-like environment. Mixing ABIs
between AOSP code and Rust code invites subtle bugs.

## Decision
AOSP code is built with llvm-mingw (clang, UCRT). Rust uses the
`x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm` targets with the
same linker. Debug info is CodeView so PDBs work with Windows debuggers.

## Amendment (ADR 0013)
AOSP code that runs in an app process is built with the same clang for
`<arch>-linux-android` as ELF and loaded by andruna-linker; its debug info
is DWARF, read by LLDB. Andruna's own code, andruna-lx and the Windows-facing
parts stay Windows PE code built as above.

## Consequences
- One ABI across the process.
- A move to the MSVC ABI is reconsidered only if a spike shows a concrete
  problem.

# 0013 - AOSP code is built as Android ELF

**Status:** proposed, 2026-11-26 (the maintainer decides)

## Context
AOSP code (ART, libcore natives, framework natives) was planned as Windows
code: compiled for `<arch>-w64-windows-gnu` against bionic's headers and
linked to andruna-lx exports (02-architecture, ADR 0008). The Windows
targets are LLP64, Android is LP64. S1 round 2 measured what that breaks:
`struct stat`, `statvfs`, `sigset_t`, `jmp_buf`, `pthread_t`,
`timespec::tv_nsec` and about 190 other `long` declarations in bionic's
headers, ART's x86 fault handler, `long double` and variadic calls on
arm64. Spike S8 built the same AOSP modules as Android ELF
(`<arch>-linux-android`) and ran them in a Windows process with a
prototype ELF loader ([S8 results](../plan/spike-results/S8.md)).

## Decision
AOSP code that runs inside an app process is compiled for
`<arch>-linux-android` as ELF shared objects with bionic's headers
unchanged, and is loaded by andruna-linker, the same loader that phase 5
needs for app native code. Calls from that code into andruna-lx use the
Android convention (System V on x64, AAPCS64 on arm64) through generated
entry points; lx keeps its Windows-convention exports for Rust and Windows
callers.

Andruna's own Rust and C# code, the Windows-facing parts (sysd's Windows
side, the manager, the launcher) and andruna-lx stay Windows PE code built
with llvm-mingw and the gnullvm targets (ADR 0008 for those).

## Consequences
- The LLP64 problem disappears: no widened-header patch, no audit of
  `long` in each AOSP project, `long double` and variadics on arm64 are
  Android's. Layers 1 and 2 and 223 of 238 libart sources compiled as ELF
  without a source change.
- andruna-linker moves from phase 5 to phase 1, with ELF TLS on the thread
  pointer (S4), RELR, packed relocations, TLSDESC, init order, namespaces
  later. The S8 prototype loader is about 1,900 lines of Rust (with the
  unwind translation), plus the host side of its libc shim.
- Phase 0 and 1 exit criteria are unchanged; the roadmap gains the
  loader in phase 1 and loses it from phase 5's list of new components.
- Every guest-to-lx call crosses a convention boundary. Measured on x64:
  about 0.45 ns per call over a direct call (S8.md, "Boundary cost").
- Debugging guest code needs a DWARF debugger (LLDB, which shows guest
  symbols, lines and stacks after the loader registers the objects) and,
  for Windows unwinding and Windows debuggers, `.eh_frame` translated to
  Windows unwind tables, which S8 prototyped; WinDbg then shows correct
  stacks but no ELF symbols. 02-architecture's calling
  convention table and ADR 0008's "CodeView so PDBs work" change for AOSP
  code.
- The guest needs a libc for ELF code: S4's real bionic routed to lx (S8
  used a shim). bionic and libc++ are built for Android once, not patched
  for LLP64.
- Option 1 (Windows target, widened headers) remains the fallback if the
  loader or the debugging story fails in phase 1.

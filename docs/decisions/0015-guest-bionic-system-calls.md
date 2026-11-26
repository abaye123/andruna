# 0015 - Real bionic for guest code, system calls through the loader

**Status:** proposed, 2026-11-26

## Context
ADR 0013 builds AOSP code as Android ELF loaded by andruna-linker, and
needs a libc for that code. S4 recommended real bionic (option A) over a
shim; S8 ran with a shim. bionic's system call stubs execute `syscall` /
`svc`, which on Windows reach the NT kernel, not Linux. bionic also expects
its dynamic linker to own the main thread set-up, the static TLS layout and
the TLS module table (`__libc_shared_globals`), and creates threads with a
raw `clone`.

## Decision
- Guest code uses AOSP's bionic (`libc.so`, `libm.so`, `libdl.so`) built
  for `<arch>-linux-android`, with scudo as the allocator and LLVM libunwind
  exported from `libc.so`, as Android does.
- Every generated system call stub calls the host function
  `__andruna_syscall` instead of the instruction: the number in `rax` / `x8`,
  arguments in the C argument registers, the raw kernel result returned. The
  stubs are generated from bionic's `SYSCALLS.TXT` with bionic's own
  generator and Andruna's templates (`aosp/tools/gensyscalls_andruna.py`).
  andruna-linker handles the calls about threads, the thread pointer and
  process lifetime and forwards the rest to andruna-lx, which answers
  `ENOSYS` for calls it does not implement.
- Hand-written bionic assembly that issues system calls (`syscall()`,
  `__bionic_clone`, `_exit_with_stack_teardown`, `vfork`, the signal return
  trampoline) is replaced by Andruna sources in `aosp/bionic/`; threads are
  created by `__andruna_clone_thread` on Windows threads running on bionic's
  stacks.
- bionic's loader-side state lives in a bridge object installed as
  `ld-android.so`, built from bionic's sources (`aosp/bionic/ld_android_bridge.cpp`
  with `libc_nomalloc`), which andruna-linker calls where bionic's linker
  does the same work.
- bionic patches stay small and gated on `ANDRUNA`: thread pointer and
  shadow call stack on arm64 (x18 stays the TEB), the signal trampoline.
- Unicode properties that Android computes with ICU4X in Rust inside
  `libc.so` are host functions answered by Windows' ICU.

## Consequences
- Guest code gets bionic's exact stdio, locale, malloc, pthread and
  `errno` behavior; what lx lacks shows up as honest `ENOSYS` failures.
- Every system call costs one call into the host (S8: about 0.45 ns over a
  direct call), no trap.
- Raw `syscall(SYS_clone, ...)`, `fork` and `vfork` fail with `ENOSYS`.
- Unicode classification follows Windows' Unicode version, which can differ
  from Android's for recently added characters.
- The bridge and the patches must be refreshed with every AOSP tag change.

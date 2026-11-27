# 0015 - ART's x64 thread register is the bionic TLS slot behind FS

**Status:** accepted, 2026-11-27

## Context
ART on x86_64 sets the GS base to the current `Thread*`
(`arch_prctl(ARCH_SET_GS)`) and its assembly entry points, nterp and
compiled code read Thread fields as `%gs:offset`. On Windows x64 GS is the
TEB. User mode can write the GS base on current CPUs, but Windows code on
the same thread needs GS to be the TEB, and a context switch restores it
without a fault, so a swapped GS base cannot be repaired lazily the way
the FS base is (S4). ARM64 is not affected: ART keeps the Thread in x19.
Spike S1 round 3 ran ART on x64 with the alternative below
([S1 results](../plan/spike-results/S1.md#the-x64-thread-register-round-2-risk-1)).

## Decision
On x64, ART reads the `Thread*` from bionic's `TLS_SLOT_ART_THREAD_SELF`,
which ART already fills for `Thread::Current()` and which is at `%fs:0x38`
(FS base = the guest thread pointer, with lazy restore, S4). Small ART
patches gated on `ANDRUNA` (`patches/art/`):
- assembly entry points: `Thread::Current()` is one FS-relative load;
  other fields go through a register that is dead at that point;
- the x86_64 assembler: `gs()` loads the Thread\* into R11 (the code
  generators' reserved temporary) and rebases the next absolute operand on
  it, so compiled code (JIT, and dex2oat when built) needs no call-site
  changes except where R11 is an input;
- nterp is ported the same way; until then it is disabled on x64.

## Consequences
- A dependent load of a Thread field costs one extra L1 load (measured
  +0.17 ns); stores and `Thread::Current()` cost the same as on Android.
- The patches touch ART's x86_64 back end and stubs and are carried for
  every AOSP tag (about 250 lines today, nterp to come).
- Compiled code depends on the FS-base lazy restore: an `%fs:` access
  after a context switch takes one fault, as bionic code does.
- A `%gs:` access left in x86_64 code reads the TEB, not the Thread (the
  JNI trampolines passed TEB+0xd8 as the `Thread*` until patch 0005): the
  art stage of `aosp/tools/build_guest.py` fails on any outside nterp.
- Rejected: a dedicated thread register in ART's x86_64 back end (a
  register allocator change), and swapping the GS base at boundaries
  (silently wrong after a context switch).

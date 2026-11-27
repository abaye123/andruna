# 0014 - Garbage collection: concurrent copying with Baker read barriers

**Status:** accepted, 2026-11-26

## Context
ART's device default collector, CMC (concurrent mark-compact), needs
`userfaultfd` and `mremap(MREMAP_DONTUNMAP)`. Windows has neither. Without
them CMC falls back to a stop-the-world compaction of the whole heap. Spike
S2 prototyped a `userfaultfd` substitute on Windows primitives: it is
correct, but the flip costs about 10 ms per 64 MiB of heap and every mutator
fault costs microseconds
([S2 results](../plan/spike-results/S2.md), "Garbage collection").

## Decision
ART is built with Baker read barriers (`ART_USE_READ_BARRIER=1`,
`ART_READ_BARRIER_TYPE_IS_BAKER=1`). With `userfaultfd` missing, the runtime
then selects CC (concurrent copying). The boot image, dex2oat output and JIT
code are all compiled with read barriers. andruna-lx reports `userfaultfd`
as `ENOSYS` and fails `mremap(MREMAP_DONTUNMAP)` cleanly. No `userfaultfd`
emulation is built.

Until the read-barrier build lands, bring-up builds may run CMC in its
stop-the-world fallback.

## Consequences
- Pauses do not grow with heap size (thread flip and checkpoints only).
- Every reference load in compiled code carries the read-barrier fast path;
  S2 measured +1% to +14% per load in a microbenchmark. Real apps are
  measured in S2 round 2, which confirms or reverts this decision.
- The heap needs headroom for evacuation.
- An emulation of `userfaultfd` is reconsidered only if the read-barrier
  cost on real apps is significant, and then with a redesigned flip.

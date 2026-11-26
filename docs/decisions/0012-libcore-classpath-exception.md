# 0012 - libcore under GPL-2.0 with the Classpath exception

**Status:** accepted, 2026-11-26

## Context
ART cannot run applications without libcore, the core Java library of
Android. Part of libcore is derived from OpenJDK and is licensed GPL-2.0
with the Classpath exception. ADR 0006 allows only permissive licenses, and
GPL code is not allowed in the repository.

## Decision
libcore is allowed as a named exception to ADR 0006:
- It is fetched from AOSP at the pinned tag, like every other AOSP project,
  and never copied into the repository.
- Changes are small patches in `patches/libcore/`, gated on `ANDRUNA`, as for
  any AOSP project. Patches to its GPL files are under the same license as
  those files.
- Andruna's own code only links to libcore and calls it through its public
  Java and JNI interfaces, which is what the Classpath exception covers.
- No other GPL code is allowed by this decision.

## Consequences
- libcore is listed in `THIRD_PARTY_NOTICES` with its license and the
  exception text when it is built.
- The future license of Andruna must be compatible with shipping libcore
  under its own license next to Andruna's code.

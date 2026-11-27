# 0006 - Open source; permissive dependencies until the license is chosen

**Status:** accepted, 2026-11-26

## Decision
Andruna is open source. The exact license will be chosen later. Until then,
only permissively licensed third-party code is used (Apache-2.0, MIT, BSD,
0BSD, zlib, ISC, and public domain with an explicit dedication), so every
licensing option stays open. No code is copied
from GPL, LGPL or AGPL projects, or from projects with unclear licensing.

## Amendment, 2026-11-26
Public domain code with an explicit dedication in its source (for example
dlmalloc and the LZMA SDK used by AOSP) is allowed. It is recorded in
`THIRD_PARTY_NOTICES` like any other dependency.

## Amendment, 2026-11-27
The Windows App SDK (WinUI 3 and its runtime parts), under Microsoft's
redistributable license terms, is allowed as a named exception: it is the
platform of the management app chosen in ADR 0005 and has no permissive
alternative. It ships only with the management app.

## Amendment, 2026-11-27 (ADR 0018)
The project license is chosen: AGPL-3.0-only for Andruna's own code, with
contributions under a CLA. The dependency policy above does not change:
third-party code stays permissive, so the project owner keeps the option of
other licensing terms.

## Consequences
- `cargo deny` and a notices file are enforced in CI.
- Contribution terms (CLA or DCO) must be decided before outside
  contributions are accepted.

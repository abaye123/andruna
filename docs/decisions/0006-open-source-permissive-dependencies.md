# 0006 - Open source; permissive dependencies until the license is chosen

**Status:** accepted, 2026-11-26

## Decision
Andruna is open source. The exact license will be chosen later. Until then,
only permissively licensed third-party code is used (Apache-2.0, MIT, BSD,
0BSD, zlib, ISC), so every licensing option stays open. No code is copied
from GPL, LGPL or AGPL projects, or from projects with unclear licensing.

## Consequences
- `cargo deny` and a notices file are enforced in CI.
- Contribution terms (CLA or DCO) must be decided before outside
  contributions are accepted.

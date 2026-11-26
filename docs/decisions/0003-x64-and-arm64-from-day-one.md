# 0003 - x64 and ARM64 from day one

**Status:** accepted, 2026-11-26

## Context
Most Android apps with native code ship arm64 libraries, many only those. On
Windows on ARM they can run natively; on x64 they need translation.

## Decision
Every component is built and tested for x64 and ARM64 Windows from the first
commit. arm64 guest code runs natively on Windows on ARM. On x64, arm64 guest
code is translated (`andruna-xlat`).

## Consequences
- CI needs ARM64 Windows runners.
- Calling-convention and register differences between Android arm64 and
  Windows ARM64 must be handled (spike S4).

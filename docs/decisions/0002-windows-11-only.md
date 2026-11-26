# 0002 - Windows 11 only for the first release

**Status:** accepted, 2026-11-26

## Context
Windows 10 support costs a second visual design (no Mica, rounded corners or
Snap Layouts), fallbacks for Windows 11-only APIs (the compositor clock),
no x64 emulation on Windows 10 on ARM, an older CPU baseline, and a doubled
test matrix. Windows 10 is out of mainstream support.

## Decision
The first release supports Windows 11 only, x64 and ARM64. Windows 10 is
reconsidered after the beta.

## Consequences
- Windows 11 APIs may be used without fallbacks.
- x64 builds may assume x86-64-v2 (SSE4.2, POPCNT), which Windows 11 24H2
  requires.
- WebView2 can be assumed present.

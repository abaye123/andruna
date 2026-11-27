# 0026 - The AIDL compiler as a host build tool, with bison and flex

**Status:** proposed, 2026-11-27

## Context
libbinder and servicemanager (frameworks/native) are built from C++
sources plus C++ code that Soong generates from `.aidl` files with the
AIDL compiler (`libbinder_aidl`: `IServiceManager`, `IServiceCallback`,
`IClientCallback`, `ConnectionInfo`, `Service`, `ServiceWithMetadata`,
`ServiceDebugInfo`; `IAccessor`; the binder stats consumer). The framework
build needs the same compiler for far more interfaces. The compiler
(system/tools/aidl, Apache-2.0) is a host program that AOSP also builds
for Windows; its lexer and parser are generated with flex and bison at
build time. This machine has neither, and running a downloaded tool
binary is a maintainer decision.

## Decision (proposed)
- Fetch `system/tools/aidl` at the pinned tag (excluding `tests/`, whose
  golden files have paths longer than Windows allows) and build the
  `aidl` executable for the Windows host with llvm-mingw, with the host
  (Windows) variants of libbase and liblog it links, in a separate host
  stage of `aosp/` (not the guest build).
- bison (3.x) and flex are build-time tools only, taken from PATH or
  `ANDRUNA_BISON` / `ANDRUNA_FLEX`: on CI from the runner's package
  manager (Chocolatey `winflexbison3`, or MSYS2 `bison` and `flex`), on
  developer machines the same. Their output is not GPL-encumbered (bison's
  parser skeleton exception; flex's skeleton is BSD-licensed), and nothing
  of them is shipped.
- Generated C++ is never committed; it is produced in `out/` from the
  pinned `.aidl` files, as Soong does.

## Consequences
- Two new build-time tools on developer machines and CI.
- The guest build of libbinder, servicemanager and later the framework's
  native code can follow Soong's `aidl` rules exactly (`--lang=cpp` and the
  same flags), instead of hand-written stubs.
- Until this is decided, libbinder and servicemanager are not built; the
  binder device is tested with guest programs that speak the protocol
  directly (`crates/andruna-binder-device/tests/guest`).

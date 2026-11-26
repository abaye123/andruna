# Build, test and CI

## Repository layout

```
crates/          Rust workspace (andruna-*)
aosp/            CMake targets mirroring Android.bp modules, lock file
patches/         Patches to AOSP projects, per project
java/            Andruna's Java code (framework service clients, glue)
ui/              Andruna.Manager (C#, WinUI 3)
installer/       Installer definition
tests/           Integration, conformance and app tests
bench/           Benchmarks
docs/            Specs, plans, ADRs
xtask/           Build orchestration (Rust)
```

## Toolchain

- **C/C++ (AOSP):** llvm-mingw (clang, UCRT), pinned by release and checksum,
  targets `x86_64-w64-windows-gnu` and `aarch64-w64-windows-gnu`. Debug info
  as CodeView (`-gcodeview`) so PDBs work with WinDbg and Visual Studio.
- **Rust:** pinned toolchain, targets `x86_64-pc-windows-gnullvm` and
  `aarch64-pc-windows-gnullvm`, linked with the same llvm-mingw.
- **Java:** a pinned JDK for javac; D8, aapt2 and aidl as pinned prebuilt
  tools.
- **C#:** .NET SDK and Windows App SDK, pinned.

One command builds everything: `cargo xtask build [--arch x64|arm64]
[--release]`. It fetches AOSP at the pinned tag (cached), applies patches,
builds AOSP modules with CMake + Ninja, builds the Rust workspace, the Java
parts, the manager, and the installer.

Local builds are supported on Windows (and on Linux/WSL for cross builds).
With `sccache` and the AOSP cache, an incremental rebuild of a Rust crate
takes minutes.

## Code signing

Every `.exe`, `.dll` and the installer are signed in the packaging step with
SHA-256 digests and an RFC 3161 timestamp. The certificate is selected by
thumbprint from the signing machine's certificate store; the thumbprint is a
build setting, never committed with a private key. Unsigned builds are for
local development only.

## Test layers

| Layer | What | Where |
| --- | --- | --- |
| Unit | Each crate's tests | Every PR, both architectures |
| Lx conformance | The same test binary on Linux and on Windows; results compared | Every PR |
| AOSP suites | bionic tests, ART run-tests and gtests, libcore tests (selected) | Every PR (fast subset), nightly (full) |
| CTS modules | Selected modules (for example os, content, view, graphics, widget, libcore) with tracked pass rates | Nightly; pass rate may only go up |
| App sweep | An automated set of open-source apps: install, start, first frame, input reaches the app, no crash for 60 s | Nightly |
| Benchmarks | [Performance](05-performance.md) benchmark set | Every merge to main, reference machines |
| Fuzzing | Binder, ELF, APK/zip, IPC parsers | Continuous |
| Manual | Release checklist incl. Google tier 2 and hardware (camera, Bluetooth, pen) | Each release |

## CI

- Hosted runners: Windows x64 and Windows ARM64 (verify availability of
  hosted ARM64 Windows runners; otherwise a self-hosted ARM64 runner).
- GPU tests need a real GPU: a self-hosted reference machine per
  architecture.
- A merge to main requires: build on both architectures, unit and
  conformance tests, the fast AOSP subset, no benchmark regression, license
  check (`cargo deny`), formatting and lints.

## Definition of done for a task

1. The behavior is implemented on the production path, on x64 and ARM64.
2. Tests exercise it; failure cases are covered.
3. CI is green.
4. Docs and ADRs are updated.
5. The final report says what was verified, how, and what was not.

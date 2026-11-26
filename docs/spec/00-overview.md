# Overview

## Vision

A private user double-clicks an `.apk` on a Windows 11 PC, and the app opens
in a normal window within a second or two, looks right, responds instantly,
and behaves the way it does on a phone. Nothing runs in the background when no
app is open.

## The approach in one paragraph

Andruna does not rewrite Android and does not boot it. It builds the Android
Open Source Project's own runtime (ART, libcore) and framework
(`framework.jar`, HWUI, Skia) for Windows, and replaces only the layers that
talk to the Linux kernel and to phone hardware. Those replacement layers are
Andruna's own code, written in Rust: a Linux ABI layer, an ELF linker, a
Binder implementation, system services backed by Windows, a window
compositor, a process launcher, and developer tooling. The management app and
system dialogs are WinUI 3.

The bar is set high from the start for performance, desktop integration,
security and code quality.

## Goals

| Goal | Measure |
| --- | --- |
| Fast | Targets in [performance](05-performance.md): warm start under 400 ms, cold start under 1.5 s, frame pacing at the display's refresh rate |
| Light | Under 60 MB private memory per app process beyond the app itself; nothing running with no app open |
| Feels like Windows | Every item in [product: desktop integration](01-product.md#desktop-integration) |
| Safe | Every app in its own AppContainer, by default |
| Compatible | Selected CTS modules and an automated F-Droid sweep, tracked per release |
| Both architectures | x64 and ARM64 Windows built and tested from the first commit |
| Honest | The [compatibility policy](06-compatibility-policy.md) holds for every change |

## Non-goals

- An app store of our own, or distributing Google's apps.
- Faking device certification: Play Integrity, SafetyNet, Play Protect.
- DRM (Widevine) and in-app purchases through Google Play Billing.
- Apps that need root, kernel drivers or a custom kernel.
- 32-bit ARM (`armeabi-v7a`) and 32-bit x86 native code.
- Windows 10 in the first release.
- Reimplementing ART or the Android framework.

## Decisions so far

| # | Decision | ADR |
| --- | --- | --- |
| 1 | AOSP's ART and framework, bridged to Windows by Rust code | [0001](../decisions/0001-architecture-aosp-core-rust-bridge.md) |
| 2 | Windows 11 only for the first release | [0002](../decisions/0002-windows-11-only.md) |
| 3 | x64 and ARM64 from day one | [0003](../decisions/0003-x64-and-arm64-from-day-one.md) |
| 4 | Google services in three tiers | [0004](../decisions/0004-google-services-three-tiers.md) |
| 5 | WinUI 3 for the management app and system dialogs | [0005](../decisions/0005-winui3-management-app.md) |
| 6 | Open source; permissive dependencies only until the license is chosen | [0006](../decisions/0006-open-source-permissive-dependencies.md) |
| 7 | AppContainer isolation by default | [0007](../decisions/0007-appcontainer-by-default.md) |
| 8 | llvm-mingw toolchain, Rust `gnullvm` targets | [0008](../decisions/0008-toolchain-llvm-mingw-gnullvm.md) |
| 9 | One maintainer, AI agents write the code | [0009](../decisions/0009-development-process.md) |
| 10 | Target users are private users | [0010](../decisions/0010-target-users.md) |

## Terms

| Term | Meaning |
| --- | --- |
| AOSP | Android Open Source Project, pinned to one release tag |
| ART | Android Runtime: the Java runtime with interpreter, JIT and AOT compiler |
| App process | The Windows process that runs one Android application package |
| Host process | A pre-warmed app process before it is assigned to a package |
| System server | Andruna's process for system services (`andruna-sysd`) |
| Lx layer | Andruna's implementation of the Linux interfaces AOSP code uses |
| Guest code | An app's native code, in ELF format |
| WoA | Windows on ARM |

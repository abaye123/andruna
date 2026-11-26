# Roadmap

Each phase ends with something demonstrable, built and tested in CI on x64
and ARM64. A phase is done when every exit criterion is verified; the
maintainer closes it.

| Phase | Goal | Exit criteria |
| --- | --- | --- |
| **0** | Foundations and spikes | Repo, CI on both architectures, toolchain, AOSP fetch and patch flow; spikes S1 to S4 answered with written results |
| **1** | Runtime core | `andruna-linker` loads AOSP code built as Android ELF (ADR 0013) with bionic; `andruna-app` runs Java programs on ART with JIT and AOT on both architectures; selected libcore tests pass; lx conformance suite green |
| **2** | Headless framework and system server | sysd with servicemanager and Binder over shared memory; a test APK is installed (parsed, verified, dex2oat) and its Activity reaches `onResume` headless; Binder round trip meets its target |
| **3** | Windows, graphics and input | HWUI draws through ANGLE into a DirectComposition window; freeform resize with configuration changes; mouse, touch, keyboard, IME (Hebrew); several pure-Java open-source apps usable end to end |
| **4** | Desktop product preview | Installer, `andruna-launch`, Start menu, manager app (library, app page, settings), notifications, clipboard, file pickers, AppContainer isolation on, crash dialog and diagnostics; spikes S6 and S7 done; **first public preview** |
| **5** | Native code | App native libraries through `andruna-linker`; x64 native libraries; arm64 native on WoA; arm64 translation on x64 (AOT + JIT); NativeActivity games run |
| **6** | Media and devices | Audio out/in with low latency, MediaCodec on Media Foundation, video zero-copy, camera, location, sensors, Bluetooth LE, WebView on WebView2, Windows Hello, passkeys |
| **7** | Google services | Account sign-in, tier 1 (microG), tier 2 (experimental) with verification and setup wizard |
| **8** | Beta | Performance targets met on reference machines; CTS module pass rates and app sweep results published; Hebrew and English user docs; self-update |

## Order and parallelism

- Phases 0 to 2 are mostly sequential: each depends on the previous one.
- From phase 3, tracks can run in parallel: graphics/input, product
  (installer, manager), native code, media/devices. Each track has one
  owner module and its own tests.
- The manager app (C#) can start in phase 2 against a mocked sysd interface,
  as long as the mock lives only in its tests.
- Maintainer decision, 2026-11-27: the product skeleton (sysd, launcher,
  manager, installer, signing) starts now, in parallel with the remaining
  phase 0 and 1 work. It shows honest "not available yet" states until the
  runtime parts land.

## Working with agents

- One task = one module + explicit exit criteria + the tests that prove them.
- Every task ends with a report: what was verified and how, what was not,
  edits outside the module.
- The maintainer reviews against the
  [compatibility policy](../spec/06-compatibility-policy.md) first, then the
  evidence.

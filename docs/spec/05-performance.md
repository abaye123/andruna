# Performance

## Targets

Targets are provisional until spike results are in; each is measured in CI on
fixed hardware (one x64, one ARM64 reference PC) and reported per release.

| Metric | Target | Measured how |
| --- | --- | --- |
| Cold start, simple app (no process, boot image not in page cache) | < 1.5 s to first frame | ETW: launcher start to first `Present` |
| Warm start (sysd running, host process available) | < 400 ms to first frame | Same |
| `andruna-launch.exe` start | < 20 ms | ETW |
| Private memory per app process beyond the app | < 60 MB | Private working set after first frame, minus the app's Java heap and native allocations |
| Binder round trip, small parcel | < 20 µs | Microbenchmark, two processes |
| Frame pacing | Display refresh rate, including 120 Hz and VRR; < 1% missed frames in the scrolling benchmark | Frame timing from the compositor clock |
| Input latency (mouse move to pixel) | One frame in the normal case | Hardware-assisted test on the reference PC |
| Audio output latency (low-latency mode) | < 20 ms | Loopback test |
| Idle cost with no app open | 0 processes, 0 CPU | Process list after the idle timeout |

## Design levers

### Start and memory

- **Shared boot image.** Built once at install or update, mapped into every
  app process from one section at one address, so pages are shared.
- **Pre-initialized framework.** Host processes run the class preloading that
  zygote does, before a package is assigned.
- **AOT at install** with the app's baseline profile (`assets/dexopt/`);
  background recompilation with the collected profile when the PC is idle
  and on mains power.
- **Lazy services.** sysd starts a service on its first lookup.
- **GC.** ART's concurrent mark-compact collector relies on `userfaultfd`.
  Spike S2 decides between an available collector without it and an
  emulation on Windows memory primitives.

### Graphics and video

- Each surface is a DirectComposition visual with a flip-model swap chain:
  no extra GPU copy for composition.
- Hardware video decode into D3D11 textures shared with ANGLE: zero copies
  from decoder to screen.
- ANGLE on D3D11 for HWUI by default; Vulkan for apps that use Vulkan
  directly, on the real driver.
- Shader caches persisted per app.

### Native code

- x64 guest code runs natively; arm64 runs natively on WoA.
- arm64 on x64: ahead-of-time translation in the background, cached and
  shared; JIT only for code not yet translated; system libraries never
  translated.

### Files

- Path translation cached; long paths supported.
- Case-sensitive directories for app data.
- Measure Microsoft Defender's impact on app data I/O; document Dev Drive as
  an option for advanced users.

## Benchmarks in CI

A benchmark app set (start time, scrolling list, Binder, file I/O, audio
loopback, video playback) runs on every merge to main on the reference
machines. A regression beyond the noise threshold (set from 10 baseline
runs) blocks the merge.

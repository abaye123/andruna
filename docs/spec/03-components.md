# Components

Each component has one owner module, a stated interface, and tests. Rust
crates live in `crates/`, C# projects in `ui/`, AOSP build glue in `aosp/`.

| Component | Kind | Language | Phase |
| --- | --- | --- | --- |
| `andruna-lx` | Library (static) | Rust | 0 |
| `andruna-linker` | Library | Rust | 5 |
| `andruna-bionic` | Library | AOSP C + Rust glue | 5 |
| `andruna-binder` | Library + sysd module | Rust | 2 |
| `andruna-sysd` | Executable | Rust + Java services | 2 |
| `andruna-compositor` | sysd module | Rust | 3 |
| `andruna-app` | Executable (app/host process) | Rust + AOSP | 1 |
| `andruna-launch` | Executable (tiny) | Rust | 4 |
| `andruna-pkg` | sysd module | Rust + framework parser | 2 |
| `andruna-xlat` | Library + worker exe | Rust | 5 |
| `andruna-media` | sysd module + app library | Rust | 6 |
| `andruna-devices` | sysd module | Rust | 6 |
| `andruna-adbd` | sysd module | Rust | later |
| `Andruna.Manager` | WinUI 3 app | C# | 4 |
| `aosp/` | CMake build of AOSP modules + patches | CMake | 0 |

## andruna-lx: the Linux interfaces

**Responsibility.** Provide the Linux and bionic interfaces that ART, libcore,
the framework natives and guest code use, implemented on Windows.

**Interface.** C exports with bionic names and Linux struct layouts
(`mmap`, `open`, `epoll_wait`, `futex` via `syscall`, `sigaction`, ...).
Headers: bionic's own UAPI headers where possible, plus a small overlay.

**Design.**
- File descriptor table owned by lx, independent of the CRT. Each fd is a
  typed object: file, directory, pipe, socket, eventfd, timerfd, epoll,
  memfd/ashmem, binder, inotify.
- Memory: `mmap` on `VirtualAlloc2`/`MapViewOfFile3` with placeholders, so
  fixed-address and partial mappings work at 4 KiB granularity where Windows
  allows, with documented fallbacks where it does not (file mappings need 64
  KiB alignment).
- Signals: delivered from Vectored Exception Handlers (SIGSEGV, SIGBUS, SIGILL,
  SIGFPE) with a Linux `ucontext_t`; ART's fault handler and libsigchain work
  unchanged. Asynchronous signals (thread suspension) by `SuspendThread` plus
  context injection, as ART needs for checkpoints, or ART's cooperative
  suspension only (decided in spike S1).
- `futex` on `WaitOnAddress`/`WakeByAddress*`.
- `epoll` on I/O completion ports and wait packets (`NtAssociateWaitCompletionPacket`), so sockets, pipes, events and timers share one wait.
- Paths: Android paths mapped to Windows paths through a mount table; illegal
  Windows characters and reserved names escaped reversibly; case-sensitive
  directories enabled for app data.
- `/proc` and `/sys`: synthesized for the entries apps and AOSP read
  (`self/maps`, `self/status`, `cpuinfo`, `meminfo`, ...), describing the
  real PC.

**Tests.** A conformance suite that runs the same test binary on Linux and on
Windows and compares results; bionic's own tests.

## andruna-linker: ELF loader and dynamic linker

**Responsibility.** Load guest `.so` files and executables, link them against
bionic and Andruna's system libraries, implement `dlopen` family and
`android_dlopen_ext`, namespaces as the native loader expects.

**Design.** Bionic linker semantics (load order, symbol lookup order,
`RTLD_*`, `DT_RUNPATH`, packed relocations, RELR, IRELATIVE, ELF TLS and
TLSDESC, `PT_GNU_RELRO`). Presented to apps as `/system/bin/linker64`.
Generates thunks at ABI boundaries. Exposes `dl_iterate_phdr` and unwind info
so C++ exceptions and `libunwind` work in guest code.

## andruna-bionic

**Decision pending (spike S4).** Option A: build AOSP's real bionic for the
guest and route its system calls to `andruna-lx`. Option B: a shim that
implements bionic's exports on Windows.
Option A gives Android-exact libc behavior (stdio, locale, malloc) and less
code to maintain; its risk is thread-local storage (bionic expects `%fs` on
x64 and `TPIDR_EL0` on arm64).

## andruna-binder

**Responsibility.** `/dev/binder` semantics for libbinder (C++), the Java
Binder, and Rust (`binder` crate) without a kernel driver.

**Design.**
- Each process has a receive buffer in a shared section mapped by sysd and
  by the process. A transaction is copied once, by the sender's call into the
  broker logic, into the receiver's buffer, as the kernel does.
- Thread selection, nested transactions, reference counting and death
  notifications follow the kernel driver's rules.
- Object translation and fd passing (`DuplicateHandle` into the receiver)
  happen in sysd, which knows each peer's AppContainer SID and package uid:
  `getCallingUid`/`getCallingPid` come from there, never from the sender.
- libbinder keeps its kernel IPC code; `andruna-lx` provides the device.

**Target.** A small round trip under 20 microseconds
([performance](05-performance.md)).

## andruna-sysd: system server

**Responsibility.** The services an app looks up through `ServiceManager`.

**Design.**
- servicemanager from AOSP.
- Services whose logic is the framework's own stay Java, running in a Java
  runtime inside sysd, using framework code where possible
  (`PackageParser2`, `ActivityTaskManager` logic where it fits). Hardware and
  Windows-facing services are Rust, implementing the AIDL interfaces through
  the Rust AIDL backend.
- Each service documents which AIDL methods it implements; an unimplemented
  method returns Android's own "unsupported" behavior and is counted (see
  "missing function counters" below).

Initial service list: `package`, `activity`, `activity_task`, `window`,
`display`, `input`, `input_method`, `user`, `appops`, `permission`,
`notification`, `alarm`, `jobscheduler`, `connectivity`, `wifi` (read-only),
`location`, `sensorservice`, `audio`, `media.*`, `camera`, `clipboard`,
`account`, `content`, `settings` provider, `uimode`, `power`, `vibrator`
(no-op only if the PC really has none and Android reports none),
`batterystats`/`battery` (real battery or none).

## andruna-compositor

**Responsibility.** What SurfaceFlinger and WindowManager do on a device:
one Windows window per task, one DirectComposition visual per surface.

**Design.**
- Each `Surface` is a flip-model swap chain or a composition surface
  attached to a DComp visual; SurfaceView and video get their own visuals,
  composited by DWM without extra copies.
- Vsync for Choreographer from the compositor clock of the monitor the
  window is on.
- Freeform windowing config: window bounds, insets (title bar as caption
  inset), density per monitor, `resizeableActivity` and minimum sizes.
- Splash, letterboxing, back button and title bar color (D-2 to D-4).

## andruna-app: app and host process

**Responsibility.** The executable for app and host processes: create the
runtime, map the boot image, attach a package, run the main Looper.

**Design.** Rust `main` that loads ART as a library, sets up `andruna-lx`,
maps the boot image from the shared section, creates the runtime, and
pre-initializes framework classes (like zygote's preload). Accepts a package
assignment from sysd.

## andruna-launch

Tiny Rust executable used by shortcuts and file associations. Talks to sysd's
named pipe; starts sysd if needed. Must start in under 20 ms.

## andruna-pkg: package manager backend

APK, split and bundle handling; signature verification (apksig semantics,
implemented in Rust or using the framework's verifier in sysd); ABI and split
selection; dex2oat at install with the baseline profile; Start menu
shortcut, icon (`.ico` from the app's adaptive icon), AppUserModelID and
uninstall entry.

## andruna-xlat: arm64 on x64

**Responsibility.** Run arm64-only native code on x64 PCs.

**Design.**
1. Ahead-of-time translation in the background: functions discovered from
   symbols, `.eh_frame_hdr` and call targets; each translated to LLVM IR
   with guest registers as locals; compiled with full optimization; cached
   by library hash and CPU features; shared between apps.
2. A JIT (dynarmic, 0BSD) for code not yet translated.
3. Calls to system libraries (libc, libm, GLES, libandroid, ...) leave
   translated code and run native x64 implementations.

On WoA this crate is not used: arm64 code runs natively.

## andruna-media and andruna-devices

| Android API | Windows backend |
| --- | --- |
| AudioTrack, AAudio, OpenSL ES | WASAPI shared mode, `IAudioClient3` low-latency periods |
| AudioRecord | WASAPI capture, with Windows microphone privacy |
| MediaCodec / NDK AMediaCodec | Media Foundation hardware decoders and encoders into D3D11 textures; software codecs from AOSP where MF lacks one |
| MediaPlayer, ExoPlayer paths | Through MediaCodec as on Android |
| Camera2 / CameraX | Media Foundation capture (`IMFCaptureEngine`/frame server), real characteristics of the real camera |
| Location | `Windows.Devices.Geolocation` |
| Sensors | `Windows.Devices.Sensors` (only sensors the PC really has) |
| Bluetooth | WinRT Bluetooth LE and RFCOMM APIs |
| Biometrics | Windows Hello |
| Passkeys / Credential Manager | Windows WebAuthn |
| WebView | WebView2, behind Android's WebView API |
| Notifications | Windows App SDK app notifications |

## andruna-adbd (later)

An adbd-protocol endpoint on localhost: install, shell (a small Android-like
shell), logcat, JDWP forwarding for debugging.

## Andruna.Manager (WinUI 3)

C#, Windows App SDK, unpackaged deployment (installed by our installer).
Talks to sysd through Binder. Also hosts system dialogs (permissions,
install confirmation, account chooser, crash dialog) so they look native.

## Cross-cutting: missing function counters

Every stubbed or unimplemented entry point (lx export, guest import, service
method) increments a counter. On process exit, the log ends with the counted
entries, most-called first. The management app's Diagnostics page shows them
for the app. This tells us which missing piece apps actually need.

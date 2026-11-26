# Architecture

## Processes

```
                    Windows 11 session (one user)
 ┌──────────────────────────────────────────────────────────────────────┐
 │  Andruna.Manager.exe (WinUI 3)        andruna-sysd.exe (system server) │
 │   library, settings, dialogs  ◄─IPC─►  package manager, activity and   │
 │                                        window management, accounts,    │
 │                                        notifications, devices, Binder  │
 │                                        context manager                 │
 │                                              ▲                         │
 │                                              │ Binder (shared memory)  │
 │       ┌──────────────────────┬───────────────┴──────┐                  │
 │  app process (AppContainer)  app process           host process        │
 │  com.example.one             com.example.two       (pre-warmed, idle)  │
 │                                                                         │
 │  andruna-compositor (in sysd): DirectComposition tree per window        │
 └──────────────────────────────────────────────────────────────────────┘
```

| Process | Runs | Lifetime |
| --- | --- | --- |
| `andruna-sysd.exe` | System services, Binder context manager, compositor, launcher, package manager | Starts with the first app or the manager; exits a configurable time after the last app closes |
| App process (`andruna-app.exe`) | One Android package (or one `android:process` of it): ART, framework, the app | Android lifecycle; reclaimed under memory pressure like Android's LRU |
| Host process | An app process before assignment: ART started, boot image mapped, framework classes initialized | Kept warm while any app is running; at most N (default 1) |
| `Andruna.Manager.exe` | Management UI and system dialogs | While open |
| `andruna-xlat-worker.exe` | Background arm64 translation (x64 only) | Idle priority, EcoQoS, job object with a memory cap |

Nothing runs when no app and no manager window is open, except an optional
scheduled wake for push and jobs (user setting, [P-25](01-product.md)).

## Layers inside an app process

```
 app code: classes.dex                 app native code: lib*.so (ELF)
 ──────────────────────────────────    ────────────────────────────────────
 framework.jar (AOSP)                  andruna-linker: ELF loader + linker
   + Andruna service clients            bionic (AOSP) or bionic shim
 libcore (AOSP), Conscrypt              arm64 translation (x64 PCs only)
 ART (AOSP): interpreter, JIT, AOT
 libandroid_runtime, HWUI, Skia ──► OpenGL ES ──► ANGLE ──► Direct3D 11
 ──────────────────────────────────────────────────────────────────────────
 andruna-lx: the Linux interfaces as a library (memory, files, fds, signals,
   futex, epoll, sockets, /proc, /dev/binder, ashmem/memfd)
 ──────────────────────────────────────────────────────────────────────────
 Windows: Win32, NT, D3D11, DirectComposition, WASAPI, Media Foundation
```

The lx layer is a library, not a system-call emulator: AOSP C/C++ code is
compiled against Linux headers (bionic's) and links to `andruna-lx` exports.
Guest ELF code links to the same exports through `andruna-linker`.

## Calling conventions

| Boundary | Convention |
| --- | --- |
| Windows APIs | Microsoft x64 / Windows ARM64 |
| AOSP C/C++ code (built by us) | Microsoft x64 / Windows ARM64 (normal Windows code) |
| ART compiled code and its entrypoints | System V on x64 (as ART generates), AAPCS64 on ARM64 |
| JNI natives | System V on x64 (`JNICALL` = `sysv_abi`); AAPCS64 on ARM64 |
| Guest ELF code (x64) | System V; every call into Windows-ABI code goes through a thunk |
| Guest ELF code (arm64 on WoA) | AAPCS64. Differences to Windows ARM64 (x18, variadic calls) are handled by thunks |
| Guest ELF code (arm64 on x64) | Translated; calls leave translated code through generated trampolines |

ARM64 note: Windows reserves x18 for the TEB; Android reserves x18 as a
platform register and NDK code does not use it. This must be verified in
[spike S4](../plan/spikes.md).

## Startup of an app

1. The user opens a Start menu shortcut, which runs `andruna-launch.exe
   <package>` (a tiny exe, no runtime).
2. The launcher asks sysd (starting it if needed) to start the package.
3. sysd shows the splash window immediately (D-3 frame, app icon).
4. sysd takes a host process from the pool, or creates one. A host process is
   created inside the package's AppContainer only after assignment, so pooled
   hosts are created per expected package (see
   [security](04-security.md#warm-processes-and-appcontainer)).
5. The host attaches the package: class loader, resources, `Application`,
   then the launcher `Activity` through `ActivityThread`, as on Android.
6. HWUI draws the first frame into the window's composition surface; the
   splash fades out.

## IPC

| Between | Mechanism |
| --- | --- |
| App processes and sysd | Binder, implemented by `andruna-binder` over shared memory sections and wait-on-address style signaling |
| Manager and sysd | Binder as well (the manager uses a small C# client library), so there is one IPC system |
| Launcher and sysd | A named pipe with a minimal protocol (start package, focus window) |

## Data on disk

| Path | Contents |
| --- | --- |
| `%LOCALAPPDATA%\Programs\Andruna\` | Binaries, AOSP runtime files, boot image (per-user install) |
| `%LOCALAPPDATA%\Andruna\system\` | Package database, settings, accounts (DPAPI-protected secrets) |
| `%LOCALAPPDATA%\Andruna\data\<package>\` | App private data (Android `/data/data/<package>`), case-sensitive directories |
| `%LOCALAPPDATA%\Andruna\app\<package>\` | Installed APKs, extracted native libraries, OAT/VDEX/ART files |
| `%LOCALAPPDATA%\Andruna\cache\` | Translation cache (arm64 on x64), shader cache |
| `%LOCALAPPDATA%\Andruna\logs\` | Logs, rotated |
| User folders | Shared storage: `Download`, `Pictures`, `Music`, `Movies`, `Documents` map to the Windows known folders |

Android paths are presented to apps as on a device (`/data/user/0/<pkg>`,
`/storage/emulated/0`); `andruna-lx` maps them.

## AOSP

- One pinned release tag for all projects (initially the latest stable
  release at the time phase 0 starts).
- Sparse fetch of only the projects and directories needed, listed in a lock
  file.
- Built with CMake + Ninja targets that mirror each `Android.bp` module.
- Patches in `patches/<project>/NNNN-<slug>.patch`, gated on
  `defined(ANDRUNA)`.
- A tag upgrade is a planned task each quarter, with a report of broken
  patches.

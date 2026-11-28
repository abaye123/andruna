# Phase 2: the app process lifecycle

**Status:** 2026-11-27, x64. sysd starts real app processes: `StartPackage`
from andruna-launch (or the manager) creates `andruna-app --host`
suspended inside the package's AppContainer and sysd's kill-on-close job,
maps the boot image view into it, resumes it, hands it the package over
the host channel, and reports how it ended, with a crash event when it
crashed. Decisions: [ADR 0037](../../decisions/0037-app-host-channel-and-lifecycle.md)
(channel and lifecycle) and [ADR 0038](../../decisions/0038-boot-image-view-and-lx-adoption.md)
(boot image view), both proposed; they implement ADR 0035 and ADR 0036.

## What runs

| | Result |
| --- | --- |
| Hello World started by the supervisor | in the package's AppContainer (token SID checked), output through sysd's pipes into its log, `Exit` status 0, exit report without a crash, no event |
| Faults with an uncaught `NullPointerException` | status 1; crash report `UncaughtException`, summary `java.lang.NullPointerException: Attempt to read from field 'int Faults.value' ...`, `(thread "main")`, `at Faults.read(Faults.java:10)`, `at Faults.main(...)`; one `AppCrashed` event with that summary |
| A second start of a running package | the same process (`already_running`) |
| Focus of a running app | `NoWindow` (apps have no window until the compositor exists) |
| `Stop` | the app ends (status 0 through the linker's exit path), no crash event |
| A process killed from outside (`taskkill /F`) | `AppCrashed` with its exit code |
| An APK installed through andruna-pkg (`StartActivity`) | the record comes from the package database, the AppContainer from its install hook; the framework entry is refused honestly (`HostStartFailed`) |
| A framework package (no `main`) | refused by the host with "the framework entry is not available: framework.jar is not part of this runtime"; `StartPackage` answers `HostStartFailed` with that message; no event |
| Installed without a package record, or no andruna-app | `HostStartFailed` naming what is missing |
| Boot image view | the ART boot image (3 images, 3 oat files) laid out in a 27.75 MiB view file; ART's mappings adopt 28 292 KiB of it, 0 KiB copied |

## Numbers

Machine: Core Ultra 7 258V, Windows 11 build 26300, other agents building
(loaded; tails are noise). andruna-app release build, sysd test build.
`cargo test -p andruna-sysd --test lifecycle boot_image_start_times` with
`ANDRUNA_MEASURE=15`: Hello World, JIT, boot image; 15 runs each way,
interleaved (view, self, view, self, ...), after one warm-up run each.
"Start" is the supervisor's `start` call (profile lookup, process
creation, mapping, resume, channel), "exit" is the exit report.

| | View mapped by sysd | andruna-app reads the image |
| --- | ---: | ---: |
| Start to exit, median [min-max] | 206.7 ms [141.4-368.6] | 208.6 ms [146.4-301.0] |
| Start to `Running` (runtime starts) | 20.5 ms | 21.8 ms |
| Mapping the view into the suspended process | 0.180 ms [0.059-1.707] | - |

An earlier run of the same test on a less loaded machine gave 146.9 and
147.2 ms start to exit (one run each). Start time does not change: the
image files are in the file cache either way, and reading 28 MiB from the
cache costs about as much as the soft faults on the shared pages.

Memory of an idle app (Faults sleeping after start, 1.5 s after start,
`GetProcessMemoryInfo`):

| | View mapped by sysd | andruna-app reads the image |
| --- | ---: | ---: |
| Working set | 83.9 MiB | 106.1 MiB |
| Private working set | 74.6 MiB | 100.3 MiB |
| Private commit | 1070.6 MiB | 1091.8 MiB |

The view saves 25.7 MiB of private working set and 21 MiB of commit per
app. The shared part is charged once, to the file cache. The remaining
commit is ART's heap reservations as lx commits them (see S7).

From `ANDRUNA_PROFILE` (one run each): `mmap` took 26-28 ms over 656 calls
with the view and 37-39 ms over 691 calls without it.

## How it fits together

- `crates/andruna-ipc/src/host.rs`: the host channel protocol.
- `crates/andruna-app/src/host.rs`: `andruna-app --host`; registers the
  view with lx, adds `-Xnorelocate`, sends `Exit` from the linker's exit
  hook.
- `crates/andruna-lx/src/image.rs`: the boot image region (adoption,
  copy-on-write, zeroing, straddling requests).
- `crates/andruna-sysd/src/apps/`: start of one host (`mod.rs`), the view
  file and section (`bootimage.rs`), crash reports (`crash.rs`), the
  package store (`store.rs`), Windows calls (`win.rs`).
- `crates/andruna-sysd/src/supervisor.rs`: one process per package,
  start, focus, stop, exit reports, `AppCrashed` events through
  `events.rs`.
- `appcontainer::launch` holds a process-creation lock from the
  inheritable handle duplicates to `CreateProcess` (the inheritance race
  noted in S7).

## Tests

- `cargo test -p andruna-sysd`: unit tests (protocol, view layout and
  file, view protections, crash summaries, records), `tests/lifecycle.rs`
  (Hello, the crash event, stop and one process per package, the
  framework refusal, the measurement), `tests/server.rs` (start, focus and
  a killed app over the pipe), `tests/appcontainer.rs`.
- `cargo test -p andruna-lx --lib image`: the region on a real view.
- `cargo test -p andruna-launch`: the launcher starting sysd, which starts
  the app.
- Tests that run ART skip without the ART root and test dex files, unless
  `ANDRUNA_REQUIRE_GUEST=1`; with them, all pass.

```
python spikes/s1/java/build_boot.py --tests-only   # faults.dex has "uncaught" and "wait"
cargo build -p andruna-app --release
set ANDRUNA_APP_EXE=%CD%\target\release\andruna-app.exe
set ANDRUNA_REQUIRE_GUEST=1
cargo test -p andruna-sysd -p andruna-launch
set ANDRUNA_MEASURE=15
cargo test -p andruna-sysd --test lifecycle boot_image_start_times -- --nocapture
```

## Not done

- The framework entry (needs framework.jar in the runtime): the host
  refuses it honestly.
- Stopping an app from the launcher or the manager: the supervisor has
  `stop`, the IPC has no request for it yet.
- The vdex files and image bitmaps are not in the view (ART maps them
  where it chooses, outside the reservation).
- arm64: not run. If ART's segment alignment differs there, lx copies
  instead of adopting (reported at exit).
- Main-class programs are installed by hand for tests and development
  (`RecordStore`); packages installed from APKs come from andruna-pkg's
  database (`DatabaseStore`) and start through the framework entry.
- A warm host (ADR 0035 option 1) is not built.

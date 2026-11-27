# Phase 2: Binder

Status of `/dev/binder` without a kernel driver (design: [ADR
0025](../../decisions/0025-binder-broker.md)).

## What exists

| Part | Where | State |
| --- | --- | --- |
| Binder UAPI (protocol 8), layout assertions | `crates/andruna-binder/src/uapi.rs` | done |
| Broker (driver logic) | `crates/andruna-binder/src/broker` | done, unit tested |
| Control channel, lanes, broker server | `crates/andruna-binder/src/{transport,host,sys}.rs` | done |
| `/dev/binder` device for andruna-lx | `crates/andruna-binder-device` | done |
| lx hook: registered devices, `ioctl`, device `mmap` | `crates/andruna-lx` (`fs::dev::register`, `FileOps::ioctl`, `FileOps::device_mmap`) | done |
| sysd: broker started with the server, servicemanager when configured | `crates/andruna-sysd/src/binder.rs` | done; servicemanager not built yet |
| andruna-elf and andruna-app serve `/dev/binder` | `andruna-elf.rs`, `andruna-app/src/layout.rs` | done |
| libbinder, servicemanager as guest ELF | - | **blocked**: needs the AIDL compiler ([ADR 0026](../../decisions/0026-aidl-compiler-build-tool.md)) |
| Java Binder (libandroid_runtime) | - | not started (needs libbinder and framework.jar) |

## Tests

- `cargo test -p andruna-binder`: broker unit tests, guest processes
  simulated in one process: registration with the context manager and
  the caller's identity, the owner's reference protocol, nested calls to
  the waiting thread, one-way ordering per node, death notification and
  `BR_DEAD_REPLY`, clearing a death notification, fd passing and
  `TF_ACCEPT_FDS`, invalid transactions, `BR_SPAWN_LOOPER`, blocking and
  non-blocking reads, thread and process exit with full cleanup,
  concurrent clients (with and without spinning).
- `cargo test -p andruna-binder-device --test transport`: two processes
  (the test binary started twice) use the device through andruna-lx's
  `openat`, `mmap` and `ioctl` with the broker in the parent: identity,
  round trips, fd passing (the client on a private inherited channel),
  death notification, cleanup.
- `cargo test -p andruna-binder-device --test guest`: the same with real
  guest programs, `binder_raw` (C, bionic, `tests/guest/binder_raw.c`)
  under andruna-elf, a context manager and a client in two guest
  processes. Needs `python aosp/tools/build_guest.py binder-tests` and
  andruna-elf; skips without them unless `ANDRUNA_REQUIRE_GUEST=1`.
- `cargo test -p andruna-sysd binder`: sysd's broker refuses processes it
  did not start or allow.

## Round-trip latency

Method: `binder_raw client` calls the context manager (`binder_raw
manager`, another guest process) synchronously; the manager echoes the
data. Each call is `BC_TRANSACTION`, the manager's `BC_FREE_BUFFER` and
`BC_REPLY`, the client's `BC_FREE_BUFFER`, through bionic's `ioctl` and
andruna-lx in both guest processes and the broker in a third process.
20000 timed calls after 500 warm-up calls per size, `CLOCK_MONOTONIC`
around each call; release builds; three runs.

Machine: Intel Core Ultra 7 258V (8 cores, no SMT), 32 GB, Windows 11 Pro
build 26300. **Heavily loaded**: about eight other agents were compiling
during the runs and free memory was about 1 GB; tails are dominated by
that and must be re-measured on the reference PC.

| Wake | Request | p50 us (3 runs) | p99 us (3 runs) |
| --- | ---: | --- | --- |
| spin 30 us (default) | 100 B | 7.1 / 6.7 / 7.3 | 32.8 / 22.9 / 21.9 |
| spin 30 us (default) | 4 KiB | 9.5 / 8.9 / 9.9 | 50.1 / 96.6 / 66.1 |
| no spinning (`ANDRUNA_BINDER_SPIN_US=0`) | 100 B | 35.6 / 32.9 / 33.6 | 125.0 / 122.8 / 121.8 |
| no spinning | 4 KiB | 36.4 / 36.9 / 38.4 | 131.2 / 123.6 / 118.1 |

The target ([05-performance](../../spec/05-performance.md): a small round
trip under 20 us) is met with spinning; without spinning it is not met on
this machine. A round trip has six wakes: client to its broker thread,
broker thread to the manager's broker thread (inside sysd), to the
manager, and the same back. S3 measured four wakes on a less loaded
machine (12 us with `SignalObjectAndWait`, 1 us with spinning); removing
the hop between the two broker threads (the sender's broker thread
writing the reply into the receiver's lane directly) is the next step if
the non-spinning case matters.

Spinning is bounded (30 us per wait), only while a lane had a message in
the last 2 ms, and off on machines with fewer than four logical
processors, so idle threads sleep in the kernel.

Death notification: from the manager's exit request to `BR_DEAD_BINDER`
in the client, 2.2 to 4.2 ms, almost all of it process teardown before
the broker's pipe breaks (as in S3).

## Not done

- **libbinder and servicemanager** are not built: their sources include
  C++ generated from `.aidl` files, and the AIDL compiler needs bison and
  flex at build time, which are not available here and need a maintainer
  decision ([ADR 0026](../../decisions/0026-aidl-compiler-build-tool.md)).
  `frameworks/native` is in `aosp/aosp.lock` for this. Once the compiler
  builds: `aosp/modules/libbinder.cmake` and `servicemanager.cmake`, a
  guest stage, and an end-to-end test where two guest processes register
  and call a service through servicemanager with libbinder (death
  notification and fd passing included); sysd already starts
  servicemanager when `ANDRUNA_SERVICEMANAGER` names it.
- servicemanager's access checks use SELinux (`Access.cpp`); the broker
  gives no security context, so an Andruna policy for those checks is
  needed when servicemanager is built.
- The Java Binder path (android.os.Binder through libandroid_runtime).
- Scatter-gather buffers, non-file descriptors in transactions, signal
  interruption of a blocked read, ARM64 measurements (the code builds for
  ARM64; the guest test was run on x64 only).

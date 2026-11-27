# Phase 1: ART on the production loader

**Status:** 2026-11-27, x64. ART (libart, the boot image, Hello World and
the S1 benchmark) runs on `crates/andruna-linker` with the real guest
bionic (ADR 0016), started by the new `crates/andruna-app`, in the
interpreter, with the JIT and with AOT code from dex2oat. The spike runner
of S1 and its libc shim are not involved. arm64 builds (Rust, cross);
nothing was run on arm64, and the arm64 ART root was not built.

```
cargo xtask fetch && cargo xtask patch
python spikes/s1/java/build_boot.py                  # boot jars, test dex files
python aosp/tools/build_guest.py --arch x64          # bionic, libc++, tests
python aosp/tools/build_guest.py --arch x64 art      # -> out\guest\x64\art-root
cargo build --release -p andruna-app
andruna-app --art-root out\guest\x64\art-root --mode int|jit|aot ^
    --classpath out\s1\java\hello.dex Hello
andruna-app --art-root ... --compile-boot            # boot image (dex2oat)
andruna-app --art-root ... --compile app\bench.dex   # app odex (dex2oat)
```

## What runs

| | Result |
| --- | --- |
| Hello World, interpreter (`-Xint`, no boot image) | prints `Hello, Andruna`, exit 0 |
| Hello World, JIT (default thresholds, and `-Xjitthreshold:0`) | exit 0 |
| Faults (`-Xjitthreshold:0`): implicit null checks, stack overflow | 2000 NullPointerException caught, StackOverflowError caught (also in AOT); the faults reach ART's handler through libsigchain as SIGSEGV |
| dex2oat: boot image (core-oj, core-libart, core-icu4j, `speed`) | 9 files, 37 MB, base `0x60000000`; dex2oat reports 0.95 s (4.25 s CPU, 8 threads) |
| dex2oat: hello, bench, faults against the boot image | 0.14 to 0.15 s each (wall), odex 34 KiB |
| AOT (`-Ximage:` the boot image, the app's odex, `-Xusejit:false`) | exit 0; `-verbose:oat` shows the odex `kOatUpToDate`, executable |
| A missing main class | non-zero exit, the error names the class |

`cargo test -p andruna-app` runs all of these (except the benchmark) in
child processes; with `ANDRUNA_REQUIRE_GUEST=1` and the ART root built, 6
of 6 pass (debug build), plus 6 unit tests.

## Numbers

Machine: Core Ultra 7 258V (8 threads), Windows 11 build 26300, other
agents building in parallel (background load varied between about 10% and
50% during the runs). Release build. "S1 runner" is the round 4 runner
(`spikes/s1/art`) run on the same machine in the same session for
comparison; the S1 document's figures were 538-725 ms (interpreter),
138-146 ms (JIT) and 25-27 ms (AOT).

Benchmark total as Bench reports it, ms, three runs each:

| Mode | andruna-app (quieter period) | andruna-app, interleaved with the S1 runner | S1 runner, same session |
| --- | ---: | ---: | ---: |
| Interpreter, no boot image | 393 to 396 | 467 to 513 | 464 to 576 |
| JIT, no boot image | 110 to 114 | 127 to 136 | 123 to 173 |
| JIT, `-Xjitthreshold:0` | 84 to 92 | | |
| AOT | 22 to 26 | | 25 to 27 |

Parts (andruna-app, quieter period): interpreter int loop 174 ms, fib 20
to 22, string build 116 to 117, sort 79 to 80; JIT 18 to 20, 4, 50 to 55,
33 to 35; AOT 11 to 12, 0, 2, 9 to 10.

Reading: with the same ART build and compiler, the production loader with
real bionic is at least as fast as the spike runner; the interpreter is
about 10% faster when the two are interleaved (bionic's string and memory
routines and scudo instead of the shim's byte loops), the JIT and AOT are
the same within noise.

Start-up, wall time of the whole process for Hello World (spawn, mount,
load 25 objects, runtime start, run, shutdown), three runs each:

| Mode | andruna-app | S1 runner (via run_art.py) |
| --- | ---: | ---: |
| Interpreter, no boot image | 156 to 157 ms | 160 to 180 ms |
| JIT, no boot image | 155 to 158 ms | 170 to 180 ms |
| JIT with the boot image | 148 to 151 ms | |
| AOT | 146 to 149 ms (one outlier 513 ms) | 160 to 170 ms |

Bench wall time: interpreter 572 to 577 ms, JIT 290 to 297 ms, AOT 174 to
196 ms. Boot image load in AOT mode (`-verbose:image`): 12.7 ms, with
relocation 1.4 ms.

Regression check of the loader changes on bionic's and libbase's suites
(`crates/andruna-linker/tools/run_gtest.py`, 60 s per run):

| Suite | Before (S4 round 2) | Now |
| --- | --- | --- |
| bionic_tests | 1104 of 1327 pass, 31 end the process | 1117 of 1327 pass, 124 fail, 13 skipped, 34 end the process |
| libbase_test | 297 of 367 pass, 37 end the process | see the branch's final report (rerun after the logd fix) |

## What the loader gained (andruna-linker)

- **System calls on the Windows stack.** `__andruna_syscall` switches a
  guest thread to its Windows stack (and the TEB bounds with it) for the
  duration of the call, as a kernel runs on its own stack. Without it the
  debug build crashed: ART releases its unused stack below the stack
  pointer with `madvise(MADV_DONTNEED)`, which lx implements by
  decommitting, freeing the host frames of that very call; and the host
  code of system calls made while ART throws StackOverflowError ran past
  ART's reserved region.
- **Signals** (`signal.rs`): actions, per-thread masks inherited by new
  threads, `sigaltstack`, `rt_sigtimedwait` (ART's signal catcher),
  `kill`/`tgkill` with queueing, default actions, and synchronous faults
  from the first vectored handler delivered with bionic's `siginfo_t` and
  `ucontext_t`, **on the alternate stack** when the action has
  `SA_ONSTACK` (bionic gives every thread one), with the TEB describing it.
- **getpid is the main thread's id**, as on Linux; bionic recognizes the
  main thread by `tid == getpid()` and then reads `/proc/self/stat` and
  `/proc/self/maps` for its stack.
- `/proc/self/{stat,maps,cmdline}` and `/sys/devices/system/cpu/{online,
  possible,present}` (`procfs.rs`); without the latter, dex2oat ran on one
  thread (4.2 s for the boot image instead of 0.95 s).
- Interim system calls (`interim.rs`), used only when lx answers `ENOSYS`:
  file mappings as copies with write-back of shared writable ones, futex
  requeue as wake-up, `ftruncate` (grow), `uname`, rlimits, `getrusage`,
  `sysinfo`, `prctl` names, scheduling calls, `membarrier`, `mincore`,
  directory calls through lx's mount table, `readlink("/proc/self/exe")`.
- A stand-in for logd's socket (`logd.rs`): liblog's records go to the
  guest's standard error as `P/tag(tid): message`, at or above
  `ANDRUNA_LOG_PRIORITY` (default W), until sysd has a log service.
- One linker namespace, exported as "default", accepted by
  `android_dlopen_ext` (ART opens odex files with it; other dlext options
  still fail and ART then uses its own ELF loader, as in S1).

## Build glue

- `aosp/guest/art.cmake`, stage `art` of `build_guest.py`: the S1 objects
  linked against real bionic, every object with `--no-undefined`, staged as
  the ART root.
- `aosp/guest/unwindstack_unavailable.cpp`: a `libunwindstack.so` that
  reports every unwind as unsupported (libart and libsigchain link it for
  native stack dumps; AOSP's needs libzstd and a Rust demangler).
- `aosp/overlay/art-gen/src/nativeloader_no_namespaces.cpp`: the two
  namespace queries libart calls, for the host variant of libnativeloader.
- `patches/bionic/0007`: `inline_raise` (abort, fdsan) used a raw
  `syscall`/`svc` instruction, which enters the Windows kernel; under
  `ANDRUNA` it calls `syscall()`.

## Not done, not verified

- arm64: the Rust side builds (`cargo clippy --target
  aarch64-pc-windows-gnullvm`), the arm64 signal frame is ported from the
  S1 runner, but no arm64 ART root was built or run here; AOT on arm64 is
  another task.
- nterp on x86_64 is still off (ART patch 0003): the interpreter numbers
  are the switch interpreter's.
- The boot jars and test dex files were the S1 build's outputs at the same
  tag (`spikes/s1/java/build_boot.py`), not rebuilt; the boot class path
  build still lives under `spikes/`.
- ICU time zone data (`/apex/com.android.tzdata`) is missing (ART logs it);
  dex files in the jars are not 4-byte aligned, so ART extracts them.
- Asynchronous signals to a running thread are not delivered (queued only);
  file mappings are copies; libunwindstack is a stand-in.

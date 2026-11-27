# Phase 1: ART on the production loader

**Status:** 2026-11-27, x64. ART (libart, the boot image, Hello World and
the S1 benchmark) runs on `crates/andruna-linker` with the real guest
bionic (ADR 0016), started by the new `crates/andruna-app`, in the
interpreter, with the JIT and with AOT code from dex2oat. The spike runner
of S1 and its libc shim are not involved. Signals, `/proc`, file mappings,
futex requeue and scheduling come from andruna-lx (phase 1 merge, ADR
0019). arm64 builds (Rust, cross); nothing was run on arm64, and the arm64
ART root was not built.

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
| Faults (`-Xjitthreshold:0`, and AOT): implicit null checks, stack overflow | 2000 NullPointerException caught, StackOverflowError caught; the faults reach ART's handler through libsigchain as SIGSEGV (lx's delivery on the alternate stack) |
| dex2oat: boot image (core-oj, core-libart, core-icu4j, `speed`) | 9 files, 37 MB, base `0x60000000`; dex2oat reports 0.95 to 1.38 s (about 4.3 s CPU, 8 threads); 1.9 s wall |
| dex2oat: hello, bench, faults against the boot image | 0.14 to 0.15 s each (wall), odex 34 KiB |
| AOT (`-Ximage:` the boot image, the app's odex, `-Xusejit:false`) | exit 0; `-verbose:oat` shows the odex `kOatUpToDate`, executable |
| A missing main class | non-zero exit, the error names the class |

`cargo test -p andruna-app` runs all of these (except the benchmark) in
child processes; with `ANDRUNA_REQUIRE_GUEST=1` and the ART root built, 6
of 6 pass in debug and release builds, plus 6 unit tests.

## Numbers

Machine: Core Ultra 7 258V (8 threads), Windows 11 build 26300, other
agents building in parallel (background CPU load between about 10% and
76% during the runs, measured with `Win32_Processor.LoadPercentage`).
Release build. "S1 runner" is the round 4 runner (`spikes/s1/art`, same
ART build settings and compiler) run on the same machine in the same
session; the S1 document's figures were 538-725 ms (interpreter), 138-146
ms (JIT) and 25-27 ms (AOT).

Benchmark total as Bench reports it, ms, three runs each. In each column
pair the two programs ran interleaved, so they saw the same load:

| Mode | andruna-app, quiet (before the lx merge) | S1 runner, same period | andruna-app, interleaved (before) | S1 runner | andruna-app, loaded (after the lx merge) | S1 runner |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Interpreter, no boot image | 393 to 396 | 464 to 524 | 467 to 513 | 510 to 576 | 594 to 724 | 645 to 783 |
| JIT, no boot image | 110 to 114 | 123 to 173 | 134 to 136 | 130 to 141 | 182 to 204 | 177 to 205 |
| JIT, `-Xjitthreshold:0` | 84 to 92 | | | | 114 to 150 | |
| AOT | 22 to 26 | 25 to 27 | | | 30 to 33 | 30 to 31 |

Parts (andruna-app, quiet): interpreter int loop 174 ms, fib 20 to 22,
string build 116 to 117, sort 79 to 80; JIT 18 to 20, 4, 50 to 55, 33 to
35; AOT 11 to 12, 0, 2, 9 to 10.

Reading: with the same ART build, the production loader with real bionic
runs the interpreter about 8 to 10% faster than the spike runner (bionic's
string and memory routines and scudo instead of the shim's byte loops);
JIT and AOT code are the same within noise, as expected (they hardly call
libc).

Start-up, wall time of the whole process for Hello World (spawn, mount,
load 26 objects, runtime start, run, shutdown):

| Mode | andruna-app, before the lx merge | andruna-app, after the lx merge | S1 runner |
| --- | ---: | ---: | ---: |
| Interpreter, no boot image | 156 to 157 ms | 191 to 250 ms (load about 20%) | 160 to 180 ms |
| JIT, no boot image | 155 to 158 ms | 282 to 342 ms (loaded) | 160 to 180 ms |
| JIT with the boot image | 148 to 151 ms | | |
| AOT | 146 to 149 ms (one outlier 513 ms) | 219 to 290 ms (loaded) | 160 to 170 ms |

After rebasing onto the andruna-lx merge, start-up is 35 to 90 ms slower;
not diagnosed yet. One Hello run makes about 19,000 system calls, 17,900
of them `clock_gettime`; the other frequent ones are `mmap` (621), `prctl`
(607, ART naming its mappings, which lx now implements), `munmap` (569),
`madvise` (547) and `mprotect` (411). Boot image load in AOT mode
(`-verbose:image`): 12.7 ms, with relocation 1.4 ms.

Regression check of the loader changes on bionic's and libbase's suites
(`crates/andruna-linker/tools/run_gtest.py`), before the lx merge:
bionic_tests 1117 of 1327 pass (S4 round 2: 1104), 124 fail, 13 skipped,
34 end the process; libbase_test 302 of 367 (S4 round 2: 297), the
`WaitForProperty*` tests time out as before. After the merge (30 s per
run, machine loaded): libbase_test 307 of 367 (the lx conformance record:
307), bionic_tests 1127 of 1327, 113 fail, 13 skipped, 35 end the process
(the lx record: 1129). Among the 35 is
`pthread_CondWakeupTest.signal_clockwait_monotonic_monotonic`, which timed
out in the suite but passed 20 of 20 runs of its test case alone.

## What the loader gained (andruna-linker)

- **System calls on the Windows stack.** `__andruna_syscall` switches a
  guest thread to its Windows stack (and the TEB bounds with it) for the
  duration of the call, as a kernel runs on its own stack. Without it the
  debug build crashed: ART releases its unused stack below the stack
  pointer with `madvise(MADV_DONTNEED)`, which lx implements by
  decommitting, freeing the host frames of that very call; and host code
  of system calls made while ART throws StackOverflowError ran past ART's
  reserved region. Nested calls (a signal handler run from inside a call)
  stay on the current stack; `madvise` there keeps the host frames.
- **andruna-lx and andruna-linker are optimized in debug builds**
  (workspace `Cargo.toml`): lx's exception handler runs on the faulting
  stack until it switches to the alternate stack, within ART's 8 KiB gap,
  and unoptimized frames overflowed it.
- `/proc/self/stat` (`startstack`, how bionic finds the main thread's
  stack; lx's `/proc` has no `stat`) and
  `/sys/devices/system/cpu/{online,possible,present}` (`procfs.rs`);
  without the latter, dex2oat ran on one thread (4.2 s for the boot image
  instead of about 1 s).
- Interim system calls (`interim.rs`), used only where lx answers `ENOSYS`
  (or, for writable shared file mappings, `ENODEV`): write-back for
  writable `MAP_SHARED` file mappings on lx's private copy (dex2oat writes
  vdex and oat files through them), `uname`, rlimits, `getrusage`,
  `sysinfo`, `getppid`, priorities, `getcpu`, `membarrier`, `mincore`,
  `umask`, `fsync`, `msync`, `chmod` family.
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

- arm64: the Rust side builds (`cargo clippy`/`cargo build --target
  aarch64-pc-windows-gnullvm`); no arm64 ART root was built or run here;
  AOT on arm64 is another task.
- The start-up regression after the lx merge (above).
- nterp on x86_64 is still off (ART patch 0003): the interpreter numbers
  are the switch interpreter's.
- The boot jars and test dex files were the S1 build's outputs at the same
  tag (`spikes/s1/java/build_boot.py`), not rebuilt; the boot class path
  build still lives under `spikes/`.
- ICU time zone data (`/apex/com.android.tzdata`) is missing (ART logs it);
  dex files in the jars are not 4-byte aligned, so ART extracts them.
- libunwindstack is a stand-in; writable shared file mappings are copies
  written back on `msync`, `munmap` and exit.

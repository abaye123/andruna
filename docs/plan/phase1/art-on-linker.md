# Phase 1: ART on the production loader

**Status:** 2026-11-27, x64 and arm64. ART (libart, the boot image, Hello World and
the S1 benchmark) runs on `crates/andruna-linker` with the real guest
bionic (ADR 0016), started by the new `crates/andruna-app`, in the
interpreter, with the JIT and with AOT code from dex2oat. The spike runner
of S1 and its libc shim are not involved. Signals, `/proc`, file mappings,
futex requeue and scheduling come from andruna-lx (phase 1 merge, ADR
0019). The same runs pass on Windows ARM64 (see [ARM64](#arm64)): the
interpreter, the JIT and AOT code from dex2oat running on arm64, through
andruna-app, in CI (`.github/workflows/art-arm64.yml`, run 46010734840).

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
of 6 passed in debug and release builds; with the wall-clock probe added
by the clock fix below, 7 of 7 pass in release builds on x64 and on
arm64 (CI), plus 6 unit tests.

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

## Start-up (x64)

**The first lx merge did not slow start-up; the machine did. lx round 2
did, by opening `/proc/self/stat`.** Measured with the builds interleaved,
so that all see the same background load, Hello World started as fast
after the first andruna-lx merge as before it: the 191 to 342 ms above
were taken while other agents loaded the machine, the 146 to 158 ms
"before" figures on a quieter one. After andruna-lx round 2 (main
80048e4) the ART part became 25 to 35 ms slower; `ANDRUNA_PROFILE` showed
`openat` going from 6 ms in total to 24 to 43 ms, all of it in the two
opens of `/proc/self/stat` (bionic reads it to find the main thread's
stack in `pthread_getattr_np`, which ART calls at start-up). Round 2
builds that file from `NtQuerySystemInformation(SystemProcessInformation)`,
a snapshot of every process and thread of the system; each open took 11
to 21 ms on this machine.

Method: release builds of andruna-app at commit 4c28486 (the last one
before the rebase onto the lx merge) and of this branch, the same ART root
and boot image, Hello World run 8 times per build and mode in an
interleaved order (`int`, `jit` without boot image; `aot` with it).
Background load during the runs between 48 and 80% (other agents). Wall
time of the process, ms:

| Mode | before the lx merge, median (min to max) | after, without vDSO | after, with vDSO |
| --- | ---: | ---: | ---: |
| Interpreter | 168 (155 to 304) | 176 (156 to 191) | 177 (151 to 244) |
| JIT | 172 (154 to 225) | 175 (161 to 216) | 176 (164 to 205) |
| AOT | 177 (148 to 214) | 170 (148 to 201) | 177 (152 to 215) |

The differences are inside the spread of each column.

After merging lx round 2 (same method, 8 interleaved runs; load 100%
during the runs, so all numbers are higher):

| Mode | before the lx merge | lx round 2, this branch | of which the two `/proc/self/stat` opens |
| --- | ---: | ---: | ---: |
| Interpreter | 209 (199 to 408) | 234 (208 to 335) | about 15 |
| JIT | 197 (187 to 235) | 234 (209 to 245) | about 15 |
| AOT | 186 (172 to 219) | 214 (193 to 239) | about 15 |

What this branch changed there (`crates/andruna-lx/src/sys/procinfo.rs`,
the only lx edit): the query buffer started at 512 KiB and, when Windows
asked for more, was reallocated at 16 times its size (8 MiB), so every
open queried twice and touched 8 MiB; it now starts at 2 MiB or the size
the last call needed and grows by an eighth over what Windows asks for.
One open now costs one query, 6.5 to 7.5 ms here (loaded desktop, a few
hundred processes) and about 1.3 ms on the CI runner. The rest of the
difference is that query itself. Making it per process (the threads lx
already knows, `NtQueryInformationThread` with
`ThreadSystemThreadInformation` for state and times, `GetProcessMemoryInfo`
for the memory fields) is for andruna-lx's owner, since it changes how
`/proc/<pid>/stat` and `status` are produced. The other system calls cost
what they did before round 2.

Where the time goes (medians of the same runs, ms since Windows created
the process, from `ANDRUNA_PROFILE`, which this branch adds to the
loader):

| Part | ms |
| --- | ---: |
| Process creation to `main` of andruna-app (Windows; a trivial `whoami.exe` takes 14 ms to run on the same machine) | 16 to 19 |
| Mounting the guest tree (andruna-lx) | 2 to 3 |
| Loading, binding and relocating the 26 objects | 12 to 16 |
| Initializers | under 1 |
| ART: runtime start, `main`, shutdown | 126 to 138 |
| Process exit after `exit_group` | about 6 |

Inside ART's part the system calls that take time are (one AOT run;
times summed over all threads, so blocking waits of other threads count
too): `mmap` 653 calls, 45 ms (file mappings are private copies filled
with `pread`, andruna-lx's current design; the boot image alone is 37
MB), `munmap` 590 calls, 9 ms, `openat` 41 calls and `newfstatat` 48
calls, about 100 and 70 us each (`CreateFileW` and its file system
filters), futex and `rt_sigtimedwait` waits. The same numbers before the
lx merge: `mmap` 39 ms, `munmap` 9 ms, `openat` 90 us, `newfstatat` 68 us.

**System call cost.** A guest program (`crates/andruna-linker/tests/guest/clock_test.c`)
measures the paths through bionic on the production loader (200 000
calls each, machine loaded):

| Path | ns per call |
| --- | ---: |
| `syscall(SYS_gettid)`: `__andruna_syscall`, switch to the Windows stack, dispatch, lx, lx's pending-signal check | 26 to 33 |
| `syscall(SYS_clock_gettime)`, `CLOCK_MONOTONIC` | 57 to 80 |
| `clock_gettime(CLOCK_MONOTONIC)` without the vDSO (the same system call) | 55 to 64 |
| `clock_gettime(CLOCK_MONOTONIC)` with the vDSO | 30 to 38 |
| `gettimeofday` with the vDSO | 43 to 65 |

So the entry path and the pending-signal check (one atomic load while
no signal is queued) are not where start-up goes: an imageless start makes
17 910 `clock_gettime` calls (ART's class linking timing), which cost
about 1 ms through the system call path and about 0.6 ms through the
vDSO. The vDSO is kept because it is how Linux provides these calls and
it halves their cost, not because it changes start-up measurably.

**The wall clock was wrong.** `System.currentTimeMillis()` and
`Instant.now()` returned 0 on andruna-app: bionic's `gettimeofday` went to
the `gettimeofday` system call, which nothing implemented then (`ENOSYS`).
The vDSO's `gettimeofday` and `time` fix it (lx round 2 now also answers
the two system calls, for code that makes them directly);
`wall_clock_is_the_system_time` in
`crates/andruna-app/tests/run.rs` (a Java probe, `spikes/s1/java/src/Clock.java`)
and `clock_test` check them against the Windows system time.

What would make start-up faster, in order of size: real file mappings
instead of copies in andruna-lx (`MapViewOfFile3` with placeholders, the
step lx's documentation names next), a per-process `/proc/self/stat`
(above), fewer and cheaper file opens, and the loader's 12 to 16 ms for
26 objects.

`ANDRUNA_PROFILE=1` prints, at exit: phase marks in ms since Windows
created the process, system call counts and dispatcher time per number,
the slowest calls with their path argument, and exception counts (lazy
FS-base restores, emulated instructions, delivered signals).

## ARM64

andruna-app runs ART on Windows ARM64 through andruna-linker with the
real bionic: Hello World, the S1 benchmark, the fault test and the
wall-clock probe in the interpreter, with the JIT (default thresholds and
`-Xjitthreshold:0`) and as AOT code, with dex2oat running on arm64 for the
boot image and the app odex files. The guest tests of andruna-linker
(bionic, C++ exceptions, `dlopen`, the vDSO) and the andruna-app tests
pass there too.

CI: `.github/workflows/art-arm64.yml` (branch `phase1-arm64-app`). The
arm64 guest userland and ART root are cross-built on the x64 runner
(`build_guest.py --arch arm64 sysroot builtins bionic libcxx aosp art`,
cached, plus `linker-tests`); andruna-app is built natively on the
`windows-11-arm` runner (hosted VM, 4 logical processors), which runs
`crates/andruna-app/tools/runs.py` (three runs per case), an
`ANDRUNA_PROFILE` start-up profile and the tests. Passing run:
46010734840.

Results (run 46010734840, ms):

| Mode | Hello World, wall | Bench total | int loop | fib(27) | string build | array sort | Bench, wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Interpreter (nterp), no boot image | 140 to 146 | 780 to 782 | 301 to 302 | 76 | 256 to 257 | 143 | 921 to 925 |
| JIT, no boot image | 147 to 151 | 66 | 27 to 28 | 2 | 17 to 18 | 14 to 15 | 210 to 215 |
| JIT, `-Xjitthreshold:0` | 256 to 264 | 141 to 142 | 33 to 34 | 2 | 23 to 24 | 19 | 329 to 338 |
| AOT (boot image and odex) | 128 to 132 | 26 | 12 | 0 | 3 to 4 | 9 | 152 to 160 |

- dex2oat on arm64: boot image 2.1 s wall, hello, bench and faults 0.14
  to 0.15 s each.
- Faults: 2000 `NullPointerException`s from implicit null checks and
  `StackOverflowError` caught, with `-Xjitthreshold:0` (depth 1 536 613)
  and in AOT code (depth 2 614 140): the implicit checks reach ART's
  handler through lx's signal delivery on arm64.
- The S1 runner on the same kind of runner (round 5): Hello 0.13 to 0.16
  s; Bench totals 742 to 799 (interpreter), 69 to 70 (JIT), 149 (JIT at
  first call), 25 (AOT). andruna-app matches them.
- Start-up (`ANDRUNA_PROFILE`, Hello World): process creation to `main`
  12 to 13 ms, loading the 26 objects 9 to 11 ms, ART 93 to 118 ms.
- System call paths (`clock_test`, ns per call): `syscall(SYS_gettid)`
  49; `clock_gettime` 85 without the vDSO and 46 with it; `gettimeofday`
  128 without and 49 with.

What arm64 needed:

1. **scudo without Top-Byte Ignore** (`aosp/modules/bionic_libc.cmake`).
   On arm64 Linux scudo writes each chunk header through a pointer with a
   tag in the top byte, relying on the kernel's TBI. Windows does not
   enable TBI for user mode, so the first `malloc` faulted (a write to
   `0x0200...`). Built with scudo's own `SCUDO_DISABLE_TBI`; no patch.
2. **TEB stack bounds of guest threads** (andruna-linker, `thread.rs`).
   The bounds a guest thread got covered only the top page of its stack
   (`VirtualQuery`'s `BaseAddress` is the queried page, not the region's
   start). Windows ARM64 checks the stack pointer against them whenever a
   vectored handler resumes a thread (`RtlGuardRestoreContext`, fail-fast
   `FAST_FAIL_INVALID_SET_OF_CONTEXT`, exit code 0xC0000409); x64 does not.
   With `-Xjitthreshold:0` the first emulated cache instruction on a JIT
   thread ended the process, and the stack overflow test did too. The
   limit is now the start of the stack's allocation. Found with cdb on
   the runner (the workflow's diagnose step).
3. **`mrs ctr_el0`** traps at EL0 on Windows (S1 round 4); the loader's
   first vectored handler now emulates it (and `dc cvau`/`ic ivau`,
   should they trap) for compiler-rt's `__clear_cache`, which the JIT
   calls.
4. The libunwindstack stand-in defines `RegsArm64::GetVgFromLocal`, which
   libart references on arm64.

Everything else came from earlier work: TPIDR_EL0 for the thread
pointer, x18 kept as the TEB (no shadow call stack, bionic patches 0004
and 0006), ART's 16 KiB stack overflow gap on arm64, the arm relative
patcher source in dex2oat (S1 round 5), and lx's arm64 signal frames.

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

- arm64 numbers come from one hosted VM (three runs per case); the
  arm64 bionic and libbase gtest suites were not run through the loader
  (only the loader's own guest tests).
- `/proc/self/stat` still costs one system-wide process query per open
  (above); start-up after lx round 2 is 10 to 30 ms slower than before it
  on a loaded desktop.
- nterp on x86_64 is still off (ART patch 0003): the interpreter numbers
  are the switch interpreter's.
- The boot jars and test dex files were the S1 build's outputs at the same
  tag (`spikes/s1/java/build_boot.py`), not rebuilt; the boot class path
  build still lives under `spikes/`.
- ICU time zone data (`/apex/com.android.tzdata`) was missing (ART logged
  it); since the libcore tests' round 2 it is staged in the ART root
  (ADR 0021). Dex files in the jars are not 4-byte aligned, so ART extracts
  them.
- libunwindstack is a stand-in; writable shared file mappings are copies
  written back on `msync`, `munmap` and exit.

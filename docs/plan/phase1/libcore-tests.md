# libcore tests

**Status:** 2026-11-27, second round (branch `p1-conscrypt-tzdata`):
Conscrypt, Bouncy Castle, BoringSSL (NativeBN) and the time zone data are
in the ART root. With main's wall clock, JNI stub and Linux layer fixes the
pass rate is **98.2% (interpreter) and 98.1% (JIT)**; the providers and the
time zone data alone took it from 89.8% to 97.4%. The JIT hangs of round 2
are fixed (branch `p1-jit-hang`, [below](#jit-hang-resolved-mincore)):
**98.3% in both modes**, no timeouts. First round: branch
`phase1-libcore-tests`. The
phase 1 exit criterion "selected libcore tests pass" is measured here: a
subset of libcore's own luni tests, compiled for ART and run in-process on
andruna-app (ART on andruna-linker with bionic), on x64, with the
interpreter and the JIT. The harness is in [tests/libcore](../../../tests/libcore/README.md).

## How it is measured

```
cargo xtask fetch && cargo xtask patch
python spikes/s1/java/build_boot.py
python aosp/tools/build_guest.py --arch x64 sysroot bionic libcxx aosp art
cargo build -p andruna-app --release --target x86_64-pc-windows-gnullvm
target\x86_64-pc-windows-gnullvm\release\andruna-app.exe --art-root out\guest\x64\art-root --compile-boot
cargo xtask libcore-tests --mode int          # build.py, then run.py
cargo xtask libcore-tests --no-build --mode jit
```

- `build.py` compiles the selection (`tests/libcore/tests.txt`) with javac
  against the boot class path classes and libcore's test support sources,
  then d8 (pinned build-tools 37, `--min-api 36`), into one jar with the
  test resources. Files that do not compile are dropped and reported.
- The in-VM runner (`andruna.libcoretest.Main`, own code) runs each class
  with JUnit 4's `JUnitCore` and prints one machine-readable line per test.
- `run.py` starts one ART process per class (4 in parallel), restarts the
  class after a crash or a hang (no result for 120 s in the interpreter, 60 s
  with the JIT) without the tests already done, and matches the results
  against `tests/libcore/expectations.txt`.
- Since round 2, `build_boot.py` also builds `bouncycastle.jar` and
  `conscrypt.jar` (`spikes/s1/java/build_security.py`; it compiles two host
  tools, flatc and Conscrypt's constants generator, with llvm-mingw's
  clang++), and the art stage of `build_guest.py` stages `libcrypto.so`,
  `libssl.so`, `libjavacrypto.so` and the time zone module files
  (`<ART root>/tzdata`, mounted at `/apex/com.android.tzdata`); ADR 0021.
- andruna-app runs with the boot image (dex2oat `--compile-boot`).
  `--mode jit` runs the boot class path as AOT code and JIT-compiles the
  test code. `--mode int` passes `-Xint`, which makes ART interpret
  everything, the boot class path's AOT code included, with the switch
  interpreter (nterp is off on x86_64, patch `art/0003`): compute-heavy
  tests are 10 to 40 times slower there (below).

Machine: Intel Core Ultra 7 258V, Windows 11 build 26300, shared with other
work. andruna-app built from `main` at `ba6f619` (this branch does not
change it), release, `x86_64-pc-windows-gnullvm`. The ART root (with its
boot image) was built by the art stage of `aosp/tools/build_guest.py` from
the branch that became `ba6f619`, at the pinned AOSP tag; the S1 runner and
its guest from `main` (spike S1 round 4).

## Test subset

Selected: every `*Test.java` of luni's `java.lang`, `java.math`, `java.util`
(with `regex` and `concurrent`), `java.io`, `java.nio` (with `charset` and
`file`) and `java.text` test packages, without `ProcessBuilderTest` (child
processes) and `VirtualThreadTest` (outside phase 1): 362 files. 350 compile
and run; 12 are dropped by the build:

| Dropped | Why |
| --- | --- |
| `lang.ThreadTest`, `lang.PackageTest`, `util.HashMapTest`, `nio.file.FilesTest`, `Files2Test`, `SimpleFileVisitorTest` (and `lang.ClassTest`, `lang.StackWalkerTest`, which use `ThreadTest`) | need Mockito (needs a dex-capable mocking backend; not set up) |
| `io.FileTest`, `util.FormatterTest`, `util.RandomTest`, `util.regex.MatcherTest` | need `CoreCompatChangeRule` (test-rules `platform_compat`, which needs Guava) |

## Round 2 results (x64)

Same machine and selection, andruna-app release
(`x86_64-pc-windows-gnullvm`), boot image of the five boot jars. Two
builds: this branch on main at `8b4cf96` (the round 1 base: only the
providers and time zone data changed), and rebased on main at `db2f059`
(with the wall clock fix, the JNI stub fix `art/0005` and andruna-lx round
2).

| Build, mode | Tests | Pass | Fail | Crash | Timeout | Not run | Pass rate | Wall |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **on `db2f059`, interpreter** | 3149 | 3088 | 58 | 0 | 0 | 0 | **98.2%** | 112 s |
| **on `db2f059`, JIT** | 3149 | 3084 | 58 | 0 | 3 | 1 | **98.1%** | 142 s |
| on `8b4cf96`, interpreter | 3145 | 3058 | 77 | 5 | 1 | 1 | 97.4% | 142 s |
| on `8b4cf96`, JIT | 3124 | 3032 | 78 | 9 | 1 | 1 | 97.2% | 88 s |
| round 1, interpreter | 3145 | 2823 | 312 | 8 | 1 | 1 | 89.8% | 138 s |

What changed:

- **Security providers.** `bouncycastle.jar` and `conscrypt.jar` are on the
  boot class path with Conscrypt's natives (`libjavacrypto.so` over
  BoringSSL's `libcrypto.so` and `libssl.so`), so
  `sun.security.jca.Providers` initializes: `SecureRandom`
  (`AndroidOpenSSL`), `MessageDigest`, `UUID.randomUUID` and temporary files
  work. 197 of the 198 `bootclasspath` failures pass (the last needs Xalan);
  the java.nio.file tests that could not create their temporary directory
  now run and show their own causes (below).
- **NativeBN.** `libjavacore.so` links BoringSSL's `libcrypto_static` and
  registers `libcore.math.NativeBN` (patches/libcore/0001 is removed). The 3
  `missing-native` tests pass with the interpreter; with the JIT the two
  prime generation tests hang (below).
- **Time zone data.** `system/timezone` at the pinned tag gives the
  `com.android.tzdata` files (tzdata 2025c, format 10, with ICU's overlay).
  All 60 `tzdata` failures pass once the wall clock is fixed; before it, 3
  `TimeZoneTest` cases failed because ICU used the zones' rules of 1970.
- **Test resources.** The harness now packages
  `harmony-tests/src/test/resources` like core-tests (a serialization
  resource of `LocaleTest`).

By package (on `db2f059`, interpreter; pass / tests run):

| Package | Pass / tests |
| --- | --- |
| java.lang | 632 / 644 |
| java.math | 140 / 140 |
| java.util | 1105 / 1110 |
| java.util.concurrent | 266 / 266 |
| java.util.regex | 48 / 48 |
| java.io | 438 / 449 |
| java.nio, java.nio.charset | 44 / 45 |
| java.nio.file | 176 / 201 |
| java.text | 239 / 242 |

Failures by category (on `db2f059`; `tests/libcore/expectations.txt`, 61
lines):

| Category | Interpreter | JIT | What |
| --- | --- | --- | --- |
| `lx-syscall` | 29 | 29 | Sockets, `statvfs`, `FIONREAD`, `/dev` entries, mounts, ENOTTY from `tcgetattr` (round 1), plus what the providers hid: no inotify (`WatchService`, 14), setting file times (`futimens`/`utimensat`) ENOSYS (4) |
| `triage` | 13 | 16 | Round 1's, plus three tests the providers hid (`LinuxPathTest#test_toRealPath`, `#test_toURI`, `ObjectStreamClassSuidTest` for target SDK 23); with the JIT, three hangs (below, fixed since: their lines are removed) |
| `environment` | 8 | 8 | Unchanged |
| `harness` | 5 | 5 | Round 1's 3, plus `filesystemstest.jar` (a built test resource) for two `FileSystemsTest` cases |
| `icu-data` | 2 | 2 | Root-locale fallbacks for unknown locales |
| `bootclasspath` | 1 | 1 | No XML transformer (Xalan is not on the boot class path) |

**JIT hangs (found in round 2, fixed since).** With the JIT,
`BigIntegerTest`'s two prime generation tests (thousands of NativeBN
allocations each) gave no result in 60 s, and once
`StandardCharsetsEncoderTest` (UTF-16LE) did too. ART aborted with "Thread
suspend timeout" while the main thread and HeapTaskDaemon (in a GC) both
held the mutator lock shared and had spent over 100 s in system time. The
cause was the loader's interim `mincore` ([below](#jit-hang-resolved-mincore)).

## JIT hang resolved: mincore

Branch `p1-jit-hang`, 2026-11-28, on main at `11f740a`, with the ART root
of round 2 (Conscrypt, BoringSSL and the time zone data).

**Evidence.** A probe (1024 times `BigInteger.probablePrime(128)` and
`isProbablePrime(100)`) under `--mode jit` hung 5 of 5 runs (aborted with
the suspend timeout after 51 to 62 s, or still running at 90 s).
`ANDRUNA_PROFILE` with the new `ANDRUNA_PROFILE_EVERY=15` (periodic
report, since the process never exits normally) showed one `mincore`
(x86_64 number 27) on `[0x2000000, +0x11c0000)` taking **35 s**, and
another on 64 pages taking 0.7 s; the faults, retries and futexes were
not involved (no signals delivered, no `MADV_DONTNEED` window retries).

**Cause.** ART's concurrent copying collector clears its from-space after
every collection with `ZeroMemory(..., release_eagerly=false)`, which calls
`mincore` on each block: resident pages are zeroed and `MADV_FREE`d, the
others `MADV_DONTNEED`ed. The loader answered `mincore` (andruna-lx does
not have it yet) with one `VirtualQuery` per page. Windows computes each
answer's region size by walking the page tables to the end of the region,
so the cost of one query grows with the resident part of the region
(measured: 0.04 ms at 16 MiB, 0.5 ms at 64 MiB, 4 ms at 256 MiB, 18 ms at
1 GiB, all pages touched) and the loop is quadratic. lx commits a writable
mapping in one allocation, so ART's region space is one large region: 4544
queries at about 8 ms each. The collector (or a mutator running a
blocking GC) spends that in kernel time inside a GC phase, holding the
mutator lock shared, and a thread that suspends all threads times out.
The JIT makes it visible because compiled code allocates fast enough to
trigger collections back to back.

**Fix.** `crates/andruna-linker/src/interim.rs`: `mincore` checks the
range with one `VirtualQuery` per region (`ENOMEM` on free memory, as
before) and reads residency with `QueryWorkingSetEx`, 4096 pages per call.
A page is resident when it is in the working set, so committed pages
never touched are not, as on Linux; before, every committed page counted
as resident and ART zeroed whole blocks by hand. andruna-lx's own
`mincore` (lx round 3) uses the same working-set rule; when it lands the
interim entry stops being reached and can be deleted. Test:
`mincore_of_a_large_resident_range_is_fast_and_reports_untouched_pages`
(64 MiB, three quarters touched; the per-page version needs about 8 s).

**After.** The probe: 40 of 40 runs pass, 1.0 to 1.3 s each.
`BigIntegerTest` and `StandardCharsetsEncoderTest` with the JIT: 20 of 20
harness runs, all 20 tests pass in each; `test_probablePrime` 1.2 s,
`test_Constructor_IILjava_util_Random` 1.3 s. The three `triage` lines
are removed from `expectations.txt`. Full runs (350 classes, 3149 tests,
same machine, andruna-app release with the fix, the ART root of round 2):

| Mode | Tests | Pass | Fail | Skip | Timeout | Pass rate | Wall |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **JIT** | 3149 | 3092 | 54 | 3 | 0 | **98.3%** | 63 s |
| **interpreter** | 3149 | 3092 | 54 | 3 | 0 | **98.3%** | 171 s |

No unexpected failures. Four unexpected passes, the same in both modes and
unrelated to this fix: `ConsoleTest#testReadPassword`,
`#testReadPasswordWithPrompt` and
`FileInputStreamTest#test_available_on_nonIOCTL_supported_file` (listed
as `lx-syscall`; they depend on what the harness's standard input is) and
`OldThreadTest#test_yield` (timing, `triage`). Their lines are kept.

**Slow interpreter times.** In the interpreter the two prime tests take
36 s and 51 s with the fix (37 s and 108 s before it): the `mincore` cost
was part of the second, the rest is not a fault. `--mode int` passes
`-Xint`, so BigInteger's Java code in the boot class path is interpreted
too, by the switch interpreter. The probe takes 22 s with `-Xint`, 1.7 s
with the boot image's AOT code and no JIT (`--mode jit -Xusejit:false`)
and 1.0 s with the JIT; its profile under `-Xint` shows no system call or
exception cost (the 22 s are user time).

## Round 1 results (x64)

| Runner, mode | Tests | Pass | Fail | Crash | Timeout | Not run | Pass rate | Wall |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **andruna-app, interpreter** | 3145 | 2823 | 312 | 8 | 1 | 1 | **89.8%** | 138 s |
| **andruna-app, JIT** | 3145 | 2824 | 311 | 8 | 1 | 1 | **89.8%** | 90 s |
| S1 runner, interpreter | 3033 | 2441 | 236 | 354 | 0 | 0 | 80.5% | 231 s |
| S1 runner, JIT | 3029 | 2434 | 236 | 356 | 0 | 1 | 80.4% | 86 s |

By package (andruna-app, interpreter; pass / tests):

| Package | Pass / tests |
| --- | --- |
| java.lang | 615 / 644 |
| java.math | 137 / 140 |
| java.util | 1053 / 1110 |
| java.util.concurrent | 265 / 266 |
| java.util.regex | 47 / 48 |
| java.io | 413 / 449 |
| java.nio, java.nio.charset | 38 / 41 |
| java.nio.file | 43 / 204 |
| java.text | 212 / 242 |

"Not run": after a crash in a JUnitParams class the runner cannot skip
single parameter sets, so the rest of that class is not run (one class,
`StandardCharsetsEncoderTest`).

Pass rate = pass / (pass + fail + crash + timeout); skipped and not-run
tests are left out. Interpreter and JIT differ only by the intermittent
crash below and timing-sensitive tests.

Repeat run (same build, with the expectations file): interpreter 2792 pass
of 3111, 89.8%; JIT 2813 of 3127, 90.0%. The test count varies because a
crash in a JUnitParams class leaves the rest of that class unrun. Every
failure matched an expectation except `OldThreadTest#test_yield` (timing,
now listed) and two time zone tests that had crashed in the first JIT run
(their lines were limited to the interpreter; now unlimited).

After the JNI stub fix (finding 2; branch `phase1-jni-thread`: ART root
rebuilt with patch `art/0005`, andruna-app with the lx retry fix, same
machine), two runs per mode without the `art` expectation:

| Runner, mode | Tests | Pass | Fail | Crash | Timeout | Not run | Pass rate | Wall |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| andruna-app, interpreter | 3149 | 2835 / 2836 | 313 / 312 | 0 | 1 | 0 | 90.0% / 90.1% | 138 s / 137 s |
| andruna-app, JIT | 3149 | 2835 / 2835 | 313 / 313 | 0 | 1 | 0 | 90.0% / 90.0% | 78 s / 75 s |

No crash in any of the four runs, so every test runs (3149, also
`StandardCharsetsEncoderTest` to its end). Every failure matched an
expectation; the unexpected passes are the two timing-sensitive `triage`
tests.

### Failures by category (andruna-app)

Every failure of both andruna-app runs is listed in
`tests/libcore/expectations.txt` with its category (314 lines, the union of
both modes; counts are the first run). The `art` category of the first
round, an intermittent crash in compiled JNI stubs, is fixed (finding 2)
and its wildcard line removed:

| Category | Tests | What |
| --- | --- | --- |
| `bootclasspath` | 198 | The minimal boot class path has no Conscrypt or Bouncy Castle, so `sun.security.jca.Providers` fails its start-up check: no `SecureRandom` (so no `File.createTempFile` or `Files.createTempDirectory`: all 161 `java.nio.file` failures), no `MessageDigest` (serialization hashes, `ObjectStreamClass` SUIDs), no `UUID.randomUUID`; one test needs Xalan |
| `tzdata` | 60 | `/apex/com.android.tzdata` is not provided: every zone is GMT without rules (`TimeZone`, `Calendar` and `GregorianCalendar` DST arithmetic, `SimpleDateFormat` zone names and offsets) |
| `lx-syscall` | 26 | 11 from the wall clock (below); `/dev/null`, `/dev/zero` missing (2); `pipe2` and pipe channels (3), sockets (2), `statvfs` (1), `FIONREAD` for `available()` (1): ENOSYS; `tcgetattr` on a non-terminal does not fail with ENOTTY (2); no mounts in `/proc/mounts` for `FileStore` (3); no inotify for `WatchService` (1) |
| `environment` | 8 | Child processes (`Runtime.exec`, a child VM: 7); `System.load` of `liblog.so` through `java.library.path` (1) |
| `missing-native` | 3 | `libcore.math.NativeBN` (BoringSSL big numbers) is not in the guest `libjavacore.so` |
| `icu-data` | 3 | Locale fallbacks for unknown locales, a serialized `Locale` resource |
| `harness` | 3 | Two classes load `libjavacoretests.so` (test JNI library, not built); `LocaleInternalsTest` has no JUnit 3 test methods in this build |
| `triage` | 14 | Not yet explained (below), including two timing-sensitive tests |

## Findings

1. **The wall clock reads 1970 on andruna-app.** `System.currentTimeMillis()`
   returns 0 and `Instant.now()` is `1970-01-01T00:00:00Z`; `System.nanoTime()`
   is correct. Checked with a one-class probe on the same build: on the S1
   runner the same probe prints the current time. So `gettimeofday` or
   `clock_gettime(CLOCK_REALTIME)` on andruna-linker / andruna-lx (bionic's
   path, possibly its vDSO lookup) reports the epoch. Besides the two clock
   tests, it makes `Currency.getInstance(Locale)` return the currency valid in
   1970 (FRF for fr_FR, ATS for de_AT, BRB, SUR), and the two-digit year
   window of `SimpleDateFormat` start in 1890: 11 failures in all, all
   expected to pass once the clock is fixed. Owner: andruna-lx or
   andruna-linker.
2. **Intermittent crash in compiled JNI stubs (fixed).** A few random tests
   per run (8 in each andruna-app run, 2 in the S1 JIT run, none in the S1
   interpreter run) faulted in `artJniMethodStart` or `artJniMethodEnd`
   reading address 0. Two causes, both fixed on branch `phase1-jni-thread`:
   - ART's `art_jni_method_start`, `art_jni_method_end` and the other
     `JNI_SAVE_*_TRAMPOLINE` entry points, which a compiled JNI stub calls
     on its slow path (a suspend or checkpoint request pending), took
     `Thread::Current()` as the macro argument `gs:THREAD_SELF_OFFSET`.
     The ADR 0015 patches did not cover these arguments, so on Windows the
     trampolines passed TEB+0xd8 (0) as the `Thread*`. The S1 interpreter
     run calls natives through the generic JNI trampoline, which was
     patched, so it never took this path. Patch `art/0005` passes the TLS
     slot; the art stage of `build_guest.py` now fails on any `%gs:`
     access outside nterp.
   - andruna-lx's `madvise(MADV_DONTNEED)` decommits and recommits pages;
     a thread touching them in between faults and the fault is retried. The
     retry gave up after four faults on the same page by one thread, ever,
     and on a page still decommitted. ART clears its card table this way
     on every GC while mutators mark cards, so after a few GCs a card mark
     became a SIGSEGV. The retry now waits for open windows and counts
     per window.
   `JniStress` (spike S1 test programs; `andruna-app` tests
   `jni_stub_slow_paths_*`) reproduces both: native calls on 4 threads
   for 2 s while the main thread runs the GC and walks their stacks
   (runs that crashed, with the boot image / interpreter / JIT stubs):

   | ART root | andruna-lx | Boot image | Interpreter | JIT stubs |
   | --- | --- | --- | --- | --- |
   | before | before | 10 of 10 (`getpid` only) | | |
   | with 0005 | before | 17 of 20 | | 2 of 20 |
   | before | fixed | | 10 of 10 | 9 of 10 |
   | with 0005 | fixed | 0 of 20 | 0 of 10 | 0 of 20 |

   With the old ART root every crash is `artJniMethodEnd` or
   `artJniMethodStart` reading address 0; with only patch 0005, the lx
   retry refused card table faults after four of them.
3. **Time zone data (resolved in round 2).** 60 failures needed `/apex/com.android.tzdata`
   (`tzdata` and ICU's `icu_tzdata.dat`). The pinned AOSP projects
   (`aosp/aosp.lock`) do not include `system/timezone`, so the ART root has
   no time zone files; adding the project and staging its files into the
   ART root would turn these into passes.
4. **Security providers (resolved in round 2, ADR 0021).** The largest group (198) was the missing Conscrypt
   and Bouncy Castle: Android's `Providers` asserts that every configured
   provider loads. Building Conscrypt (Java and its BoringSSL natives) into
   the boot class path is the fix; until then file-system tests that create
   temporary directories cannot run.
5. **Linux layer gaps** seen from Java: `pipe2`, sockets, `statvfs`,
   `FIONREAD`, inotify, `/dev/null` and `/dev/zero`, mounts in
   `/proc/mounts`, ENOTTY from `tcgetattr` (26 tests, `lx-syscall`).
6. Interpreter and JIT give the same results except for the (fixed)
   intermittent crash and one timing-sensitive `ForkJoinPool` test. No JIT-only failure
   was found in this subset.

Not yet explained (`triage`, 14): `List.of().contains(null)`,
`Set.of().contains(null)` and two `Map.of()` cases do not throw
`NullPointerException` inside `ListOfTest`/`SetOfTest`/`MapOfTest`, although a
direct check of `List.of().contains(null)` does throw; `Record` reflection
(`isRecord()` false: possibly how d8 compiles the test's records);
`SystemTest#testSystemProperties_mutable` (NPE putting a null property);
`OldThreadTest#test_getState` hangs; `DecimalFormatTest#test_setMaximumIntegerDigits`;
`OldFileWriterTest#test_handleEarlyEOFChar_2`; `OldAndroidFileTest#testFile`;
a missing serialization resource; two timing-sensitive tests
(`ForkJoinPoolTest#testGetRunningThreadCount`, `OldThreadTest#test_yield`),
each failed once in four runs.

## The S1 runner (fallback)

`run.py --runner s1` runs the same jar on the spike S1 runner
(`dalvikvm` without a boot image, on the spike's own small guest libc).
It shows the same causes except the clock (its wall clock works), plus 354
crashes from its guest libc: `readdir64`
is not implemented there, so every `File.list()` crashes the VM in
`Java_java_io_UnixFileSystem_list0`, and ICU's start-up lists a directory,
which takes down most `java.text` and locale tests (category `s1-libc`).
`setmntent` is missing too. The expectations file covers andruna-app only; the S1 numbers are for
comparison.

## License choices

The test libraries are test tools only (ADR 0006, amendment of 2026-11-27):
fetched by `build.py` from Maven Central at pinned versions, checked by
SHA-256, never committed and never part of a shipped binary.

| Library | Version | License |
| --- | --- | --- |
| JUnit | 4.13.2 | EPL-1.0 |
| Hamcrest Core | 1.3 | BSD-3-Clause |
| JUnitParams | 1.1.1 | Apache-2.0 |

The runner and the harness scripts are our own code. libcore's tests and
test support code are compiled from the fetched AOSP tree, never copied
(ADR 0012).

## What remains

- Linux layer: inotify, file times, sockets, `statvfs`, `FIONREAD`, mounts
  (29 tests).
- Mockito for 8 dropped classes needs a dex-capable mocking backend (for
  example dexmaker); `CoreCompatChangeRule` needs Guava. Both are test-only
  and would be fetched like JUnit.
- Classes not yet selected: `java.net`, `java.util.zip`, `java.util.jar`,
  `java.time` (the ojluni-derived tests use TestNG), `javax.crypto`, and
  libcore's `dalvik`, `json`, `xml` and harmony suites.
- The first CI run of `.github/workflows/libcore-tests.yml` builds the guest
  from scratch; its cache then keeps the job short.
- arm64 and AOT modes: the harness supports more modes (`--mode jit0`, other
  runners), not measured here.
- Measured once on a shared machine; wall times are indicative only.

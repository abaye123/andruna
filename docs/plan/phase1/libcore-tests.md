# libcore tests

**Status:** 2026-11-27, first round (branch `phase1-libcore-tests`). The
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
- andruna-app runs with the boot image (dex2oat `--compile-boot`), so the
  boot class path runs as AOT code in both modes; `--mode int` interprets the
  test code, `--mode jit` JIT-compiles it.

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

## Results (x64)

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
crash below and one timing-sensitive test.

### Failures by category (andruna-app)

Every failure of both andruna-app runs is listed in
`tests/libcore/expectations.txt` with its category (314 lines, the union of
both modes, plus one line for the intermittent crash):

| Category | Tests | What |
| --- | --- | --- |
| `bootclasspath` | 198 | The minimal boot class path has no Conscrypt or Bouncy Castle, so `sun.security.jca.Providers` fails its start-up check: no `SecureRandom` (so no `File.createTempFile` or `Files.createTempDirectory`: all 161 `java.nio.file` failures), no `MessageDigest` (serialization hashes, `ObjectStreamClass` SUIDs), no `UUID.randomUUID`; one test needs Xalan |
| `tzdata` | 60 | `/apex/com.android.tzdata` is not provided: every zone is GMT without rules (`TimeZone`, `Calendar` and `GregorianCalendar` DST arithmetic, `SimpleDateFormat` zone names and offsets) |
| `lx-syscall` | 26 | 11 from the wall clock (below); `/dev/null`, `/dev/zero` missing (2); `pipe2` and pipe channels (3), sockets (2), `statvfs` (1), `FIONREAD` for `available()` (1): ENOSYS; `tcgetattr` on a non-terminal does not fail with ENOTTY (2); no mounts in `/proc/mounts` for `FileStore` (3); no inotify for `WatchService` (1) |
| `environment` | 8 | Child processes (`Runtime.exec`, a child VM: 7); `System.load` of `liblog.so` through `java.library.path` (1) |
| `missing-native` | 3 | `libcore.math.NativeBN` (BoringSSL big numbers) is not in the guest `libjavacore.so` |
| `icu-data` | 3 | Locale fallbacks for unknown locales, a serialized `Locale` resource |
| `harness` | 3 | Two classes load `libjavacoretests.so` (test JNI library, not built); `LocaleInternalsTest` has no JUnit 3 test methods in this build |
| `art` | about 8 per run | Intermittent fault in compiled JNI stubs (below); random tests |
| `triage` | 13 | Not yet explained (below) |

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
2. **Intermittent crash in compiled JNI stubs.** A few random tests per run
   (8 in each andruna-app run, 2 in the S1 JIT run, none in the S1
   interpreter run) fault in `artJniMethodStart` or `artJniMethodEnd`
   reading address 0: the stub passes a null `Thread*`, which suggests the
   thread register (ADR 0015) is not valid on some entry into compiled
   code that calls a native method. The S1 interpreter run has no boot image
   and calls every native method through the generic JNI trampoline, and it
   never crashes this way. The expectation for it matches the crash
   signature on any test, so the job does not fail on it; it must be fixed
   (owner: ART integration).
3. **Time zone data.** 60 failures need `/apex/com.android.tzdata`
   (`tzdata` and ICU's `icu_tzdata.dat`). The pinned AOSP projects
   (`aosp/aosp.lock`) do not include `system/timezone`, so the ART root has
   no time zone files; adding the project and staging its files into the
   ART root would turn these into passes.
4. **Security providers.** The largest group (198) is the missing Conscrypt
   and Bouncy Castle: Android's `Providers` asserts that every configured
   provider loads. Building Conscrypt (Java and its BoringSSL natives) into
   the boot class path is the fix; until then file-system tests that create
   temporary directories cannot run.
5. **Linux layer gaps** seen from Java: `pipe2`, sockets, `statvfs`,
   `FIONREAD`, inotify, `/dev/null` and `/dev/zero`, mounts in
   `/proc/mounts`, ENOTTY from `tcgetattr` (26 tests, `lx-syscall`).
6. Interpreter and JIT give the same results except for the intermittent
   crash and one timing-sensitive `ForkJoinPool` test. No JIT-only failure
   was found in this subset.

Not yet explained (`triage`, 13): `List.of().contains(null)`,
`Set.of().contains(null)` and two `Map.of()` cases do not throw
`NullPointerException` inside `ListOfTest`/`SetOfTest`/`MapOfTest`, although a
direct check of `List.of().contains(null)` does throw; `Record` reflection
(`isRecord()` false: possibly how d8 compiles the test's records);
`SystemTest#testSystemProperties_mutable` (NPE putting a null property);
`OldThreadTest#test_getState` hangs; `DecimalFormatTest#test_setMaximumIntegerDigits`;
`OldFileWriterTest#test_handleEarlyEOFChar_2`; `OldAndroidFileTest#testFile`;
a missing serialization resource; one `ForkJoinPool` timing failure.

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

- Fix the wall clock and the JNI stub crash (findings 1 and 2); then the
  clock-related expectations should be removed (they will show as
  unexpected passes).
- Time zone data and Conscrypt in the ART root (findings 3 and 4): about
  260 of the 314 known failures.
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

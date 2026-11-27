# libcore tests

Runs a selected subset of libcore's own tests (`libcore/luni/src/test`) on
ART on Windows, for the phase 1 exit criterion "selected libcore tests pass".
Results and failure analysis: [docs/plan/phase1/libcore-tests.md](../../docs/plan/phase1/libcore-tests.md).

| File | Role |
| --- | --- |
| `tests.txt` | The selection: globs of test source files in the fetched libcore, and removals with a reason |
| `build.py` | Fetches the test libraries, compiles the selection with javac and d8 into `out/libcore-tests/libcore-tests.jar` |
| `harness/src/andruna/libcoretest/Main.java` | The in-VM runner: runs each class with JUnit 4 and prints one result line per test |
| `run.py` | Starts ART once per class, restarts after crashes and hangs, matches results to the expectations, writes the summary |
| `expectations.txt` | Known failures, each with a category and a reason |
| `expect.py` | Suggests expectation lines for the unexpected failures of one or more runs (`results.json`), with a guessed category, for review |

## Running

Prerequisites (as in the CI job, `.github/workflows/libcore-tests.yml`):
`cargo xtask fetch` and `cargo xtask patch`, the boot class path
(`python spikes/s1/java/build_boot.py`), the ART root
(`python aosp/tools/build_guest.py --arch x64 sysroot bionic libcxx aosp art`),
andruna-app (`cargo build -p andruna-app --release --target
x86_64-pc-windows-gnullvm`), optionally its boot image
(`andruna-app --art-root out\guest\x64\art-root --compile-boot`), JDK 21 or
newer, and llvm-mingw's `x86_64-w64-mingw32\bin` on `PATH`.

```
cargo xtask libcore-tests --mode int          # build, then run with the interpreter
cargo xtask libcore-tests --no-build --mode jit
python tests/libcore/run.py --mode jit --filter "*.StringTest" --jobs 1
```

`run.py` writes `out/libcore-tests/<runner>-<mode>/`: `results.json` (every
test with status, time, reason and matching expectation), `summary.txt`,
`suggested-expectations.txt` (lines for the unexpected failures, with a
guessed category) and `logs/<class>.log` (the VM output). It exits with 1 when
a test fails that `expectations.txt` does not list.

Inputs can come from another checkout: `S1_AOSP` (AOSP tree), `S1_JAVA`
(boot class path build), `LIBCORE_D8` (`d8.jar`), `ANDRUNA_APP`,
`ANDRUNA_ART_ROOT`.

## The ART launcher

`run.py` starts the VM through a runner class (`RUNNERS`, default
`DEFAULT_RUNNER`, or `--runner` / `LIBCORE_RUNNER`):

- `app` (default): `andruna-app --art-root <root> --data <dir> --mode int|jit
  --classpath <jar> andruna.libcoretest.Main ...`, ART on andruna-linker with
  bionic, the production path. It uses the boot image when the ART root has
  one; `LIBCORE_APP_ARGS` adds options (for example `--no-boot-image`).
- `s1`: the spike S1 runner (`spikes/s1/art/run_art.py`: `dalvikvm` with no
  boot image, on the spike's own small guest libc), kept as a fallback
  (`S1_GUEST`, `S1_RUNNER`).

## Result protocol

The in-VM runner prints, on standard output:

```
##lt START <class>#<method>
##lt PASS <class>#<method> <ms>
##lt FAIL <class>#<method> <ms> <exception: message @ top frame>
##lt SKIP <class>#<method> <ms> <reason>
##lt CLASS <class> RAN <n> | ABSTRACT | LOADERROR <reason> | RUNNERERROR <reason>
##lt DONE
```

A START without its result means the VM crashed or hung in that test:
`run.py` records `crash` or `timeout` and runs the class again with that test
and the finished ones excluded (`--exclude-file`).

## Expectations

`expectations.txt` has one line per known failure:

```
<class>#<method> | <category> | <reason> [| <only> [| <match>]]
```

The test may be a whole class (`<class>`) or a glob. `<only>` limits the
line to runners (`s1`, `app`) and modes (`int`, `jit`, `jit0`). `<match>`
makes the line apply only when the failure reason contains that text; with
the test `*` it describes an intermittent failure that can hit any test (a
known crash signature). Categories
are listed in `run.py` (`CATEGORIES`): `lx-syscall`, `s1-libc`,
`missing-native`, `bootclasspath`, `tzdata`, `icu-data`, `environment`, `art`, `slow`,
`harness`, `triage`. A listed test that passes
is reported as an unexpected pass, so the list shrinks as fixes land.

## Test libraries

Fetched by `build.py` from Maven Central at pinned versions, checked by
SHA-256, into `out/libcore-tests/tools`. They are test tools only (ADR 0006,
amendment of 2026-11-27): never committed, never part of a shipped binary.

| Library | Version | License | Use |
| --- | --- | --- | --- |
| JUnit | 4.13.2 | EPL-1.0 | Test API and runner (`JUnitCore`) |
| Hamcrest Core | 1.3 | BSD-3-Clause | Matchers, required by JUnit 4 |
| JUnitParams | 1.1.1 | Apache-2.0 | Parameterized tests of some luni classes |

libcore itself (the tests and their support code) is fetched from AOSP and
compiled from `third_party/aosp/libcore`, never copied (ADR 0012).

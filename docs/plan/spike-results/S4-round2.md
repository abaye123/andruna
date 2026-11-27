# S4 round 2: real bionic on andruna-linker

**Status:** 2026-11-27, x64 run; arm64 built and link-checked, not run.
Decision proposed as [ADR 0016](../../decisions/0016-guest-bionic-system-calls.md).

Real bionic (`libc.so`, `libm.so`, `libdl.so`, scudo, LLVM libunwind
exported from `libc.so`) built for `<arch>-linux-android` from AOSP at the
pinned tag (`aosp/tools/build_guest.py`), with every system call stub
calling `__andruna_syscall`, loaded by the production loader
(`crates/andruna-linker`) with the bridge `ld-android.so` holding bionic's
loader-side state.

## Results (x64)

| Suite | Result | Notes |
| --- | --- | --- |
| `cargo test -p andruna-linker` | 27 unit + 4 integration pass | integration with `ANDRUNA_REQUIRE_GUEST=1`; includes arm64 link-only |
| `linker_test` (`crates/andruna-linker/tests/guest`) | pass | stdio, malloc, pthread, static and dynamic TLS, dlopen/dlsym/dladdr/dlclose with destructor, dl_iterate_phdr, IFUNC |
| `cxx_test` | pass | C++ exceptions across frames and threads, RTTI, `std::thread`, `thread_local`, iostream on libc++ |
| libbase gtest suite | **297 of 367 pass**, 33 fail, 37 end the process | S8 with the shim libc: 328 of 367 |
| bionic's own tests (21 files) | 1104 of 1327 pass, 142 fail, 11 skipped, 31 end the process, 34 death tests excluded | math 332/332, string 107/109, wchar 106/110, ctype, locale, uchar, wctype all pass |

`crates/andruna-linker/tools/run_gtest.py` runs a suite, excluding each
test that ends the process (and death tests, which fork) and running
again.

### Why libbase is below S8

S8's shim implemented files, properties and `/proc` itself on the host;
real bionic reaches them only through system calls, so what lx does not
have yet now shows up. None of the failures comes from the loader or the
binary format:

- System properties (22 tests, plus 6 `logging.LOG_*_enabled` that read a
  property): bionic's property area is a file mapped from
  `/dev/__properties__`; lx has no file-backed `mmap` and Andruna no
  property service yet. `WaitForProperty*` tests time out.
- `/proc/self/exe`, `/proc/self/maps`, `readlink` (`Realpath`,
  `GetExecutablePath`, `GetExecutableDirectory`, `StdioLogger`,
  `VerifyRunningWithScudo`, `process.find_ourselves`).
- `unlink` and symbolic links (`RemoveFileIfExists*`, `Readlink`,
  `symlink`), file modes (`WriteStringToFile2`).
- Sockets (16 `CmsgTest`), fork (11 death-style tests, `ForkSafe`),
  `ftruncate` (2), file-backed `mmap` (`mapped_file.smoke`).

### bionic's tests

Failures cluster in what lx lacks: signals (`pthread_kill`, `sigmask`,
`sigaction`-based tests), `fork`, `/proc`, `prctl` names, `clock_*` with
CPU-time clocks, `gwp_asan` (needs signals). One loader bug was found and
fixed by this suite: the thread's tid word was cleared before the thread
left its stack, so a joiner could unmap a stack still in use.

## Measurements

Link of S8's `libbase_test` tree (5 objects, 13,081 relocations, 8,610
RELR, 3,255 packed), release build, warm: 14 to 15 ms (first run 395 ms).
arm64 tree: 14,474 relocations (9,961 RELR, 3,270 packed, 5 TLSDESC), the
same counts as S8.

## lx additions made for this round

Standard streams as descriptors 0-2 (`fs::stream`), `getcwd`, `chdir`,
`mremap` (anonymous), `MADV_WIPEONFORK`/`KEEPONFORK` accepted.

## Not verified

- No arm64 guest code ran (objects build and link-only load; the runner
  command is in `crates/andruna-linker/README.md`).
- File-backed segment mapping (lx returns `ENOSYS`; segments are copied).
- Automatic LLDB registration (command file only, as S8).

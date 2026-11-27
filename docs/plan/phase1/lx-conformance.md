# lx conformance

**Status:** 2026-11-27, first phase 1 round of the andruna-lx track
(branch `phase1-lx`). The phase 1 exit criterion "lx conformance suite
green" is measured here with the suites of S4 round 2 / S8: libbase's
gtest suite and bionic's own tests (21 files), both built against real
bionic and run through andruna-linker.

## How it is measured

```
python aosp/tools/build_guest.py --arch x64          # or reuse out/guest
cargo build -p andruna-linker --release
python crates/andruna-linker/tools/run_gtest.py out/guest/x64/tests/libbase_test ^
    -L out/guest/x64/sysroot/usr/lib -L out/guest/x64/tests --timeout 120
python crates/andruna-linker/tools/run_gtest.py out/guest/x64/tests/bionic_tests ^
    -L out/guest/x64/sysroot/usr/lib -L out/guest/x64/tests --timeout 120
```

`run_gtest.py` excludes death tests (they fork) and every test that ends the
process, and reruns until the suite completes. `--timeout 120` bounds each
run; a test that hangs (for example a property wait) counts as "ended the
process". Both columns below use the same guest objects (one build of
bionic, libc++, libbase and the tests at the pinned AOSP tag), the same
machine (Intel Core Ultra 7 258V, Windows 11 build 26300, shared with other
work), and andruna-elf built from `main` at `40d7f82` (before) and from
this branch (after), release, `x86_64-pc-windows-msvc` host.

## Results (x64)

| Suite | Before | After |
| --- | --- | --- |
| libbase (367 tests) | 297 pass, 33 fail, 37 end the process | **307 pass**, 28 fail, 32 end the process |
| bionic tests (1327 listed, 34 death tests excluded) | 1104 pass, 142 fail, 11 skipped, 31 end the process | **1129 pass**, 115 fail, 14 skipped, 30 end the process |

No test that passed before fails after. The three tests that moved from
failed to skipped are `pthread_attr_setinheritsched_*_takes_effect`: they
skip when `SCHED_FIFO` is refused, which lx now reports honestly (`EPERM`,
as for a process without `CAP_SYS_NICE`).

Newly passing, by feature:

- Signals: `pthread_sigmask`, `pthread_sigmask64_SIGTRMIN`,
  `pthread_kill__0`, `__invalid_signal`, `__in_signal_handler`,
  `__exited_thread`, `big_enough_signal_stack`, `run_on_all_threads`.
- Scheduling and affinity: `pthread_setschedparam`, `pthread_setschedprio`,
  `pthread_attr_setinheritsched__PTHREAD_INHERIT_SCHED__PTHREAD_EXPLICIT_SCHED`,
  `pthread_getaffinity`, `pthread_getaffinity_np_failure`,
  `pthread_setaffinity`.
- Thread names (`/proc/self/task/<tid>/comm`, `prctl(PR_SET_NAME)`):
  three `pthread_setname_np__pthread_getname_np__*` tests.
- Names and files: `stdio.remove`, `stdio.rename`, `renameat`, `renameat2`,
  `fseek_overflow_32bit`, `freopen_null_filename_mode`,
  `stdlib.realpath__ELOOP`, `realpath__deleted`; libbase
  `RemoveFileIfExists`, `Realpath`, `ReadFileToString_WriteStringToFile_symlink`.
- File mappings: libbase `mapped_file.smoke`.
- `ftruncate`: `TestUtilsTest.CaptureStdout_smoke`, `CaptureStderr_smoke`.
- `/proc/self/maps`: `process.find_ourselves`, `VerifyRunningWithScudo`.
- Property area: `WaitForProperty_timeout`, `WaitForPropertyCreation_timeout`
  now time out as specified instead of hanging.

## Stack overflow on the alternate stack

lx delivers a signal by writing a Linux frame (`siginfo_t`, `ucontext_t`,
restorer) on the alternate stack and resuming the thread at the handler, so
the faulting stack only carries Windows' own exception dispatch. Measured
by `signal::tests::stack_overflow_is_delivered_on_the_alternate_stack`
(ART's layout: committed stack, 16 KiB protected at the bottom, every frame
probing `GAP` below its stack pointer, `SA_ONSTACK` handler), release,
three runs each:

| Build | Gap | Used below the faulting SP | Left of the gap |
| --- | ---: | ---: | ---: |
| x64, `x86_64-pc-windows-msvc` | 8 KiB | 3408 B | 4784 B |
| x64, `x86_64-pc-windows-gnullvm` | 8 KiB | 3344 B | 4848 B |
| x64, handler on the faulting stack (comparison) | | 8281 to 8429 B | none |
| arm64, CI runner (`windows-11-arm`) | 16 KiB | passes (`< 16 KiB` asserted); bytes not printed by CI | |

S1 round 4 measured 6944 bytes on the faulting stack with the handler run
there (1240 bytes left of ART's 8 KiB x64 gap). The difference is ART's
handler and the ucontext conversion, which now run on the alternate stack;
bionic gives every thread one (`sigaltstack` at thread start). On arm64
Windows' dispatch alone uses 7.1 KiB (S1), so ART's arm64 gap still needs
the 16 KiB patch; the test runs with 16 KiB there.

## What lx gained in this round

- Signals: `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`, `rt_sigreturn`,
  `kill`, `tkill`, `tgkill`, `rt_(tg)sigqueueinfo`, `rt_sigpending`,
  `rt_sigtimedwait`, `rt_sigsuspend`, `pause`; Windows exceptions become
  `SIGSEGV`/`SIGBUS`/`SIGILL`/`SIGFPE`/`SIGTRAP`; queued signals are
  delivered at system call exit; futex waits return `EINTR`; new threads
  inherit the mask; the main thread's tid is the pid.
- Files: private file mappings and read-only shared ones, `getdents64`,
  `unlinkat`, `mkdirat`, `renameat2`, `symlinkat`, `readlinkat`,
  `ftruncate`, `truncate`, `fsync`, `fdatasync`, `faccessat(2)`.
- `futex` requeue and wake-op, `sched_*` policy and affinity calls,
  `prctl` (names, dumpable, no-new-privs, VMA names).
- `/proc/self` (`exe`, `cmdline`, `status`, `maps`, `fd/`, `task/`).
- System properties: a read-only area at `/dev/__properties__` with the
  defaults of ADR 0019 (proposed).

## What still fails, by cause

libbase (60 not passing):

- Fork for death-style tests: `logging.CHECK`, `LOG_FATAL`, `PLOG_FATAL`,
  `LOG_FATAL_NOOP_ABORTER`, `no_destructor.*`, five `result.*`,
  `logging.ForkSafe` (12).
- Sockets: 16 `CmsgTest`.
- Setting properties (no property service until sysd): 17 `properties.*`
  and the 6 `logging.LOG_*_enabled` tests, which set a log tag property.
- `GetExecutablePath`, `GetExecutableDirectory`, `StdioLogger`: the test
  binary lies outside the guest file tree (andruna-elf mounts a temporary
  root), so `/proc/self/exe` has no Linux path to report.
- `file.Readlink`: a 4095-byte link target does not fit a Windows symbolic
  link (reparse data is limited to 16 KiB for both stored names).
- `WriteStringToFile2`, `RemoveFileIfExists_EACCES`: lx does not store
  permission bits.

bionic (115 failing, 30 ending the process), largest groups:

- `pthread_rwlock_*` wakeup and timeout tests (18) and
  `pthread_mutex_*_wakeup` / PI (8): they wait until `/proc/<tid>/stat`
  shows the other thread sleeping (`S`); lx's `/proc` has no `stat` and no
  `/proc/<tid>` for threads yet, and PI futexes are `ENOSYS`.
- Fork, `popen`, `system`, ptys and ttys (about 25).
- Timers (`timer_create`, POSIX timers with `SIGEV_SIGNAL`) and
  `clock_settime`, `clock_getres` on some clocks, `time(nullptr)` vs
  `gettimeofday` (14).
- `stdio` line reading (`getdelim`, `getline`, `getc`) on `tmpfile` and
  `/dev` streams, `fread_with_locked_file` (about 20): need `tmpfile`
  (`O_TMPFILE`) and `/dev/null`, `/dev/zero`.
- `gwp_asan_integration.*` (7): set properties and need `fork`.
- `stack_unwinding.unwind_through_signal_frame*`: the signal is sent with
  `kill(getpid(), ...)` and delivered from lx's system call exit, so the
  interrupted context is host code that the guest unwinder cannot walk.
- `android_mallopt.*` (7): allocation limits and zygote profiling, which
  need properties set at run time or `fork`.

## Not verified

- arm64 guest code through andruna-linker (the suites ran on x64 only);
  arm64 lx unit tests, including the signal and stack overflow tests, pass
  natively on the CI runner.
- The byte count of the arm64 stack overflow measurement (the test only
  asserts the bound on CI).
- ART's own fault handler and libsigchain on top of lx signals (ART runs
  through the S1 runner, not yet through andruna-linker).
- Cross-process behavior: properties, shared file mappings and signals are
  per process.

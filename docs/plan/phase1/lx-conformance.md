# lx conformance

**Status:** 2026-11-28, third phase 1 round of the andruna-lx track
(branch `p1-lx-round3`). The phase 1 exit criterion "lx conformance suite
green" is measured here with the suites of S4 round 2 / S8: libbase's gtest
suite and bionic's own tests (21 files), both built against real bionic
and run through andruna-linker. Earlier rounds' results follow the third's.

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

## Round 3 results (x64)

Same method and machine as rounds 1 and 2 (loaded by other work during
the runs). "Before" is andruna-elf built from `main` at `da8ef51`, "after"
is this branch, both release, `x86_64-pc-windows-gnullvm`; the guest
objects are the round 2 ones.

| Suite | Before | After |
| --- | --- | --- |
| libbase (367 tests) | 324 pass, 27 fail, 16 end the process | 324 pass, 27 fail, 16 end the process |
| bionic tests (1327 listed, 34 death tests excluded) | 1168 pass, 79 fail, 13 skipped, 28 end the process | **1194 pass**, 68 fail, 13 skipped, 13 end the process |

No test that passed before fails or ends the process after (checked test by
test). Newly passing, by feature:

- `/proc/version` and `uname` (ADR 0039): `stdio.getc`, `putc`,
  `fopen64_freopen64`, `fdopen_failures`, `fdopen_add_CLOEXEC`,
  `fdopen_remove_CLOEXEC`, `freopen_invalid_mode`,
  `freopen_add_CLOEXEC`, `freopen_remove_CLOEXEC`,
  `freopen_append_mode_and_ftell`, `fflush_POSIX_2008`,
  `cantwrite_EBADF`, `fprintf_to_read_only_stream`,
  `fputc_to_read_only_stream`, `fwrite_to_read_only_stream`, the four
  `wchar.fwide_*` tests (which ended the process before),
  `stdlib.ttyname_r_ENOTTY`, `unlockpt_ENOTTY`.
- `/proc/loadavg` and `sysinfo` loads: `stdlib.getloadavg`.
- `/proc/sys/kernel/pid_max`: `time.clock_getcpuclockid_ESRCH`.
- PI futexes: `pthread_mutex_timedlock_pi`, `pthread_mutex_clocklock_pi`,
  `pthread_mutex_timedlock_monotonic_np_pi`.

libbase is unchanged: its remaining failures are fork, setting properties,
the test binary outside the guest tree, long link targets and permission
enforcement (below).

## What lx gained in round 3

- **File mappings are section views** (`MapViewOfFile3` into
  placeholders) wherever the address and the file offset agree modulo
  64 KiB, which lx arranges for every mapping without `MAP_FIXED`.
  `MAP_SHARED` is the file's own pages, coherent with `read`/`write` and
  other mappings (the read-only snapshot of rounds 1 and 2 is gone);
  writable shared mappings work, and andruna-linker's interim fallback for
  them is deleted. `MAP_PRIVATE` is copy-on-write; `MADV_DONTNEED` gives
  written pages the file's contents again; pages wholly past the end of
  the file raise `SIGBUS`. Fixed mappings that Windows cannot view
  (offset and address not congruent modulo 64 KiB, or the view's first
  block taken) are copies, as before; shared copies are written back on
  `msync`, `munmap` and exit. Design and deviations:
  `crates/andruna-lx/src/mem/file.rs`.
- **Per-process `/proc/<pid>/stat` and `status`**: `NtGetNextThread`,
  `NtQueryInformationThread` and `NtQueryInformationProcess` instead of a
  snapshot of every process of the system.
- **Default signal actions** as on Linux (ADR 0040): terminate and core end
  the process with `0xE0A00000` plus the Linux wait status, stop suspends
  the process, an unclaimed fault in guest code ends it at once. The
  exit-code rule for supervisors is in `crates/andruna-lx/README.md`.
- `nanosleep`/`clock_nanosleep` end with `EINTR` and the time left;
  `EPOLLET` reports new events only (pipes, eventfd and timerfd count
  their own; other objects use the process-wide change count);
  `FUTEX_LOCK_PI`, `LOCK_PI2`, `TRYLOCK_PI`, `UNLOCK_PI` without priority
  inheritance.
- `uname`, `sethostname`, `sysinfo`, `getrusage`, `getpriority`/
  `setpriority` (nice values on Windows thread priorities, `RLIMIT_NICE`
  40), `getcpu`, `membarrier`, `mincore` (working-set residency, one query
  per region), `msync`; `/proc/version`, `/proc/loadavg`,
  `/proc/sys/kernel/{pid_max,osrelease,ostype,version,hostname}`. The
  kernel identity is ADR 0039.

## Loader fallbacks made dead and removed (round 3)

`crates/andruna-linker/src/interim.rs` is deleted: `uname`, `getrusage`,
`sysinfo`, priorities, `getcpu`, `membarrier`, `mincore`, `msync` and the
writable `MAP_SHARED` write-back are lx's. The loader calls
`andruna_lx::mem::sync_all` at exit and prints its crash report from lx's
fatal hook. Verified with both suites above and `cargo test -p
andruna-app` with `ANDRUNA_REQUIRE_GUEST=1` (9 of 9, ART root with
Conscrypt). dex2oat writing the boot image through writable shared views
gives the same `boot.oat` and `.vdex` files as before (`.art` files differ
between any two runs, also before this round).

## What still fails after round 3, by cause

bionic (68 failing, 13 ending the process):

- Fork, `popen`, `system`, `pthread_atfork_*`, `pthread_key_fork`,
  `timer_create` (forks), ptys and ttys, `quick_exit`, `_Exit` (about 30).
- `getdelim`, `getline`, `fgetln` (7): pass alone, fail after
  `tmpfile_TMPDIR` leaves `TMPDIR` naming a removed directory.
- `clock_getres` (4): Windows clocks have 100 ns resolution, reported
  honestly.
- Time zones (about 10): andruna-elf's temporary guest tree has no tzdata
  (andruna-app's ART root has it since ADR 0021).
- `gwp_asan_integration.*` (7) and `android_mallopt.*` (8): properties set
  at run time, `fork`.
- `stack_unwinding.unwind_through_signal_frame*` (2), `realpath*` (4),
  `pthread_mutex_pi_wakeup` (checks the waiter's scheduler state while it
  waits; lx's PI wait is a bounded `WaitOnAddress` loop),
  `pthread_setaffinity_np_failure`, `sscanf_ls_n`, `consistent_fpos_t`,
  `fprintf_failures_7229520`.
- Ending the process: `malloc.allocation_slack`,
  `android_mallopt.set_allocation_limit`, `pthread_create__mmap_failures`,
  `freopen_null_filename_*` (2), `fread_fwrite_int_overflow`,
  `exit_torture`, `strchr_chk_bounds`, `strrchr_chk_bounds`,
  `timer_create_EINVAL`, `localtime_rz`,
  `mktime_z`, `thread_local.dtor_for_main_thread`.

## Round 2 results (x64)

Same method, guest objects and machine as round 1. "Before" is `main` at
`921b6cf` (the end of round 1), "after" is this branch at `bafe5a0`.

| Suite | Before | After |
| --- | --- | --- |
| libbase (367 tests) | 307 pass, 28 fail, 32 end the process | **324 pass**, 27 fail, 16 end the process |
| bionic tests (1327 listed, 34 death tests excluded) | 1129 pass, 115 fail, 14 skipped, 30 end the process | **1168 pass**, 79 fail, 13 skipped, 28 end the process |

No test that passed before fails or ends the process after (checked test by
test from the result files). `stdlib.getloadavg` now ends the process
instead of failing: `sysinfo` (the loader's interim answer) lets it get
further, to an `fopen("/proc/loadavg")` that lx does not serve, and bionic's
FORTIFY `fscanf` aborts on the NULL stream. `stdio.fread_fwrite_int_overflow`
was skipped before (its memory check reads `sysinfo`, then `ENOSYS`); it
now runs, passes alone (1.1 s), and exceeded the 120 s round limit in the
suite run, where it allocates 2 GiB while the libbase suite runs beside it.

Newly passing, by feature:

- `/proc/<pid>/stat`, `/proc/<tid>/stat` and `/proc/<tid>` with the Windows
  scheduler's thread state: 18 `pthread_rwlock_*` wakeup and timeout tests,
  `pthread_mutex_{NORMAL,ERRORCHECK,RECURSIVE}_wakeup`,
  `pthread_barrier_destroy`.
- `startstack` in `/proc/self/stat`, `prlimit64` (`RLIMIT_STACK`):
  `pthread_attr_getstack__main_thread`, `..._in_signal_handler`.
- POSIX timers and `setitimer`: `timer_create_SIGEV_SIGNAL`,
  `timer_create_NULL`, `timer_settime_0`, `timer_settime_repeats`,
  `timer_create_multiple`, `timer_disarm_terminates`,
  `timer_delete_terminates`, `timer_delete_from_timer_thread`.
- `time`, `gettimeofday`, `clock_settime` (`EPERM`/`EINVAL`): `time.time`,
  `time.clock_settime`.
- `O_TMPFILE`, `/dev/null`, `/dev/zero`, `/dev/stdin` and friends:
  `tmpfile_TMPDIR`, `tmpfile_fileno_fprintf_rewind_fgets`,
  `dev_std_files`, `fread_with_locked_file`,
  `fread_unbuffered_pathological_performance`.
- Unix domain sockets with `SCM_RIGHTS`: all 16 libbase `CmsgTest`.
- Stored file modes and `fchown`: libbase `WriteStringToFile2`.

## What lx gained in round 2

- Pipes (`pipe2`), `poll`/`ppoll`, `select`/`pselect6`, `epoll`
  (`epoll_create1`, `epoll_ctl`, `epoll_pwait`, `epoll_pwait2`), `eventfd2`,
  over every lx descriptor type, with `EINTR` when a signal arrives.
- POSIX timers (`timer_create` with `SIGEV_SIGNAL`, `SIGEV_THREAD_ID`,
  `SIGEV_NONE`, `timer_settime`, `timer_gettime`, `timer_getoverrun`,
  `timer_delete`), `timerfd`, `setitimer`/`getitimer`/`alarm`
  (`ITIMER_REAL`), on one lx timer thread that exists only while a timer is
  armed.
- `AF_UNIX` sockets: `socketpair`, stream, sequenced-packet and datagram
  sockets, `bind`/`listen`/`accept4`/`connect` on file-system and abstract
  names (and autobind), `sendmsg`/`recvmsg` with `SCM_RIGHTS`,
  `SCM_CREDENTIALS`, `MSG_PEEK`, `MSG_TRUNC`, `MSG_CTRUNC`,
  `MSG_CMSG_CLOEXEC`, `shutdown`, `getsockname`, `getpeername`,
  `SO_PEERCRED`, `SO_PASSCRED`, timeouts and buffer sizes; `readv`/`writev`
  (one datagram per call on sockets). Other families are `EAFNOSUPPORT`.
  The host can receive datagrams sent to a name
  (`socket::register_sink`); the loader's logd stand-in uses it.
- File modes (ADR 0020, proposed): stored in an NTFS extended attribute at
  creation (mode less umask) and by `chmod`/`fchmod`/`fchmodat(2)`; `umask`;
  `chown` family (own ids succeed, others `EPERM`).
- `O_TMPFILE`; `/proc/self/fd/N` of a removed file reads `... (deleted)`,
  and opening it reopens that same file.
- `/dev/null`, `/dev/zero`, `/dev/full`, `/dev/random`, `/dev/urandom`
  (Windows' system generator), `/dev/stdin`, `/dev/stdout`, `/dev/stderr`,
  `/dev/fd`.
- `/proc/<pid>/stat`, `/proc/<tid>/stat`, `/proc/<pid>/task/<tid>/stat` and
  `/proc/<tid>`: state from the Windows scheduler (`S` waiting, `D` paging,
  `T` suspended, `R` otherwise), CPU times, thread count, start time,
  memory counters, `startstack`; `Umask` in `status` is the real umask.
  `/sys/devices/system/cpu/{online,possible,present}`.
- `prlimit64`/`getrlimit`/`setrlimit` (ADR 0020; `RLIMIT_NOFILE` enforced),
  `time`, `gettimeofday`, `settimeofday`/`clock_settime` (`EPERM`),
  `getppid` (the starting process, 0 once it ended), CPU-time clocks of
  other processes.

## Signals and the system call stack switch

Since the ART merge `__andruna_syscall` runs each system call on the
thread's Windows stack. lx used to deliver queued signals inside its
`syscall` export, that is on the Windows stack with the TEB describing it:
a handler without `SA_ONSTACK` then ran on the Windows stack, and a handler
that left with `siglongjmp` returned to guest code with the TEB still
describing the Windows stack and the loader still marked "in a system
call". Now the loader calls lx with `syscall_without_delivery` and delivers
with `signal::deliver_after_syscall` after `__andruna_syscall` is back on
the guest stack with the guest's TEB bounds.
`thread::tests::queued_signal_runs_on_the_guest_stack` (andruna-linker)
checks it on a guest thread: `tgkill` to itself, the handler's stack
pointer lies on the guest stack and the TEB base is the guest stack's. The
same change fixed delivery to a thread that was sent a signal before it
used any signal call (its state existed by tid but was not looked up).

## Loader fallbacks made dead and removed

lx now answers these, so their entries in andruna-linker's `interim.rs`
and `procfs.rs` were deleted: `getrlimit`, `setrlimit`, `prlimit64`,
`umask`, `chmod`, `fchmod`, `fchmodat`, `fsync`, `getppid`, the logd
`socket`/`connect`/`sendmsg` (logd is now an lx socket name), the loader's
`readv`/`writev`, `/proc/self/stat` and the `/sys/devices/system/cpu`
lists (`procfs.rs` is gone). Still in the loader: `uname`, `getrusage`,
`sysinfo`, `getpriority`/`setpriority`, `getcpu`, `membarrier`, `mincore`,
`msync` and writable `MAP_SHARED` file mappings. Verified with both suites
above and `cargo test -p andruna-app` with `ANDRUNA_REQUIRE_GUEST=1` (6 of
6 pass, ART root and test dex files of the ART-on-linker build).

## Start-up time

The lead reported Hello World start-up 35 to 90 ms slower after the lx
merge, with about 17,900 of 19,000 system calls being `clock_gettime`.
lx's own cost was measured with a release build calling lx's `syscall`
export 200,000 times: 65 ns per `clock_gettime(CLOCK_MONOTONIC)` call
through the export, 54 ns calling `time::clock_gettime` directly (machine
shared with the suites). 17,900 calls are about 1.2 ms in lx, so the
slowdown is not in lx's clock or dispatch; the loader's entry (stack
switch, TEB updates) is the next place to measure.

## Fork

`fork`, `vfork` and raw `clone` without `CLONE_VM | CLONE_THREAD` fail with
`ENOSYS` (andruna-linker), so `popen`, `system`, `posix_spawn` and death
tests fail honestly. What fork would take:

- A new Windows process with a copy of the address space: Windows has no
  user-mode fork of a Win32 process. The options are an NT "clone" process
  (`RtlCloneUserProcess`/`NtCreateUserProcess` with an inherited section;
  undocumented, not supported for Win32 subsystem processes, breaks loader
  and CRT state) or a copy made by Andruna: start a new process, map every
  lx region again (anonymous memory copied, file mappings re-opened,
  placeholders re-reserved at the same addresses) and resume the forking
  thread's register state in the child.
- lx state copied into the child: the descriptor table (Windows handles
  duplicated into the child, lx objects such as pipes and sockets made
  cross-process, which today they are not), signal actions and the
  calling thread's mask, the mount table, cwd, umask, limits, timers not
  inherited.
- Guest state that lives outside lx: the loader's object list and TLS, the
  thread pointer, the Windows stacks of the system call switch, the
  property area mapping.
- Only the calling thread survives; every lock another thread held stays
  held in the child, which bionic handles with `pthread_atfork`, but host
  locks (Windows heap, loader, lx mutexes) need the same care.

For Android itself the zygote's fork is replaced by starting app processes
directly (spec), so fork matters for apps that run shell commands
(`Runtime.exec` uses `posix_spawn`/`vfork`). The cheaper path is
`posix_spawn`/`vfork`+`exec` only: a new process started from a fresh
image with the descriptor actions applied, which needs cross-process
descriptors but no address-space copy. Recommended as the first step when
a phase needs process creation.

## What still fails after round 2, by cause

libbase (43 not passing):

- Fork for death-style tests (12): `logging.CHECK`, `LOG_FATAL`,
  `PLOG_FATAL`, `LOG_FATAL_NOOP_ABORTER`, `no_destructor.*`, five
  `result.*`, `logging.ForkSafe`.
- Setting properties (no property service until sysd): 17 `properties.*`
  and 6 `logging.LOG_*_enabled`.
- `GetExecutablePath`, `GetExecutableDirectory`, `StdioLogger`: the test
  binary lies outside the guest file tree.
- `file.Readlink`: a 4095-byte link target does not fit a Windows symbolic
  link.
- `RemoveFileIfExists_EACCES`: needs permission bits enforced for a
  non-root user; lx records them only (ADR 0020).

bionic (79 failing, 28 ending the process), largest groups:

- Fork, `popen`, `system`, `pthread_atfork_*`, `pthread_key_fork`,
  `timer_create` (forks), ptys and ttys (about 30).
- `/proc/version` and `uname` (`getc`, `putc`, `fopen64_freopen64`,
  `fdopen_failures`, `freopen_invalid_mode`, `fflush_POSIX_2008`, and
  `getdelim`, `getline`, `fgetln` when run after `tmpfile_TMPDIR`, which
  leaves `TMPDIR` naming a removed directory; alone they pass): reporting a
  Linux kernel identity is an application-visible default that needs an
  ADR (the loader still answers `uname` itself).
- `clock_getres` of `CLOCK_REALTIME`, `CLOCK_MONOTONIC`, `CLOCK_BOOTTIME`
  and `timespec_getres` (4): the tests expect 1 ns; Windows clocks have
  100 ns resolution, which lx reports honestly.
- Time zones (`mktime_*`, `strftime_*`, `strptime_Z`, `tzalloc_nullptr`,
  `bug_*`, `localtime_rz`, `mktime_z`; about 11): tzdata is not in the
  guest tree.
- PI mutexes (4) and `pthread_mutex_pi_wakeup`: `FUTEX_LOCK_PI` is
  `ENOSYS`; `setpriority` is the loader's no-op.
- `gwp_asan_integration.*` (7) and `android_mallopt.*` (7): properties set
  at run time, `fork`.
- `stack_unwinding.unwind_through_signal_frame*` (2): the interrupted
  context of a signal delivered after a system call is host code.
- `realpath`, `realpath__dot*` (`/proc/self/exe` outside the guest tree),
  `realpath__component_after_non_directory` (`..` and `.` are resolved
  lexically, so `/dev/null/.` is `/dev/null`).
- `/proc/loadavg`, `/proc/sys/kernel/pid_max` (`getloadavg`,
  `clock_getcpuclockid_ESRCH`): not served.
- `timer_create_EINVAL` waits until `/proc/self/status` shows one thread;
  Windows thread-pool threads keep the count above 1.

## Round 1 results (x64)

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

## What lx gained in round 1

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
  defaults of ADR 0019 (accepted).

## What failed after round 1, by cause

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
- Cross-process behavior: properties, shared file mappings, signals, pipes
  and Unix sockets are per process (a socket bound in another process
  cannot be reached).
- `EPOLLET` and interruptible sleeps (done in round 3; the bionic suite
  has no edge-triggered epoll test, the lx unit tests cover them).
- Timer precision beyond the bionic tests' 500 ms windows.
- Modes on volumes without extended attributes.

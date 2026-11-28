# 0039 - Kernel identity and system figures reported by lx

**Status:** proposed, 2026-11-28

## Context
Applications and libraries read the kernel's identity (`uname`,
`/proc/version`, `/proc/sys/kernel/osrelease`; Java's `os.version` comes
from `uname`), system figures (`sysinfo`, `/proc/loadavg`,
`/proc/sys/kernel/pid_max`) and set thread priorities (`setpriority`,
`Process.setThreadPriority`). Until now andruna-linker answered `uname`
with an invented `6.6.0-andruna` and `getpriority` with a constant, and lx
served none of these files. bionic's stdio tests use `/proc/version` as a
readable file. The values are visible to every application, so they are a
maintainer decision (compatibility policy).

## Decision
- `uname`: sysname `Linux` (the interface lx implements); release
  `6.19.0-andruna`: 6.19 is the version of the Linux UAPI headers in the
  guest's bionic at the pinned AOSP tag, which is the interface lx is
  written against, and `-andruna` names the implementation; version
  `#1 SMP PREEMPT Andruna lx <crate version>`; machine `x86_64` or
  `aarch64`; nodename `localhost` (Android's, instead of the Windows
  computer name, which often holds the user's name); domainname `(none)`.
  `sethostname` and `setdomainname` fail with `EPERM`.
- `/proc/version` is `Linux version 6.19.0-andruna (andruna-lx <version>)
  <uname version>`; `/proc/sys/kernel/{osrelease,ostype,version,hostname}`
  repeat `uname`; `pid_max` is 4194304 (Linux's 64-bit limit).
- Windows keeps no load average. `sysinfo` loads and `/proc/loadavg` are an
  estimate of busy processors (system processor busy time over elapsed
  time, times the processor count), averaged over 1, 5 and 15 minutes from
  the samples taken when they are read. Process and thread counts describe
  this process only, as lx's `/proc` does.
- `RLIMIT_NICE` is 40, as Android's init sets it for the zygote and so for
  every app (this changes ADR 0020, which had 0): apps may lower their
  threads' nice values down to -20, which lx maps onto Windows thread
  priorities (-20 to -15 highest, -14 to -5 above normal, -4 to 4 normal,
  5 to 14 below normal, 15 to 19 lowest). Real-time scheduling stays
  refused.

## Consequences
- Code that compares the kernel version (feature checks by version) sees a
  recent kernel; lx still answers `ENOSYS` or `EINVAL` for what it does not
  implement, so such code gets Linux's own "unsupported" answers.
- The release changes when the AOSP tag (and with it bionic's UAPI
  headers) changes; the crate version identifies the implementation.
- The load average is an honest estimate, not the run-queue length Linux
  reports; it reads as 0 to the processor count.
- RenderThread and other display threads get above-normal Windows priority,
  as on Android; a misbehaving app can only affect its own process's share
  within Windows' normal priority class.

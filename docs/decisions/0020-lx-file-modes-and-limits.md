# 0020 - File modes, umask and resource limits reported by lx

**Status:** proposed, 2026-11-27

## Context
Applications read file modes (`stat`, `access`), set them (`chmod`,
`open`/`mkdir` modes, `umask`) and read resource limits (`getrlimit`:
bionic derives the main thread's stack from `RLIMIT_STACK`, ART and
libraries size descriptor tables from `RLIMIT_NOFILE`). NTFS has no Linux
mode bits and Windows has no resource limits, so lx must choose what to
store and what to report. These values are visible to every application,
so they are a maintainer decision (compatibility policy).

## Decision
- lx stores a file's Linux mode in an NTFS extended attribute
  (`ANDRUNA.MODE`, 4 bytes): when lx creates a file, directory or socket
  name (requested mode less the umask) and on `chmod`/`fchmod`/`fchmodat`.
  `stat` and `faccessat` report it. Files without it (created outside lx)
  report a mode derived from Windows attributes: `0644`, `0444` when
  read-only, `0755` for directories.
- Mode bits are reported, not enforced; Windows access control decides
  what the process may open. Every file belongs to the process's user and
  group; `chown` to other ids is `EPERM`.
- The initial umask is `077`, as Android gives application processes.
- Resource limits describe the real process: `RLIMIT_STACK` is the main
  thread's stack size; `RLIMIT_NOFILE` is lx's descriptor table size
  (32768) and is enforced; `RLIMIT_CORE` is 0 (no core files are written);
  `RLIMIT_NICE` and `RLIMIT_RTPRIO` are 0 (raised and real-time priorities
  are refused); all other limits are unlimited. Lowering a limit other than
  `RLIMIT_NOFILE` is recorded but not enforced.

## Consequences
- `chmod` followed by `stat` behaves as on Linux, including `+x` checks
  through `access`, within lx and across runs (the attribute stays with
  the file, also when it is copied with Windows tools that keep extended
  attributes).
- Files placed in an app's directories by Windows tools show the derived
  modes until a program changes them.
- A program that relies on lowered `RLIMIT_AS`, `RLIMIT_DATA`,
  `RLIMIT_FSIZE` or `RLIMIT_CPU` to fail allocations, writes or runaway
  loops does not get those failures; this is listed as a deviation of lx.
- Volumes without extended attributes (FAT, some network shares) keep the
  derived modes.

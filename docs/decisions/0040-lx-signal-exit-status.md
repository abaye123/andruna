# 0040 - Exit status of a process ended by a signal

**Status:** proposed, 2026-11-28

## Context
On Linux a process killed by a signal has a wait status that says so
(`WIFSIGNALED`, the signal number, `WCOREDUMP`). Android's activity
manager and tombstone handling tell a native crash (`SIGSEGV`, `SIGABRT`)
from a normal exit that way. Windows has only a 32-bit exit code. lx used
`128 + signal`, which `exit_group(139)` can produce as well, and an
unclaimed fault in guest code could fall through to Windows' exception
dispatch, which sometimes let the process exit with 0. sysd's app
lifecycle work needs a status it can rely on.

## Decision
- A signal's default action "terminate" or "core" ends the process with
  `TerminateProcess(0xE0A00000 | wait_status)`, where `wait_status` is the
  Linux wait status of a signal death: the signal number in bits 0 to 6
  and `0x80` when the action is "core". `0xE0A0` is an NTSTATUS error
  value with the customer bit set, so the code cannot be an `exit_group`
  status (0 to 255) or a Windows exception code.
- A synchronous fault in guest code (outside any Windows image) whose
  signal is at its default action, ignored or blocked ends the process
  that way at once, as Linux forces the default action. Faults in host
  code stay with Windows' structured exception handling.
- "stop" suspends the whole process (`NtSuspendProcess`); a supervisor
  continues it with `NtResumeProcess`. "ignore" discards the signal.
- `andruna_lx::signal::decode_exit_code` and `crates/andruna-lx/README.md`
  give the rule to supervisors.

## Consequences
- sysd reports a crash from the exit code alone: `0xE0A0008B` is an
  unhandled `SIGSEGV`, `0xE0A00086` `abort()`, `0xE0A00009` `SIGKILL`.
- Tools that print exit codes show large hexadecimal values for signal
  deaths instead of 134 or 139.
- No core file is written; a supervisor that wants a dump takes it itself
  (the host's fatal hook runs before the process ends).

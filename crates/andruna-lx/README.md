# andruna-lx

The Linux layer: Linux system call and process semantics on Windows, for
the guest bionic and everything above it. The design is in
[docs/spec/03-components.md](../../docs/spec/03-components.md#andruna-lx-the-linux-interfaces);
what it passes and what it does not is recorded in
[docs/plan/phase1/lx-conformance.md](../../docs/plan/phase1/lx-conformance.md).
Each module's documentation lists its deviations from Linux.

## Process exit codes (for supervisors)

Decided in [ADR 0040](../../docs/decisions/0040-lx-signal-exit-status.md)
(proposed).

A process that runs lx ends in one of three ways, and its Windows exit code
tells them apart. `andruna_lx::signal::decode_exit_code` decodes it; a
supervisor that does not link lx can apply the same rules:

| Exit code | Meaning |
| --- | --- |
| `0` to `255` | `exit_group(status)`: a normal exit with that status (`WIFEXITED`). |
| `0xE0A000xx` with `xx & 0x7f` not 0 | Ended by a signal's default action (`WIFSIGNALED`): the signal number is `xx & 0x7f`, and bit `0x80` is set when the action is "core" (`WCOREDUMP`). The low byte is the Linux wait status of a signal death. |
| anything else | A Windows status: an unhandled exception in host code (for example `0xC0000005`), `TerminateProcess` from outside, a fail-fast (`0xC0000409`). |

Signals whose default action is "core" (a crash, in Android's terms):
`SIGQUIT`, `SIGILL`, `SIGTRAP`, `SIGABRT`, `SIGBUS`, `SIGFPE`, `SIGSEGV`,
`SIGXCPU`, `SIGXFSZ`, `SIGSYS`. So `abort()` ends with `0xE0A00086` and an
unhandled `SIGSEGV` with `0xE0A0008B`; `SIGKILL` gives `0xE0A00009` and
`SIGTERM` `0xE0A0000F`. Android's activity manager treats a native crash
(a "core" signal) and a Java crash (the runtime calls `exit` or kills itself
with `SIGKILL` after the uncaught-exception handler) differently; the
supervisor can report the signal number and the core bit.

`0xE0A0` makes the code an NTSTATUS error value with the customer bit set,
so it can never be an `exit_group` status (0 to 255) or a Windows
exception code. No core file is written.

How the default actions are taken:

- "terminate" and "core": `TerminateProcess` with the code above, after
  the host's fatal hook (`signal::set_fatal_hook`; andruna-linker prints
  the guest backtrace there) and one line on standard error.
- A synchronous fault in guest code (anything outside a Windows image)
  whose signal is at its default action, ignored or blocked ends the
  process the same way, as Linux forces the default action then. A fault
  in host code (a Windows image) is left to Windows' structured exception
  handling.
- "stop" (`SIGSTOP`, `SIGTSTP`, `SIGTTIN`, `SIGTTOU`): every thread of the
  process is suspended (`NtSuspendProcess`). Nothing inside the process
  can continue it; a supervisor continues it with `NtResumeProcess`.
- "ignore" (`SIGCHLD`, `SIGURG`, `SIGWINCH`, and `SIGCONT`'s "continue"
  inside a running process): the signal is discarded.

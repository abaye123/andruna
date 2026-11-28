# 0037 - App host channel and process lifecycle

**Status:** proposed, 2026-11-27

## Context
ADR 0035 starts every app process suspended in its package's AppContainer;
ADR 0036 gives it an inherited pipe to sysd instead of the named pipe. The
app process (`andruna-app`) needs its assignment (package, code, data
directory, boot image view) and sysd needs to know how it ended, including
a crash summary for the manager's crash notice (`AppCrashed`, event type 3).
A crash often ends the process abruptly: lx ends it with
`TerminateProcess` on a fatal signal, and a fault in native code ends it
with an exception code, so the process itself cannot be relied on to
report its crash.

## Decision
- **Channel:** two anonymous pipes per app process, one end each in the
  launch handle list; their values are in `ANDRUNA_HOST_CHANNEL`. The
  protocol (`andruna_ipc::host`) uses the frame layout of the launcher
  protocol with its own version and kinds: `Hello`, `Attach`, `Running`,
  `Refused`, `MissingFunctions`, `Exit` (status and an optional crash
  report), and `Stop` from sysd. `Attach` names the entry: a class's
  `main` now, the framework entry (`ActivityThread.main` through an
  `app_process`-like start) later; the host refuses the framework entry
  until the framework is part of the runtime.
- **Standard output and error** of the app are pipes that sysd reads, not
  messages on the channel, so nothing written before an abrupt end is
  lost. sysd logs them and keeps the last 64 lines of standard error.
- **Crash reports** are made by sysd from that tail when the app did not
  end with status 0 and sysd did not stop it: an uncaught exception (the
  runtime's `Exception in thread ...` and its first frames), a native fault
  (the linker's fault report), a fatal signal or abort, or the last lines.
  The summary is the `reason` of `AppCrashed`.
- **Lifecycle:** one process per package (starts are serialized); a start
  answers only after the host sent `Running` (or with the host's refusal);
  `Stop` asks the app to end and kills it after 2 s; a stopped app is not
  a crash. Packages are read through a `PackageStore` trait: the package
  database (andruna-pkg) first, then hand-installed main-class programs
  (`system\packages\<package>.txt`) for development and tests.
- **Process creation** in sysd holds one lock from the inheritable handle
  duplicates to `CreateProcess`, so no other child inherits them.

## Consequences
- A crash report is available for every way an app can end, without
  trusting the app process.
- The framework entry is a change in andruna-app (its plan) and in the
  package store (which entry a package has), not in sysd's lifecycle.
- Stopping an app is in the supervisor's API but not yet a launcher or
  manager request (the 0x0010 range belongs to the product track).
- The hand-installed record format is for development and tests only.

# andruna-sysd

The system server skeleton: one process per user and Windows session. See
[03-components.md](../../docs/spec/03-components.md#andruna-sysd-system-server)
and [02-architecture.md](../../docs/spec/02-architecture.md#processes).

What exists now:

- **Single instance** per user, session and instance name (named mutex in
  the session namespace). A second sysd exits with code 3.
- **Headless**: windows subsystem, one hidden top-level window that gets
  `WM_QUERYENDSESSION`/`WM_ENDSESSION` at logoff.
- **Logs** in `%LOCALAPPDATA%\Andruna\logs\sysd.log` (and `host.log`), one
  logfmt line per event, rotated at 1 MiB with four old files kept.
- **Idle shutdown**: exits 30 s (`--idle-timeout-ms`) after the last client
  disconnected and the last host process ended.
- **Pipe server** with the protocol of [andruna-ipc](../andruna-ipc/README.md):
  ping, status, start package or activity, focus window, list packages,
  the event stream, missing-function counters.
- **Events** (`events.rs`): the last 256 events for `WaitEvents` long polls.
  An app that crashed (ended without status 0 and not stopped by sysd) is
  published as AppCrashed with the crash summary. Connections that wait
  for events do not keep sysd running.
- **Missing-function counters** (`diagnostics.rs`): reports from host
  processes sysd started (identified by process id) are added up in
  `diagnostics\<package>.missing`.
- **Package manager** ([andruna-pkg](../andruna-pkg/README.md)): inspect,
  install, uninstall and list requests (`src/pkg.rs`). The product writes
  Start menu shortcuts and "Installed apps" entries; with `--root` (tests,
  development) shortcuts go to `<root>\start-menu` and no registry entry is
  written. dex2oat runs through `andruna-app.exe` next to sysd when it and
  its ART root exist; otherwise installs report the compile step as
  unavailable.
- **App processes** (`supervisor.rs`, `apps/`, ADRs 0035, 0036, 0037,
  0038): one `andruna-app --host` per package, created suspended in the
  package's AppContainer and a kill-on-close job; sysd maps the boot image
  view into it, resumes it, sends the assignment over the host channel
  (`andruna_ipc::host`), logs its standard output and error, and builds
  an exit report (status, crash report from standard error). Start,
  focus, stop, one process per package. See
  [app lifecycle](../../docs/plan/phase2/app-lifecycle.md).
- **What a package runs** comes from a `PackageStore`: the package
  database (`DatabaseStore`, APKs start through the framework entry, which
  the host refuses until the framework is in the runtime), with
  hand-installed main-class programs (`system\packages\<package>.txt`,
  `RecordStore`) for development and tests (`apps/store.rs`).

The earlier placeholder host (`andruna-sysd --host <package>`, a window
saying it is a placeholder) is still in the executable but no longer
started. Not implemented: the warm host (ADR 0035), Binder, services,
splash.

**AppContainer** (`src/appcontainer/`, spike S7, ADRs 0035 and 0036): per-package
profiles (`install`/`uninstall` hooks for the package manager, salted
names), directory security for `data\<package>` and `app\<package>`, the
`andrunaRuntime` capability for the install directory, capabilities from
granted permissions, `launch` inside the container (handle list, job,
suspended start, the handle values in an environment variable) and
`process_sid`/`pid_sid` for peer identity. Process creation holds one
lock while inheritable handle duplicates exist (`spawn_lock`).

```
andruna-sysd [--instance <name>] [--root <dir>] [--idle-timeout-ms <n>] [--ready-event <handle>]
             [--app-exe <andruna-app.exe>] [--art-root <dir>]
andruna-sysd --host <package> [--activity <name>] [--root <dir>]
```

`--app-exe` and `--art-root` (or `ANDRUNA_APP_EXE` and `ANDRUNA_ART_ROOT`)
replace `andruna-app.exe` and `art\` next to sysd, for development.

`--root` replaces `%LOCALAPPDATA%\Andruna` and `--instance` the `default`
instance; both exist for tests and development. `--ready-event` is the
handle value of an inherited event that sysd signals once the pipe accepts
clients (the launcher's startup handshake).

Exit codes: 0 clean shutdown, 2 bad command line, 3 already running,
4 startup failed.

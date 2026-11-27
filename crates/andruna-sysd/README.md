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
  ping, status, start package, focus window, list packages.
- **Supervision**: host processes run in a kill-on-close job object, their
  exit is tracked, and their window can be focused.
- **Installed packages** are the directories `app\<package>\`; nothing
  installs packages yet, so the list is empty on a real machine.

**Placeholder.** The host process is meant to be `andruna-app.exe`, which
does not exist yet. Until it does, sysd starts its own executable as
`andruna-sysd --host <package>`. That process runs no app code: it logs
`event=placeholder_host` and shows a window (English or Hebrew, from
`res/`) saying that it is a placeholder. `host_started` log lines carry
`placeholder=true`. Not implemented: AppContainer, the warm host pool,
Binder, services, splash.

```
andruna-sysd [--instance <name>] [--root <dir>] [--idle-timeout-ms <n>] [--ready-event <handle>]
andruna-sysd --host <package> [--root <dir>]
```

`--root` replaces `%LOCALAPPDATA%\Andruna` and `--instance` the `default`
instance; both exist for tests and development. `--ready-event` is the
handle value of an inherited event that sysd signals once the pipe accepts
clients (the launcher's startup handshake).

Exit codes: 0 clean shutdown, 2 bad command line, 3 already running,
4 startup failed.

# ui

Andruna.Manager, the management app, in C# with WinUI 3
([ADR 0005](../docs/decisions/0005-winui3-management-app.md)): unpackaged,
self-contained, Windows App SDK pinned in the project file.

| Path | Contents |
| --- | --- |
| `src/Andruna.Manager.Core/` | No WinUI: the sysd client (`Sysd/`), settings, view models, language choice. Unit tested |
| `src/Andruna.Manager/` | The WinUI app: window, pages, resource lookup, strings |
| `src/Andruna.Manager/Strings/<language>/Resources.resw` | All user-facing text, `en-US` and `he-IL` |
| `tests/Andruna.Manager.Tests/` | xUnit tests; the fake sysd lives only here |
| `tools/make-icon.ps1` | Regenerates `Assets/Andruna.ico` |

## Build, test, run

```powershell
dotnet build ui/src/Andruna.Manager -r win-x64      # or -r win-arm64
dotnet test  ui/tests/Andruna.Manager.Tests
ui/src/Andruna.Manager/bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64/Andruna.Manager.exe
```

`installer/build.ps1` publishes the Release build for the installer.

Command line: `--page settings` opens Settings, `--page notices` the
third-party notices, `--app <package>` the page of one installed app; the
default is the Library.

## Pages

- **Library** (M-1): the apps sysd reports. When sysd is not installed, not
  running or answers wrongly, the page says which, and shows no apps. The
  "Install APK" button and drag and drop accept `.apk`, `.apks`, `.xapk` and
  `.apkm`, and then say that installing is not available yet.
- **App page** (M-2): header with what sysd reports for the package, and the
  sections permissions, storage, window size, graphics processor, start with
  Windows, uninstall. Their actions are disabled until sysd implements them.
- **Settings** (M-5, M-6): language (Windows, Hebrew, English), theme
  (Windows, light, dark), About with version, architecture, service state and
  third-party notices. A language change asks for a restart, after which the
  whole window, including the frame, is laid out right to left for Hebrew.
- **Third-party notices** (M-6): opened from About, back to Settings with the
  back button, the "Settings" breadcrumb or Alt+Left. Shows
  `THIRD_PARTY_NOTICES.txt` from the app folder, read asynchronously, in a
  read-only monospace text box (selectable, Ctrl+A / Ctrl+C) that stays left
  to right in Hebrew because the notices are English. A missing or unreadable
  file is shown as an error, not as an empty text. "Open in text editor"
  opens the file with its Windows association.

## Strings

Every user-facing string is in both `.resw` files under the same key. XAML
uses `{l:Str Key=Name}`, code uses `Loc.Get` / `Loc.Format`, view models get
an `IStrings`. The tests check that both languages have the same keys and
placeholders and that every key used in code exists.

## Settings file

`%LOCALAPPDATA%\Andruna\system\manager.json`, for example
`{ "language": "hebrew", "theme": "dark" }`. A missing or broken file gives
the defaults.

## sysd protocol (client side)

The manager speaks the binary pipe protocol of
[andruna-ipc](../crates/andruna-ipc/README.md), version 1, which is
authoritative; `Sysd/SysdWire.cs` is its C# codec and `Sysd/SysdClient.cs`
the client.

- Pipe `\\.\pipe\Andruna.sysd.<user SID>.<session id>.default`
  (`SysdEndpoint`; the session id comes from `ProcessIdToSessionId`).
- Opened with `CurrentUserOnly` and identification-level impersonation;
  before anything is sent, `GetNamedPipeServerProcessId` and the server
  process token must show the same user. Otherwise the manager reports that
  it does not trust the service.
- One connection, kept open while the manager runs (it keeps sysd from
  exiting on idle); calls are serialized on it with increasing request ids.
  If sysd closed it, the next call reconnects once. A check every 10 s
  reopens a lost connection.
- Error responses keep their code; each code has a localized message
  (`ServiceMessages.ForError`). Unknown codes are failures.
- Protocol version 1 reports package names only, so the library shows names
  and no versions or labels.

The codec is tested against `crates/andruna-ipc/tests/golden/v1.txt`, the
bytes the Rust encoder produces (checked on the Rust side by
`crates/andruna-ipc/tests/golden.rs`). The test-only pipe server in
`tests/` speaks the same protocol.

If sysd does not answer and `andruna-sysd.exe` is next to the manager, the
manager starts it (at most once every 10 s) and retries the connection for
up to 3 s. Settings > About shows the sysd version, uptime, running app
processes and process id. The architecture names Binder for the manager in
the long run (02-architecture.md, IPC); this pipe is the bring-up channel.

## Screenshots on a locked machine

Debug builds only: with `ANDRUNA_MANAGER_SNAPSHOT=<file.png>` set, the app
renders its window content into that file after 4 s and exits. Mica and the
caption buttons are drawn by the system and are not in the image. This code
is not compiled into Release builds.

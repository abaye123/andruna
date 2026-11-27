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

Command line: `--page settings` opens Settings, `--app <package>` opens the
page of one installed app; the default is the Library.

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

The manager talks to andruna-sysd over the named pipe `\\.\pipe\andruna-sysd`
(`SysdClient`). The pipe is opened with `CurrentUserOnly`, so only a server
running as the same user is accepted. One request per connection:

- Frame: 4-byte little-endian length, then that many bytes of UTF-8 JSON;
  at most 4 MiB.
- Request: `{"id": 1, "method": "<name>"}`.
- Response: `{"id": 1, "result": {...}}` or
  `{"id": 1, "error": {"code": "...", "message": "..."}}`; `id` echoes the
  request.

| Method | Result |
| --- | --- |
| `ping` | `{}` |
| `status` | `{"version": "0.1.0", "protocol": 1, "uptime_ms": 1234}` |
| `list_packages` | `{"packages": [{"name": "org.example", "label": "Example", "version_name": "1.0", "version_code": 1}]}` |

This is the client's side of the protocol. When the sysd crate defines the
protocol, the constants and shapes in `Sysd/SysdWire.cs` and
`Sysd/SysdModels.cs` follow it. The architecture names Binder for the
manager in the long run (02-architecture.md, IPC); this pipe is the bring-up
channel.

If sysd does not answer and `andruna-sysd.exe` is next to the manager, the
manager starts it once and waits up to 3 s for it.

## Screenshots on a locked machine

Debug builds only: with `ANDRUNA_MANAGER_SNAPSHOT=<file.png>` set, the app
renders its window content into that file after 4 s and exits. Mica and the
caption buttons are drawn by the system and are not in the image. This code
is not compiled into Release builds.

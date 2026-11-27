# ui

Andruna.Manager, the management app, in C# with WinUI 3
([ADR 0005](../docs/decisions/0005-winui3-management-app.md)): unpackaged,
self-contained, Windows App SDK pinned in the project file.

| Path | Contents |
| --- | --- |
| `src/Andruna.Manager.Core/` | No WinUI: the sysd client (`Sysd/`), events and notifications (`Events/`), paths, launcher, Start menu shortcuts and command line (`Platform/`), settings, view models, language choice. Unit tested |
| `src/Andruna.Manager/` | The WinUI app: entry point, window, pages, install dialog, notifications and crash dialog (`Notifications/`), resource lookup, strings |
| `src/Andruna.Manager/Strings/<language>/Resources.resw` | All user-facing text, `en-US` and `he-IL` |
| `tests/Andruna.Manager.Tests/` | xUnit tests; the fake sysd and all demo data live only here |
| `tools/make-icon.ps1` | Regenerates `Assets/Andruna.ico` |

## Build, test, run

```powershell
dotnet build ui/src/Andruna.Manager -r win-x64      # or -r win-arm64
dotnet test  ui/tests/Andruna.Manager.Tests
ui/src/Andruna.Manager/bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64/Andruna.Manager.exe
```

`installer/build.ps1` publishes the Release build for the installer.

## Command line and instances

The manager runs once per user and session. A second start (Start menu,
a package file, a notification click, andruna-launch) passes its command
line to the running instance and exits.

| Arguments | Opens |
| --- | --- |
| (none), `--page library` | The library |
| `--page settings`, `--page notices`, `--page diagnostics` | That page |
| `--app <package>` | The page of one installed app |
| `--diagnostics <package>` | Diagnostics with that app selected (the crash dialog's "Report a problem") |
| `--install <file>`, or a package file alone | The install dialog (the `.apk` file association) |
| `--background` | No window: only the notification and crash bridge |
| `--unregister` | Removes the Windows notification registration and exits (the uninstaller) |

Without a window the manager keeps running while sysd runs, and exits when
sysd is gone and no crash dialog is open
([ADR 0033](../docs/decisions/0033-manager-notification-bridge.md)).
andruna-launch starts it with `--background` after it started an app.

## Pages

- **Library** (M-1): the installed apps with icon, label in the manager's
  language, version and size (ListPackageInfo), sorted by label, with a
  search box. A sysd without ListPackageInfo gives package names only, and
  the page says so. When sysd is not installed, not running or answers
  wrongly, the page says which and shows no apps. "Install APK" and drag
  and drop open the install dialog.
- **Install dialog** (P-10, P-12): sysd reads and verifies the file
  (InspectPackage); the dialog shows icon, label, version (new or update
  from which version), the signing certificate's SHA-256 and the requested
  permissions (sensitive ones first, with plain descriptions), then installs
  (InstallPackage) with progress and offers "Open". Refusals (bad
  signature, other signer, processor, Android version, downgrade) are
  explained. The manager parses nothing itself (04-security.md).
- **App page** (M-2): header with icon, label, package, version; Open
  (andruna-launch), Pin to Start (creates the app's Start menu shortcut and
  says how to pin it; Windows allows only the user to pin), permissions
  (InspectPackage on the installed APKs in `app\<package>\`), app size,
  data size (`data\<package>\` on disk), install date, Diagnostics, and
  Uninstall with "keep the app's data". Clear data, window size and GPU
  preference say "not available yet": sysd has no request for them.
- **Diagnostics** (M-3): service state; the app crashes sysd remembers
  (explained in plain language); the missing functions each app called,
  most-called first (03-components.md, "missing function counters"); the
  last 200 lines of `sysd.log` and `host.log`; "Open logs folder"; "Save
  problem report", a .zip with a summary (versions, crashes, counters) and
  the logs, no app data.
- **Settings** (M-5, M-6): language (Windows, Hebrew, English), theme
  (Windows, light, dark), About with version, architecture, service state and
  third-party notices. A language change asks for a restart, after which the
  whole window, including the frame, is laid out right to left for Hebrew.
- **Third-party notices** (M-6): opened from About; shows
  `THIRD_PARTY_NOTICES.txt` from the app folder in a read-only monospace
  text box that stays left to right in Hebrew. A missing or unreadable file
  is shown as an error, not as an empty text.
- **Notifications** (D-14): Android notifications from sysd become Windows
  app notifications, source "Andruna", the app's label as attribution, at
  most 20 per app and minute; a click opens the app through andruna-launch.
  A cancelled notification is removed.
- **Crash dialog** (P-23): when an app process ends abnormally, a small
  window says which app stopped and why (the runtime's reason, or the exit
  code explained), with "Open again", "Report a problem" (Diagnostics for
  that app) and "Close".

Package and version strings inside Hebrew text are isolated as left to
right (U+2066 ... U+2069) so they read correctly.

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
- Opened with identification-level impersonation; before anything is sent,
  the pipe owner and the server process token must show the same user.
  Otherwise the manager reports that it does not trust the service.
- One connection, kept open while the window is (it keeps sysd from exiting
  on idle); calls are serialized on it with increasing request ids. If sysd
  closed it, the next call reconnects once. A check every 10 s reopens a
  lost connection. Install, inspect and uninstall may take 10 minutes.
- The event stream (WaitEvents, 0x0010) uses a connection of its own: a
  long poll of up to 25 s that sysd answers as soon as an event arrives.
  Such a connection does not keep sysd running.
- Requests an older sysd does not know are answered `UnknownRequest`; the
  pages then say "not available yet" for that feature.
- Error responses keep their code; each code has a localized message
  (`ServiceMessages.ForError`). Unknown codes are failures.

The codec is tested against the golden payloads the Rust encoder writes:
`crates/andruna-ipc/tests/golden/v1.txt` (basics), `v1-pkg.txt` (package
manager) and `v1-events.txt` (events and diagnostics). The test-only pipe
server in `tests/` speaks the same protocol.

If sysd does not answer and `andruna-sysd.exe` is next to the manager, the
manager starts it (at most once every 10 s) and retries the connection for
up to 3 s; in the background it never starts sysd. The architecture names
Binder for the manager in the long run (02-architecture.md, IPC); this pipe
is the bring-up channel.

## Screenshots

Debug builds only, for machines whose screen cannot be captured (a locked
session):

- `ANDRUNA_MANAGER_SNAPSHOT=<file.png>` renders the main window, with an
  open dialog drawn over it, 5 s after start, then exits.
  `ANDRUNA_MANAGER_SNAPSHOT_CRASH=<file.png>` does the same for a crash
  dialog. Mica and the caption buttons are drawn by the system and are not
  in the image.
- `ANDRUNA_MANAGER_INSTANCE`, `ANDRUNA_MANAGER_ROOT` and
  `ANDRUNA_MANAGER_SETTINGS` point the manager at a development sysd
  instance (`andruna-sysd --instance <name> --root <dir>`), data tree and
  settings file, so the user's Andruna is not touched.
- `ANDRUNA_SCREENSHOTS=<folder> dotnet test ui/tests/Andruna.Manager.Tests
  --filter Screenshots` renders every page in Hebrew and English, light and
  dark, against the tests' fake sysd with demo apps (the only place the
  demo data exists). Without the variable that test does nothing.

None of this is compiled into Release builds.

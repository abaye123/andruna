# andruna-launch

The tiny executable behind Start menu shortcuts and notification clicks:

```
andruna-launch.exe [--quiet] [--instance <name>] [--root <dir>] <package>[/<activity>]
```

- `<package>` starts the package's launcher activity, or brings its window
  forward when it already runs (`StartPackage`).
- `<package>/<activity>` starts that activity (`StartActivity`), in
  Android's component name form: `com.example.app/.SettingsActivity` or
  `com.example.app/com.example.app.SettingsActivity`. Packages with several
  launcher activities get one shortcut per activity.

Steps:

1. Connects to the user's andruna-sysd pipe.
2. If no sysd runs, starts `andruna-sysd.exe` from its own directory
   (outside the caller's job where allowed), passing an inheritable event,
   and waits up to 10 s for sysd to signal it or exit. A sysd that exits
   with "already running" lost a race to another launcher; the launcher
   then connects to the winner.
3. Passes its foreground right to sysd and sends the request. If the
   connection closes before the answer (sysd was just exiting on idle), it
   tries once more.
4. For the product instance (no `--instance`, no `--root`), starts
   `Andruna.Manager.exe --background` from its own directory unless the
   manager already runs (mutex `Local\Andruna.Manager.<user SID>`): the
   manager shows the app's notifications and the crash dialog
   ([ADR 0033](../../docs/decisions/0033-manager-notification-bridge.md)).

It is a windows-subsystem program (no console). Errors are shown in a task
dialog in the user's Windows display language; the strings are in
`res/strings.<lang>.txt` (English, Hebrew). `--quiet` writes the English
message to stderr without a dialog. Exit codes: 0 started or focused,
1 bad command line, 2 sysd unavailable, 3 request refused (for example,
package not installed).

## Uninstall

```
andruna-launch.exe [--quiet [--keep-data]] [--instance <name>] [--root <dir>] --uninstall <package>
```

What a package's entry in Windows "Installed apps" runs
([ADR 0027](../../docs/decisions/0027-installed-apps-entries.md)): asks
"Uninstall <label>?" with a "Keep the app's data" check box, then sends
`UninstallPackage`. `--quiet` asks nothing and removes the data unless
`--keep-data` is given. A running app is not removed (the user is asked to
close it first).

## Shortcuts

Start menu shortcuts of installed apps follow one convention, shared by the
package manager (which creates them at install) and the manager's "Pin to
Start" ([ADR 0034](../../docs/decisions/0034-file-association-and-shortcuts.md)):

| Property | Value |
| --- | --- |
| File | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\<label>.lnk`; characters not allowed in file names become spaces |
| Target | `%LOCALAPPDATA%\Programs\Andruna\andruna-launch.exe` |
| Arguments | `<package>`, or `<package>/<activity>` for further launcher activities |
| Working directory | the target's directory |
| AppUserModelID | the package's, as sysd reports it (`PackageInfo.app_user_model_id`) |
| Icon | the package's `.ico`, as sysd reports it |

`res/andruna-launch.rc` embeds the manifest (Common Controls 6 for the task
dialog, per-monitor DPI, UTF-8) and version information; `build.rs`
compiles it with llvm-mingw's windres.

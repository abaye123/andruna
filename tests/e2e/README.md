# tests/e2e

The end-to-end test of the product path on a real PC: the signed installer
with the runtime, sysd, the package manager, Java code from an installed
package, the manager, uninstall. Results are recorded in
[docs/plan/phase4/e2e.md](../../docs/plan/phase4/e2e.md).

| File | Contents |
| --- | --- |
| `run.ps1` | The test (Windows PowerShell 5.1, as the user, no administrator rights) |
| `SysdPipe.psm1` | A minimal client of sysd's pipe protocol (crates/andruna-ipc/README.md) |
| `fixture/` | `e2e.apk`, the test's own package (launcher activity, English and Hebrew label, adaptive icon, `org.andruna.e2e.Main` with a `main` method), its sources and `build.py` (Android SDK) |

## Run

```powershell
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Arch x64
powershell -ExecutionPolicy Bypass -File tests\e2e\run.ps1 -Thumbprint <signing thumbprint>
```

Options: `-Installer <exe>` (default: the newest in `out\installer`),
`-InstallDir <dir>` (default `%LOCALAPPDATA%\AndrunaE2E\Programs\Andruna`),
`-OutDir` (default `out\e2e`), `-ScreenshotDir`, `-ManagerSnapshotExe`
(a Debug build of the manager, for screenshots on a locked session),
`-SkipNetwork` (no Binary Eye download), `-StopRunning`.

## What it touches

The installer and sysd in product mode write into the user's profile: the
"Installed apps" entry of Andruna (one fixed key), the Start menu, the
package file types, the notification registration and
`%LOCALAPPDATA%\Andruna` (the product has no separate data root). The test
installs into its own folder, never over the user's install, and:

1. snapshots and backs up all of these first (`out\e2e\backup`);
2. refuses to start while a product sysd or manager of this user runs
   (`-StopRunning` ends them);
3. in a `finally` block, uninstalls, removes what points into the test
   folder, restores the backup and compares the profile with the snapshot
   (the last steps of the results).

Installed packages get AppContainer profiles; the test uninstalls the
packages through sysd, which deletes them, and deletes the profile of the
one it leaves for the Andruna uninstaller (a silent uninstall keeps app
data) before it restores the data root. The snapshot includes the
profiles.

## Steps and checks

Install (silent, timed); the runtime files; the boot image written at
install and dex2oat's time from sysd's log; the `andrunaRuntime` ACE on the
install folder and its inheritance by the boot image; Authenticode
signatures (signer thumbprint and timestamp of our binaries, every binary
signed); Andruna's own Installed apps entry and shortcut; sysd start and
ping over the pipe; inspect and install of `e2e.apk`, the package
manager's fixture APK and Binary Eye 1.76.1 from F-Droid (pinned SHA-256,
cached in `.cache\e2e`); dex2oat output, Start menu shortcut and
Installed apps entry per package; ListPackageInfo in Hebrew;
`org.andruna.e2e.Main` from the installed `base.apk` with the installed
`andruna-app` (JIT and AOT); StartPackage through `andruna-launch`;
manager screenshots (library and app page, English and Hebrew); uninstall
of each package (files, data, shortcut, entry gone); one package installed
again and left for the Andruna uninstaller; Andruna's silent uninstall and
the cleanup (entry, shortcut, file types, notification registration, the
Android app's shortcut and entry, processes); restore.

`results.json` and `results.md` in the output folder hold every step with
its time and detail.

# 0027 - Installed Android apps in Windows "Installed apps"

**Status:** proposed, 2026-11-27

## Context
Users expect every app on their PC to appear in Windows Settings,
"Installed apps", and to be removable there (P-4 asks this for Andruna
itself). Android apps installed through Andruna are per-user and have no
installer of their own. Two ways to remove one exist: the manager's app page
(M-2) and the Windows list. Windows runs an entry's `UninstallString`
without arguments it controls and expects the command to ask the user
itself.

## Decision
- At install, andruna-pkg writes one entry per package under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Andruna.App.<package>`
  (the key is the package's AppUserModelID): `DisplayName` (label in the
  Windows UI language), `DisplayIcon` (the package's `.ico`),
  `DisplayVersion`, `Publisher` "Android app (Andruna)", `InstallLocation`,
  `InstallDate`, `EstimatedSize`, `NoModify` and `NoRepair`.
- `UninstallString` is `"<install dir>\andruna-launch.exe" --uninstall
  <package>`. The launcher (already the small, always-present entry point)
  asks "Uninstall <label>?" with a "Keep the app's data" check box and sends
  `UninstallPackage` to sysd, starting sysd if needed. The manager's app
  page uses the same request.
- Uninstall removes the entry, the shortcuts, the AppContainer profile and
  the package files; the data too unless kept. A running app is not removed:
  the user is asked to close it.
- Development and test roots (`andruna-sysd --root`) write no registry
  entries.

## Consequences
- Android apps show up next to Windows apps, with their icon, size and
  date, as users expect; one more registry key per app.
- Uninstalling Andruna itself must remove these entries (the installer's
  uninstaller: a follow-up for the installer track).
- If andruna-launch is missing (damaged install), the entry fails with
  Windows' "could not find" message; reinstalling Andruna repairs it.

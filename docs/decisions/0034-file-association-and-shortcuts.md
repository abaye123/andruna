# 0034 - Package file association and app shortcuts

**Status:** proposed, 2026-11-27

## Context
P-3 asks that `.apk`, `.apks`, `.xapk` and `.apkm` open with Andruna. The
install is per user without administrator rights, so the association goes
to `HKCU\Software\Classes`. Windows protects the user's choice of default
program (the `UserChoice` key): a program may offer itself, but must not
take over a type the user gave to another program. Other programs (archive
tools, emulators, Android development tools) also handle these files.

Start menu shortcuts of installed apps (P-16) are written by the package
manager (andruna-pkg), and the manager's "Pin to Start" needs the same
shortcut. Windows lets only the user pin a desktop program to Start; there
is no supported API for a program to pin itself.

## Decision
- The installer registers the ProgID `Andruna.Package` ("Android app
  package", the manager's icon, verb "Install with Andruna" running
  `Andruna.Manager.exe --install "%1"`), lists it under `OpenWithProgids`
  of the four extensions and under `Applications\Andruna.Manager.exe`.
  It becomes an extension's default only when that extension has no default
  in `HKCU\Software\Classes`. The uninstaller removes exactly what it added.
- App shortcuts: `<Start menu>\Programs\<label>.lnk`, target
  `andruna-launch.exe <package>` (or `<package>/<activity>` for further
  launcher activities), the package's AppUserModelID from sysd, its icon.
  The command line is documented in crates/andruna-launch/README.md.
- "Pin to Start" makes sure that shortcut exists and tells the user how to
  pin it from All apps.

## Consequences
- On a PC where another program already owns `.apk`, double-click keeps
  opening that program; Andruna appears in "Open with" and the user can
  choose it as default in Windows Settings.
- The package manager and the manager write the same shortcut file; the
  one written last wins, with the same content.
- Labels that collide (two apps called "Notes") need a rule (for example
  appending the package name); not handled yet.

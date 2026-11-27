# installer

The Andruna installer, built with NSIS
([ADR 0017](../docs/decisions/0017-installer-nsis.md)).

| File | Contents |
| --- | --- |
| `andruna.nsi` | The installer script |
| `lang/<language>.nsh` | Installer strings, `en-US` and `he-IL` (NSIS's own pages come translated with NSIS) |
| `build.ps1` | Publishes, stages, signs, builds and verifies |

## Build

```powershell
$env:ANDRUNA_SIGN_THUMBPRINT = '<certificate thumbprint>'
powershell -ExecutionPolicy Bypass -File installer\build.ps1            # x64 and arm64
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Arch x64  # one architecture
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Unsigned  # local development only
```

Output: `out\installer\Andruna-<version>-setup-<arches>.exe`. The version
is the `Version` in `ui\Directory.Build.props`.

NSIS 3.11 is downloaded once into `.cache\nsis-3.11` and checked against its
SHA-256, or taken from `ANDRUNA_NSIS` (a folder with `makensis.exe`).

## What the installer does

- Installs per user into `%LOCALAPPDATA%\Programs\Andruna`, no administrator
  rights (`RequestExecutionLevel user`).
- Refuses to run before Windows 11, and when it has no build for the PC's
  processor; otherwise installs the matching architecture.
- Ends running Andruna processes of this user that run from the install
  folder (manager, sysd, apps) before it replaces files.
- Creates the Start menu shortcut "Andruna" for `Andruna.Manager.exe`.
- Registers the package file types per user (P-3,
  [ADR 0034](../docs/decisions/0034-file-association-and-shortcuts.md)):
  ProgID `Andruna.Package` ("Install with Andruna" runs
  `Andruna.Manager.exe --install "%1"`) under `OpenWithProgids` of `.apk`,
  `.apks`, `.xapk` and `.apkm`; it becomes the default only for an
  extension that has no default in `HKCU\Software\Classes`.
- Registers the uninstaller in Windows "Installed apps" (HKCU).
- The uninstaller ends Andruna's processes, removes the manager's
  notification registration (`Andruna.Manager.exe --unregister`), the
  program folder, the shortcut, the file types (only values it added) and
  the uninstall registration, then asks whether to remove installed Android
  apps and their data in `%LOCALAPPDATA%\Andruna` (default: keep; `/S`
  keeps them).
- `/S` installs or uninstalls silently.

## Slots for the Rust executables

Each architecture's staging folder gets, next to the manager:

| File | Taken from |
| --- | --- |
| `andruna-sysd.exe` | `target\<triple>\release\` (`cargo xtask build --arch <arch> --release`) |
| `andruna-launch.exe` | the same folder |

`<triple>` is `x86_64-pc-windows-gnullvm` or `aarch64-pc-windows-gnullvm`.
`build.ps1` builds them first (`cargo xtask build --arch <arch> --release`,
then `cargo xtask sign --arch <arch> --release` unless `-Unsigned`), so it
needs Rust and llvm-mingw (`cargo xtask toolchain`). `-SkipRustBuild` uses
the files already in `target\`; `-RustBinDir <dir>` takes them from
`<dir>\x64\` and `<dir>\arm64\`. A missing file fails the build.

The build checks with llvm-objdump that the Rust executables import no DLL
that llvm-mingw ships (libc++, libunwind, libwinpthread and the like): the
installer does not carry them, so the executables use only Windows system
DLLs and the UCRT.

## Signing

Every `.exe` and `.dll` in the staging folder without a signature (our own
binaries) is signed with `signtool sign /sha1 <thumbprint> /fd sha256 /tr
http://timestamp.digicert.com /td sha256`; files already signed by their
vendor keep that signature. The installer and the uninstaller inside it are
signed by makensis (`!finalize`, `!uninstfinalize`). The build then checks
signer thumbprint and timestamp of each file it signed. A self-signed
development certificate makes `Get-AuthenticodeSignature` report
`UnknownError`; that is expected.

`signtool.exe` is the newest one in the Windows 10/11 SDK, or
`ANDRUNA_SIGNTOOL`.

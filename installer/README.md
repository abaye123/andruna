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

Output: `target\installer\Andruna-<version>-setup-<arches>.exe`. The version
is the `Version` in `ui\Directory.Build.props`.

NSIS 3.11 is downloaded once into `.cache\nsis-3.11` and checked against its
SHA-256, or taken from `ANDRUNA_NSIS` (a folder with `makensis.exe`).

## What the installer does

- Installs per user into `%LOCALAPPDATA%\Programs\Andruna`, no administrator
  rights (`RequestExecutionLevel user`).
- Refuses to run before Windows 11, and when it has no build for the PC's
  processor; otherwise installs the matching architecture.
- Creates the Start menu shortcut "Andruna" for `Andruna.Manager.exe`.
- Registers the uninstaller in Windows "Installed apps" (HKCU).
- The uninstaller removes the program folder, the shortcut and the
  registration, then asks whether to remove installed Android apps and their
  data in `%LOCALAPPDATA%\Andruna` (default: keep; `/S` keeps them).
- `/S` installs or uninstalls silently.

## Slots for the Rust executables

Each architecture's staging folder gets, next to the manager:

| File | Taken from |
| --- | --- |
| `andruna-sysd.exe` | `target\<triple>\release\` (`cargo xtask build --arch <arch> --release`) |
| `andruna-launch.exe` | the same folder |

`<triple>` is `x86_64-pc-windows-gnullvm` or `aarch64-pc-windows-gnullvm`.
`-RustBinDir <dir>` overrides the folder (`<dir>\x64\`, `<dir>\arm64\`).
A missing file leaves the slot empty and the build says so; the manager then
reports that the service is not installed. If the Rust executables need
runtime DLLs, add them to `$RustBinaries` in `build.ps1`.

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

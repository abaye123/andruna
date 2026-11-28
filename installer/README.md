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
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -NoRuntime # manager, sysd, launcher only
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
  folder (manager, sysd, launcher, andruna-app) before it replaces files.
- Prepares the runtime (see "The runtime" below).
- Creates the Start menu shortcut "Andruna" for `Andruna.Manager.exe`.
- Registers the package file types per user (P-3,
  [ADR 0034](../docs/decisions/0034-file-association-and-shortcuts.md)):
  ProgID `Andruna.Package` ("Install with Andruna" runs
  `Andruna.Manager.exe --install "%1"`) under `OpenWithProgids` of `.apk`,
  `.apks`, `.xapk` and `.apkm`; it becomes the default only for an
  extension that has no default in `HKCU\Software\Classes`.
- Registers the uninstaller in Windows "Installed apps" (HKCU).
- The uninstaller ends Andruna's processes, removes the manager's
  notification registration (`Andruna.Manager.exe --unregister`, and the
  `AppUserModelId\<manager path>` key Windows keeps after it), the Start
  menu shortcuts and "Installed apps" entries of Android apps that start
  this folder's `andruna-launch.exe` (ADR 0027), the program folder, the
  shortcut, the file types (only values it added) and the uninstall
  registration, then asks whether to remove installed Android
  apps and their data in `%LOCALAPPDATA%\Andruna` (default: keep; `/S`
  keeps them).
- `/S` installs or uninstalls silently.

## The runtime

Unless `-NoRuntime` is given, each architecture's staging folder also gets
the runtime ([ADR 0043](../docs/decisions/0043-runtime-in-the-installer-and-boot-image-at-install.md)):

| Installed as | Taken from |
| --- | --- |
| `andruna-app.exe` | `target\<triple>\release\`, like sysd and the launcher |
| `art\` (`lib64`, `bin`, `javalib`, `icu`, and `tzdata` when built) | the guest ART root: `-ArtRootX64` / `-ArtRootArm64`, else `ANDRUNA_ART_ROOT_X64` / `ANDRUNA_ART_ROOT_ARM64`, else `out\guest\<arch>\art-root` (`python aosp/tools/build_guest.py --arch <arch> sysroot builtins bionic libcxx aosp art`, after `cargo xtask fetch`, `cargo xtask patch` and `python spikes/s1/java/build_boot.py`; arm64 is cross-built on x64) |
| `angle\libEGL.dll`, `angle\libGLESv2.dll` | ANGLE ([ADR 0029](../docs/decisions/0029-angle-build-cmake-llvm-mingw.md)): `-AngleDirX64` / `-AngleDirArm64`, else `ANDRUNA_ANGLE_DIR_X64` / `ANDRUNA_ANGLE_DIR_ARM64`, else `out\angle\<arch>\bin` (`cargo xtask angle-build --arch <arch>`) |

The build checks that each ART root has what andruna-app needs and that
`libart.so` and the DLLs are for the architecture, strips the DWARF
sections of the ELF files (`llvm-strip --strip-debug`; `.symtab` stays for
crash reports), leaves out any boot image (`javalib\<isa>\`), and signs
the ANGLE DLLs with the rest. A missing or incomplete input fails the
build with the command that makes it; `-BuildRuntime` runs those commands
for a missing one. The build prints the staged sizes and the installer
size.

At install, after copying the files, the installer runs
`andruna-sysd --prepare-runtime`: it grants the `andrunaRuntime`
capability read and execute on the install folder (ADR 0036) and writes
the boot image with `andruna-app --compile-boot` into
`art\javalib\<isa>\` (x64: 9 files, 36.4 MiB, 1.5 to 3.3 s). The
installer first removes `art\`, so an update never keeps the previous
runtime's boot image. If the step fails the install still succeeds; sysd
grants again and writes a missing boot image in the background at every
start.

## Slots for the Rust executables

Each architecture's staging folder gets, next to the manager:

| File | Taken from |
| --- | --- |
| `andruna-sysd.exe` | `target\<triple>\release\` (`cargo xtask build --arch <arch> --release`) |
| `andruna-launch.exe` | the same folder |

| `andruna-app.exe` | the same folder (with the runtime) |

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

## Tests

`tests/e2e/run.ps1` installs the built installer into a separate folder,
installs real packages through sysd, runs Java code from one, takes
screenshots of the manager, uninstalls everything and restores the user's
profile (`tests/e2e/README.md`). CI builds the installer with the runtime
for both architectures (`.github/workflows/installer.yml`, unsigned).

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

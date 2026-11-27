# 0017 - Installer built with NSIS

**Status:** accepted, 2026-11-27

## Context
P-1, P-2 and P-4 ask for one signed per-user installer, no administrator
rights, that picks the x64 or ARM64 build and registers a clean uninstaller.
The installer is our output, but every installer tool embeds its own runtime
(a setup stub, an uninstaller, sometimes custom actions) into that output, so
the tool's license applies to what we ship, not only to the build
([ADR 0006](0006-open-source-permissive-dependencies.md)).

| Tool | License of what ends up in the installer | Fit |
| --- | --- | --- |
| WiX Toolset | Microsoft Reciprocal License (MS-RL), a file-level copyleft, for the toolset and its custom actions; recent releases also tie binary use to a maintenance-fee agreement | Not permissive. MSI per-user installs also fight the MSI model (ALLUSERS, per-machine assumptions) |
| Inno Setup | Its own license: permissive in spirit, but not one of the licenses ADR 0006 lists, with conditions on keeping notices in redistributed binaries | Would need a policy exception |
| NSIS | zlib/libpng for the stub, plug-ins and headers; the bzip2 module under the bzip2 license; the LZMA module under CPL-1.0 | Permissive when the LZMA module is not used |

## Decision
The installer is an NSIS 3 script (`installer/andruna.nsi`), built by
`installer/build.ps1` with a pinned NSIS release downloaded and checked by
SHA-256. Only the zlib compressor is used (`SetCompressor /SOLID zlib`), so the
installer contains only zlib/libpng-licensed NSIS code. No third-party
plug-ins. Installer strings are in `installer/lang/<language>.nsh`.

One installer carries both architectures and installs the one that matches
the PC. It installs into `%LOCALAPPDATA%\Programs\Andruna` with
`RequestExecutionLevel user`, creates a Start menu shortcut, registers the
uninstaller under `HKCU`, and signs the installer and the uninstaller in the
build (`!finalize`, `!uninstfinalize`).

## Consequences
- NSIS is recorded in `THIRD_PARTY_NOTICES` as a component of the installer.
- The LZMA compressor stays off unless CPL-1.0 is accepted, so the installer
  is larger (about 150 MB for both architectures with zlib). bzip2 or
  per-architecture installers are the options if size matters.
- The NSIS stub is 32-bit x86; it runs under emulation on ARM64 PCs. The
  installed files are native.
- Installing updates in the background (P-5) needs its own design; this ADR
  covers the installer only.

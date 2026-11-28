# 0043 - The runtime in the installer; boot image written at install

**Status:** proposed, 2026-11-28

## Context
The installer (ADR 0017) carried the manager, sysd and the launcher. Apps
need the runtime: `andruna-app.exe`, the guest ART root (bionic, libc++,
ART, the JNI libraries ART opens, `dalvikvm`, `dex2oat`, the boot class
path jars, ICU's data) and ANGLE's DLLs (ADR 0029). The package manager
compiles apps at install with `andruna-app --compile`, which needs a boot
image; app processes run in AppContainers that can read the runtime only
through the `andrunaRuntime` capability (ADR 0036).

The ART root as built is 320 MiB, mostly DWARF debug sections (libart.so
alone is 201 MiB). The x64 boot image is nine files, 36.4 MiB; dex2oat
writes it in 1.5 to 3.3 s on the development PC (x64). It depends on the
exact runtime build and on the processor's instruction set, and the arm64
one cannot be written on the x64 build machine with today's
`andruna-app --compile-boot`.

## Decision
- **Layout** in the install folder: `andruna-app.exe` next to sysd,
  `art\` (`lib64`, `bin`, `javalib`, `icu`, and `tzdata` once built), and
  `angle\libEGL.dll`, `angle\libGLESv2.dll`. andruna-app finds `art\` next
  to itself; sysd finds both.
- **Size:** ELF files are shipped with `llvm-strip --strip-debug`: `.symtab`
  stays (andruna-linker uses it for crash reports), DWARF goes. 320 MiB
  becomes 65.5 MiB on x64.
- **Boot image at install, on the user's PC.** The installer runs
  `andruna-sysd --prepare-runtime` after copying the files. It grants the
  `andrunaRuntime` capability read and execute on the install folder
  (inheritable), then writes the boot image with `andruna-app
  --compile-boot` into `art\javalib\<isa>\`, so it inherits the grant. The
  installer removes `art\` before copying, so an update never keeps a boot
  image of the previous runtime. A failure does not fail the install: sysd
  checks at every start, grants again, and writes a missing boot image in
  the background; until then installs report the compile step as
  unavailable (apps run with the JIT).
- The build (`installer/build.ps1`) takes the ART root and ANGLE per
  architecture from the build outputs or given folders, checks that they
  are complete and for the right architecture, stages no boot image, and
  fails honestly when one is missing; `-NoRuntime` builds an installer
  without the runtime.

## Consequences
- The installer stays small enough (x64 alone: 96 MiB, most of it the
  self-contained manager) and has one boot image per PC, matching its
  processor, with no cross-compilation at build time.
- Installing takes a few seconds longer (the boot image), and the install
  folder grows by the image (36 MiB on x64).
- Debug symbols are not on users' PCs; crash reports carry symbol names
  from `.symtab` but no line numbers. A symbol archive per release is a
  follow-up.
- The installer and sysd share one implementation of the grant and the
  boot image step (`crates/andruna-sysd/src/runtime.rs`).

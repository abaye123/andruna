# 0029 - ANGLE: AOSP's copy, D3D11 only, built with CMake and llvm-mingw

**Status:** proposed, 2026-11-27

## Context
Guest OpenGL ES and EGL (HWUI, Skia, app GL code) run on ANGLE over
Direct3D 11 (02-architecture). ANGLE's own build is gn with Chromium's
`build/` tree and depot_tools, and on Windows it expects clang-cl with the
MSVC libraries and a Windows SDK. Andruna builds all Windows code with
llvm-mingw (ADR 0008) and fetches AOSP sources at one pinned tag
(ADR 0011). AOSP carries ANGLE as `platform/external/angle` at that tag,
without Chromium's `build/` directory.

## Decision
- ANGLE comes from AOSP: `external/angle` at `android-17.0.0_r1`
  (commit `1f4500d3906c`, ANGLE hash `8d8796935e4f`) in `aosp/aosp.lock`.
  Chromium zlib's `compression_utils_portable`, which ANGLE uses for
  program binaries, comes from AOSP's `external/zlib`, already locked.
- Only the D3D11 back end is built (win32, not UWP): no Vulkan, desktop
  GL, D3D9, Metal, null back end, frame capture, OpenCL or abseil (ANGLE
  falls back to the standard containers). The translator has the HLSL
  output only.
- `aosp/angle/CMakeLists.txt` builds `libGLESv2.dll` (libANGLE, the
  translator, the D3D11 renderer, EGL and GLES 1 to 3 entry points) and
  `libEGL.dll` (the EGL loader that forwards to `libGLESv2.dll` in its own
  directory) with llvm-mingw clang for x64 and arm64. The source lists are
  read at configure time from ANGLE's `.gni` files by
  `aosp/angle/gni_sources.py`; the handful of conditional additions and
  the defines follow `BUILD.gn` for `is_win` with `angle_enable_d3d11`
  (`ANGLE_IS_WIN`, `ANGLE_ENABLE_D3D11`, `ANGLE_ENABLE_HLSL`,
  `ANGLE_ENABLE_SHARE_CONTEXT_LOCK=1`, `ANGLE_ENABLE_CONTEXT_MUTEX=1`,
  `ANGLE_CAPTURE_ENABLED=0`, `LIBANGLE_IMPLEMENTATION`,
  `LIBGLESV2_IMPLEMENTATION` / `LIBEGL_IMPLEMENTATION`). Exports come from
  ANGLE's `.def` files. libc++, libunwind and the mingw-w64 runtime are
  linked statically, so the DLLs import only system DLLs (UCRT, kernel32,
  user32, dxgi, synchronization).
- `cargo xtask angle-build --arch x64|arm64` configures and builds it into
  `out/angle/<arch>` (4 jobs unless `CMAKE_BUILD_PARALLEL_LEVEL` says
  otherwise). The DLLs ship and are signed with `cargo xtask sign`.
- No source patch is needed at this tag. Build fixes, if a later tag needs
  them, go to `patches/external/angle/`.
- At run time ANGLE loads `d3d11.dll`, `dcomp.dll` and the HLSL compiler
  `d3dcompiler_47.dll` from System32, where Windows 10 and 11 ship them;
  the compiler DLL is not redistributed.

## Consequences
- One toolchain for all Windows code, no depot_tools or MSVC on build
  machines or CI. A build compiles about 380 files per architecture;
  arm64 is cross-compiled on x64 hosts. libGLESv2.dll is about 8 MB and
  libEGL.dll about 1.5 MB.
- The glue must follow ANGLE's `.gni` files and `BUILD.gn` when the tag
  moves: new source lists or conditional files are added by name, and new
  required defines are found by a failing build or smoke test, not by gn.
- ANGLE's own tests and gn-only features (trace, capture, Vulkan back end
  for a later D3D12 or Vulkan path) are not available until the glue
  grows them.
- ANGLE reports OpenGL ES 3.0 on D3D11 by default (ES 3.1 is exposed only
  as non-conformant); a later decision may enable it for apps that ask.

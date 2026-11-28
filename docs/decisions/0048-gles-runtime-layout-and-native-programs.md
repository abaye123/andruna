# 0048 - GLES in the runtime layout, native programs in app processes

**Status:** proposed, 2026-11-28 (phase 3, GL in app processes; the
maintainer delegated the decision and reviews it)

## Context
andruna-gles gives guest code EGL and OpenGL ES from ANGLE (ADRs 0029,
0030): two host DLLs (`libEGL.dll`, `libGLESv2.dll`) and five guest stub
libraries (`libEGL.so`, `libGLESv1_CM.so`, `libGLESv2.so`, `libGLESv3.so`,
`libandruna_window.so`). The app host process, andruna-app, has to find
both kinds in the installed runtime and in a development tree. GL apps
without the framework (native programs, Java programs that call GL from
JNI) are the first things that can render in an app process, before
`GLSurfaceView` and HWUI exist.

## Decision
- **andruna-app links andruna-gles** and calls `andruna_gles::register()`
  before the linker starts, so every guest program in an app process can
  load the GLES stubs.
- **Layout next to `andruna-app.exe`:** `angle\` holds ANGLE's DLLs
  (andruna-gles already looks there), `gles\` the guest stub libraries,
  which go on the guest library search path after the ART root's `lib64`.
  `ANDRUNA_ANGLE_DIR` and `ANDRUNA_GLES_GUEST` override them (tests,
  development trees). The guest stubs stay out of the ART root: they are
  built by andruna-gles, not by the AOSP guest build, and they follow
  ANGLE's version.
- **`andruna-app --exec <elf> [args]`** runs a native guest program in the
  app process's tree (the program's directory is `/data/app/cp0`; an ART
  root is optional). With `--compositor`, the guest's
  `andruna_window_create` returns the surface the compositor hands over,
  at the compositor's size, as a `NativeActivity` gets its window.

## Consequences
- The installer carries `angle\` and `gles\` next to `andruna-app.exe`
  (documented in `installer/README.md`; staged with the runtime).
  The ANGLE DLLs are signed with the other executables; the guest `.so`
  files are ELF and are not Authenticode-signed.
- Java programs reach GL now through JNI (`System.load` of a library that
  links `libEGL.so`); `GLSurfaceView` and `TextureView` wait for the
  framework (`android.view.Surface` natives on `producer::RemoteSurface`).
- `--exec` is also how other native test programs run in the real app
  process environment (AppContainer, sysd channel) once sysd starts app
  processes.

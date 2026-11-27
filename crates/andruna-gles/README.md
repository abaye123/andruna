# andruna-gles

OpenGL ES and EGL for guest code (Android ELF, ADR 0013): ANGLE on
Direct3D 11 (ADR 0029), window surfaces on DirectComposition (ADR 0030).

## Pieces

- `guest/lib*.c` (generated): the guest's `libEGL.so`, `libGLESv1_CM.so`,
  `libGLESv2.so`, `libGLESv3.so` and `libandruna_window.so`. Each function
  jumps through a slot that the library's constructor fills by name with
  the host function `__andruna_gles_resolve`.
- `src/entries.rs` (generated) and `src/thunk.rs`: one slot per ANGLE
  export and, on x64, the System V to Microsoft x64 thunk for it.
- `src/angle.rs`: loads `libEGL.dll` and `libGLESv2.dll`
  (`ANDRUNA_ANGLE_DIR`, else next to the executable or in its `angle`
  directory).
- `src/egl.rs`: the EGL functions implemented on top of ANGLE (default
  display, window surfaces, Android config attributes, `eglGetProcAddress`,
  `eglGetError`); `src/callbacks.rs`: debug and blob cache callbacks.
- `src/window.rs`: `WindowTarget`, the interface a compositor implements,
  and `WindowSurface` (composition swap chain on the target's
  DirectComposition surface handle, ANGLE pbuffer on its back buffer).
- `src/native_window.rs`: `ANativeWindow` objects with Android's layout.
- `src/harness.rs`: a standalone window (or headless target) for tests and
  demos, reachable from guest code through `libandruna_window.so`.
- `src/bin/andruna-gles-run.rs`: `andruna-elf run` plus the GLES host
  functions.

A binary that runs guest code calls `andruna_gles::register()` before it
creates its `andruna_linker::Linker`.

## Implementing `WindowTarget` in a compositor

1. `DCompositionCreateSurfaceHandle` (duplicate the handle into the app
   process when the compositor is elsewhere).
2. `IDCompositionDevice::CreateSurfaceFromHandle(handle)` as the content of
   the surface's visual.
3. `size()` reports the content size in pixels; the swap chain follows it
   at the next `eglSwapBuffers`.
4. `presented(width, height)`: apply `window::flip_transform(height)` to
   the visual when the height changed (the content is bottom-up) and
   commit.
5. `andruna_gles::native_window::create(target)` gives the guest's
   `ANativeWindow*`.

## Environment

| Variable | Effect |
| --- | --- |
| `ANDRUNA_ANGLE_DIR` | Directory of ANGLE's DLLs |
| `ANDRUNA_GLES_DEVICE=warp` | Direct3D 11 software rasterizer (default: hardware) |
| `ANDRUNA_GLES_LATENCY` | Frames in flight before `eglSwapBuffers` waits (1 to 3, default 2) |
| `ANDRUNA_GLES_HEADLESS` | Harness windows are headless |
| `ANDRUNA_GLES_TRACE` | Report unavailable functions and surface events |

## Building and testing

```
cargo xtask fetch --project external/angle --project external/zlib
cargo xtask angle-build --arch x64
python crates/andruna-gles/tools/build_guest.py --arch x64
cargo test -p andruna-gles
```

`build_guest.py` builds the stubs and `gles_test` freestanding; it builds
`gles_demo` too when the guest bionic sysroot exists
(`aosp/tools/build_guest.py`). The tests skip without ANGLE or the guest
build unless `ANDRUNA_REQUIRE_GLES=1` (CI sets it, `.github/workflows/gles.yml`).

Demo (a window with an animated triangle, frame times on exit):

```
set ANDRUNA_ANGLE_DIR=out\angle\x64\bin
target\release\andruna-gles-run -L out\gles\x64\guest -L out\guest\x64\sysroot\usr\lib ^
    out\gles\x64\guest\gles_demo 600
```

After an ANGLE update, regenerate the tables with
`python crates/andruna-gles/tools/gen_gles.py`.

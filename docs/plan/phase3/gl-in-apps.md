# Phase 3: GL in app processes, presented by the compositor

**Status:** 2026-11-28, integration of the graphics track (branch
`p3-gl-in-apps`). Guest code in an app process (`andruna-app`) renders
with OpenGL ES through ANGLE into a task window of andruna-compositor in
another process. Decisions: [ADR 0047](../../decisions/0047-surface-handshake-on-the-window-channel.md)
(surface handshake on the window channel, flip, vsync) and
[ADR 0048](../../decisions/0048-gles-runtime-layout-and-native-programs.md)
(GLES in the runtime layout, `andruna-app --exec`). Both are proposed.
Builds on [gles-angle.md](gles-angle.md) and [compositor.md](compositor.md).

## What works

```
 host (sysd later; now tests/gl.rs or andruna-compositor-host --app)
   Compositor::create_window            task window, configuration
   InputChannel::pair + export_inheritable
   spawn andruna-app --compositor <handles> --exec <elf> | --classpath ...
   attach_input(window, publisher)      ── configuration ──►
   create_surface + offer_surface(pid)  ── attach {handle} ─►  andruna_window_create
                                                              eglCreateWindowSurface
                                                              composition swap chain
                                        ◄── presented {w, h, bottom_up}  first Present
   visual flipped, splash ends, Event::SurfacePresented
   window resized: Event::Configuration ── configuration ──►
   resize_surface(window, surface, w, h) ── resize ────────►  next eglSwapBuffers:
                                        ◄── presented {w', h'}  ResizeBuffers
   window closed                        ── detach ─────────►  is_open() == 0, exit
```

- **andruna-app** calls `andruna_gles::register()` before the linker
  starts, puts the guest GLES stubs on the library path (`gles\` next to
  the executable, or `ANDRUNA_GLES_GUEST`) and loads ANGLE from `angle\`
  (or `ANDRUNA_ANGLE_DIR`). `--compositor <handles>` opens the window
  channel; `--exec <elf>` runs a native guest program (ART root optional).
- **Compositor side.** The window channel (shared memory rings, ADR 0032)
  carries the surface handshake next to input. `offer_surface` duplicates
  the composition surface handle into the app process; the app's
  `producer::RemoteSurface` implements andruna-gles's `WindowTarget`
  (feature `gles` of andruna-compositor), so `eglCreateWindowSurface`
  binds a swap chain to it. A thread pool wait on the channel wakes the
  compositor thread when the app sends something.
- **Flip per surface.** A producer reports bottom-up content (OpenGL row
  order) with its buffer height; the compositor sets one transform with
  the flip and the position. Top-down producers keep the identity.
- **Configuration and resize.** Each configuration change also goes to the
  app (size in app pixels, density, orientation, refresh period); the
  host sends the new surface size, and the producer resizes its buffers
  at its next `eglSwapBuffers`.
- **Pacing.** Producers present at swap interval 1 and wait on the frame
  latency object; vsync events now come from the frames' target present
  times, so the compositor clock and the app's frames have the same
  period (see the numbers).
- **Java.** `GlMain` (a plain Java main, no framework) loads a JNI library
  with `System.load` whose native method takes the compositor's window
  and renders with EGL and GLES 3, in the same process as ART.
- **Demo.** `andruna-compositor-host --app andruna-app.exe --exec
  gles_demo 100000` shows the animated GLES demo from an app process in a
  task window and forwards resizes.

## Fixed on the way

- **Draws after the first present did not reach the screen** (andruna-gles).
  `Present` of a flip-model swap chain unbinds the back buffer from the
  pipeline; ANGLE kept it bound in its state cache, so draws went nowhere
  while clears (which need no binding) still landed. The checks so far
  only cleared. The window surface now makes the context current without
  and again with the surface after each present; `gles_window` draws a
  shader quad from an interleaved buffer and reads it back on the first
  two frames, and the screenshots check it. Cost: `eglSwapBuffers` 0.19 ms
  headless at interval 0 (0.21 ms before, within noise).
- **Vsync ticks at the wrong rate while apps present** (andruna-compositor).
  With a producer presenting, `DCompositionWaitForCompositorClock`
  returned about 530 times per refresh and the frames' start times were
  not on the vsync grid, so `VsyncSource` sent ticks 4.1 ms apart at a
  120 Hz period and spun a core. Ticks now follow the frames' target
  present times (one per period) and the thread sleeps until shortly
  before the next one: one wakeup per refresh.

## Verification

Machine: Intel Core Ultra 7 258V, Intel Arc 140V, 120 Hz panel at 150%,
Windows 11 build 26300. Target `x86_64-pc-windows-gnullvm`.

| Test | What it shows |
| --- | --- |
| `crates/andruna-app/tests/gl.rs`, native | The test is the compositor host. `andruna-app --compositor ... --exec gles_window` gets the surface, renders red over blue with a yellow shader quad; the screenshot (`PrintWindow` with full content) is upright and shows the quad; resize 640x480 to 400x600: configuration change, `resize`, the producer's next present is 400x600 and the screenshot (now green on top) is 400x600; closing the window ends the app with exit code 0 (it checks that it saw the resize and the close) |
| `tests/gl.rs`, Java | The same with `andruna-app --art-root ... --classpath gljava.dex GlMain /data/app/cp0/libgljni.so`: ART runs `main`, `System.load` loads the JNI library (which links the guest `libEGL.so`/`libGLESv2.so`), the native method renders until the window closes, `System.exit(0)` |
| andruna-compositor unit tests (48 + new) | Wire round trips of the surface messages, channel flow of surface messages next to input, the command line form of channel handles, the event watch posting a thread message, the producer connection (attach, resize, presented once per size, input finished as not handled, detach), one vsync tick per refresh |
| andruna-compositor `tests/window.rs` | Unchanged and passing (draw, resize, letterbox, input, vsync) |
| andruna-app unit tests | `--exec` and `--compositor` parsing, the plan of `--exec` (argv in `/data/app/cp0`, GLES directory on the search path) |
| CI | `.github/workflows/gles.yml` runs `tests/gl.rs` (native) on WARP after the ANGLE build, x64 and arm64 runners |
| Demo | `andruna-compositor-host --app andruna-app.exe --exec gles_demo` on hardware: the window shows the demo's frames; with `ANDRUNA_GLES_DEVICE=warp` too |

## Numbers

First frame: from spawning the app process to the compositor applying
the producer's first `presented` (the first `Present` of its swap chain).
Release build, hardware GPU, warm file cache.

| Measurement | Result |
| --- | --- |
| First frame, native program (`--exec gles_window`) | 171 to 188 ms (3 runs); the guest's entry point at 43 ms after process creation (32 ms to `main`, 4 ms to mount the tree, 8 ms to load and relocate); the rest is ANGLE (DLLs, Direct3D 11 device, display, shaders) and the first present |
| First frame, Java (`GlMain` on ART, JIT) | 317 to 374 ms warm, 1019 ms on the first run after a build; `System.load` of the JNI library 5 to 7 ms |
| Frame interval in the app process, swap interval 1, 640x480 then 400x600 | mean 8.33 ms, p99 8.43 ms (252 frames over 2.1 s), `eglSwapBuffers` mean 8.24 ms, 2 frames in flight (debug build; the frame rate is the display's) |
| Compositor clock over the same 2 s | 242 ticks, mean 8.33 ms, reported period 8.33 ms (before the vsync fix: 4.46 to 6.5 ms mean intervals and about 530 wakeups per tick) |
| Resize: `set_client_size` to the producer's first present at the new size | 15 to 25 ms (debug build, unlocked session) |
| Swap overhead, headless, interval 0 | 0.19 ms per `eglSwapBuffers` with the rebind after present |

The frame interval and resize numbers were taken while the session was
unlocked. Later in the same session the machine locked: DWM then
throttles presents of composition swap chains to about 4 frames per
second (intervals of 180 to 240 ms, resize to new pixels 260 to 1000 ms),
while the compositor clock keeps its 8.33 ms period. The tests pass
locked as well (screenshots through `PrintWindow`); frame rates measured
on a locked machine say nothing about the product.

## What the Java path needs next

`GLSurfaceView` and `TextureView` are framework classes; nothing here
replaces them:

| Piece | Needs |
| --- | --- |
| `android.view.Surface` | Natives (`nativeCreateFromSurfaceControl` and friends) that wrap a `producer::RemoteSurface` in an `ANativeWindow` (`andruna_gles::native_window::create`), so `eglCreateWindowSurface(..., surface)` works from Java (`EGL14`, `GLSurfaceView`'s `EglHelper`) |
| `SurfaceView` / `SurfaceHolder` | A child surface per `SurfaceView` (`create_surface` with its z order, `move_surface` for layout), handed over with `offer_surface`; `surfaceCreated`/`surfaceChanged`/`surfaceDestroyed` from `attach`/`resize`/`detach` |
| `ViewRootImpl` relayout | `relayoutWindow` answered from the window's configuration and the surface size the compositor sends; the window's main surface for HWUI |
| `Choreographer` | `DisplayEventReceiver` on the compositor's vsync (period now consistent with presents) |
| `TextureView`, `SurfaceTexture` | A buffer queue with `AHardwareBuffer` on shared D3D11 textures and `EGLImage` import (see gles-angle.md, HWUI needs); not served by a composition swap chain |
| `android.opengl.GLES20`, `EGL14` | The framework's JNI (`libandroid_runtime`) linking the guest `libEGL.so`/`libGLESv2.so`, which work as shown by the JNI program |
| App processes from sysd | sysd starts app processes in AppContainers (p2-app-lifecycle); it then creates the channel and surfaces exactly as `tests/gl.rs` does, with the handles in the launch handle list |

## Not done

- sysd does not create task windows or start GL app processes yet; the
  test and `andruna-compositor-host --app` play its part. GL inside an
  AppContainer was shown by spike S7, not re-checked with this path.
- Input reaches the app process over the same channel but the native and
  Java programs do not consume it (events are finished as not handled).
- arm64 runs in CI only; not run on a local arm64 device.
- The flip is applied by a compositor commit after the producer's present
  at a new height: the first frame after a resize can show once at the
  old flip (not observed in screenshots, not measured).
- Present to photon in an app process is not measured (needs an unlocked
  machine and a camera or PresentMon, as in gles-angle.md).

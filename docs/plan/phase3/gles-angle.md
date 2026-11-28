# Phase 3: OpenGL ES and EGL through ANGLE

Graphics foundation of phase 3: guest code (Android ELF) gets EGL and
OpenGL ES 1 to 3 from ANGLE on Direct3D 11, and window surfaces present
into DirectComposition. Decisions: [ADR 0029](../../decisions/0029-angle-build-cmake-llvm-mingw.md)
(the ANGLE build), [ADR 0030](../../decisions/0030-guest-gles-bindings-and-window-surfaces.md)
(guest bindings and window surfaces). Code: `aosp/angle/`,
`xtask/src/angle.rs`, `crates/andruna-gles/` (its README has the usage).

## What works (2026-11-27)

- ANGLE from AOSP `external/angle` at `android-17.0.0_r1`, D3D11 back end
  only, built by `cargo xtask angle-build` with CMake, Ninja and llvm-mingw
  for x64 and arm64 (arm64 cross-compiled). No ANGLE source patch.
  `libGLESv2.dll` 8.5 MB, `libEGL.dll` 1.7 MB (x64); system DLL imports
  only. ANGLE reports OpenGL ES 3.0 and EGL 1.5.
- Guest `libEGL.so`, `libGLESv1_CM.so`, `libGLESv2.so`, `libGLESv3.so`:
  generated stubs bound by name to host functions; 937 entry points (114
  EGL, 158 GLES 1, 699 GLES 2/3 including extensions); x64 thunks from the
  Khronos prototypes; `eglGetProcAddress` returns guest-callable addresses
  for every function, and 0 for unknown names.
- EGL on Android terms: `EGL_DEFAULT_DISPLAY` and `EGL_PLATFORM_ANDROID_KHR`
  are ANGLE's D3D11 display; `EGL_NATIVE_VISUAL_ID` is the HAL pixel format;
  `EGL_RECORDABLE_ANDROID` / `EGL_FRAMEBUFFER_TARGET_ANDROID` are answered
  (no such configs yet); `eglGetError` covers errors raised by Andruna.
- Current context per guest thread (ANGLE's per-thread state; one Windows
  thread per guest thread), checked with two threads current at once.
- Window surfaces: `ANativeWindow` (Android layout) from a `WindowTarget`;
  composition swap chain on a DirectComposition surface handle; ANGLE
  pbuffer on the back buffer; vsync-paced `eglSwapBuffers` with a frame
  latency waitable; resize through `ResizeBuffers`; swap interval 0 and 1.
- Debug output and blob cache callbacks from ANGLE call guest functions
  with the guest convention, only on guest threads.
- Harness window (Windows window + DirectComposition tree) and a headless
  target, with a guest API in `libandruna_window.so`.

## Verification

| What | How | Result |
| --- | --- | --- |
| Guest program on WARP | `tests/guest.rs` runs `gles_test` (freestanding Android ELF) through andruna-linker: display, config, ES 3 context on a pbuffer, shaders, a draw read back with `glReadPixels`, `eglGetProcAddress`, Android attributes, window surface on a headless target (clear, read back, 4 swaps, resize 48x40 to 32x24) | passes on x64, WARP and hardware (Intel Arc 140V) |
| Per-thread current context | `tests/host.rs` | passes |
| Row order of the texture client buffer (the window surface mechanism) | `tests/host.rs`: memory row 0 holds OpenGL's bottom row | passes: the compositor flips |
| Windowed demo with the guest bionic | `gles_demo` in a harness window | runs, 120 fps on a 120 Hz panel |
| CI | `.github/workflows/gles.yml`: ANGLE build, guest build and the tests with `ANDRUNA_REQUIRE_GLES=1`, x64 and arm64 runners | passes on both (run 46019196947); on the arm64 runner the guest test runs natively; ANGLE builds in 312 s (x64) and 218 s (arm64) on hosted runners |

Not verified: the picture on screen. The measurements below were taken
while the machine was locked: frames were presented and paced by the
compositor, but screen capture shows only the lock screen, so the
orientation on screen is checked through the texture row order test, not
by eye. arm64 has run only on the CI runner, not on a local device.

## Measurements

Reference laptop: Intel Core Ultra 7 258V, Intel Arc 140V, 120 Hz panel,
Windows 11; release build of `andruna-gles-run`; `gles_demo` (clear and
one triangle per frame), 600 frames.

| Case | Frame interval mean / p99 | `eglSwapBuffers` mean | First frame after `eglGetDisplay` |
| --- | --- | --- | --- |
| Hardware, 640x480 window, interval 1 | 8.33 / 8.43 ms (120.0 fps) | 8.23 ms | 150 to 250 ms |
| Hardware, 1920x1080 window, interval 1 | 8.32 / 8.39 ms | 8.25 ms | 181 ms |
| WARP, 640x480 window, interval 1 | 8.36 / 8.49 ms | 8.20 ms | 56 ms |
| WARP, 1920x1080 window, interval 1 | 8.33 / 8.41 ms | 8.20 ms | 21 ms |
| Hardware, 640x480 headless, interval 0 | 0.22 / 0.38 ms | 0.21 ms | - |

- **Pacing.** With interval 1 every frame waits for the compositor:
  intervals are the refresh period with a p99 within 0.1 ms of it.
- **Present latency.** Measured as the wait after `Present` until the
  swap chain accepts the next frame. With one frame in flight
  (`ANDRUNA_GLES_LATENCY=1`), that is the time until the compositor took
  the frame: 7.95 ms mean, 8.11 ms p99, one refresh. The frame is then on
  screen at the next scan-out, so present to photon is about two refresh
  periods (17 ms at 120 Hz) with one frame in flight, one period more with
  the default of two. Not measured end to end: composition swap chains
  report no presentation statistics (`GetFrameStatisticsMedia` returns
  zeros for them) and desktop duplication is denied while the session is
  locked. The demo's `--latency` mode (alternating black and white frames)
  is there for a photodiode or camera measurement.
- **Call cost.** `glGetError` with a current context: 17.5 ns through the
  guest binding (x64 thunk), 18.9 ns calling ANGLE's export directly
  (`tests/host.rs`, noise of the same order). On the shared CI runners:
  x64 28.8 ns direct and 36.6 ns through the thunk, arm64 24.9 ns and
  24.8 ns (direct binding). From guest code: 21 ns for `glGetError` and
  17 ns for `glUniform1f` (`gles_demo --calls`). The thunk costs at most
  a few nanoseconds; ANGLE's entry (context lookup, locks) dominates.
- **Swap overhead.** About 0.2 ms CPU per `eglSwapBuffers` without vsync
  (flush, `Present`, latency wait), interval 0 headless.

## What HWUI and Skia need next

HWUI's `EglManager` and Skia's GL back end on top of this path:

| Need | Status with ANGLE D3D11 | Next step |
| --- | --- | --- |
| `EGL_ANDROID_native_fence_sync` (`eglDupNativeFenceFDANDROID`, fence fds for the buffer queue and the frame completion) | missing | Fence objects on D3D11 fences (`ID3D11Fence` shared handle) exposed as an lx file descriptor; or HWUI's no-fence path |
| `EGL_KHR_fence_sync`, `EGL_KHR_wait_sync` | missing (only `EGL_KHR_reusable_sync`) | Implement in andruna-gles on `glFenceSync` / `glClientWaitSync` (ES 3.0 has them) |
| `EGL_KHR_partial_update`, `EGL_EXT_buffer_age` | missing: HWUI redraws the whole frame | Buffer age from the flip-model swap chain (2 buffers: age 2 after warm-up), damage as a hint or `Present1` dirty rectangles |
| `EGL_KHR_swap_buffers_with_damage` | accepted by andruna-gles (damage ignored) | `IDXGISwapChain1::Present1` dirty rectangles |
| `EGL_ANDROID_presentation_time`, `EGL_ANDROID_get_frame_timestamps` | missing | From the compositor clock (vsync source of andruna-compositor) |
| `EGL_KHR_gl_colorspace` (sRGB, Display P3, scRGB) | ANGLE supports colorspace on window surfaces; andruna-gles passes sRGB to the pbuffer, wide gamut not wired | Swap chain color space (`IDXGISwapChain3::SetColorSpace1`) and FP16 swap chains (`EGL_EXT_pixel_format_float` is present) |
| `EGL_ANDROID_get_native_client_buffer`, `EGL_ANDROID_image_native_buffer` (AHardwareBuffer to EGLImage: HardwareBitmap, TextureView, SurfaceTexture) | missing | AHardwareBuffer on D3D11 shared textures, EGLImage through `EGL_ANGLE_image_d3d11_texture` (present) |
| `EGL_KHR_surfaceless_context`, `EGL_KHR_no_config_context`, `EGL_ANDROID_blob_cache`, `EGL_KHR_image_base`, `GL_OES_EGL_image_external` | present | - |
| `EGL_IMG_context_priority` (RenderThread priority) | missing | Optional |
| OpenGL ES 3.1 (Skia uses it when present) | non-conformant on D3D11, not exposed | ADR if needed |
| Renderer choice | Android 17 defaults HWUI to Skia on Vulkan | `debug.hwui.renderer=skiagl` system property default until a Vulkan path exists |
| Buffer queue in `ANativeWindow` (`dequeueBuffer`/`queueBuffer`, CPU `lock`) | `-ENOSYS` | Needed for `Surface.lockCanvas`, media producers; design with the compositor |
| `Surface` from the framework | not connected | andruna-compositor gives each `Surface` a `WindowTarget`; `android_view_Surface` natives wrap `native_window::create` |

## Integration with andruna-compositor

andruna-compositor (merged on main) hands producers a `SurfaceHandle`
(a composition surface handle duplicated into the app process, and the
size) and documents the same producer model in its `surface` module: a
flip-model swap chain bound to the handle, an ANGLE pbuffer on the back
buffer, `glFlush` and `Present` for `eglSwapBuffers`. andruna-gles's
`WindowTarget` maps onto it directly: `surface_handle()` is
`SurfaceHandle::handle`, `size()` follows the window. Two points to settle
when the two are connected:

- **Orientation.** ANGLE renders into the texture in OpenGL row order
  (checked by `texture_client_buffer_content_is_bottom_up`), so a GL
  producer's content is bottom-up. The compositor's surface visual needs a
  per-surface vertical flip for GL producers (`window::flip_transform`),
  which its `presented` hook or a flag on the surface can carry. Producers
  that write top-down (video, CPU) keep the identity transform.
- **Device.** The compositor's notes create ANGLE's display on the
  producer's device (`EGL_ANGLE_device_creation`); andruna-gles instead
  uses the device of ANGLE's default display, so one device serves every
  EGL display of the process. Either works with the same handle.

Connected on branch `p3-gl-in-apps`: see [gl-in-apps.md](gl-in-apps.md)
(orientation is a per-surface flip reported by the producer; ANGLE's
default device serves the producer). That work also found and fixed draws
after the first present not reaching the screen (the window surface is
bound again after each present).

## Open points

- Present to photon is not measured end to end; measure it
  with a camera or PresentMon on an unlocked machine (`gles_demo --latency`).
- Device loss (driver update, TDR): ANGLE reports
  `EGL_CONTEXT_LOST`; swap chains are not recreated yet.
- `ANGLE_EXPOSE_NON_CONFORMANT_EXTENSIONS_AND_VERSIONS` (ES 3.1) is off.
- The ANGLE DLLs are signed in the packaging step (`cargo xtask sign`);
  the installer does not ship them yet.

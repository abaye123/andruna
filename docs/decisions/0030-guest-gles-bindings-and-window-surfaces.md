# 0030 - Guest OpenGL ES and EGL: stub libraries, host thunks, composition swap chains

**Status:** proposed, 2026-11-27

## Context
Guest code (HWUI, Skia, app GL code; Android ELF, ADR 0013) calls EGL and
OpenGL ES through `libEGL.so`, `libGLESv1_CM.so`, `libGLESv2.so` and
`libGLESv3.so`. The implementation is ANGLE on Direct3D 11, built as
Windows DLLs (ADR 0029), which uses the Microsoft calling convention. On
Android, EGL window surfaces are `ANativeWindow`s backed by a buffer
queue that SurfaceFlinger composites; in Andruna the compositor is
DirectComposition, owned by andruna-compositor in sysd, another process.

## Decision
- **Stub libraries.** The four guest libraries (and `libandruna_window.so`,
  the test harness API) are small generated ELF objects
  (`crates/andruna-gles/guest/`, from `tools/gen_gles.py`): every exported
  function is a two- or three-instruction jump through a slot, filled by
  the library's constructor with the host function `__andruna_gles_resolve`
  by name. They are real files, so `dlopen`, `dlsym` and linking against
  them work as on Android; they need no libc.
- **Host side.** `__andruna_gles_resolve` returns, per name: a host
  implementation (EGL functions about displays, window surfaces, configs,
  errors, `eglGetProcAddress` and the callback-taking functions); on x64 a
  generated System V to Microsoft x64 thunk into ANGLE's export (types from
  the Khronos prototypes, 937 functions); on arm64 ANGLE's export itself,
  since AAPCS64 and Windows ARM64 agree for these non-variadic functions
  and guest code leaves x18 alone. Unavailable functions bind to a stub
  that returns 0. The host symbol reaches the loader through
  `andruna_linker::host::register` (the binary that runs guest code calls
  `andruna_gles::register()`).
- **Per-thread state** is ANGLE's: every guest thread is one Windows
  thread, so the current context, current surfaces and `eglGetError` are
  per guest thread with no extra bookkeeping. Callbacks from ANGLE into
  guest code (debug output, blob caches) run only on guest threads.
- **Window surfaces.** A compositor provides a `WindowTarget`: a
  DirectComposition surface handle (`DCompositionCreateSurfaceHandle`) set
  as the content of its visual, the size, and a hook after each present.
  The GL side creates a flip-model composition swap chain on ANGLE's
  device for that handle (works across processes) and an ANGLE pbuffer on
  its back buffer (`EGL_ANGLE_d3d_texture_client_buffer`); the guest's
  `EGLSurface` is a handle of andruna-gles mapped to that pbuffer.
  `eglSwapBuffers` presents and then waits on the frame latency waitable
  object (default 2 frames in flight), like a buffer queue whose
  `dequeueBuffer` blocks; it follows the target's size with
  `ResizeBuffers`. Content is bottom-up (OpenGL row order in the texture);
  the compositor shows it flipped with a visual transform at no cost.
- **`ANativeWindow`** objects have Android's layout and support `query`,
  `perform` for the operations that need no buffer queue, and reference
  counting. The buffer queue (`dequeueBuffer`, CPU `lock`) returns
  `-ENOSYS` until a later design needs it (media, CPU rendering).

## Consequences
- A GL call from guest code costs the same as a direct call of ANGLE's
  export within measurement noise (about 18 ns for `glGetError` on the
  reference laptop, either way).
- New ANGLE functions are picked up by rerunning the generator at a new
  tag. Functions without a Khronos prototype (7 at this tag) are not
  exposed.
- The compositor interface is a Rust trait plus a surface handle, so
  andruna-compositor can implement it in sysd and pass the handle to the
  app process; until then a standalone harness window implements it.
- No `EGL_ANDROID_native_fence_sync`, `EGL_KHR_fence_sync`,
  `EGL_KHR_partial_update`, buffer age or frame timestamps on this path
  yet; HWUI's needs are listed in docs/plan/phase3/gles-angle.md.
- One swap chain per window surface: SurfaceView and video surfaces get
  their own targets (visuals), as 03-components describes.

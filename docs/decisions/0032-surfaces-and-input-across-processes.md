# 0032 - Surfaces and input between the compositor and app processes

**Status:** proposed, 2026-11-27 (phase 3, andruna-compositor; the
maintainer delegated the decision and reviews it)

## Context
The compositor runs in sysd ([architecture](../spec/02-architecture.md));
apps render in their own processes (HWUI on ANGLE on Direct3D 11) and
receive input there. On a device, `BLASTBufferQueue` moves buffers from
the app to SurfaceFlinger and `InputChannel` (a socket pair) carries
input. Neither exists yet on Windows (Binder is phase 2 work), and the
GLES track needs a stable way to get an EGL native window now.

## Decision
- **Surfaces.** For each Android surface, the compositor creates a
  DirectComposition visual and a composition surface handle
  (`DCompositionCreateSurfaceHandle`) and duplicates the handle into the
  app process. The app binds a flip-model swap chain on its own Direct3D
  11 device to that handle
  (`IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle`) and
  presents; DWM composes it with no copy and no round trip to sysd. The
  Rust interface is `andruna_compositor::surface::NativeSurface`, the
  base of `ANativeWindow` in andruna-gles: ANGLE renders into the swap
  chain's back buffer through `EGL_ANGLE_d3d_texture_client_buffer` on
  the same device.
- **Input.** Until Binder hands out `InputChannel`s, each window's events
  go through a shared-memory channel: two single-producer,
  single-consumer byte rings in one section and one event per direction,
  carrying a versioned little-endian message format (motion, key, IME,
  focus, and the app's "finished" replies). The section and events are
  duplicated into the app process. The app is untrusted: every length
  and position read from the section is checked.
- **Replaceable.** The producers of events (the translation state
  machines) and the consumers (the framework's input receiver) do not
  depend on the transport; replacing it with `InputChannel` touches only
  `channel.rs` and `wire.rs`.

## Consequences
- Frames go from the app's GPU work to the screen as on a device, with
  each surface its own layer, ready for SurfaceView and video visuals.
- A misbehaving app can fill its own ring or corrupt its own section; the
  compositor then drops that window's events and reports it, other windows
  are not affected.
- Handles are duplicated by sysd, which knows each app process; an app
  cannot reach another app's surfaces.
- When Binder lands, the surface handle travels in a Binder transaction
  and the input channel becomes a real `InputChannel` (needs a new
  `InputMessage` writer and an `lx` file descriptor for the Looper).

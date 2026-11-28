# 0047 - Surface handshake on the window channel

**Status:** proposed, 2026-11-28 (phase 3, GL in app processes; the
maintainer delegated the decision and reviews it)

## Context
ADR 0032 lets the compositor (in sysd) duplicate a composition surface
handle into an app process, and gives each window a shared-memory input
channel. ADR 0030 makes an EGL window surface out of a `WindowTarget`: a
surface handle, a size, and a hook after each present. Connecting the two
needs a way for the app process to receive surfaces and size changes, for
the compositor to learn how a producer stores its pixels (OpenGL renders
bottom-up), and a vsync that matches the rate producers present at. Binder
is not there yet.

## Decision
- **One channel per window carries input and surfaces.** The window's
  channel (ADR 0032) gets surface messages next to input events, with
  sequence number 0: `attach` (surface id, handle value in the app process,
  size, z), `resize`, `detach`, `configuration` (a compact window
  configuration with the display's refresh period) from the compositor,
  and `presented` (surface id, buffer size, bottom-up flag) from the app.
- **Bootstrap.** The app process gets its end of the channel as inherited
  handles in its launch handle list, their values on the command line
  (`andruna-app --compositor <section:capacity:to_app:to_compositor>`),
  like the sysd pipe (ADR 0036). Surface handles are duplicated by the
  compositor into the app with the process handle sysd holds, and owned by
  the app after `attach`; an `attach` that cannot be sent is closed in the
  app again.
- **Orientation per surface.** The compositor shows a surface flipped when
  its producer reports bottom-up content, over the buffer height it
  reported. The flip and the surface position are one visual transform
  (offsets stay 0), so the result does not depend on how
  DirectComposition orders offsets and transforms. The app is untrusted:
  only its own window's surfaces are affected; the first `presented` of a
  window ends its splash.
- **Who wakes whom.** A thread pool wait on the channel's event posts a
  thread message to the compositor thread; the app side has a reader
  thread that answers input (not handled, until a consumer is set).
- **Vsync on the display grid.** Compositor clock ticks are taken from the
  frames' target present times, one per refresh period, with one wakeup
  per period: while producers present, DWM creates frames far more often
  than the display refreshes, and their start times are not on the grid.

## Consequences
- A GL app in its own process presents to the screen with no copy and no
  round trip to sysd per frame; the compositor hears from it only when the
  buffer size or orientation changes.
- The flip is updated by a DirectComposition commit after the producer's
  present at a new height, so the first frame after a resize may show at
  the old flip for one composition.
- Vsync events and producers' presents share one period (8.33 ms each at
  120 Hz on the reference laptop); the vsync phase is the display's.
- When Binder lands, `attach`/`resize`/`detach` become the window
  manager's relayout results and `configuration` the activity's
  `ClientTransaction`; the producer side (`producer::RemoteSurface`) keeps
  its interface.

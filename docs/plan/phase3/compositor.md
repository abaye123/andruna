# Phase 3: andruna-compositor

**Status:** 2026-11-27, first round of the graphics/input track (branch
`p3-compositor`). The crate is [crates/andruna-compositor](../../../crates/andruna-compositor/README.md).
Decisions: [ADR 0031](../../decisions/0031-freeform-window-configuration.md)
(what apps see), [ADR 0032](../../decisions/0032-surfaces-and-input-across-processes.md)
(surfaces and input across processes). Both are proposed.

## What the compositor does

| On a device | Here |
| --- | --- |
| WindowManager: one task, one freeform window | One top-level Windows window per task, Windows 11 frame |
| Window configuration (bounds, insets, density, orientation) | `config::compute` from the window's client rectangle, monitor and DPI |
| SurfaceFlinger layers | One DirectComposition visual per surface, inside a per-window tree |
| `BLASTBufferQueue` / `ANativeWindow` | A composition surface handle per surface, a flip-model swap chain in the app (`surface::NativeSurface`) |
| `DisplayEventReceiver` vsync | `vsync::VsyncSource`, from the DWM compositor clock |
| InputReader / InputDispatcher | `input::pointer`, `input::keys` state machines; `channel::InputPublisher` |
| `InputChannel` | `channel::InputChannel` (shared memory rings) until Binder |
| Hardware keyboard layout (`.kcm` overlay) | `input::kcm::generate` from the active Windows layout |
| InputMethodManager with an IME | Windows input methods through the IMM/TSF bridge (see the TSF plan below) |

### Window tree

```
root
 ├─ content   transform: letterbox offset and size compatibility scale;
 │   │        clip: hides app pixels under the opaque caption
 │   └─ surface visuals, z ordered, each a composition surface the app presents to
 ├─ splash    app icon on the backdrop or the splash color; fades out on the first frame
 └─ caption   back button and title (Direct2D, Segoe Fluent Icons and Segoe UI Variable)
```

The window is DWM's frame everywhere (`DwmExtendFrameIntoClientArea` with
-1 margins, `WM_NCCALCSIZE` keeps the caption in the client area), so Mica
shows where nothing is drawn (caption, letterbox) and DWM draws the
caption buttons, with Snap Layouts. With an app color, the backdrop is
off and `DWMWA_CAPTION_COLOR` paints the caption behind the buttons in the
same color our caption visual uses. Hit testing is stateless
(`WM_NCHITTEST`: back button = client, caption = `HTCAPTION`, top band =
resize, caption buttons by `DwmDefWindowProc`).

### Threading

One compositor thread owns all windows, the Direct3D 11, Direct2D and
DirectComposition devices, and the input state machines. Commands arrive
on a channel and wake the thread with a thread message; events leave on a
channel (or, per window, through an attached input channel). Win32 sends
some messages synchronously from inside calls the compositor makes; those
paths are stateless or post a layout pass for later, so the state is
never borrowed twice. Vsync has its own thread, idle (no wakeups) while
nobody subscribes.

### Input

- Mouse, pen and touch arrive as pointer messages (`EnableMouseInPointer`).
  Mouse and pen read their coalesced history
  (`GetPointer*InfoHistory`), which becomes Android historical samples;
  touch reads the whole frame (`GetPointerFrameTouchInfo`) once per frame
  id.
- Times: Windows' performance counter (pointer messages) and tick count
  (other messages) are converted to `CLOCK_MONOTONIC`, which andruna-lx
  implements with `QueryUnbiasedInterruptTimePrecise`.
- Keys: scan codes (with AltGr's fake left Ctrl removed) to Linux and
  Android key codes; meta state from held modifiers and lock keys; repeat
  counts; focus loss cancels held keys (`FLAG_CANCELED`).
- Characters: the app gets them through the key character map, so the
  `WM_CHAR` a key down produces is matched (with `ToUnicodeEx` on the
  current keyboard state) and not sent again; a `WM_CHAR` without a key
  (emoji panel, Alt codes, `VK_PACKET`) is committed text. After a dead
  key, the next character is the key character map's to compose.

### Transport (until Binder)

`channel.rs`: one section, two rings (compositor to app, app to
compositor), positions on separate cache lines, one auto-reset event per
direction. Messages are `wire.rs` frames: a 12-byte header (length, kind,
version, sequence) and a body. A two-pointer move with three samples is
168 bytes; the default 64 KiB ring holds about 400 of them. The
compositor counts the app's finished replies (latency, oldest pending
event) as the input dispatcher does for ANR. Replacing the transport with
`InputChannel` touches `channel.rs` and `wire.rs` only.

### Interface for the GLES track

`surface::NativeSurface` (documented in [surface.rs](../../../crates/andruna-compositor/src/surface.rs)):
open it on ANGLE's own `ID3D11Device` with the `SurfaceHandle` the
compositor hands out, wrap `back_buffer()` with
`eglCreatePbufferFromClientBuffer(EGL_D3D_TEXTURE_ANGLE, ...)`, call
`present()` after `glFlush` for `eglSwapBuffers`, and `resize()` for
`ANativeWindow_setBuffersGeometry`. Formats are Android's
(`RGBA_8888`, `RGBX_8888`, `BGRA_8888`, `RGBA_FP16`, `RGBA_1010102`).

## TSF plan

Now: Windows input methods reach the window through the IMM messages,
which Windows 11 serves from TSF for TSF input methods (the IMM/TSF
bridge). The compositor reads the composition string, caret and target
clause (`WM_IME_COMPOSITION`) and the result string, and delivers them
as `ImeEvent::Composing`, `Commit` and `FinishComposing`, which map to
`InputConnection.setComposingText`, `commitText` and
`finishComposingText`. The default composition window is hidden; the
candidate window is placed at the caret the app reports
(`Compositor::set_ime_caret`). Hebrew is a keyboard layout, not an input
method: it types through key events and the generated key character map.

Next, a TSF text store per window (`ITextStoreACP2` on a document
manager the compositor activates on focus), backed by the focused
editor's state that the app reports through `InputConnection`
(`getTextBeforeCursor`, `getSelectedText`, `getExtractedText`):

1. Read access for input methods that look at surrounding text (Chinese
   and Japanese prediction, reconversion, Windows text suggestions).
2. Composition ranges and display attributes from `ITfDisplayAttributeMgr`
   as Android composing spans (underline styles).
3. Edits from the IME (`SetText`, `InsertTextAtSelection`) as
   `InputConnection` operations, with the app's acknowledgements as the
   text store's lock grants.
4. The touch keyboard (`InputPane`) shown when an editable view gets focus
   on a tablet, with `EditorInfo.inputType` mapped to input scopes.
5. Right-to-left: the text store reports the editor's direction so
   Hebrew and Arabic input methods place their UI correctly.

## Verification

Machine: Intel Core Ultra 7 258V, Intel Arc 140V, Windows 11 build 26300,
a 120 Hz panel at 150% (144 DPI) with dynamic refresh rate. Debug build,
`x86_64-pc-windows-gnullvm`.

| Test | What it shows |
| --- | --- |
| Unit tests (42) | Configuration tables (density for every Windows scale, dp rounding, size classes, edge to edge vs legacy, letterbox and size compatibility scaling, minimum sizes, aspect limits, diffs for resize / move / monitor change / theme), pointer sequences (two-finger touch with id reuse, historical samples, cancel, mouse hover/press/buttons, back button key, pen hover/pressure/tilt/eraser, wheel, pinch), keys (scan code table, positional key codes under Hebrew, repeats, meta table, Alt+Left, focus cancel), key character map generation (Hebrew, AltGr, dead keys), wire round trips and every truncation and bit flip, channel flow, wraparound, full, cross-thread wakeup, corruption, clocks, vsync extrapolation |
| `tests/window.rs` draws_resizes... | A real window; a producer on its own device binds to the surface handle and clears red, the screenshot (`PrintWindow` with full content) shows red; resize to portrait gives a configuration change with screen size, orientation, smallest width and window bits and not density; the producer resizes and draws green, the screenshot is 400x600 and green; the minimum size holds |
| letterbox | A non-resizable portrait app: maximized, the configuration keeps its dp size and only the window bounds change, the content is centered, the side is not the app's color; the caption has the app's color |
| input | Posted keyboard messages through the window's message loop: `KEYCODE_A` down/up without duplicate text, a surrogate-pair emoji as committed text, `Alt+Left` as `KEYCODE_BACK` with `FLAG_FROM_SYSTEM`. `SendInput` and synthetic touch run when the test process owns the foreground; otherwise they report "skipped" |
| vsync | 60 ticks of the compositor clock on the uptime clock, increasing ids |

Screenshots of the test host (`andruna-compositor-host --caption-color
1565C0` and without a color) show the caption with the back button, the
title, DWM's caption buttons on the app's color or on Mica, and the app's
surface below.

## Numbers

| Measurement | Result |
| --- | --- |
| Posted key message to compositor event (message loop, translation, delivery) | 0.36 ms |
| `SetWindowPos` resize to configuration event | 1.4 ms |
| Producer `ResizeBuffers` + clear + `Present` to new pixels in a screenshot | 13.5 ms (screenshots polled every 20 ms; bounded by the next composition) |
| Compositor clock | reported period 8.333 ms (120 Hz); measured mean 9.9 to 10.7 ms over 60 ticks while the desktop is idle (dynamic refresh rate lowers the panel rate; the reported period is the last composed frame's) |
| Input message size | key 60 bytes; motion with 2 pointers and 3 samples 168 bytes |

Not measured yet: input latency from a hardware event to the app's pixel
(needs an app process and the reference machine's hardware-assisted
test); `SendInput` and touch injection latency (the agent's test process
cannot take the foreground on this machine; the test prints them where it
can).

## Not done in this round

- **sysd wiring.** The compositor is a library with a test host. sysd's
  activity manager will own one `Compositor` (started on the first task),
  create a window per task with the package's icon as splash, hand the
  surface handle and input channel to the app process (duplicated into it,
  [ADR 0032](../../decisions/0032-surfaces-and-input-across-processes.md))
  and forward `Configuration` events to the activity's
  `ClientTransaction`. Not wired now because sysd has no activity manager
  and app processes have no framework yet; sysd is unchanged.
- **Touchpad pinch source.** The translation to Android's pinch events is
  done and tested; precision touchpads report pinches to Win32 windows
  only through DirectManipulation (or as Ctrl+wheel), which is not wired
  yet, so pinches arrive as Ctrl+wheel scrolls.
- **TSF text store** (plan above).
- **Per-display vsync pacing.** Events carry each display's period and the
  compositor reports each window's display; Choreographer pacing per
  window and dynamic refresh rate awareness come with the framework.
- **Esc as Back** relies on the fallback key Android's generic key
  character map defines for `ESCAPE`; to be checked end to end when the
  framework runs in app processes (if it does not hold, the compositor
  sends Back for an unhandled Escape itself).
- **AppUserModelID per package (D-5)**, taskbar badges, the system menu's
  Android entries, Windows text scale to `fontScale` (D-9, reported as
  1.0), high contrast.
- **Hardware input on ARM64**: CI (hosted x64 and ARM64 runners) builds
  the crate and passes every test, the window tests included; foreground
  input injection and the compositor clock parts report "skipped" where
  the runner does not offer them (the CI log does not show which ran).

# andruna-compositor

What SurfaceFlinger and WindowManager do on a device, on Windows: one
Windows window per Android task, one DirectComposition visual per surface,
vsync from the compositor clock, and input and IME translated to Android
events. See [03-components.md](../../docs/spec/03-components.md#andruna-compositor),
the plan and numbers in [docs/plan/phase3/compositor.md](../../docs/plan/phase3/compositor.md),
and [ADR 0031](../../docs/decisions/0031-freeform-window-configuration.md)
and [ADR 0032](../../docs/decisions/0032-surfaces-and-input-across-processes.md).

| Module | |
| --- | --- |
| `compositor` | The compositor thread: task windows (Windows 11 frame, Mica or the app's caption color, back button, Snap Layouts, per-monitor DPI, minimum sizes, letterbox, splash), surfaces, input routing; commands in, `Event`s out |
| `config` | Pure: `Configuration`, window bounds, caption inset, letterbox and minimum size from a window's measurements |
| `input::pointer`, `input::keys`, `input::kcm` | Pure: Windows pointer frames and key messages to `MotionEvent`/`KeyEvent`; key character map overlays from a keyboard layout |
| `surface` | **The interface for andruna-gles**: `NativeSurface`, the producer side of a surface (an `ANativeWindow`'s swap chain) |
| `producer` | The app process's end of a window channel: surfaces handed over by the compositor (`RemoteSurface`, an `andruna_gles` `WindowTarget` with the `gles` feature), configuration, input ([ADR 0047](../../docs/decisions/0047-surface-handshake-on-the-window-channel.md)) |
| `vsync` | `VsyncSource`: compositor clock ticks as vsync events, one per refresh period on the frames' target present times |
| `wire`, `channel` | The input message format and the shared-memory input channel (until `InputChannel` over Binder) |
| `clock` | Timestamps on `CLOCK_MONOTONIC`, as andruna-lx defines it |
| `sys` | All `unsafe` Windows API wrappers |

## Test host

```
cargo run -p andruna-compositor --bin andruna-compositor-host -- [--size 700x500]
    [--caption-color 1565C0] [--fixed] [--splash] [--vsync] [--seconds N]
    [--screenshot out.bmp]
```

Opens a task window whose surface a producer in the same process fills
with a color (a press changes it) and prints every event as logfmt:
configurations, input with its latency, IME text, keyboard layout, vsync.
`--fixed` is a non-resizable portrait app (maximize it to see the
letterbox). `--app <exe> [args...]` makes an app process the producer:
the host starts `<exe> --compositor <handles> [args...]`, hands it the
surface and forwards resizes (`--app target\release\andruna-app.exe
--exec out\gles\x64\guest\gles_demo 100000` shows GL from an app
process). A screenshot path ending in `.screen.bmp` captures the screen
instead of the window.

## Tests

`cargo test -p andruna-compositor` runs the unit tests and
`tests/window.rs`, which opens real windows (draw, resize, letterbox,
input, vsync). Tests that need the foreground (`SendInput`, synthetic
touch) report "skipped" when the process does not have it.

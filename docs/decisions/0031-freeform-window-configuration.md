# 0031 - Freeform windows: configuration, caption and input seen by apps

**Status:** proposed, 2026-11-27 (phase 3, andruna-compositor; the
maintainer delegated the decision and reviews it)

## Context
Each Android task is one Windows window (D-1 to D-4, D-7, D-10 to D-12).
What the app sees of that window - its `Configuration`, its insets, the
input devices and the keys - is application-visible behavior, so the
mapping is a maintainer decision (compatibility policy, "Desktop
adaptations"). Android's own freeform (desktop) windowing gives the
model: a window with a caption bar, real configuration changes on
resize, letterboxing for apps that cannot resize.

## Decision
- **Density.** `densityDpi = monitor DPI * 160 / 96`, rounded, no density
  buckets: one dp is one Windows effective pixel (100% = 160, 150% = 240,
  200% = 320). Moving to a monitor with another scale is a `density`
  configuration change (D-7).
- **Bounds and caption.** The Android window is the whole client area of
  the Windows window. The title bar (32 effective pixels) is a
  `captionBar` inset at the top. Apps targeting SDK 35 or later get screen
  sizes from the full bounds (edge to edge, as Android 15 and later);
  older apps get them without the caption, as Android computes sizes
  without decor insets for them. The caption is opaque: app pixels under
  it are hidden, like under Android's caption bar.
- **Caption.** Mica backdrop, Windows' own caption buttons (so Snap
  Layouts work), a back button and the title. When the app sets a status
  bar or task description color, the caption takes that color, with
  light or dark text and buttons by contrast (D-4).
- **Orientation** comes from the aspect ratio (portrait when
  width <= height). `smallestScreenWidthDp` and `screenLayout` follow
  Android's documented size classes. `windowingMode` is freeform.
- **Minimum size**: the activity's `<layout android:minWidth/minHeight>`,
  at least Android's freeform minimum of 220 dp.
- **Not resizable** (`resizeableActivity="false"`): a fixed content size,
  chosen at launch: 75% of the monitor's work area on the long side of the
  requested orientation (portrait unless landscape is requested), at 16:9
  within the app's `minAspectRatio`/`maxAspectRatio`. The window has no
  resize border; maximized, the content is centered below the caption on
  the window's backdrop, never stretched; on a monitor where it does not
  fit, it is scaled down uniformly and the configuration does not change
  (Android's size compatibility mode) (D-2).
- **Device configuration**: `touchscreen` is `finger` only when the PC has
  a touch screen; `keyboard` is `qwerty`; `uiMode` night follows the
  Windows app theme (D-8); `navigation` is `nonav`.
- **Input devices**: keyboard, mouse, touch screen, stylus and touchpad are
  separate Android input devices with the sources Android uses for them.
  Mouse back/forward buttons also send `KEYCODE_BACK`/`KEYCODE_FORWARD`,
  touchpad pinches are two-finger events with `CLASSIFICATION_PINCH`, the
  wheel is `ACTION_SCROLL` in detents (fractional for smooth wheels).
- **Keys** are positional: key codes come from scan codes through the
  generic Linux key layout, whatever the Windows layout; characters come
  from an Android key character map overlay generated from the active
  Windows layout (Hebrew types Hebrew, shortcuts stay `Ctrl+C`).
- **Back**: the title bar back button, `Alt+Left` (system shortcut) and the
  mouse back button send `KEYCODE_BACK`; `Esc` is delivered as
  `KEYCODE_ESCAPE`, which Android's key character map falls back to Back
  when the app does not handle it (D-12).
- **Text input**: Windows input methods (TSF) deliver composition and
  committed text as `InputConnection` composing/commit operations;
  characters without a key (emoji panel, Alt codes) are committed text.

## Consequences
- Apps see a real desktop window: real sizes, real density, real input
  devices; nothing pretends to be a phone.
- Apps that hard-code a portrait phone layout still work through the
  letterbox; they never get stretched pixels.
- The caption hides the app's top 32 dp; edge-to-edge apps already pad for
  the inset. A transparent caption (Android's
  `APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND`) is supported by the
  visual tree and can be enabled when the framework passes the flag.
- Changing any of these values changes what applications see and needs a
  new ADR.

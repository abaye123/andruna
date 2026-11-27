//! The configuration an Android window gets from its Windows window.
//!
//! Pure functions: the Windows side measures the window (client rectangle,
//! monitor, DPI, caption height) and this module computes what the
//! application sees - `Configuration` with its `WindowConfiguration`, the
//! caption bar inset, where the app's surface goes in the window, and the
//! window's minimum size. The rules are recorded in
//! [ADR 0031](../../../docs/decisions/0031-freeform-window-configuration.md):
//!
//! - **Density.** `densityDpi = dpi * 160 / 96`, rounded: one Android `dp` is
//!   one Windows effective pixel, so Android UI has the size Windows UI has
//!   on the same monitor and scale (100% = 160, 125% = 200, 150% = 240,
//!   200% = 320). No rounding to density buckets.
//! - **Bounds.** The Android window is the whole client area of the
//!   Windows window, caption included. The caption is reported as a
//!   `captionBar` inset at the top; the app draws under it like under a
//!   status bar. Apps targeting SDK 35 or later get screen sizes from the
//!   full bounds (edge to edge); older apps get them without the caption,
//!   as Android computes them without decor insets for those apps.
//! - **Orientation** from the aspect: portrait when width <= height.
//! - **Not resizable** (`resizeableActivity="false"`): a fixed content size
//!   in dp, chosen once at launch from the requested orientation and
//!   aspect ratio limits; the window is sized to it, and when the window is
//!   larger (maximized), the content is centered below the caption and the
//!   rest is the window's backdrop. If the window is smaller (a smaller
//!   monitor), the content is scaled down uniformly, never stretched, and
//!   the configuration stays the same (Android's size compatibility mode).
//! - **Minimum size**: the activity's `<layout android:minWidth/minHeight>`,
//!   at least Android's freeform minimum of 220 dp.

use crate::android::config::*;
use crate::geom::{Insets, Rect, Size};

/// Windows' baseline DPI (100% scale).
pub const BASELINE_DPI: u32 = 96;

/// The smallest freeform window on Android (`default_minimal_size_resizable_task`).
pub const MIN_FREEFORM_DP: i32 = 220;

/// The height of the caption (title bar) in Windows effective pixels.
pub const CAPTION_HEIGHT_DIP: i32 = 32;

/// The first SDK level whose apps are laid out edge to edge by default and
/// get screen sizes that include system bar and caption insets.
pub const EDGE_TO_EDGE_SDK: u32 = 35;

/// Aspect ratio (long side / short side) of a non-resizable app's content
/// when the app does not limit it.
pub const DEFAULT_FIXED_ASPECT: f32 = 16.0 / 9.0;

/// Fraction of the monitor's work area a new window uses on its long side.
pub const INITIAL_WORK_AREA_FRACTION: f32 = 0.75;

/// Android `densityDpi` for a Windows DPI.
pub fn density_dpi(dpi: u32) -> u32 {
    (dpi * DENSITY_DEFAULT + BASELINE_DPI / 2) / BASELINE_DPI
}

/// Pixels to dp, rounded as Android rounds (`(int) (px / density + 0.5f)`).
pub fn px_to_dp(px: i32, density_dpi: u32) -> i32 {
    let d = density_dpi as i64;
    let px = px as i64;
    ((px * DENSITY_DEFAULT as i64 * 2 + d) / (2 * d)) as i32
}

/// dp to pixels, rounded (`(int) (dp * density + 0.5f)`).
pub fn dp_to_px(dp: i32, density_dpi: u32) -> i32 {
    let d = density_dpi as i64;
    ((dp as i64 * d * 2 + DENSITY_DEFAULT as i64) / (2 * DENSITY_DEFAULT as i64)) as i32
}

/// Windows effective pixels (DIP) to physical pixels at `dpi`.
pub fn dip_to_px(dip: i32, dpi: u32) -> i32 {
    ((dip as i64 * dpi as i64 * 2 + BASELINE_DPI as i64) / (2 * BASELINE_DPI as i64)) as i32
}

/// The orientation an activity requests (`android:screenOrientation`),
/// reduced to what matters for a desktop window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestedOrientation {
    #[default]
    Unspecified,
    Portrait,
    Landscape,
}

/// What the activity declares about its window (from the manifest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActivitySpec {
    /// `android:resizeableActivity` (true by default from SDK 24).
    pub resizeable: bool,
    /// `<layout android:minWidth>` in dp.
    pub min_width_dp: Option<i32>,
    /// `<layout android:minHeight>` in dp.
    pub min_height_dp: Option<i32>,
    pub orientation: RequestedOrientation,
    /// `android:minAspectRatio` (long / short, >= 1).
    pub min_aspect_ratio: Option<f32>,
    /// `android:maxAspectRatio` (long / short, >= 1).
    pub max_aspect_ratio: Option<f32>,
    /// The app's `targetSdkVersion`.
    pub target_sdk: u32,
}

impl Default for ActivitySpec {
    fn default() -> Self {
        Self {
            resizeable: true,
            min_width_dp: None,
            min_height_dp: None,
            orientation: RequestedOrientation::Unspecified,
            min_aspect_ratio: None,
            max_aspect_ratio: None,
            target_sdk: EDGE_TO_EDGE_SDK,
        }
    }
}

/// Facts about the PC and the user's settings that are part of the
/// configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceTraits {
    /// The PC has a touch screen.
    pub touchscreen: bool,
    /// A hardware keyboard is present.
    pub keyboard: bool,
    /// Windows apps use the dark theme.
    pub night: bool,
    /// The UI language is written right to left.
    pub rtl: bool,
    /// Windows text scale (Accessibility > Text size), 1.0 by default.
    pub font_scale: f32,
}

impl Default for DeviceTraits {
    fn default() -> Self {
        Self {
            touchscreen: false,
            keyboard: true,
            night: false,
            rtl: false,
            font_scale: 1.0,
        }
    }
}

/// The measured state of a Windows window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowMetrics {
    /// The client area in display coordinates (relative to the monitor's
    /// top left corner), physical pixels.
    pub client: Rect,
    /// The monitor's rectangle in its own coordinates (origin 0, 0).
    pub display: Rect,
    /// The window's DPI (its monitor's DPI).
    pub dpi: u32,
    /// The caption height in physical pixels.
    pub caption_px: i32,
}

/// `android.app.WindowConfiguration`, the fields a freeform window has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowConfiguration {
    pub bounds: Rect,
    pub app_bounds: Rect,
    pub max_bounds: Rect,
    pub windowing_mode: i32,
}

/// `android.content.res.Configuration`, the fields the compositor owns.
/// Locale, MCC/MNC and the like are set by the system server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Configuration {
    pub font_scale: f32,
    pub density_dpi: u32,
    pub screen_width_dp: i32,
    pub screen_height_dp: i32,
    pub smallest_screen_width_dp: i32,
    pub orientation: i32,
    pub screen_layout: i32,
    pub ui_mode: i32,
    pub touchscreen: i32,
    pub keyboard: i32,
    pub keyboard_hidden: i32,
    pub hard_keyboard_hidden: i32,
    pub navigation: i32,
    pub navigation_hidden: i32,
    pub window: WindowConfiguration,
}

/// Everything an Android window gets from one state of its Windows window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowLayout {
    pub config: Configuration,
    /// `WindowInsets.Type.captionBar()`, in the window's coordinates.
    pub caption_insets: Insets,
    /// Where the app's surfaces go, in client coordinates. The whole client
    /// area unless letterboxed.
    pub content: Rect,
    /// Content pixels per app pixel: 1.0 unless the fixed content does not
    /// fit (size compatibility scaling).
    pub scale: f32,
    pub letterboxed: bool,
}

/// `Configuration.screenLayout` for a size in dp (Android's documented
/// size buckets: normal from 470x320 dp, large from 640x480, xlarge from
/// 960x720).
pub fn screen_layout(width_dp: i32, height_dp: i32, rtl: bool) -> i32 {
    let long = width_dp.max(height_dp);
    let short = width_dp.min(height_dp);
    let (size, is_long) = if long < 470 {
        (SCREENLAYOUT_SIZE_SMALL, false)
    } else {
        let size = if long >= 960 && short >= 720 {
            SCREENLAYOUT_SIZE_XLARGE
        } else if long >= 640 && short >= 480 {
            SCREENLAYOUT_SIZE_LARGE
        } else {
            SCREENLAYOUT_SIZE_NORMAL
        };
        (size, (long * 3) / 5 >= short - 1)
    };
    size | if is_long {
        SCREENLAYOUT_LONG_YES
    } else {
        SCREENLAYOUT_LONG_NO
    } | if rtl {
        SCREENLAYOUT_LAYOUTDIR_RTL
    } else {
        SCREENLAYOUT_LAYOUTDIR_LTR
    } | SCREENLAYOUT_ROUND_NO
}

/// Portrait when width <= height, as Android decides it.
pub fn orientation(width: i32, height: i32) -> i32 {
    if width <= height {
        ORIENTATION_PORTRAIT
    } else {
        ORIENTATION_LANDSCAPE
    }
}

/// The aspect ratio a non-resizable app gets: the default, within the
/// app's limits.
fn fixed_aspect(spec: &ActivitySpec) -> f32 {
    let mut a = DEFAULT_FIXED_ASPECT;
    if let Some(max) = spec.max_aspect_ratio.filter(|m| *m >= 1.0) {
        a = a.min(max);
    }
    if let Some(min) = spec.min_aspect_ratio.filter(|m| *m >= 1.0) {
        a = a.max(min);
    }
    a
}

/// The content size, in dp, of a non-resizable app: `fraction` of the
/// work area on the long side of the requested orientation, at the app's
/// aspect ratio, fitted into the work area below the caption.
pub fn fixed_content_dp(spec: &ActivitySpec, work_area: Size, dpi: u32) -> Size {
    let density = density_dpi(dpi);
    let caption = dip_to_px(CAPTION_HEIGHT_DIP, dpi);
    let avail_w = px_to_dp(work_area.width, density) as f32 * INITIAL_WORK_AREA_FRACTION;
    let avail_h =
        px_to_dp(work_area.height - caption, density) as f32 * INITIAL_WORK_AREA_FRACTION;
    let aspect = fixed_aspect(spec);
    let landscape = spec.orientation == RequestedOrientation::Landscape;
    let (w, h) = if landscape {
        // Long side horizontal.
        let w = avail_w.min(avail_h * aspect);
        (w, w / aspect)
    } else {
        let h = avail_h.min(avail_w * aspect);
        (h / aspect, h)
    };
    let min = MIN_FREEFORM_DP as f32;
    Size::new(w.max(min).round() as i32, h.max(min).round() as i32)
}

/// The client size of a new window for `spec` on a monitor with this
/// work area. `fixed_dp` is the content size of a non-resizable app.
pub fn initial_client_size(
    spec: &ActivitySpec,
    fixed_dp: Option<Size>,
    work_area: Size,
    dpi: u32,
) -> Size {
    let density = density_dpi(dpi);
    let caption = dip_to_px(CAPTION_HEIGHT_DIP, dpi);
    if let (false, Some(f)) = (spec.resizeable, fixed_dp) {
        return Size::new(
            dp_to_px(f.width, density),
            dp_to_px(f.height, density) + caption,
        );
    }
    let frac = INITIAL_WORK_AREA_FRACTION;
    let size = match spec.orientation {
        // A portrait-first app gets a portrait window of the default
        // aspect; others a landscape share of the work area.
        RequestedOrientation::Portrait => {
            let h = work_area.height as f32 * frac;
            Size::new((h / DEFAULT_FIXED_ASPECT) as i32, h as i32)
        }
        _ => Size::new(
            (work_area.width as f32 * frac) as i32,
            (work_area.height as f32 * frac) as i32,
        ),
    };
    let min = min_client_size(spec, fixed_dp, dpi);
    Size::new(size.width.max(min.width), size.height.max(min.height))
}

/// The smallest client size the window may be resized to. A non-resizable
/// app's window cannot be smaller than its content.
pub fn min_client_size(spec: &ActivitySpec, fixed_dp: Option<Size>, dpi: u32) -> Size {
    let density = density_dpi(dpi);
    let caption = dip_to_px(CAPTION_HEIGHT_DIP, dpi);
    if let (false, Some(f)) = (spec.resizeable, fixed_dp) {
        return Size::new(
            dp_to_px(f.width, density),
            dp_to_px(f.height, density) + caption,
        );
    }
    let w = spec.min_width_dp.unwrap_or(0).max(MIN_FREEFORM_DP);
    let h = spec.min_height_dp.unwrap_or(0).max(MIN_FREEFORM_DP);
    // The app's minimum height is for its content, below the caption for
    // apps that are not edge to edge; the caption is added for all, so the
    // content area is never smaller than asked.
    Size::new(dp_to_px(w, density), dp_to_px(h, density) + caption)
}

/// Computes the configuration and layout of an Android window from the
/// state of its Windows window. `fixed_dp` is the content size of a
/// non-resizable app ([`fixed_content_dp`]), ignored for resizable apps.
pub fn compute(
    m: &WindowMetrics,
    spec: &ActivitySpec,
    fixed_dp: Option<Size>,
    traits: &DeviceTraits,
) -> WindowLayout {
    let density = density_dpi(m.dpi);
    let client_size = m.client.size();
    let caption = m.caption_px.clamp(0, client_size.height.max(0));

    let (bounds, content, caption_insets, scale, letterboxed, size_dp) =
        match (spec.resizeable, fixed_dp) {
            (false, Some(f)) => {
                // Below the caption, centered, scaled down only if needed.
                let area = Rect::new(0, caption, client_size.width, client_size.height);
                let want = Size::new(dp_to_px(f.width, density), dp_to_px(f.height, density));
                let scale = if want.width > area.width() || want.height > area.height() {
                    (area.width() as f32 / want.width as f32)
                        .min(area.height() as f32 / want.height as f32)
                        .max(0.0)
                } else {
                    1.0
                };
                let shown = Size::new(
                    (want.width as f32 * scale).round() as i32,
                    (want.height as f32 * scale).round() as i32,
                );
                let content = area.center(shown);
                // Android sees its unscaled window at the content's origin.
                let bounds = Rect::from_size(
                    m.client.left + content.left,
                    m.client.top + content.top,
                    want,
                );
                let letterboxed = content != area;
                (bounds, content, Insets::NONE, scale, letterboxed, f)
            }
            _ => {
                let content = Rect::new(0, 0, client_size.width, client_size.height);
                let insets = Insets::top(caption);
                // Legacy apps get sizes without the caption (non-decor
                // bounds); edge-to-edge apps get the full window.
                let sized = if spec.target_sdk >= EDGE_TO_EDGE_SDK {
                    m.client
                } else {
                    m.client.inset(insets)
                };
                let size_dp = Size::new(
                    px_to_dp(sized.width(), density),
                    px_to_dp(sized.height(), density),
                );
                (m.client, content, insets, 1.0, false, size_dp)
            }
        };

    let app_bounds = if spec.target_sdk >= EDGE_TO_EDGE_SDK || letterboxed {
        bounds
    } else {
        bounds.inset(caption_insets)
    };

    let config = Configuration {
        font_scale: traits.font_scale,
        density_dpi: density,
        screen_width_dp: size_dp.width,
        screen_height_dp: size_dp.height,
        smallest_screen_width_dp: size_dp.width.min(size_dp.height),
        orientation: orientation(size_dp.width, size_dp.height),
        screen_layout: screen_layout(size_dp.width, size_dp.height, traits.rtl),
        ui_mode: UI_MODE_TYPE_NORMAL
            | if traits.night {
                UI_MODE_NIGHT_YES
            } else {
                UI_MODE_NIGHT_NO
            },
        touchscreen: if traits.touchscreen {
            TOUCHSCREEN_FINGER
        } else {
            TOUCHSCREEN_NOTOUCH
        },
        keyboard: if traits.keyboard {
            KEYBOARD_QWERTY
        } else {
            KEYBOARD_NOKEYS
        },
        keyboard_hidden: if traits.keyboard {
            KEYBOARDHIDDEN_NO
        } else {
            KEYBOARDHIDDEN_YES
        },
        hard_keyboard_hidden: if traits.keyboard {
            HARDKEYBOARDHIDDEN_NO
        } else {
            HARDKEYBOARDHIDDEN_YES
        },
        navigation: NAVIGATION_NONAV,
        navigation_hidden: NAVIGATIONHIDDEN_NO,
        window: WindowConfiguration {
            bounds,
            app_bounds,
            max_bounds: m.display,
            windowing_mode: WINDOWING_MODE_FREEFORM,
        },
    };
    WindowLayout {
        config,
        caption_insets,
        content,
        scale,
        letterboxed,
    }
}

/// The `ActivityInfo.CONFIG_*` bits that differ between two
/// configurations (what `Configuration.diff` returns for these fields).
pub fn diff(old: &Configuration, new: &Configuration) -> u32 {
    let mut c = 0;
    let mut set = |cond: bool, bit: u32| {
        if cond {
            c |= bit;
        }
    };
    set(old.font_scale != new.font_scale, CONFIG_FONT_SCALE);
    set(old.density_dpi != new.density_dpi, CONFIG_DENSITY);
    set(
        old.screen_width_dp != new.screen_width_dp || old.screen_height_dp != new.screen_height_dp,
        CONFIG_SCREEN_SIZE,
    );
    set(
        old.smallest_screen_width_dp != new.smallest_screen_width_dp,
        CONFIG_SMALLEST_SCREEN_SIZE,
    );
    set(old.orientation != new.orientation, CONFIG_ORIENTATION);
    let dir = SCREENLAYOUT_LAYOUTDIR_LTR | SCREENLAYOUT_LAYOUTDIR_RTL;
    set(
        (old.screen_layout & !dir) != (new.screen_layout & !dir),
        CONFIG_SCREEN_LAYOUT,
    );
    set(
        (old.screen_layout & dir) != (new.screen_layout & dir),
        CONFIG_LAYOUT_DIRECTION,
    );
    set(old.ui_mode != new.ui_mode, CONFIG_UI_MODE);
    set(old.touchscreen != new.touchscreen, CONFIG_TOUCHSCREEN);
    set(old.keyboard != new.keyboard, CONFIG_KEYBOARD);
    set(
        old.keyboard_hidden != new.keyboard_hidden
            || old.hard_keyboard_hidden != new.hard_keyboard_hidden,
        CONFIG_KEYBOARD_HIDDEN,
    );
    set(
        old.navigation != new.navigation || old.navigation_hidden != new.navigation_hidden,
        CONFIG_NAVIGATION,
    );
    set(old.window != new.window, CONFIG_WINDOW_CONFIGURATION);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_follows_windows_scale() {
        for (dpi, density) in [
            (96, 160),
            (120, 200),
            (144, 240),
            (168, 280),
            (192, 320),
            (216, 360),
            (240, 400),
            (288, 480),
            (336, 560),
            (384, 640),
        ] {
            assert_eq!(density_dpi(dpi), density, "dpi {dpi}");
        }
    }

    #[test]
    fn dp_px_round_trip_and_rounding() {
        assert_eq!(px_to_dp(1000, 160), 1000);
        assert_eq!(px_to_dp(1001, 240), 667); // 667.33
        assert_eq!(px_to_dp(1003, 240), 669); // 668.67 rounds up
        assert_eq!(px_to_dp(3, 240), 2); // 2.0
        assert_eq!(dp_to_px(220, 240), 330);
        assert_eq!(dp_to_px(1, 200), 1); // 1.25
        assert_eq!(dp_to_px(3, 200), 4); // 3.75
        assert_eq!(dip_to_px(32, 144), 48);
        assert_eq!(dip_to_px(32, 120), 40);
    }

    #[test]
    fn screen_layout_buckets() {
        use crate::android::config::*;
        let size = |w, h| screen_layout(w, h, false) & 0x0f;
        assert_eq!(size(320, 426), SCREENLAYOUT_SIZE_SMALL);
        assert_eq!(size(320, 470), SCREENLAYOUT_SIZE_NORMAL);
        assert_eq!(size(411, 891), SCREENLAYOUT_SIZE_NORMAL);
        assert_eq!(size(480, 640), SCREENLAYOUT_SIZE_LARGE);
        assert_eq!(size(800, 1280), SCREENLAYOUT_SIZE_XLARGE);
        assert_eq!(size(1280, 720), SCREENLAYOUT_SIZE_XLARGE);
        // A phone is long, a 4:3 tablet is not.
        assert_ne!(screen_layout(411, 891, false) & SCREENLAYOUT_LONG_YES, 0);
        assert_ne!(screen_layout(768, 1024, false) & SCREENLAYOUT_LONG_NO, 0);
        assert_ne!(
            screen_layout(768, 1024, true) & SCREENLAYOUT_LAYOUTDIR_RTL,
            0
        );
    }

    fn metrics(w: i32, h: i32, dpi: u32) -> WindowMetrics {
        WindowMetrics {
            client: Rect::from_size(100, 50, Size::new(w, h)),
            display: Rect::new(0, 0, 2560, 1600),
            dpi,
            caption_px: dip_to_px(CAPTION_HEIGHT_DIP, dpi),
        }
    }

    #[test]
    fn resizable_window_table() {
        // (client w, h, dpi, target sdk) ->
        // (density, w dp, h dp, smallest, orientation, caption px)
        let table = [
            (800, 600, 96, 35, (160, 800, 600, 600, ORIENTATION_LANDSCAPE, 32)),
            (800, 600, 96, 33, (160, 800, 568, 568, ORIENTATION_LANDSCAPE, 32)),
            (600, 900, 144, 36, (240, 400, 600, 400, ORIENTATION_PORTRAIT, 48)),
            (600, 900, 144, 30, (240, 400, 568, 400, ORIENTATION_PORTRAIT, 48)),
            (1000, 1000, 120, 35, (200, 800, 800, 800, ORIENTATION_PORTRAIT, 40)),
            (2560, 1528, 192, 35, (320, 1280, 764, 764, ORIENTATION_LANDSCAPE, 64)),
        ];
        let traits = DeviceTraits::default();
        for (w, h, dpi, sdk, want) in table {
            let spec = ActivitySpec {
                target_sdk: sdk,
                ..Default::default()
            };
            let l = compute(&metrics(w, h, dpi), &spec, None, &traits);
            let c = &l.config;
            assert_eq!(
                (
                    c.density_dpi,
                    c.screen_width_dp,
                    c.screen_height_dp,
                    c.smallest_screen_width_dp,
                    c.orientation,
                    l.caption_insets.top
                ),
                want,
                "{w}x{h} at {dpi} dpi, sdk {sdk}"
            );
            assert!(!l.letterboxed);
            assert_eq!(l.scale, 1.0);
            assert_eq!(l.content, Rect::new(0, 0, w, h));
            assert_eq!(c.window.bounds, Rect::from_size(100, 50, Size::new(w, h)));
            assert_eq!(c.window.windowing_mode, WINDOWING_MODE_FREEFORM);
            assert_eq!(c.window.max_bounds, Rect::new(0, 0, 2560, 1600));
            if sdk >= EDGE_TO_EDGE_SDK {
                assert_eq!(c.window.app_bounds, c.window.bounds);
            } else {
                assert_eq!(c.window.app_bounds.top, c.window.bounds.top + want.5);
            }
        }
    }

    #[test]
    fn traits_reach_the_configuration() {
        let spec = ActivitySpec::default();
        let m = metrics(800, 600, 96);
        let plain = compute(&m, &spec, None, &DeviceTraits::default()).config;
        assert_eq!(plain.touchscreen, TOUCHSCREEN_NOTOUCH);
        assert_eq!(plain.keyboard, KEYBOARD_QWERTY);
        assert_eq!(plain.hard_keyboard_hidden, HARDKEYBOARDHIDDEN_NO);
        assert_eq!(plain.ui_mode, UI_MODE_TYPE_NORMAL | UI_MODE_NIGHT_NO);
        let t = DeviceTraits {
            touchscreen: true,
            night: true,
            rtl: true,
            font_scale: 1.25,
            ..Default::default()
        };
        let dark = compute(&m, &spec, None, &t).config;
        assert_eq!(dark.touchscreen, TOUCHSCREEN_FINGER);
        assert_eq!(dark.ui_mode, UI_MODE_TYPE_NORMAL | UI_MODE_NIGHT_YES);
        assert_eq!(
            diff(&plain, &dark),
            CONFIG_TOUCHSCREEN | CONFIG_UI_MODE | CONFIG_LAYOUT_DIRECTION | CONFIG_FONT_SCALE
        );
    }

    #[test]
    fn resize_and_monitor_change_diffs() {
        let spec = ActivitySpec::default();
        let t = DeviceTraits::default();
        let a = compute(&metrics(800, 600, 96), &spec, None, &t).config;
        // Narrower: screen size and window, orientation flips.
        let b = compute(&metrics(500, 600, 96), &spec, None, &t).config;
        assert_eq!(
            diff(&a, &b),
            CONFIG_SCREEN_SIZE
                | CONFIG_SMALLEST_SCREEN_SIZE
                | CONFIG_ORIENTATION
                | CONFIG_SCREEN_LAYOUT
                | CONFIG_WINDOW_CONFIGURATION
        );
        // Moved to a 150% monitor with the same dp size: density and
        // window bounds change, sizes in dp do not.
        let c = compute(&metrics(1200, 900, 144), &spec, None, &t).config;
        assert_eq!(
            diff(&a, &c),
            CONFIG_DENSITY | CONFIG_WINDOW_CONFIGURATION
        );
        // Only moved: window configuration only.
        let mut m = metrics(800, 600, 96);
        m.client = m.client.offset(30, 0);
        let d = compute(&m, &spec, None, &t).config;
        assert_eq!(diff(&a, &d), CONFIG_WINDOW_CONFIGURATION);
        assert_eq!(diff(&a, &a), 0);
    }

    #[test]
    fn non_resizable_apps_are_letterboxed_not_stretched() {
        let spec = ActivitySpec {
            resizeable: false,
            orientation: RequestedOrientation::Portrait,
            ..Default::default()
        };
        // A 1920x1080 work area at 100%: 75% of the height below the
        // caption, at 16:9.
        let fixed = fixed_content_dp(&spec, Size::new(1920, 1040), 96);
        assert_eq!(fixed, Size::new(425, 756));
        let t = DeviceTraits::default();

        // Window sized to the content: no letterbox.
        let init = initial_client_size(&spec, Some(fixed), Size::new(1920, 1040), 96);
        assert_eq!(init, Size::new(425, 756 + 32));
        let l = compute(&metrics(init.width, init.height, 96), &spec, Some(fixed), &t);
        assert!(!l.letterboxed);
        assert_eq!(l.content, Rect::new(0, 32, 425, 788));
        assert_eq!(l.caption_insets, Insets::NONE);
        assert_eq!(
            (l.config.screen_width_dp, l.config.screen_height_dp),
            (425, 756)
        );
        assert_eq!(l.config.orientation, ORIENTATION_PORTRAIT);

        // Maximized: same configuration, content centered below the
        // caption, bounds at the content.
        let big = compute(&metrics(1920, 1040, 96), &spec, Some(fixed), &t);
        assert!(big.letterboxed);
        assert_eq!(big.scale, 1.0);
        assert_eq!(big.content, Rect::new(747, 158, 1172, 914));
        assert_eq!(big.config.window.bounds, Rect::new(847, 208, 1272, 964));
        assert_eq!(
            diff(&l.config, &big.config),
            CONFIG_WINDOW_CONFIGURATION
        );

        // Moved to a 150% monitor of the same pixel size: the content keeps
        // its dp size (638x1134 px), which no longer fits below the 48 px
        // caption, so it is scaled down to the 992 px available.
        let hi = compute(&metrics(1920, 1040, 144), &spec, Some(fixed), &t);
        assert_eq!(
            (hi.config.screen_width_dp, hi.config.screen_height_dp),
            (425, 756)
        );
        assert_eq!(hi.config.density_dpi, 240);
        assert_eq!(hi.content.height(), 992);
        assert_eq!(hi.content.top, 48);
        assert!((hi.scale - 992.0 / 1134.0).abs() < 1e-4);
    }

    #[test]
    fn fixed_content_scales_down_uniformly_when_it_does_not_fit() {
        let spec = ActivitySpec {
            resizeable: false,
            ..Default::default()
        };
        let fixed = Size::new(400, 800);
        let t = DeviceTraits::default();
        // 432 px below a 32 px caption: half the height.
        let l = compute(&metrics(1000, 432, 96), &spec, Some(fixed), &t);
        assert!(l.letterboxed);
        assert_eq!(l.scale, 0.5);
        assert_eq!(l.content, Rect::new(400, 32, 600, 432));
        assert_eq!(
            (l.config.screen_width_dp, l.config.screen_height_dp),
            (400, 800)
        );
    }

    #[test]
    fn aspect_limits_shape_fixed_content() {
        let work = Size::new(1920, 1040);
        let base = ActivitySpec {
            resizeable: false,
            ..Default::default()
        };
        let landscape = ActivitySpec {
            orientation: RequestedOrientation::Landscape,
            ..base
        };
        let l = fixed_content_dp(&landscape, work, 96);
        assert!(l.width > l.height);
        assert!(((l.width as f32 / l.height as f32) - 16.0 / 9.0).abs() < 0.01);
        let narrow = ActivitySpec {
            max_aspect_ratio: Some(1.5),
            ..base
        };
        let n = fixed_content_dp(&narrow, work, 96);
        assert!(((n.height as f32 / n.width as f32) - 1.5).abs() < 0.01);
        let wide = ActivitySpec {
            min_aspect_ratio: Some(2.0),
            ..base
        };
        let w = fixed_content_dp(&wide, work, 96);
        assert!(((w.height as f32 / w.width as f32) - 2.0).abs() < 0.01);
    }

    #[test]
    fn minimum_sizes() {
        let t = ActivitySpec::default();
        assert_eq!(min_client_size(&t, None, 96), Size::new(220, 252));
        assert_eq!(min_client_size(&t, None, 144), Size::new(330, 378));
        let big = ActivitySpec {
            min_width_dp: Some(400),
            min_height_dp: Some(100),
            ..t
        };
        assert_eq!(min_client_size(&big, None, 96), Size::new(400, 252));
        let fixed = ActivitySpec {
            resizeable: false,
            ..t
        };
        assert_eq!(
            min_client_size(&fixed, Some(Size::new(300, 500)), 120),
            Size::new(375, 625 + 40)
        );
        // A new resizable window respects the minimum on a tiny monitor.
        let s = initial_client_size(&big, None, Size::new(300, 200), 96);
        assert_eq!(s, Size::new(400, 252));
    }
}

# Product requirements

## Users

Private users on Windows 11 who have an Android app they want on their PC: a
messaging app, a bank or health provider's app (where it works without Play
Integrity), a transit app, a game, a utility that has no Windows version.
They are not developers. They expect Windows conventions, Hebrew and English,
and clear explanations when something does not work.

Developers are a secondary audience: `adb` compatibility comes after the core
product.

Requirement IDs: `P-` product, `D-` desktop integration, `M-` management app.
Priority: **M** must for the first public preview, **S** should, **L** later.

## Installing Andruna

| ID | Requirement | Pri |
| --- | --- | --- |
| P-1 | A single signed installer, per-user, no administrator rights | M |
| P-2 | The installer detects x64 or ARM64 and installs the matching build | M |
| P-3 | After install, `.apk`, `.apks`, `.xapk`, `.apkm` open with Andruna | M |
| P-4 | Andruna appears in Windows "Installed apps" and uninstalls cleanly, asking whether to keep app data | M |
| P-5 | Andruna updates itself in the background; installing an update needs one confirmation and keeps all apps and data | S |
| P-6 | First run: a short welcome flow (language, theme follows Windows, optional Google account) | S |

## Installing an app

| ID | Requirement | Pri |
| --- | --- | --- |
| P-10 | Double-click or drag a package onto the management app to install | M |
| P-11 | Split bundles: choose the splits that match the CPU, language and display density | M |
| P-12 | An install screen shows the app's icon, name, version, developer certificate and requested permissions before installing | M |
| P-13 | Signature verification (schemes v1 to v4); an update must be signed by the same key or a valid v3 rotation | M |
| P-14 | An APK whose native code cannot run on this PC is refused with a clear reason (for example only `armeabi-v7a`) | M |
| P-15 | ART compiles the app at install time with its baseline profile, with visible progress | M |
| P-16 | After install, the app appears in the Start menu with its own icon | M |

## Running an app

| ID | Requirement | Pri |
| --- | --- | --- |
| P-20 | The app opens in its own window, in its own process | M |
| P-21 | A splash with the app's icon appears immediately, before the first frame | M |
| P-22 | Permission requests show Android's permission dialog, styled as a Windows dialog, and also respect Windows privacy settings | M |
| P-23 | A crash shows a Windows dialog with the reason in plain language and a "Report a problem" button | M |
| P-24 | Closing the last window of an app follows Android's lifecycle (stop, then the process may be reclaimed) | M |
| P-25 | Apps receive push messages and run scheduled jobs while Andruna is running, even with no window open | S |

## Desktop integration

| ID | Requirement | Pri |
| --- | --- | --- |
| D-1 | Windows are resizable; the app gets real configuration changes (Android freeform windowing) | M |
| D-2 | Apps that are not resizable get a fixed-size window; when maximized, the content is centered on a styled background, never stretched | M |
| D-3 | Windows 11 frame: Mica title bar, rounded corners, Snap Layouts, a back button in the title bar | M |
| D-4 | Title bar color follows the status bar color the app requests | S |
| D-5 | Each package has its own AppUserModelID: its windows group under one taskbar icon | M |
| D-6 | Taskbar badge with the notification count; Jump List from the app's shortcuts | S |
| D-7 | Per-monitor DPI: density follows the monitor, moving a window triggers a configuration change | M |
| D-8 | Dark/light mode and the Windows accent color reach the app (`UiModeManager`, dynamic color) | M |
| D-9 | Windows text size and high contrast map to `fontScale` and accessibility settings | S |
| D-10 | Mouse: hover, wheel as `ACTION_SCROLL`, right button as secondary button, pointer icons | M |
| D-11 | Touchpad pinch to zoom and smooth scrolling; touch screen and pen as real touch and stylus with pressure and tilt | M |
| D-12 | Keyboard: Esc and Alt+Left as Back; app keyboard shortcuts; full IME via TSF including Hebrew and RTL | M |
| D-13 | Game controllers (XInput, GameInput) as Android input devices | S |
| D-14 | Notifications as Windows toasts: actions, inline reply, images, progress; channels listed in Windows notification settings | M |
| D-15 | Share: `ACTION_SEND` opens the Windows share sheet; Android apps appear as Windows share targets | S |
| D-16 | Drag and drop of files and text in both directions | S |
| D-17 | Deep links and file types from intent filters registered with Windows, only after the user agrees | S |
| D-18 | Clipboard: text, rich text, images, URIs | M |
| D-19 | Shared storage maps to the user's Downloads, Pictures, Music, Videos and Documents folders | M |

## Management app

A WinUI 3 app ("Andruna") that follows Windows 11 design (NavigationView,
Mica, Fluent icons), in Hebrew (RTL) and English.

| ID | Screen | Contents | Pri |
| --- | --- | --- | --- |
| M-1 | Library | Installed apps as tiles: search, running state, size; install by drag and drop | M |
| M-2 | App page | Permissions, storage (clear cache, clear data), default window size, GPU preference, start with Windows, uninstall | M |
| M-3 | Diagnostics | Why an app failed, in plain language; the list of missing functions the app called most; one-click problem report with logs | M |
| M-4 | Google | Account sign-in, tier selection, the real-services setup wizard, device registration helper | S |
| M-5 | Settings | Language, updates, background activity, storage location, telemetry (off by default) | M |
| M-6 | About | Version, licenses, links | M |

## User-facing text

- Hebrew and English from the first preview; all strings in resource files.
- Error messages say what happened, why, and what the user can do. They never
  show raw exception text alone.
- User documentation (getting started, troubleshooting, FAQ) in both languages,
  with screenshots, maintained with the product.

## Developers (later)

| ID | Requirement | Pri |
| --- | --- | --- |
| P-40 | An adbd-compatible endpoint: `adb install`, `adb logcat`, `adb shell`, Android Studio debugging | L |
| P-41 | Native crashes produce Windows minidumps with PDBs for our code | S |
| P-42 | ETW events for app start phases, frames and Binder transactions | S |

# Phase 2/4: the package manager backend (andruna-pkg)

Owner module: `crates/andruna-pkg`. Used by andruna-sysd (`src/pkg.rs`).
Decisions: [ADR 0027](../../decisions/0027-installed-apps-entries.md)
(Installed apps entries, launcher-driven uninstall),
[ADR 0028](../../decisions/0028-package-manager-in-rust.md) (Rust parsers,
dependencies, ABI and split policy), and the shortcut convention of
[ADR 0034](../../decisions/0034-file-association-and-shortcuts.md).

## Goals and status

| Goal | Status |
| --- | --- |
| Zip with Android's checks, alignment rules | Done. `resources.arsc` stored and 4-byte aligned from target API 30; uncompressed page-aligned `.so` with `extractNativeLibs="false"` |
| Binary XML manifest | Done: package, versionCode (+major), versionName, min/target/max SDK, label, icon, activities and aliases, intent filters (MAIN/LAUNCHER), permissions, `hasCode`, `extractNativeLibs`; native ABIs from `lib/` |
| `resources.arsc` for label and icon | Done: locales (Hebrew as `iw` or `he`), densities, SDK qualifiers, references; labels stored per locale |
| Icon to `.ico` | Done: PNG, WebP, vector drawables, adaptive icons, insets, layer lists; `.ico` at 16, 20, 24, 32, 40, 48, 64, 256 px |
| Signatures v2/v3 (and v1) with apksig semantics | Done: v1, v2, v3, v3.1, rotation lineage, stripping protection, update rule. Not done: v4, DSA, verity variants (always paired with a supported one), source stamps |
| Install over IPC | Done: InspectPackage, InstallPackage, UninstallPackage, ListPackageInfo, ListPackages from the database |
| Per-user store, splits, bundles | Done: `app\<package>\`, `data\<package>\`; config splits for ABI, language, density; `.apks`, `.xapk`, `.apkm`, `.zip` bundles (unencrypted) |
| dex2oat at install | Done through `andruna-app --compile`; reports `unavailable` when andruna-app or the ART root is missing; a failure does not fail the install (as on Android) |
| Package database | Done: `system\packages.json`, atomic, versioned, stable uids from 10000, per-install salt, signer certificates and lineage |
| Uninstall | Done: shortcuts, Installed apps entry, AppContainer profile, files, data unless kept; refused while the app runs |
| Start menu shortcuts, AppUserModelID, icon | Done, per launcher activity, as ADR 0034 (`<package>[/<activity>]`) |
| Uninstall entry | Done (ADR 0027): `andruna-launch --uninstall <package>` asks and sends the request |
| AppContainer hook | Done: `hooks::InstallHooks`; sysd's `AppContainerHooks` creates and deletes the profile and secures the directories ([appcontainer](../../../crates/andruna-sysd/src/appcontainer/mod.rs)) |
| Tests with real APKs | Done: aapt2/d8/apksigner fixture (committed), an F-Droid app (downloaded, pinned) |

## Verification (2026-11-27, x64, Windows 11)

- `cargo test -p andruna-pkg`: 28 unit tests, 9 signature tests, 8 install
  tests, 3 real-APK tests; `-p andruna-sysd` (2 package request tests over
  the pipe), `-p andruna-launch` (uninstall through the launcher),
  `-p andruna-ipc` (golden payloads `v1-pkg.txt`). Clippy with
  `-D warnings` and rustfmt clean.
- apksigner (build-tools 36.1.0) accepts APKs signed by the testkit (v1, v2,
  v3, with and without rotation) and andruna-pkg verifies APKs apksigner
  signed (RSA 2048 and P-256; minSdk 16/18 and 24; v1 only). These tests
  skip where the SDK is not installed (CI).
- dex2oat for real: the fixture APK compiled by andruna-app with the x64 ART
  root and boot image (`ANDRUNA_APP_EXE`, `ANDRUNA_ART_ROOT`), 17,856 bytes
  of odex plus vdex in `app\<package>\oat\x86_64\`. Not run in CI (no ART
  root there).
- F-Droid: Binary Eye 1.76.1 (7.5 MB, MIT) verifies with scheme v3, label
  "Binary Eye", ABIs arm64-v8a, armeabi-v7a, x86, x86_64 (x86_64 selected),
  its adaptive icon renders, it installs and uninstalls. f-droid.org itself
  answers automated clients with HTTP 418; the test falls back to an
  official mirror.
- Shortcuts are read back through `IShellLinkW`/`IPropertyStore` (target,
  arguments, icon, AppUserModelID); the Installed apps entry is read back
  from the registry (tests use a key under `Software\Andruna\Tests`).

Observed on the development machine: renaming a freshly written directory
in `%TEMP%` took 60 s at times (later 26 ms for the same operation), which
slowed the install tests to minutes. Install swaps directories by rename;
if this shows up on users' machines (antivirus scanning new files), the
staging step needs another look. Not seen in CI.

## Not done / next

- Install progress: Install and Inspect answer once at the end; P-15 asks
  for visible progress (a progress event on the event stream is the natural
  place).
- Labels from language splits (only the base APK's resources are read).
- Baseline profiles (`assets/dexopt/baseline.prof`) are not passed to
  dex2oat; feature splits with code are not compiled.
- v4 signatures, DSA, the Google package rule of the security spec (Play
  services must be signed by Google), encrypted `.apkm`.
- Shortcut name collisions follow a simple rule (` (Android)` is appended
  when another file has the name); ADR 0034 leaves the rule open.
- The installer's uninstaller should remove the Installed apps entries of
  Android apps (ADR 0027).
- Fuzzing in CI (the security spec asks for it): the parsers have
  deterministic random-input tests, not a fuzzer job yet.

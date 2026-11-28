# 0046 - Guest fonts: Roboto from AOSP, an OFL exception for Noto, Windows fonts until then

**Status:** proposed, 2026-11-28

## Context
Android apps draw text through HWUI, minikin and Skia, which take their
fonts from `/system/etc/fonts.xml` and the files in `/system/fonts`. Apps
expect Roboto for Latin, Greek and Cyrillic and a Noto font for every other
script (Hebrew and Arabic among them), and they measure text with those
fonts, so the font set changes layouts.

AOSP's fonts at our tag (android-17.0.0_r1):

| Project | Files | License |
| --- | --- | --- |
| external/roboto-fonts | `Roboto-Regular.ttf` 3.005 (variable, the default build), `RobotoStatic-Regular.ttf` | Apache-2.0 (`README.android`: "License: Apache 2.0"); later Roboto versions in the same project are OFL-1.1 |
| external/noto-fonts | every non-Latin script, emoji, CJK | SIL Open Font License 1.1; AOSP marks it "by exception only" |

ADR 0006 allows Apache-2.0, MIT, BSD, 0BSD, zlib, ISC and public domain.
OFL-1.1 is not on that list. It is a font license: the fonts may be used,
bundled and redistributed with any software, including proprietary
software; the font files themselves may not be sold alone, and modified
fonts must be renamed. It places no condition on the software that uses
the fonts, so it does not limit how Andruna itself is licensed.

Windows 11 has fonts for every script Android covers (Segoe UI, Segoe UI
Historic, Nirmala UI, Ebrima, Microsoft Yi Baiti, Segoe UI Emoji, Yu Gothic,
Microsoft YaHei, Malgun Gothic, and others; for Hebrew: Segoe UI, Arial,
David, Gisha). They may be used by software on the same machine but not
redistributed.

## Decision
1. **Roboto ships.** Andruna ships `Roboto-Regular.ttf` 3.005 and
   `RobotoStatic-Regular.ttf` from external/roboto-fonts (Apache-2.0),
   recorded in `THIRD_PARTY_NOTICES`, as the `sans-serif` family.
2. **Proposed amendment to ADR 0006: OFL-1.1 for font files.** Font files
   (`.ttf`, `.otf`, `.ttc`) under OFL-1.1 may ship unmodified, each with its
   license text in `THIRD_PARTY_NOTICES`. The exception covers font data
   only, never code. With it, Andruna ships AOSP's Noto set and Android's
   own `fonts.xml`, which gives apps the fonts and metrics they were tested
   with. This needs the maintainer's acceptance.
3. **Until the amendment is accepted: Windows fonts.** Andruna generates a
   `fonts.xml` for the installed Windows fonts (Roboto as `sans-serif`, then
   one fallback family per script from the table in
   [hwui.md](../plan/phase3/hwui.md#fonts)) and maps the Windows font
   directory read-only into the guest as `/system/fonts`. Nothing is
   copied or redistributed. Layouts then differ from a device for
   non-Latin scripts (other glyph shapes and advances); this is reported as
   a known difference, not hidden.
4. **Tests use fixed fonts.** The Skia scene test (`tests/skia`) downloads
   `Roboto-Regular.ttf` and `NotoSansHebrew-Regular.ttf` from AOSP at the
   pinned tag, checked by SHA-256, and never puts them in a build output.
   Under the build and test tools amendment of ADR 0006 this is test data
   that is fetched at test time and not shipped, so the OFL Hebrew font is
   allowed there even before the amendment of point 2. Windows fonts are not
   used in tests, because they differ between Windows builds.

## Consequences
- Latin text matches a device now; other scripts match after the amendment.
- The Windows font mapping is a guest file system mount (andruna-lx) and a
  generated file; it is the next step of the fonts work, listed in
  hwui.md.
- If the amendment is rejected, the Windows fonts stay the permanent answer
  for non-Latin scripts, and app layouts in those scripts keep differing
  from devices.

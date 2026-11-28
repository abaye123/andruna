# Phase 3: Skia and HWUI as guest libraries

**Status:** 2026-11-28, first round of the HWUI track (branch `p3-hwui`).
Decisions: [ADR 0045](../../decisions/0045-skia-and-hwui-guest-build.md)
(the build) and [ADR 0046](../../decisions/0046-guest-fonts.md) (fonts),
both proposed. Code: `aosp/modules/skia*.cmake`, `aosp/modules/hwui*.cmake`,
`aosp/guest/hwui.cmake` (stage `hwui` of `aosp/tools/build_guest.py`),
`aosp/overlay/skia`, `tests/skia`, CI `.github/workflows/hwui.yml`.

## What is built

Everything below is Android ELF for the guest (ADR 0013), from AOSP at
`android-17.0.0_r1`, x64 built and run here, arm64 cross-built in CI.

| Library | From | Notes |
| --- | --- | --- |
| `libskia` (static) | external/skia, `Android.bp` `libskia` through `blueprint.py` | 862 of Android's 912 x86_64 sources: Vulkan (44), AVIF, RAW, Fontations (3) and Perfetto (1) left out (ADR 0045); Ganesh GL, CPU raster, PDF, SkSL, all other codecs |
| `libskia_shaper` (static, tests) | external/skia `modules/skshaper`, `modules/skunicode`, `SkFontMgr_android` | HarfBuzz shaper, ICU bidi and line breaking, the fonts.xml font manager; HWUI shapes with minikin instead |
| `libft2`, `libharfbuzz_ng`, `libharfbuzz_subset`, `libpng`, `libjpeg`, `libwebp_decode`, `libwebp_encode`, `libwuffs` (static) | their `Android.bp` | x64 libjpeg without the NASM SIMD routines (portable C) |
| `libicuuc` (static, with the stub data) | external/icu (`aosp/modules/icu.cmake`, shared with ART's stage) | data from `icudt78l.dat` at run time, mapped by libandroidicuinit as on Android |
| `libutils_full`, `libvndksupport` (static) | system/core | RefBase, String8/16, Vector, Looper, Threads, Trace |
| `libminikin` (static) | frameworks/minikin | Rust hyphenator replaced by the C++ one (patch, ADR 0045) |
| HWUI | frameworks/base/libs/hwui | see [HWUI](#hwui) |

The Skia configuration (`aosp/overlay/skia/include/config/SkUserConfig.h`)
is Android's generated one minus `SK_VULKAN`, AVIF, RAW, Fontations and
Perfetto. EGL and OpenGL ES come from andruna-gles' guest `libEGL.so` and
`libGLESv2.so` (on main since `ba0bfc0`), linked by soname.

## Skia scene test

`tests/skia/skia_scene.cpp` (built by the stage, run by
`tests/skia/run.py`) draws, on one 640x480 surface:

- a linear gradient, a filled star, a dashed circle, a rounded rectangle
  with a blurred shadow, a stroked cubic;
- images that went through Skia's PNG, WebP (lossless) and JPEG encoders and
  decoders, scaled with nearest and linear sampling, and one rotated under
  an anti-aliased rounded clip;
- English, a right-to-left Hebrew paragraph with an English phrase and
  digits inside, and an English paragraph with a Hebrew word, shaped with
  HarfBuzz, bidi and line breaking from ICU, fonts from the Android font
  manager reading `/system/etc/fonts.xml` (Roboto, and Noto Sans Hebrew
  reached through fonts.xml fallback).

Checks in the program: codec round trips (PNG and WebP exact, JPEG within
24), every character has a glyph, Hebrew runs are right to left at an odd
bidi level and use the fallback font, Latin runs inside the RTL paragraph
are at level 2, runs come out in visual order, the surface reads back.
`run.py` then compares the PNG with `tests/skia/reference/scene_<backend>.png`
(a pixel is different when a channel differs by more than 8; at most 0.2%
may differ).

The guest root `run.py` builds is the device layout: fonts in
`/system/fonts`, `fonts.xml` in `/system/etc`, ICU's data in
`/apex/com.android.i18n/etc/icu`, `ANDROID_I18N_ROOT` set.

Results on the reference laptop (x64, `andruna-gles-run`, three runs each):

| Back end | Checks | Image vs reference | First frame | Second frame |
| --- | --- | --- | --- | --- |
| CPU raster | all pass | identical (0 pixels) | 16 to 17 ms | 4.2 to 5.0 ms |
| Ganesh GL, ANGLE on WARP | all pass | identical to the WARP reference | 176 to 202 ms | 11 to 16 ms |
| Ganesh GL, ANGLE on the GPU | all pass | 824 pixels (0.27%) differ from the WARP reference, up to 89 | 126 to 141 ms | 4.9 to 5.0 ms |

GL against raster: 1378 pixels (0.45%) differ, at edges of anti-aliased
paths and glyphs, as between Skia's GPU and CPU back ends on a device.
The first GL frame includes shader compilation (no shader cache yet).

Sizes (x64, release, `-O2 -g`): `libskia.a` 252 MB with debug
information; the stripped `skia_scene` executable, which links the parts
of Skia, HarfBuzz, FreeType, the codecs and ICU it uses, is 8.0 MB
(7.9 MB of text).

The images are the CI artifacts `skia-scene-x64` and `skia-scene-arm64`.

## HWUI

Android's `libhwui_defaults` (android target) has 105 C++ sources plus the
generated `HWUIProperties.sysprop` accessors. **65 compile** and form
`libhwui_core` (static, 17 MB with debug information), built by the stage
and in CI; **40 do not compile yet**, and `libhwui.so` is not linked.

What compiles: the canvas API (`hwui/Canvas`, `SkiaCanvas`, `PaintImpl`),
text through minikin (`Typeface`, `MinikinSkia`, `MinikinUtils`,
`TypefaceUtils`), animators and interpolators, `RenderProperties`,
`DamageAccumulator`, `FrameInfo`, `JankTracker`, `Properties` (with the
generated sysprop accessors and aconfig flags), `PathParser`, `Mesh`,
`ImageDecoder`, `AnimatedImageDrawable`, the stretch effect, the canvas
front end and op buffer, `ReliableSurface`, `PipelineCache`,
`PersistentGraphicsCache`, `SkiaMemoryTracer`, `GLUtils`, `NdkUtils`.

What blocks the rest, by the first header each source misses:

| Missing | Why | Sources |
| --- | --- | --- |
| `aidl/android/hardware/graphics/common/*.h` and `android/hardware/graphics/common/1.x/types.h`, through `<ui/GraphicTypes.h>` | Generated by the AIDL (NDK backend) and HIDL compilers from hardware/interfaces; reached mostly through `renderthread/RenderProxy.h` -> `<gui/SurfaceComposerClient.h>` | 32: `RenderNode`, `RecordingCanvas`, `VectorDrawable`, `TreeInfo`, the Skia display list and recording canvas, all pipelines including `SkiaOpenGLPipeline`, `RenderThread`, `CanvasContext`, `EglManager`, `CacheManager`, `RenderProxy`, `DrawFrameTask`, `GainmapRenderer`, `Readback`, `HardwareBitmapUploader`, ... |
| `android/gui/*.h` | libgui's own AIDL (cpp backend) | `SkiaIpcPipeline` |
| `hardware/hardware.h` | hardware/libhardware headers (not fetched) | `hwui/Bitmap` |
| `android/ipcrenderbuffer/*` | frameworks/native ipcrenderbuffer (not built) | `OutOfProcessRendering` |
| `statslog_hwui.h`, `stats_pull_atom_callback.h` | Generated by stats-log-api-gen; statsd client libraries | `StatsUtils`, `DeferredLayerUpdater`, `GraphicsStatsService` |
| `tonemap/tonemap.h` | frameworks/native libtonemap (not built) | `LayerDrawable` |
| `openssl/sha.h` | BoringSSL | `ShaderCache` |

A link of a test program against `libhwui_core` (SkiaCanvas and minikin
text on a bitmap) was tried: besides the blocked sources (`Bitmap`,
`VectorDrawable`, `GainmapRenderer`) it needs `android::ColorSpace` (libui's
`libui-types`) and `SkTypeface_Make_Fontations` (referenced by
`TypefaceUtils.cpp` for the Fontations back end, which Andruna's Skia
leaves out; it needs a small `ANDRUNA` patch or Fontations). That test is
therefore not part of this round.

Flags: the aconfig flag headers of hwui, libgui, surfaceflinger, text and
view accessibility are generated with every flag at its declared default
(disabled), as for ART. On a device some of them are enabled by the
release configuration; behavior gated on them is the older behavior here.

## Fonts

ADR 0046 (proposed): Roboto from AOSP ships (Apache-2.0); AOSP's Noto
fonts (OFL-1.1) need an amendment to ADR 0006 for font files; until then
`aosp/fonts/windows_fonts.py` writes a `fonts.xml` for the installed
Windows fonts (Roboto as `sans-serif`, one fallback family per script) for
the Windows font directory mounted at `/system/fonts`. `run.py
--windows-fonts` runs the scene that way (locally only: the image depends
on the Windows build, so it is not compared).

| Script | Windows font (regular, bold) |
| --- | --- |
| Arabic, Hebrew, Armenian, Georgian | Segoe UI (`segoeui.ttf`, `segoeuib.ttf`); Hebrew also Arial |
| Thai, Khmer, Lao | Leelawadee UI |
| Indic scripts, Sinhala | Nirmala UI (`nirmala.ttc`) |
| Myanmar | Myanmar Text |
| Ethiopic | Ebrima |
| Cherokee | Gadugi |
| Tibetan, Mongolian, Yi, Thaana | Microsoft Himalaya, Mongolian Baiti, Microsoft Yi Baiti, MV Boli |
| Chinese (Simplified, Traditional), Japanese, Korean | Microsoft YaHei, Microsoft JhengHei, Yu Gothic, Malgun Gothic |
| Emoji, symbols, historic scripts | Segoe UI Emoji, Segoe UI Symbol, Segoe UI Historic |

Not done: mounting the Windows font directory as `/system/fonts` in the
app's guest file system (andruna-lx mount table, read-only, plus Roboto),
and generating `fonts.xml` at install or first start.

## Next steps

1. **Generated graphics headers.** The aidl compiler is built as a host
   tool on the framework branch (`aosp/host`, `build_host.py`). With it on
   main: add hardware/interfaces (graphics/common AIDL) to the lock and
   generate the NDK backend headers, and libgui's cpp backend headers.
   The HIDL `types.h` of graphics.common 1.0 to 1.2 need hidl-gen (or a
   small `ANDRUNA` patch to `<ui/GraphicTypes.h>` that drops the HIDL
   aliases, which HWUI does not use).
2. **libbinder** (p2-libbinder) and then **libgui** and **libui**: HWUI
   uses `Surface`/`ANativeWindow`, `SurfaceControl`, transactions and
   `BLASTBufferQueue` through libgui. In Andruna the buffer queue is not
   SurfaceFlinger's: the window surface is andruna-gles' composition swap
   chain on the compositor's DirectComposition surface (ADR 0030, ADR 0032).
   The plan is to keep libgui's API and back `Surface` and
   `BLASTBufferQueue` with that surface (an Andruna implementation of the
   producer side, documented per method), so that `RenderThread`,
   `EglManager` and `SkiaOpenGLPipeline` run unchanged.
3. **RenderThread and `SkiaOpenGLPipeline`.** With 1 and 2, link
   `libhwui.so` (version script `libhwui.map.txt`, Skia whole-archived as
   on Android), then run HWUI's own `hwui_unit_tests` (gtest) on the guest
   and a frame from `RenderProxy` into a compositor surface on WARP.
4. **Surface from the compositor.** `ANativeWindow` from
   andruna-compositor's surface handle in the app process (ADR 0032),
   with vsync from its `VsyncSource` driving `DrawFrameTask` through
   Choreographer.
5. Small items: a patch for `TypefaceUtils.cpp`'s Fontations reference,
   `libui-types` (ColorSpace), hardware/libhardware headers, libtonemap
   and libshaders, BoringSSL for the shader cache keys, statsd stubs or the
   statsd client libraries, x64 libjpeg SIMD (needs an assembler; NASM is
   not used), Vulkan (`libvulkan`, Skia's Vulkan sources) if ever wanted.
6. **Fonts**: the Windows font mount and generated `fonts.xml` (see above),
   and the decision on ADR 0046.
7. **Performance**: a shader cache (ANGLE's blob cache through
   `eglSetBlobCacheFuncsANDROID`, which HWUI's `ShaderCache` uses) for the
   first-frame cost measured above.

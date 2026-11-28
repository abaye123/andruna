// Guest test: renders a Skia scene and writes it as a PNG.
//
//   skia_scene <out.png> [raster|gl] [<Hebrew font>]
//
// The scene has a gradient, paths (filled, stroked, dashed, blurred), images
// that went through Skia's PNG, JPEG and WebP encoders and decoders, and
// text in English and Hebrew shaped with HarfBuzz (SkShaper) with ICU bidi
// (SkUnicode). Fonts come from the Android font manager, which reads
// /system/etc/fonts.xml and loads the files from /system/fonts, as on a
// device; Hebrew reaches its font through fonts.xml fallback.
//
// With `raster` (the default) the scene is drawn with the CPU raster back
// end. With `gl` it is drawn with Ganesh GL into an EGL pbuffer
// (libEGL.so and libGLESv2.so) and read back.
//
// Besides the image, the program checks what it can check without one
// (shaping and bidi results, codec round trips) and exits non-zero when a
// check fails. The Hebrew runs must use a font whose PostScript name
// contains <Hebrew font> (default "Hebrew", as in NotoSansHebrew-Regular of
// the test fonts.xml). tests/skia/run.py compares the image with the reference.

#include <chrono>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "include/codec/SkCodec.h"
#include "include/codec/SkJpegDecoder.h"
#include "include/codec/SkPngDecoder.h"
#include "include/codec/SkWebpDecoder.h"
#include "include/core/SkBitmap.h"
#include "include/core/SkCanvas.h"
#include "include/core/SkColorSpace.h"
#include "include/core/SkData.h"
#include "include/core/SkFont.h"
#include "include/core/SkFontMgr.h"
#include "include/core/SkImage.h"
#include "include/core/SkBlurTypes.h"
#include "include/core/SkMaskFilter.h"
#include "include/core/SkPaint.h"
#include "include/core/SkPathBuilder.h"
#include "include/core/SkRRect.h"
#include "include/core/SkStream.h"
#include "include/core/SkSurface.h"
#include "include/core/SkTextBlob.h"
#include "include/core/SkTypeface.h"
#include "include/effects/SkDashPathEffect.h"
#include "include/effects/SkGradient.h"
#include "include/encode/SkJpegEncoder.h"
#include "include/encode/SkPngEncoder.h"
#include "include/encode/SkWebpEncoder.h"
#include "include/ports/SkFontMgr_android.h"
#include "include/ports/SkFontScanner_FreeType.h"
#include "modules/skshaper/include/SkShaper.h"
#include "modules/skshaper/include/SkShaper_harfbuzz.h"
#include "modules/skshaper/include/SkShaper_skunicode.h"
#include "modules/skunicode/include/SkUnicode_icu.h"

#ifdef SKIA_SCENE_GL
#include <EGL/egl.h>
#include "include/gpu/ganesh/GrDirectContext.h"
#include "include/gpu/ganesh/SkSurfaceGanesh.h"
#include "include/gpu/ganesh/gl/GrGLDirectContext.h"
#include "include/gpu/ganesh/gl/GrGLInterface.h"
#include "include/gpu/ganesh/gl/egl/GrGLMakeEGLInterface.h"
#endif

namespace {

constexpr int kWidth = 640;
constexpr int kHeight = 480;

int failures = 0;
const char* hebrewFamily = "Hebrew";

void check(bool ok, const char* what) {
    std::printf("%s: %s\n", ok ? "ok" : "FAILED", what);
    if (!ok) failures++;
}

// Records what the shaper produced for each run, then hands the run to the
// text blob builder.
class RecordingRunHandler : public SkShaper::RunHandler {
public:
    struct Run {
        uint8_t bidiLevel;
        std::string family;
        std::vector<SkGlyphID> glyphs;
        size_t utf8Begin;
        size_t utf8Size;
    };

    RecordingRunHandler(const char* utf8, SkPoint origin) : fBlob(utf8, origin) {}

    void beginLine() override { fBlob.beginLine(); }
    void runInfo(const RunInfo& info) override { fBlob.runInfo(info); }
    void commitRunInfo() override { fBlob.commitRunInfo(); }
    Buffer runBuffer(const RunInfo& info) override {
        fCurrent = fBlob.runBuffer(info);
        return fCurrent;
    }
    void commitRunBuffer(const RunInfo& info) override {
        Run r;
        r.bidiLevel = info.fBidiLevel;
        // The PostScript name: Android's font manager names fallback
        // families "<n>##fallback".
        SkString name;
        info.fFont.getTypeface()->getPostScriptName(&name);
        r.family = name.c_str();
        r.glyphs.assign(fCurrent.glyphs, fCurrent.glyphs + info.glyphCount);
        r.utf8Begin = info.utf8Range.begin();
        r.utf8Size = info.utf8Range.size();
        fRuns.push_back(r);
        fBlob.commitRunBuffer(info);
    }
    void commitLine() override { fBlob.commitLine(); }

    sk_sp<SkTextBlob> makeBlob() { return fBlob.makeBlob(); }
    const std::vector<Run>& runs() const { return fRuns; }

private:
    SkTextBlobBuilderRunHandler fBlob;
    Buffer fCurrent{};
    std::vector<Run> fRuns;
};

struct Shaped {
    sk_sp<SkTextBlob> blob;
    std::vector<RecordingRunHandler::Run> runs;
};

Shaped shape(const char* utf8, const SkFont& font, sk_sp<SkFontMgr> mgr, sk_sp<SkUnicode> unicode,
             uint8_t paragraphLevel, SkPoint origin) {
    size_t len = std::strlen(utf8);
    // ShapeThenWrap reorders the runs of each line into visual order (bidi).
    auto shaper = SkShapers::HB::ShapeThenWrap(unicode, mgr);
    auto fonts = SkShaper::MakeFontMgrRunIterator(utf8, len, font, mgr);
    auto bidi = SkShapers::unicode::BidiRunIterator(unicode, utf8, len, paragraphLevel);
    auto script = SkShapers::HB::ScriptRunIterator(utf8, len);
    SkShaper::TrivialLanguageRunIterator lang("he-IL", len);
    RecordingRunHandler handler(utf8, origin);
    shaper->shape(utf8, len, *fonts, *bidi, *script, lang, nullptr, 0, 1e6f, &handler);
    return {handler.makeBlob(), handler.runs()};
}

bool isHebrew(const char* s, size_t n) {
    // UTF-8 lead bytes 0xD6/0xD7 cover U+0580..U+05FF.
    for (size_t i = 0; i < n; i++) {
        unsigned char c = s[i];
        if (c == 0xD6 || c == 0xD7) return true;
    }
    return false;
}

void checkShaping(const char* label, const char* utf8, const Shaped& s) {
    bool hebrewRtl = false, latinLtr = false, noNotdef = true, hebrewFont = false;
    for (const auto& r : s.runs) {
        for (SkGlyphID g : r.glyphs) noNotdef &= g != 0;
        bool he = isHebrew(utf8 + r.utf8Begin, r.utf8Size);
        if (he) {
            hebrewRtl |= (r.bidiLevel & 1) == 1;
            hebrewFont |= r.family.find(hebrewFamily) != std::string::npos;
        } else if ((r.bidiLevel & 1) == 0) {
            latinLtr = true;
        }
        std::printf("  run: level %d, %zu glyphs, %s\n", r.bidiLevel, r.glyphs.size(), r.family.c_str());
    }
    std::string what = std::string(label) + ": Hebrew runs are right to left";
    check(hebrewRtl, what.c_str());
    what = std::string(label) + ": Latin runs are left to right";
    check(latinLtr, what.c_str());
    what = std::string(label) + ": Hebrew uses the fonts.xml fallback font";
    check(hebrewFont, what.c_str());
    what = std::string(label) + ": every character has a glyph";
    check(noNotdef, what.c_str());
}

sk_sp<SkImage> checker(int size, SkColor a, SkColor b) {
    SkBitmap bm;
    bm.allocN32Pixels(size, size, true);
    for (int y = 0; y < size; y++) {
        for (int x = 0; x < size; x++) {
            *bm.getAddr32(x, y) = SkPreMultiplyColor(((x / 4 + y / 4) & 1) ? a : b);
        }
    }
    bm.setImmutable();
    return bm.asImage();
}

sk_sp<SkImage> gradientImage(int w, int h) {
    SkBitmap bm;
    bm.allocN32Pixels(w, h, true);
    for (int y = 0; y < h; y++) {
        for (int x = 0; x < w; x++) {
            *bm.getAddr32(x, y) = SkPreMultiplyColor(
                    SkColorSetARGB(255, x * 255 / (w - 1), y * 255 / (h - 1), 160));
        }
    }
    bm.setImmutable();
    return bm.asImage();
}

sk_sp<SkImage> decode(std::unique_ptr<SkCodec> codec, const char* what) {
    if (!codec) {
        check(false, what);
        return nullptr;
    }
    auto [image, result] = codec->getImage();
    check(image && result == SkCodec::kSuccess, what);
    return image;
}

int maxDiff(const SkImage* a, const SkImage* b) {
    SkBitmap ba, bb;
    SkImageInfo info = SkImageInfo::MakeN32Premul(a->width(), a->height());
    ba.allocPixels(info);
    bb.allocPixels(info);
    if (!a->readPixels(nullptr, ba.pixmap(), 0, 0) || !b->readPixels(nullptr, bb.pixmap(), 0, 0)) return 256;
    int worst = 0;
    for (int y = 0; y < a->height(); y++) {
        for (int x = 0; x < a->width(); x++) {
            SkColor ca = ba.getColor(x, y), cb = bb.getColor(x, y);
            for (int s = 0; s < 32; s += 8) {
                int d = std::abs(int((ca >> s) & 255) - int((cb >> s) & 255));
                if (d > worst) worst = d;
            }
        }
    }
    return worst;
}

void drawScene(SkCanvas* c) {
    // Background: a vertical gradient.
    {
        SkPoint pts[2] = {{0, 0}, {0, kHeight}};
        SkColor4f colors[2] = {SkColor4f::FromColor(0xFFF4F7FB), SkColor4f::FromColor(0xFFD9E3F0)};
        SkPaint p;
        p.setShader(SkShaders::LinearGradient(
                pts, SkGradient(SkGradient::Colors(colors, SkTileMode::kClamp), {})));
        c->drawPaint(p);
    }

    // Paths: a filled star, a dashed circle, a blurred rounded rectangle,
    // a stroked cubic.
    {
        SkPathBuilder star;
        const float cx = 90, cy = 110, ro = 60, ri = 26;
        for (int i = 0; i < 10; i++) {
            float a = -SK_ScalarPI / 2 + i * SK_ScalarPI / 5;
            float r = (i & 1) ? ri : ro;
            SkPoint pt = {cx + r * std::cos(a), cy + r * std::sin(a)};
            if (i == 0) star.moveTo(pt); else star.lineTo(pt);
        }
        star.close();
        SkPaint p;
        p.setAntiAlias(true);
        p.setColor(0xFFE8A317);
        c->drawPath(star.detach(), p);

        SkPaint dash;
        dash.setAntiAlias(true);
        dash.setStyle(SkPaint::kStroke_Style);
        dash.setStrokeWidth(6);
        dash.setColor(0xFF2E6BD8);
        const SkScalar intervals[] = {18, 10};
        dash.setPathEffect(SkDashPathEffect::Make(intervals, 0));
        c->drawCircle(240, 110, 55, dash);

        SkPaint shadow;
        shadow.setAntiAlias(true);
        shadow.setColor(0x80000000);
        shadow.setMaskFilter(SkMaskFilter::MakeBlur(kNormal_SkBlurStyle, 6));
        c->drawRRect(SkRRect::MakeRectXY(SkRect::MakeXYWH(338, 68, 120, 84), 16, 16), shadow);
        SkPaint card;
        card.setAntiAlias(true);
        card.setColor(0xFF3BA55C);
        c->drawRRect(SkRRect::MakeRectXY(SkRect::MakeXYWH(330, 60, 120, 84), 16, 16), card);

        SkPathBuilder wave;
        wave.moveTo(480, 150);
        wave.cubicTo(520, 40, 560, 200, 610, 70);
        SkPaint stroke;
        stroke.setAntiAlias(true);
        stroke.setStyle(SkPaint::kStroke_Style);
        stroke.setStrokeWidth(5);
        stroke.setStrokeCap(SkPaint::kRound_Cap);
        stroke.setColor(0xFFC2185B);
        c->drawPath(wave.detach(), stroke);
    }

    // Images through the codecs: PNG (lossless), WebP (lossless), JPEG.
    {
        sk_sp<SkImage> check8 = checker(32, 0xFF1F2937, 0xFFF59E0B);
        sk_sp<SkImage> grad = gradientImage(64, 48);
        SkPixmap pm;
        SkBitmap bm;
        bm.allocN32Pixels(32, 32);
        check8->readPixels(nullptr, bm.pixmap(), 0, 0);
        sk_sp<SkData> png = SkPngEncoder::Encode(bm.pixmap(), {});
        check(png && png->size() > 0, "PNG encode");
        sk_sp<SkImage> fromPng =
                png ? decode(SkPngDecoder::Decode(png, nullptr), "PNG decode") : nullptr;
        if (fromPng) check(maxDiff(fromPng.get(), check8.get()) == 0, "PNG round trip is exact");

        SkBitmap gb;
        gb.allocN32Pixels(64, 48);
        grad->readPixels(nullptr, gb.pixmap(), 0, 0);
        SkWebpEncoder::Options wo;
        wo.fCompression = SkWebpEncoder::Compression::kLossless;
        SkDynamicMemoryWStream ws;
        check(SkWebpEncoder::Encode(&ws, gb.pixmap(), wo), "WebP encode");
        sk_sp<SkImage> fromWebp = decode(SkWebpDecoder::Decode(ws.detachAsData(), nullptr), "WebP decode");
        if (fromWebp) check(maxDiff(fromWebp.get(), grad.get()) == 0, "WebP lossless round trip is exact");

        SkJpegEncoder::Options jo;
        jo.fQuality = 92;
        SkDynamicMemoryWStream js;
        check(SkJpegEncoder::Encode(&js, gb.pixmap(), jo), "JPEG encode");
        sk_sp<SkImage> fromJpeg = decode(SkJpegDecoder::Decode(js.detachAsData(), nullptr), "JPEG decode");
        if (fromJpeg) check(maxDiff(fromJpeg.get(), grad.get()) <= 24, "JPEG round trip is close");

        SkSamplingOptions nearest(SkFilterMode::kNearest);
        SkSamplingOptions linear(SkFilterMode::kLinear, SkMipmapMode::kNone);
        if (fromPng) c->drawImageRect(fromPng, SkRect::MakeXYWH(30, 200, 96, 96), nearest);
        if (fromWebp) c->drawImageRect(fromWebp, SkRect::MakeXYWH(150, 200, 128, 96), linear);
        if (fromJpeg) c->drawImageRect(fromJpeg, SkRect::MakeXYWH(300, 200, 128, 96), linear);
        // Rotated, with a rounded clip.
        c->save();
        c->translate(530, 248);
        c->rotate(20);
        c->clipRRect(SkRRect::MakeRectXY(SkRect::MakeXYWH(-50, -40, 100, 80), 14, 14), true);
        if (fromPng) c->drawImageRect(fromPng, SkRect::MakeXYWH(-50, -40, 100, 80), linear);
        c->restore();
    }
}

void drawText(SkCanvas* c) {
    sk_sp<SkFontMgr> mgr = SkFontMgr_New_Android(nullptr, SkFontScanner_Make_FreeType());
    check(mgr && mgr->countFamilies() > 0, "fonts.xml has font families");
    sk_sp<SkTypeface> sans = mgr ? mgr->matchFamilyStyle("sans-serif", SkFontStyle()) : nullptr;
    check(sans != nullptr, "fonts.xml has sans-serif");
    sk_sp<SkUnicode> unicode = SkUnicodes::ICU::Make();
    check(unicode != nullptr, "ICU SkUnicode");
    if (!sans || !unicode) return;

    SkFont font(sans, 26);
    font.setEdging(SkFont::Edging::kAntiAlias);
    font.setSubpixel(true);
    SkPaint ink;
    ink.setAntiAlias(true);
    ink.setColor(0xFF111827);

    const char* english = "Andruna: Skia with HarfBuzz on Android ELF";
    // "Shalom olam! Hello, world 2026" in a right-to-left paragraph.
    const char* hebrew = "\xD7\xA9\xD7\x9C\xD7\x95\xD7\x9D \xD7\xA2\xD7\x95\xD7\x9C\xD7\x9D! Hello, world 2026";
    // "Hebrew" inside an English sentence, left-to-right paragraph.
    const char* mixed = "The word \xD7\xA2\xD7\x91\xD7\xA8\xD7\x99\xD7\xAA means Hebrew";

    Shaped e = shape(english, font, mgr, unicode, 0, {24, 350});
    check(!e.runs.empty() && e.runs[0].bidiLevel == 0, "English: one left-to-right run");
    if (e.blob) c->drawTextBlob(e.blob, 0, 0, ink);

    // Right-aligned like an RTL paragraph: measure the shaped width first.
    Shaped h = shape(hebrew, font, mgr, unicode, 1, {0, 0});
    float width = h.blob ? h.blob->bounds().width() : 0;
    Shaped h2 = shape(hebrew, font, mgr, unicode, 1, {kWidth - 24 - width, 400});
    checkShaping("Hebrew paragraph", hebrew, h2);
    // Visual order, left to right: the Latin run first, the first Hebrew
    // word (logical start) last.
    check(!h2.runs.empty() && h2.runs.front().bidiLevel == 2 && h2.runs.back().utf8Begin == 0,
          "Hebrew paragraph: runs are in visual order");
    if (h2.blob) c->drawTextBlob(h2.blob, 0, 0, ink);

    Shaped m = shape(mixed, font, mgr, unicode, 0, {24, 450});
    checkShaping("English paragraph with Hebrew", mixed, m);
    if (m.blob) c->drawTextBlob(m.blob, 0, 0, ink);
}

bool writePng(const SkPixmap& pm, const char* path) {
    SkFILEWStream out(path);
    if (!out.isValid()) return false;
    SkPngEncoder::Options o;
    o.fZLibLevel = 9;
    return SkPngEncoder::Encode(&out, pm, o);
}

#ifdef SKIA_SCENE_GL
sk_sp<SkSurface> makeGlSurface(sk_sp<GrDirectContext>* ctx) {
    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (dpy == EGL_NO_DISPLAY || !eglInitialize(dpy, nullptr, nullptr)) return nullptr;
    const EGLint cfgAttrs[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
                               EGL_OPENGL_ES2_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                               EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_STENCIL_SIZE, 8,
                               EGL_NONE};
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(dpy, cfgAttrs, &cfg, 1, &n) || n < 1) return nullptr;
    const EGLint pbAttrs[] = {EGL_WIDTH, kWidth, EGL_HEIGHT, kHeight, EGL_NONE};
    EGLSurface surf = eglCreatePbufferSurface(dpy, cfg, pbAttrs);
    const EGLint ctxAttrs[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
    EGLContext egl = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctxAttrs);
    if (surf == EGL_NO_SURFACE || egl == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, surf, surf, egl)) {
        return nullptr;
    }
    sk_sp<const GrGLInterface> gl = GrGLInterfaces::MakeEGL();
    check(gl != nullptr, "GrGLInterface from EGL");
    if (!gl) return nullptr;
    *ctx = GrDirectContexts::MakeGL(gl);
    check(*ctx != nullptr, "Ganesh GL context");
    if (!*ctx) return nullptr;
    // Render into a texture-backed surface Skia owns, without MSAA (HWUI
    // does not use MSAA by default either).
    return SkSurfaces::RenderTarget(ctx->get(), skgpu::Budgeted::kNo,
                                    SkImageInfo::MakeN32Premul(kWidth, kHeight), 1, nullptr);
}
#endif

}  // namespace

int main(int argc, char** argv) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: %s <out.png> [raster|gl] [<Hebrew font>]\n", argv[0]);
        return 2;
    }
    if (argc > 3) hebrewFamily = argv[3];
    bool gl = argc > 2 && std::strcmp(argv[2], "gl") == 0;
    sk_sp<SkSurface> surface;
#ifdef SKIA_SCENE_GL
    sk_sp<GrDirectContext> ctx;
    if (gl) {
        surface = makeGlSurface(&ctx);
        check(surface != nullptr, "GL surface");
        if (!surface) return 1;
    }
#else
    if (gl) {
        std::fprintf(stderr, "built without GL\n");
        return 2;
    }
#endif
    if (!surface) surface = SkSurfaces::Raster(SkImageInfo::MakeN32Premul(kWidth, kHeight));
    check(surface != nullptr, "surface");
    if (!surface) return 1;

    SkCanvas* c = surface->getCanvas();
    auto t0 = std::chrono::steady_clock::now();
    drawScene(c);
    drawText(c);
#ifdef SKIA_SCENE_GL
    if (ctx) ctx->flushAndSubmit(GrSyncCpu::kYes);
#endif
    auto t1 = std::chrono::steady_clock::now();
    // A second frame of the same scene: fonts, shaders and codecs are warm.
    drawScene(c);
    drawText(c);
#ifdef SKIA_SCENE_GL
    if (ctx) ctx->flushAndSubmit(GrSyncCpu::kYes);
#endif
    auto t2 = std::chrono::steady_clock::now();
    auto ms = [](auto d) { return std::chrono::duration<double, std::milli>(d).count(); };
    std::printf("time: first frame %.1f ms, second frame %.1f ms\n", ms(t1 - t0), ms(t2 - t1));

    SkBitmap out;
    out.allocPixels(SkImageInfo::Make(kWidth, kHeight, kRGBA_8888_SkColorType, kPremul_SkAlphaType));
    check(surface->readPixels(out.pixmap(), 0, 0), "read back the surface");
    check(writePng(out.pixmap(), argv[1]), "write the PNG");
    std::printf("%s: %d failed checks\n", gl ? "gl" : "raster", failures);
    return failures == 0 ? 0 : 1;
}

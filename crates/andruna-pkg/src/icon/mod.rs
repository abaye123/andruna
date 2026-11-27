//! The launcher icon, rendered for Windows.
//!
//! The manifest's icon resource is resolved at the highest density
//! (xxxhdpi, `anydpi` first, so adaptive icons win on API 26+). Supported
//! drawables: PNG and WebP bitmaps, vector drawables (paths with solid or
//! gradient fills and strokes, groups with transforms, clip paths),
//! `<adaptive-icon>` (background and foreground layers), `<inset>`,
//! `<layer-list>`, `<bitmap>`, `<shape>` with a solid color, `<selector>`
//! (first item) and color resources. Theme attributes (`?attr`) cannot be
//! resolved outside the app and draw nothing.
//!
//! Every size is rendered from the source, not scaled from one image. An
//! adaptive icon shows the inner 72 of its 108 dp layers, masked to a
//! rounded square (corner radius 22% of the size, close to Windows 11 app
//! tiles and common Android launcher shapes). A legacy icon is drawn as
//! is: it brings its own shape.
//!
//! [`render`] returns a 256 px PNG and an `.ico` with PNG-compressed
//! images at the sizes the Windows shell uses (16 to 256 px).

pub mod path;

use std::io::Cursor;

use tiny_skia::{
    FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pixmap,
    PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

use crate::arsc::{Preferences, ResTable};
use crate::axml::{self, ANDROID_NS, Element, Value, complex_to_float};
use crate::zip::Archive;

/// Icon sizes in the `.ico`, as the Windows shell asks for them at 100% to
/// 250% scaling.
pub const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// Corner radius of the adaptive icon mask, as a fraction of the size.
pub const MASK_CORNER: f32 = 0.22;

/// A rendered icon.
#[derive(Clone, Debug)]
pub struct Icon {
    /// 256 x 256 PNG.
    pub png256: Vec<u8>,
    /// `.ico` with PNG-compressed images at [`ICO_SIZES`].
    pub ico: Vec<u8>,
    /// What was rendered, for logs (for example `adaptive-icon`).
    pub kind: &'static str,
}

#[derive(Clone)]
enum Drawable {
    Color(u32),
    Bitmap(Pixmap),
    Vector(Element),
    Adaptive {
        background: Option<Box<Drawable>>,
        foreground: Option<Box<Drawable>>,
    },
    Inset {
        inner: Box<Drawable>,
        /// Left, top, right, bottom as fractions of the bounds.
        insets: [f32; 4],
    },
    Layers(Vec<Drawable>),
}

impl Drawable {
    fn kind(&self) -> &'static str {
        match self {
            Self::Color(_) => "color",
            Self::Bitmap(_) => "bitmap",
            Self::Vector(_) => "vector",
            Self::Adaptive { .. } => "adaptive-icon",
            Self::Inset { inner, .. } => inner.kind(),
            Self::Layers(_) => "layer-list",
        }
    }
}

/// Looks up an attribute by name in the Android namespace (or without a
/// namespace).
fn attr<'e>(el: &'e Element, name: &str) -> Option<&'e Value> {
    el.attrs
        .iter()
        .find(|a| a.name == name && (a.ns.is_none() || a.ns.as_deref() == Some(ANDROID_NS)))
        .map(|a| &a.value)
}

struct Loader<'a, 'd> {
    apk: &'a Archive<'d>,
    table: Option<&'a ResTable>,
    prefs: Preferences,
}

const MAX_DEPTH: u32 = 12;
/// Largest bitmap decoded (pixels per side).
const MAX_BITMAP: u32 = 4096;

fn premultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

fn decode_bitmap(path: &str, data: &[u8]) -> Option<Pixmap> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        let p = Pixmap::decode_png(data).ok()?;
        (p.width() <= MAX_BITMAP && p.height() <= MAX_BITMAP).then_some(p)
    } else if lower.ends_with(".webp") {
        let mut d = image_webp::WebPDecoder::new(Cursor::new(data)).ok()?;
        let (w, h) = d.dimensions();
        if w == 0 || h == 0 || w > MAX_BITMAP || h > MAX_BITMAP {
            return None;
        }
        let mut buf = vec![0u8; d.output_buffer_size()?];
        d.read_image(&mut buf).ok()?;
        let rgba = if d.has_alpha() {
            buf
        } else {
            buf.chunks_exact(3)
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect()
        };
        let mut rgba = rgba;
        premultiply(&mut rgba);
        Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(w, h)?)
    } else {
        None
    }
}

fn fraction(v: &Value) -> Option<f32> {
    match v {
        Value::Fraction(f) => Some(complex_to_float(*f).0),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

impl Loader<'_, '_> {
    /// Resolves references to a final value.
    fn value(&self, v: &Value) -> Option<Value> {
        match v {
            Value::Reference(id) => Some(self.table?.resolve(*id, &self.prefs)?.value),
            Value::Attribute(_) => None,
            v => Some(v.clone()),
        }
    }

    fn float(&self, el: &Element, name: &str) -> Option<f32> {
        match self.value(attr(el, name)?)? {
            Value::Float(f) => Some(f),
            Value::Int(i) => Some(i as f32),
            Value::Dimension(d) => Some(complex_to_float(d).0),
            Value::String(s) => s.trim().trim_end_matches("dp").parse().ok(),
            _ => None,
        }
    }

    fn drawable_value(&self, v: &Value, depth: u32) -> Option<Drawable> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.value(v)? {
            Value::Color(c) => Some(Drawable::Color(c)),
            Value::String(path) => self.file(&path, depth + 1),
            _ => None,
        }
    }

    fn file(&self, path: &str, depth: u32) -> Option<Drawable> {
        let data = self.apk.read_name(path).ok()??;
        if path.to_ascii_lowercase().ends_with(".xml") {
            let el = axml::parse(&data).ok()?;
            self.element(&el, depth)
        } else {
            decode_bitmap(path, &data).map(Drawable::Bitmap)
        }
    }

    /// A drawable given by an attribute or, failing that, the first child
    /// element.
    fn attr_or_child(&self, el: &Element, name: &str, depth: u32) -> Option<Drawable> {
        if let Some(v) = attr(el, name)
            && let Some(d) = self.drawable_value(v, depth)
        {
            return Some(d);
        }
        el.children.first().and_then(|c| self.element(c, depth + 1))
    }

    fn element(&self, el: &Element, depth: u32) -> Option<Drawable> {
        if depth > MAX_DEPTH {
            return None;
        }
        match el.name.as_str() {
            "vector" => Some(Drawable::Vector(el.clone())),
            "adaptive-icon" => {
                let layer = |n: &str| {
                    el.children_named(n)
                        .next()
                        .and_then(|c| self.attr_or_child(c, "drawable", depth + 1))
                        .map(Box::new)
                };
                Some(Drawable::Adaptive {
                    background: layer("background"),
                    foreground: layer("foreground"),
                })
            }
            "inset" => {
                let inner = self.attr_or_child(el, "drawable", depth + 1)?;
                let all = attr(el, "inset").and_then(fraction).unwrap_or(0.0);
                let side = |n: &str| attr(el, n).and_then(fraction).unwrap_or(all);
                let insets = [
                    side("insetLeft"),
                    side("insetTop"),
                    side("insetRight"),
                    side("insetBottom"),
                ];
                Some(Drawable::Inset {
                    inner: Box::new(inner),
                    insets,
                })
            }
            "bitmap" => attr(el, "src").and_then(|v| self.drawable_value(v, depth + 1)),
            "layer-list" => Some(Drawable::Layers(
                el.children_named("item")
                    .filter_map(|i| self.attr_or_child(i, "drawable", depth + 1))
                    .collect(),
            )),
            "selector" | "level-list" => el
                .children_named("item")
                .next()
                .and_then(|i| self.attr_or_child(i, "drawable", depth + 1)),
            "shape" => {
                let solid = el.children_named("solid").next()?;
                match self.value(attr(solid, "color")?)? {
                    Value::Color(c) => Some(Drawable::Color(c)),
                    _ => None,
                }
            }
            "color" => match self.value(attr(el, "color")?)? {
                Value::Color(c) => Some(Drawable::Color(c)),
                _ => None,
            },
            _ => None,
        }
    }

    /// A fill or stroke shader: a color, or a gradient from a complex color
    /// resource.
    fn shader(&self, v: &Value, alpha: f32, tf: Transform) -> Option<Shader<'static>> {
        match self.value(v)? {
            Value::Color(c) => Some(Shader::SolidColor(color(c, alpha))),
            Value::String(path) if path.ends_with(".xml") => {
                let data = self.apk.read_name(&path).ok()??;
                let el = axml::parse(&data).ok()?;
                let g = if el.name == "gradient" {
                    el
                } else {
                    el.children_named("gradient").next()?.clone()
                };
                self.gradient(&g, alpha, tf)
            }
            _ => None,
        }
    }

    fn color_attr(&self, el: &Element, name: &str) -> Option<u32> {
        match self.value(attr(el, name)?)? {
            Value::Color(c) => Some(c),
            _ => None,
        }
    }

    fn gradient(&self, g: &Element, alpha: f32, tf: Transform) -> Option<Shader<'static>> {
        let mut stops = Vec::new();
        for item in g.children_named("item") {
            let c = self.color_attr(item, "color")?;
            let off = self.float(item, "offset").unwrap_or(0.0);
            stops.push(GradientStop::new(off, color(c, alpha)));
        }
        if stops.is_empty() {
            let start = self.color_attr(g, "startColor")?;
            let end = self.color_attr(g, "endColor")?;
            stops.push(GradientStop::new(0.0, color(start, alpha)));
            if let Some(c) = self.color_attr(g, "centerColor") {
                stops.push(GradientStop::new(0.5, color(c, alpha)));
            }
            stops.push(GradientStop::new(1.0, color(end, alpha)));
        }
        let f = |n: &str| self.float(g, n).unwrap_or(0.0);
        let spread = match attr(g, "tileMode").and_then(Value::as_int) {
            Some(2) => SpreadMode::Repeat,
            Some(3) => SpreadMode::Reflect,
            _ => SpreadMode::Pad,
        };
        match attr(g, "type").and_then(Value::as_int).unwrap_or(0) {
            1 => RadialGradient::new(
                Point::from_xy(f("centerX"), f("centerY")),
                Point::from_xy(f("centerX"), f("centerY")),
                f("gradientRadius").max(0.01),
                stops,
                spread,
                tf,
            ),
            _ => LinearGradient::new(
                Point::from_xy(f("startX"), f("startY")),
                Point::from_xy(f("endX"), f("endY")),
                stops,
                spread,
                tf,
            ),
        }
    }

    fn draw(&self, d: &Drawable, pm: &mut Pixmap, rect: Rect, mask: Option<&Mask>) {
        match d {
            Drawable::Color(c) => {
                let mut paint = Paint::default();
                paint.set_color(color(*c, 1.0));
                pm.fill_rect(rect, &paint, Transform::identity(), mask);
            }
            Drawable::Bitmap(b) => draw_bitmap(pm, b, rect, mask),
            Drawable::Vector(v) => self.draw_vector(v, pm, rect, mask),
            Drawable::Inset { inner, insets } => {
                let (w, h) = (rect.width(), rect.height());
                if let Some(r) = Rect::from_ltrb(
                    rect.left() + insets[0] * w,
                    rect.top() + insets[1] * h,
                    rect.right() - insets[2] * w,
                    rect.bottom() - insets[3] * h,
                ) {
                    self.draw(inner, pm, r, mask);
                }
            }
            Drawable::Layers(ls) => {
                for l in ls {
                    self.draw(l, pm, rect, mask);
                }
            }
            Drawable::Adaptive {
                background,
                foreground,
            } => {
                // The layers are 108 dp; the inner 72 dp fill `rect`.
                let (w, h) = (rect.width(), rect.height());
                let layer =
                    Rect::from_xywh(rect.x() - w / 4.0, rect.y() - h / 4.0, w * 1.5, h * 1.5);
                let shape = rounded_rect(rect, MASK_CORNER * w.min(h));
                let mut m = Mask::new(pm.width(), pm.height()).expect("pixmap size");
                if let Some(shape) = shape {
                    m.fill_path(&shape, FillRule::Winding, true, Transform::identity());
                }
                if let Some(outer) = mask {
                    intersect(&mut m, outer);
                }
                if let Some(layer) = layer {
                    for l in [background, foreground].into_iter().flatten() {
                        self.draw(l, pm, layer, Some(&m));
                    }
                }
            }
        }
    }

    fn draw_vector(&self, v: &Element, pm: &mut Pixmap, rect: Rect, mask: Option<&Mask>) {
        let vw = self.float(v, "viewportWidth").unwrap_or(0.0);
        let vh = self.float(v, "viewportHeight").unwrap_or(0.0);
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }
        let alpha = self.float(v, "alpha").unwrap_or(1.0);
        let base = Transform::from_row(
            rect.width() / vw,
            0.0,
            0.0,
            rect.height() / vh,
            rect.x(),
            rect.y(),
        );
        self.draw_group(v, pm, base, alpha, mask.cloned());
    }

    fn draw_group(
        &self,
        g: &Element,
        pm: &mut Pixmap,
        tf: Transform,
        alpha: f32,
        mut clip: Option<Mask>,
    ) {
        for c in &g.children {
            match c.name.as_str() {
                "group" => {
                    let f = |n: &str, d: f32| self.float(c, n).unwrap_or(d);
                    let (px, py) = (f("pivotX", 0.0), f("pivotY", 0.0));
                    let local = Transform::from_translate(-px, -py)
                        .post_scale(f("scaleX", 1.0), f("scaleY", 1.0))
                        .post_concat(Transform::from_rotate(f("rotation", 0.0)))
                        .post_translate(px + f("translateX", 0.0), py + f("translateY", 0.0));
                    self.draw_group(c, pm, tf.pre_concat(local), alpha, clip.clone());
                }
                "clip-path" => {
                    let Some(Value::String(d)) = attr(c, "pathData").and_then(|v| self.value(v))
                    else {
                        continue;
                    };
                    let Some(p) = path::parse(&d) else { continue };
                    let mut m = Mask::new(pm.width(), pm.height()).expect("pixmap size");
                    m.fill_path(&p, FillRule::Winding, true, tf);
                    if let Some(old) = &clip {
                        intersect(&mut m, old);
                    }
                    clip = Some(m);
                }
                "path" => self.draw_path(c, pm, tf, alpha, clip.as_ref()),
                _ => {}
            }
        }
    }

    fn draw_path(
        &self,
        p: &Element,
        pm: &mut Pixmap,
        tf: Transform,
        alpha: f32,
        clip: Option<&Mask>,
    ) {
        let Some(Value::String(d)) = attr(p, "pathData").and_then(|v| self.value(v)) else {
            return;
        };
        let Some(path) = path::parse(&d) else { return };
        if let Some(fill) = attr(p, "fillColor") {
            let a = alpha * self.float(p, "fillAlpha").unwrap_or(1.0);
            if let Some(shader) = self.shader(fill, a, Transform::identity()) {
                let paint = Paint {
                    shader,
                    anti_alias: true,
                    ..Default::default()
                };
                let rule = match attr(p, "fillType").and_then(Value::as_int) {
                    Some(1) => FillRule::EvenOdd,
                    _ => FillRule::Winding,
                };
                pm.fill_path(&path, &paint, rule, tf, clip);
            }
        }
        if let Some(stroke) = attr(p, "strokeColor") {
            let a = alpha * self.float(p, "strokeAlpha").unwrap_or(1.0);
            let width = self.float(p, "strokeWidth").unwrap_or(0.0);
            if width > 0.0
                && let Some(shader) = self.shader(stroke, a, Transform::identity())
            {
                let paint = Paint {
                    shader,
                    anti_alias: true,
                    ..Default::default()
                };
                let mut s = Stroke {
                    width,
                    ..Default::default()
                };
                s.line_cap = match attr(p, "strokeLineCap").and_then(Value::as_int) {
                    Some(1) => tiny_skia::LineCap::Round,
                    Some(2) => tiny_skia::LineCap::Square,
                    _ => tiny_skia::LineCap::Butt,
                };
                s.line_join = match attr(p, "strokeLineJoin").and_then(Value::as_int) {
                    Some(1) => tiny_skia::LineJoin::Round,
                    Some(2) => tiny_skia::LineJoin::Bevel,
                    _ => tiny_skia::LineJoin::Miter,
                };
                s.miter_limit = self.float(p, "strokeMiterLimit").unwrap_or(4.0);
                pm.stroke_path(&path, &paint, &s, tf, clip);
            }
        }
    }
}

fn color(argb: u32, alpha: f32) -> tiny_skia::Color {
    let a = ((argb >> 24) as f32 / 255.0) * alpha.clamp(0.0, 1.0);
    tiny_skia::Color::from_rgba(
        ((argb >> 16) & 0xff) as f32 / 255.0,
        ((argb >> 8) & 0xff) as f32 / 255.0,
        (argb & 0xff) as f32 / 255.0,
        a.clamp(0.0, 1.0),
    )
    .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

fn intersect(m: &mut Mask, other: &Mask) {
    for (a, b) in m.data_mut().iter_mut().zip(other.data()) {
        *a = ((*a as u16 * *b as u16 + 127) / 255) as u8;
    }
}

fn rounded_rect(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let k = 0.552_284_8 * radius;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + radius, t);
    pb.line_to(rt - radius, t);
    pb.cubic_to(rt - radius + k, t, rt, t + radius - k, rt, t + radius);
    pb.line_to(rt, b - radius);
    pb.cubic_to(rt, b - radius + k, rt - radius + k, b, rt - radius, b);
    pb.line_to(l + radius, b);
    pb.cubic_to(l + radius - k, b, l, b - radius + k, l, b - radius);
    pb.line_to(l, t + radius);
    pb.cubic_to(l, t + radius - k, l + radius - k, t, l + radius, t);
    pb.close();
    pb.finish()
}

/// Draws a bitmap into `rect`, halving it first while it is more than
/// twice the target size (for clean downscaling).
fn draw_bitmap(pm: &mut Pixmap, b: &Pixmap, rect: Rect, mask: Option<&Mask>) {
    let mut src = b.clone();
    while src.width() as f32 > 2.0 * rect.width() && src.width() > 2 && src.height() > 2 {
        let (w, h) = (src.width().div_ceil(2), src.height().div_ceil(2));
        let Some(mut half) = Pixmap::new(w, h) else {
            break;
        };
        half.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::from_scale(
                w as f32 / src.width() as f32,
                h as f32 / src.height() as f32,
            ),
            None,
        );
        src = half;
    }
    let tf = Transform::from_row(
        rect.width() / src.width() as f32,
        0.0,
        0.0,
        rect.height() / src.height() as f32,
        rect.x(),
        rect.y(),
    );
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..Default::default()
    };
    pm.draw_pixmap(0, 0, src.as_ref(), &paint, tf, mask);
}

/// Packs PNG images (size, bytes) into an `.ico` file.
pub fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        let s = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[s, s, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

/// Renders the drawable resource `icon` of the APK. `Err` says why there
/// is no icon (missing resource, unsupported drawable).
pub fn render(apk: &Archive<'_>, table: Option<&ResTable>, icon: u32) -> Result<Icon, String> {
    let loader = Loader {
        apk,
        table,
        prefs: Preferences {
            density: 640,
            ..Preferences::default()
        },
    };
    let d = loader
        .drawable_value(&Value::Reference(icon), 0)
        .ok_or_else(|| {
            format!("icon resource {icon:#010x} cannot be loaded or is not supported")
        })?;
    let mut images = Vec::new();
    let mut png256 = Vec::new();
    for size in ICO_SIZES {
        let mut pm = Pixmap::new(size, size).ok_or("pixmap")?;
        let rect = Rect::from_xywh(0.0, 0.0, size as f32, size as f32).ok_or("rect")?;
        loader.draw(&d, &mut pm, rect, None);
        let png = pm.encode_png().map_err(|e| e.to_string())?;
        if size == 256 {
            png256 = png.clone();
        }
        images.push((size, png));
    }
    if Pixmap::decode_png(&png256).is_ok_and(|p| p.pixels().iter().all(|px| px.alpha() == 0)) {
        return Err("the icon renders fully transparent".into());
    }
    Ok(Icon {
        png256,
        ico: encode_ico(&images),
        kind: d.kind(),
    })
}

#[cfg(test)]
mod tests;

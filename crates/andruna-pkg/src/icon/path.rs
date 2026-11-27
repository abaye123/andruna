//! Path data (`android:pathData`, the SVG path syntax) to tiny-skia paths.

use tiny_skia::{Path, PathBuilder};

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn skip_sep(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b',' | b'\t' | b'\n' | b'\r')
        {
            self.i += 1;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() && c != b'e' && c != b'E' {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn has_number(&mut self) -> bool {
        self.skip_sep();
        matches!(self.s.get(self.i), Some(b'0'..=b'9' | b'-' | b'+' | b'.'))
    }

    /// Reads a number, allowing the compact forms `1.5.5` and `1-2`.
    fn number(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        let s = self.s;
        let mut i = self.i;
        if matches!(s.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        let mut dot = false;
        let mut digits = false;
        while let Some(&c) = s.get(i) {
            if c.is_ascii_digit() {
                digits = true;
                i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !digits {
            return None;
        }
        if matches!(s.get(i), Some(b'e' | b'E')) {
            let mut j = i + 1;
            if matches!(s.get(j), Some(b'-' | b'+')) {
                j += 1;
            }
            if s.get(j).is_some_and(u8::is_ascii_digit) {
                while s.get(j).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                }
                i = j;
            }
        }
        self.i = i;
        std::str::from_utf8(&s[start..i]).ok()?.parse().ok()
    }

    /// Arc flags may be written without separators (`a1 1 0 00 1 1`).
    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

/// Appends an elliptical arc as cubic Béziers (SVG implementation notes,
/// F.6).
#[allow(clippy::too_many_arguments)]
fn arc(
    pb: &mut PathBuilder,
    x0: f32,
    y0: f32,
    mut rx: f32,
    mut ry: f32,
    angle: f32,
    large: bool,
    sweep: bool,
    x: f32,
    y: f32,
) {
    if (x0 - x).abs() < 1e-6 && (y0 - y).abs() < 1e-6 {
        return;
    }
    rx = rx.abs();
    ry = ry.abs();
    if rx < 1e-6 || ry < 1e-6 {
        pb.line_to(x, y);
        return;
    }
    let phi = angle.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let dx = (x0 - x) / 2.0;
    let dy = (y0 - y) / 2.0;
    let x1p = cos_phi * dx + sin_phi * dy;
    let y1p = -sin_phi * dx + cos_phi * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den == 0.0 {
        0.0
    } else {
        (num / den).max(0.0).sqrt()
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cos_phi * cxp - sin_phi * cyp + (x0 + x) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (y0 + y) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    let tau = std::f32::consts::TAU;
    if !sweep && dtheta > 0.0 {
        dtheta -= tau;
    } else if sweep && dtheta < 0.0 {
        dtheta += tau;
    }
    let segments = (dtheta.abs() / (std::f32::consts::FRAC_PI_2) - 1e-3)
        .ceil()
        .max(1.0) as usize;
    let delta = dtheta / segments as f32;
    let t = 4.0 / 3.0 * (delta / 4.0).tan();
    let point = |theta: f32| {
        let (s, c) = theta.sin_cos();
        (
            cx + rx * c * cos_phi - ry * s * sin_phi,
            cy + rx * c * sin_phi + ry * s * cos_phi,
        )
    };
    let deriv = |theta: f32| {
        let (s, c) = theta.sin_cos();
        (
            -rx * s * cos_phi - ry * c * sin_phi,
            -rx * s * sin_phi + ry * c * cos_phi,
        )
    };
    let mut th = theta1;
    for _ in 0..segments {
        let (p1x, p1y) = point(th);
        let (d1x, d1y) = deriv(th);
        let th2 = th + delta;
        let (p2x, p2y) = point(th2);
        let (d2x, d2y) = deriv(th2);
        pb.cubic_to(
            p1x + t * d1x,
            p1y + t * d1y,
            p2x - t * d2x,
            p2y - t * d2y,
            p2x,
            p2y,
        );
        th = th2;
    }
}

/// Parses path data. Malformed data ends the path at the last good
/// command, as Android's parser logs and stops.
pub fn parse(data: &str) -> Option<Path> {
    let mut lx = Lexer {
        s: data.as_bytes(),
        i: 0,
    };
    let mut pb = PathBuilder::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    // The last control point, for S/s and T/t.
    let mut last_ctrl: Option<(u8, f32, f32)> = None;
    let mut cmd = 0u8;
    let mut open = false;
    loop {
        let c = match lx.command() {
            Some(c) => c,
            None if cmd != 0 && lx.has_number() => match cmd {
                b'M' => b'L',
                b'm' => b'l',
                c => c,
            },
            None => break,
        };
        cmd = c;
        let rel = c.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let ok = (|| -> Option<()> {
            match c.to_ascii_uppercase() {
                b'M' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    pb.move_to(x, y);
                    open = true;
                    (cx, cy, sx, sy) = (x, y, x, y);
                    last_ctrl = None;
                }
                b'L' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    if !open {
                        pb.move_to(cx, cy);
                        open = true;
                    }
                    pb.line_to(x, y);
                    (cx, cy) = (x, y);
                    last_ctrl = None;
                }
                b'H' => {
                    let x = lx.number()? + ox;
                    pb.line_to(x, cy);
                    cx = x;
                    last_ctrl = None;
                }
                b'V' => {
                    let y = lx.number()? + oy;
                    pb.line_to(cx, y);
                    cy = y;
                    last_ctrl = None;
                }
                b'C' => {
                    let v = [
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                    ];
                    let (x1, y1, x2, y2, x, y) = (
                        v[0] + ox,
                        v[1] + oy,
                        v[2] + ox,
                        v[3] + oy,
                        v[4] + ox,
                        v[5] + oy,
                    );
                    pb.cubic_to(x1, y1, x2, y2, x, y);
                    last_ctrl = Some((b'C', x2, y2));
                    (cx, cy) = (x, y);
                }
                b'S' => {
                    let v = [lx.number()?, lx.number()?, lx.number()?, lx.number()?];
                    let (x1, y1) = match last_ctrl {
                        Some((b'C', px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        _ => (cx, cy),
                    };
                    let (x2, y2, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    pb.cubic_to(x1, y1, x2, y2, x, y);
                    last_ctrl = Some((b'C', x2, y2));
                    (cx, cy) = (x, y);
                }
                b'Q' => {
                    let v = [lx.number()?, lx.number()?, lx.number()?, lx.number()?];
                    let (x1, y1, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    pb.quad_to(x1, y1, x, y);
                    last_ctrl = Some((b'Q', x1, y1));
                    (cx, cy) = (x, y);
                }
                b'T' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    let (x1, y1) = match last_ctrl {
                        Some((b'Q', px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        _ => (cx, cy),
                    };
                    pb.quad_to(x1, y1, x, y);
                    last_ctrl = Some((b'Q', x1, y1));
                    (cx, cy) = (x, y);
                }
                b'A' => {
                    let (rx, ry, rot) = (lx.number()?, lx.number()?, lx.number()?);
                    let large = lx.flag()?;
                    let sweep = lx.flag()?;
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    arc(&mut pb, cx, cy, rx, ry, rot, large, sweep, x, y);
                    (cx, cy) = (x, y);
                    last_ctrl = None;
                }
                b'Z' => {
                    pb.close();
                    (cx, cy) = (sx, sy);
                    open = false;
                    last_ctrl = None;
                    cmd = 0;
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() {
            break;
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let p = parse("M0,0L10,0 10,10z").unwrap();
        let b = p.bounds();
        assert_eq!((b.width(), b.height()), (10.0, 10.0));
        let p =
            parse("m1.5.5l-1-.5h2v2H0V0c1,1 2,2 3,3s1,1 2,2q1,1 2,2t1,1a2,2 0 1,1 4,0z").unwrap();
        assert!(p.bounds().width() > 0.0);
        // Compact arc flags.
        assert!(parse("M0 0a5 5 0 105 5").is_some());
        // Exponents.
        let p = parse("M0 0L1e1 0L1e1 1e1Z").unwrap();
        assert_eq!(p.bounds().width(), 10.0);
    }

    #[test]
    fn malformed_data_never_panics() {
        for s in [
            "",
            "M",
            "M1",
            "L1 2",
            "Z",
            "M0 0 C1",
            "M0 0 A1 1 0 2 0 1 1",
            "M0 0x",
            "M--1 0",
        ] {
            let _ = parse(s);
        }
    }
}

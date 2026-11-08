//! Markup: pen strokes, highlighter, arrows and text, drawn anti-aliased on the finished picture.
//!
//! Marks live in *frame* coordinates (see [`super`]): `x` and widths are fractions of the
//! straightened frame's width, `y` of its height, so they stay on the same spot of the picture when
//! the crop box moves. Each stroke is first rasterised into one coverage mask (the union of its
//! segments, so a stroke that crosses itself is not darker there) and then blended once.

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};

use super::{bad, in_range, Layout, MAX_MARKS, MAX_POINTS, MAX_TEXT};
use crate::{Error, Frame};

/// The embedded font: a Latin subset of Liberation Sans Bold (assets/FONT-LICENSE.txt).
static FONT: &[u8] = include_bytes!("../../assets/markup-font.ttf");

/// How strongly the highlighter covers what is under it.
const HIGHLIGHTER_ALPHA: f32 = 0.45;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Mark {
    /// A free-hand line.
    Pen {
        color: String,
        width: f32,
        points: Vec<[f32; 2]>,
    },
    /// A wide, see-through line.
    Highlighter {
        color: String,
        width: f32,
        points: Vec<[f32; 2]>,
    },
    /// A line with an arrow head at `to`.
    Arrow {
        color: String,
        width: f32,
        from: [f32; 2],
        to: [f32; 2],
    },
    /// Text with its top left at (`x`, `y`); `size` is the text height as a fraction of the frame
    /// width. `\n` starts a new line.
    Text {
        color: String,
        size: f32,
        x: f32,
        y: f32,
        text: String,
    },
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`.
fn parse_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
            Some([d(0)?, d(1)?, d(2)?, 255])
        }
        6 => Some([pair(0)?, pair(2)?, pair(4)?, 255]),
        8 => Some([pair(0)?, pair(2)?, pair(4)?, pair(6)?]),
        _ => None,
    }
}

fn point_ok(name: &str, p: [f32; 2]) -> Result<(), Error> {
    in_range(name, p[0], -0.5, 1.5)?;
    in_range(name, p[1], -0.5, 1.5)
}

pub(super) fn validate(marks: &[Mark]) -> Result<(), Error> {
    if marks.len() > MAX_MARKS {
        return Err(bad(format!("at most {MAX_MARKS} marks")));
    }
    let mut points = 0usize;
    for m in marks {
        let color = match m {
            Mark::Pen { color, .. }
            | Mark::Highlighter { color, .. }
            | Mark::Arrow { color, .. }
            | Mark::Text { color, .. } => color,
        };
        if parse_color(color).is_none() {
            return Err(bad("a mark colour must be #rgb, #rrggbb or #rrggbbaa"));
        }
        match m {
            Mark::Pen {
                width, points: pts, ..
            }
            | Mark::Highlighter {
                width, points: pts, ..
            } => {
                in_range("width", *width, 0.0005, 0.2)?;
                if pts.is_empty() {
                    return Err(bad("a stroke needs at least one point"));
                }
                points += pts.len();
                for p in pts {
                    point_ok("point", *p)?;
                }
            }
            Mark::Arrow {
                width, from, to, ..
            } => {
                in_range("width", *width, 0.0005, 0.2)?;
                point_ok("from", *from)?;
                point_ok("to", *to)?;
                points += 2;
            }
            Mark::Text {
                size, x, y, text, ..
            } => {
                in_range("size", *size, 0.005, 0.5)?;
                point_ok("text position", [*x, *y])?;
                if text.chars().count() > MAX_TEXT {
                    return Err(bad(format!("text is limited to {MAX_TEXT} characters")));
                }
            }
        }
        if points > MAX_POINTS {
            return Err(bad(format!("at most {MAX_POINTS} points in all")));
        }
    }
    Ok(())
}

/// Frame fractions → output pixels, for one render.
struct Map {
    frame_w: f32,
    frame_h: f32,
    crop_x: f32,
    crop_y: f32,
}

impl Map {
    fn px(&self, p: [f32; 2]) -> (f32, f32) {
        (
            p[0] * self.frame_w - self.crop_x,
            p[1] * self.frame_h - self.crop_y,
        )
    }

    /// A width given as a fraction of the frame width, in pixels.
    fn len(&self, fraction: f32) -> f32 {
        fraction * self.frame_w
    }
}

/// Draw `marks` on `f`, which is the crop of the frame described by `lay`.
pub(super) fn draw(f: &mut Frame, marks: &[Mark], lay: &Layout) {
    let map = Map {
        frame_w: lay.frame_w as f32,
        frame_h: lay.frame_h as f32,
        crop_x: lay.crop_x as f32,
        crop_y: lay.crop_y as f32,
    };
    for m in marks {
        match m {
            Mark::Pen {
                color,
                width,
                points,
            } => {
                let pts: Vec<_> = points.iter().map(|p| map.px(*p)).collect();
                stroke(
                    f,
                    &pts,
                    map.len(*width) / 2.0,
                    parse_color(color).unwrap_or([0, 0, 0, 255]),
                    1.0,
                );
            }
            Mark::Highlighter {
                color,
                width,
                points,
            } => {
                let pts: Vec<_> = points.iter().map(|p| map.px(*p)).collect();
                stroke(
                    f,
                    &pts,
                    map.len(*width) / 2.0,
                    parse_color(color).unwrap_or([0, 0, 0, 255]),
                    HIGHLIGHTER_ALPHA,
                );
            }
            Mark::Arrow {
                color,
                width,
                from,
                to,
            } => {
                arrow(
                    f,
                    map.px(*from),
                    map.px(*to),
                    map.len(*width),
                    parse_color(color).unwrap_or([0, 0, 0, 255]),
                );
            }
            Mark::Text {
                color,
                size,
                x,
                y,
                text,
            } => {
                let (px, py) = map.px([*x, *y]);
                write_text(
                    f,
                    text,
                    px,
                    py,
                    map.len(*size),
                    parse_color(color).unwrap_or([0, 0, 0, 255]),
                );
            }
        }
    }
}

/// An 8-bit coverage mask over a part of the picture.
struct Cover {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    a: Vec<u8>,
}

impl Cover {
    /// A mask for the box (`min`, `max`) in pixels, cut to the picture. `None` when nothing of it shows.
    fn new(min: (f32, f32), max: (f32, f32), pic_w: u32, pic_h: u32) -> Option<Cover> {
        let x0 = (min.0.floor() as i64).clamp(0, pic_w as i64) as i32;
        let y0 = (min.1.floor() as i64).clamp(0, pic_h as i64) as i32;
        let x1 = (max.0.ceil() as i64).clamp(0, pic_w as i64) as i32;
        let y1 = (max.1.ceil() as i64).clamp(0, pic_h as i64) as i32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        Some(Cover {
            x0,
            y0,
            w,
            h,
            a: vec![0; w * h],
        })
    }

    fn put(&mut self, x: i32, y: i32, coverage: f32) {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx as usize >= self.w || ly as usize >= self.h {
            return;
        }
        let v = (coverage.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let slot = &mut self.a[ly as usize * self.w + lx as usize];
        *slot = (*slot).max(v);
    }

    /// Cover the pixels within `r` of the segment `a`–`b` (a round-capped line), anti-aliased.
    fn capsule(&mut self, a: (f32, f32), b: (f32, f32), r: f32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let reach = r + 1.0;
        let (x_min, x_max) = (
            (a.0.min(b.0) - reach).floor() as i32,
            (a.0.max(b.0) + reach).ceil() as i32,
        );
        let (y_min, y_max) = (
            (a.1.min(b.1) - reach).floor() as i32,
            (a.1.max(b.1) + reach).ceil() as i32,
        );
        for y in y_min.max(self.y0)..y_max.min(self.y0 + self.h as i32) {
            for x in x_min.max(self.x0)..x_max.min(self.x0 + self.w as i32) {
                let (px, py) = (x as f32 + 0.5 - a.0, y as f32 + 0.5 - a.1);
                let t = if len2 > 0.0 {
                    ((px * dx + py * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (ex, ey) = (px - t * dx, py - t * dy);
                let d = (ex * ex + ey * ey).sqrt();
                let c = r + 0.5 - d;
                if c > 0.0 {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// Cover a convex polygon (vertices in either order), anti-aliased by edge distance.
    fn convex(&mut self, verts: &[(f32, f32)]) {
        // Orient so that "inside" is on the left of every edge.
        let area: f32 = (0..verts.len())
            .map(|i| {
                let (p, q) = (verts[i], verts[(i + 1) % verts.len()]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum();
        let sign = if area >= 0.0 { 1.0 } else { -1.0 };
        let min = verts
            .iter()
            .fold((f32::MAX, f32::MAX), |m, v| (m.0.min(v.0), m.1.min(v.1)));
        let max = verts
            .iter()
            .fold((f32::MIN, f32::MIN), |m, v| (m.0.max(v.0), m.1.max(v.1)));
        for y in (min.1.floor() as i32 - 1).max(self.y0)
            ..(max.1.ceil() as i32 + 1).min(self.y0 + self.h as i32)
        {
            for x in (min.0.floor() as i32 - 1).max(self.x0)
                ..(max.0.ceil() as i32 + 1).min(self.x0 + self.w as i32)
            {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut inside = f32::MAX;
                for i in 0..verts.len() {
                    let (p, q) = (verts[i], verts[(i + 1) % verts.len()]);
                    let (ex, ey) = (q.0 - p.0, q.1 - p.1);
                    let len = (ex * ex + ey * ey).sqrt().max(1e-6);
                    // Signed distance to the edge line, positive inside.
                    inside = inside.min(sign * (ex * (py - p.1) - ey * (px - p.0)) / len);
                }
                let c = inside + 0.5;
                if c > 0.0 {
                    self.put(x, y, c);
                }
            }
        }
    }
}

/// `src` over the pixel at (`x`, `y`) with weight `a` (0..=1).
fn blend(f: &mut Frame, x: usize, y: usize, color: [u8; 4], a: f32) {
    let i = (y * f.width as usize + x) * 4;
    let a = a * color[3] as f32 / 255.0;
    for (px, ink) in f.rgba[i..i + 3].iter_mut().zip(color) {
        let under = *px as f32;
        *px = (under + (ink as f32 - under) * a + 0.5) as u8;
    }
}

fn paint(f: &mut Frame, cover: &Cover, color: [u8; 4], opacity: f32) {
    for ly in 0..cover.h {
        for lx in 0..cover.w {
            let c = cover.a[ly * cover.w + lx];
            if c > 0 {
                blend(
                    f,
                    cover.x0 as usize + lx,
                    cover.y0 as usize + ly,
                    color,
                    c as f32 / 255.0 * opacity,
                );
            }
        }
    }
}

/// Split `a`–`b` into pieces no longer than `step` so each capsule's box stays small.
fn pieces(
    a: (f32, f32),
    b: (f32, f32),
    step: f32,
) -> impl Iterator<Item = ((f32, f32), (f32, f32))> {
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let n = ((len / step).ceil() as usize).clamp(1, 20_000);
    (0..n).map(move |i| {
        let at = |t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        (at(i as f32 / n as f32), at((i + 1) as f32 / n as f32))
    })
}

fn stroke(f: &mut Frame, pts: &[(f32, f32)], r: f32, color: [u8; 4], opacity: f32) {
    let r = r.max(0.5);
    let min = pts
        .iter()
        .fold((f32::MAX, f32::MAX), |m, p| (m.0.min(p.0), m.1.min(p.1)));
    let max = pts
        .iter()
        .fold((f32::MIN, f32::MIN), |m, p| (m.0.max(p.0), m.1.max(p.1)));
    let Some(mut cover) = Cover::new(
        (min.0 - r - 1.0, min.1 - r - 1.0),
        (max.0 + r + 1.0, max.1 + r + 1.0),
        f.width,
        f.height,
    ) else {
        return;
    };
    let step = (2.0 * r).max(6.0);
    if let [only] = pts {
        cover.capsule(*only, *only, r);
    }
    for pair in pts.windows(2) {
        for (a, b) in pieces(pair[0], pair[1], step) {
            cover.capsule(a, b, r);
        }
    }
    paint(f, &cover, color, opacity);
}

fn arrow(f: &mut Frame, from: (f32, f32), to: (f32, f32), width: f32, color: [u8; 4]) {
    let r = (width / 2.0).max(0.5);
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    let reach = width * 6.0 + 2.0;
    let min = (from.0.min(to.0) - reach, from.1.min(to.1) - reach);
    let max = (from.0.max(to.0) + reach, from.1.max(to.1) + reach);
    let Some(mut cover) = Cover::new(min, max, f.width, f.height) else {
        return;
    };
    if len < 0.5 {
        cover.capsule(from, from, r);
        paint(f, &cover, color, 1.0);
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    // The head is 5 line widths long (at most 60% of the arrow) and 4.4 wide.
    let head = (width * 5.0).max(8.0).min(len * 0.6);
    let half = head * 0.44;
    let base = (to.0 - ux * head, to.1 - uy * head);
    let step = (2.0 * r).max(6.0);
    for (a, b) in pieces(
        from,
        (base.0 + ux * head * 0.3, base.1 + uy * head * 0.3),
        step,
    ) {
        cover.capsule(a, b, r);
    }
    cover.convex(&[
        to,
        (base.0 - uy * half, base.1 + ux * half),
        (base.0 + uy * half, base.1 - ux * half),
    ]);
    paint(f, &cover, color, 1.0);
}

fn write_text(f: &mut Frame, text: &str, x: f32, y: f32, size: f32, color: [u8; 4]) {
    let Ok(font) = FontRef::try_from_slice(FONT) else {
        return;
    };
    let scale = PxScale::from(size.max(1.0));
    let scaled = font.as_scaled(scale);
    let line_height = scaled.height() + scaled.line_gap();
    // A soft dark shadow keeps light text readable on bright areas.
    let shadow = size * 0.05;
    for pass in 0..2 {
        let (ink, ox, alpha) = if pass == 0 {
            ([0, 0, 0, 255], shadow, 0.55)
        } else {
            (color, 0.0, 1.0)
        };
        let mut baseline = y + scaled.ascent();
        for line in text.split('\n') {
            let mut caret = x;
            for ch in line.chars() {
                let mut id = font.glyph_id(ch);
                if id.0 == 0 {
                    id = font.glyph_id('?');
                }
                let glyph = id.with_scale_and_position(scale, point(caret + ox, baseline + ox));
                caret += scaled.h_advance(id);
                if let Some(outline) = font.outline_glyph(glyph) {
                    let b = outline.px_bounds();
                    let (bx, by) = (b.min.x.floor() as i32, b.min.y.floor() as i32);
                    outline.draw(|gx, gy, c| {
                        let (px, py) = (bx + gx as i32, by + gy as i32);
                        if px >= 0
                            && py >= 0
                            && (px as u32) < f.width
                            && (py as u32) < f.height
                            && c > 0.0
                        {
                            blend(f, px as usize, py as usize, ink, c * alpha);
                        }
                    });
                }
            }
            baseline += line_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_color("#f80"), Some([255, 136, 0, 255]));
        assert_eq!(parse_color("#00000080"), Some([0, 0, 0, 128]));
        assert_eq!(parse_color("red"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#ggg"), None);
    }

    #[test]
    fn the_embedded_font_has_the_latin_letters_the_app_needs() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        for ch in "AaZz09 .,!?éèàùìòøæåÅÆØßüöäñç€…’".chars() {
            assert!(font.glyph_id(ch).0 != 0 || ch == ' ', "no glyph for {ch}");
        }
    }
}

//! Non-destructive photo edits: a small, versioned [`EditList`] and a pure, deterministic
//! [`render`] that turns a decoded picture and an edit list into the edited picture.
//!
//! The same Rust runs everywhere: the web editor previews it through `secretary-image-wasm`
//! (a downscaled copy, live while a slider moves) and the media server renders the full
//! resolution derivative with it, so what you see while editing is what gets saved. The original
//! is never touched; the edit list is stored next to the item and "revert" just deletes it.
//!
//! # The edit list (JSON, `v: 1`)
//!
//! ```json
//! { "v": 1,
//!   "rotate": 0,                       // quarter turns clockwise, 0..=3
//!   "straighten": 0.0,                 // degrees, -45..=45, positive turns the picture clockwise
//!   "crop": { "x": 0, "y": 0, "w": 1, "h": 1 },   // fractions of the straightened frame; null = all
//!   "adjust": { "exposure": 0, "contrast": 0, "highlights": 0, "shadows": 0, "saturation": 0,
//!               "warmth": 0, "tint": 0, "vignette": 0, "fade": 0 },
//!   "filter": { "name": "vivid", "strength": 1.0 },   // null = none
//!   "markup": [ { "t": "pen", "color": "#ff3b30", "width": 0.006, "points": [[0.1, 0.2], [0.3, 0.4]] },
//!               { "t": "highlighter", "color": "#ffeb3b", "width": 0.03, "points": [[0.1, 0.5], [0.6, 0.5]] },
//!               { "t": "arrow", "color": "#ffffff", "width": 0.008, "from": [0.2, 0.2], "to": [0.6, 0.5] },
//!               { "t": "text", "color": "#ffffff", "size": 0.06, "x": 0.1, "y": 0.1, "text": "Hello" } ] }
//! ```
//!
//! Units: `exposure` is in EV (stops, -3..=3); every other adjustment is -100..=100 (`fade` 0..=100,
//! `filter.strength` 0..=1). Markup coordinates and sizes are fractions of the *frame* (below), so a
//! stroke stays on the same spot of the picture when the crop box moves: `x` and widths/sizes are
//! fractions of the frame's width, `y` of its height.
//!
//! # Render order
//!
//! 1. **Quarter turns** (`rotate`), a lossless copy.
//! 2. **Straighten** by `straighten` degrees about the centre, bilinear. The result is cut to the
//!    largest axis-aligned rectangle with the same aspect ratio that fits inside the turned picture,
//!    so no empty corners show. That rectangle is the *frame*: the space crop and markup live in.
//! 3. **Crop** to the `crop` box of the frame (rounded to whole pixels). 1 to 3 together are one
//!    resampling pass, and a plain copy when there is no straightening.
//! 4. **Colour**, per pixel in linear light (sRGB decoded, 8-bit in and out, alpha untouched):
//!    exposure and white balance (`warmth`, `tint`) as per-channel gains, shadows and highlights as a
//!    luminance-masked gain, saturation as a mix with the luminance, vignette as a radial gain
//!    (elliptical, following the picture), then contrast around mid-grey 0.18, fade (lifted blacks)
//!    and the sRGB encode as one tone curve (a table over the linear value's bits, 9 bits of precision). A named `filter` is a preset [`Adjust`] added, scaled by
//!    its strength, to the user's own adjustments before this step.
//! 5. **Markup**, anti-aliased, in the order of the list, on top of the finished colours.
//!
//! Everything is single-threaded and deterministic: the same input gives the same bytes on the
//! same platform. The wasm and native builds can differ by one level in a few pixels (their
//! `powf` differs in the last bit); tests compare them with a tolerance.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::{Error, Frame};

mod markup;
pub use markup::Mark;

/// The only edit list version this build writes and reads.
pub const VERSION: u32 = 1;
/// The most marks one list may hold, and the most points in all of them.
pub const MAX_MARKS: usize = 400;
pub const MAX_POINTS: usize = 40_000;
/// The longest text of one text mark, in characters.
pub const MAX_TEXT: usize = 300;
/// Pictures larger than this many pixels are refused (a decode bomb guard; 200 megapixels).
pub const MAX_PIXELS: u64 = 200_000_000;

fn version() -> u32 {
    VERSION
}

fn one() -> f32 {
    1.0
}

/// The crop box, as fractions of the straightened frame: `x` and `w` of its width, `y` and `h` of
/// its height.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Crop {
    pub const FULL: Crop = Crop {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    fn is_full(&self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.w == 1.0 && self.h == 1.0
    }
}

/// The tone and colour sliders. All zero is "no change".
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Adjust {
    /// Stops of light: +1 doubles the linear values. -3..=3.
    pub exposure: f32,
    pub contrast: f32,
    /// Negative recovers bright areas, positive pushes them up.
    pub highlights: f32,
    /// Positive lifts dark areas, negative deepens them.
    pub shadows: f32,
    pub saturation: f32,
    /// Positive is warmer (more red, less blue).
    pub warmth: f32,
    /// Positive is more magenta, negative more green.
    pub tint: f32,
    /// Positive darkens the edges, negative lightens them.
    pub vignette: f32,
    /// 0..=100: lifts the blacks for a matte look.
    pub fade: f32,
}

impl Adjust {
    fn fields_mut(&mut self) -> [&mut f32; 9] {
        [
            &mut self.exposure,
            &mut self.contrast,
            &mut self.highlights,
            &mut self.shadows,
            &mut self.saturation,
            &mut self.warmth,
            &mut self.tint,
            &mut self.vignette,
            &mut self.fade,
        ]
    }

    fn fields(&self) -> [f32; 9] {
        [
            self.exposure,
            self.contrast,
            self.highlights,
            self.shadows,
            self.saturation,
            self.warmth,
            self.tint,
            self.vignette,
            self.fade,
        ]
    }

    pub fn is_neutral(&self) -> bool {
        self.fields().iter().all(|v| *v == 0.0)
    }

    /// Every field inside its range; `NaN` becomes 0.
    pub fn clamped(mut self) -> Self {
        let ranges: [(f32, f32); 9] = [
            (-3.0, 3.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (-100.0, 100.0),
            (0.0, 100.0),
        ];
        for (f, (lo, hi)) in self.fields_mut().into_iter().zip(ranges) {
            *f = if f.is_nan() { 0.0 } else { f.clamp(lo, hi) };
        }
        self
    }

    /// `self + preset * k`, per field (a filter at strength `k` on top of the user's sliders).
    fn plus(&self, preset: &Adjust, k: f32) -> Adjust {
        let mut out = *self;
        for (o, p) in out.fields_mut().into_iter().zip(preset.fields()) {
            *o += p * k;
        }
        out.clamped()
    }
}

/// A named filter and how strongly it is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterRef {
    pub name: String,
    #[serde(default = "one")]
    pub strength: f32,
}

const A0: Adjust = Adjust {
    exposure: 0.0,
    contrast: 0.0,
    highlights: 0.0,
    shadows: 0.0,
    saturation: 0.0,
    warmth: 0.0,
    tint: 0.0,
    vignette: 0.0,
    fade: 0.0,
};

/// The named filters: presets are just parameter sets, so the editor can show and fine-tune them.
pub const FILTERS: &[(&str, Adjust)] = &[
    (
        "vivid",
        Adjust {
            contrast: 12.0,
            shadows: 8.0,
            saturation: 35.0,
            ..A0
        },
    ),
    (
        "warm",
        Adjust {
            exposure: 0.05,
            saturation: 8.0,
            warmth: 35.0,
            tint: 5.0,
            ..A0
        },
    ),
    (
        "cool",
        Adjust {
            saturation: 4.0,
            warmth: -35.0,
            tint: -4.0,
            ..A0
        },
    ),
    (
        "bw",
        Adjust {
            contrast: 12.0,
            saturation: -100.0,
            ..A0
        },
    ),
    (
        "noir",
        Adjust {
            exposure: -0.15,
            contrast: 45.0,
            shadows: -10.0,
            saturation: -100.0,
            vignette: 35.0,
            ..A0
        },
    ),
    (
        "fade",
        Adjust {
            contrast: -12.0,
            saturation: -15.0,
            warmth: 8.0,
            fade: 40.0,
            ..A0
        },
    ),
    (
        "vintage",
        Adjust {
            contrast: 8.0,
            saturation: -20.0,
            warmth: 28.0,
            tint: 8.0,
            vignette: 25.0,
            fade: 25.0,
            ..A0
        },
    ),
    (
        "dramatic",
        Adjust {
            contrast: 35.0,
            highlights: -35.0,
            shadows: 30.0,
            saturation: 15.0,
            vignette: 20.0,
            ..A0
        },
    ),
];

/// The parameters of a named filter.
pub fn filter_params(name: &str) -> Option<Adjust> {
    FILTERS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// Everything an edit changes, in a form that serialises to the JSON above.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditList {
    #[serde(default = "version")]
    pub v: u32,
    #[serde(default)]
    pub rotate: u8,
    #[serde(default)]
    pub straighten: f32,
    #[serde(default)]
    pub crop: Option<Crop>,
    #[serde(default)]
    pub adjust: Adjust,
    #[serde(default)]
    pub filter: Option<FilterRef>,
    #[serde(default)]
    pub markup: Vec<Mark>,
}

impl Default for EditList {
    fn default() -> Self {
        Self {
            v: VERSION,
            rotate: 0,
            straighten: 0.0,
            crop: None,
            adjust: A0,
            filter: None,
            markup: Vec::new(),
        }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

fn in_range(name: &str, v: f32, lo: f32, hi: f32) -> Result<(), Error> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(bad(format!("{name} must be between {lo} and {hi}")))
    }
}

impl EditList {
    /// Parse and validate a JSON edit list.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let list: EditList =
            serde_json::from_str(json).map_err(|e| bad(format!("bad edit list: {e}")))?;
        list.validate()?;
        Ok(list)
    }

    /// Reject anything the renderer would not draw faithfully: unknown version or filter, values out
    /// of range, a crop that leaves the frame, oversized markup.
    pub fn validate(&self) -> Result<(), Error> {
        if self.v != VERSION {
            return Err(bad(format!("unsupported edit list version {}", self.v)));
        }
        if self.rotate > 3 {
            return Err(bad("rotate must be 0, 1, 2 or 3 quarter turns"));
        }
        in_range("straighten", self.straighten, -45.0, 45.0)?;
        if let Some(c) = &self.crop {
            for (name, v) in [
                ("crop.x", c.x),
                ("crop.y", c.y),
                ("crop.w", c.w),
                ("crop.h", c.h),
            ] {
                in_range(name, v, 0.0, 1.0)?;
            }
            if c.w < 0.005 || c.h < 0.005 || c.x + c.w > 1.0 + 1e-4 || c.y + c.h > 1.0 + 1e-4 {
                return Err(bad("the crop box must lie inside the picture"));
            }
        }
        let a = &self.adjust;
        in_range("exposure", a.exposure, -3.0, 3.0)?;
        for (name, v) in [
            ("contrast", a.contrast),
            ("highlights", a.highlights),
            ("shadows", a.shadows),
            ("saturation", a.saturation),
            ("warmth", a.warmth),
            ("tint", a.tint),
            ("vignette", a.vignette),
        ] {
            in_range(name, v, -100.0, 100.0)?;
        }
        in_range("fade", a.fade, 0.0, 100.0)?;
        if let Some(f) = &self.filter {
            if filter_params(&f.name).is_none() {
                return Err(bad(format!("unknown filter {:?}", f.name)));
            }
            in_range("filter.strength", f.strength, 0.0, 1.0)?;
        }
        markup::validate(&self.markup)
    }

    /// True when rendering would return the picture unchanged.
    pub fn is_identity(&self) -> bool {
        self.rotate == 0
            && self.straighten.abs() < MIN_STRAIGHTEN
            && self.crop.is_none_or(|c| c.is_full())
            && self.effective_adjust().is_neutral()
            && self.markup.is_empty()
    }

    /// The sliders plus the filter at its strength: what the colour step applies.
    pub fn effective_adjust(&self) -> Adjust {
        let base = self.adjust.clamped();
        match &self.filter {
            Some(f) => match filter_params(&f.name) {
                Some(p) => base.plus(&p, f.strength.clamp(0.0, 1.0)),
                None => base,
            },
            None => base,
        }
    }
}

/// Straightening below this many degrees is treated as none (no resampling).
const MIN_STRAIGHTEN: f32 = 0.001;

/// Where the pixels of the output come from: the sizes of every geometry stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The picture after the quarter turns.
    pub turned_w: u32,
    pub turned_h: u32,
    /// The straightened frame (crop and markup coordinates are fractions of it).
    pub frame_w: u32,
    pub frame_h: u32,
    /// The crop box in frame pixels, which is also the output's top left.
    pub crop_x: u32,
    pub crop_y: u32,
    pub out_w: u32,
    pub out_h: u32,
}

/// The scale (≤ 1) at which a `w`×`h` picture rotated by `theta` radians still covers a centred
/// rectangle of the same aspect ratio: the largest inscribed rectangle.
fn inscribed_scale(w: f64, h: f64, theta: f64) -> f64 {
    let (s, c) = (theta.abs().sin(), theta.abs().cos());
    (w / (w * c + h * s)).min(h / (w * s + h * c)).min(1.0)
}

/// The sizes the render of a `src_w`×`src_h` picture will have, without rendering it.
pub fn layout(src_w: u32, src_h: u32, edits: &EditList) -> Layout {
    let (turned_w, turned_h) = if edits.rotate % 2 == 1 {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    };
    let straighten = edits.straighten.abs() >= MIN_STRAIGHTEN;
    let (frame_w, frame_h) = if straighten {
        let s = inscribed_scale(
            turned_w as f64,
            turned_h as f64,
            (edits.straighten as f64).to_radians(),
        );
        (
            ((turned_w as f64 * s).round() as u32).max(1),
            ((turned_h as f64 * s).round() as u32).max(1),
        )
    } else {
        (turned_w, turned_h)
    };
    let c = edits.crop.unwrap_or(Crop::FULL);
    let x0 = ((c.x * frame_w as f32).round() as u32).min(frame_w - 1);
    let y0 = ((c.y * frame_h as f32).round() as u32).min(frame_h - 1);
    let x1 = (((c.x + c.w) * frame_w as f32).round() as u32).clamp(x0 + 1, frame_w);
    let y1 = (((c.y + c.h) * frame_h as f32).round() as u32).clamp(y0 + 1, frame_h);
    Layout {
        turned_w,
        turned_h,
        frame_w,
        frame_h,
        crop_x: x0,
        crop_y: y0,
        out_w: x1 - x0,
        out_h: y1 - y0,
    }
}

/// Render `edits` onto `src`. The result is a new picture; `src` is not changed.
pub fn render(src: &Frame, edits: &EditList) -> Result<Frame, Error> {
    edits.validate()?;
    let (w, h) = (src.width as usize, src.height as usize);
    if w == 0 || h == 0 || src.rgba.len() != w * h * 4 {
        return Err(bad("the picture has no pixels or a wrong buffer size"));
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(bad("the picture is too large to edit"));
    }
    let lay = layout(src.width, src.height, edits);
    let mut out = geometry(src, edits, &lay);
    let adjust = edits.effective_adjust();
    if !adjust.is_neutral() {
        colour(&mut out, &adjust);
    }
    if !edits.markup.is_empty() {
        markup::draw(&mut out, &edits.markup, &lay);
    }
    out.complete = true;
    Ok(out)
}

// ---- geometry ----------------------------------------------------------------------------------

/// Turn clockwise by `q` quarter turns.
fn quarter_turn(f: &Frame, q: u8) -> Frame {
    let (w, h) = (f.width as usize, f.height as usize);
    let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut rgba = vec![0u8; nw * nh * 4];
    for ny in 0..nh {
        for nx in 0..nw {
            // The source pixel that lands on (nx, ny).
            let (sx, sy) = match q % 4 {
                1 => (ny, h - 1 - nx),
                2 => (w - 1 - nx, h - 1 - ny),
                3 => (w - 1 - ny, nx),
                _ => (nx, ny),
            };
            let s = (sy * w + sx) * 4;
            let d = (ny * nw + nx) * 4;
            rgba[d..d + 4].copy_from_slice(&f.rgba[s..s + 4]);
        }
    }
    Frame {
        width: nw as u32,
        height: nh as u32,
        rgba,
        complete: true,
    }
}

fn geometry(src: &Frame, edits: &EditList, lay: &Layout) -> Frame {
    let turned_owned;
    let turned: &Frame = if edits.rotate.is_multiple_of(4) {
        src
    } else {
        turned_owned = quarter_turn(src, edits.rotate);
        &turned_owned
    };
    let (ow, oh) = (lay.out_w as usize, lay.out_h as usize);
    if edits.straighten.abs() < MIN_STRAIGHTEN {
        // No resampling: the frame is the turned picture and the crop is a copy of its rows.
        if lay.out_w == turned.width && lay.out_h == turned.height {
            return turned.clone();
        }
        let tw = turned.width as usize;
        let mut rgba = Vec::with_capacity(ow * oh * 4);
        for y in 0..oh {
            let s = ((lay.crop_y as usize + y) * tw + lay.crop_x as usize) * 4;
            rgba.extend_from_slice(&turned.rgba[s..s + ow * 4]);
        }
        return Frame {
            width: lay.out_w,
            height: lay.out_h,
            rgba,
            complete: true,
        };
    }

    // Straighten + crop in one bilinear pass: each output pixel is traced back through the
    // crop offset and the rotation to a point of the turned picture.
    let theta = (edits.straighten as f64).to_radians();
    let (sin, cos) = (theta.sin() as f32, theta.cos() as f32);
    let (tw, th) = (turned.width as usize, turned.height as usize);
    let (half_fw, half_fh) = (lay.frame_w as f32 / 2.0, lay.frame_h as f32 / 2.0);
    let (half_tw, half_th) = (tw as f32 / 2.0 - 0.5, th as f32 / 2.0 - 0.5);
    let (max_x, max_y) = ((tw - 1) as f32, (th - 1) as f32);
    let mut rgba = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        let fy = (lay.crop_y as usize + oy) as f32 + 0.5 - half_fh;
        // Along the row the source point moves by (cos, -sin) per pixel.
        let fx0 = lay.crop_x as f32 + 0.5 - half_fw;
        let mut px = fx0 * cos + fy * sin + half_tw;
        let mut py = -fx0 * sin + fy * cos + half_th;
        let row = &mut rgba[oy * ow * 4..(oy + 1) * ow * 4];
        for d in row.as_chunks_mut::<4>().0.iter_mut() {
            let x = px.clamp(0.0, max_x);
            let y = py.clamp(0.0, max_y);
            let (xi, yi) = (x as usize, y as usize);
            let (xj, yj) = ((xi + 1).min(tw - 1), (yi + 1).min(th - 1));
            let (tx, ty) = (x - xi as f32, y - yi as f32);
            let p = |ix: usize, iy: usize| &turned.rgba[(iy * tw + ix) * 4..(iy * tw + ix) * 4 + 4];
            let (a, b, c, e) = (p(xi, yi), p(xj, yi), p(xi, yj), p(xj, yj));
            for k in 0..4 {
                let top = a[k] as f32 + (b[k] as f32 - a[k] as f32) * tx;
                let bottom = c[k] as f32 + (e[k] as f32 - c[k] as f32) * tx;
                d[k] = (top + (bottom - top) * ty + 0.5) as u8;
            }
            px += cos;
            py -= sin;
        }
    }
    Frame {
        width: lay.out_w,
        height: lay.out_h,
        rgba,
        complete: true,
    }
}

// ---- colour ------------------------------------------------------------------------------------

pub(crate) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn decode_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Contrast pivots around this linear value (sRGB 0.46, mid grey).
const PIVOT: f32 = 0.18;
/// The tone curve is tabulated over the bit pattern of the linear value: positive floats order like
/// their bits, so `bits >> TONE_SHIFT` is a table index with the same relative precision (2^-9) at every
/// brightness, with no `sqrt` or `powf` per pixel. Values below 2^-14 (which encode to 0 anyway) share entry 0.
const TONE_SHIFT: u32 = 14;
const TONE_FLOOR_BITS: u32 = 113 << 23; // 2^-14
const TONE_LEN: usize = (((127 << 23) - TONE_FLOOR_BITS) >> TONE_SHIFT) as usize + 2;
const VIGNETTE_STEPS: usize = 1024;

#[inline]
fn tone_index(v: f32) -> usize {
    // A NaN fails `v > 0.0`, so a broken value becomes black instead of an index out of range.
    let bits = if v > 0.0 { v.min(1.0) } else { 0.0 }.to_bits();
    if bits <= TONE_FLOOR_BITS {
        0
    } else {
        ((bits - TONE_FLOOR_BITS) >> TONE_SHIFT) as usize + 1
    }
}

/// Linear value → 8-bit sRGB after contrast and fade, indexed by [`tone_index`] (each entry is its bin's centre).
fn tone_table(a: &Adjust) -> Vec<u8> {
    let k = 2f32.powf(0.6 * a.contrast / 100.0);
    let lift = 0.05 * a.fade / 100.0;
    (0..TONE_LEN)
        .map(|i| {
            let mut v = if i == 0 {
                0.0
            } else {
                f32::from_bits(
                    TONE_FLOOR_BITS + (((i - 1) as u32) << TONE_SHIFT) + (1 << (TONE_SHIFT - 1)),
                )
            };
            if k != 1.0 && v > 0.0 {
                v = PIVOT * (v / PIVOT).powf(k);
            }
            if lift > 0.0 {
                v = lift + v * (1.0 - lift);
            }
            (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8
        })
        .collect()
}

fn colour(f: &mut Frame, a: &Adjust) {
    colour_with(f, a, false);
}

/// The colour step. `general` forces the per-pixel path even when per-channel tables would do (tests compare the two).
fn colour_with(f: &mut Frame, a: &Adjust, general: bool) {
    let dec = decode_table();
    // Exposure and white balance: one gain per channel. Warmth moves red and blue in opposite
    // directions, tint moves green; the gains are normalised to keep white's luminance.
    let (warm, tint) = (a.warmth / 100.0, a.tint / 100.0);
    let mut gain = [
        2f32.powf(0.35 * warm),
        2f32.powf(-0.25 * tint),
        2f32.powf(-0.35 * warm),
    ];
    let norm: f32 = gain.iter().zip(LUMA).map(|(g, l)| g * l).sum();
    let ev = 2f32.powf(a.exposure);
    for g in &mut gain {
        *g = *g / norm * ev;
    }
    // The 8-bit input of each channel, decoded and gained.
    let lin: [[f32; 256]; 3] = std::array::from_fn(|k| std::array::from_fn(|v| dec[v] * gain[k]));

    // Shadows and highlights: a gain by perceptual luminance (sqrt of linear), in 256 steps.
    let mask_on = a.shadows != 0.0 || a.highlights != 0.0;
    let mask: Vec<f32> = (0..256)
        .map(|i| {
            let p = i as f32 / 255.0;
            let shadow_w = 1.0 - smoothstep(0.0, 0.65, p);
            let highlight_w = smoothstep(0.35, 1.0, p);
            2f32.powf(a.shadows / 100.0 * 2.0 * shadow_w + a.highlights / 100.0 * highlight_w)
        })
        .collect();

    let sat = 1.0 + a.saturation / 100.0;

    // Vignette: a gain by squared distance from the centre, 0 at the middle, 1 at the corners.
    let vignette_on = a.vignette != 0.0;
    let vig: Vec<f32> = (0..=VIGNETTE_STEPS)
        .map(|i| {
            let rn = (i as f32 / VIGNETTE_STEPS as f32).sqrt();
            let falloff = smoothstep(0.3, 1.0, rn);
            2f32.powf(-a.vignette / 100.0 * 2.0 * falloff * falloff)
        })
        .collect();

    let tone = tone_table(a);

    // Without shadows, highlights, saturation or vignette every channel is its own function of its 8-bit input:
    // three 256-entry tables replace all the arithmetic.
    if !general && !mask_on && sat == 1.0 && !vignette_on {
        let tables: [[u8; 256]; 3] =
            std::array::from_fn(|k| std::array::from_fn(|v| tone[tone_index(lin[k][v])]));
        for px in f.rgba.as_chunks_mut::<4>().0.iter_mut() {
            px[0] = tables[0][px[0] as usize];
            px[1] = tables[1][px[1] as usize];
            px[2] = tables[2][px[2] as usize];
        }
        return;
    }

    let (w, h) = (f.width as usize, f.height as usize);
    let dx2: Vec<f32> = (0..w)
        .map(|x| {
            let d = (x as f32 + 0.5 - w as f32 / 2.0) / (w as f32 / 2.0);
            d * d
        })
        .collect();
    for (y, row) in f.rgba.chunks_exact_mut(w * 4).enumerate() {
        let dy = (y as f32 + 0.5 - h as f32 / 2.0) / (h as f32 / 2.0);
        let dy2 = dy * dy;
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let mut c = [
                lin[0][px[0] as usize],
                lin[1][px[1] as usize],
                lin[2][px[2] as usize],
            ];
            if mask_on {
                let y = c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2];
                let m =
                    mask[(if y > 0.0 { y.min(1.0) } else { 0.0 }.sqrt() * 255.0 + 0.5) as usize];
                c = [c[0] * m, c[1] * m, c[2] * m];
            }
            if sat != 1.0 {
                let y = c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2];
                c = [
                    (y + (c[0] - y) * sat).max(0.0),
                    (y + (c[1] - y) * sat).max(0.0),
                    (y + (c[2] - y) * sat).max(0.0),
                ];
            }
            if vignette_on {
                // Corner distance² is 2; map 0..2 onto the table.
                let g = vig
                    [(((dx2[x] + dy2) * 0.5 * VIGNETTE_STEPS as f32) as usize).min(VIGNETTE_STEPS)];
                c = [c[0] * g, c[1] * g, c[2] * g];
            }
            px[0] = tone[tone_index(c[0])];
            px[1] = tone[tone_index(c[1])];
            px[2] = tone[tone_index(c[2])];
        }
    }
}

// ---- auto enhance ------------------------------------------------------------------------------

/// Per-channel and luma histograms of an 8-bit picture (the input of [`auto_enhance`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    /// Rec. 709 luma of the encoded values.
    pub luma: [u32; 256],
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
            luma: [0; 256],
        }
    }

    /// Count the pixels of `f`, skipping some on large pictures (at most about 400 000 are read).
    pub fn of(f: &Frame) -> Self {
        let mut h = Self::new();
        let n = f.rgba.len() / 4;
        let step = (n / 400_000).max(1);
        for px in f.rgba.as_chunks::<4>().0.iter().step_by(step) {
            h.r[px[0] as usize] += 1;
            h.g[px[1] as usize] += 1;
            h.b[px[2] as usize] += 1;
            let y = px[0] as f32 * LUMA[0] + px[1] as f32 * LUMA[1] + px[2] as f32 * LUMA[2];
            h.luma[(y + 0.5) as usize] += 1;
        }
        h
    }

    fn total(&self) -> u32 {
        self.luma.iter().sum()
    }

    /// The luma (0..=1, encoded) below which `fraction` of the pixels lie.
    fn quantile(&self, fraction: f32) -> f32 {
        let target = (self.total() as f32 * fraction).ceil().max(1.0) as u32;
        let mut seen = 0;
        for (i, &c) in self.luma.iter().enumerate() {
            seen += c;
            if seen >= target {
                return i as f32 / 255.0;
            }
        }
        1.0
    }

    /// The share of pixels with luma in `lo..=hi` (encoded 0..=1).
    fn share(&self, lo: f32, hi: f32) -> f32 {
        let (a, b) = ((lo * 255.0).round() as usize, (hi * 255.0).round() as usize);
        let n: u32 = self.luma[a..=b.min(255)].iter().sum();
        n as f32 / self.total().max(1) as f32
    }

    fn mean(channel: &[u32; 256]) -> f32 {
        let total: u32 = channel.iter().sum();
        if total == 0 {
            return 0.0;
        }
        channel
            .iter()
            .enumerate()
            .map(|(i, &c)| i as f32 * c as f32)
            .sum::<f32>()
            / total as f32
            / 255.0
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Slider values that bring the picture to a pleasant exposure and contrast, from its histogram:
/// the median is moved towards mid grey (damped, and never into clipping), dark and bright
/// shares ask for shadows and highlights, a narrow tonal range for contrast, and a colour cast is
/// corrected a third of the way. An empty histogram gives no change.
pub fn auto_enhance(h: &Histogram) -> Adjust {
    if h.total() == 0 {
        return Adjust::default();
    }
    let mut a = Adjust::default();
    let (p1, p50, p99) = (h.quantile(0.01), h.quantile(0.5), h.quantile(0.99));
    let lin = |v: f32| srgb_to_linear(v.max(1.0 / 255.0));

    // Exposure: towards a median of 0.46 (encoded), without pushing the 99th percentile past 0.98.
    let wanted = (lin(0.46) / lin(p50)).log2() * 0.7;
    let headroom = (lin(0.98) / lin(p99)).log2().max(0.0);
    let ev = if wanted > 0.0 {
        wanted.min(headroom).min(1.5)
    } else {
        wanted.max(-1.5)
    };
    a.exposure = if ev.abs() < 0.08 {
        0.0
    } else {
        (ev * 20.0).round() / 20.0
    };

    let dark = h.share(0.0, 0.18);
    if dark > 0.15 {
        a.shadows = ((dark - 0.15) * 120.0).clamp(0.0, 60.0).round();
    }
    let bright = h.share(0.9, 1.0);
    if bright > 0.04 {
        a.highlights = -((bright - 0.04) * 300.0).clamp(0.0, 60.0).round();
    }
    let spread = p99 - p1;
    if spread < 0.8 {
        a.contrast = ((0.8 - spread) * 100.0).clamp(0.0, 40.0).round();
    }

    let (mr, mg, mb) = (
        Histogram::mean(&h.r).max(1e-3),
        Histogram::mean(&h.g).max(1e-3),
        Histogram::mean(&h.b).max(1e-3),
    );
    let chroma = mr.max(mg).max(mb) - mr.min(mg).min(mb);
    if chroma > 0.03 {
        a.saturation = 12.0;
        // Warmth moves log2(R/B) by 0.7 over its range; tint moves log2(G) by 0.25.
        let cast = (mr / mb).log2();
        a.warmth = (-cast / 0.7 * 100.0 * 0.33).clamp(-25.0, 25.0).round();
        let green = (mg / ((mr + mb) / 2.0)).log2();
        a.tint = (green / 0.25 * 100.0 * 0.25).clamp(-20.0, 20.0).round();
    }
    a.clamped()
}

#[cfg(test)]
mod tests;

//! System screens drawn by Lucina Ink itself: the splash at start-up and the sleep screen (screensaver).
//!
//! A screen is a picture (PNG) fitted to the panel, or a generated "Lucina Ink" text screen when the
//! picture is missing or unreadable. Loading never panics and never fails the caller: every problem is
//! reported in [`ScreenSource::Fallback`].
//!
//! **16 gray levels.** The PW3's i.MX6SL EPDC drives the panel with 4-bit waveforms: GC16 has 16 gray
//! levels and the controller looks up each 8-bit framebuffer pixel by its top 4 bits (`v >> 4`). Truncation
//! would push light grays one level darker (`0xEF` shows as `0xEE`). Screens are therefore quantised in
//! software to the nearest of the 16 levels `0x00, 0x11, …, 0xFF` ([`quantize16`]); those values are exact
//! under the hardware's `v >> 4`, so the panel shows what the emulator shows. Nothing is dithered: the
//! owner's pictures are already black and white.
use crate::font::{Face, Fonts};
use crate::render::{draw_text, fit_text};
use ink_kindle_hal::{GrayImage, Rect, Size};
use std::path::{Path, PathBuf};

/// Where the picture lives on the Kindle (the owner's B/W Lucina picture, 1072×1448 grayscale PNG).
pub const DEVICE_SCREENSAVER: &str = "/mnt/us/screensaver/lucina.png";

/// Largest PNG file read (bytes) and largest decoded image (pixels per side). The panel is 1072×1448.
pub const MAX_FILE_BYTES: u64 = 32 << 20;
pub const MAX_SIDE: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Background {
    /// Letterbox margins (and transparent pixels) are white paper.
    #[default]
    White,
    Black,
}

impl Background {
    pub fn value(self) -> u8 {
        match self {
            Background::White => 0xff,
            Background::Black => 0x00,
        }
    }
}

/// Which system screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    Splash,
    Sleep,
}

impl ScreenKind {
    pub fn name(self) -> &'static str {
        match self {
            ScreenKind::Splash => "splash",
            ScreenKind::Sleep => "sleep",
        }
    }
    fn caption(self) -> &'static str {
        match self {
            ScreenKind::Splash => "Starting\u{2026}",
            ScreenKind::Sleep => "Sleeping",
        }
    }
}

/// Configuration of the screens Lucina Ink owns while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemScreens {
    /// Picture shown at start-up until the first page is ready. `None`: the generated text screen.
    pub splash: Option<PathBuf>,
    /// Pictures for the sleep screen; one per sleep, in order (sleep `n` shows `sleep[n % len]`).
    /// Empty: the generated text screen.
    pub sleep: Vec<PathBuf>,
    pub background: Background,
    /// Draw a small caption ("Sleeping") in the bottom margin. Off by default: the picture alone.
    pub show_text: bool,
}

impl Default for SystemScreens {
    /// The device defaults: the owner's picture at [`DEVICE_SCREENSAVER`] for both screens, white margins.
    fn default() -> Self {
        Self {
            splash: Some(PathBuf::from(DEVICE_SCREENSAVER)),
            sleep: vec![PathBuf::from(DEVICE_SCREENSAVER)],
            background: Background::White,
            show_text: false,
        }
    }
}

impl SystemScreens {
    /// The picture for the `n`-th sleep (0-based), rotating deterministically through `sleep`.
    pub fn sleep_image(&self, n: u64) -> Option<&Path> {
        if self.sleep.is_empty() {
            return None;
        }
        Some(&self.sleep[(n % self.sleep.len() as u64) as usize])
    }
    pub fn image_for(&self, kind: ScreenKind, n: u64) -> Option<&Path> {
        match kind {
            ScreenKind::Splash => self.splash.as_deref(),
            ScreenKind::Sleep => self.sleep_image(n),
        }
    }
}

/// What a rendered screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenSource {
    Image(PathBuf),
    /// The generated text screen; `tried` is the picture that could not be used, if one was configured.
    Fallback {
        tried: Option<PathBuf>,
        reason: String,
    },
}

impl ScreenSource {
    pub fn describe(&self) -> String {
        match self {
            ScreenSource::Image(p) => p.display().to_string(),
            ScreenSource::Fallback {
                tried: Some(p),
                reason,
            } => format!("generated text screen ({}: {reason})", p.display()),
            ScreenSource::Fallback {
                tried: None,
                reason,
            } => {
                format!("generated text screen ({reason})")
            }
        }
    }
}

/// Nearest of the 16 EPDC gray levels (`k * 17`, `k = 0..=15`). See the module docs.
pub fn quantize16(v: u8) -> u8 {
    ((u16::from(v) * 15 + 127) / 255 * 17) as u8
}

pub fn quantize16_image(img: &mut GrayImage) {
    let mut lut = [0u8; 256];
    for (v, slot) in lut.iter_mut().enumerate() {
        *slot = quantize16(v as u8);
    }
    for p in &mut img.data {
        *p = lut[*p as usize];
    }
}

/// Where a `src`-sized picture goes on a `panel`-sized screen, centred with its aspect kept.
///
/// * Larger than the panel (either side): scaled down to fit (letterbox / pillarbox).
/// * Fits already: scaled up by the largest **integer** factor that still fits (1 = unchanged), so pixels
///   stay crisp with nearest-neighbour and no interpolation artefacts appear.
pub fn fit_rect(src: Size, panel: Size) -> Rect {
    if src.w == 0 || src.h == 0 || panel.w == 0 || panel.h == 0 {
        return Rect::default();
    }
    let (w, h) = if src.w <= panel.w && src.h <= panel.h {
        let k = (panel.w / src.w).min(panel.h / src.h).max(1);
        (src.w * k, src.h * k)
    } else {
        let (sw, sh, pw, ph) = (
            u64::from(src.w),
            u64::from(src.h),
            u64::from(panel.w),
            u64::from(panel.h),
        );
        if sw * ph >= sh * pw {
            // relatively wider than the panel: full width, bars top and bottom
            let h = ((sh * pw + sw / 2) / sw).clamp(1, ph);
            (panel.w, h as u32)
        } else {
            let w = ((sw * ph + sh / 2) / sh).clamp(1, pw);
            (w as u32, panel.h)
        }
    };
    Rect::new((panel.w - w) / 2, (panel.h - h) / 2, w, h)
}

/// Source span `[a, b)` covered by destination index `i` of `dst` when mapping `src` onto `dst`.
fn span(i: u32, src: u32, dst: u32) -> (usize, usize) {
    let a = u64::from(i) * u64::from(src) / u64::from(dst);
    let b = (u64::from(i + 1) * u64::from(src) / u64::from(dst)).max(a + 1);
    (a as usize, b.min(u64::from(src)) as usize)
}

/// Resize to `dst`: box filter (area average) when shrinking, nearest-neighbour when growing.
pub fn resize(src: &GrayImage, dst: Size) -> GrayImage {
    let mut out = GrayImage::new(dst, 0xff);
    if dst.w == 0 || dst.h == 0 || src.size.w == 0 || src.size.h == 0 {
        return out;
    }
    let xs: Vec<(usize, usize)> = (0..dst.w).map(|x| span(x, src.size.w, dst.w)).collect();
    let sw = src.size.w as usize;
    for y in 0..dst.h {
        let (y0, y1) = span(y, src.size.h, dst.h);
        let row = &mut out.data[(y * dst.w) as usize..((y + 1) * dst.w) as usize];
        for (o, &(x0, x1)) in row.iter_mut().zip(&xs) {
            let mut sum = 0u64;
            for sy in y0..y1 {
                sum += src.data[sy * sw + x0..sy * sw + x1]
                    .iter()
                    .map(|&v| u64::from(v))
                    .sum::<u64>();
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            *o = ((sum + n / 2) / n) as u8;
        }
    }
    out
}

/// Decode a PNG (any colour type and bit depth) to 8-bit luminance. RGB uses Rec. 601 weights
/// (`0.299 R + 0.587 G + 0.114 B`); alpha is composited over `bg`.
pub fn decode_png(bytes: &[u8], bg: u8) -> Result<GrayImage, String> {
    let limits = png::Limits {
        bytes: (MAX_SIDE as usize) * (MAX_SIDE as usize) * 4,
    };
    let mut dec = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), limits);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("not a readable PNG: {e}"))?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("unsupported size {w}x{h} (max {MAX_SIDE})"));
    }
    let len = reader
        .output_buffer_size()
        .ok_or("image too large for memory")?;
    let mut buf = vec![0u8; len];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("corrupt PNG data: {e}"))?;
    let (color, depth) = (frame.color_type, frame.bit_depth);
    if depth != png::BitDepth::Eight {
        return Err(format!("unexpected bit depth {depth:?} after normalising"));
    }
    let stride = frame.line_size;
    let size = Size { w, h };
    let mut out = GrayImage::new(size, bg);
    let bg = u32::from(bg);
    let over = |v: u32, a: u32| ((v * a + bg * (255 - a) + 127) / 255) as u8;
    let luma = |r: u8, g: u8, b: u8| {
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000
    };
    for y in 0..h as usize {
        let line = &buf[y * stride..];
        let dst = &mut out.data[y * w as usize..(y + 1) * w as usize];
        match color {
            png::ColorType::Grayscale => dst.copy_from_slice(&line[..w as usize]),
            png::ColorType::GrayscaleAlpha => {
                for (d, px) in dst.iter_mut().zip(line.as_chunks::<2>().0) {
                    *d = over(u32::from(px[0]), u32::from(px[1]));
                }
            }
            png::ColorType::Rgb => {
                for (d, px) in dst.iter_mut().zip(line.as_chunks::<3>().0) {
                    *d = luma(px[0], px[1], px[2]) as u8;
                }
            }
            png::ColorType::Rgba => {
                for (d, px) in dst.iter_mut().zip(line.as_chunks::<4>().0) {
                    *d = over(luma(px[0], px[1], px[2]), u32::from(px[3]));
                }
            }
            png::ColorType::Indexed => return Err("palette was not expanded".into()),
        }
    }
    Ok(out)
}

/// Read and decode a picture file (PNG only).
pub fn load_picture(path: &Path, bg: u8) -> Result<GrayImage, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file is {} bytes (max {MAX_FILE_BYTES})",
            meta.len()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    decode_png(&bytes, bg)
}

/// Place `pic` on a `panel`-sized screen of `bg` (see [`fit_rect`]).
pub fn compose(pic: &GrayImage, panel: Size, bg: u8) -> GrayImage {
    let r = fit_rect(pic.size, panel);
    let mut out = GrayImage::new(panel, bg);
    if r.is_empty() {
        return out;
    }
    let scaled;
    let src = if r.w == pic.size.w && r.h == pic.size.h {
        pic
    } else {
        scaled = resize(pic, Size { w: r.w, h: r.h });
        &scaled
    };
    for y in 0..r.h {
        let d = ((r.y + y) * panel.w + r.x) as usize;
        out.data[d..d + r.w as usize].copy_from_slice(src.row(y));
    }
    out
}

fn centered(
    img: &mut GrayImage,
    fonts: &mut Fonts,
    face: Face,
    px: u16,
    baseline: i32,
    text: &str,
) {
    let max = img.size.w as f32 * 0.9;
    let t = fit_text(fonts, face, px, text, max);
    let w = fonts.measure(face, px, &t);
    draw_text(
        img,
        fonts,
        face,
        px,
        (img.size.w as f32 - w) / 2.0,
        baseline,
        &t,
    );
}

fn invert(img: &mut GrayImage, r: Rect) {
    let r = r.intersect(&img.size.rect());
    for y in r.y..r.bottom() {
        let s = (y * img.size.w + r.x) as usize;
        for p in &mut img.data[s..s + r.w as usize] {
            *p = 255 - *p;
        }
    }
}

/// The generated screen: "Lucina Ink" and a subtitle, black on white (or inverted on black).
pub fn text_screen(size: Size, fonts: &mut Fonts, kind: ScreenKind, bg: Background) -> GrayImage {
    let mut img = GrayImage::new(size, 0xff);
    let title_px = (size.w / 9).clamp(24, 160) as u16;
    let sub_px = (title_px / 3).max(14);
    let mid = size.h as i32 * 9 / 20;
    centered(&mut img, fonts, Face::Regular, title_px, mid, "Lucina Ink");
    centered(
        &mut img,
        fonts,
        Face::Italic,
        sub_px,
        mid + i32::from(title_px),
        kind.caption(),
    );
    if bg == Background::Black {
        invert(&mut img, size.rect());
    }
    img
}

/// Caption band at the bottom of the screen (only with `show_text`).
fn draw_caption(img: &mut GrayImage, fonts: &mut Fonts, kind: ScreenKind, bg: Background) {
    let px = (img.size.w / 30).clamp(14, 48) as u16;
    let band_h = u32::from(px) * 2;
    if img.size.h <= band_h {
        return;
    }
    let band = Rect::new(0, img.size.h - band_h, img.size.w, band_h);
    img.fill_rect(band, 0xff);
    let baseline = (band.y + band_h * 2 / 3) as i32;
    centered(img, fonts, Face::Italic, px, baseline, kind.caption());
    if bg == Background::Black {
        invert(img, band);
    }
}

/// Render system screen `kind` (the `n`-th sleep for [`ScreenKind::Sleep`]) for a `size` panel, already
/// quantised to 16 levels. Never fails: problems with the picture give the generated text screen.
pub fn render_screen(
    screens: &SystemScreens,
    kind: ScreenKind,
    n: u64,
    size: Size,
    fonts: &mut Fonts,
) -> (GrayImage, ScreenSource) {
    let bg = screens.background.value();
    let path = screens.image_for(kind, n).map(Path::to_path_buf);
    let (mut img, source) = match &path {
        Some(p) => match load_picture(p, bg) {
            Ok(pic) => (compose(&pic, size, bg), ScreenSource::Image(p.clone())),
            Err(reason) => (
                text_screen(size, fonts, kind, screens.background),
                ScreenSource::Fallback {
                    tried: Some(p.clone()),
                    reason,
                },
            ),
        },
        None => (
            text_screen(size, fonts, kind, screens.background),
            ScreenSource::Fallback {
                tried: None,
                reason: "no picture configured".into(),
            },
        ),
    };
    if screens.show_text && matches!(source, ScreenSource::Image(_)) {
        draw_caption(&mut img, fonts, kind, screens.background);
    }
    quantize16_image(&mut img);
    (img, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sz(w: u32, h: u32) -> Size {
        Size { w, h }
    }

    #[test]
    fn quantize_hits_the_16_levels_and_survives_the_epdc_truncation() {
        let mut levels: Vec<u8> = (0..=255u8).map(quantize16).collect();
        levels.dedup();
        assert_eq!(levels, (0..16).map(|k| k * 17).collect::<Vec<u8>>());
        for k in 0..16u8 {
            assert_eq!(quantize16(k * 17), k * 17, "levels are fixed points");
            assert_eq!((k * 17) >> 4, k, "the EPDC's v >> 4 gives the same level");
        }
        // nearest level, midpoints split evenly
        assert_eq!(quantize16(8), 0);
        assert_eq!(quantize16(9), 17);
        assert_eq!(quantize16(0xef), 0xee);
        assert_eq!(quantize16(0xf8), 0xff);
    }

    #[test]
    fn fit_rect_letterboxes_without_upscaling_artifacts() {
        let pw3 = sz(1072, 1448);
        assert_eq!(fit_rect(pw3, pw3), Rect::new(0, 0, 1072, 1448), "1:1");
        // twice the panel: exact halving
        assert_eq!(fit_rect(sz(2144, 2896), pw3), Rect::new(0, 0, 1072, 1448));
        // landscape picture: full width, bars top and bottom
        assert_eq!(fit_rect(sz(2000, 1000), pw3), Rect::new(0, 456, 1072, 536));
        // tall picture: full height, bars left and right
        assert_eq!(fit_rect(sz(1000, 2896), pw3), Rect::new(286, 0, 500, 1448));
        // small picture: integer nearest-neighbour upscale only (100x100 -> 1000x1000 = 10x)
        assert_eq!(fit_rect(sz(100, 100), pw3), Rect::new(36, 224, 1000, 1000));
        // a picture that already fits but cannot double stays 1:1, centred
        assert_eq!(fit_rect(sz(800, 1000), pw3), Rect::new(136, 224, 800, 1000));
        assert_eq!(fit_rect(sz(0, 10), pw3), Rect::default());
        // aspect is kept within a pixel for many shapes, and the rect is always inside the panel
        for (w, h) in [
            (3000, 7),
            (7, 3000),
            (1073, 1448),
            (1072, 1449),
            (5000, 5000),
        ] {
            let r = fit_rect(sz(w, h), pw3);
            assert!(
                pw3.rect().contains_rect(&r) && !r.is_empty(),
                "{w}x{h} -> {r:?}"
            );
            let lhs = u64::from(r.w) * u64::from(h);
            let rhs = u64::from(r.h) * u64::from(w);
            assert!(lhs.abs_diff(rhs) <= u64::from(w.max(h)), "{w}x{h} -> {r:?}");
            assert!(r.w == pw3.w || r.h == pw3.h, "touches two edges");
        }
    }

    #[test]
    fn box_filter_averages_and_nearest_upscale_is_exact() {
        let mut src = GrayImage::new(sz(4, 2), 0);
        // left 2x2 block black, right 2x2 block white
        for y in 0..2 {
            src.set(2, y, 255);
            src.set(3, y, 255);
        }
        let half = resize(&src, sz(2, 1));
        assert_eq!(half.data, vec![0, 255]);
        let mut checker = GrayImage::new(sz(2, 2), 0);
        checker.set(1, 0, 255);
        checker.set(0, 1, 255);
        assert_eq!(resize(&checker, sz(1, 1)).data, vec![128], "mean, rounded");
        let up = resize(&checker, sz(4, 4));
        assert_eq!(up.get(0, 0), 0);
        assert_eq!(up.get(1, 1), 0);
        assert_eq!(up.get(2, 0), 255);
        assert_eq!(up.get(3, 1), 255);
        assert_eq!(up.get(0, 3), 255);
    }

    #[test]
    fn compose_centres_on_the_background() {
        let pic = GrayImage::new(sz(10, 10), 0);
        let out = compose(&pic, sz(30, 50), 0xff);
        let r = fit_rect(pic.size, sz(30, 50));
        assert_eq!(r, Rect::new(0, 10, 30, 30));
        assert_eq!(out.get(15, 5), 0xff, "bar");
        assert_eq!(out.get(15, 25), 0, "picture");
        assert_eq!(out.get(15, 45), 0xff, "bar");
        let black = compose(&pic, sz(30, 50), 0);
        assert!(black.data.iter().all(|&v| v == 0));
    }

    #[test]
    fn sleep_pictures_rotate_deterministically() {
        let s = SystemScreens {
            sleep: vec!["a.png".into(), "b.png".into(), "c.png".into()],
            ..SystemScreens::default()
        };
        let seq: Vec<&str> = (0..7)
            .map(|n| s.sleep_image(n).unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            seq,
            ["a.png", "b.png", "c.png", "a.png", "b.png", "c.png", "a.png"]
        );
        let none = SystemScreens {
            sleep: vec![],
            ..SystemScreens::default()
        };
        assert_eq!(none.sleep_image(3), None);
        let d = SystemScreens::default();
        assert_eq!(d.splash.as_deref(), Some(Path::new(DEVICE_SCREENSAVER)));
        assert_eq!(d.sleep_image(5), Some(Path::new(DEVICE_SCREENSAVER)));
        assert!(!d.show_text);
        assert_eq!(d.background, Background::White);
    }

    #[test]
    fn corrupt_or_odd_png_input_is_an_error_not_a_panic() {
        assert!(decode_png(b"", 255).is_err());
        assert!(decode_png(b"\x89PNG\r\n\x1a\n garbage", 255).is_err());
        assert!(decode_png(&[0u8; 1000], 255).is_err());
    }
}

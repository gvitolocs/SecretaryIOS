//! Thumbnails and posters, on NVMe under `thumbs/<id[0..2]>/<id>/`:
//! - `s.jpg`: short side 360 px, for the grid;
//! - `l.jpg`: long side 1600 px, for the viewer;
//! - `poster.jpg`: a frame of a video (or an audio file's cover art);
//! - `page1.jpg`: the first page of a Drive PDF or converted Office file (`drive.rs`);
//! - `edited.jpg`: the photo editor's full-resolution derivative of an edited photo (`edits.rs`); the
//!   renditions below are made from it instead of the original while it exists;
//! - `base.jpg`: the unedited picture at 1600 px, the editor's working copy of an edited photo;
//! - `preview.jpg`: the rendering the phone sent with the upload, used as the source for formats
//!   we can't decode here (HEIC: nezopt's libheif has no HEVC plugin);
//! - `g.jxl`: the grid tile, 256 px square (centre crop), JPEG XL d2.5 (about 9 KB);
//! - `p.jxl`: the full-screen preview, 1440 px long side, progressive JPEG XL d2.5 (about 75 KB),
//!   which the Rust viewer refines as the bytes arrive (docs/media-library.md, "Renditions");
//! - a ThumbHash (about 18 bytes) kept in the catalog and sent inside the item JSON.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::config::Config;

/// The editor's derivative (`edits.rs`).
pub const EDITED_FILE: &str = "edited.jpg";

pub const SMALL_SHORT_SIDE: u32 = 360;
pub const LARGE_LONG_SIDE: u32 = 1600;

pub fn dir_for(cfg: &Config, item_id: &str) -> PathBuf {
    let shard = item_id.rsplit('_').next().unwrap_or(item_id);
    cfg.thumbs_dir()
        .join(&shard[..2.min(shard.len())])
        .join(item_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbSize {
    Small,
    Large,
}

impl ThumbSize {
    pub fn from_px(px: Option<u32>) -> Self {
        match px {
            Some(p) if p > 512 => ThumbSize::Large,
            _ => ThumbSize::Small,
        }
    }
    pub fn file(self) -> &'static str {
        match self {
            ThumbSize::Small => "s.jpg",
            ThumbSize::Large => "l.jpg",
        }
    }
}

pub const GRID_PX: u32 = 256;
pub const PREVIEW_PX: u32 = 1440;
/// Butteraugli distance: SSIMULACRA2 about 81 at 256 px and 83 at 1440 px on the owner's photos.
pub const JXL_DISTANCE: f32 = 2.5;

/// Outcome stored in `items.thumb_state`.
pub enum ThumbState {
    /// Thumbnails written; the ThumbHash of the picture.
    Ready(Vec<u8>),
    /// Nothing to show (plain files, audio without art, HEIC without a phone preview).
    None,
}

impl ThumbState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThumbState::Ready(_) => "ready",
            ThumbState::None => "none",
        }
    }
}

/// `duration_ms` picks the video frame: 10% in (at most 5 min), past the black opening of films.
pub fn generate(
    cfg: &Config,
    item_id: &str,
    kind: &str,
    original: &Path,
    duration_ms: Option<i64>,
) -> anyhow::Result<ThumbState> {
    let dir = dir_for(cfg, item_id);
    std::fs::create_dir_all(&dir)?;
    let preview = dir.join("preview.jpg");
    let poster = dir.join("poster.jpg");
    let source: Option<PathBuf> = match kind {
        // An edited photo's renditions show the edit (`edits.rs` keeps the derivative here).
        "photo" => {
            let edited = dir.join(EDITED_FILE);
            if edited.is_file() {
                Some(edited)
            } else {
                display_source(&dir, original)
            }
        }
        "video" => {
            if !poster.is_file() {
                video_poster(cfg, original, &poster, duration_ms)?;
            }
            poster
                .is_file()
                .then(|| poster.clone())
                .or_else(|| preview.is_file().then(|| preview.clone()))
        }
        "audio" => {
            if !poster.is_file() {
                let _ = cover_art(cfg, original, &poster);
            }
            poster.is_file().then(|| poster.clone())
        }
        // A Drive document's first page (`drive.rs`: pdftoppm, LibreOffice), when it has one.
        _ => Some(dir.join("page1.jpg")).filter(|p| p.is_file()),
    };
    let Some(src) = source else {
        return Ok(ThumbState::None);
    };
    let img = load_oriented(&src)?;
    write_jpeg(
        &resize_short(&img, SMALL_SHORT_SIDE),
        &dir.join(ThumbSize::Small.file()),
    )?;
    write_jpeg(
        &resize_long(&img, LARGE_LONG_SIDE),
        &dir.join(ThumbSize::Large.file()),
    )?;
    let grid = img.resize_to_fill(GRID_PX, GRID_PX, image::imageops::FilterType::Lanczos3);
    write_atomic(&dir.join("g.jxl"), &encode_jxl(&grid, false)?)?;
    write_atomic(
        &dir.join("p.jxl"),
        &encode_jxl(&resize_long(&img, PREVIEW_PX), true)?,
    )?;
    Ok(ThumbState::Ready(thumb_hash(&img)))
}

/// The file renditions of a photo are made from: the phone's own rendering of it (`preview.jpg`,
/// for formats we can't decode, like HEIC) or the original itself when the `image` crate reads it.
pub fn display_source(dir: &Path, original: &Path) -> Option<PathBuf> {
    let preview = dir.join("preview.jpg");
    if preview.is_file() {
        Some(preview)
    } else if decodable(original) {
        Some(original.to_path_buf())
    } else {
        None
    }
}

fn decodable(p: &Path) -> bool {
    ImageReader::open(p)
        .and_then(|r| r.with_guessed_format())
        .map(|r| r.format().is_some_and(|f| f.reading_enabled()))
        .unwrap_or(false)
}

pub fn load_oriented(p: &Path) -> anyhow::Result<DynamicImage> {
    let mut decoder = ImageReader::open(p)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn resize_short(img: &DynamicImage, short: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let s = w.min(h);
    if s <= short {
        return img.clone();
    }
    let scale = short as f64 / s as f64;
    img.resize_exact(
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    )
}

pub fn resize_long(img: &DynamicImage, long: u32) -> DynamicImage {
    if img.width().max(img.height()) <= long {
        return img.clone();
    }
    img.resize(long, long, image::imageops::FilterType::Triangle)
}

/// ThumbHash of a picture (from a copy no larger than 100 px, as the format wants).
pub fn thumb_hash(img: &DynamicImage) -> Vec<u8> {
    let small = img.thumbnail(100, 100).to_rgba8();
    thumbhash::rgba_to_thumb_hash(
        small.width() as usize,
        small.height() as usize,
        small.as_raw(),
    )
}

/// JPEG XL at [`JXL_DISTANCE`]; `progressive` orders the data so a partial file renders (DC first).
pub fn encode_jxl(img: &DynamicImage, progressive: bool) -> anyhow::Result<Vec<u8>> {
    use jpegxl_rs::encode::EncoderSpeed;
    use jpegxl_sys::encoder::encode::JxlEncoderFrameSettingId as Opt;
    let rgb = img.to_rgb8();
    let mut enc = jpegxl_rs::encoder_builder()
        .quality(JXL_DISTANCE)
        .speed(EncoderSpeed::Squirrel)
        .build()
        .map_err(|e| anyhow::anyhow!("jxl encoder: {e}"))?;
    if progressive {
        enc.set_frame_option(Opt::Responsive, 1);
        enc.set_frame_option(Opt::ProgressiveAc, 1);
        enc.set_frame_option(Opt::ProgressiveDc, 1);
    }
    enc.encode::<u8>(rgb.as_raw(), rgb.width(), rgb.height())
        .map_err(|e| anyhow::anyhow!("jxl encode: {e}"))
}

fn write_atomic(dst: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = dst.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

fn write_jpeg(img: &DynamicImage, dst: &Path) -> anyhow::Result<()> {
    let tmp = dst.with_extension("jpg.part");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(BufWriter::new(f), 82);
        enc.encode_image(&img.to_rgb8())?;
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// A frame 10% in (at most 5 min; 1 s when the length is unknown, then the first frame),
/// scaled to 1600 px on the long side.
fn video_poster(
    cfg: &Config,
    src: &Path,
    dst: &Path,
    duration_ms: Option<i64>,
) -> anyhow::Result<()> {
    let tmp = dst.with_extension("part.jpg");
    let at = duration_ms
        .filter(|d| *d > 20_000)
        .map(|d| (d / 10).min(300_000))
        .unwrap_or(1000);
    let first = format!("{}.{:03}", at / 1000, at % 1000);
    for seek in [first.as_str(), "0"] {
        let status = Command::new(&cfg.ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-ss", seek, "-i"])
            .arg(src)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale='min(1600,iw)':-2",
                "-q:v",
                "3",
            ])
            .arg(&tmp)
            .stderr(Stdio::null())
            .status()?;
        if status.success() && tmp.is_file() && std::fs::metadata(&tmp)?.len() > 0 {
            std::fs::rename(&tmp, dst)?;
            return Ok(());
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

fn cover_art(cfg: &Config, src: &Path, dst: &Path) -> anyhow::Result<()> {
    let tmp = dst.with_extension("part.jpg");
    let status = Command::new(&cfg.ffmpeg)
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-an", "-frames:v", "1"])
        .arg(&tmp)
        .stderr(Stdio::null())
        .status()?;
    if status.success() && tmp.is_file() {
        std::fs::rename(&tmp, dst)?;
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_thumbs_keep_aspect() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        let src = tmp.path().join("in.png");
        DynamicImage::new_rgb8(2000, 1000).save(&src).unwrap();
        let ThumbState::Ready(hash) = generate(&cfg, "itm_abcdef", "photo", &src, None).unwrap()
        else {
            panic!("no thumbs")
        };
        assert!((5..=40).contains(&hash.len()), "{}", hash.len());
        let dir = dir_for(&cfg, "itm_abcdef");
        let s = image::image_dimensions(dir.join("s.jpg")).unwrap();
        let l = image::image_dimensions(dir.join("l.jpg")).unwrap();
        assert_eq!(s, (720, 360));
        assert_eq!(l, (1600, 800));
        let g = std::fs::read(dir.join("g.jxl")).unwrap();
        let p = std::fs::read(dir.join("p.jxl")).unwrap();
        assert_eq!(&g[..2], &[0xFF, 0x0A], "bare JPEG XL codestream");
        assert!(!p.is_empty() && p.len() < 200_000);
        let bogus = tmp.path().join("x.heic");
        std::fs::write(&bogus, b"not an image").unwrap();
        assert!(matches!(
            generate(&cfg, "itm_zz", "photo", &bogus, None).unwrap(),
            ThumbState::None
        ));
    }
}

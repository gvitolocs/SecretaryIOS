//! Thumbnails and posters, on NVMe under `thumbs/<id[0..2]>/<id>/`:
//! - `s.jpg`: short side 360 px, for the grid;
//! - `l.jpg`: long side 1600 px, for the viewer;
//! - `poster.jpg`: a frame of a video (or an audio file's cover art);
//! - `preview.jpg`: the rendering the phone sent with the upload, used as the source for formats
//!   we can't decode here (HEIC: nezopt's libheif has no HEVC plugin).

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::config::Config;

pub const SMALL_SHORT_SIDE: u32 = 360;
pub const LARGE_LONG_SIDE: u32 = 1600;

pub fn dir_for(cfg: &Config, item_id: &str) -> PathBuf {
    let shard = item_id.rsplit('_').next().unwrap_or(item_id);
    cfg.thumbs_dir().join(&shard[..2.min(shard.len())]).join(item_id)
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

/// Outcome stored in `items.thumb_state`.
pub enum ThumbState {
    Ready,
    /// Nothing to show (plain files, audio without art, HEIC without a phone preview).
    None,
}

impl ThumbState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThumbState::Ready => "ready",
            ThumbState::None => "none",
        }
    }
}

/// `duration_ms` picks the video frame: 10% in (at most 5 min), past the black opening of films.
pub fn generate(cfg: &Config, item_id: &str, kind: &str, original: &Path, duration_ms: Option<i64>) -> anyhow::Result<ThumbState> {
    let dir = dir_for(cfg, item_id);
    std::fs::create_dir_all(&dir)?;
    let preview = dir.join("preview.jpg");
    let poster = dir.join("poster.jpg");
    let source: Option<PathBuf> = match kind {
        "photo" => {
            if preview.is_file() {
                Some(preview.clone())
            } else if decodable(original) {
                Some(original.to_path_buf())
            } else {
                None
            }
        }
        "video" => {
            if !poster.is_file() {
                video_poster(cfg, original, &poster, duration_ms)?;
            }
            poster.is_file().then(|| poster.clone()).or_else(|| preview.is_file().then(|| preview.clone()))
        }
        "audio" => {
            if !poster.is_file() {
                let _ = cover_art(cfg, original, &poster);
            }
            poster.is_file().then(|| poster.clone())
        }
        _ => None,
    };
    let Some(src) = source else { return Ok(ThumbState::None) };
    let img = load_oriented(&src)?;
    write_jpeg(&resize_short(&img, SMALL_SHORT_SIDE), &dir.join(ThumbSize::Small.file()))?;
    write_jpeg(&resize_long(&img, LARGE_LONG_SIDE), &dir.join(ThumbSize::Large.file()))?;
    Ok(ThumbState::Ready)
}

fn decodable(p: &Path) -> bool {
    ImageReader::open(p)
        .and_then(|r| r.with_guessed_format())
        .map(|r| r.format().is_some_and(|f| f.reading_enabled()))
        .unwrap_or(false)
}

pub fn load_oriented(p: &Path) -> anyhow::Result<DynamicImage> {
    let mut decoder = ImageReader::open(p)?.with_guessed_format()?.into_decoder()?;
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

fn resize_long(img: &DynamicImage, long: u32) -> DynamicImage {
    if img.width().max(img.height()) <= long {
        return img.clone();
    }
    img.resize(long, long, image::imageops::FilterType::Triangle)
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
fn video_poster(cfg: &Config, src: &Path, dst: &Path, duration_ms: Option<i64>) -> anyhow::Result<()> {
    let tmp = dst.with_extension("part.jpg");
    let at = duration_ms.filter(|d| *d > 20_000).map(|d| (d / 10).min(300_000)).unwrap_or(1000);
    let first = format!("{}.{:03}", at / 1000, at % 1000);
    for seek in [first.as_str(), "0"] {
        let status = Command::new(&cfg.ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-ss", seek, "-i"])
            .arg(src)
            .args(["-frames:v", "1", "-vf", "scale='min(1600,iw)':-2", "-q:v", "3"])
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
        assert!(matches!(generate(&cfg, "itm_abcdef", "photo", &src, None).unwrap(), ThumbState::Ready));
        let dir = dir_for(&cfg, "itm_abcdef");
        let s = image::image_dimensions(dir.join("s.jpg")).unwrap();
        let l = image::image_dimensions(dir.join("l.jpg")).unwrap();
        assert_eq!(s, (720, 360));
        assert_eq!(l, (1600, 800));
        let bogus = tmp.path().join("x.heic");
        std::fs::write(&bogus, b"not an image").unwrap();
        assert!(matches!(generate(&cfg, "itm_zz", "photo", &bogus, None).unwrap(), ThumbState::None));
    }
}

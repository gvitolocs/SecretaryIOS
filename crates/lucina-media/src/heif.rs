//! HEIC/HEIF photos that arrive without the phone's JPEG rendering (the background upload
//! extension sends the original only). nezopt's libheif has no HEVC plugin, and ffmpeg 8 decodes an
//! iPhone HEIC's tiles but does not join them, so the tiles are decoded one by one here and
//! stitched into `preview.jpg`, the same file the app's own uploads bring along.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::{DynamicImage, RgbImage};
use serde_json::Value;

use crate::config::Config;

/// Long side of the rendering; the largest rendition made from it is 1600 px.
pub const RENDER_PX: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub stream: usize,
    pub x: u32,
    pub y: u32,
}

/// The picture of a HEIF tile grid: its size and where each tile stream goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<Tile>,
}

/// The first "Tile Grid" stream group of `ffprobe -of json -show_stream_groups`.
pub fn grid_from_probe(probe: &Value) -> Option<Grid> {
    let group = probe
        .get("stream_groups")?
        .as_array()?
        .iter()
        .find(|g| g.get("type").and_then(Value::as_str) == Some("Tile Grid"))?;
    let comp = group.get("components")?.as_array()?.first()?;
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    let width = u32::try_from(num(comp, "width")?).ok()?;
    let height = u32::try_from(num(comp, "height")?).ok()?;
    let tiles: Vec<Tile> = comp
        .get("subcomponents")?
        .as_array()?
        .iter()
        .map(|t| {
            Some(Tile {
                stream: usize::try_from(num(t, "stream_index")?).ok()?,
                x: u32::try_from(num(t, "tile_horizontal_offset")?).ok()?,
                y: u32::try_from(num(t, "tile_vertical_offset")?).ok()?,
            })
        })
        .collect::<Option<_>>()?;
    (width > 0 && height > 0 && !tiles.is_empty()).then_some(Grid {
        width,
        height,
        tiles,
    })
}

/// An ISO-BMFF file whose major brand is a HEIF one (the name is not trusted).
pub fn is_heif(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    let ok = std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok();
    ok && &head[4..8] == b"ftyp"
        && matches!(
            &head[8..12],
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
        )
}

/// Write a JPEG rendering of `src` (oriented, at most [`RENDER_PX`] long) to `dst`.
/// Blocking: runs ffprobe and ffmpeg.
pub fn render_preview(cfg: &Config, src: &Path, dst: &Path) -> anyhow::Result<()> {
    let out = Command::new(&cfg.ffprobe)
        .args(["-v", "error", "-of", "json", "-show_stream_groups"])
        .arg(src)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    anyhow::ensure!(out.status.success(), "ffprobe failed");
    let probe: Value = serde_json::from_slice(&out.stdout)?;
    let work = dst.with_extension(format!("tiles-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&work)?;
    let res = render_in(cfg, src, &work, grid_from_probe(&probe));
    let _ = std::fs::remove_dir_all(&work);
    let mut img = res?;
    if let Some(o) =
        exif_orientation(src).and_then(|o| image::metadata::Orientation::from_exif(o as u8))
    {
        img.apply_orientation(o);
    }
    if img.width().max(img.height()) > RENDER_PX {
        img = img.resize(RENDER_PX, RENDER_PX, image::imageops::FilterType::Triangle);
    }
    let tmp = dst.with_extension("part.jpg");
    let mut buf = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
        .encode_image(&img.to_rgb8())?;
    drop(buf);
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

fn render_in(
    cfg: &Config,
    src: &Path,
    work: &Path,
    grid: Option<Grid>,
) -> anyhow::Result<DynamicImage> {
    let mut cmd = Command::new(&cfg.ffmpeg);
    cmd.args(["-nostdin", "-v", "error", "-y", "-i"]).arg(src);
    let tile_path = |i: usize| -> PathBuf { work.join(format!("{i}.png")) };
    match &grid {
        Some(g) => {
            for (i, t) in g.tiles.iter().enumerate() {
                cmd.args(["-map", &format!("0:{}", t.stream)])
                    .args(["-frames:v", "1", "-update", "1", "-pix_fmt", "rgb24"])
                    .arg(tile_path(i));
            }
        }
        None => {
            cmd.args([
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-update",
                "1",
                "-pix_fmt",
                "rgb24",
            ])
            .arg(tile_path(0));
        }
    }
    let status = cmd.stdin(Stdio::null()).stderr(Stdio::null()).status()?;
    anyhow::ensure!(status.success(), "ffmpeg could not decode the HEIF");
    let Some(g) = grid else {
        return Ok(image::open(tile_path(0))?);
    };
    let mut canvas = RgbImage::new(g.width, g.height);
    for (i, t) in g.tiles.iter().enumerate() {
        let tile = image::open(tile_path(i))?.to_rgb8();
        // Edge tiles reach past the picture (5760 coded for 5712 shown): `replace` clips them.
        image::imageops::replace(&mut canvas, &tile, i64::from(t.x), i64::from(t.y));
    }
    Ok(DynamicImage::ImageRgb8(canvas))
}

/// The EXIF orientation (1 to 8) the camera stored: the tiles are the sensor's layout.
fn exif_orientation(path: &Path) -> Option<u16> {
    let exif = nom_exif::read_exif(path).ok()?;
    exif.get(nom_exif::ExifTag::Orientation)?.as_u16()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_an_iphone_tile_grid() {
        // `ffprobe -show_stream_groups` of a 12 MP iPhone 16e HEIC, trimmed to two tiles.
        let probe = json!({"stream_groups": [{
            "index": 0, "type": "Tile Grid", "nb_streams": 2,
            "components": [{
                "nb_tiles": 2, "coded_width": 5760, "coded_height": 4480,
                "width": 5712, "height": 4284,
                "subcomponents": [
                    {"stream_index": 0, "tile_horizontal_offset": 0, "tile_vertical_offset": 0},
                    {"stream_index": 1, "tile_horizontal_offset": 640, "tile_vertical_offset": 0}
                ]
            }]
        }]});
        assert_eq!(
            grid_from_probe(&probe),
            Some(Grid {
                width: 5712,
                height: 4284,
                tiles: vec![
                    Tile {
                        stream: 0,
                        x: 0,
                        y: 0
                    },
                    Tile {
                        stream: 1,
                        x: 640,
                        y: 0
                    }
                ],
            })
        );
    }

    #[test]
    fn no_grid_means_a_single_image() {
        assert_eq!(grid_from_probe(&json!({"stream_groups": []})), None);
        assert_eq!(grid_from_probe(&json!({})), None);
        let other = json!({"stream_groups": [{"type": "Tile Grid", "components": [{"width": 0, "height": 10, "subcomponents": []}]}]});
        assert_eq!(grid_from_probe(&other), None);
    }

    /// `LUCINA_HEIF_SAMPLE=<an iPhone .heic> cargo test -p lucina-media heif -- --ignored`
    #[test]
    #[ignore]
    fn renders_a_real_iphone_heic() {
        let src = PathBuf::from(std::env::var("LUCINA_HEIF_SAMPLE").expect("LUCINA_HEIF_SAMPLE"));
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        let dst = tmp.path().join("preview.jpg");
        render_preview(&cfg, &src, &dst).unwrap();
        let img = image::open(&dst).unwrap();
        eprintln!("rendered {}x{}", img.width(), img.height());
        assert_eq!(img.width().max(img.height()), RENDER_PX);
        if let Ok(out) = std::env::var("LUCINA_HEIF_OUT") {
            std::fs::copy(&dst, out).unwrap();
        }
    }

    #[test]
    fn heif_is_told_by_its_brand() {
        let tmp = tempfile::tempdir().unwrap();
        let heic = tmp.path().join("a.bin");
        std::fs::write(&heic, b"\0\0\0\x18ftypheic\0\0\0\0").unwrap();
        assert!(is_heif(&heic));
        let mp4 = tmp.path().join("b.heic");
        std::fs::write(&mp4, b"\0\0\0\x18ftypisom\0\0\0\0").unwrap();
        assert!(!is_heif(&mp4));
        assert!(!is_heif(&tmp.path().join("missing")));
    }
}

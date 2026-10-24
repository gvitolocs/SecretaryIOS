//! ffprobe, normalised for `secretary_player::playback` and cached on disk
//! per (path, size, mtime) so a file is probed once.
use secretary_player::playback::Probe;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub probe: Probe,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Embedded subtitle streams (all kinds; `text` ones can become WebVTT).
    pub subtitles: Vec<EmbeddedSub>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedSub {
    /// ffmpeg stream index.
    pub index: u32,
    pub codec: String,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub text: bool,
    pub forced: bool,
}

/// Text subtitle codecs ffmpeg converts to WebVTT. Bitmap ones (dvd_subtitle,
/// hdmv_pgs_subtitle, dvb_subtitle) need OCR or burning in.
fn is_text_sub(codec: &str) -> bool {
    matches!(codec, "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "mov_text" | "text")
}

/// A stable cache key for a file's current bytes, without hashing them.
pub fn file_key(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    h.update(meta.len().to_le_bytes());
    h.update(mtime.to_le_bytes());
    Ok(hex(&h.finalize()[..12]))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn probe(ffprobe: &Path, cache_dir: &Path, path: &Path) -> Result<Info, String> {
    let key = file_key(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cached = cache_dir.join("probe").join(format!("{key}.json"));
    if let Ok(bytes) = tokio::fs::read(&cached).await {
        if let Ok(info) = serde_json::from_slice(&bytes) {
            return Ok(info);
        }
    }
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let json: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let info = parse(&json, path);
    if let Some(dir) = cached.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
        let _ = tokio::fs::write(&cached, serde_json::to_vec(&info).unwrap_or_default()).await;
    }
    Ok(info)
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

pub fn parse(json: &Value, path: &Path) -> Info {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let format = json["format"]["format_name"].as_str().unwrap_or("");
    let container = if format.contains("mp4") || format.contains("mov") {
        if ext == "mov" { "mov" } else { "mp4" }.to_string()
    } else if format.contains("matroska") {
        if ext == "webm" { "webm" } else { "matroska" }.to_string()
    } else {
        format.split(',').next().unwrap_or(&ext).to_string()
    };
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let of = |kind: &str| {
        streams.iter().find(|st| {
            st["codec_type"] == kind
                // Cover art is a video stream with attached_pic set.
                && st["disposition"]["attached_pic"].as_i64() != Some(1)
        })
    };
    let video = of("video");
    let audio = of("audio");
    let bit_depth = video.and_then(|v| {
        v["bits_per_raw_sample"].as_str().and_then(|b| b.parse().ok()).or_else(|| {
            let pix = v["pix_fmt"].as_str()?;
            if pix.contains("10") {
                Some(10)
            } else if pix.contains("12") {
                Some(12)
            } else {
                Some(8)
            }
        })
    });
    let hdr = video.is_some_and(|v| matches!(v["color_transfer"].as_str(), Some("smpte2084" | "arib-std-b67")));
    let probe = Probe {
        container,
        video_codec: video.and_then(|v| s(&v["codec_name"])),
        audio_codec: audio.and_then(|a| s(&a["codec_name"])),
        codec_tag: video.and_then(|v| s(&v["codec_tag_string"])).filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric())),
        hdr,
        bit_depth,
    };
    let duration_ms = json["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).map(|d| (d * 1000.0) as u64);
    let subtitles = streams
        .iter()
        .filter(|st| st["codec_type"] == "subtitle")
        .map(|st| {
            let codec = st["codec_name"].as_str().unwrap_or("").to_string();
            EmbeddedSub {
                index: st["index"].as_u64().unwrap_or(0) as u32,
                text: is_text_sub(&codec),
                codec,
                lang: s(&st["tags"]["language"]).filter(|l| l != "und"),
                title: s(&st["tags"]["title"]),
                forced: st["disposition"]["forced"].as_i64() == Some(1),
            }
        })
        .collect();
    Info {
        probe,
        duration_ms,
        width: video.and_then(|v| v["width"].as_u64()).map(|w| w as u32),
        height: video.and_then(|v| v["height"].as_u64()).map(|h| h as u32),
        subtitles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_mkv_with_subs_and_cover() {
        let j = json!({
            "format": {"format_name": "matroska,webm", "duration": "4578.040000"},
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264", "codec_tag_string": "[0][0][0][0]", "pix_fmt": "yuv420p", "width": 720, "height": 576},
                {"index": 1, "codec_type": "audio", "codec_name": "ac3", "tags": {"language": "dan"}},
                {"index": 2, "codec_type": "subtitle", "codec_name": "dvd_subtitle", "tags": {"language": "dan", "title": "Dansk"}},
                {"index": 3, "codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "eng"}, "disposition": {"forced": 1}},
                {"index": 4, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}}
            ]
        });
        let info = parse(&j, Path::new("/x/film.mkv"));
        assert_eq!(info.probe.container, "matroska");
        assert_eq!(info.probe.video_codec.as_deref(), Some("h264"));
        assert_eq!(info.probe.audio_codec.as_deref(), Some("ac3"));
        assert_eq!(info.probe.codec_tag, None);
        assert_eq!(info.probe.bit_depth, Some(8));
        assert_eq!(info.duration_ms, Some(4_578_040));
        assert_eq!(info.subtitles.len(), 2);
        assert!(!info.subtitles[0].text);
        assert!(info.subtitles[1].text && info.subtitles[1].forced);
    }

    #[test]
    fn audio_with_cover_art_has_no_video() {
        let j = json!({
            "format": {"format_name": "mp3", "duration": "146.83"},
            "streams": [
                {"index": 0, "codec_type": "audio", "codec_name": "mp3"},
                {"index": 1, "codec_type": "video", "codec_name": "png", "disposition": {"attached_pic": 1}}
            ]
        });
        let info = parse(&j, Path::new("a.mp3"));
        assert_eq!(info.probe.video_codec, None);
        assert_eq!(info.probe.container, "mp3");
    }

    #[test]
    fn hevc_hdr_mov() {
        let j = json!({
            "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2"},
            "streams": [{"index": 0, "codec_type": "video", "codec_name": "hevc", "codec_tag_string": "hvc1", "pix_fmt": "yuv420p10le", "color_transfer": "arib-std-b67"}]
        });
        let info = parse(&j, Path::new("IMG_0001.MOV"));
        assert_eq!(info.probe.container, "mov");
        assert!(info.probe.hdr);
        assert_eq!(info.probe.bit_depth, Some(10));
        assert_eq!(info.probe.codec_tag.as_deref(), Some("hvc1"));
    }
}

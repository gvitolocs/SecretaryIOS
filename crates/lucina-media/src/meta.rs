//! Metadata extraction: EXIF (nom-exif) for images, ffprobe for video and audio, and the
//! phone's own PhotoKit/MediaStore values, which win when present.

use std::path::Path;
use std::process::Command;

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// What the client knows about an asset (all optional).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ClientMeta {
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub taken_at: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Stable id of the device ("iphone-16e-…"), used with `asset_id` for edit versioning.
    pub device: Option<String>,
    /// PhotoKit localIdentifier / MediaStore id.
    pub asset_id: Option<String>,
    /// The asset id of the other half of a Live Photo.
    pub live_pair_asset: Option<String>,
    /// This file is the motion part of a Live Photo (hidden from the timeline).
    pub live_motion: Option<bool>,
    pub favorite: Option<bool>,
    pub screenshot: Option<bool>,
    pub source: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub album: Option<String>,
}

/// Metadata after merging the file's own and the client's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extracted {
    pub kind: String,
    pub mime: String,
    pub taken_at: Option<DateTime<Utc>>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub camera: Option<String>,
    pub orientation: Option<u16>,
}

pub fn kind_for_mime(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "photo",
        "video" => "video",
        "audio" => "audio",
        _ => "file",
    }
}

pub fn mime_for(path: &Path, hint: Option<&str>) -> String {
    if let Some(h) = hint.filter(|h| h.contains('/') && *h != "application/octet-stream") {
        return h.to_ascii_lowercase();
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "heic" | "heif" => "image/heic".into(),
        "dng" => "image/x-adobe-dng".into(),
        "mov" => "video/quicktime".into(),
        "m4a" => "audio/mp4".into(),
        "mkv" => "video/x-matroska".into(),
        _ => mime_guess::from_ext(&ext).first_or_octet_stream().essence_str().to_string(),
    }
}

/// Lower-case extension used for the stored object, derived from the MIME type if the name has none.
pub fn ext_for(path: &Path, mime: &str) -> String {
    if let Some(e) = path.extension().and_then(|e| e.to_str()).filter(|e| e.len() <= 8 && e.is_ascii()) {
        return e.to_ascii_lowercase();
    }
    match mime {
        "image/jpeg" => "jpg",
        "image/heic" => "heic",
        "image/png" => "png",
        "video/quicktime" => "mov",
        "video/mp4" => "mp4",
        "audio/mp4" => "m4a",
        "audio/mpeg" => "mp3",
        _ => "bin",
    }
    .into()
}

pub fn extract(path: &Path, mime: &str, client: &ClientMeta, tz: Tz, ffprobe: &Path) -> Extracted {
    let kind = kind_for_mime(mime).to_string();
    let mut x = Extracted { kind: kind.clone(), mime: mime.to_string(), ..Default::default() };
    match kind.as_str() {
        "photo" => exif_into(path, tz, &mut x),
        "video" | "audio" => probe_into(path, ffprobe, &mut x),
        _ => {}
    }
    if x.kind == "photo" && (x.width.is_none() || x.height.is_none()) {
        if let Ok((w, h)) = image::image_dimensions(path) {
            x.width = Some(w as i64);
            x.height = Some(h as i64);
        }
    }
    // The phone's values describe the asset as the owner sees it (edited dates, places).
    if let Some(t) = client.taken_at.as_deref().and_then(|t| DateTime::parse_from_rfc3339(t).ok()) {
        x.taken_at = Some(t.with_timezone(&Utc));
    }
    if let (Some(lat), Some(lon)) = (client.lat, client.lon) {
        x.lat = Some(lat);
        x.lon = Some(lon);
    }
    x.width = client.width.or(x.width);
    x.height = client.height.or(x.height);
    x.duration_ms = x.duration_ms.or(client.duration_ms);
    x
}

fn exif_into(path: &Path, tz: Tz, x: &mut Extracted) {
    use nom_exif::ExifTag;
    let Ok(exif) = nom_exif::read_exif(path) else { return };
    let text = |t: ExifTag| exif.get(t).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let num = |t: ExifTag| exif.get(t).and_then(|v| v.as_u32().map(i64::from).or_else(|| v.as_u16().map(i64::from)));
    let offset = text(ExifTag::OffsetTimeOriginal).and_then(|o| parse_offset(&o));
    for tag in [ExifTag::DateTimeOriginal, ExifTag::CreateDate] {
        if let Some(dt) = exif.get(tag).and_then(|v| v.as_datetime()) {
            x.taken_at = match (dt.aware(), offset) {
                (Some(a), _) => Some(a.with_timezone(&Utc)),
                (None, Some(off)) => off.from_local_datetime(&dt.into_naive()).single().map(|d| d.with_timezone(&Utc)),
                (None, None) => local_to_utc(dt.into_naive(), tz),
            };
            if x.taken_at.is_some() {
                break;
            }
        }
    }
    if let Some(g) = exif.gps_info() {
        if let (Some(lat), Some(lon)) = (g.latitude_decimal(), g.longitude_decimal()) {
            if lat.is_finite() && lon.is_finite() && !(lat == 0.0 && lon == 0.0) {
                x.lat = Some(lat);
                x.lon = Some(lon);
            }
        }
    }
    x.width = num(ExifTag::ExifImageWidth);
    x.height = num(ExifTag::ExifImageHeight);
    x.orientation = exif.get(ExifTag::Orientation).and_then(|v| v.as_u16());
    x.camera = match (text(ExifTag::Make), text(ExifTag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (a, b) => a.or(b),
    };
}

fn parse_offset(s: &str) -> Option<FixedOffset> {
    let s = s.trim();
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let (h, m) = s[1..].split_once(':').unwrap_or((&s[1..3.min(s.len())], s.get(3..).unwrap_or("0")));
    FixedOffset::east_opt(sign * (h.parse::<i32>().ok()? * 3600 + m.parse::<i32>().ok()? * 60))
}

fn local_to_utc(n: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&n).earliest().map(|d| d.with_timezone(&Utc))
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

fn probe_into(path: &Path, ffprobe: &Path, x: &mut Extracted) {
    let out = Command::new(ffprobe)
        .args(["-v", "quiet", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output();
    let Ok(out) = out else { return };
    let Ok(p) = serde_json::from_slice::<Probe>(&out.stdout) else { return };
    let mut rotation = 0i64;
    for s in &p.streams {
        match s.codec_type.as_deref() {
            Some("video") if x.video_codec.is_none() && s.codec_name.as_deref() != Some("mjpeg") => {
                x.video_codec = s.codec_name.clone();
                x.width = s.width;
                x.height = s.height;
                rotation = s
                    .side_data_list
                    .iter()
                    .find_map(|d| d.get("rotation").and_then(|r| r.as_i64()))
                    .or_else(|| s.tags.get("rotate").and_then(|r| r.parse().ok()))
                    .unwrap_or(0);
            }
            Some("audio") if x.audio_codec.is_none() => x.audio_codec = s.codec_name.clone(),
            _ => {}
        }
    }
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut x.width, &mut x.height);
    }
    if x.video_codec.is_none() && x.kind == "video" && x.audio_codec.is_some() {
        x.kind = "audio".into();
    }
    let Some(f) = p.format else { return };
    x.duration_ms = f.duration.and_then(|d| d.parse::<f64>().ok()).map(|d| (d * 1000.0).round() as i64);
    let tag = |k: &str| f.tags.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
    x.taken_at = tag("com.apple.quicktime.creationdate")
        .and_then(|d| parse_loose_datetime(&d))
        .or_else(|| tag("creation_time").and_then(|d| parse_loose_datetime(&d)))
        .filter(|d| d.timestamp() > 0);
    if let Some((lat, lon)) = tag("com.apple.quicktime.location.ISO6709")
        .or_else(|| tag("location"))
        .and_then(|l| parse_iso6709(&l))
    {
        x.lat = Some(lat);
        x.lon = Some(lon);
    }
    x.camera = tag("com.apple.quicktime.model").or_else(|| tag("model"));
}

/// RFC 3339, or Apple's `2025-07-12T14:03:22+0200`.
pub fn parse_loose_datetime(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z"))
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// `+55.6761+012.5683+010.000/` → (55.6761, 12.5683).
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let mut parts = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if (ch == '+' || ch == '-') && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    let lat: f64 = parts.first()?.parse().ok()?;
    let lon: f64 = parts.get(1)?.parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && !(lat == 0.0 && lon == 0.0)).then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709_and_apple_dates_parse() {
        assert_eq!(parse_iso6709("+55.6761+012.5683+010.000/"), Some((55.6761, 12.5683)));
        assert_eq!(parse_iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(parse_iso6709("+00.0000+000.0000/"), None);
        let d = parse_loose_datetime("2025-07-12T14:03:22+0200").unwrap();
        assert_eq!(d.to_rfc3339(), "2025-07-12T12:03:22+00:00");
        assert!(parse_loose_datetime("2025-07-12T12:03:22.000000Z").is_some());
    }

    #[test]
    fn mime_and_kind() {
        assert_eq!(mime_for(Path::new("IMG_0001.HEIC"), None), "image/heic");
        assert_eq!(mime_for(Path::new("a.mov"), None), "video/quicktime");
        assert_eq!(mime_for(Path::new("a.bin"), Some("image/jpeg")), "image/jpeg");
        assert_eq!(kind_for_mime("audio/mp4"), "audio");
        assert_eq!(ext_for(Path::new("x"), "image/heic"), "heic");
        assert_eq!(parse_offset("+02:00"), FixedOffset::east_opt(7200));
        assert_eq!(parse_offset("-0530"), FixedOffset::east_opt(-(5 * 3600 + 30 * 60)));
    }
}

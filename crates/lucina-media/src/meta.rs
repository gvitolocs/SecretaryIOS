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
    /// The type identifier the device reports for the original (`public.heic`), shown in the compare view.
    pub uti: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub album: Option<String>,
    /// The folder the file was in, relative to the import root (`gallery::clean_folder`).
    pub origin_folder: Option<String>,
    /// The import root's own name, or a configured folder's label.
    pub origin_root: Option<String>,
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
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "heic" | "heif" => "image/heic".into(),
        "dng" => "image/x-adobe-dng".into(),
        "mov" => "video/quicktime".into(),
        "m4a" => "audio/mp4".into(),
        "mkv" => "video/x-matroska".into(),
        _ => mime_guess::from_ext(&ext)
            .first_or_octet_stream()
            .essence_str()
            .to_string(),
    }
}

/// Lower-case extension used for the stored object, derived from the MIME type if the name has none.
pub fn ext_for(path: &Path, mime: &str) -> String {
    if let Some(e) = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 8 && e.is_ascii())
    {
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
    let mut x = Extracted {
        kind: kind.clone(),
        mime: mime.to_string(),
        ..Default::default()
    };
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
    if let Some(t) = client
        .taken_at
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
    {
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
    let Ok(exif) = nom_exif::read_exif(path) else {
        return;
    };
    let text = |t: ExifTag| {
        exif.get(t)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let num = |t: ExifTag| {
        exif.get(t).and_then(|v| {
            v.as_u32()
                .map(i64::from)
                .or_else(|| v.as_u16().map(i64::from))
        })
    };
    x.taken_at = exif_taken_at(&exif, tz).map(|(t, _)| t);
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

/// When the picture was taken, from EXIF (UTC), and the offset from UTC it states when it does.
/// A zone-less time is read in `tz`.
fn exif_taken_at(exif: &nom_exif::Exif, tz: Tz) -> Option<(DateTime<Utc>, Option<FixedOffset>)> {
    use nom_exif::ExifTag;
    let offset = exif
        .get(ExifTag::OffsetTimeOriginal)
        .and_then(|v| v.as_str())
        .and_then(|o| parse_offset(o.trim()));
    for tag in [ExifTag::DateTimeOriginal, ExifTag::CreateDate] {
        if let Some(dt) = exif.get(tag).and_then(|v| v.as_datetime()) {
            let taken = match (dt.aware(), offset) {
                (Some(a), _) => Some((a.with_timezone(&Utc), Some(*a.offset()))),
                (None, Some(off)) => off
                    .from_local_datetime(&dt.into_naive())
                    .single()
                    .map(|d| (d.with_timezone(&Utc), Some(off))),
                (None, None) => local_to_utc(dt.into_naive(), tz).map(|d| (d, None)),
            };
            if taken.is_some() {
                return taken;
            }
        }
    }
    None
}

/// `+02:00`, the way the Gallery shows an offset from UTC.
pub fn format_offset(o: FixedOffset) -> String {
    let secs = o.local_minus_utc();
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

/// The camera data of one file, read on demand from the original (`GET items/{id}/details`).
/// Fields the file does not have are `null`.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ExifDetails {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    /// Exposure time in seconds (`0.008` for 1/125).
    pub exposure_s: Option<f64>,
    pub f_number: Option<f64>,
    pub focal_mm: Option<f64>,
    /// Whether the flash fired.
    pub flash: Option<bool>,
    /// The EXIF orientation, 1 to 8.
    pub orientation: Option<u16>,
    pub software: Option<String>,
    /// RFC 3339, UTC.
    pub taken_at: Option<String>,
    /// The offset from UTC the camera stated (`+02:00`).
    pub offset: Option<String>,
    /// Videos and audio: the length in milliseconds.
    pub duration_ms: Option<i64>,
}

impl ExifDetails {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read the camera data of a photo (EXIF) or a video or audio file (what nom-exif finds in the
/// container). `None` when the file has none or cannot be read.
pub fn read_details(path: &Path, kind: &str, tz: Tz) -> Option<ExifDetails> {
    let d = match kind {
        "photo" => exif_details(path, tz)?,
        "video" | "audio" => track_details(path)?,
        _ => return None,
    };
    (!d.is_empty()).then_some(d)
}

fn exif_details(path: &Path, tz: Tz) -> Option<ExifDetails> {
    use nom_exif::ExifTag;
    let exif = nom_exif::read_exif(path).ok()?;
    let text = |t: ExifTag| {
        exif.get(t)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let float = |t: ExifTag| {
        exif.get(t)
            .and_then(|v| v.try_as_float())
            .filter(|f| f.is_finite() && *f > 0.0)
    };
    let int = |t: ExifTag| exif.get(t).and_then(|v| v.try_as_integer());
    let taken = exif_taken_at(&exif, tz);
    Some(ExifDetails {
        make: text(ExifTag::Make),
        model: text(ExifTag::Model),
        lens: text(ExifTag::LensModel),
        iso: int(ExifTag::ISOSpeedRatings),
        exposure_s: float(ExifTag::ExposureTime),
        f_number: float(ExifTag::FNumber),
        focal_mm: float(ExifTag::FocalLength),
        flash: int(ExifTag::Flash).map(|f| f & 1 == 1),
        orientation: int(ExifTag::Orientation)
            .and_then(|o| u16::try_from(o).ok())
            .filter(|o| (1..=8).contains(o)),
        software: text(ExifTag::Software),
        taken_at: taken.map(|(t, _)| rfc3339(t)),
        offset: taken.and_then(|(_, o)| o).map(format_offset),
        duration_ms: None,
    })
}

fn track_details(path: &Path) -> Option<ExifDetails> {
    use nom_exif::TrackInfoTag;
    let t = nom_exif::read_track(path).ok()?;
    let text = |tag: TrackInfoTag| {
        t.get(tag)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some(ExifDetails {
        make: text(TrackInfoTag::Make),
        model: text(TrackInfoTag::Model),
        software: text(TrackInfoTag::Software),
        taken_at: t
            .get(TrackInfoTag::CreateDate)
            .and_then(|v| v.as_datetime())
            .and_then(|d| d.aware())
            .map(|d| rfc3339(d.with_timezone(&Utc))),
        duration_ms: t
            .get(TrackInfoTag::DurationMs)
            .and_then(|v| v.try_as_integer()),
        ..Default::default()
    })
}

fn parse_offset(s: &str) -> Option<FixedOffset> {
    let s = s.trim();
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let (h, m) = s[1..]
        .split_once(':')
        .unwrap_or((&s[1..3.min(s.len())], s.get(3..).unwrap_or("0")));
    FixedOffset::east_opt(sign * (h.parse::<i32>().ok()? * 3600 + m.parse::<i32>().ok()? * 60))
}

fn local_to_utc(n: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&n)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
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
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output();
    let Ok(out) = out else { return };
    let Ok(p) = serde_json::from_slice::<Probe>(&out.stdout) else {
        return;
    };
    let mut rotation = 0i64;
    for s in &p.streams {
        match s.codec_type.as_deref() {
            Some("video")
                if x.video_codec.is_none() && s.codec_name.as_deref() != Some("mjpeg") =>
            {
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
    x.duration_ms = f
        .duration
        .and_then(|d| d.parse::<f64>().ok())
        .map(|d| (d * 1000.0).round() as i64);
    let tag = |k: &str| {
        f.tags
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
    };
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
    ((-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
        && !(lat == 0.0 && lon == 0.0))
        .then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709_and_apple_dates_parse() {
        assert_eq!(
            parse_iso6709("+55.6761+012.5683+010.000/"),
            Some((55.6761, 12.5683))
        );
        assert_eq!(
            parse_iso6709("-33.8688+151.2093/"),
            Some((-33.8688, 151.2093))
        );
        assert_eq!(parse_iso6709("+00.0000+000.0000/"), None);
        let d = parse_loose_datetime("2025-07-12T14:03:22+0200").unwrap();
        assert_eq!(d.to_rfc3339(), "2025-07-12T12:03:22+00:00");
        assert!(parse_loose_datetime("2025-07-12T12:03:22.000000Z").is_some());
    }

    #[test]
    fn mime_and_kind() {
        assert_eq!(mime_for(Path::new("IMG_0001.HEIC"), None), "image/heic");
        assert_eq!(mime_for(Path::new("a.mov"), None), "video/quicktime");
        assert_eq!(
            mime_for(Path::new("a.bin"), Some("image/jpeg")),
            "image/jpeg"
        );
        assert_eq!(kind_for_mime("audio/mp4"), "audio");
        assert_eq!(ext_for(Path::new("x"), "image/heic"), "heic");
        assert_eq!(parse_offset("+02:00"), FixedOffset::east_opt(7200));
        assert_eq!(
            parse_offset("-0530"),
            FixedOffset::east_opt(-(5 * 3600 + 30 * 60))
        );
    }
}

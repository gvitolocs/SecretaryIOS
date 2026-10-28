//! yt-dlp (pinned) into the archive folder, and the media library entry (source `youtube`).
//!
//! Three yt-dlp calls, kept apart on purpose: the details (`--dump-single-json`), the video
//! (no subtitle flags: asking for subtitles in the same call got HTTP 429 and made yt-dlp
//! exit before the video was downloaded) and the subtitles (best effort, one track, one retry).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Map, Value};

use super::{thermal, Chapter, VideoConfig, VideoMeta};

const VERSION_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 3600);
const SUBTITLE_TIMEOUT: Duration = Duration::from_secs(180);
/// Pause before the one retry of a rate-limited (HTTP 429) subtitle request.
const SUBTITLE_RETRY_AFTER: Duration = Duration::from_secs(20);
const MEDIA_TIMEOUT: Duration = Duration::from_secs(15);

/// What [`download`] left in the folder (absolute paths).
#[derive(Debug, Clone, Default)]
pub struct Files {
    pub video: PathBuf,
    pub info_json: PathBuf,
    pub description: Option<PathBuf>,
    pub thumbnail: Option<PathBuf>,
    /// The best subtitle track (manual first, then the original-language auto captions), VTT.
    pub subtitles: Option<PathBuf>,
    pub chapters: Option<PathBuf>,
}

// ---------------------------------------------------------------- yt-dlp plumbing

/// yt-dlp with the options every call needs.
fn ytdlp(cfg: &VideoConfig) -> tokio::process::Command {
    let mut cmd = thermal::command(&cfg.thermal, &cfg.ytdlp);
    cmd.args(["--js-runtimes", &cfg.js_runtime]);
    cmd
}

/// Refuses to run with a yt-dlp other than the pinned one (YouTube breaks old versions, and a
/// silent upgrade changes behaviour; both are updated together).
async fn check_version(cfg: &VideoConfig) -> anyhow::Result<()> {
    let mut cmd = thermal::command(&cfg.thermal, &cfg.ytdlp);
    cmd.arg("--version");
    let out = thermal::output(cmd, VERSION_TIMEOUT)
        .await
        .map_err(|e| anyhow!("yt-dlp ({}) does not run: {e}", cfg.ytdlp.display()))?;
    version_matches(out.trim(), &cfg.ytdlp_version)
}

fn version_matches(found: &str, pinned: &str) -> anyhow::Result<()> {
    if found == pinned.trim() {
        Ok(())
    } else {
        bail!("yt-dlp is {found} but Lucina is pinned to {pinned}")
    }
}

/// The full info JSON of a video (megabytes: formats and caption URLs included).
async fn dump_info(cfg: &VideoConfig, url: &str) -> anyhow::Result<Value> {
    check_version(cfg).await?;
    let mut cmd = ytdlp(cfg);
    cmd.args([
        "--dump-single-json",
        "--no-playlist",
        "--skip-download",
        "--",
    ])
    .arg(url);
    let out = thermal::output(cmd, METADATA_TIMEOUT).await?;
    serde_json::from_str(&out).map_err(|e| {
        let head: String = out.chars().take(200).collect();
        anyhow!("yt-dlp printed no valid JSON ({e}): {head}")
    })
}

// ---------------------------------------------------------------- metadata

/// Reads the video's details without downloading anything.
pub async fn metadata(cfg: &VideoConfig, url: &str) -> anyhow::Result<VideoMeta> {
    let info = dump_info(cfg, url).await?;
    parse_meta(&info, url)
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// yt-dlp's info JSON → [`VideoMeta`]. `url` is the link the owner shared.
fn parse_meta(info: &Value, url: &str) -> anyhow::Result<VideoMeta> {
    let id = str_field(info, "id").ok_or_else(|| anyhow!("yt-dlp gave no video id"))?;
    let duration = info.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    let webpage_url = str_field(info, "webpage_url").unwrap_or_else(|| url.to_owned());
    let is_short = url.contains("/shorts/")
        || webpage_url.contains("/shorts/")
        || str_field(info, "media_type").is_some_and(|t| t == "short");
    let chapters = info
        .get("chapters")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| Chapter {
                    start: c.get("start_time").and_then(Value::as_f64).unwrap_or(0.0),
                    end: c
                        .get("end_time")
                        .and_then(Value::as_f64)
                        .unwrap_or(duration),
                    title: str_field(c, "title").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let tags = info
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(VideoMeta {
        title: str_field(info, "title")
            .or_else(|| str_field(info, "fulltitle"))
            .unwrap_or_else(|| id.clone()),
        channel: str_field(info, "channel")
            .or_else(|| str_field(info, "uploader"))
            .unwrap_or_default(),
        upload_date: str_field(info, "upload_date"),
        duration,
        language: str_field(info, "language"),
        description: info
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        tags,
        chapters,
        webpage_url,
        is_short,
        id,
    })
}

// ---------------------------------------------------------------- the info.json we keep

/// Keys that make the info JSON megabytes long, or are only useful while downloading.
const DROP_KEYS: &[&str] = &[
    "formats",
    "requested_formats",
    "requested_downloads",
    "requested_subtitles",
    "automatic_captions",
    "subtitles",
    "thumbnails",
    "heatmap",
    "http_headers",
    "fragments",
    "url",
    "manifest_url",
];

fn lang_keys(info: &Value, key: &str) -> Vec<String> {
    let mut langs: Vec<String> = info
        .get(key)
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    langs.sort();
    langs
}

/// The info JSON without the bulk (formats, caption URL lists, thumbnails, heatmap, `_*`
/// keys…). The caption languages stay as `subtitle_langs` and `automatic_caption_langs`, so a
/// later run can still choose a subtitle track.
fn trim_info(info: &Value) -> Value {
    let Some(map) = info.as_object() else {
        return info.clone();
    };
    let mut out: Map<String, Value> = map
        .iter()
        .filter(|(k, _)| !k.starts_with('_') && !DROP_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if map.contains_key("subtitles") || map.contains_key("subtitle_langs") {
        let langs = if map.contains_key("subtitles") {
            lang_keys(info, "subtitles")
        } else {
            string_list(info, "subtitle_langs")
        };
        out.insert("subtitle_langs".into(), json!(langs));
    }
    if map.contains_key("automatic_captions") || map.contains_key("automatic_caption_langs") {
        let langs = if map.contains_key("automatic_captions") {
            lang_keys(info, "automatic_captions")
        } else {
            string_list(info, "automatic_caption_langs")
        };
        out.insert("automatic_caption_langs".into(), json!(langs));
    }
    Value::Object(out)
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- subtitles

/// Which subtitle track to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubChoice {
    pub lang: String,
    /// Auto-generated captions rather than a manual track.
    pub auto: bool,
}

/// `en-US` → `en`.
fn primary_lang(lang: &str) -> String {
    lang.split(['-', '_'])
        .next()
        .unwrap_or(lang)
        .to_ascii_lowercase()
}

/// The one track worth fetching: a manual track in the video's language first, else the
/// original-language auto captions (`<lang>-orig`). `manual` and `auto` are the language
/// codes yt-dlp lists. Without a known video language the first manual track, or the first
/// `*-orig` auto track, is taken. Translated auto captions are never chosen.
pub fn choose_subtitles(
    manual: &[String],
    auto: &[String],
    language: Option<&str>,
) -> Option<SubChoice> {
    let usable = |list: &'_ [String]| -> Vec<String> {
        list.iter().filter(|l| *l != "live_chat").cloned().collect()
    };
    let (manual, auto) = (usable(manual), usable(auto));
    let pick = |lang: &str, auto: bool| {
        Some(SubChoice {
            lang: lang.to_owned(),
            auto,
        })
    };
    let has_orig = auto.iter().any(|l| l.ends_with("-orig"));

    let Some(lang) = language.map(str::trim).filter(|l| !l.is_empty()) else {
        if let Some(m) = manual.first() {
            return pick(m, false);
        }
        return auto
            .iter()
            .find(|l| l.ends_with("-orig"))
            .and_then(|l| pick(l, true));
    };
    let primary = primary_lang(lang);

    let manual_hit = manual
        .iter()
        .find(|l| l.eq_ignore_ascii_case(lang))
        .or_else(|| manual.iter().find(|l| primary_lang(l) == primary));
    if let Some(m) = manual_hit {
        return pick(m, false);
    }
    let orig_hit = auto.iter().find(|l| {
        l.strip_suffix("-orig")
            .is_some_and(|b| primary_lang(b) == primary)
    });
    if let Some(a) = orig_hit {
        return pick(a, true);
    }
    // No `-orig` tracks at all: the plain code of the video's language is the original ASR.
    if !has_orig {
        let plain = auto
            .iter()
            .find(|l| l.eq_ignore_ascii_case(lang))
            .or_else(|| auto.iter().find(|l| l.eq_ignore_ascii_case(&primary)));
        if let Some(a) = plain {
            return pick(a, true);
        }
    }
    None
}

/// `subtitles.<lang>.vtt` already in the folder.
fn find_subtitles(folder: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("subtitles.")
                    && n.ends_with(".vtt")
                    && n.len() > "subtitles..vtt".len()
            }) && std::fs::metadata(p).is_ok_and(|m| m.len() > 0)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// One subtitle track as `subtitles.<lang>.vtt`. Best effort: any failure is logged.
async fn fetch_subtitles(
    cfg: &VideoConfig,
    url: &str,
    choice: &SubChoice,
    folder: &Path,
    retry_after: Duration,
) -> Option<PathBuf> {
    for attempt in 0..2 {
        let mut cmd = ytdlp(cfg);
        cmd.args([
            "--skip-download",
            "--no-playlist",
            "--sub-format",
            "vtt",
            "--sub-langs",
        ])
        .arg(&choice.lang)
        .arg(if choice.auto {
            "--write-auto-subs"
        } else {
            "--write-subs"
        })
        .arg("-o")
        .arg(format!("subtitle:{}", folder.join("subtitles").display()))
        .arg("--")
        .arg(url);
        match thermal::output(cmd, SUBTITLE_TIMEOUT).await {
            Ok(_) => break,
            Err(e) if attempt == 0 && e.to_string().contains("429") => {
                tracing::info!(lang = %choice.lang, "subtitle request rate-limited; retrying once");
                tokio::time::sleep(retry_after).await;
            }
            Err(e) => {
                tracing::warn!(lang = %choice.lang, error = %e, "subtitles not fetched");
                break;
            }
        }
    }
    let found = find_subtitles(folder);
    if found.is_none() {
        tracing::info!(lang = %choice.lang, "yt-dlp left no subtitle file");
    }
    found
}

// ---------------------------------------------------------------- download

/// `folder/video.<ext>` (not a part file or a separate stream), non-empty.
fn find_video(folder: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_stem().is_some_and(|s| s == "video")
                && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    !matches!(e, "part" | "ytdl" | "temp" | "json" | "jpg" | "vtt")
                })
                && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

fn nonempty(p: PathBuf) -> Option<PathBuf> {
    std::fs::metadata(&p)
        .is_ok_and(|m| m.len() > 0)
        .then_some(p)
}

/// Downloads the best video + audio (remuxed to mkv) and writes the description, a trimmed
/// `info.json`, `chapters.json`, `thumbnail.jpg` and one subtitle track into `folder`.
/// Idempotent: a folder that already holds `video.*` skips the video download.
pub async fn download(
    cfg: &VideoConfig,
    url: &str,
    meta: &VideoMeta,
    folder: &Path,
) -> anyhow::Result<Files> {
    let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    tokio::fs::create_dir_all(&folder)
        .await
        .with_context(|| format!("creating {}", folder.display()))?;
    let info_path = folder.join("info.json");
    let full_path = folder.join("info-full.info.json");

    let video = match find_video(&folder) {
        Some(v) => v,
        None => {
            check_version(cfg).await?;
            let mut cmd = ytdlp(cfg);
            cmd.args(["-f", "bv*+ba/b", "--merge-output-format", "mkv"])
                .args([
                    "--no-playlist",
                    "--no-progress",
                    "--retries",
                    "5",
                    "--fragment-retries",
                    "10",
                ])
                .args([
                    "--write-thumbnail",
                    "--convert-thumbnails",
                    "jpg",
                    "--write-info-json",
                ]);
            if cfg
                .ffmpeg
                .parent()
                .is_some_and(|p| !p.as_os_str().is_empty())
            {
                cmd.arg("--ffmpeg-location").arg(&cfg.ffmpeg);
            }
            cmd.arg("-o").arg(folder.join("video.%(ext)s"));
            cmd.arg("-o").arg(format!(
                "thumbnail:{}",
                folder.join("thumbnail.%(ext)s").display()
            ));
            cmd.arg("-o")
                .arg(format!("infojson:{}", folder.join("info-full").display()));
            cmd.arg("--").arg(url);
            thermal::output(cmd, DOWNLOAD_TIMEOUT).await?;
            find_video(&folder).ok_or_else(|| {
                anyhow!("yt-dlp finished but left no video in {}", folder.display())
            })?
        }
    };

    // The info JSON: trim the one yt-dlp just wrote; on a re-run keep the trimmed one, and
    // ask yt-dlp again only when there is none.
    let info = match tokio::fs::read_to_string(&full_path).await {
        Ok(text) => {
            let v: Value =
                serde_json::from_str(&text).context("yt-dlp's info JSON is not valid")?;
            let _ = tokio::fs::remove_file(&full_path).await;
            Some(v)
        }
        Err(_) if info_path.is_file() => None,
        Err(_) => Some(dump_info(cfg, url).await?),
    };
    if let Some(full) = &info {
        let trimmed = serde_json::to_vec_pretty(&trim_info(full))?;
        tokio::fs::write(&info_path, trimmed)
            .await
            .with_context(|| format!("writing {}", info_path.display()))?;
    }
    let kept: Value = match &info {
        Some(v) => trim_info(v),
        None => serde_json::from_slice(&tokio::fs::read(&info_path).await?).unwrap_or(Value::Null),
    };

    let description = folder.join("description.txt");
    tokio::fs::write(&description, &meta.description).await?;
    let chapters = if meta.chapters.is_empty() {
        None
    } else {
        let p = folder.join("chapters.json");
        tokio::fs::write(&p, serde_json::to_vec_pretty(&meta.chapters)?).await?;
        Some(p)
    };

    let subtitles = match find_subtitles(&folder) {
        Some(existing) => Some(existing),
        None => {
            let manual = string_list(&kept, "subtitle_langs");
            let auto = string_list(&kept, "automatic_caption_langs");
            match choose_subtitles(&manual, &auto, meta.language.as_deref()) {
                Some(choice) => {
                    fetch_subtitles(cfg, url, &choice, &folder, SUBTITLE_RETRY_AFTER).await
                }
                None => None,
            }
        }
    };

    Ok(Files {
        video,
        info_json: info_path,
        description: nonempty(description),
        thumbnail: nonempty(folder.join("thumbnail.jpg")),
        subtitles,
        chapters,
    })
}

// ---------------------------------------------------------------- media library

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        _ => "video/x-matroska",
    }
}

/// The body of `PUT /internal/v1/external/youtube/{id}` (the shape lucina-memo uses for
/// meeting tracks): the file stays where it is, lucina-media only points at it.
fn media_body(meta: &VideoMeta, files: &Files, created_at: &str) -> Value {
    json!({
        "kind": "video",
        "title": meta.title,
        "created_at": created_at,
        "duration_ms": (meta.duration * 1000.0).round().max(0.0) as u64,
        "mime": mime_for(&files.video),
        "path": files.video,
        "album": "YouTube",
        "caption": meta.channel,
        "links": {
            "source": meta.webpage_url,
            "folder": files.video.parent(),
        },
    })
}

/// Registers the video in lucina-media (album "YouTube", source `youtube`, idempotent per
/// video id). Best effort: an unreachable or refusing library is logged and ignored.
pub async fn register_media(
    http: &reqwest::Client,
    cfg: &VideoConfig,
    meta: &VideoMeta,
    files: &Files,
) {
    let body = media_body(meta, files, &lucina_store::timeutil::now_iso());
    let url = format!(
        "{}/internal/v1/external/youtube/{}",
        cfg.media_url.trim_end_matches('/'),
        meta.id
    );
    match http
        .put(url)
        .timeout(MEDIA_TIMEOUT)
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!(id = %meta.id, "video registered in the media library")
        }
        Ok(r) => {
            tracing::warn!(id = %meta.id, status = r.status().as_u16(), "media library refused the video")
        }
        Err(e) => {
            tracing::warn!(id = %meta.id, error = %e.without_url(), "media library not reachable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A small stand-in for the real info JSON of `FBs--2nSPLQ` (a Short; the real one is 8.8 MB).
    fn short_info() -> Value {
        json!({
            "id": "FBs--2nSPLQ",
            "title": "I challenged Rust Developers",
            "formats": [{"format_id": "616", "url": "https://x/y"}, {"format_id": "251"}],
            "thumbnails": [{"url": "https://i.ytimg.com/vi/FBs--2nSPLQ/hq.jpg"}],
            "thumbnail": "https://i.ytimg.com/vi/FBs--2nSPLQ/maxresdefault.jpg",
            "description": "#coding #programming",
            "duration": 98,
            "webpage_url": "https://www.youtube.com/watch?v=FBs--2nSPLQ",
            "tags": [],
            "media_type": "short",
            "automatic_captions": {"en-orig": [{"ext": "vtt"}], "en": [{"ext": "vtt"}], "it": [{"ext": "vtt"}]},
            "subtitles": {},
            "channel": "Arjay McCandless",
            "uploader": "Arjay McCandless",
            "upload_date": "20261015",
            "language": "en-US",
            "chapters": null,
            "heatmap": [{"start_time": 0.0, "value": 1.0}],
            "view_count": 147508,
            "_format_sort_fields": ["quality"],
            "_version": {"version": "2026.08.19"},
        })
    }

    fn test_cfg(ytdlp: &Path, version: &str, media_url: &str) -> VideoConfig {
        let get = |k: &str| match k {
            "LUCINA_VIDEO_SCOPE" => Some("false".to_string()),
            "LUCINA_YTDLP" => Some(ytdlp.display().to_string()),
            "LUCINA_YTDLP_VERSION" => Some(version.to_string()),
            "LUCINA_MEDIA_INTERNAL_URL" => Some(media_url.to_string()),
            "HOME" => Some("/nonexistent".to_string()),
            _ => None,
        };
        let whisper = lucina_memo::transcribe::WhisperConfig::from_lookup(|_| None);
        VideoConfig::from_lookup(&get, Path::new("/nonexistent"), &whisper)
    }

    fn fake_ytdlp(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("yt-dlp");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn langs(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_mismatch_message() {
        let e = version_matches("2026.09.01", "2026.08.19")
            .unwrap_err()
            .to_string();
        assert_eq!(e, "yt-dlp is 2026.09.01 but Lucina is pinned to 2026.08.19");
        assert!(version_matches("2026.08.19", "2026.08.19").is_ok());
    }

    #[tokio::test]
    async fn metadata_refuses_a_different_ytdlp_and_parses_the_pinned_one() {
        let dir = tempfile::tempdir().unwrap();
        let info = short_info().to_string();
        let script = format!(
            "if [ \"$1\" = \"--version\" ]; then echo 2026.08.19; exit 0; fi\ncat <<'EOF'\n{info}\nEOF"
        );
        let bin = fake_ytdlp(dir.path(), &script);

        let wrong = test_cfg(&bin, "2030.01.01", "http://127.0.0.1:1");
        let e = metadata(&wrong, "https://youtu.be/FBs--2nSPLQ")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(e, "yt-dlp is 2026.08.19 but Lucina is pinned to 2030.01.01");

        let ok = test_cfg(&bin, "2026.08.19", "http://127.0.0.1:1");
        let m = metadata(&ok, "https://youtu.be/FBs--2nSPLQ").await.unwrap();
        assert_eq!(m.id, "FBs--2nSPLQ");
        assert!(m.is_short);
    }

    #[test]
    fn parses_the_short() {
        let m = parse_meta(&short_info(), "https://youtu.be/FBs--2nSPLQ").unwrap();
        assert_eq!(m.id, "FBs--2nSPLQ");
        assert_eq!(m.title, "I challenged Rust Developers");
        assert_eq!(m.channel, "Arjay McCandless");
        assert_eq!(m.upload_date.as_deref(), Some("20261015"));
        assert_eq!(m.duration, 98.0);
        assert_eq!(m.language.as_deref(), Some("en-US"));
        assert_eq!(m.description, "#coding #programming");
        assert!(m.tags.is_empty() && m.chapters.is_empty());
        assert_eq!(m.webpage_url, "https://www.youtube.com/watch?v=FBs--2nSPLQ");
        assert!(m.is_short);
    }

    #[test]
    fn parses_chapters_channel_fallback_and_shorts_url() {
        let info = json!({
            "id": "abc", "title": "Talk", "uploader": "Someone", "duration": 600.5,
            "webpage_url": "https://www.youtube.com/watch?v=abc",
            "tags": ["rust", "async"],
            "chapters": [
                {"start_time": 0.0, "end_time": 120.0, "title": "Intro"},
                {"start_time": 120.0, "title": "Main"},
            ],
        });
        let m = parse_meta(&info, "https://www.youtube.com/watch?v=abc").unwrap();
        assert_eq!(m.channel, "Someone");
        assert!(!m.is_short);
        assert_eq!(m.tags, vec!["rust", "async"]);
        assert_eq!(
            m.chapters,
            vec![
                Chapter {
                    start: 0.0,
                    end: 120.0,
                    title: "Intro".into()
                },
                Chapter {
                    start: 120.0,
                    end: 600.5,
                    title: "Main".into()
                },
            ]
        );
        let via_url = parse_meta(&info, "https://www.youtube.com/shorts/abc").unwrap();
        assert!(via_url.is_short);
        assert!(parse_meta(&json!({"title": "x"}), "u").is_err());
    }

    #[test]
    fn trimmed_info_drops_the_bulk_and_keeps_caption_languages() {
        let t = trim_info(&short_info());
        let o = t.as_object().unwrap();
        for k in [
            "formats",
            "thumbnails",
            "heatmap",
            "automatic_captions",
            "subtitles",
            "_version",
            "_format_sort_fields",
        ] {
            assert!(!o.contains_key(k), "{k} should be dropped");
        }
        assert_eq!(o["title"], "I challenged Rust Developers");
        assert_eq!(o["view_count"], 147508);
        assert_eq!(o["automatic_caption_langs"], json!(["en", "en-orig", "it"]));
        assert_eq!(o["subtitle_langs"], json!([]));
        // Trimming twice changes nothing (a re-run reads its own info.json).
        assert_eq!(trim_info(&t), t);
        assert!(t.to_string().len() < 1200);
    }

    #[test]
    fn subtitle_choice() {
        let c = |m: &[&str], a: &[&str], l: Option<&str>| choose_subtitles(&langs(m), &langs(a), l);
        let manual = |l: &str| {
            Some(SubChoice {
                lang: l.into(),
                auto: false,
            })
        };
        let auto = |l: &str| {
            Some(SubChoice {
                lang: l.into(),
                auto: true,
            })
        };
        // Manual in the video's language wins over auto captions.
        assert_eq!(
            c(&["en", "it"], &["en-orig", "en"], Some("en-US")),
            manual("en")
        );
        assert_eq!(c(&["en-GB"], &["en-orig"], Some("en-US")), manual("en-GB"));
        assert_eq!(c(&["it", "de"], &["en-orig"], Some("en")), auto("en-orig"));
        // Only the original-language captions, never the translations.
        assert_eq!(
            c(&[], &["en", "it", "en-orig", "de"], Some("en-US")),
            auto("en-orig")
        );
        assert_eq!(c(&[], &["en", "it", "ja-orig"], Some("en")), None);
        assert_eq!(c(&[], &["it", "de"], Some("en")), None);
        // No `-orig` list: the plain code of the video's language.
        assert_eq!(c(&[], &["en", "it"], Some("en-US")), auto("en"));
        // Unknown language.
        assert_eq!(c(&["fr", "live_chat"], &["fr-orig"], None), manual("fr"));
        assert_eq!(c(&[], &["fr", "fr-orig"], None), auto("fr-orig"));
        assert_eq!(c(&["live_chat"], &[], Some("en")), None);
        assert_eq!(c(&[], &[], None), None);
    }

    #[test]
    fn finds_only_a_finished_video() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(find_video(p).is_none());
        std::fs::write(p.join("video.mkv.part"), "x").unwrap();
        std::fs::write(p.join("video.f251.webm"), "x").unwrap();
        std::fs::write(p.join("video.info.json"), "{}").unwrap();
        std::fs::write(p.join("video.mkv"), "").unwrap();
        assert!(find_video(p).is_none());
        std::fs::write(p.join("video.mkv"), "data").unwrap();
        assert_eq!(find_video(p), Some(p.join("video.mkv")));
    }

    #[tokio::test]
    async fn download_is_idempotent_and_never_calls_ytdlp_again() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("2026").join("2026-10-28 T [abc]");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("video.mkv"), "data").unwrap();
        std::fs::write(folder.join("thumbnail.jpg"), "jpg").unwrap();
        std::fs::write(folder.join("subtitles.en-orig.vtt"), "WEBVTT").unwrap();
        let before = serde_json::to_vec(&trim_info(&short_info())).unwrap();
        std::fs::write(folder.join("info.json"), &before).unwrap();
        // A yt-dlp that fails the test if it is run at all.
        let marker = dir.path().join("was-called");
        let bin = fake_ytdlp(dir.path(), &format!("touch {}\nexit 1", marker.display()));
        let cfg = test_cfg(&bin, "2026.08.19", "http://127.0.0.1:1");

        let meta = VideoMeta {
            id: "abc".into(),
            title: "T".into(),
            description: "desc".into(),
            language: Some("en-US".into()),
            chapters: vec![Chapter {
                start: 0.0,
                end: 5.0,
                title: "One".into(),
            }],
            ..VideoMeta::default()
        };
        let files = download(&cfg, "https://youtu.be/abc", &meta, &folder)
            .await
            .unwrap();
        assert!(!marker.exists(), "yt-dlp was run");
        assert_eq!(files.video, folder.join("video.mkv"));
        assert_eq!(files.info_json, folder.join("info.json"));
        assert_eq!(files.thumbnail, Some(folder.join("thumbnail.jpg")));
        assert_eq!(files.subtitles, Some(folder.join("subtitles.en-orig.vtt")));
        assert_eq!(files.description, Some(folder.join("description.txt")));
        assert_eq!(
            std::fs::read_to_string(folder.join("description.txt")).unwrap(),
            "desc"
        );
        let chapters: Vec<Chapter> =
            serde_json::from_slice(&std::fs::read(files.chapters.unwrap()).unwrap()).unwrap();
        assert_eq!(chapters, meta.chapters);
        assert_eq!(std::fs::read(folder.join("info.json")).unwrap(), before);
    }

    #[test]
    fn media_body_shape() {
        let meta = VideoMeta {
            id: "FBs--2nSPLQ".into(),
            title: "I challenged Rust Developers".into(),
            channel: "Arjay McCandless".into(),
            duration: 98.0,
            webpage_url: "https://www.youtube.com/watch?v=FBs--2nSPLQ".into(),
            ..VideoMeta::default()
        };
        let files = Files {
            video: PathBuf::from("/mnt/v/2026/x [FBs--2nSPLQ]/video.mkv"),
            ..Files::default()
        };
        let b = media_body(&meta, &files, "2026-10-28T10:00:00.000Z");
        assert_eq!(
            b,
            json!({
                "kind": "video",
                "title": "I challenged Rust Developers",
                "created_at": "2026-10-28T10:00:00.000Z",
                "duration_ms": 98000,
                "mime": "video/x-matroska",
                "path": "/mnt/v/2026/x [FBs--2nSPLQ]/video.mkv",
                "album": "YouTube",
                "caption": "Arjay McCandless",
                "links": {
                    "source": "https://www.youtube.com/watch?v=FBs--2nSPLQ",
                    "folder": "/mnt/v/2026/x [FBs--2nSPLQ]",
                },
            })
        );
    }

    #[tokio::test]
    async fn register_media_puts_to_the_youtube_source_and_survives_failures() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/internal/v1/external/youtube/abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"item_id": "m1"})))
            .expect(1)
            .mount(&server)
            .await;
        let cfg = test_cfg(Path::new("/bin/true"), "x", &server.uri());
        let meta = VideoMeta {
            id: "abc".into(),
            title: "T".into(),
            ..VideoMeta::default()
        };
        let files = Files {
            video: PathBuf::from("/x/video.mp4"),
            ..Files::default()
        };
        let http = reqwest::Client::new();
        register_media(&http, &cfg, &meta, &files).await;
        let sent = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["mime"], "video/mp4");
        assert_eq!(body["album"], "YouTube");

        // Nothing listening: no panic, no error.
        let down = test_cfg(Path::new("/bin/true"), "x", "http://127.0.0.1:1");
        register_media(&http, &down, &meta, &files).await;
    }

    /// Downloads a real Short (network; YouTube rate-limits, so run it by hand and rarely):
    /// `cargo test -p lucina-server --lib video::fetch::tests::real_short -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn real_short() {
        let home = std::env::var("HOME").unwrap();
        let bin = PathBuf::from(&home).join(".local/bin/yt-dlp");
        let cfg = test_cfg(&bin, "2026.08.19", "http://127.0.0.1:1");
        let url = "https://www.youtube.com/shorts/FBs--2nSPLQ";
        let meta = metadata(&cfg, url).await.unwrap();
        assert_eq!(meta.id, "FBs--2nSPLQ");
        assert!(meta.is_short);
        let dir = tempfile::tempdir().unwrap();
        let files = download(&cfg, url, &meta, dir.path()).await.unwrap();
        assert!(files.video.metadata().unwrap().len() > 100_000);
        assert!(files.thumbnail.is_some(), "no thumbnail.jpg");
        assert!(std::fs::metadata(&files.info_json).unwrap().len() < 100_000);
        eprintln!("{files:#?}");
    }
}

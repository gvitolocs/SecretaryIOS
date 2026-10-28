//! YouTube video reports (docs/youtube-reports.md).
//!
//! The owner shares a YouTube link and asks for a transcript, a report or a summary
//! ("Transcribe this video and visual inspection … then do a report"). The chat answers at
//! once and starts the background job `youtube_report` (crate::chat::spawn_job), which:
//!
//! 1. fetches the video with a pinned yt-dlp (best video + audio, info JSON, description,
//!    chapters, thumbnail, subtitles when there are any) into the MyBook archive
//!    `lucina-videos/<YYYY>/<YYYY-MM-DD> <title> [<id>]/` and registers it in the media
//!    library (source `youtube`); one folder per video id ([`archive`], [`fetch`]);
//! 2. transcribes it with lucina-memo's Whisper, primed with the title, tags and chapter
//!    names so jargon comes out spelled right ([`transcript`]);
//! 3. inspects it: scene-change keyframes, OCR, and Gemini vision over the keyframes
//!    ([`visual`]);
//! 4. asks ZCode (GLM through Z.ai's vision MCP) for its own transcript and visual notes,
//!    running alongside 2 and 3 ([`zcode`]);
//! 5. merges the transcripts window by window, keeping the better one ([`merge`]);
//! 6. writes the report and saves it as a Keep note labelled "YouTube" ([`report`]).
//!
//! CPU-heavy steps (ffmpeg, the CPU Whisper fallback) run in a capped transient scope and
//! wait while nezopt is hot ([`thermal`]). One video at a time.

pub mod archive;
pub mod fetch;
pub mod gemini;
pub mod merge;
pub mod report;
pub mod thermal;
pub mod transcript;
pub mod trigger;
pub mod visual;
pub mod zcode;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

/// The job action (crate::jobs::ASYNC_ACTIONS).
pub const ACTION: &str = "youtube_report";

#[derive(Debug, Clone)]
pub struct VideoConfig {
    /// `LUCINA_YOUTUBE_REPORTS` (default on).
    pub enabled: bool,
    /// The MyBook archive, `LUCINA_VIDEOS_DIR`.
    pub archive_dir: PathBuf,
    /// Scratch space for one job (`<data>/video-work`), removed when the job ends.
    pub work_dir: PathBuf,
    pub ytdlp: PathBuf,
    /// The pinned yt-dlp version: a different one refuses to run (update both together).
    pub ytdlp_version: String,
    /// yt-dlp's `--js-runtimes` (YouTube needs one for some formats).
    pub js_runtime: String,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Longer videos are refused before anything is downloaded.
    pub max_duration: Duration,
    /// Gemini for keyframe vision and the report (`GEMINI_API_KEY`, read when called).
    pub gemini_model: String,
    pub gemini_fallback_model: String,
    pub gemini_url: String,
    /// The ZCode CLI pass (`LUCINA_VIDEO_ZCODE`, default on).
    pub zcode: bool,
    /// `~/opt/zcode-appimage/resources/glm` (holds `zcode.cjs`).
    pub zcode_dir: PathBuf,
    pub node: PathBuf,
    pub zcode_timeout: Duration,
    pub thermal: thermal::ThermalConfig,
    pub whisper: lucina_memo::transcribe::WhisperConfig,
    /// lucina-media (`LUCINA_MEDIA_INTERNAL_URL`).
    pub media_url: String,
}

impl VideoConfig {
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
        data_dir: &Path,
        whisper: &lucina_memo::transcribe::WhisperConfig,
    ) -> Self {
        let home = get("HOME")
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_else(|| "/home/nez".into());
        let path = |k: &str, d: String| PathBuf::from(get(k).unwrap_or(d));
        let secs = |k: &str, d: u64| {
            Duration::from_secs(get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d))
        };
        let flag = |k: &str, d: bool| {
            get(k).map_or(d, |v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        };
        let mut whisper = whisper.clone();
        // A 3-hour video on the CPU model takes a while; the GPU path is minutes.
        whisper.timeout = secs("LUCINA_VIDEO_WHISPER_TIMEOUT_SEC", 3 * 3600);
        whisper.gpu_wait = secs("LUCINA_VIDEO_GPU_WAIT_SEC", 900);
        Self {
            enabled: flag("LUCINA_YOUTUBE_REPORTS", true),
            archive_dir: path(
                "LUCINA_VIDEOS_DIR",
                format!("{home}/mnt/mybook/lucina-videos"),
            ),
            work_dir: path(
                "LUCINA_VIDEO_WORK_DIR",
                data_dir.join("video-work").display().to_string(),
            ),
            ytdlp: path("LUCINA_YTDLP", format!("{home}/.local/bin/yt-dlp")),
            ytdlp_version: get("LUCINA_YTDLP_VERSION").unwrap_or_else(|| "2026.08.19".into()),
            js_runtime: get("LUCINA_YTDLP_JS_RUNTIME").unwrap_or_else(|| "node".into()),
            ffmpeg: whisper.ffmpeg.clone(),
            ffprobe: whisper.ffprobe.clone(),
            max_duration: secs("LUCINA_VIDEO_MAX_SEC", 3 * 3600),
            gemini_model: get("LUCINA_VIDEO_GEMINI_MODEL")
                .unwrap_or_else(|| "gemini-3.5-flash".into()),
            // 3.8 answered "high demand" for hours on 2026-10-28: the fallback, not the default.
            gemini_fallback_model: get("LUCINA_VIDEO_GEMINI_FALLBACK_MODEL")
                .unwrap_or_else(|| "gemini-3.8-flash".into()),
            gemini_url: get("LUCINA_GEMINI_URL")
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".into()),
            zcode: flag("LUCINA_VIDEO_ZCODE", true),
            zcode_dir: path(
                "LUCINA_ZCODE_DIR",
                format!("{home}/opt/zcode-appimage/resources/glm"),
            ),
            node: path("LUCINA_NODE", "/usr/bin/node".into()),
            zcode_timeout: secs("LUCINA_VIDEO_ZCODE_TIMEOUT_SEC", 40 * 60),
            thermal: thermal::ThermalConfig::from_lookup(get),
            whisper,
            media_url: get("LUCINA_MEDIA_INTERNAL_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8830".into()),
        }
    }
}

// ---------------------------------------------------------------- shared types

/// What yt-dlp says about the video (`--dump-single-json`, trimmed).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VideoMeta {
    pub id: String,
    pub title: String,
    pub channel: String,
    /// `YYYYMMDD`.
    pub upload_date: Option<String>,
    pub duration: f64,
    pub language: Option<String>,
    pub description: String,
    pub tags: Vec<String>,
    pub chapters: Vec<Chapter>,
    pub webpage_url: String,
    pub is_short: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    pub title: String,
}

/// A timed piece of transcript (seconds from the start).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// One keyframe and what is on it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    /// Seconds from the start.
    pub t: f64,
    /// Relative to the video's archive folder (`keyframes/0012.345.jpg`).
    pub file: String,
    /// Tesseract's text, cleaned (may be empty).
    pub ocr: String,
    /// slide | code | terminal | diagram | ui | chart | person | other.
    pub kind: String,
    /// What the vision model saw.
    pub description: String,
    /// Code or commands read off the screen, verbatim.
    pub code: Option<String>,
}

/// Where a piece of the merged transcript came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergedSeg {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// whisper | zcode | captions | both
    pub source: String,
}

/// What the ZCode pass returned (`zcode-result.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ZcodeResult {
    pub transcript: Vec<Seg>,
    pub visual: Vec<ZcodeVisual>,
    pub terms: Vec<String>,
    pub summary: String,
    /// The ZCode session (`sess_…`), for a follow-up.
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ZcodeVisual {
    pub t: f64,
    pub description: String,
    pub on_screen_text: String,
}

/// A step that did not work: the report says so instead of failing the whole job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Skipped {
    pub step: String,
    pub reason: String,
}

// ---------------------------------------------------------------- the job

/// One video at a time; the ids being worked on (a second request for the same video
/// waits for the first one's answer instead of starting again).
fn gate() -> &'static tokio::sync::Semaphore {
    static GATE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Semaphore::new(1))
}

fn running() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

struct Running(String);
impl Drop for Running {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Is a job for this video id running now?
pub fn is_running(id: &str) -> bool {
    running()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(id)
}

/// The job's progress, on the job record (`GET /api/secretary/jobs/{id}` → `stage`).
pub(crate) fn stage(st: &AppState, job_id: &str, stage: &str) {
    tracing::info!(job_id, stage, "youtube report");
    let _ = st.jobs.update(job_id, |j| {
        j.extra.insert("stage".into(), json!(stage));
        j.extra
            .insert("stageAt".into(), json!(lucina_store::timeutil::now_iso()));
    });
}

/// A running job's way back after a restart (lucina-rs is redeployed often): the plan and
/// where it reports, in `<work_dir>/<video id>/resume.json` until the job ends.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Resume {
    pub job_id: String,
    pub plan: Value,
    pub message: String,
    pub session: String,
    pub channel: String,
    pub user_item: Option<String>,
    /// Starts so far (the first included); a job is not resumed a third time.
    pub attempts: u32,
}

pub const MAX_ATTEMPTS: u32 = 3;

fn resume_path(cfg: &VideoConfig, plan: &Value) -> Option<PathBuf> {
    let id = plan["videoId"].as_str()?;
    (trigger::video_id(&format!("https://youtu.be/{id}")).as_deref() == Some(id))
        .then(|| cfg.work_dir.join(id).join("resume.json"))
}

/// Records (or counts another start of) a job; best effort.
pub fn save_resume(cfg: &VideoConfig, r: &Resume) {
    let Some(p) = resume_path(cfg, &r.plan) else {
        return;
    };
    let mut r = r.clone();
    if let Some(old) = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Resume>(&b).ok())
        .filter(|o| o.job_id == r.job_id)
    {
        r.attempts = r.attempts.max(old.attempts);
    }
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        lucina_store::write_atomic(&p, serde_json::to_string_pretty(&r)?.as_bytes())?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!(error = %e, "youtube report: could not save the resume file");
    }
}

/// The job ended (either way): nothing to resume.
pub fn clear_resume(cfg: &VideoConfig, plan: &Value) {
    if let Some(p) = resume_path(cfg, plan) {
        let _ = std::fs::remove_file(p);
    }
}

/// Jobs a restart interrupted, oldest first.
pub fn pending_resumes(cfg: &VideoConfig) -> Vec<Resume> {
    let Ok(dirs) = std::fs::read_dir(&cfg.work_dir) else {
        return Vec::new();
    };
    let mut out: Vec<(std::time::SystemTime, Resume)> = dirs
        .flatten()
        .filter_map(|d| {
            let p = d.path().join("resume.json");
            let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            let r = serde_json::from_slice::<Resume>(&std::fs::read(&p).ok()?).ok()?;
            Some((t, r))
        })
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out.into_iter().map(|(_, r)| r).collect()
}

/// A step's saved result in the archive folder, reused when a resumed job gets there again.
fn reuse<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The plan the chat hands to the job (crate::chat::youtube_turn).
pub fn plan(url: &str, id: &str, request: &str, redo: bool) -> Value {
    json!({"action": ACTION, "url": url, "videoId": id, "request": request, "redo": redo})
}

/// Runs the whole pipeline for `plan` and returns (the chat reply, the action result).
pub async fn run(st: &AppState, plan: &Value, job_id: &str) -> Result<(String, String), String> {
    let cfg = &st.cfg.video;
    let url = plan["url"].as_str().unwrap_or_default().to_owned();
    let id = plan["videoId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| trigger::video_id(&url))
        .ok_or("that is not a YouTube video link")?;
    let request = plan["request"].as_str().unwrap_or_default().to_owned();
    let redo = plan["redo"].as_bool().unwrap_or(false);

    let archive = archive::Archive::new(&cfg.archive_dir);
    if !redo {
        if let Some(done) = archive.find(&id).filter(|e| e.note_slug.is_some()) {
            return Ok(report::already_done(&done));
        }
    }
    {
        let mut r = running().lock().unwrap_or_else(|e| e.into_inner());
        if !r.insert(id.clone()) {
            return Err("a report for this video is already being made".into());
        }
    }
    let _running = Running(id.clone());
    stage(st, job_id, "queued");
    let _permit = gate().acquire().await.map_err(|e| e.to_string())?;

    stage(st, job_id, "reading the video's details");
    let meta = fetch::metadata(cfg, &url)
        .await
        .map_err(|e| format!("yt-dlp: {e:#}"))?;
    if meta.duration > cfg.max_duration.as_secs_f64() {
        return Err(format!(
            "the video is {} long; the limit is {}",
            report::hms(meta.duration),
            report::hms(cfg.max_duration.as_secs_f64())
        ));
    }
    let folder = archive
        .folder_for(&meta, chrono::Utc::now(), st.cfg.time_zone)
        .map_err(|e| format!("archive: {e:#}"))?;
    let work = cfg.work_dir.join(&id);
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| format!("work dir: {e}"))?;
    archive
        .put(archive::Entry::started(&meta, &folder))
        .map_err(|e| format!("archive index: {e:#}"))?;

    stage(st, job_id, "downloading");
    let files = fetch::download(cfg, &url, &meta, &folder)
        .await
        .map_err(|e| format!("download: {e:#}"))?;
    fetch::register_media(&st.tool_env.http, cfg, &meta, &files).await;

    let mut skipped: Vec<Skipped> = Vec::new();
    stage(
        st,
        job_id,
        "transcribing, inspecting the frames, asking ZCode",
    );
    let zcode = async {
        if !cfg.zcode {
            return Err(anyhow::anyhow!("turned off (LUCINA_VIDEO_ZCODE=false)"));
        }
        zcode::run(cfg, &meta, &files.video, &work, &folder).await
    };
    let whisper = async {
        if let Some(t) = reuse::<Value>(&folder.join("transcript.whisper.json")) {
            let segments: Vec<Seg> =
                serde_json::from_value(t["segments"].clone()).unwrap_or_default();
            if !segments.is_empty() {
                return Ok(transcript::Transcript {
                    segments,
                    engine: t["engine"].as_str().unwrap_or_default().to_owned(),
                    language: t["language"].as_str().map(str::to_owned),
                });
            }
        }
        transcript::whisper(cfg, &meta, &files.video, &work).await
    };
    let frames = async {
        match reuse::<Vec<Frame>>(&folder.join("visual.json")) {
            Some(f) if !f.is_empty() => Ok(f),
            _ => visual::inspect(st, cfg, &meta, &files.video, &folder, &work).await,
        }
    };
    let (whisper, frames, zcode) = tokio::join!(whisper, frames, zcode);

    let whisper = whisper.unwrap_or_else(|e| {
        skipped.push(Skipped {
            step: "Whisper".into(),
            reason: format!("{e:#}"),
        });
        transcript::Transcript::default()
    });
    let frames = frames.unwrap_or_else(|e| {
        skipped.push(Skipped {
            step: "visual inspection".into(),
            reason: format!("{e:#}"),
        });
        Vec::new()
    });
    let zcode = zcode.unwrap_or_else(|e| {
        skipped.push(Skipped {
            step: "ZCode".into(),
            reason: format!("{e:#}"),
        });
        ZcodeResult::default()
    });
    let captions = files
        .subtitles
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|v| transcript::parse_vtt(&v))
        .unwrap_or_default();
    if whisper.segments.is_empty() && zcode.transcript.is_empty() && captions.is_empty() {
        skipped.push(Skipped {
            step: "transcript".into(),
            reason: "no speech found by Whisper, ZCode or the captions".into(),
        });
    }

    stage(st, job_id, "merging the transcripts");
    // Saved now so a restart during the merge or the report does not run Whisper again.
    if !whisper.segments.is_empty() {
        let _ = lucina_store::write_atomic(
            &folder.join("transcript.whisper.json"),
            serde_json::to_string_pretty(&json!({"engine": whisper.engine, "language": whisper.language, "segments": whisper.segments}))
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    let merged = merge::merge(
        &st.tool_env.http,
        cfg,
        &meta,
        &whisper.segments,
        &zcode.transcript,
        &captions,
    )
    .await;
    report::write_transcripts(&folder, &meta, &whisper, &zcode, &captions, &merged)
        .map_err(|e| format!("saving the transcript: {e:#}"))?;

    stage(st, job_id, "writing the report");
    let input = report::Input {
        meta: &meta,
        request: &request,
        merged: &merged,
        frames: &frames,
        zcode: &zcode,
        skipped: &skipped,
        folder: &folder,
        whisper_engine: &whisper.engine,
    };
    let rep = report::write(st, cfg, &input)
        .await
        .map_err(|e| format!("report: {e:#}"))?;
    let saved = report::save_note(st, &input, &rep)
        .await
        .map_err(|e| format!("saving the note: {e:#}"))?;
    archive
        .put(archive::Entry::finished(&meta, &folder, &saved.slug))
        .map_err(|e| format!("archive index: {e:#}"))?;
    let _ = tokio::fs::remove_dir_all(&work).await;
    stage(st, job_id, "done");
    Ok(report::chat_reply(&input, &rep, &saved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(work: &Path) -> VideoConfig {
        let w = lucina_memo::transcribe::WhisperConfig::from_lookup(|_| None);
        let mut c = VideoConfig::from_lookup(&|_: &str| None, work, &w);
        c.work_dir = work.to_path_buf();
        c
    }

    #[test]
    fn resume_files_round_trip_and_count_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let r = Resume {
            job_id: "job-1".into(),
            plan: plan(
                "https://youtu.be/FBs--2nSPLQ",
                "FBs--2nSPLQ",
                "report",
                false,
            ),
            message: "report https://youtu.be/FBs--2nSPLQ".into(),
            session: "secretary_app".into(),
            channel: "ios".into(),
            user_item: Some("01M".into()),
            attempts: 2,
        };
        save_resume(&cfg, &r);
        // A later save of the same job never lowers the count.
        save_resume(
            &cfg,
            &Resume {
                attempts: 1,
                ..r.clone()
            },
        );
        assert_eq!(pending_resumes(&cfg), vec![r.clone()]);
        clear_resume(&cfg, &r.plan);
        assert!(pending_resumes(&cfg).is_empty());
        // A plan without a valid id never names a path.
        save_resume(
            &cfg,
            &Resume {
                plan: json!({"videoId": "../../etc"}),
                ..r
            },
        );
        assert!(pending_resumes(&cfg).is_empty());
    }

    #[test]
    fn gemini_defaults_to_the_stable_model() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        assert_eq!(
            (c.gemini_model.as_str(), c.gemini_fallback_model.as_str()),
            ("gemini-3.5-flash", "gemini-3.8-flash")
        );
    }
}

//! Lucina Notebooks (`crates/lucina-notebooks`, `docs/design/notebooks-api.md`) inside the Rust
//! Lucina: `/api/secretary/notebooks/<rest>` goes to the crate's router at `/<rest>` after the
//! Secretary bearer check, and the crate gets Lucina's own pieces:
//!
//! - ingest: web pages and PDF links through the share pipeline's SSRF-safe fetch
//!   (`share::extract`), YouTube through the video pipeline (yt-dlp captions, else local
//!   Whisper; never Gemini or ZCode for a notebook), audio and video files through Whisper,
//!   Office files and OCR from `share::extract`, files on Lucina's computer with `docker cp`;
//! - models: DeepSeek with Lucina's key (`cfg.deepseek`) and the local Qwen gateway;
//! - embeddings: `lucina-notebooks-embed.service` (EmbeddingGemma 2 on the CPU, :8897);
//! - notes: secretary-server's Keep store (`LUCINA_SECRETARY_SERVER_URL`, service token);
//! - speech: kokoro (:18880).
//!
//! Env: `LUCINA_NOTEBOOKS` (`off` disables), `LUCINA_NOTEBOOKS_DIR` (default
//! `<data dir>/notebooks`), `LUCINA_NOTEBOOKS_EMBED_URL`, `LUCINA_NOTEBOOKS_TTS_URL`,
//! `LUCINA_NOTEBOOKS_QWEN_URL`, `LUCINA_NOTEBOOKS_QWEN_MODEL`, `LUCINA_NOTEBOOKS_QWEN_KEY_FILE`,
//! `LUCINA_NOTEBOOKS_SANDBOX` (container, default `lucina-workspace`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, Uri};
use axum::response::{IntoResponse, Response};
use lucina_llm::{DeepSeek, DeepSeekConfig, KeySource};
use lucina_notebooks::chunk::Part;
use lucina_notebooks::embed::HttpEmbedder;
use lucina_notebooks::ingest::{self, Extracted, Extractor};
use lucina_notebooks::keep::HttpKeep;
use lucina_notebooks::llm::Clients;
use lucina_notebooks::podcast::Kokoro;
use lucina_notebooks::{Config as NbConfig, Deps, Notebooks};
use tower::ServiceExt;

use crate::AppState;

const PREFIX: &str = "/api/secretary/notebooks";
const SANDBOX_MAX: u64 = 200 * 1024 * 1024;

static NOTEBOOKS: OnceLock<Option<Notebooks>> = OnceLock::new();

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The local Qwen gateway's bearer: a key file, or `QWEN_OPENAI_API_KEY` in `~/.config/qwen-openai.env`.
fn qwen_key() -> Option<String> {
    if let Some(f) = env("LUCINA_NOTEBOOKS_QWEN_KEY_FILE") {
        return std::fs::read_to_string(f).ok().map(|s| s.trim().to_owned());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let text = std::fs::read_to_string(home.join(".config/qwen-openai.env")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("QWEN_OPENAI_API_KEY="))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

fn build(st: &AppState) -> anyhow::Result<Option<Notebooks>> {
    if env("LUCINA_NOTEBOOKS").as_deref() == Some("off") {
        return Ok(None);
    }
    let dir = env("LUCINA_NOTEBOOKS_DIR").map(PathBuf::from).unwrap_or_else(|| st.cfg.data_dir.join("notebooks"));
    let mut cfg = NbConfig::new(dir);
    cfg.ffmpeg = st.cfg.video.ffmpeg.clone();
    let deepseek = DeepSeek::new(st.cfg.deepseek.clone())?;
    let qwen = match qwen_key() {
        Some(key) => Some(DeepSeek::new(DeepSeekConfig {
            base_url: env("LUCINA_NOTEBOOKS_QWEN_URL").unwrap_or_else(|| "http://127.0.0.1:11436/v1".into()),
            model: env("LUCINA_NOTEBOOKS_QWEN_MODEL").unwrap_or_else(|| "qwen3.8:27b-128k".into()),
            key: KeySource::Inline(key),
            // A cold start loads the model first.
            timeout: Duration::from_secs(600),
            ..DeepSeekConfig::default()
        })?),
        None => None,
    };
    let keep_url = env("LUCINA_SECRETARY_SERVER_URL").unwrap_or_else(|| "https://getsecretary.tech".into());
    let keep_token = env("LUCINA_SECRETARY_SERVICE_TOKEN_FILE").unwrap_or_default();
    let deps = Deps {
        extractor: Arc::new(LucinaExtractor { st: st.clone() }),
        embedder: Some(Arc::new(HttpEmbedder::new(
            env("LUCINA_NOTEBOOKS_EMBED_URL").unwrap_or_else(|| "http://127.0.0.1:8897/v1/embeddings".into()),
            "embeddinggemma-2",
        ))),
        llm: Arc::new(Clients { deepseek, qwen }),
        keep: Arc::new(HttpKeep::new(keep_url, keep_token)),
        tts: Arc::new(Kokoro::new(env("LUCINA_NOTEBOOKS_TTS_URL").unwrap_or_else(|| "http://127.0.0.1:18880".into()))),
    };
    Ok(Some(Notebooks::open(cfg, deps)?))
}

fn get(st: &AppState) -> Option<&'static Notebooks> {
    NOTEBOOKS
        .get_or_init(|| match build(st) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = %e, "notebooks: could not start");
                None
            }
        })
        .as_ref()
}

/// At startup: open the store and start the workers (they resume unfinished work).
pub fn start(st: &AppState) {
    if let Some(nb) = get(st) {
        nb.start();
        tracing::info!("notebooks: ready");
    }
}

/// `/api/secretary/notebooks/<rest>` → the crate's router.
pub async fn route(st: &AppState, method: &Method, uri: &Uri, headers: &HeaderMap, body: Body) -> Response {
    let Some(nb) = get(st) else {
        return crate::api::ApiError::new(503, "unavailable", "Notebooks are not available").into_response();
    };
    let rest = uri.path().strip_prefix(PREFIX).unwrap_or("");
    let path = if rest.is_empty() { "/" } else { rest };
    let pq = match uri.query() {
        Some(q) => format!("{path}?{q}"),
        None => path.to_owned(),
    };
    let mut req = Request::builder().method(method.clone()).uri(pq);
    for (k, v) in headers {
        if k != axum::http::header::AUTHORIZATION && k != axum::http::header::COOKIE {
            req = req.header(k, v);
        }
    }
    let Ok(req) = req.body(body) else {
        return crate::api::ApiError::new(400, "bad_request", "bad path").into_response();
    };
    match lucina_notebooks::api::router(nb.clone()).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    }
}

struct LucinaExtractor {
    st: AppState,
}

const AV_EXT: [&str; 12] = ["mp3", "m4a", "wav", "ogg", "opus", "flac", "aac", "mp4", "mkv", "mov", "webm", "m4v"];
const OFFICE_EXT: [&str; 7] = ["docx", "pptx", "xlsx", "odt", "odp", "ods", "epub"];
const IMAGE_EXT: [&str; 5] = ["png", "jpg", "jpeg", "webp", "tiff"];

impl LucinaExtractor {
    async fn youtube(&self, url: &str, work: &Path) -> anyhow::Result<Extracted> {
        let cfg = &self.st.cfg.video;
        let meta = crate::video::fetch::metadata(cfg, url).await?;
        if meta.duration > cfg.max_duration.as_secs_f64() {
            anyhow::bail!("the video is longer than {} minutes", cfg.max_duration.as_secs() / 60);
        }
        let files = crate::video::fetch::download(cfg, url, &meta, work).await?;
        let mut segs = Vec::new();
        if let Some(vtt) = &files.subtitles {
            segs = crate::video::transcript::parse_vtt(&tokio::fs::read_to_string(vtt).await.unwrap_or_default());
        }
        if segs.iter().map(|s| s.text.len()).sum::<usize>() < 200 {
            segs = crate::video::transcript::whisper(cfg, &meta, &files.video, work).await?.segments;
        }
        let mut parts = vec![Part {
            text: format!("{}\n{}\n\n{}", meta.title, meta.channel, meta.description.chars().take(3000).collect::<String>()),
            ..Default::default()
        }];
        parts.extend(segs.into_iter().map(|s| Part { text: s.text, page: None, start_s: Some(s.start) }));
        let _ = tokio::fs::remove_file(&files.video).await;
        Ok(Extracted {
            title: Some(meta.title.clone()),
            kind: Some("youtube".into()),
            parts,
            duration_s: Some(meta.duration),
            ..Default::default()
        })
    }

    async fn whisper(&self, path: &Path, work: &Path) -> anyhow::Result<Extracted> {
        let cfg = &self.st.cfg.video;
        let wav = work.join("audio.wav");
        let out = tokio::process::Command::new("nice")
            .args(["-n", "10"])
            .arg(&cfg.ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(path)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .kill_on_drop(true)
            .output()
            .await?;
        if !out.status.success() {
            anyhow::bail!("ffmpeg could not read the audio");
        }
        let secs = tokio::fs::metadata(&wav).await?.len() as f64 / 32_000.0;
        let dir = work.join("whisper");
        tokio::fs::create_dir_all(&dir).await?;
        let t = lucina_memo::transcribe::transcribe_prompted(&cfg.whisper, &wav, &dir, secs, cfg.whisper.gpu_wait, "")
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let _ = tokio::fs::remove_file(&wav).await;
        Ok(Extracted {
            kind: Some("audio".into()),
            parts: t
                .segments
                .into_iter()
                .map(|s| Part { text: s.text.trim().to_owned(), page: None, start_s: Some(s.start) })
                .filter(|p| !p.text.is_empty())
                .collect(),
            duration_s: Some(secs),
            ..Default::default()
        })
    }
}

#[async_trait::async_trait]
impl Extractor for LucinaExtractor {
    async fn url(&self, url: &str, work: &Path) -> anyhow::Result<Extracted> {
        if ingest::youtube_id(url).is_some() {
            return self.youtube(url, work).await;
        }
        let (bytes, mime) = crate::share::extract::download(&self.st, url).await?;
        if mime.starts_with("application/pdf") || bytes.starts_with(b"%PDF") {
            let name = url
                .rsplit('/')
                .next()
                .filter(|n| n.to_ascii_lowercase().ends_with(".pdf"))
                .map(|n| n.split('?').next().unwrap_or(n).to_owned())
                .unwrap_or_else(|| "document.pdf".into());
            let path = work.join(&name);
            tokio::fs::write(&path, &bytes).await?;
            let mut x = self.file(&path, &name, work).await?;
            x.file = Some(path);
            return Ok(x);
        }
        let (title, text) = crate::share::extract::html_text(&String::from_utf8_lossy(&bytes));
        Ok(Extracted {
            title: Some(title).filter(|t| !t.is_empty()),
            kind: Some("web".into()),
            parts: vec![Part { text, ..Default::default() }],
            ..Default::default()
        })
    }

    async fn file(&self, path: &Path, name: &str, work: &Path) -> anyhow::Result<Extracted> {
        if let Some(mut x) = ingest::basic_file(path, name).await? {
            // A scanned PDF has no text layer: OCR what the share pipeline can.
            if x.kind.as_deref() == Some("pdf") && x.text().chars().count() < 200 {
                let ocr = crate::share::extract::pdf(&self.st, path).await?;
                if ocr.text.trim().chars().count() > x.text().chars().count() {
                    x.parts = vec![Part { text: ocr.text, page: Some(1), start_s: None }];
                }
            }
            return Ok(x);
        }
        let ext = ingest::extension(name);
        if AV_EXT.contains(&ext.as_str()) {
            return self.whisper(path, work).await;
        }
        if OFFICE_EXT.contains(&ext.as_str()) {
            let p = path.to_owned();
            let text = tokio::task::spawn_blocking(move || crate::share::extract::document(&p)).await??;
            return Ok(Extracted { kind: Some("file".into()), parts: vec![Part { text, ..Default::default() }], ..Default::default() });
        }
        if IMAGE_EXT.contains(&ext.as_str()) {
            let text = crate::share::extract::ocr(path).await?;
            return Ok(Extracted { kind: Some("file".into()), parts: vec![Part { text, ..Default::default() }], ..Default::default() });
        }
        anyhow::bail!("Notebooks can't read .{ext} files yet")
    }

    async fn sandbox(&self, path: &str, dest_dir: &Path) -> anyhow::Result<PathBuf> {
        if !path.starts_with("/home/cua/") || path.split('/').any(|p| p == "..") {
            anyhow::bail!("only files under /home/cua/ on Lucina's computer");
        }
        let container = env("LUCINA_NOTEBOOKS_SANDBOX").unwrap_or_else(|| "lucina-workspace".into());
        let size = tokio::process::Command::new("docker")
            .args(["exec", &container, "stat", "-c", "%s %F", path])
            .kill_on_drop(true)
            .output()
            .await?;
        let info = String::from_utf8_lossy(&size.stdout).trim().to_owned();
        let (bytes, kind) = info.split_once(' ').unwrap_or(("", ""));
        if !size.status.success() || kind != "regular file" {
            anyhow::bail!("no such file on Lucina's computer: {path}");
        }
        if bytes.parse::<u64>().unwrap_or(u64::MAX) > SANDBOX_MAX {
            anyhow::bail!("the file is larger than 200 MB");
        }
        tokio::fs::create_dir_all(dest_dir).await?;
        let name = path.rsplit('/').next().unwrap_or("file");
        let dest = dest_dir.join(name);
        let out = tokio::time::timeout(
            Duration::from_secs(300),
            tokio::process::Command::new("docker")
                .arg("cp")
                .arg(format!("{container}:{path}"))
                .arg(&dest)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        if !out.status.success() {
            anyhow::bail!("copying from Lucina's computer failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(dest)
    }
}

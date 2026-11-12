//! Durable, asynchronous owner shares (`POST /v1/share`, docs/share.md) and
//! the per-file-type handling behind them (docs/attachments.md). The gateway
//! authenticates the device; this service authenticates the gateway before
//! reading a request body.
pub(crate) mod extract;
pub mod guidance;
pub mod handlers;
pub mod library;
pub mod memory;
pub mod router;
mod structured;
pub mod tools;
pub(crate) mod vision;
pub use vision::stop_orphans;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use lucina_store::timeline::{Attachment, Meta, NewItem, TimelineItem};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore};

use crate::AppState;
pub use guidance::SHARE_SYSTEM;
use handlers::Extracted;

pub const MAX_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_PARTS: usize = 32;
/// Characters of extracted text all parts may show the model.
const TURN_TEXT_BUDGET: usize = 30_000;
/// Characters one part may show the model.
const PART_TEXT_MAX: usize = 12_000;
/// Longest read of one file that is not audio or video: above `vision_budget` (360 s by default), so an
/// image whose vision is slow still comes back with its OCR text.
const READ_LIMIT_SECS: u64 = 420;
/// Lucina's reply to a voice note whose recording was silent (peak under -60 dBFS, or no frame above the noise gate), English
/// and Danish; never Italian. The same text as the voice socket's `silent` error.
pub const SILENT_REPLY: &str = "The recording was silent: check the microphone.";
pub const SILENT_REPLY_DA: &str = "Optagelsen var tavs: tjek mikrofonen.";

#[derive(Clone, Debug)]
pub struct ShareConfig {
    pub key_file: Option<PathBuf>,
    pub dir: PathBuf,
    pub media_url: String,
    pub vision_bin: PathBuf,
    pub vision_model: PathBuf,
    pub vision_mmproj: PathBuf,
    /// An already managed OpenAI-compatible vision endpoint (also useful in tests).
    pub vision_url: Option<String>,
    /// How long one image may wait for vision (lease, cold load, the answer) before the read goes on with OCR.
    pub vision_budget: Duration,
    pub whisper: lucina_memo::transcribe::WhisperConfig,
    /// Whisper for short clips (voice notes and chat voice, at most `whisper_short_secs`): a small model with a
    /// small GPU lease that fits beside Qwen, a short wait for it, and the same small model on the CPU after.
    /// `None` (no `LUCINA_WHISPER_SHORT_MODEL`): short clips use `whisper` like everything else.
    pub whisper_short: Option<lucina_memo::transcribe::WhisperConfig>,
    pub whisper_short_secs: f64,
    /// Only compiled into debug/test builds; never populated from the environment.
    #[cfg(debug_assertions)]
    pub test_allow_loopback: bool,
}
impl ShareConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let home = get("HOME")
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_else(|| "/home/nez".into());
        let data_dir = get("LUCINA_DATA_DIR").unwrap_or_else(|| "data".into());
        let path = |k, d: String| PathBuf::from(get(k).unwrap_or(d));
        let mut whisper = lucina_memo::transcribe::WhisperConfig::from_lookup(|k| match k {
            "HOME" => Some(home.clone()),
            "LUCINA_MEMO_WHISPER_CLI" => get("LUCINA_WHISPER_BIN"),
            "LUCINA_MEMO_WHISPER_MODEL" => get("LUCINA_WHISPER_MODEL"),
            // The CPU fallback: lucina-memo's default file (large-v3-turbo-q8_0) is not on every host, and a missing
            // one means no fallback at all: after the GPU wait the share got no transcript.
            "LUCINA_MEMO_WHISPER_CPU_MODEL" => get("LUCINA_WHISPER_CPU_MODEL"),
            _ => None,
        });
        whisper.timeout = Duration::from_secs(14 * 60);
        whisper.gpu_wait = Duration::from_secs(
            get("LUCINA_WHISPER_GPU_WAIT_SEC")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(90),
        );
        let whisper_short = get("LUCINA_WHISPER_SHORT_MODEL")
            .filter(|m| !m.trim().is_empty())
            .map(|model| {
                let mut w = lucina_memo::transcribe::WhisperConfig::from_lookup(|k| match k {
                    "HOME" => Some(home.clone()),
                    "LUCINA_MEMO_WHISPER_CLI" => get("LUCINA_WHISPER_BIN"),
                    "LUCINA_MEMO_WHISPER_MODEL" => Some(model.clone()),
                    "LUCINA_MEMO_WHISPER_CPU_MODEL" => Some(model.clone()),
                    "LUCINA_MEMO_GPU_MB" => {
                        get("LUCINA_WHISPER_SHORT_GPU_MB").or_else(|| Some("1500".into()))
                    }
                    _ => None,
                });
                w.timeout = Duration::from_secs(3 * 60);
                w.gpu_wait = Duration::from_secs(
                    get("LUCINA_WHISPER_SHORT_GPU_WAIT_SEC")
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(3),
                );
                w
            });
        let whisper_short_secs = get("LUCINA_WHISPER_SHORT_SECS")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(60.0);
        Self {
            key_file: get("LUCINA_SHARE_GATEWAY_KEY_FILE").map(PathBuf::from),
            dir: path("LUCINA_SHARE_DIR", format!("{data_dir}/share")),
            media_url: get("LUCINA_MEDIA_INTERNAL_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8830".into()),
            vision_bin: path(
                "LUCINA_VISION_SERVER_BIN",
                format!("{home}/llama.cpp/build/bin/llama-server"),
            ),
            vision_model: path(
                "LUCINA_VISION_MODEL",
                format!("{home}/models/gemma-4-12b-it-qat/gemma-4-12b-it-qat-q4_0.gguf"),
            ),
            vision_mmproj: path(
                "LUCINA_VISION_MMPROJ",
                format!("{home}/models/gemma-4-12b-it-qat/mmproj-gemma-4-12b-it-qat-q4_0.gguf"),
            ),
            vision_url: get("LUCINA_VISION_URL"),
            vision_budget: Duration::from_secs(
                get("LUCINA_VISION_BUDGET_SECS")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(360),
            ),
            whisper,
            whisper_short,
            whisper_short_secs,
            #[cfg(debug_assertions)]
            test_allow_loopback: false,
        }
    }
}
pub struct ShareRuntime {
    ingest: Mutex<()>,
    /// [`store_again`] is running.
    storing_again: AtomicBool,
    /// Shares with a processing task right now (a `processing` share outside
    /// this set has lost its worker and is respawned on the next request).
    live: std::sync::Mutex<std::collections::HashSet<String>>,
    worker: Semaphore,
    recovered: AtomicBool,
    vision: Mutex<Option<vision::Server>>,
    /// One start of the vision server at a time; `vision` itself is only held for a moment.
    vision_start: Mutex<()>,
    /// What Lucina read from every share (opened on first use).
    pub memory: Arc<memory::Memory>,
}
impl ShareRuntime {
    pub fn new(cfg: &ShareConfig) -> Self {
        Self {
            ingest: Mutex::new(()),
            storing_again: AtomicBool::new(false),
            live: std::sync::Mutex::new(std::collections::HashSet::new()),
            worker: Semaphore::new(1),
            recovered: AtomicBool::new(false),
            vision: Mutex::new(None),
            vision_start: Mutex::new(()),
            memory: Arc::new(memory::Memory::new(cfg.dir.join("attachments.db"))),
        }
    }
}

#[derive(Debug)]
struct Error(u16, String);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.0).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"ok":false,"error":self.1})),
        )
            .into_response()
    }
}
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Self(500, e.to_string())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(500, e.to_string())
    }
}
fn bad(s: impl Into<String>) -> Error {
    Error(400, s.into())
}
fn multipart_error(e: axum::extract::multipart::MultipartError) -> Error {
    Error(e.status().as_u16(), e.body_text())
}
async fn authenticate(st: &AppState, headers: &HeaderMap) -> Result<(), Error> {
    let Some(path) = &st.cfg.share.key_file else {
        return Err(Error(503, "share is disabled".into()));
    };
    let key = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| Error(503, "share gateway key unavailable".into()))?;
    let key = key.trim();
    let supplied = headers
        .get("x-share-gateway")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    // Hash first so the comparison always has equal lengths.
    use sha2::{Digest, Sha256};
    if key.is_empty()
        || !bool::from(Sha256::digest(key.as_bytes()).ct_eq(&Sha256::digest(supplied.as_bytes())))
    {
        return Err(Error(401, "unauthorized".into()));
    }
    Ok(())
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/share",
            post(receive).layer(DefaultBodyLimit::max(MAX_BYTES)),
        )
        .route("/v1/share/{id}", get(status))
}

/// The optional `meta` form field (docs/share.md).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileMeta {
    index: usize,
    #[serde(default)]
    voice: bool,
    #[serde(default)]
    waveform: Option<String>,
    #[serde(default)]
    transcript: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    taken_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct LocationMeta {
    lat: f64,
    lon: f64,
    #[serde(default)]
    name: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ShareMeta {
    #[serde(default)]
    files: Vec<FileMeta>,
    #[serde(default)]
    location: Option<LocationMeta>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    attachment: Attachment,
    path: Option<PathBuf>,
    text: Option<String>,
    /// Index among the request's `file` parts (for `meta.files`).
    #[serde(default)]
    file_index: Option<usize>,
    /// `pending` → `reading` → `read` | `failed`.
    #[serde(default = "pending")]
    state: String,
    /// Handler kind once read (`sheet`, `calendar`, …).
    #[serde(default)]
    handler: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}
fn pending() -> String {
    "pending".into()
}
impl Part {
    fn new(attachment: Attachment) -> Self {
        Self {
            attachment,
            path: None,
            text: None,
            file_index: None,
            state: pending(),
            handler: None,
            summary: None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    share_id: String,
    status: String,
    /// `received` → `reading` → `thinking` → `done` | `error`.
    #[serde(default)]
    stage: String,
    user_item: TimelineItem,
    parts: Vec<Part>,
    intent: String,
    note: String,
    lang: String,
    session: String,
    source: String,
    /// The device that posted it (gateway header); kept out of the public status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_device: Option<String>,
    #[serde(default)]
    meta: ShareMeta,
    lucina_item_id: Option<String>,
    reply: Option<String>,
    error: Option<String>,
}
impl Job {
    fn public(&self, duplicate: bool) -> Value {
        let attachments: Vec<Value> = self
            .parts
            .iter()
            .enumerate()
            .map(|(index, p)| {
                let mut a = serde_json::to_value(&p.attachment).unwrap_or(Value::Null);
                a["index"] = json!(index);
                a["state"] = json!(p.state);
                if let Some(s) = &p.summary {
                    a["summary"] = json!(s);
                }
                a
            })
            .collect();
        let mut v = json!({"ok":true,"shareId":self.share_id,"duplicate":duplicate,"status":self.status,
            "stage":self.stage,"userItemId":self.user_item.id,"attachments":attachments});
        for (key, value) in [
            ("lucinaItemId", &self.lucina_item_id),
            ("reply", &self.reply),
            ("error", &self.error),
        ] {
            if let Some(s) = value {
                v[key] = json!(s);
            }
        }
        v
    }
}
async fn save(dir: &Path, job: &Job) -> anyhow::Result<()> {
    let tmp = dir.join("status.tmp");
    let mut f = tokio::fs::File::create(&tmp).await?;
    f.write_all(&serde_json::to_vec(job)?).await?;
    f.sync_all().await?;
    tokio::fs::rename(tmp, dir.join("status.json")).await?;
    // Persist directory entries as well as file contents before acknowledging.
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}
async fn find(root: &Path, id: &str) -> Option<(PathBuf, Job)> {
    let mut months = tokio::fs::read_dir(root).await.ok()?;
    while let Ok(Some(month)) = months.next_entry().await {
        let dir = month.path().join(id);
        if let Ok(bytes) = tokio::fs::read(dir.join("status.json")).await {
            if let Ok(job) = serde_json::from_slice(&bytes) {
                return Some((dir, job));
            }
        }
    }
    None
}
fn valid_id(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn valid_session(s: &str) -> bool {
    s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
async fn status(
    State(st): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    authenticate(&st, &headers).await?;
    if !valid_id(&id) {
        return Err(bad("invalid shareId"));
    }
    let (_, job) = find(&st.cfg.share.dir, &id)
        .await
        .ok_or_else(|| Error(404, "share not found".into()))?;
    Ok(Json(job.public(false)))
}

pub fn sanitize(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut s = s.replace("..", "_").trim_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return "file".into();
    }
    if s.len() > 120 {
        let ext = Path::new(&s)
            .extension()
            .and_then(|s| s.to_str())
            .filter(|e| e.len() <= 20)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        s = clip(&s, 120 - ext.len());
        while s.len() > 120 - ext.len() {
            s.pop();
        }
        // A cut right after a '.' plus ".ext" would make ".." (Verus SK4.4).
        while s.ends_with('.') {
            s.pop();
        }
        s.push_str(&ext);
    }
    s
}
pub(super) fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn intent(s: &str) -> bool {
    ["ask", "library", "shopping", "note", "remind"].contains(&s)
}
/// The language replies, summaries and fixed texts of a share use: that of the owner's own note, else of
/// his recent messages, else English. The device locale (`lang` on the request) is never used: on
/// 2026-10-26/27 a Danish device made the app's default line, the summaries and Lucina's replies Danish for
/// Italian notes and bare links.
fn owner_language(st: &AppState, session: &str, note: &str) -> lucina_intent::lang::Lang {
    crate::planner::resolve_reply_language(
        note,
        &crate::planner::recent_owner_texts(&st.recent, session),
    )
}
fn check_parts(count: usize) -> Result<(), Error> {
    if count > MAX_PARTS {
        Err(bad("at most 32 parts"))
    } else {
        Ok(())
    }
}
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn receive(State(st): State<AppState>, req: Request) -> Result<Response, Error> {
    authenticate(&st, req.headers()).await?;
    // From the gateway (Caddy copies them from secretary-server's device check): who asked, and
    // whether that device is a test client. `authenticate` proved the request came through it.
    let from_device = req
        .headers()
        .get("x-secretary-device")
        .and_then(|h| h.to_str().ok())
        .map(str::trim)
        .filter(|d| lucina_store::timeline::valid_device(d))
        .map(str::to_owned);
    let test_device = req
        .headers()
        .get("x-secretary-test")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v.trim() == "1");
    if req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(Error(413, "share exceeds 2 GB".into()));
    }
    let mut multipart = Multipart::from_request(req, &st)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let root = &st.cfg.share.dir;
    tokio::fs::create_dir_all(root).await?;
    let canonical_root = tokio::fs::canonicalize(root).await?;
    let root = &canonical_root;
    let staging = tempfile::Builder::new()
        .prefix(".ingest-")
        .tempdir_in(root)?;
    let mut fields = HashMap::<String, String>::new();
    let mut parts = Vec::new();
    let mut count = 0;
    let mut files = 0;
    let mut total = 0usize;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        count += 1;
        check_parts(count)?;
        let name = field.name().unwrap_or("").to_owned();
        if name == "file" {
            let filename = sanitize(field.file_name().unwrap_or("file"));
            let mime = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_owned();
            let path = staging.path().join(format!("{count:02}-{filename}"));
            let mut file = tokio::fs::File::create(&path).await?;
            let mut size = 0;
            while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                total = total.saturating_add(chunk.len());
                if total > MAX_BYTES {
                    return Err(Error(413, "share exceeds 2 GB".into()));
                }
                size += chunk.len() as u64;
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            let mut part = Part::new(Attachment {
                kind: "file".into(),
                name: Some(filename),
                mime: Some(mime),
                bytes: Some(size),
                ..Attachment::default()
            });
            part.path = Some(path);
            part.file_index = Some(files);
            files += 1;
            parts.push(part);
        } else {
            if ![
                "shareId",
                "intent",
                "note",
                "lang",
                "sessionId",
                "channel",
                "source",
                "title",
                "text",
                "url",
                "meta",
                "replyTo",
                "conversation",
            ]
            .contains(&name.as_str())
            {
                return Err(bad("unknown form field"));
            }
            let mut bytes = Vec::new();
            let cap = match name.as_str() {
                "text" => 3 * 1024 * 1024,
                "meta" => 64 * 1024,
                _ => 16 * 1024,
            };
            while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                total = total.saturating_add(chunk.len());
                if total > MAX_BYTES {
                    return Err(Error(413, "share exceeds 2 GB".into()));
                }
                if bytes.len() + chunk.len() > cap {
                    return Err(bad(format!("{name} is too long")));
                }
                bytes.extend_from_slice(&chunk);
            }
            let value = String::from_utf8(bytes).map_err(|_| bad("form fields must be UTF-8"))?;
            if name == "text" || name == "url" {
                if value.trim().is_empty() {
                    continue;
                }
                if name == "url" && value.len() > 8192 {
                    return Err(bad("url is too long"));
                }
                let location = name == "url" && extract::is_location(&value);
                if name == "url" && !location {
                    let u = reqwest::Url::parse(&value).map_err(|_| bad("invalid url"))?;
                    if !["http", "https"].contains(&u.scheme()) {
                        return Err(bad("url must use http or https"));
                    }
                }
                let mut part = Part::new(Attachment {
                    kind: if name == "text" {
                        "text"
                    } else if location {
                        "location"
                    } else {
                        "link"
                    }
                    .into(),
                    name: Some(clip(&value, 80)),
                    url: (name == "url").then(|| value.clone()),
                    ..Attachment::default()
                });
                part.text = (name == "text").then_some(value);
                parts.push(part);
            } else if fields.insert(name.clone(), value).is_some() {
                return Err(bad(format!("{name} must not repeat")));
            }
        }
    }
    let meta: ShareMeta = match fields.get("meta") {
        Some(raw) => serde_json::from_str(raw).map_err(|e| bad(format!("invalid meta: {e}")))?,
        None => ShareMeta::default(),
    };
    if let Some(loc) = &meta.location {
        if !(-90.0..=90.0).contains(&loc.lat) || !(-180.0..=180.0).contains(&loc.lon) {
            return Err(bad("invalid meta.location"));
        }
        // Defence in depth: `meta` itself was one of the counted fields, so this
        // never fires today (Verus SK4.1), but keeps the bound if fields change.
        check_parts(parts.len() + 1)?;
        let label = loc
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("{:.5}, {:.5}", loc.lat, loc.lon));
        let mut part = Part::new(Attachment {
            kind: "location".into(),
            name: Some(clip(&label, 200)),
            url: Some(format!(
                "geo:{:.6},{:.6}?q={:.6},{:.6}({})",
                loc.lat,
                loc.lon,
                loc.lat,
                loc.lon,
                label.replace(['(', ')', '&', '?'], " ")
            )),
            ..Attachment::default()
        });
        part.state = "read".into();
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(bad("at least one text, url or file is required"));
    }
    let field = |k: &str, d: &str| fields.get(k).cloned().unwrap_or_else(|| d.into());
    let id = field("shareId", &uuid::Uuid::new_v4().to_string());
    if !valid_id(&id) {
        return Err(bad("invalid shareId"));
    }
    let action = field("intent", "ask");
    if !intent(&action) {
        return Err(bad("invalid intent"));
    }
    let channel = field("channel", "web");
    if !secretary_core::channel::APP_CHANNELS.contains(&channel.as_str()) {
        return Err(bad("channel must be web, ios, dsi or mac"));
    }
    let session = field("sessionId", "");
    if !valid_session(&session) {
        return Err(bad("invalid sessionId"));
    }
    // The firmware that predates the `dsi` channel says `web`; its source and session id say DSi.
    let channel = secretary_core::channel::client_channel(
        &channel,
        Some(field("source", "").trim()),
        Some(session.as_str()),
        None,
    );
    // Test traffic (a test device, `conversation=selftest`, a `probe…`/`test…` source) runs in the
    // self-test conversation: never the owner's chat, never a push (docs/share.md).
    let selftest = test_device
        || crate::selftest::requested(
            &json!({"conversation": field("conversation", ""), "source": field("source", "")}),
            &session,
            "",
        );
    let session = if selftest {
        crate::selftest::SESSION.to_owned()
    } else {
        session
    };
    // Swipe to reply: the earlier timeline item this share answers.
    let reply_to = field("replyTo", "").trim().to_owned();
    if !reply_to.is_empty() && !crate::chat::reply::valid_id(&reply_to) {
        return Err(bad("invalid replyTo"));
    }
    // An id that names no stored item is ignored (the share is still taken).
    let reply_to = (!reply_to.is_empty()).then_some(reply_to).filter(|id| {
        match st.timeline_for(&session).get(id) {
            Ok(Some(_)) => true,
            _ => {
                tracing::debug!(reply_to = %id, "share replyTo names no timeline item; ignored");
                false
            }
        }
    });
    for (key, max) in [
        ("note", 4000),
        ("lang", 32),
        ("source", 120),
        ("title", 1000),
    ] {
        if field(key, "").chars().count() > max {
            return Err(bad(format!("{key} is too long")));
        }
    }
    let _lock = st.share.ingest.lock().await;
    if let Some((dir, job)) = find(root, &id).await {
        let response = job.public(true);
        // A share accepted earlier whose worker is gone (an error or panic
        // after status.json): the retry restarts it (Stateright SH6).
        if job.status == "processing" && !is_live(&st, &job.share_id) {
            spawn(st.clone(), dir, job);
        }
        return Ok((StatusCode::ACCEPTED, Json(response)).into_response());
    }
    let month = root.join(Utc::now().format("%Y-%m").to_string());
    tokio::fs::create_dir_all(&month).await?;
    let dir = month.join(&id);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    tokio::fs::rename(staging.path(), &dir).await?;
    std::fs::File::open(&month)?.sync_all()?;
    let note = field("note", "");
    let source = field("source", "share")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let source = source.trim_matches('.');
    let source = if source.is_empty() { "share" } else { source }.to_owned();
    tracing::info!(share_id = %id, %source, intent = %action, parts = parts.len(), "owner share received");
    let title = field("title", "");
    for (i, part) in parts.iter_mut().enumerate() {
        if let Some(path) = &mut part.path {
            let name = path
                .file_name()
                .context("staged file has a name")?
                .to_owned();
            *path = tokio::fs::canonicalize(dir.join(name)).await?;
            let (p, n, m) = (
                path.clone(),
                part.attachment.name.clone().unwrap_or_default(),
                part.attachment.mime.clone().unwrap_or_default(),
            );
            let voice = part
                .file_index
                .and_then(|fi| meta.files.iter().find(|f| f.index == fi))
                .is_some_and(|f| f.voice);
            let routed = tokio::task::spawn_blocking(move || router::route_for(&p, &n, &m, voice))
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            part.attachment.kind = router::timeline_kind(routed).into();
            part.handler = Some(routed.into());
            if let Some(fm) = part
                .file_index
                .and_then(|fi| meta.files.iter().find(|f| f.index == fi))
            {
                part.attachment.width = fm.width;
                part.attachment.height = fm.height;
                part.attachment.duration_ms = fm.duration_ms;
                part.attachment.kind = router::timeline_kind_for(routed, fm.voice).into();
                if part.attachment.kind == "voice" {
                    part.attachment.waveform = fm
                        .waveform
                        .as_deref()
                        .and_then(lucina_store::timeline::valid_waveform);
                    part.attachment.transcript = fm
                        .transcript
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| clip(s, 4000));
                }
            }
            let taken_at = part
                .file_index
                .and_then(|fi| meta.files.iter().find(|f| f.index == fi))
                .and_then(|f| f.taken_at.clone());
            store_in_library(&st, &id, i, &source, &note, part, taken_at).await;
        } else if part.attachment.kind == "link" {
            let host = host_of(part.attachment.url.as_deref().unwrap_or(""));
            part.attachment.name = Some(if title.trim().is_empty() {
                host
            } else {
                clip(&title, 200)
            });
        }
    }
    let mut text = if note.trim().is_empty() {
        format!(
            "📎 {}",
            parts
                .iter()
                // A voice note shows only the voice note (play control, duration, waveform), never its file name.
                .filter(|p| p.text.is_none() && p.attachment.kind != "voice")
                .map(|p| p.attachment.name.as_deref().unwrap_or("file"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        note.clone()
    };
    if text == "📎 " {
        text.clear();
    }
    for part in &parts {
        if let Some(snippet) = &part.text {
            if snippet.chars().count() <= 500 {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&format!("“{snippet}”"));
            }
        }
    }
    let attachments = parts
        .iter()
        .filter(|p| p.text.as_ref().is_none_or(|s| s.chars().count() > 500))
        .map(|p| p.attachment.clone())
        .collect();
    let item = lucina_store::timeline::normalize(
        NewItem {
            channel,
            role: "user".into(),
            text,
            reply_to,
            attachments,
            meta: Some(Meta {
                share_id: Some(id.clone()),
                intent: Some(action.clone()),
                source: Some(source.clone()),
                from_device: from_device.clone(),
                ..Meta::kind("share")
            }),
            ..NewItem::default()
        },
        Utc::now().timestamp_millis(),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let job = Job {
        share_id: id,
        status: "processing".into(),
        stage: "received".into(),
        user_item: item,
        parts,
        intent: action,
        note,
        lang: field("lang", "en"),
        source,
        from_device,
        session: if selftest {
            session
        } else {
            lucina_memory::secretary_session_id(&session, &st.cfg.default_session)
        },
        meta,
        lucina_item_id: None,
        reply: None,
        error: None,
    };
    save(&dir, &job).await?;
    // status.json says processing now: the share must be processed even if
    // the append fails here (`process` appends it again). Returning an error
    // without spawning left it stuck until a restart (Stateright SH6).
    if let Err(e) = ensure_user(&st, &job) {
        tracing::warn!(share_id = %job.share_id, error = %e, "share user item not appended yet; processing retries it");
    }
    let response = job.public(false);
    spawn(st.clone(), dir, job);
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

/// Put a shared file into lucina-media (album "Shared"): moved into the
/// content-addressed store by `/internal/v1/ingest`, or (older lucina-media)
/// registered in place. Without lucina-media the file stays in the share dir.
async fn store_in_library(
    st: &AppState,
    id: &str,
    i: usize,
    source: &str,
    note: &str,
    part: &mut Part,
    taken_at: Option<String>,
) {
    let base = st.cfg.share.media_url.trim_end_matches('/').to_owned();
    let http = reqwest::Client::new();
    let Some(path) = part.path.clone() else {
        return;
    };
    let ingest = http
        .post(format!("{base}/internal/v1/ingest"))
        .timeout(Duration::from_secs(120))
        .json(&json!({
            "path": path, "mime": part.attachment.mime, "name": part.attachment.name,
            "source": source, "album": "Shared", "caption": note, "taken_at": taken_at, "move": true,
        }))
        .send()
        .await;
    match ingest {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            let Some(item) = v["item_id"].as_str().map(str::to_owned) else {
                tracing::warn!(share_id = %id, "lucina-media ingest answered without item_id");
                return;
            };
            part.attachment.media_id = Some(item.clone());
            // The file now lives in the library; read it from there.
            let stored = http
                .get(format!("{base}/internal/v1/items/{item}/path"))
                .timeout(Duration::from_secs(10))
                .send()
                .await;
            match stored {
                Ok(r) if r.status().is_success() => {
                    if let Some(p) = r
                        .json::<Value>()
                        .await
                        .ok()
                        .and_then(|v| v["path"].as_str().map(PathBuf::from))
                    {
                        part.path = Some(p);
                    }
                }
                _ => tracing::warn!(share_id = %id, %item, "lucina-media path lookup failed"),
            }
            if !path.exists() {
                return;
            }
            if part.path.as_deref() == Some(path.as_path()) {
                return;
            }
            // Copied rather than moved (or the lookup failed): keep our copy readable.
            if part.path.as_ref().is_none_or(|p| !p.exists()) {
                part.path = Some(path);
            }
        }
        Ok(r) if matches!(r.status().as_u16(), 404 | 405) => {
            register_in_place(&http, &base, id, i, source, note, part).await
        }
        Ok(r) => {
            tracing::warn!(share_id = %id, status = %r.status(), "lucina-media ingest refused the file; keeping it")
        }
        Err(e) => {
            tracing::warn!(share_id = %id, error = %e, "lucina-media unavailable; keeping the file")
        }
    }
}
async fn register_in_place(
    http: &reqwest::Client,
    base: &str,
    id: &str,
    i: usize,
    source: &str,
    note: &str,
    part: &mut Part,
) {
    let result = http
        .put(format!("{base}/internal/v1/external/{source}/{id}-{i}"))
        .timeout(Duration::from_secs(30))
        .json(
            &json!({"path":part.path,"title":part.attachment.name,"mime":part.attachment.mime,
            "caption":note,"album":"Shared","created_at":Utc::now().to_rfc3339()}),
        )
        .send()
        .await;
    match result {
        Ok(r) if r.status().is_success() => {
            if let Ok(v) = r.json::<Value>().await {
                part.attachment.media_id = v["item_id"].as_str().map(str::to_owned);
            }
        }
        _ => {
            tracing::warn!(share_id = %id, "share media registration unavailable; keeping original")
        }
    }
}

/// How often shares whose files missed lucina-media are tried again.
const STORE_AGAIN_EVERY: Duration = Duration::from_secs(300);
/// Shares older than this are left alone (their month folder is skipped).
const STORE_AGAIN_DAYS: i64 = 31;

/// A file that arrived while lucina-media was down stays in the share folder, and its timeline item has no
/// media id, so no client can open it (an iOS tap on the card did nothing). This stores such files once the
/// library answers again and rewrites the item with their ids; clients get the item again under its id.
/// Runs at startup and every [`STORE_AGAIN_EVERY`].
pub(crate) fn store_again(st: AppState) {
    if st.share.storing_again.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        loop {
            store_unstored(&st).await;
            tokio::time::sleep(STORE_AGAIN_EVERY).await;
        }
    });
}

/// One pass over recent shares; how many files it stored.
async fn store_unstored(st: &AppState) -> usize {
    let oldest = (Utc::now() - chrono::Duration::days(STORE_AGAIN_DAYS))
        .format("%Y-%m")
        .to_string();
    let Ok(mut months) = tokio::fs::read_dir(&st.cfg.share.dir).await else {
        return 0;
    };
    let mut stored = 0;
    while let Ok(Some(month)) = months.next_entry().await {
        if month.file_name().to_string_lossy().as_ref() < oldest.as_str() {
            continue;
        }
        let Ok(mut dirs) = tokio::fs::read_dir(month.path()).await else {
            continue;
        };
        while let Ok(Some(entry)) = dirs.next_entry().await {
            let Ok(bytes) = tokio::fs::read(entry.path().join("status.json")).await else {
                continue;
            };
            let Ok(mut job) = serde_json::from_slice::<Job>(&bytes) else {
                continue;
            };
            // A share still being read stores its own files; this only picks up finished ones.
            if job.status == "processing"
                || is_live(st, &job.share_id)
                || !job.parts.iter().any(unstored)
            {
                continue;
            }
            stored += store_job_again(st, &entry.path(), &mut job).await;
        }
    }
    stored
}

/// One [`store_again`] pass now; how many files it stored (tests).
#[doc(hidden)]
pub async fn store_again_now(st: &AppState) -> usize {
    store_unstored(st).await
}

/// A file part with no media id whose file is still on disk.
fn unstored(p: &Part) -> bool {
    p.attachment.media_id.is_none() && p.path.as_ref().is_some_and(|path| path.exists())
}

/// Whether a part is one of the user item's attachments (short text snippets went into its text instead).
fn listed(p: &Part) -> bool {
    p.text.as_ref().is_none_or(|s| s.chars().count() > 500)
}

async fn store_job_again(st: &AppState, dir: &Path, job: &mut Job) -> usize {
    let _lock = st.share.ingest.lock().await;
    let (id, source, note) = (job.share_id.clone(), job.source.clone(), job.note.clone());
    // (index among the item's attachments, file name, media id)
    let mut stored: Vec<(usize, Option<String>, String)> = Vec::new();
    let mut at = 0;
    for (i, part) in job.parts.iter_mut().enumerate() {
        let shown = listed(part);
        if unstored(part) {
            let taken_at = part
                .file_index
                .and_then(|fi| job.meta.files.iter().find(|f| f.index == fi))
                .and_then(|f| f.taken_at.clone());
            store_in_library(st, &id, i, &source, &note, part, taken_at).await;
            if let (true, Some(media)) = (shown, part.attachment.media_id.clone()) {
                stored.push((at, part.attachment.name.clone(), media));
            }
        }
        if shown {
            at += 1;
        }
    }
    if stored.is_empty() {
        return 0;
    }
    for (k, _, media) in &stored {
        if let Some(a) = job
            .user_item
            .attachments
            .as_mut()
            .and_then(|v| v.get_mut(*k))
        {
            a.media_id = Some(media.clone());
        }
    }
    if let Err(e) = save(dir, job).await {
        tracing::warn!(share_id = %id, error = %e, "share manifest not updated with the stored files");
    }
    let edited = st.timeline_for(&job.session).edit(&job.user_item.id, |v| {
        let mut changed = false;
        for (k, name, media) in &stored {
            let Some(a) = v
                .get_mut("attachments")
                .and_then(|a| a.get_mut(*k))
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            // The same file, not whatever moved into its place.
            if a.get("name").and_then(Value::as_str) != name.as_deref() || a.contains_key("mediaId")
            {
                continue;
            }
            a.insert("mediaId".into(), json!(media));
            changed = true;
        }
        changed
    });
    match edited {
        Ok(changed) => tracing::info!(share_id = %id, files = stored.len(), item_updated = changed,
            "shared files stored in lucina-media after it came back"),
        Err(e) => {
            tracing::warn!(share_id = %id, error = %e, "shared files stored; timeline item not updated")
        }
    }
    stored.len()
}

/// Whether a timeline item exists (in memory; no file read).
fn has_item(st: &AppState, session: &str, id: &str) -> bool {
    st.timeline_for(session)
        .read_all()
        .map(|items| items.iter().rev().any(|i| i.id == id))
        .unwrap_or(false)
}
fn ensure_user(st: &AppState, job: &Job) -> anyhow::Result<()> {
    if !has_item(st, &job.session, &job.user_item.id) {
        let item = &job.user_item;
        st.timeline_for(&job.session).append(NewItem {
            id: Some(item.id.clone()),
            channel: item.channel.clone(),
            role: "user".into(),
            text: item.text.clone(),
            reply_to: item.reply_to.clone(),
            attachments: item.attachments.clone().unwrap_or_default(),
            meta: item.meta.clone(),
            ..NewItem::default()
        })?;
    }
    Ok(())
}
/// Lucina's reply to the share's user item, if one was written before a
/// restart. Every item is checked: other writers keep appending, so a reply
/// can be far from the end (a "newest N" window answered twice; Stateright
/// case and Verus `unfixed_window_scan` in docs/verification.md). Items that
/// don't mention the id are skipped before any JSON parsing.
fn existing_reply(st: &AppState, session: &str, user_item: &str) -> Option<(String, String)> {
    let items = st.timeline_for(session).read_all().ok()?;
    items
        .iter()
        .rev()
        .filter(|i| i.raw.contains(user_item))
        .find_map(|i| {
            let v: Value = serde_json::from_str(&i.raw).ok()?;
            (v["role"] == "lucina" && v["replyTo"] == user_item).then(|| {
                (
                    i.id.to_string(),
                    v["text"].as_str().unwrap_or_default().to_owned(),
                )
            })
        })
}
/// Resume acknowledged shares after a server restart, including a crash between
/// saving the manifest and appending its user item.
pub(crate) fn recover(st: AppState) {
    if st.share.recovered.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let _lock = st.share.ingest.lock().await;
        let Ok(mut months) = tokio::fs::read_dir(&st.cfg.share.dir).await else {
            return;
        };
        while let Ok(Some(month)) = months.next_entry().await {
            let Ok(mut dirs) = tokio::fs::read_dir(month.path()).await else {
                continue;
            };
            while let Ok(Some(entry)) = dirs.next_entry().await {
                if let Ok(bytes) = tokio::fs::read(entry.path().join("status.json")).await {
                    if let Ok(job) = serde_json::from_slice::<Job>(&bytes) {
                        if job.status == "processing" {
                            spawn(st.clone(), entry.path(), job);
                        }
                    }
                }
            }
        }
    });
}
fn is_live(st: &AppState, id: &str) -> bool {
    st.share
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(id)
}

/// Removes the share from the live set when its task ends, panics included.
struct LiveGuard(AppState, String);
impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0
            .share
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

fn spawn(st: AppState, dir: PathBuf, mut job: Job) {
    {
        let mut live = st.share.live.lock().unwrap_or_else(|e| e.into_inner());
        if !live.insert(job.share_id.clone()) {
            return; // already being processed
        }
    }
    tokio::spawn(async move {
        let _live = LiveGuard(st.clone(), job.share_id.clone());
        let _permit = st
            .share
            .worker
            .acquire()
            .await
            .expect("share semaphore stays open");
        let result = process(&st, &dir, &mut job).await;
        if let Err(e) = result {
            job.status = "error".into();
            job.stage = "error".into();
            job.error = Some(e.to_string());
            let reply = format!("I couldn't finish reading this share: {e}");
            let item = st.timeline_for(&job.session).record(NewItem {
                channel: job.user_item.channel.clone(),
                role: "lucina".into(),
                text: reply.clone(),
                reply_to: Some(job.user_item.id.clone()),
                meta: Some(Meta {
                    source: Some(job.source.clone()),
                    from_device: job.from_device.clone(),
                    ..Meta::default()
                }),
                ..NewItem::default()
            });
            job.lucina_item_id = item.map(|i| i.id);
            job.reply = Some(reply);
        }
        if let Ok(file) = std::fs::File::open(&st.cfg.timeline_path) {
            if let Err(e) = file.sync_all() {
                tracing::error!(error = %e, "cannot sync share timeline");
            }
        }
        if let Err(e) = save(&dir, &job).await {
            tracing::error!(share_id = %job.share_id, error = %e, "cannot persist share completion");
        }
    });
}

/// A link inside the share dir to a file stored elsewhere (the library), so
/// work files (vision JPEG, WAV, frames) never land next to the original.
async fn work_path(dir: &Path, i: usize, path: &Path) -> anyhow::Result<PathBuf> {
    if path.parent() == Some(dir) {
        return Ok(path.to_owned());
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize)
        .unwrap_or_else(|| "file".into());
    let link = dir.join(format!("work-{i:02}-{name}"));
    if tokio::fs::symlink_metadata(&link).await.is_err() {
        tokio::fs::symlink(path, &link).await?;
    }
    Ok(link)
}

/// One part's section of the turn message.
fn section(n: usize, part: &Part, x: &Extracted, budget: &mut usize) -> String {
    let mut s = format!(
        "\n### {n}. {} — {}{}{}{}\n",
        part.attachment.name.as_deref().unwrap_or("shared text"),
        x.kind,
        x.subtype
            .as_deref()
            .map(|t| format!("/{t}"))
            .unwrap_or_default(),
        part.attachment
            .mime
            .as_deref()
            .map(|m| format!(", {m}"))
            .unwrap_or_default(),
        part.attachment
            .bytes
            .map(|b| format!(", {b} bytes"))
            .unwrap_or_default(),
    );
    s.push_str(&format!(
        "How to handle it: {}\n",
        guidance::for_kind(x.kind, x.subtype.as_deref())
    ));
    for w in &x.warnings {
        s.push_str(&format!("Could not read: {w}\n"));
    }
    if !x.structured.is_null() {
        s.push_str(&format!(
            "Data: {}\n",
            clip(&x.structured.to_string(), 4000)
        ));
    }
    if let Some(summary) = &x.summary {
        s.push_str(&format!("Summary: {summary}\n"));
    }
    let room = PART_TEXT_MAX.min(*budget);
    let text = clip(&x.text, room);
    *budget = budget.saturating_sub(text.chars().count());
    let cut = x.text.chars().count() > text.chars().count();
    s.push_str(&format!(
        "Text{}:\n{text}\n",
        if cut {
            " (beginning; the rest is stored and searchable)"
        } else {
            ""
        }
    ));
    s
}

/// One local file through the per-file-type handling, outside a share job
/// (Telegram photos and documents): the section for the turn message and the
/// timeline attachment. Nothing is stored in the attachment memory.
pub(crate) async fn read_local_file(
    st: &AppState,
    dir: &Path,
    path: &Path,
    name: &str,
    mime: &str,
) -> (String, Attachment) {
    let (p, n, m) = (path.to_owned(), name.to_owned(), mime.to_owned());
    let routed = tokio::task::spawn_blocking(move || router::route(&p, &n, &m))
        .await
        .unwrap_or("other");
    let bytes = tokio::fs::metadata(path).await.map(|m| m.len()).ok();
    let mut part = Part::new(Attachment {
        kind: router::timeline_kind(routed).into(),
        name: Some(sanitize(name)),
        mime: (!mime.is_empty()).then(|| mime.to_owned()),
        bytes,
        ..Attachment::default()
    });
    part.path = Some(path.to_owned());
    part.handler = Some(routed.into());
    let seconds = if ["audio", "video"].contains(&part.attachment.kind.as_str()) {
        1200
    } else {
        READ_LIMIT_SECS
    };
    let x = match tokio::time::timeout(
        Duration::from_secs(seconds),
        extract::read(st, dir, &mut part, true, None, "", true),
    )
    .await
    {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            let mut x = Extracted::new("other");
            x.warnings.push(e.to_string());
            x
        }
        Err(_) => {
            let mut x = Extracted::new("other");
            x.warnings.push("reading it took too long".into());
            x
        }
    };
    let mut budget = TURN_TEXT_BUDGET;
    (section(1, &part, &x, &mut budget), part.attachment)
}

/// The share pipeline's Whisper (`LUCINA_WHISPER_BIN`, `LUCINA_WHISPER_MODEL`) on a local file.
pub(crate) async fn transcribe_local(st: &AppState, path: &Path) -> anyhow::Result<String> {
    extract::transcribe(st, path, false)
        .await
        .map(|(text, _)| text)
}

/// The share turn message around the read content (see `process`).
pub(crate) fn turn_message(note: &str, _device_lang: &str, content: &str) -> String {
    let note = note.trim();
    let owner = if note.is_empty() {
        "(no note: react as rule 2 says)".to_owned()
    } else {
        format!("\"{note}\"")
    };
    format!("Owner's question or note: {owner}\n\n<Shared content>{content}</Shared content>")
}

async fn process(st: &AppState, dir: &Path, job: &mut Job) -> anyhow::Result<()> {
    ensure_user(st, job)?;
    if let Some((id, text)) = existing_reply(st, &job.session, &job.user_item.id) {
        job.status = "done".into();
        job.stage = "done".into();
        job.reply = Some(text);
        job.lucina_item_id = Some(id);
        return Ok(());
    }
    job.stage = "reading".into();
    save(dir, job).await?;
    let mut content = String::new();
    let mut voice_transcripts = Vec::new();
    // A voice note whose recording was silent (peak under -60 dBFS): it gets its own reply, not "I couldn't hear you".
    let mut silent_voice = false;
    let mut budget = TURN_TEXT_BUDGET;
    let has_voice = job.parts.iter().any(|p| p.attachment.kind == "voice");
    let voice_only = job.parts.len() == 1 && has_voice && job.note.trim().is_empty();
    let extract_files = job.intent != "library" || has_voice;
    for i in 0..job.parts.len() {
        let on_device = job.parts[i]
            .file_index
            .and_then(|fi| job.meta.files.iter().find(|f| f.index == fi))
            .and_then(|f| f.transcript.clone());
        let seconds =
            if ["audio", "video", "voice"].contains(&job.parts[i].attachment.kind.as_str()) {
                1200
            } else {
                READ_LIMIT_SECS
            };
        job.parts[i].state = "reading".into();
        save(dir, job).await?;
        let original = job.parts[i].path.clone();
        if let Some(p) = &original {
            job.parts[i].path = Some(work_path(dir, i, p).await?);
        }
        let lang = owner_language(st, &job.session, &job.note)
            .code()
            .to_owned();
        let result = tokio::time::timeout(
            Duration::from_secs(seconds),
            extract::read(
                st,
                dir,
                &mut job.parts[i],
                extract_files,
                on_device.as_deref(),
                &lang,
                true,
            ),
        )
        .await;
        // A fetched link file is new (keep it); a stored original keeps its real path.
        if original.is_some() {
            job.parts[i].path = original;
        }
        let part = &mut job.parts[i];
        let x = match result {
            Ok(Ok(x)) => {
                part.state = "read".into();
                x
            }
            Ok(Err(e)) => {
                part.state = "failed".into();
                let mut x = Extracted::new("other");
                x.warnings.push(e.to_string());
                x
            }
            Err(_) => {
                part.state = "failed".into();
                let mut x = Extracted::new("other");
                x.warnings.push("reading it took too long".into());
                x
            }
        };
        if part.attachment.kind == "voice" {
            part.attachment.transcript = (!x.text.trim().is_empty()
                && x.subtype.as_deref() == Some("voice"))
            .then(|| clip(x.text.trim(), 4000));
            if part.attachment.transcript.is_some() {
                voice_transcripts.push(x.text.trim().to_owned());
            }
            if part.attachment.transcript.is_none() {
                part.state = "failed".into();
                silent_voice |= x.structured["silent"] == true;
            }
        }
        part.handler = Some(x.kind.into());
        part.summary = x
            .summary
            .clone()
            .or_else(|| (!x.text.trim().is_empty()).then(|| clip(x.text.trim(), 300)));
        if part.state == "read" && extract_files {
            let record = memory::Record {
                id: format!("{}-{i}", job.share_id),
                share_id: job.share_id.clone(),
                media_id: part.attachment.media_id.clone(),
                item_id: Some(job.user_item.id.clone()),
                name: x
                    .title
                    .clone()
                    .or_else(|| part.attachment.name.clone())
                    .unwrap_or_else(|| "shared text".into()),
                kind: x.kind.into(),
                subtype: x.subtype.clone(),
                created_at: chrono::DateTime::parse_from_rfc3339(&job.user_item.ts)
                    .map(|t| t.timestamp_millis())
                    .unwrap_or_else(|_| Utc::now().timestamp_millis()),
                text: x.text.clone(),
                summary: x.summary.clone(),
                structured: x.structured.clone(),
            };
            let memory = st.share.memory.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || memory.put(&record)).await? {
                tracing::warn!(share_id = %job.share_id, error = %e, "attachment memory write failed");
            }
        }
        content.push_str(&section(i + 1, part, &x, &mut budget));
        save(dir, job).await?;
    }
    job.stage = "thinking".into();
    save(dir, job).await?;
    let voice_meta = Meta {
        transcript: has_voice
            .then(|| {
                job.parts
                    .iter()
                    .filter(|p| p.attachment.kind == "voice")
                    .filter_map(|p| p.attachment.transcript.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.is_empty())
            .map(|s| clip(&s, 4000)),
        voice_reply: has_voice.then_some(true),
        // Who asked, on Lucina's items of this turn (the notifier routes the reply by it).
        source: Some(job.source.clone()),
        from_device: job.from_device.clone(),
        ..Meta::default()
    };
    if has_voice
        && job
            .parts
            .iter()
            .any(|p| p.attachment.kind == "voice" && p.attachment.transcript.is_none())
    {
        let silent = silent_voice && voice_transcripts.is_empty();
        let reply = match (
            owner_language(st, &job.session, &job.note).reply().code(),
            silent,
        ) {
            ("da", true) => SILENT_REPLY_DA,
            (_, true) => SILENT_REPLY,
            ("da", false) => "Jeg kunne ikke høre dig. Kan du gentage det?",
            _ => "I couldn't hear you. Could you repeat that?",
        };
        let item = st.timeline_for(&job.session).append(NewItem {
            channel: job.user_item.channel.clone(),
            role: "lucina".into(),
            text: reply.into(),
            reply_to: Some(job.user_item.id.clone()),
            meta: Some(voice_meta),
            ..NewItem::default()
        })?;
        job.reply = Some(reply.into());
        job.lucina_item_id = Some(item.id);
    } else if job.intent == "library" && !voice_only {
        if job
            .parts
            .iter()
            .any(|p| p.text.is_some() || p.attachment.url.is_some())
        {
            use lucina_tools::Tool;
            let ctx =
                lucina_tools::Ctx::new(st.tool_env.clone(), &job.user_item.channel, &job.session)
                    .with_origin(lucina_tools::Origin::Owner);
            tokio::time::timeout(Duration::from_secs(30), lucina_tools::tools::notes::NotesTool.call(json!({
                "action":"create", "slug":format!("share-{}", job.share_id.to_ascii_lowercase()),
                "title":job.parts[0].attachment.name.as_deref().unwrap_or("Shared"),
                "content":clip(&format!("{}\n{}", job.note, content), 19_000)
            }), &ctx)).await.context("saving shared note timed out")?.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        let reply = match owner_language(st, &job.session, &job.note).reply().code() {
            "da" => "Gemt i samlingen Shared.",
            _ => "Saved to the Shared library.",
        };
        let item = st.timeline_for(&job.session).append(NewItem {
            channel: job.user_item.channel.clone(),
            role: "lucina".into(),
            text: reply.into(),
            reply_to: Some(job.user_item.id.clone()),
            meta: Some(voice_meta),
            ..NewItem::default()
        })?;
        job.reply = Some(reply.into());
        job.lucina_item_id = Some(item.id);
    } else {
        let note = job.note.trim();
        let owner = if note.is_empty() {
            "(no note: react as rule 2 says)".to_owned()
        } else {
            format!("\"{note}\"")
        };
        let mut message = String::new();
        if let Some(instruction) = guidance::intent_instruction(&job.intent) {
            message.push_str(instruction);
            message.push('\n');
        }
        message.push_str(&format!(
            "Owner's question or note: {owner}\n\n<Shared content>{content}</Shared content>"
        ));
        if voice_only {
            message = voice_transcripts.remove(0);
        }
        let owner_text = if voice_only {
            message.clone()
        } else {
            job.note.clone()
        };
        crate::about::observe(
            st,
            &job.session,
            "document",
            &job.share_id,
            &format!("{}\n{content}", job.note.trim()),
        );
        let out = crate::chat::share_turn(
            st.clone(),
            message,
            job.session.clone(),
            job.user_item.channel.clone(),
            crate::chat::ShareTurn {
                item: job.user_item.clone(),
                voice_only,
                reply_meta: voice_meta,
                owner_text,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
        if out["action"] == "share_error" {
            job.status = "error".into();
            job.error = out["reply"].as_str().map(str::to_owned);
        }
        job.reply = out["reply"].as_str().map(str::to_owned);
        job.lucina_item_id = out["lucinaItemId"].as_str().map(str::to_owned);
    }
    if job.status != "error" {
        job.status = "done".into();
        job.stage = "done".into();
    } else {
        job.stage = "error".into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_clips_get_their_own_whisper() {
        let dir = std::env::temp_dir().join(format!("share-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("ggml-small.bin");
        std::fs::write(&small, b"x").unwrap();
        let small_s = small.to_string_lossy().to_string();
        let env = |k: &str| -> Option<String> {
            match k {
                "HOME" => Some("/home/test".into()),
                "LUCINA_WHISPER_SHORT_MODEL" | "LUCINA_WHISPER_CPU_MODEL" => Some(small_s.clone()),
                _ => None,
            }
        };
        let c = ShareConfig::from_lookup(&env);
        let s = c.whisper_short.as_ref().expect("short whisper");
        assert_eq!(s.model, small);
        assert_eq!(
            s.cpu_model.as_deref(),
            Some(small.as_path()),
            "the CPU fallback is a file that exists"
        );
        assert_eq!(s.gpu_wait, std::time::Duration::from_secs(3));
        assert_eq!(s.gpu_mb, 1500);
        assert_eq!(c.whisper_short_secs, 60.0);
        assert_eq!(c.whisper.gpu_wait, std::time::Duration::from_secs(90));
        assert_eq!(c.whisper.cpu_model.as_deref(), Some(small.as_path()));
        let none =
            ShareConfig::from_lookup(&|k: &str| (k == "HOME").then(|| "/home/test".to_string()));
        assert!(
            none.whisper_short.is_none(),
            "without LUCINA_WHISPER_SHORT_MODEL nothing changes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn names_limits_and_wording() {
        for name in ["../../etc/passwd", "a\\b\n.jpg", "...", "", "/"] {
            let clean = sanitize(name);
            assert!(
                !clean.contains('/')
                    && !clean.contains('\\')
                    && !clean.contains("..")
                    && !clean.chars().any(char::is_control)
            );
            assert!(!clean.is_empty() && clean.len() <= 120);
        }
        let long = sanitize(&format!("{}.jpeg", "è".repeat(200)));
        assert!(long.len() <= 120 && long.ends_with(".jpeg"));
        assert!(check_parts(32).is_ok());
        assert_eq!(check_parts(33).unwrap_err().0, 400);
        assert!(valid_id("abc-def12"));
        assert!(!valid_id("../../abcdefgh") && !valid_id("short"));
        assert!(valid_session("") && valid_session("my_session-1"));
        assert!(!valid_session("my session"));
        assert!(
            intent("ask")
                && intent("library")
                && intent("shopping")
                && intent("note")
                && intent("remind")
        );
        assert!(!intent("delete"));
    }

    #[test]
    fn the_share_message_carries_no_device_language() {
        for device in ["da-DK", "da", "it-IT"] {
            let m = turn_message("", device, "<page text>");
            assert!(!m.contains("Device language"), "{m}");
            assert!(!m.contains("Se hvad jeg har delt"), "{m}");
            assert!(m.contains("(no note: react as rule 2 says)"));
        }
        let m = turn_message("[selftest] Comprare: latte, uova", "da-DK", "x");
        assert!(m.contains("\"[selftest] Comprare: latte, uova\""));
        assert!(!m.contains("da-DK"));
    }

    #[test]
    fn sections_carry_guidance_data_and_budget() {
        let mut part = Part::new(Attachment {
            kind: "pdf".into(),
            name: Some("bolletta.pdf".into()),
            mime: Some("application/pdf".into()),
            bytes: Some(1234),
            ..Attachment::default()
        });
        part.state = "read".into();
        let mut x = Extracted::new("pdf");
        x.subtype = Some("invoice".into());
        x.text = "Totale da pagare € 84,30 ".repeat(1000);
        x.structured = json!({"total": "84,30", "due_date": "15/11/2026"});
        x.warnings.push("page 3 is blank".into());
        let mut budget = 5_000;
        let s = section(1, &part, &x, &mut budget);
        assert!(s.contains("### 1. bolletta.pdf — pdf/invoice, application/pdf, 1234 bytes"));
        assert!(s.contains("amount due"), "invoice guidance: {s}");
        assert!(s.contains("\"due_date\":\"15/11/2026\""));
        assert!(s.contains("Could not read: page 3 is blank"));
        assert!(s.contains("(beginning; the rest is stored and searchable)"));
        assert_eq!(budget, 0);
        let s = section(2, &part, &x, &mut budget);
        assert!(s.contains("Text (beginning"));
    }

    #[test]
    fn meta_field_shape() {
        let m: ShareMeta = serde_json::from_str(
            r#"{"files":[{"index":0,"transcript":"ciao","width":1170,"height":2532,"durationMs":4200,"takenAt":"2026-10-25T09:00:00Z"}],
                "location":{"lat":55.6,"lon":12.5,"name":"Home"}}"#,
        )
        .unwrap();
        assert_eq!(m.files[0].transcript.as_deref(), Some("ciao"));
        assert_eq!(m.files[0].duration_ms, Some(4200));
        assert_eq!(m.location.unwrap().name.as_deref(), Some("Home"));
        assert!(serde_json::from_str::<ShareMeta>("{}")
            .unwrap()
            .files
            .is_empty());
    }
}

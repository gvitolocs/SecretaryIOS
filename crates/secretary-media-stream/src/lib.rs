//! Streaming for the Secretary players, mounted by the media library server.
//!
//! The library implements [`MediaSource`] (id → file on disk + metadata) and
//! mounts [`router`]. Routes (see docs/media-player.md):
//!
//! - `POST /v1/media/items/{id}/play` `{client, device?}` → tokenised URLs,
//!   the playback mode, subtitles and the resume position.
//! - `GET /v1/media/s/{token}/{id}/file`: the original, with HTTP range.
//! - `GET /v1/media/s/{token}/{id}/hls/{variant}/index.m3u8[?start_ms=N]` (+ `init.mp4`, `seg_N.m4s`).
//! - `GET /v1/media/s/{token}/{id}/poster.jpg`, `thumb.jpg`, `subs/{sid}.vtt`.
//! - `GET /v1/media/progress?since=ms`, `POST /v1/media/progress {entries}`,
//!   `PUT /v1/media/progress/{id}`.
//!
//! `/play` and `/progress` trust the caller: the server in front (p1's
//! secretary-server) authenticates them. `/s/...` URLs carry their own token.
pub mod dir;
pub mod gate;
pub mod hls;
pub mod kernel;
pub mod ocr;
pub mod prepare;
pub mod probe;
pub mod progress;
pub mod sidecar;
pub mod subs;
pub mod token;

use axum::{
    body::Body,
    extract::{Path as UrlPath, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use secretary_player::{
    playback::{self, Client, Mode},
    progress::{resume_point, Entry},
};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// One item as the library knows it.
#[derive(Debug, Clone)]
pub struct SourceItem {
    pub id: String,
    /// `video`, `audio` or `photo`.
    pub kind: String,
    pub title: String,
    /// The original on disk.
    pub path: PathBuf,
    pub mime: String,
    pub poster: Option<PathBuf>,
    pub thumb: Option<PathBuf>,
    /// Subtitle files next to the original (`.srt`, `.vtt`, `.ass`).
    pub subtitles: Vec<SourceSub>,
}

#[derive(Debug, Clone)]
pub struct SourceSub {
    pub path: PathBuf,
    pub lang: Option<String>,
    pub label: String,
}

/// A video in the library, for the background preparation.
#[derive(Debug, Clone)]
pub struct Video {
    pub item: SourceItem,
    /// When the library got it (ms since 1970): newer films are prepared first.
    pub added_at: u64,
}

pub trait MediaSource: Send + Sync + 'static {
    fn item(&self, id: &str) -> impl Future<Output = Option<SourceItem>> + Send;

    /// Every video a viewer can open (not hidden, not trashed). The default,
    /// none, leaves the background preparation with nothing to do.
    fn videos(&self) -> impl Future<Output = Vec<Video>> + Send {
        async { Vec::new() }
    }

    /// Resolves when items may have been added, so the preparation looks
    /// again before its next timed scan. The default never resolves.
    fn changed(&self) -> impl Future<Output = ()> + Send {
        std::future::pending()
    }
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// HMAC key for play tokens (32 random bytes).
    pub secret: Vec<u8>,
    pub token_ttl: Duration,
    /// HLS output, posters, probe results, converted subtitles (NVMe).
    pub cache_dir: PathBuf,
    pub progress_path: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// ffmpeg HLS jobs at once for viewers (background preparation has one slot of its own).
    pub max_jobs: usize,
    /// Prepare films' HLS output in the background, so they start at once.
    pub prepare: bool,
    /// Run background jobs in `systemd-run --user --scope` with a CPU quota.
    pub background_scope: bool,
    /// The background scope's CPU quota in percent (100 = one core).
    pub background_cpu_quota: u32,
    /// VAAPI render node for background H.264 (None: libx264).
    pub vaapi_device: Option<PathBuf>,
    /// The GPU VRAM semaphore VAAPI jobs run under (None: no lease).
    pub gpu_lease: Option<PathBuf>,
    /// First local hour background CPU jobs may start (`[start, end)`, wraps past midnight).
    pub prepare_window_start: u32,
    /// The hour the background CPU window ends (excluded).
    pub prepare_window_end: u32,
    /// Tctl at which no background job starts.
    pub prepare_max_temp_c: u32,
    /// One-minute load at which no background job starts.
    pub prepare_max_load: u32,
    /// tesseract for image subtitles (DVD, Blu-ray); None hides those tracks.
    pub ocr: Option<ocr::OcrConfig>,
}

impl StreamConfig {
    pub fn new(secret: Vec<u8>, cache_dir: PathBuf, progress_path: PathBuf) -> Self {
        Self {
            secret,
            token_ttl: Duration::from_secs(12 * 3600),
            cache_dir,
            progress_path,
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            max_jobs: 2,
            prepare: true,
            background_scope: false,
            background_cpu_quota: 100,
            vaapi_device: None,
            gpu_lease: None,
            prepare_window_start: 1,
            prepare_window_end: 7,
            prepare_max_temp_c: 80,
            prepare_max_load: 16,
            ocr: Some(ocr::OcrConfig {
                tesseract: "tesseract".into(),
                // tessdata_fast languages installed for the user (dan, eng, ita...), else tesseract's own.
                tessdata: std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local/share/tessdata"))
                    .filter(|d| d.is_dir()),
            }),
        }
    }
}

/// Mints play URLs; shared with the library's listing so grids get posters.
#[derive(Clone)]
pub struct Signer {
    secret: Arc<[u8]>,
    ttl: Duration,
}

impl Signer {
    pub fn new(secret: &[u8], ttl: Duration) -> Self {
        Self { secret: secret.into(), ttl }
    }

    /// `(base, expires_at)`: `/v1/media/s/{token}/{id}`.
    pub fn base(&self, id: &str) -> (String, u64) {
        let exp = now_secs() + self.ttl.as_secs();
        (format!("/v1/media/s/{}/{}", token::sign(&self.secret, id, exp), encode_segment(id)), exp)
    }

    fn verify(&self, id: &str, token: &str) -> bool {
        token::verify(&self.secret, id, token, now_secs())
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Percent-encodes one path segment (ids are opaque to this crate).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct App<S> {
    src: Arc<S>,
    cfg: StreamConfig,
    signer: Signer,
    jobs: Arc<hls::Jobs>,
    progress: progress::Store,
    subs: Arc<subs::Jobs>,
}

pub fn router<S: MediaSource>(src: Arc<S>, cfg: StreamConfig) -> Result<Router, String> {
    let background = hls::BackgroundConfig::from(&cfg);
    let app = Arc::new(App {
        signer: Signer::new(&cfg.secret, cfg.token_ttl),
        jobs: Arc::new(hls::Jobs::new(cfg.ffmpeg.clone(), cfg.max_jobs, background)),
        progress: progress::Store::open(cfg.progress_path.clone())?,
        subs: Arc::new(subs::Jobs::new(subs::Converter {
            ffmpeg: cfg.ffmpeg.clone(),
            // Image subtitles only when tesseract actually runs here.
            ocr: cfg.ocr.clone().filter(|o| {
                std::process::Command::new(&o.tesseract).arg("--version").output().is_ok_and(|out| out.status.success())
            }),
            cache_dir: cfg.cache_dir.clone(),
        })),
        src,
        cfg,
    });
    if app.cfg.prepare {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn(prepare::run(Arc::clone(&app)))),
            Err(_) => tracing::warn!("no tokio runtime: background preparation is off"),
        }
    }
    Ok(Router::new()
        .route("/v1/media/items/{id}/play", post(play::<S>))
        .route("/v1/media/s/{token}/{id}/file", get(file::<S>))
        .route("/v1/media/s/{token}/{id}/poster.jpg", get(poster::<S>))
        .route("/v1/media/s/{token}/{id}/thumb.jpg", get(thumb::<S>))
        .route("/v1/media/s/{token}/{id}/subs/{sub}", get(subtitle::<S>))
        .route("/v1/media/s/{token}/{id}/hls/{variant}/{name}", get(hls_file::<S>))
        .route("/v1/media/progress", get(progress_since::<S>).post(progress_post::<S>))
        .route("/v1/media/progress/{id}", put(progress_put::<S>))
        .with_state(app))
}

#[derive(Debug)]
struct Error(StatusCode, String);

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"ok": false, "error": self.1}))).into_response()
    }
}

fn not_found() -> Error {
    Error(StatusCode::NOT_FOUND, "Not found".into())
}

fn internal(e: impl std::fmt::Display) -> Error {
    tracing::warn!("media stream error: {e}");
    Error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Deserialize)]
struct PlayRequest {
    client: Client,
    #[serde(default)]
    device: Option<String>,
}

#[derive(Serialize)]
struct SubtitleOut {
    id: String,
    lang: Option<String>,
    label: String,
    forced: bool,
    /// `text`, or `image` for DVD/Blu-ray pictures read by OCR.
    kind: &'static str,
    /// `srt`, `ass`, `vobsub`, `pgs`, `subrip`, `dvd_subtitle`...
    format: String,
    /// `file` (next to the video) or `embedded`.
    source: &'static str,
    url: String,
}

#[derive(Serialize)]
struct PlayResponse {
    ok: bool,
    id: String,
    kind: String,
    title: String,
    mime: String,
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    mode: Mode,
    /// What to hand the player: `file_url` for direct play, else `hls_url`.
    url: String,
    file_url: String,
    hls_url: Option<String>,
    poster_url: Option<String>,
    subtitles: Vec<SubtitleOut>,
    /// Image subtitle tracks left out because OCR is not available.
    bitmap_subtitles: usize,
    expires_at: u64,
    progress: Option<Entry>,
    resume_ms: u64,
}

impl<S: MediaSource> App<S> {
    async fn item(&self, id: &str) -> Result<SourceItem> {
        self.src.item(id).await.ok_or_else(not_found)
    }

    async fn authorised(&self, token: &str, id: &str) -> Result<SourceItem> {
        if !self.signer.verify(id, token) {
            return Err(Error(StatusCode::FORBIDDEN, "Link expired".into()));
        }
        self.item(id).await
    }

    async fn probe(&self, item: &SourceItem) -> Result<probe::Info> {
        probe::probe(&self.cfg.ffprobe, &self.cfg.cache_dir, &item.path).await.map_err(internal)
    }

    fn hls_dir(&self, item: &SourceItem, variant: &str) -> Result<PathBuf> {
        let key = probe::file_key(&item.path).map_err(internal)?;
        Ok(self.cfg.cache_dir.join("hls").join(key).join(variant))
    }
}

async fn play<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath(id): UrlPath<String>,
    Json(req): Json<PlayRequest>,
) -> Result<Json<PlayResponse>> {
    let item = app.item(&id).await?;
    let (base, expires_at) = app.signer.base(&id);
    let file_url = format!("{base}/file");
    let progress = app.progress.get(&id).await;
    if item.kind == "photo" {
        return Ok(Json(PlayResponse {
            ok: true,
            id,
            kind: item.kind,
            title: item.title,
            mime: item.mime,
            duration_ms: None,
            width: None,
            height: None,
            mode: Mode::Direct,
            url: file_url.clone(),
            file_url,
            hls_url: None,
            poster_url: Some(format!("{base}/thumb.jpg")),
            subtitles: vec![],
            bitmap_subtitles: 0,
            expires_at,
            progress: None,
            resume_ms: 0,
        }));
    }
    let info = app.probe(&item).await?;
    let plan = playback::plan(&info.probe, req.client);
    let hls_url = if plan.mode == Mode::Direct {
        None
    } else {
        let variant = hls::variant_name(&plan);
        // Start ffmpeg now: by the time the player asks, the first segment is there.
        let out = app.hls_dir(&item, &variant)?;
        let segment = hls::segment_seconds(&plan, info.audio_lead_ms);
        if let Err(e) = app.jobs.ensure(item.path.clone(), plan.clone(), out, segment) {
            tracing::warn!(%id, "retrying failed hls job: {e}");
            app.jobs.ensure(item.path.clone(), plan.clone(), app.hls_dir(&item, &variant)?, segment).map_err(internal)?;
        }
        Some(format!("{base}/hls/{variant}/index.m3u8"))
    };
    let (tracks, hidden) = subs::tracks(&item, &info, app.subs.ocr_available());
    let subs: Vec<SubtitleOut> = tracks
        .iter()
        .map(|t| SubtitleOut {
            id: t.id.clone(),
            lang: t.lang.clone(),
            label: t.label.clone(),
            forced: t.forced,
            kind: if t.image { "image" } else { "text" },
            format: t.format.clone(),
            source: if matches!(t.origin, subs::Origin::File(_)) { "file" } else { "embedded" },
            url: format!("{base}/subs/{}.vtt", t.id),
        })
        .collect();
    // Embedded text tracks take a full read of the file: start them now. Image
    // tracks (OCR, minutes of CPU) start when a player asks for one.
    for track in tracks.into_iter().filter(|t| !t.image && matches!(t.origin, subs::Origin::Embedded(_))) {
        let (jobs, item, info) = (Arc::clone(&app.subs), item.clone(), info.clone());
        tokio::spawn(async move {
            let size = info.width.zip(info.height);
            let _ = jobs.get(&item, &track, size, info.duration_ms.unwrap_or(0), Duration::ZERO).await;
        });
    }
    let resume_ms = resume_point(progress.as_ref(), info.duration_ms);
    if let Some(device) = &req.device {
        tracing::info!(%id, %device, mode = ?plan.mode, "play");
    }
    Ok(Json(PlayResponse {
        ok: true,
        url: hls_url.clone().unwrap_or_else(|| file_url.clone()),
        id,
        kind: item.kind,
        title: item.title,
        mime: item.mime,
        duration_ms: info.duration_ms,
        width: info.width,
        height: info.height,
        mode: plan.mode,
        file_url,
        hls_url,
        poster_url: Some(format!("{base}/poster.jpg")),
        subtitles: subs,
        bitmap_subtitles: hidden,
        expires_at,
        progress,
        resume_ms,
    }))
}

/// Serves a file with Range / If-Modified-Since handling from tower-http.
async fn serve_file(path: &Path, mime: Option<&str>, req: Request, cache: &'static str) -> Result<Response> {
    let svc = match mime.and_then(|m| m.parse::<mime::Mime>().ok()) {
        Some(m) => ServeFile::new_with_mime(path, &m),
        None => ServeFile::new(path),
    };
    let mut res = svc.oneshot(req).await.map_err(internal)?.map(Body::new);
    if res.status().is_success() || res.status() == StatusCode::PARTIAL_CONTENT {
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    }
    Ok(res)
}

const IMMUTABLE: &str = "private, max-age=86400";

async fn file<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath((token, id)): UrlPath<(String, String)>,
    req: Request,
) -> Result<Response> {
    let item = app.authorised(&token, &id).await?;
    serve_file(&item.path, Some(&item.mime), req, IMMUTABLE).await
}

#[derive(Deserialize)]
struct PlaylistQuery {
    /// Where the viewer resumes (ms). Kept as text: anything but an integer is ignored.
    start_ms: Option<String>,
}

async fn hls_file<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath((token, id, variant, name)): UrlPath<(String, String, String, String)>,
    // A query string that does not parse is ignored like a bad start_ms.
    q: std::result::Result<Query<PlaylistQuery>, axum::extract::rejection::QueryRejection>,
    req: Request,
) -> Result<Response> {
    let item = app.authorised(&token, &id).await?;
    let plan = hls::parse_variant(&variant).ok_or_else(not_found)?;
    let out = app.hls_dir(&item, &variant)?;
    if name == "index.m3u8" {
        // Starts the job again if the cache was cleared since /play.
        let segment = hls::segment_seconds(&plan, app.probe(&item).await?.audio_lead_ms);
        app.jobs.ensure(item.path.clone(), plan, out.clone(), segment).map_err(internal)?;
        app.jobs.wait_ready(&out, Duration::from_secs(60)).await.map_err(internal)?;
        let text = tokio::fs::read_to_string(out.join("index.m3u8")).await.map_err(internal)?;
        // ffmpeg writes the end of the list just before it exits, a moment before the marker.
        let done = out.join(hls::DONE_MARKER).exists() || text.contains("#EXT-X-ENDLIST");
        return Ok((
            [
                (header::CONTENT_TYPE, "application/vnd.apple.mpegurl"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            hls::rewrite_playlist(&text, done, q.ok().and_then(|q| q.0.start_ms).as_deref().and_then(hls::parse_start_ms)),
        )
            .into_response());
    }
    if !hls::valid_segment_name(&name) {
        return Err(not_found());
    }
    let mime = if name.ends_with(".mp4") { "video/mp4" } else { "video/iso.segment" };
    serve_file(&out.join(&name), Some(mime), req, IMMUTABLE).await
}

async fn poster<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath((token, id)): UrlPath<(String, String)>,
    req: Request,
) -> Result<Response> {
    let item = app.authorised(&token, &id).await?;
    let path = poster_path(&app, &item).await?;
    serve_file(&path, Some("image/jpeg"), req, IMMUTABLE).await
}

async fn thumb<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath((token, id)): UrlPath<(String, String)>,
    req: Request,
) -> Result<Response> {
    let item = app.authorised(&token, &id).await?;
    let path = match &item.thumb {
        Some(t) => t.clone(),
        None => poster_path(&app, &item).await?,
    };
    serve_file(&path, Some("image/jpeg"), req, IMMUTABLE).await
}

/// The library's poster, or a frame 10% in (at most 5 min) for video, the
/// cover art for audio, a scaled copy for photos. Cached by file key.
async fn poster_path<S: MediaSource>(app: &App<S>, item: &SourceItem) -> Result<PathBuf> {
    if let Some(p) = &item.poster {
        return Ok(p.clone());
    }
    let key = probe::file_key(&item.path).map_err(internal)?;
    let out = app.cfg.cache_dir.join("posters").join(format!("{key}.jpg"));
    if out.exists() {
        return Ok(out);
    }
    tokio::fs::create_dir_all(out.parent().expect("has parent")).await.map_err(internal)?;
    let mut cmd = tokio::process::Command::new(&app.cfg.ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    if item.kind == "video" {
        let duration = app.probe(item).await?.duration_ms.unwrap_or(0);
        let at = (duration / 10).min(300_000);
        cmd.arg("-ss").arg(format!("{}.{:03}", at / 1000, at % 1000));
    }
    cmd.arg("-i").arg(&item.path);
    cmd.args(["-map", "0:v:0", "-frames:v", "1", "-vf", "scale='min(640,iw)':-2", "-q:v", "4"]);
    let tmp = out.with_extension("tmp.jpg");
    cmd.arg(&tmp);
    let status = cmd.kill_on_drop(true).output().await.map_err(internal)?;
    if !status.status.success() || !tmp.exists() {
        return Err(not_found());
    }
    tokio::fs::rename(&tmp, &out).await.map_err(internal)?;
    Ok(out)
}

async fn subtitle<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath((token, id, sub)): UrlPath<(String, String, String)>,
) -> Result<Response> {
    let item = app.authorised(&token, &id).await?;
    let sid = sub.strip_suffix(".vtt").ok_or_else(not_found)?;
    let info = app.probe(&item).await?;
    let (tracks, _) = subs::tracks(&item, &info, app.subs.ocr_available());
    let track = tracks.into_iter().find(|t| t.id == sid).ok_or_else(not_found)?;
    let size = info.width.zip(info.height);
    match app.subs.get(&item, &track, size, info.duration_ms.unwrap_or(0), Duration::from_secs(15)).await {
        Ok(Some(subs::Vtt::Complete(vtt))) => {
            Ok(([(header::CONTENT_TYPE, "text/vtt; charset=utf-8"), (header::CACHE_CONTROL, IMMUTABLE)], vtt).into_response())
        }
        // OCR still running: what it has read so far; the player asks again for the rest.
        Ok(Some(subs::Vtt::Partial(vtt))) => Ok((
            [
                (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
                (header::HeaderName::from_static("x-subtitles-partial"), "1"),
            ],
            vtt,
        )
            .into_response()),
        // Still converting (a film read from disk, OCR): the player asks again.
        Ok(None) => Ok((
            StatusCode::ACCEPTED,
            [(header::RETRY_AFTER, "5"), (header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"ok": false, "preparing": true})),
        )
            .into_response()),
        Err(e) => Err(internal(e)),
    }
}

#[derive(Deserialize)]
struct SinceQuery {
    #[serde(default)]
    since: u64,
}

async fn progress_since<S: MediaSource>(State(app): State<Arc<App<S>>>, Query(q): Query<SinceQuery>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "entries": app.progress.since(q.since).await}))
}

#[derive(Deserialize)]
struct ProgressBatch {
    entries: Vec<Entry>,
}

#[derive(Serialize)]
struct Ack {
    id: String,
    updated_at: u64,
}

/// Stores a batch. Every submitted entry is acknowledged (stored, or older
/// than the server's); server entries that are newer come back in `entries`.
async fn progress_post<S: MediaSource>(State(app): State<Arc<App<S>>>, Json(batch): Json<ProgressBatch>) -> Result<Json<serde_json::Value>> {
    let acks: Vec<Ack> = batch.entries.iter().map(|e| Ack { id: e.id.clone(), updated_at: e.updated_at }).collect();
    let newer = app.progress.put(batch.entries).await.map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true, "acks": acks, "entries": newer})))
}

async fn progress_put<S: MediaSource>(
    State(app): State<Arc<App<S>>>,
    UrlPath(id): UrlPath<String>,
    Json(mut entry): Json<Entry>,
) -> Result<Json<serde_json::Value>> {
    entry.id = id;
    let ack = Ack { id: entry.id.clone(), updated_at: entry.updated_at };
    let newer = app.progress.put(vec![entry]).await.map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true, "acks": [ack], "entries": newer})))
}

#[cfg(test)]
mod tests;

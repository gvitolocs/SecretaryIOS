//! `/v1/memos` HTTP API (docs/memos.md).
//!
//! Auth: requests from the apps arrive through Caddy on p1, which first asks
//! secretary-server whether the device token or cookie is valid
//! (`forward_auth`) and then adds `X-Memo-Gateway: <shared secret>`. Local
//! callers (Lucina's tools) use `Authorization: Bearer <service token>`.
//! Cookie-authenticated writes need `X-Secretary-CSRF: 1`, like secretary-server.
//!
//! Upload protocol (resumable, one request per chunk):
//! 1. `POST /v1/memos {clientId, recordedAt, durationSec, mime, sizeBytes, ...}`
//!    → the memo (same one again for a repeated `clientId`).
//! 2. `PUT /v1/memos/{id}/audio` with `Upload-Offset: <receivedBytes>` and the
//!    next bytes as the body → `{receivedBytes}`. A wrong offset gets 409 with
//!    the server's `receivedBytes` (also in the `Upload-Offset` header).
//! 3. `POST /v1/memos/{id}/complete {sha256?}` → queued for the pipeline.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Duration as CDuration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::model::{Status, Summary, Transcript};
use crate::store::{read_json, valid_id, NewMemo, StoreError};
use crate::{search, worker, App};

#[derive(Debug)]
pub struct ApiError(
    pub(crate) StatusCode,
    pub(crate) &'static str,
    pub(crate) String,
    pub(crate) Option<u64>,
);

impl ApiError {
    pub(crate) fn new(s: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self(s, code, msg.into(), None)
    }
    pub(crate) fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such memo")
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Invalid(m) => Self::bad(m),
            StoreError::NotFound => Self::not_found(),
            StoreError::Offset(n) => Self(
                StatusCode::CONFLICT,
                "offset_mismatch",
                format!("the server has {n} bytes"),
                Some(n),
            ),
            StoreError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            StoreError::Io(e) => {
                tracing::error!(error = %e, "memo store I/O");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "memo store unavailable",
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": {"code": self.1, "message": self.2}});
        if let Some(n) = self.3 {
            body["receivedBytes"] = json!(n);
        }
        let mut r = (self.0, Json(body)).into_response();
        if let Some(n) = self.3 {
            r.headers_mut()
                .insert("upload-offset", HeaderValue::from(n));
        }
        r
    }
}

pub(crate) type R<T = Json<Value>> = Result<T, ApiError>;

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

pub(crate) async fn auth(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let header_str = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::trim);
    if let (Some(want), Some(got)) = (app.gateway_token(), header_str("x-memo-gateway")) {
        if ct_eq(want, got) {
            let cookie_only =
                h.contains_key(header::COOKIE) && !h.contains_key(header::AUTHORIZATION);
            let writes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
            if cookie_only && writes && header_str("x-secretary-csrf") != Some("1") {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "csrf",
                    "cookie-authenticated requests that change state need X-Secretary-CSRF: 1",
                )
                .into_response();
            }
            return next.run(req).await;
        }
    }
    let bearer = header_str("authorization")
        .and_then(|v| v.split_once(' '))
        .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim());
    if let (Some(want), Some(got)) = (app.service_token(), bearer) {
        if ct_eq(want, got) {
            return next.run(req).await;
        }
    }
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "memo API: unknown caller",
    )
    .into_response()
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/v1/memos", get(list).post(create))
        .route("/v1/memos/search", get(search_memos))
        .route("/v1/memos/{id}", get(detail).patch(rename).delete(remove))
        .route("/v1/memos/{id}/audio", put(upload).get(audio))
        .route("/v1/memos/{id}/complete", post(complete))
        .route("/v1/memos/{id}/pdf", get(pdf))
        .route("/v1/memos/{id}/transcript", get(transcript))
        .route("/v1/memos/{id}/device-transcript", put(device_transcript))
        .route("/v1/memos/{id}/events", get(event_candidates))
        .route("/v1/memos/{id}/event", put(choose_event))
        .route("/v1/memos/{id}/resummarize", post(resummarize))
        .route("/v1/memos/{id}/reprocess", post(reprocess))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth));
    Router::new()
        .route("/health", get(health))
        .merge(api)
        .merge(crate::meetings::api::routes(app.clone()))
        .merge(crate::prompter::routes(app.clone()))
        .merge(crate::stt::routes(app.clone()))
        .with_state(app)
}

async fn health(State(app): State<Arc<App>>) -> Json<Value> {
    let memos = app.store.list();
    let count = |s: Status| memos.iter().filter(|m| m.status == s).count();
    Json(json!({
        "ok": true,
        "memos": memos.len(),
        "uploading": count(Status::Uploading),
        "pipeline": memos.iter().filter(|m| m.status.is_pipeline()).count(),
        "failed": count(Status::Failed),
        "obsidian": app.cfg.vault.is_some(),
        "cpuFallback": app.cfg.whisper.cpu_model.is_some(),
        "meetings": app.meetings().map(|s| {
            let all = s.list();
            json!({
                "count": all.len(),
                "pipeline": all.iter().filter(|m| m.status.is_pipeline()).count(),
                "failed": all.iter().filter(|m| m.status == crate::meetings::model::Status::Failed).count(),
            })
        }).unwrap_or(Value::Null),
        "diarization": app.cfg.diarize.as_ref().map(|c| c.missing().is_none()),
    }))
}

fn id_ok(id: &str) -> Result<(), ApiError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(ApiError::not_found())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    #[serde(default)]
    ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    location: Option<crate::model::GeoFix>,
    client_id: String,
    #[serde(default)]
    title: Option<String>,
    recorded_at: DateTime<Utc>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    duration_sec: f64,
    mime: String,
    size_bytes: u64,
    #[serde(default)]
    source: Option<String>,
}

async fn create(
    State(app): State<Arc<App>>,
    Json(b): Json<CreateBody>,
) -> R<(StatusCode, Json<Value>)> {
    let source = b.source.unwrap_or_else(|| "app".into());
    if source.len() > 16 || !source.bytes().all(|c| c.is_ascii_lowercase()) {
        return Err(ApiError::bad("source: a short lowercase word"));
    }
    if b.recorded_at > Utc::now() + CDuration::days(1) {
        return Err(ApiError::bad("recordedAt is in the future"));
    }
    if b.ended_at
        .is_some_and(|end| end < b.recorded_at || end > b.recorded_at + CDuration::hours(24))
    {
        return Err(ApiError::bad(
            "endedAt must be within 24 hours after recordedAt",
        ));
    }
    if b.location.as_ref().is_some_and(|p| {
        !p.lat.is_finite()
            || !p.lon.is_finite()
            || !(-90.0..=90.0).contains(&p.lat)
            || !(-180.0..=180.0).contains(&p.lon)
            || p.accuracy_m.is_some_and(|a| !a.is_finite() || a < 0.0)
    }) {
        return Err(ApiError::bad("invalid location coordinates or accuracy"));
    }
    let (m, created) = app.store.create(NewMemo {
        ended_at: b.ended_at,
        location: b.location,
        client_id: b.client_id,
        title: b.title,
        recorded_at: b.recorded_at,
        timezone: b.timezone,
        duration_sec: b.duration_sec,
        mime: b.mime,
        size_bytes: b.size_bytes,
        source,
    })?;
    if created {
        tracing::info!(id = %m.id, size = m.size_bytes, source = %m.source, "memo created");
    }
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"memo": m})),
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    /// `recordedAt` of the last memo of the previous page.
    before: Option<DateTime<Utc>>,
}

async fn list(State(app): State<Arc<App>>, Query(q): Query<ListQuery>) -> R {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let mut memos: Vec<_> = app
        .store
        .list()
        .into_iter()
        .filter(|m| q.before.is_none_or(|b| m.recorded_at < b))
        .take(limit + 1)
        .collect();
    let next = (memos.len() > limit).then(|| memos[limit - 1].recorded_at);
    memos.truncate(limit);
    Ok(Json(json!({"memos": memos, "next": next})))
}

async fn detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let summary: Option<Summary> =
        read_json(&app.store.summary_path(&id)).map_err(StoreError::from)?;
    Ok(Json(json!({
        "memo": m,
        "summary": summary,
        "pdf": app.store.pdf_path(&id).exists(),
        "url": app.cfg.memo_url(&id),
    })))
}

async fn transcript(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    id_ok(&id)?;
    app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let t: Option<Transcript> =
        read_json(&app.store.transcript_path(&id)).map_err(StoreError::from)?;
    match t {
        Some(t) => Ok(Json(json!({"transcript": t}))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_ready",
            "no transcript yet",
        )),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceTranscriptBody {
    language: String,
    engine: String,
    #[serde(default)]
    duration_sec: f64,
    segments: Vec<crate::model::Segment>,
}

/// The recording device's own transcript (short memos on the iPhone). Used while the GPU
/// is busy; never replaces a Whisper transcript.
async fn device_transcript(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<DeviceTranscriptBody>,
) -> R {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let lang_ok = (2..=8).contains(&b.language.len())
        && b.language
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c == b'-');
    if !lang_ok || b.engine.is_empty() || b.engine.chars().count() > 80 {
        return Err(ApiError::bad(
            "language: a code like it; engine: 1-80 characters",
        ));
    }
    if b.segments.is_empty() || b.segments.len() > 20_000 {
        return Err(ApiError::bad("segments: 1 to 20000"));
    }
    let mut segments = Vec::with_capacity(b.segments.len());
    for s in b.segments {
        let text = s.text.trim();
        if !(s.start.is_finite() && s.end.is_finite() && s.start >= 0.0 && s.end >= s.start)
            || text.chars().count() > 4000
        {
            return Err(ApiError::bad(
                "segment: start <= end, finite, text up to 4000 characters",
            ));
        }
        if !text.is_empty() {
            segments.push(crate::model::Segment {
                start: s.start,
                end: s.end,
                text: text.to_owned(),
                speaker: None,
                words: vec![],
            });
        }
    }
    let duration = if b.duration_sec.is_finite() && b.duration_sec > 0.0 {
        b.duration_sec
    } else {
        m.duration_sec
    };
    let t = Transcript {
        language: b.language.split('-').next().unwrap_or("und").to_owned(),
        engine: b.engine,
        duration_sec: duration,
        segments,
        speakers: false,
        created_at: Utc::now(),
        corrections: vec![],
    };
    // The iPhone's own recognizer misspells taught words too (Voice Learn).
    let mut t = t;
    crate::vocab::correct_transcript(&app.cfg.whisper.vocab.get(), &mut t);
    crate::store::write_json(&app.store.device_transcript_path(&id), &t)
        .map_err(StoreError::from)?;
    // A memo waiting for the GPU can go ahead now.
    if m.status == Status::Queued && m.next_attempt_at.is_some() {
        app.store.update(&id, |m| m.next_attempt_at = None)?;
    }
    app.wake.notify_one();
    Ok(Json(
        json!({"stored": true, "used": m.transcript_source.as_deref() != Some("whisper")}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameBody {
    title: String,
}

async fn rename(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<RenameBody>,
) -> R {
    id_ok(&id)?;
    let title = crate::store::clean_title(&b.title);
    if title.is_empty() {
        return Err(ApiError::bad("title: 1 to 160 characters"));
    }
    let m = app.store.update(&id, |m| {
        m.title = title;
        m.title_source = "owner".into();
        // The PDF and note show the title: refresh them (not the summary).
        if m.steps.rendered_at.is_some() {
            m.steps.rendered_at = None;
            m.steps.published_at = None;
            m.steps.notified_at = m.steps.notified_at.or(Some(Utc::now()));
            if m.status == Status::Done {
                m.status = Status::Queued;
            }
        }
    })?;
    app.wake.notify_one();
    Ok(Json(json!({"memo": m})))
}

async fn remove(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    if m.status.is_pipeline() && !matches!(m.status, Status::Queued) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "busy",
            "the memo is being processed; try again in a minute",
        ));
    }
    app.store.trash(&id)?;
    crate::library::unregister(&id).await;
    tracing::info!(%id, "memo moved to trash");
    Ok(Json(json!({"deleted": id})))
}

async fn upload(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<OffsetQuery>,
    body: Body,
) -> R<Response> {
    id_ok(&id)?;
    let offset = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or(q.offset)
        .ok_or_else(|| ApiError::bad("Upload-Offset header (or ?offset=) required"))?;
    let m = app
        .store
        .append(&id, offset, body.into_data_stream())
        .await?;
    let mut r =
        Json(json!({"receivedBytes": m.received_bytes, "sizeBytes": m.size_bytes})).into_response();
    r.headers_mut()
        .insert("upload-offset", HeaderValue::from(m.received_bytes));
    Ok(r)
}

#[derive(Deserialize)]
struct OffsetQuery {
    offset: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteBody {
    #[serde(default)]
    sha256: Option<String>,
}

async fn complete(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    body: Option<Json<CompleteBody>>,
) -> R {
    id_ok(&id)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    if let Some(h) = &b.sha256 {
        if h.len() != 64 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(ApiError::bad("sha256: 64 hex digits"));
        }
    }
    let m = app.store.complete(&id, b.sha256.as_deref()).await?;
    tracing::info!(%id, size = m.size_bytes, "memo upload complete");
    app.wake.notify_one();
    crate::library::register(&app, &m).await;
    Ok(Json(json!({"memo": m})))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ResummarizeBody {
    #[serde(default)]
    instructions: Option<String>,
}

async fn resummarize(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    body: Option<Json<ResummarizeBody>>,
) -> R {
    id_ok(&id)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let instructions = b
        .instructions
        .map(|s| s.trim().chars().take(2000).collect::<String>())
        .filter(|s| !s.is_empty());
    let m = worker::request_resummary(&app, &id, instructions)?;
    Ok(Json(json!({"memo": m})))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReprocessBody {
    #[serde(default)]
    from_transcript: bool,
}

async fn reprocess(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    body: Option<Json<ReprocessBody>>,
) -> R {
    id_ok(&id)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    if m.status == Status::Uploading {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "the upload is not complete",
        ));
    }
    let m = worker::request_reprocess(&app, &id, b.from_transcript)?;
    Ok(Json(json!({"memo": m})))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
    /// `YYYY-MM-DD` (owner's zone) or RFC 3339.
    from: Option<String>,
    to: Option<String>,
    /// Shortcut: the last N days.
    days: Option<i64>,
    limit: Option<usize>,
}

fn parse_when(app: &App, s: &str, end: bool) -> Result<DateTime<Utc>, ApiError> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| ApiError::bad("dates: YYYY-MM-DD or RFC 3339"))?;
    let d = if end { d.succ_opt().unwrap_or(d) } else { d };
    let local = app
        .cfg
        .timezone
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
        .earliest()
        .ok_or_else(|| ApiError::bad("date not in the owner's time zone"))?;
    Ok(local.with_timezone(&Utc))
}

async fn search_memos(State(app): State<Arc<App>>, Query(q): Query<SearchQuery>) -> R {
    let mut from = q
        .from
        .as_deref()
        .map(|s| parse_when(&app, s, false))
        .transpose()?;
    let to =
        q.to.as_deref()
            .map(|s| parse_when(&app, s, true))
            .transpose()?;
    if let Some(d) = q.days {
        from = Some(Utc::now() - CDuration::days(d.clamp(1, 3650)));
    }
    let query = search::Query {
        q: q.q.as_deref().filter(|s| !s.trim().is_empty()),
        from,
        to,
        limit: q.limit.unwrap_or(10).clamp(1, 50),
    };
    let hits = search::search(&app.store, &query, |id| app.cfg.memo_url(id));
    Ok(Json(json!({"memos": hits})))
}

async fn pdf(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> R<Response> {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let path = app.store.pdf_path(&id);
    if !path.exists() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_ready",
            "no PDF yet",
        ));
    }
    let name = format!("{}.pdf", crate::obsidian::safe_name(&m.title));
    serve_file(&path, "application/pdf", &headers, Some(&name)).await
}

async fn audio(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> R<Response> {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    if m.status == Status::Uploading {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "uploading",
            "the audio is still uploading",
        ));
    }
    serve_file(&app.store.audio_path(&m), &m.mime, &headers, None).await
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` → inclusive (start, end). None = not satisfiable.
pub fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (a, b) = first.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => (s.parse().ok()?, len - 1),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

pub(crate) async fn serve_file(
    path: &std::path::Path,
    mime: &str,
    headers: &HeaderMap,
    download: Option<&str>,
) -> R<Response> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let etag = format!("\"{len:x}-{mtime:x}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
        Some(r) => match parse_range(r, len) {
            Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
            None => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{len}"))],
                )
                    .into_response())
            }
        },
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|_| ApiError::not_found())?;
    f.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| ApiError::from(StoreError::Io(e)))?;
    let stream = tokio_util::io::ReaderStream::new(f.take(count));
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).unwrap_or(HeaderValue::from_static("\"0\"")),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0, must-revalidate"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{len}"))
                .unwrap_or(HeaderValue::from_static("bytes */0")),
        );
    }
    if let Some(name) = download {
        let ascii: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || " .-_".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{ascii}\"")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-1", 10), Some((0, 1)));
        assert_eq!(parse_range("bytes=5-", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=8-100", 10), Some((8, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=4-2", 10), None);
        assert_eq!(parse_range("items=0-1", 10), None);
    }
}

async fn event_candidates(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let candidates = crate::events::candidates(&app, &m, 120)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "calendar_unreachable", &e))?;
    Ok(Json(
        json!({"current":m.event,"eventSource":m.event_source,"candidates":candidates.items,"stale":candidates.stale}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChooseEvent {
    event_id: Option<String>,
}
async fn choose_event(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(b): Json<ChooseEvent>,
) -> R {
    id_ok(&id)?;
    let m = app.store.get(&id).ok_or_else(ApiError::not_found)?;
    let allowed = |m: &crate::model::Memo| {
        matches!(
            m.status,
            Status::Done | Status::Failed | Status::Uploading | Status::Transcribing
        ) || (m.status == Status::Queued && m.steps.summarized_at.is_none())
    };
    if !allowed(&m) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "busy",
            "the memo summary is being processed",
        ));
    }
    let event = if let Some(want) = b.event_id {
        Some(
            crate::events::candidates(&app, &m, 120)
                .await
                .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "calendar_unreachable", &e))?
                .into_iter()
                .find(|c| c.event.id == want)
                .ok_or_else(ApiError::not_found)?
                .event,
        )
    } else {
        None
    };
    let fallback: Option<Summary> =
        crate::store::read_json(&app.store.summary_path(&id)).map_err(StoreError::from)?;
    let mut busy = false;
    let m = app.store.update(&id, |m| {
        if !allowed(m) {
            busy = true;
            return;
        }
        m.event = event;
        m.event_source = Some("owner".into());
        m.steps.linked_at = Some(Utc::now());
        if m.event.is_none() && m.title_source == "event" {
            m.title = fallback
                .as_ref()
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Voice memo".into());
            m.title_source = "lucina".into();
        }
        crate::events::apply_title(m, app.cfg.timezone);
    })?;
    if busy {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "busy",
            "the memo summary is being processed",
        ));
    }
    let m = if matches!(m.status, Status::Done | Status::Failed) {
        worker::request_resummary(&app, &id, None)?
    } else {
        m
    };
    Ok(Json(json!({"memo":m})))
}

//! `/v1/diagnostics` HTTP API (docs/diagnostics.md).
//!
//! Auth, as `/v1/memos`: requests from the apps arrive through Caddy on p1,
//! which first asks secretary-server whether the device token or cookie is
//! valid (`forward_auth`) and then adds `X-Diagnostics-Gateway: <shared
//! secret>`. Local callers (Lucina, secretary-mcp, the servers) use
//! `Authorization: Bearer <service token>`. Cookie-authenticated writes need
//! `X-Secretary-CSRF: 1`.
//!
//! - `POST /v1/diagnostics/reports`: a report as JSON, or `multipart/form-data`
//!   with a `report` part (the same JSON) and file parts (logs, minidumps,
//!   gzip accepted). The whole body may also be `Content-Encoding: gzip`.
//!   → 201 `{id, duplicate: false, count: 1, fingerprint}` for a new report,
//!   200 `{id, duplicate: true, count, fingerprint}` for a repeat.
//! - `GET /v1/diagnostics/reports?app=&kind=&since=&until=&device=&fingerprint=&limit=`
//! - `GET /v1/diagnostics/reports/{id}` → `{report, attachments}`
//! - `GET /v1/diagnostics/reports/{id}/files/{name}[?tail=<bytes>|head=<bytes>]`
use std::io::Read;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration as CDuration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::model::{clip, new_id, valid_id, AppName, Attachment, FileKind, Kind, Report};
use crate::store::{Ingest, ListQuery};
use crate::{fingerprint, scrub, App, MB};

/// Field limits (longer text is cut, after scrubbing).
pub const MAX_MESSAGE: usize = 16 * 1024;
pub const MAX_STACK: usize = 128 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 64 * 1024;
/// Reports a list returns without `limit`.
const DEFAULT_LIST: usize = 50;

pub struct ApiError(StatusCode, &'static str, String, Option<u64>);

impl ApiError {
    fn new(s: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self(s, code, msg.into(), None)
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such report or file")
    }
    fn too_large(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", msg)
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "diagnostics store");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "diagnostics store unavailable",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (
            self.0,
            Json(json!({"error": {"code": self.1, "message": self.2}})),
        )
            .into_response();
        if let Some(n) = self.3 {
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(n));
        }
        r
    }
}

type R<T = Json<Value>> = Result<T, ApiError>;

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

async fn auth(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let header_str = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::trim);
    if let (Some(want), Some(got)) = (app.gateway_token(), header_str("x-diagnostics-gateway")) {
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
        "diagnostics API: unknown caller",
    )
    .into_response()
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/v1/diagnostics/reports", get(list).post(create))
        .route("/v1/diagnostics/reports/{id}", get(detail))
        .route("/v1/diagnostics/reports/{id}/files/{name}", get(file))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth))
        // `create` enforces the upload cap itself (before and after gzip).
        .layer(DefaultBodyLimit::disable());
    Router::new()
        .route("/health", get(health))
        .merge(api)
        .with_state(app)
}

async fn health(State(app): State<Arc<App>>) -> R {
    let a = app.clone();
    let n = tokio::task::spawn_blocking(move || a.store.count())
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "ok": true,
        "reports": n,
        "retentionDays": app.cfg.retention.num_days(),
        "notify": app.cfg.notify.enabled(),
    })))
}

// -- ingest --------------------------------------------------------------------

/// The report as a client sends it (snake_case; camelCase accepted).
#[derive(Debug, Deserialize)]
pub struct NewReport {
    pub app: String,
    #[serde(default, alias = "appVersion")]
    pub app_version: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub stack: Option<String>,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default, alias = "occurredAt")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// The client's id for this report; a retry with the same id is a no-op.
    #[serde(default, alias = "clientId")]
    pub client_id: Option<String>,
}

fn short(field: &str, v: Option<String>, max: usize) -> Result<String, ApiError> {
    let v = v.unwrap_or_default();
    let v = v.trim();
    if v.chars().count() > max || v.chars().any(char::is_control) {
        return Err(ApiError::bad(format!(
            "{field}: at most {max} characters, one line"
        )));
    }
    Ok(v.to_owned())
}

/// Validates, scrubs and fingerprints a client's report.
pub fn build_report(b: NewReport, now: DateTime<Utc>) -> Result<Report, ApiError> {
    let app = AppName::parse(&b.app)
        .ok_or_else(|| ApiError::bad("app: one of secretary-code, ios, android, web, server"))?;
    let kind = Kind::parse(&b.kind).ok_or_else(|| {
        ApiError::bad("kind: one of crash, fatal_error, error, user_report, hang")
    })?;
    let app_version = short("app_version", b.app_version, 64)?;
    let os = short("os", b.os, 120)?;
    let device = short("device", b.device, 120)?;
    if let Some(c) = &b.client_id {
        let ok = (8..=100).contains(&c.len())
            && c.bytes()
                .all(|x| x.is_ascii_alphanumeric() || b"-_.:".contains(&x));
        if !ok {
            return Err(ApiError::bad(
                "client_id: 8 to 100 characters of letters, digits, - _ . :",
            ));
        }
    }
    let occurred_at = b.occurred_at.unwrap_or(now);
    if occurred_at > now + CDuration::days(1) {
        return Err(ApiError::bad("occurred_at is in the future"));
    }
    let (message, n1) = scrub::text(&b.message.unwrap_or_default().replace('\0', ""));
    let (stack, n2) = scrub::text(&b.stack.unwrap_or_default().replace('\0', ""));
    if message.trim().is_empty() && stack.trim().is_empty() {
        return Err(ApiError::bad("message or stack is required"));
    }
    let mut context = b.context.unwrap_or(Value::Null);
    let n3 = scrub::json(&mut context);
    let size = serde_json::to_vec(&context).map_or(0, |v| v.len());
    if size > MAX_CONTEXT_BYTES {
        context = json!({"_truncated": true, "_bytes": size});
    }
    let message = clip(message.trim(), MAX_MESSAGE);
    let stack = clip(stack.trim_end(), MAX_STACK);
    let signature = fingerprint::signature(&message, &stack);
    let fingerprint = fingerprint::fingerprint(app, &app_version, kind, &signature);
    let fingerprint = if kind.dedupes() {
        fingerprint
    } else {
        // Owner reports are never merged: their own fingerprint.
        format!("{fingerprint}-{}", &new_id()[3..11])
    };
    Ok(Report {
        id: new_id(),
        app,
        app_version,
        os,
        device,
        kind,
        message,
        stack,
        context,
        occurred_at,
        received_at: now,
        fingerprint,
        signature,
        count: 1,
        last_seen_at: now,
        last_occurred_at: occurred_at,
        recent: Vec::new(),
        attachments: Vec::new(),
        redactions: n1 + n2 + n3,
        client_id: b.client_id,
    })
}

/// A file part as received.
struct Upload {
    name: String,
    /// The part's field name (`minidump` forces that kind for binary data).
    field: String,
    bytes: Vec<u8>,
}

fn gunzip(data: &[u8], limit: u64) -> Result<Vec<u8>, ApiError> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(data)
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|_| ApiError::bad("a gzip part could not be decompressed"))?;
    if out.len() as u64 > limit {
        return Err(ApiError::too_large(format!(
            "attachments expand to more than {} MB",
            limit / MB
        )));
    }
    Ok(out)
}

fn is_gzip(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b
}

/// A file name safe in the report directory.
fn safe_name(raw: &str, n: usize, taken: &[Attachment]) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    name = name.trim_start_matches('.').chars().take(100).collect();
    if name.is_empty() || name == "report.json" || name.ends_with(".tmp") {
        name = format!("file{n}{}", if name.is_empty() { "" } else { ".txt" });
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_owned(), name[i..].to_owned()),
        _ => (name.clone(), String::new()),
    };
    let mut candidate = name;
    let mut i = 2;
    while taken.iter().any(|a| a.name == candidate) {
        candidate = format!("{stem}-{i}{ext}");
        i += 1;
    }
    candidate
}

/// Expands, classifies, scrubs and writes the files into `dir`.
fn write_files(
    uploads: Vec<Upload>,
    dir: &std::path::Path,
    expanded_limit: u64,
) -> Result<Vec<Attachment>, ApiError> {
    let mut out: Vec<Attachment> = Vec::new();
    let mut expanded = 0u64;
    for (n, u) in uploads.into_iter().enumerate() {
        let gzip = is_gzip(&u.bytes);
        let mut raw_name = u.name.clone();
        let data = if gzip {
            for ext in [".gz", ".gzip"] {
                if raw_name.len() > ext.len() && raw_name.to_ascii_lowercase().ends_with(ext) {
                    raw_name.truncate(raw_name.len() - ext.len());
                }
            }
            gunzip(&u.bytes, expanded_limit.saturating_sub(expanded))?
        } else {
            u.bytes
        };
        expanded += data.len() as u64;
        if expanded > expanded_limit {
            return Err(ApiError::too_large(format!(
                "attachments expand to more than {} MB",
                expanded_limit / MB
            )));
        }
        let head = &data[..data.len().min(8192)];
        let kind = if data.starts_with(b"MDMP") || (u.field == "minidump" && head.contains(&0)) {
            FileKind::Minidump
        } else if !head.contains(&0) {
            FileKind::Log
        } else {
            FileKind::Binary
        };
        let (bytes, redactions) = if kind.is_text() {
            let (clean, n) = scrub::text(&String::from_utf8_lossy(&data));
            (clean.into_bytes(), n)
        } else {
            (data, 0)
        };
        let name = safe_name(&raw_name, n + 1, &out);
        std::fs::write(dir.join(&name), &bytes).map_err(ApiError::internal)?;
        out.push(Attachment {
            name,
            kind,
            bytes: bytes.len() as u64,
            gzip,
            sha256: Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            redactions,
        });
    }
    Ok(out)
}

const FILE_FIELDS: [&str; 7] = [
    "file",
    "files",
    "log",
    "logs",
    "minidump",
    "attachment",
    "attachments",
];

async fn create(State(app): State<Arc<App>>, req: Request) -> R<(StatusCode, Json<Value>)> {
    let (parts, body) = req.into_parts();
    let cap = app.cfg.max_upload_bytes;
    let mut bytes = to_bytes(body, cap as usize + 64 * 1024)
        .await
        .map_err(|_| {
            ApiError::too_large(format!(
                "a report with its attachments is at most {} MB",
                cap / MB
            ))
        })?
        .to_vec();
    let encoding = parts
        .headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match encoding.as_str() {
        "" | "identity" => {}
        "gzip" | "x-gzip" => bytes = gunzip(&bytes, app.cfg.max_expanded_bytes + MB)?,
        _ => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_encoding",
                "Content-Encoding: gzip or none",
            ))
        }
    }
    let ctype = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (new, uploads) = if ctype.starts_with("application/json") {
        let r: NewReport = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::bad(format!("report JSON: {e}")))?;
        (r, Vec::new())
    } else if ctype.starts_with("multipart/form-data") {
        // Same parts (and body-limit setting), the expanded body.
        let mut parts = parts;
        parts.headers.remove(header::CONTENT_ENCODING);
        let rebuilt = Request::from_parts(parts, Body::from(bytes));
        let mut mp = Multipart::from_request(rebuilt, &())
            .await
            .map_err(|e| ApiError::bad(format!("multipart: {e}")))?;
        let mut report = None;
        let mut uploads = Vec::new();
        while let Some(field) = mp
            .next_field()
            .await
            .map_err(|e| ApiError::bad(format!("multipart: {e}")))?
        {
            let name = field.name().unwrap_or("").to_owned();
            let file_name = field.file_name().map(str::to_owned);
            if name == "report" {
                if report.is_some() {
                    return Err(ApiError::bad("multipart: one `report` part"));
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::bad(format!("multipart: {e}")))?;
                report = Some(
                    serde_json::from_slice::<NewReport>(&data)
                        .map_err(|e| ApiError::bad(format!("report JSON: {e}")))?,
                );
            } else if file_name.is_some() || FILE_FIELDS.contains(&name.as_str()) {
                if uploads.len() >= app.cfg.max_files {
                    return Err(ApiError::too_large(format!(
                        "at most {} files per report",
                        app.cfg.max_files
                    )));
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::bad(format!("multipart: {e}")))?;
                uploads.push(Upload {
                    name: file_name.unwrap_or_else(|| name.clone()),
                    field: name,
                    bytes: data.to_vec(),
                });
            }
        }
        let report =
            report.ok_or_else(|| ApiError::bad("multipart: the `report` part is missing"))?;
        (report, uploads)
    } else {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "application/json or multipart/form-data",
        ));
    };
    let mut report = build_report(new, Utc::now())?;
    let key = format!(
        "{}:{}",
        report.app.as_str(),
        if report.device.is_empty() {
            "unknown"
        } else {
            &report.device
        }
    );
    if let Err(wait) = app.limits.check(&key) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many reports from this device; retry later".into(),
            Some(wait.as_secs().max(1)),
        ));
    }
    let a = app.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Ingest, ApiError> {
        let dir = a.store.stage(&report.id).map_err(ApiError::internal)?;
        match write_files(uploads, &dir, a.cfg.max_expanded_bytes) {
            Ok(files) => report.attachments = files,
            Err(e) => {
                std::fs::remove_dir_all(&dir).ok();
                return Err(e);
            }
        }
        report.redactions += report.attachments.iter().map(|f| f.redactions).sum::<u32>();
        a.store.commit(report, &dir).map_err(ApiError::internal)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(match outcome {
        Ingest::Created(r) => {
            let notified = app.notifier.maybe_send(&r);
            tracing::info!(id = %r.id, app = r.app.as_str(), kind = r.kind.as_str(), files = r.attachments.len(), redactions = r.redactions, notified, "report stored");
            (
                StatusCode::CREATED,
                Json(
                    json!({"id": r.id, "duplicate": false, "count": 1, "fingerprint": r.fingerprint}),
                ),
            )
        }
        Ingest::Duplicate { id, count } => {
            tracing::info!(%id, count, "repeat counted");
            (
                StatusCode::OK,
                Json(json!({"id": id, "duplicate": true, "count": count})),
            )
        }
        Ingest::Replay { id, count } => (
            StatusCode::OK,
            Json(json!({"id": id, "duplicate": true, "count": count, "replay": true})),
        ),
    })
}

// -- reads ---------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ListParams {
    app: Option<String>,
    kind: Option<String>,
    since: Option<String>,
    until: Option<String>,
    device: Option<String>,
    fingerprint: Option<String>,
    limit: Option<usize>,
}

/// `2026-10-26T19:44:00Z`, `2026-10-26` (midnight in the owner's zone) or
/// `30m` / `24h` / `7d` back from now.
pub fn parse_time(s: &str, tz: chrono_tz::Tz, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return tz
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|t| t.with_timezone(&Utc));
    }
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok().filter(|n| *n >= 0 && *n <= 100_000)?;
    let d = match unit {
        "m" => CDuration::minutes(n),
        "h" => CDuration::hours(n),
        "d" => CDuration::days(n),
        _ => return None,
    };
    Some(now - d)
}

fn list_of<T>(
    field: &str,
    v: Option<&str>,
    parse: fn(&str) -> Option<T>,
) -> Result<Vec<T>, ApiError> {
    v.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| parse(x).ok_or_else(|| ApiError::bad(format!("{field}: unknown value {x:?}"))))
            .collect()
    })
    .unwrap_or(Ok(Vec::new()))
}

async fn list(State(app): State<Arc<App>>, Query(p): Query<ListParams>) -> R {
    let tz = app.cfg.timezone;
    let now = Utc::now();
    let time = |field: &str, v: &Option<String>| -> Result<Option<DateTime<Utc>>, ApiError> {
        match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => parse_time(s, tz, now).map(Some).ok_or_else(|| {
                ApiError::bad(format!(
                    "{field}: RFC 3339, YYYY-MM-DD, or a span like 30m, 24h, 7d"
                ))
            }),
        }
    };
    let q = ListQuery {
        apps: list_of("app", p.app.as_deref(), AppName::parse)?,
        kinds: list_of("kind", p.kind.as_deref(), Kind::parse)?,
        since: time("since", &p.since)?,
        until: time("until", &p.until)?,
        device: p.device.filter(|d| !d.trim().is_empty()),
        fingerprint: p.fingerprint.filter(|d| !d.trim().is_empty()),
        limit: p.limit.unwrap_or(DEFAULT_LIST).clamp(1, 200),
    };
    let a = app.clone();
    let reports = tokio::task::spawn_blocking(move || a.store.list(&q))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(json!({"reports": reports, "timeZone": tz.name()})))
}

fn files_json(r: &Report) -> Value {
    Value::Array(
        r.attachments
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "kind": f.kind,
                    "bytes": f.bytes,
                    "gzip": f.gzip,
                    "text": f.kind.is_text(),
                    "sha256": f.sha256,
                    "url": format!("/v1/diagnostics/reports/{}/files/{}", r.id, f.name),
                })
            })
            .collect(),
    )
}

async fn detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let a = app.clone();
    let got = tokio::task::spawn_blocking(move || a.store.get(&id))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let (r, _) = got.ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({
        "attachments": files_json(&r),
        "report": r,
        "timeZone": app.cfg.timezone.name(),
    })))
}

#[derive(Deserialize, Default)]
struct FileParams {
    tail: Option<u64>,
    head: Option<u64>,
}

/// Reads `[start, start+len)` of a file.
fn read_range(p: &std::path::Path, start: u64, len: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    f.seek(SeekFrom::Start(start))?;
    let mut out = Vec::with_capacity(len.min(64 * MB) as usize);
    f.take(len).read_to_end(&mut out)?;
    Ok(out)
}

async fn file(
    State(app): State<Arc<App>>,
    Path((id, name)): Path<(String, String)>,
    Query(p): Query<FileParams>,
) -> Result<Response, ApiError> {
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let a = app.clone();
    let got = tokio::task::spawn_blocking(move || a.store.file(&id, &name))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let (att, path) = got.ok_or_else(ApiError::not_found)?;
    let size = att.bytes;
    let (start, len) = match (p.head, p.tail) {
        (Some(_), Some(_)) => return Err(ApiError::bad("head or tail, not both")),
        (Some(h), None) => (0, h.min(size)),
        (None, Some(t)) => (size.saturating_sub(t), t.min(size)),
        (None, None) => (0, size),
    };
    let mut body = tokio::task::spawn_blocking(move || read_range(&path, start, len))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    if att.kind.is_text() && start > 0 {
        // Start at a whole line.
        if let Some(i) = body.iter().position(|b| *b == b'\n') {
            if i + 1 < body.len() {
                body.drain(..=i);
            }
        }
    }
    let truncated = (body.len() as u64) < size;
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if att.kind.is_text() {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        }),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", att.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert("x-diagnostics-size", HeaderValue::from(size));
    h.insert(
        "x-diagnostics-truncated",
        HeaderValue::from_static(if truncated { "true" } else { "false" }),
    );
    Ok((h, body).into_response())
}

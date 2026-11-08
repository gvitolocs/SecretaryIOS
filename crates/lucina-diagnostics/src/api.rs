//! `/v1/diagnostics/reports*` HTTP API (docs/diagnostics.md).
//!
//! Auth (as `/v1/memos`): requests from the clients arrive through Caddy on
//! p1, which first asks secretary-server whether the device token or cookie
//! is valid (`forward_auth`) and then adds `X-Diagnostics-Gateway: <shared
//! secret>`. Local callers (secretary-mcp, Lucina's tools) use
//! `Authorization: Bearer <service token>`. Cookie-authenticated writes need
//! `X-Secretary-CSRF: 1`, like secretary-server.
//!
//! - `POST /v1/diagnostics/reports`: JSON, or multipart with a `report` part
//!   and attachments → 201 `{id, duplicate: false, count: 1}`, or 200
//!   `{id, duplicate: true, count}` for a repeat of a stored fingerprint.
//! - `GET /v1/diagnostics/reports?app&kind&device&since&before&limit` →
//!   `{reports, next}` (newest `last_seen_at` first).
//! - `GET /v1/diagnostics/reports/{id}` → `{report, count, last_seen_at,
//!   last_device, attachments}`.
//! - `GET /v1/diagnostics/reports/{id}/files/{name}[?tail=N|head=N]`.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration as CDuration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::ingest::{self, IngestError};
use crate::model::{AppName, Kind};
use crate::store::{valid_id, Filter, StoreError};
use crate::{notify, App};

pub struct ApiError(StatusCode, &'static str, String, Option<u64>);

impl ApiError {
    fn new(s: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self(s, code, msg.into(), None)
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    fn not_found(what: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no such {what}"),
        )
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        tracing::error!(error = %e, "diagnostics store");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "diagnostics store unavailable",
        )
    }
}

impl From<IngestError> for ApiError {
    fn from(e: IngestError) -> Self {
        match e {
            IngestError::Bad(m) => Self::bad(m),
            IngestError::TooLarge(m) => Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", m),
            IngestError::Unsupported(m) => {
                Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported", m)
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": {"code": self.1, "message": self.2}});
        if let Some(n) = self.3 {
            body["retry_after"] = json!(n);
        }
        let mut r = (self.0, Json(body)).into_response();
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
        .route_layer(middleware::from_fn_with_state(app.clone(), auth));
    Router::new()
        .route("/health", get(health))
        .merge(api)
        // The handler reads the body itself, up to the attachment cap.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app)
}

async fn health(State(app): State<Arc<App>>) -> R {
    let (reports, occurrences) = app.store.count()?;
    Ok(Json(json!({
        "ok": true,
        "reports": reports,
        "occurrences": occurrences,
        "retention_days": app.cfg.retention_days,
        "notify": app.cfg.notify.enabled(),
    })))
}

async fn create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Body,
) -> R<(StatusCode, Json<Value>)> {
    let limits = app.cfg.limits;
    let bytes = axum::body::to_bytes(body, limits.body_bytes())
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                format!("the body is over {} bytes", limits.body_bytes()),
            )
        })?;
    let h = |n: header::HeaderName| headers.get(n).and_then(|v| v.to_str().ok());
    let content_type = h(header::CONTENT_TYPE).unwrap_or("application/json");
    let (incoming, files) =
        ingest::parse(&limits, content_type, h(header::CONTENT_ENCODING), bytes).await?;
    let key = format!("{}:{}", incoming.app.trim(), incoming.device.trim());
    if let Err(wait) = app.limiter.check(&key) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!("too many reports from this device; retry in {wait} s"),
            Some(wait),
        ));
    }
    let a = app.clone();
    let (report, ingested) = tokio::task::spawn_blocking(move || {
        let prepared = ingest::prepare(&a.cfg.limits, incoming, files, Utc::now())?;
        let report = prepared.report.clone();
        let ingested = a.store.ingest(prepared)?;
        Ok::<_, ApiError>((report, ingested))
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "diagnostics ingest task");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "ingest failed",
        )
    })??;
    if ingested.duplicate {
        tracing::info!(id = %ingested.id, count = ingested.count, app = report.app.as_str(), "diagnostics repeat");
    } else {
        tracing::info!(
            id = %ingested.id,
            app = report.app.as_str(),
            kind = report.kind.as_str(),
            attachments = report.attachments.len(),
            redactions = report.redactions,
            "diagnostics report stored"
        );
        if report.kind.notifies() && app.cfg.notify.enabled() {
            if let Some(held) = app.gate.admit(report.app, std::time::Instant::now()) {
                let cfg = app.cfg.notify.clone();
                let (title, body) = notify::message(&report, app.cfg.timezone, held);
                tokio::spawn(async move {
                    match notify::send(&cfg, &report, &title, &body).await {
                        Ok(sent) => tracing::info!(id = %report.id, ?sent, "crash notified"),
                        Err(e) => {
                            tracing::warn!(id = %report.id, error = %e, "crash notification failed")
                        }
                    }
                });
            }
        }
    }
    Ok((
        if ingested.duplicate {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(json!({
            "id": ingested.id,
            "duplicate": ingested.duplicate,
            "count": ingested.count,
        })),
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    app: Option<String>,
    kind: Option<String>,
    device: Option<String>,
    /// RFC 3339, `YYYY-MM-DD` (owner's zone), or a span back from now: `30m`, `24h`, `7d`.
    since: Option<String>,
    /// Pagination: the `next` cursor of the previous page.
    before: Option<String>,
    limit: Option<usize>,
}

/// `since`: RFC 3339, a date in the owner's zone, or `<n>m|h|d` ago.
pub fn parse_since(s: &str, tz: chrono_tz::Tz, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
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
    let (i, _) = s.char_indices().last()?;
    let (n, unit) = s.split_at(i);
    let n: i64 = n.parse().ok().filter(|n| (1..=100_000).contains(n))?;
    let span = match unit {
        "m" => CDuration::minutes(n),
        "h" => CDuration::hours(n),
        "d" => CDuration::days(n),
        _ => return None,
    };
    Some(now - span)
}

async fn list(State(app): State<Arc<App>>, Query(q): Query<ListQuery>) -> R {
    let blank = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let app_name = blank(q.app);
    if let Some(a) = &app_name {
        AppName::parse(a).ok_or_else(|| {
            ApiError::bad("app: one of secretary-code, ios, android, web, server")
        })?;
    }
    let kind = blank(q.kind);
    if let Some(k) = &kind {
        Kind::parse(k).ok_or_else(|| {
            ApiError::bad("kind: one of crash, fatal_error, error, user_report, hang")
        })?;
    }
    let since = blank(q.since)
        .map(|s| {
            parse_since(&s, app.cfg.timezone, Utc::now())
                .ok_or_else(|| ApiError::bad("since: RFC 3339, YYYY-MM-DD, or 30m / 24h / 7d"))
        })
        .transpose()?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let filter = Filter {
        app: app_name,
        kind,
        device: blank(q.device),
        since,
        before: blank(q.before),
        limit,
    };
    let a = app.clone();
    let (reports, next) = tokio::task::spawn_blocking(move || a.store.list(&filter))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "list"))??;
    Ok(Json(json!({"reports": reports, "next": next})))
}

async fn detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    if !valid_id(&id) {
        return Err(ApiError::not_found("report"));
    }
    let a = app.clone();
    let got = tokio::task::spawn_blocking(move || a.store.get(&id))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "get"))??;
    let (report, summary) = got.ok_or_else(|| ApiError::not_found("report"))?;
    let attachments: Vec<Value> = report
        .attachments
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "size": f.size,
                "content_type": f.content_type,
                "text": f.text,
                "gzip": f.gzip,
                "sha256": f.sha256,
                "url": format!("/v1/diagnostics/reports/{}/files/{}", report.id, f.name),
            })
        })
        .collect();
    Ok(Json(json!({
        "report": report,
        "count": summary.count,
        "last_seen_at": summary.last_seen_at,
        "last_device": summary.last_device,
        "attachments": attachments,
    })))
}

#[derive(Deserialize)]
struct FileQuery {
    /// Text files: only the last N bytes.
    tail: Option<u64>,
    /// Text files: only the first N bytes.
    head: Option<u64>,
}

async fn file(
    State(app): State<Arc<App>>,
    Path((id, name)): Path<(String, String)>,
    Query(q): Query<FileQuery>,
) -> R<Response> {
    if !valid_id(&id) {
        return Err(ApiError::not_found("attachment"));
    }
    let a = app.clone();
    let found = tokio::task::spawn_blocking(move || a.store.file(&id, &name))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "file"))??;
    let (path, att) = found.ok_or_else(|| ApiError::not_found("attachment"))?;
    let mut f = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("attachment"))?;
    let len = f
        .metadata()
        .await
        .map_err(|e| ApiError::from(StoreError::Io(e)))?
        .len();
    if (q.tail.is_some() || q.head.is_some()) && !att.text {
        return Err(ApiError::bad("tail/head: text attachments only"));
    }
    let (start, count) = match (q.tail, q.head) {
        (Some(_), Some(_)) => return Err(ApiError::bad("tail or head, not both")),
        (Some(n), None) => (len.saturating_sub(n), n.min(len)),
        (None, Some(n)) => (0, n.min(len)),
        (None, None) => (0, len),
    };
    let mut r = if count < len {
        // A slice of a text file: cut to whole UTF-8 characters.
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| ApiError::from(StoreError::Io(e)))?;
        let mut buf = Vec::with_capacity(count as usize);
        (&mut f)
            .take(count)
            .read_to_end(&mut buf)
            .await
            .map_err(|e| ApiError::from(StoreError::Io(e)))?;
        let text = String::from_utf8_lossy(&buf);
        let text = text
            .trim_start_matches('\u{fffd}')
            .trim_end_matches('\u{fffd}');
        let mut r = Response::new(Body::from(text.to_owned()));
        r.headers_mut().insert(
            "x-slice",
            HeaderValue::from_static(if q.tail.is_some() { "tail" } else { "head" }),
        );
        r
    } else {
        let stream = tokio_util::io::ReaderStream::new(f);
        let mut r = Response::new(Body::from_stream(stream));
        r.headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        r
    };
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if att.text {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        }),
    );
    h.insert("x-full-size", HeaderValue::from(len));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", att.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_forms() {
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        let tz = chrono_tz::Europe::Copenhagen;
        assert_eq!(
            parse_since("24h", tz, now),
            Some(now - CDuration::hours(24))
        );
        assert_eq!(parse_since("7d", tz, now), Some(now - CDuration::days(7)));
        assert_eq!(
            parse_since("2026-10-26", tz, now),
            Some(Utc.with_ymd_and_hms(2026, 10, 25, 23, 0, 0).unwrap())
        );
        assert_eq!(
            parse_since("2026-10-26T10:00:00+02:00", tz, now),
            Some(Utc.with_ymd_and_hms(2026, 10, 26, 8, 0, 0).unwrap())
        );
        for bad in ["", "h", "0d", "5y", "yesterday", "7é"] {
            assert_eq!(parse_since(bad, tz, now), None, "{bad}");
        }
    }
}

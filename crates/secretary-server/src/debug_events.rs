//! Debug event log of the owner's apps (`docs/debug-events.md`): what
//! happened on the phone (taps, intents, notifications, Face ID, failed
//! requests), readable from the server without USB.
//!
//! - `POST /v1/debug/events {events: [...]}` (a device token): at most
//!   [`MAX_BATCH`] events, a body of at most [`BODY_LIMIT`]. Each event is
//!   `{ts, seq, build, process, type, fields}`; `device` is the
//!   authenticated device, whatever the client sent. [`sanitize`] runs
//!   again here: an event with a forbidden field name (secrets, message or
//!   note text, bodies, URLs, see [`FORBIDDEN_WORDS`]) or a bad shape is
//!   refused, secret-looking strings become `[redacted]`, URLs lose their
//!   query, ids in `path` / `route` become `{id}`. The rest is appended to
//!   `debug-events.jsonl` in the data dir (0600) with `received_at`; at
//!   [`ROTATE_AT`] it moves to `.1` (`.1` to `.2`, `.2` to `.3`, the old
//!   `.3` is deleted). Own per-device rate limit of [`BATCHES_PER_MINUTE`].
//! - `GET /v1/debug/events?device=&since=&until=&type=&limit=` (any device
//!   token or the service token): matching events, oldest first and newest
//!   last, the last `limit` of them. `device` is a device id or part of a
//!   device name (`iphone`); `type` a prefix (`net`, `auth.`,
//!   `notification.action`); `since` / `until` RFC 3339, `YYYY-MM-DD` (owner's
//!   zone) or `30m` / `24h` / `7d` back from now.
//!
//! The shape reuses the device-bridge conventions (`ts`, dotted `type`,
//! `device` = the device id, as `secretary-core`'s `DeviceEvent`), the JSONL
//! storage of [`crate::voice_telemetry`] and the secret key names of
//! lucina-diagnostics' scrubber.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::auth::{Identity, SERVICE_DEVICE_ID};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::ratelimit::RateLimiter;
use crate::AppState;

/// Events in one `POST`.
pub const MAX_BATCH: usize = 200;
/// Body limit of `POST /v1/debug/events`.
pub const BODY_LIMIT: usize = 256 * 1024;
/// `debug-events.jsonl` moves aside at this size.
pub const ROTATE_AT: u64 = 50 * 1024 * 1024;
/// Rotated files kept (`.1` .. `.3`).
pub const KEEP_ROTATED: usize = 3;
/// `POST`s per device and minute (burst), refilled continuously.
pub const BATCHES_PER_MINUTE: u32 = 30;
/// Fields kept per event.
pub const MAX_FIELDS: usize = 40;
/// Characters kept of a string value.
pub const MAX_VALUE_CHARS: usize = 200;
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 2000;
const FILE: &str = "debug-events.jsonl";
const REDACTED: &str = "[redacted]";

/// A field whose name contains one of these words (split at `_`, `.`, `-`
/// and camelCase, case-insensitive) is never recorded: secrets, and the
/// content of messages, notes, commands and plans, bodies and URLs.
pub const FORBIDDEN_WORDS: &[&str] = &[
    // secrets
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "passwd",
    "passphrase",
    "key",
    "keys",
    "signature",
    "sig",
    "cookie",
    "cookies",
    "authorization",
    "credential",
    "credentials",
    "otp",
    "pin",
    "nonce",
    "jwt",
    "bearer",
    "private",
    // content
    "body",
    "text",
    "message",
    "messages",
    "msg",
    "content",
    "note",
    "notes",
    "command",
    "commands",
    "cmd",
    "plan",
    "prompt",
    "transcript",
    "chat",
    "reply",
    "title",
    "subtitle",
    "summary",
    "description",
    "detail",
    "details",
    "payload",
    "email",
    "address",
    // URLs and their queries
    "url",
    "urls",
    "uri",
    "query",
    "querystring",
];

/// Why an event was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Shape(&'static str),
    ForbiddenField(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Shape(what) => write!(f, "bad_{what}"),
            Refused::ForbiddenField(k) => write!(f, "forbidden_field:{k}"),
        }
    }
}

/// The words of a field name: `authToken` → `auth`, `token`; `unlock_key.id` → `unlock`, `key`, `id`.
fn words(key: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '.' || c == '-' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether a field name may be recorded.
pub fn forbidden_key(key: &str) -> bool {
    words(key)
        .iter()
        .any(|w| FORBIDDEN_WORDS.contains(&w.as_str()))
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 48
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

/// `ui.tap`, `notification.action`, `activity.start`.
pub fn valid_type(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.split('.').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn valid_token(s: &str, max: usize, extra: &[u8]) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

/// A string that looks like a credential: auth schemes, our token prefixes,
/// JWTs, or a long unbroken run of base64 / hex.
fn secret_like(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if ["bearer ", "basic ", "digest "]
        .iter()
        .any(|p| lower.contains(p))
    {
        return true;
    }
    if s.split(|c: char| c.is_whitespace() || c == '/' || c == ',')
        .any(|w| {
            ["sec_", "svc_", "mcp_", "eyJ"]
                .iter()
                .any(|p| w.starts_with(p))
        })
    {
        return true;
    }
    s.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '=' | '_' | '-')))
        .any(|run| run.len() >= 40)
}

/// `/v1/approvals/ap_8f3a19c2d4/decide` → `/v1/approvals/{id}/decide`. Note and file names are content: the
/// segment after `notes` / `files` becomes `{id}` too.
pub fn template_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').collect();
    segs.iter()
        .enumerate()
        .map(|(i, seg)| {
            let after_content = i > 0 && matches!(segs[i - 1], "notes" | "files");
            let id_like = !seg.is_empty()
                && !(seg.starts_with('{') && seg.ends_with('}'))
                && (after_content
                    || seg.chars().count() > 40
                    || seg.bytes().all(|b| b.is_ascii_digit())
                    || (seg.len() >= 8
                        && seg.bytes().any(|b| b.is_ascii_digit())
                        && seg
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                    || seg.contains('@')
                    || seg.contains('%'));
            if id_like {
                "{id}"
            } else {
                *seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A clean string value: no control characters, URL query/fragment and
/// credentials gone, at most [`MAX_VALUE_CHARS`].
fn clean_string(key: &str, s: &str) -> String {
    let mut s: String = s.chars().filter(|c| !c.is_control()).collect();
    if s.contains("://") {
        if let Some(i) = s.find(['?', '#']) {
            s.truncate(i);
        }
        if let Some(scheme_end) = s.find("://") {
            let rest = &s[scheme_end + 3..];
            if let Some(at) = rest.find('@').filter(|&at| !rest[..at].contains('/')) {
                s = format!("{}://{}", &s[..scheme_end], &rest[at + 1..]);
            }
        }
    }
    if matches!(key, "path" | "route") || key.ends_with("_path") || key.ends_with("_route") {
        s = template_path(&s);
    }
    if secret_like(&s) {
        return REDACTED.into();
    }
    s.chars().take(MAX_VALUE_CHARS).collect()
}

/// The fields as recorded: flat string / number / bool only (anything else is
/// dropped), at most [`MAX_FIELDS`]. `Err` on a forbidden field name.
pub fn sanitize_fields(fields: &Map<String, Value>) -> Result<Map<String, Value>, Refused> {
    let mut out = Map::new();
    for (k, v) in fields {
        if forbidden_key(k) {
            return Err(Refused::ForbiddenField(k.chars().take(48).collect()));
        }
        if !valid_key(k) || out.len() >= MAX_FIELDS {
            continue;
        }
        let clean = match v {
            Value::String(s) => Value::String(clean_string(k, s)),
            Value::Bool(b) => Value::Bool(*b),
            Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => Value::Number(n.clone()),
            _ => continue,
        };
        out.insert(k.clone(), clean);
    }
    Ok(out)
}

fn ms_to_rfc(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

/// RFC 3339 in the fixed-width UTC form events are stored in, so strings compare as times.
fn normalize_time(s: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| ms_to_rfc(d.timestamp_millis()))
}

/// One event from a client, cleaned, ready to store (without `received_at`).
pub fn sanitize(ev: &Value, ident: &Identity) -> Result<Map<String, Value>, Refused> {
    let obj = ev.as_object().ok_or(Refused::Shape("event"))?;
    let ts = obj
        .get("ts")
        .and_then(Value::as_str)
        .and_then(normalize_time)
        .ok_or(Refused::Shape("ts"))?;
    let seq = obj
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or(Refused::Shape("seq"))?;
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| valid_type(t))
        .ok_or(Refused::Shape("type"))?;
    let process = obj
        .get("process")
        .and_then(Value::as_str)
        .filter(|p| valid_token(p, 32, b"_-"))
        .ok_or(Refused::Shape("process"))?;
    let build = match obj.get("build") {
        None | Some(Value::Null) => "unknown",
        Some(Value::String(b)) if valid_token(b, 40, b"._+-") => b,
        Some(_) => return Err(Refused::Shape("build")),
    };
    let fields = match obj.get("fields") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(f)) => sanitize_fields(f)?,
        Some(_) => return Err(Refused::Shape("fields")),
    };
    let mut out = Map::new();
    out.insert("ts".into(), json!(ts));
    out.insert("seq".into(), json!(seq));
    out.insert("device".into(), json!(ident.device_id));
    out.insert("device_name".into(), json!(ident.device_name));
    out.insert("build".into(), json!(build));
    out.insert("process".into(), json!(process));
    out.insert("type".into(), json!(kind));
    out.insert("fields".into(), Value::Object(fields));
    Ok(out)
}

// -- storage ---------------------------------------------------------------------

/// What `GET /v1/debug/events` asks for.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// A device id, or part of a device name (case-insensitive).
    pub device: Option<String>,
    /// Events at or after this (normalized RFC 3339).
    pub since: Option<String>,
    /// Events at or before this (normalized RFC 3339).
    pub until: Option<String>,
    /// A prefix of `type`.
    pub kind: Option<String>,
    pub limit: usize,
}

impl Query {
    fn matches(&self, v: &Value) -> bool {
        let ts = v["ts"].as_str().unwrap_or("");
        if self.since.as_deref().is_some_and(|s| ts < s)
            || self.until.as_deref().is_some_and(|u| ts > u)
        {
            return false;
        }
        if let Some(k) = &self.kind {
            if !v["type"]
                .as_str()
                .is_some_and(|t| t.starts_with(k.as_str()))
            {
                return false;
            }
        }
        if let Some(d) = &self.device {
            let id = v["device"].as_str().unwrap_or("");
            let name = v["device_name"].as_str().unwrap_or("").to_lowercase();
            if id != d && !name.contains(&d.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

pub struct Store {
    dir: PathBuf,
    rotate_at: u64,
    /// Size of the current file; `None` until first used.
    len: Mutex<Option<u64>>,
    limiter: RateLimiter,
}

impl Store {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.to_owned(),
            rotate_at: ROTATE_AT,
            len: Mutex::new(None),
            limiter: RateLimiter::new(BATCHES_PER_MINUTE),
        }
    }

    pub fn with_rotation(mut self, bytes: u64) -> Self {
        self.rotate_at = bytes;
        self
    }

    fn lock(&self) -> MutexGuard<'_, Option<u64>> {
        self.len.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn path(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.dir.join(FILE)
        } else {
            self.dir.join(format!("{FILE}.{n}"))
        }
    }

    fn rotate(&self) -> std::io::Result<()> {
        let _ = fs::remove_file(self.path(KEEP_ROTATED));
        for n in (0..KEEP_ROTATED).rev() {
            match fs::rename(self.path(n), self.path(n + 1)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Append clean events, each with `received_at`.
    pub fn append(&self, events: &[Map<String, Value>], now: DateTime<Utc>) -> std::io::Result<()> {
        let received = ms_to_rfc(now.timestamp_millis());
        let mut text = String::new();
        for ev in events {
            let mut ev = ev.clone();
            ev.insert("received_at".into(), json!(received));
            text.push_str(&Value::Object(ev).to_string());
            text.push('\n');
        }
        let mut len = self.lock();
        fs::create_dir_all(&self.dir)?;
        let current = match *len {
            Some(n) => n,
            None => fs::metadata(self.path(0)).map_or(0, |m| m.len()),
        };
        let current = if current > 0 && current + text.len() as u64 > self.rotate_at {
            self.rotate()?;
            0
        } else {
            current
        };
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.path(0))?;
        f.write_all(text.as_bytes())?;
        *len = Some(current + text.len() as u64);
        Ok(())
    }

    /// The last `limit` matching events, oldest first. Files are read newest
    /// first and older ones only while fewer than `limit` matched.
    pub fn query(&self, q: &Query) -> (Vec<Value>, bool) {
        let limit = q.limit.clamp(1, MAX_LIMIT);
        let _guard = self.lock(); // no rotation while reading
        let mut out: Vec<Value> = vec![];
        for n in 0..=KEEP_ROTATED {
            let Ok(f) = fs::File::open(self.path(n)) else {
                continue;
            };
            let mut found: Vec<Value> = BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
                .filter(|v| q.matches(v))
                .collect();
            found.append(&mut out);
            out = found;
            if out.len() >= limit {
                break;
            }
        }
        // Stable: equal times keep their order of arrival.
        out.sort_by(|a, b| a["ts"].as_str().cmp(&b["ts"].as_str()));
        let more = out.len() > limit;
        let cut = out.len().saturating_sub(limit);
        (out.split_off(cut), more)
    }
}

// -- HTTP ------------------------------------------------------------------------

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

async fn post_events(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    if ident.device_id == SERVICE_DEVICE_ID {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "devices_only",
            "debug events come from the owner's devices",
        ));
    }
    if let Err(wait) = st.debug_events.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("events: an array"))?;
    if events.len() > MAX_BATCH {
        return Err(bad(format!("events: at most {MAX_BATCH}")));
    }
    let mut clean = vec![];
    let mut refused = vec![];
    let mut max_seq: Option<u64> = None;
    for ev in events {
        if let Some(s) = ev.get("seq").and_then(Value::as_u64) {
            max_seq = Some(max_seq.map_or(s, |m| m.max(s)));
        }
        match sanitize(ev, &ident) {
            Ok(e) => clean.push(e),
            Err(why) => refused.push(json!({"seq": ev.get("seq"), "reason": why.to_string()})),
        }
    }
    if !refused.is_empty() {
        tracing::warn!(device = %ident.device_id, refused = refused.len(), "debug events refused");
    }
    if !clean.is_empty() {
        st.debug_events.append(&clean, Utc::now()).map_err(|e| {
            tracing::error!(error = %e, "debug events not stored");
            ApiError::internal("debug events not stored")
        })?;
    }
    refused.truncate(20);
    Ok(Json(json!({
        "accepted": clean.len(),
        "refused": refused,
        "max_seq": max_seq,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetParams {
    device: Option<String>,
    since: Option<String>,
    until: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<usize>,
}

/// RFC 3339, `YYYY-MM-DD` (start of that day in `tz`) or `30m` / `24h` / `7d` before `now`.
pub fn parse_when(s: &str, now: DateTime<Utc>, tz: chrono_tz::Tz) -> Option<String> {
    let s = s.trim();
    if let Some(t) = normalize_time(s) {
        return Some(t);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let start = tz
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .earliest()?;
        return Some(ms_to_rfc(start.timestamp_millis()));
    }
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok().filter(|n| (0..=100_000).contains(n))?;
    let back = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => return None,
    };
    Some(ms_to_rfc((now - back).timestamp_millis()))
}

async fn get_events(
    State(st): State<AppState>,
    QueryArgs(p): QueryArgs<GetParams>,
) -> Result<Json<Value>, ApiError> {
    let now = Utc::now();
    let tz = st.links.tz;
    let when = |name: &str, v: Option<String>| -> Result<Option<String>, ApiError> {
        v.filter(|s| !s.trim().is_empty())
            .map(|s| {
                parse_when(&s, now, tz)
                    .ok_or_else(|| bad(format!("{name}: RFC 3339, YYYY-MM-DD or 30m / 24h / 7d")))
            })
            .transpose()
    };
    let kind = p.kind.filter(|k| !k.is_empty());
    if kind.as_deref().is_some_and(|k| {
        k.len() > 64
            || !k
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    }) {
        return Err(bad("type: a prefix of lowercase letters, digits, _ and ."));
    }
    let device = p
        .device
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty());
    if device.as_deref().is_some_and(|d| d.chars().count() > 128) {
        return Err(bad("device: at most 128 characters"));
    }
    let q = Query {
        device,
        since: when("since", p.since)?,
        until: when("until", p.until)?,
        kind,
        limit: p.limit.unwrap_or(DEFAULT_LIMIT),
    };
    let store = st.debug_events.clone();
    let (events, more) = tokio::task::spawn_blocking(move || store.query(&q))
        .await
        .map_err(|_| ApiError::internal("debug events unreadable"))?;
    Ok(Json(
        json!({"count": events.len(), "more": more, "events": events}),
    ))
}

/// Behind `require_device_or_service`.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/debug/events",
        get(get_events)
            .post(post_events)
            .layer(DefaultBodyLimit::max(BODY_LIMIT)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Via;

    fn ident() -> Identity {
        Identity {
            device_id: "dev_phone".into(),
            device_name: "Giuseppe's iPhone".into(),
            via: Via::Bearer,
        }
    }

    fn ev(seq: u64, kind: &str, fields: Value) -> Value {
        json!({"ts": "2026-11-01T10:00:00.250+01:00", "seq": seq, "device": "spoofed",
               "build": "adfd5fe", "process": "app", "type": kind, "fields": fields})
    }

    #[test]
    fn a_clean_event_keeps_its_shape_and_the_real_device() {
        let e = sanitize(
            &ev(
                7,
                "ui.tap",
                json!({"element": "approve", "screen": "approval_sheet", "ok": true, "ms": 12.5}),
            ),
            &ident(),
        )
        .unwrap();
        assert_eq!(e["ts"], "2026-11-01T09:00:00.250Z");
        assert_eq!(e["seq"], 7);
        assert_eq!(e["device"], "dev_phone");
        assert_eq!(e["device_name"], "Giuseppe's iPhone");
        assert_eq!(e["type"], "ui.tap");
        assert_eq!(
            e["fields"],
            json!({"element": "approve", "screen": "approval_sheet", "ok": true, "ms": 12.5})
        );
    }

    #[test]
    fn forbidden_field_names_refuse_the_event() {
        for key in [
            "token",
            "authToken",
            "device_token",
            "unlock_key",
            "privateKey",
            "signature",
            "password",
            "Cookie",
            "message",
            "message_text",
            "note_text",
            "command",
            "plan",
            "body",
            "response_body",
            "url",
            "requestURL",
            "query",
            "title",
            "otp_code",
            "apiKey",
        ] {
            let r = sanitize(&ev(1, "approval.decide", json!({key: "x"})), &ident());
            assert_eq!(r, Err(Refused::ForbiddenField(key.into())), "{key}");
        }
        for key in [
            "id",
            "action",
            "sensitive",
            "decision",
            "outcome",
            "error",
            "error_code",
            "status",
            "method",
            "path",
            "duration_ms",
            "keyboard_shown",
            "monkey",
        ] {
            assert!(!forbidden_key(key), "{key}");
        }
    }

    #[test]
    fn values_are_cleaned_and_secrets_redacted() {
        let e = sanitize(
            &ev(
                2,
                "net.request",
                json!({
                    "path": "/v1/approvals/ap_8f3a19c2d4e5/decide?pin=1234",
                    "route": "/v1/notes/12345",
                    "endpoint": "https://user:pw@getsecretary.tech/v1/x?token=abc#frag",
                    "error": "Bearer sec_abcdef",
                    "raw": "a".repeat(64),
                    "jwt": "x",
                }),
            ),
            &ident(),
        );
        assert_eq!(e, Err(Refused::ForbiddenField("jwt".into())));
        let e = sanitize(
            &ev(
                2,
                "net.request",
                json!({
                    "path": "/v1/approvals/ap_8f3a19c2d4e5/decide?pin=1234",
                    "route": "/v1/notes/12345",
                    "endpoint": "https://user:pw@getsecretary.tech/v1/x?token=abc#frag",
                    "error": "Bearer sec_abcdef",
                    "raw": "a".repeat(64),
                    "long": "word ".repeat(100),
                    "nested": {"a": 1},
                    "list": [1],
                    "nothing": null,
                    "bad key!": "x",
                }),
            ),
            &ident(),
        )
        .unwrap();
        let f = &e["fields"];
        assert_eq!(f["path"], "/v1/approvals/{id}/decide");
        assert_eq!(f["route"], "/v1/notes/{id}");
        assert_eq!(f["endpoint"], "https://getsecretary.tech/v1/x");
        assert_eq!(f["error"], REDACTED);
        assert_eq!(f["raw"], REDACTED);
        assert_eq!(f["long"].as_str().unwrap().chars().count(), MAX_VALUE_CHARS);
        for gone in ["nested", "list", "nothing", "bad key!"] {
            assert!(f.get(gone).is_none(), "{gone}");
        }
    }

    #[test]
    fn bad_shapes_are_refused() {
        let base = ev(1, "ui.tap", json!({}));
        let with = |k: &str, v: Value| {
            let mut e = base.clone();
            e[k] = v;
            sanitize(&e, &ident())
        };
        assert_eq!(with("ts", json!("yesterday")), Err(Refused::Shape("ts")));
        assert_eq!(with("seq", json!(-1)), Err(Refused::Shape("seq")));
        assert_eq!(with("type", json!("UI.Tap")), Err(Refused::Shape("type")));
        assert_eq!(with("type", json!("ui..tap")), Err(Refused::Shape("type")));
        assert_eq!(with("process", json!("")), Err(Refused::Shape("process")));
        assert_eq!(
            with("build", json!("abc def")),
            Err(Refused::Shape("build"))
        );
        assert_eq!(with("fields", json!([1])), Err(Refused::Shape("fields")));
        assert_eq!(
            sanitize(&json!([1]), &ident()),
            Err(Refused::Shape("event"))
        );
        let mut no_build = base.clone();
        no_build.as_object_mut().unwrap().remove("build");
        assert_eq!(sanitize(&no_build, &ident()).unwrap()["build"], "unknown");
    }

    #[test]
    fn paths_get_ids_templated() {
        assert_eq!(template_path("/v1/approvals/{id}"), "/v1/approvals/{id}");
        assert_eq!(
            template_path("/v1/devices/dev_3fa2b1c9/unlock"),
            "/v1/devices/{id}/unlock"
        );
        assert_eq!(
            template_path("/v1/x/3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
            "/v1/x/{id}"
        );
        assert_eq!(template_path("/v1/notes/shopping-list"), "/v1/notes/{id}");
        assert_eq!(template_path("/v1/shopping/lists"), "/v1/shopping/lists");
        assert_eq!(template_path("/v1/people/a%40b"), "/v1/people/{id}");
    }

    #[test]
    fn when_accepts_rfc3339_dates_and_relative() {
        let now = DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let tz = chrono_tz::Europe::Copenhagen;
        assert_eq!(
            parse_when("30m", now, tz).unwrap(),
            "2026-11-01T11:30:00.000Z"
        );
        assert_eq!(
            parse_when("2d", now, tz).unwrap(),
            "2026-10-30T12:00:00.000Z"
        );
        assert_eq!(
            parse_when("2026-11-01", now, tz).unwrap(),
            "2026-10-31T23:00:00.000Z"
        );
        assert_eq!(
            parse_when("2026-11-01T13:00:00+01:00", now, tz).unwrap(),
            "2026-11-01T12:00:00.000Z"
        );
        assert!(parse_when("soon", now, tz).is_none());
        assert!(parse_when("5w", now, tz).is_none());
    }

    fn stored(seq: u64, ts: &str, kind: &str, device: &str, name: &str) -> Map<String, Value> {
        json!({"ts": ts, "seq": seq, "device": device, "device_name": name, "build": "b",
               "process": "app", "type": kind, "fields": {}})
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn query_filters_and_returns_the_newest_last() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let now = Utc::now();
        let evs: Vec<_> = (0..10)
            .map(|i| {
                let (dev, name) = if i % 2 == 0 {
                    ("dev_a", "Giuseppe's iPhone")
                } else {
                    ("dev_b", "MacBook")
                };
                let kind = if i % 3 == 0 { "net.request" } else { "ui.tap" };
                stored(i, &format!("2026-11-01T10:00:0{i}.000Z"), kind, dev, name)
            })
            .collect();
        s.append(&evs, now).unwrap();
        let q = |f: &dyn Fn(&mut Query)| {
            let mut q = Query {
                limit: 100,
                ..Default::default()
            };
            f(&mut q);
            let (v, more) = s.query(&q);
            (
                v.iter()
                    .map(|e| e["seq"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                more,
            )
        };
        assert_eq!(q(&|_| {}).0, (0..10).collect::<Vec<_>>());
        assert_eq!(q(&|q| q.limit = 3), (vec![7, 8, 9], true));
        assert_eq!(
            q(&|q| q.device = Some("iphone".into())).0,
            vec![0, 2, 4, 6, 8]
        );
        assert_eq!(
            q(&|q| q.device = Some("dev_b".into())).0,
            vec![1, 3, 5, 7, 9]
        );
        assert_eq!(q(&|q| q.kind = Some("net".into())).0, vec![0, 3, 6, 9]);
        assert_eq!(
            q(&|q| {
                q.since = Some("2026-11-01T10:00:02.000Z".into());
                q.until = Some("2026-11-01T10:00:05.000Z".into());
            })
            .0,
            vec![2, 3, 4, 5]
        );
        let raw = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(raw.lines().all(|l| l.contains("\"received_at\"")));
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn files_rotate_and_three_old_ones_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).with_rotation(400);
        let now = Utc::now();
        for i in 0..40 {
            let e = stored(
                i,
                &format!("2026-11-01T10:00:{i:02}.000Z"),
                "ui.tap",
                "dev_a",
                "iPhone",
            );
            s.append(&[e], now).unwrap();
        }
        for n in 0..=KEEP_ROTATED {
            assert!(s.path(n).exists(), "{n}");
        }
        assert!(!s.path(KEEP_ROTATED + 1).exists());
        for n in 0..=KEEP_ROTATED {
            assert!(fs::metadata(s.path(n)).unwrap().len() <= 400);
        }
        let (all, _) = s.query(&Query {
            limit: MAX_LIMIT,
            ..Default::default()
        });
        let seqs: Vec<u64> = all.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(*seqs.last().unwrap(), 39);
        assert!(seqs.windows(2).all(|w| w[0] + 1 == w[1]), "{seqs:?}");
        // A small limit is served from the newest file alone.
        let (last, more) = s.query(&Query {
            limit: 1,
            ..Default::default()
        });
        assert_eq!(last[0]["seq"], 39);
        assert!(more || last.len() == 1);
    }
}

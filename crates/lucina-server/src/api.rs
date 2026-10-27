//! `/api/secretary/*`, route for route the Node `src/secretary-api.js`
//! (shopping v2 + device bridge branch): same order of checks (disabled ->
//! 404, token -> 401, then routing, method, body, validation), same envelope,
//! same codes, `Cache-Control: no-store`, `Allow` on 405.

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use chrono::{NaiveDate, Utc};
use futures_util::StreamExt;
use lucina_grocery::GroceryError;
use lucina_store::device::{
    public_command, result_text, validate_state, ResultError, MAX_EVENTS_PER_BATCH,
};
use lucina_store::reminders::{filter_for_scope, CancelOutcome, CreateReminder, Reminder};
use lucina_store::timeline::{Meta, NewItem, MAX_LIST_LIMIT};
use lucina_store::timeutil::{iso_ms, now_iso, parse_js_date, utf16_len, ymd_in_tz};
use lucina_store::ulid::is_timeline_id;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::AppState;

pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_DEVICE_BODY_BYTES: usize = 1024 * 1024;
const MAX_REMINDER_TEXT_CHARS: usize = 500;
const MAX_SHOPPING_NAME_CHARS: usize = 200;
const TIMELINE_REPLAY_MAX: usize = 500;
const BRIEFING_MAX_SENTENCES: usize = 6;
const BRIEFING_CACHE_KEEP: usize = 31;
pub const USER_LABEL: &str = "Giuseppe (operator, Secretary app)";
pub const PLATFORM_LABEL: &str = "Secretary app";

// ---------------------------------------------------------------- envelope

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub allow: Option<String>,
    pub extra: Option<Value>,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: code.into(),
            message: message.into(),
            allow: None,
            extra: None,
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "bad_request", message)
    }
    pub fn not_found() -> Self {
        Self::new(404, "not_found", "")
    }
    pub fn internal() -> Self {
        Self::new(500, "internal_error", "")
    }
    pub fn method_not_allowed(allowed: &[&str]) -> Self {
        let mut e = Self::new(
            405,
            "method_not_allowed",
            format!("Allowed: {}", allowed.join(", ")),
        );
        e.allow = Some(allowed.join(", "));
        e
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut resp = (status, body.to_string()).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub fn ok(data: Value) -> Response {
    ok_status(200, data)
}

pub fn ok_status(status: u16, data: Value) -> Response {
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        &json!({"ok": true, "data": data}),
    )
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"ok": false, "error": self.code});
        if !self.message.is_empty() {
            body["message"] = json!(self.message);
        }
        if let Some(Value::Object(extra)) = &self.extra {
            for (k, v) in extra {
                body[k] = v.clone();
            }
        }
        let mut resp = json_response(self.status, &body);
        if let Some(allow) = self
            .allow
            .as_deref()
            .and_then(|a| HeaderValue::from_str(a).ok())
        {
            resp.headers_mut().insert(header::ALLOW, allow);
        }
        resp
    }
}

type ApiResult = Result<Response, ApiError>;

/// The local lucina-pokemon server (`LUCINA_GAME_URL`, default `127.0.0.1:18767`): `state` (JSON), `screenshot`
/// (PNG) and `control` (`{"running": bool, "reason"?}`). Read-only callers are told apart upstream: secretary-server
/// lets only the owner's devices reach `control`.
async fn game(st: &AppState, method: &Method, what: Option<&str>, body: Body) -> ApiResult {
    let base = std::env::var("LUCINA_GAME_URL").unwrap_or_else(|_| "http://127.0.0.1:18767".into());
    let base = base.trim_end_matches('/');
    let http = &st.secretary.http;
    let down = |e: String| ApiError::new(503, "game_unavailable", e);
    match what {
        Some("state") => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let r = http
                .get(format!("{base}/state"))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map_err(|e| down(e.to_string()))?;
            if !r.status().is_success() {
                return Err(down(format!("game server answered {}", r.status())));
            }
            Ok(ok(r
                .json::<Value>()
                .await
                .map_err(|e| down(e.to_string()))?))
        }
        Some("screenshot") => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let r = http
                .get(format!("{base}/screenshot"))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map_err(|e| down(e.to_string()))?;
            if !r.status().is_success() {
                return Err(down(format!("game server answered {}", r.status())));
            }
            let png = r.bytes().await.map_err(|e| down(e.to_string()))?;
            let mut resp = (StatusCode::OK, png).into_response();
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(resp)
        }
        Some("control") => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let b = read_json(body, MAX_BODY_BYTES).await?;
            let running = b["running"].as_bool().ok_or_else(|| {
                ApiError::new(400, "invalid_request", "running must be true or false")
            })?;
            let reason: String = b["reason"]
                .as_str()
                .unwrap_or(if running {
                    "Started by the owner from the app"
                } else {
                    "Paused by the owner from the app"
                })
                .chars()
                .take(120)
                .collect();
            let r = http
                .post(format!("{base}/control"))
                .header("X-Lucina-Game", "1")
                .json(&json!({"running": running, "reason": reason}))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map_err(|e| down(e.to_string()))?;
            if !r.status().is_success() {
                return Err(down(format!("game server answered {}", r.status())));
            }
            Ok(ok(r
                .json::<Value>()
                .await
                .map_err(|e| down(e.to_string()))?))
        }
        _ => Err(ApiError::not_found()),
    }
}

// ---------------------------------------------------------------- body helpers

pub async fn read_json(body: Body, limit: usize) -> Result<Value, ApiError> {
    let bytes = axum::body::to_bytes(body, limit + 1).await.map_err(|_| {
        ApiError::new(
            413,
            "payload_too_large",
            format!("Body exceeds {limit} bytes"),
        )
    })?;
    if bytes.len() > limit {
        return Err(ApiError::new(
            413,
            "payload_too_large",
            format!("Body exceeds {limit} bytes"),
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| ApiError::new(400, "invalid_json", "Body is not valid JSON"))?;
    if !v.is_object() {
        return Err(ApiError::bad_request("JSON body must be an object"));
    }
    Ok(v)
}

/// Node `optionalString`: missing/null -> "", else a trimmed string <= max.
pub fn optional_string(body: &Value, key: &str, max: usize) -> Result<String, ApiError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => {
            let t = lucina_store::timeutil::js_trim(s);
            if utf16_len(t) > max {
                return Err(ApiError::bad_request(format!(
                    "{key} must be at most {max} characters"
                )));
            }
            Ok(t.to_owned())
        }
        Some(_) => Err(ApiError::bad_request(format!("{key} must be a string"))),
    }
}

pub(crate) fn query_param(uri: &Uri, key: &str) -> Option<String> {
    let q = uri.query()?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if decode(k) == key {
            return Some(decode(v));
        }
    }
    None
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                    16,
                ) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_reminder_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn is_item_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

// ---------------------------------------------------------------- router

pub fn router(state: AppState) -> Router {
    crate::share::recover(state.clone());
    Router::new()
        .merge(crate::share::router())
        .route("/health", get(health))
        .route("/api/secretary", any(secretary))
        .route("/api/secretary/", any(secretary))
        .route("/api/secretary/{*rest}", any(secretary))
        .with_state(state)
}

async fn health(State(st): State<AppState>) -> Response {
    let c = &st.cfg;
    // 503 only when a background loop died (the watchdog restarts on it);
    // upstream outages are delivery alerts, not restarts.
    let liveness = crate::delivery_monitor::liveness(&st, Utc::now());
    json_response(
        if liveness.is_empty() {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        &json!({
            "ok": liveness.is_empty(),
            "liveness": {"problems": liveness, "loops": st.supervisor.names()},
            "delivery": st.monitor.health(),
            "service": "lucina-rs",
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSeconds": st.started.elapsed().as_secs(),
            "secretaryApi": c.secretary_enabled,
            "shadow": c.shadow,
            "binds": c.binds.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
            "timelinePath": c.timeline_path.display().to_string(),
            "intent": {"fastChat": c.intent.fast_chat, "router": c.intent.intent_router, "capabilityHints": c.capability_hints},
            "bridges": {"notes": c.notes_enabled, "calendar": c.calendar_enabled, "outfit": c.outfit_enabled, "appNotify": c.app_notify, "secretaryServer": c.secretary_url.is_some()},
            "jobs": {"harness": c.dsh.ready(), "enabled": c.dsh.enabled},
            "planner": match c.planner { crate::PlannerMode::Json => "json", crate::PlannerMode::Tools => "tools" },
            "tools": st.registry.names(),
            "memory": {"provider": "honcho", "mode": st.memory.mode().as_str(), "workspace": c.honcho.workspace_id},
            "reminders": {"enabled": c.reminders_enabled, "writer": st.writer.kind(), "scheduler": c.scheduler && c.reminders_writer == crate::WriterKind::Local},
            "scheduler": st.scheduler_health.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            "channels": {"telegram": st.telegram_active(), "discord": false},
            "telegram": st.telegram_health.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            "grocery": st.grocery.is_some(),
            "delegation": {"enabled": st.delegator.is_some()},
            "botHospital": crate::bot_hospital::health_value(&st.hospital_health),
            "presence": {"context": c.presence_context, "fresh": lucina_discord::store::context_line(&c.presence_dir, c.time_zone, Utc::now(), c.presence_max_age).is_some()},
            "deepseek": {"model": c.deepseek.model, "hasKey": (st.deepseek_has_key)(), "usage": st.usage.as_ref().map(|u| u.snapshot())},
            "timelineStreams": st.streams.load(Ordering::Relaxed),
            "meshy": st.meshy.as_ref().map(|m| m.health()),
        }),
    )
}

async fn secretary(
    State(st): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !st.cfg.secretary_enabled {
        return ApiError::not_found().into_response();
    }
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split_once(' ')
                .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
                .map(|(_, t)| t.trim().to_owned())
        })
        .unwrap_or_default();
    let authorized = match st.cfg.token.as_deref() {
        Some(expected) if !provided.is_empty() => {
            provided.len() == expected.len()
                && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()))
        }
        _ => false,
    };
    if !authorized {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    let path = uri.path().trim_start_matches("/api/secretary").to_owned();
    let parts: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(decode)
        .collect();
    let bearer = crate::surface::BearerVerified::checked();
    match route(&st, bearer, &method, &uri, &headers, &parts, body).await {
        Ok(r) => r,
        Err(e) => {
            if e.status == StatusCode::INTERNAL_SERVER_ERROR {
                tracing::error!(method = %method, resource = parts.first().map(String::as_str).unwrap_or(""), error = %e.message, "secretary api request failed");
                return ApiError::internal().into_response();
            }
            e.into_response()
        }
    }
}

async fn route(
    st: &AppState,
    bearer: crate::surface::BearerVerified,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    parts: &[String],
    body: Body,
) -> ApiResult {
    let p = |i: usize| parts.get(i).map(String::as_str);
    let (resource, id, extra, fourth) = (p(0), p(1), p(2), p(3));
    if resource == Some("device") {
        if parts.len() > 4 {
            return Err(ApiError::not_found());
        }
        return device(st, method, id, extra, fourth, body).await;
    }
    if resource == Some("shopping") && id == Some("items") && extra.is_none() {
        if method != Method::GET {
            return Err(ApiError::method_not_allowed(&["GET"]));
        }
        let repo = repo(st)?;
        let items = repo
            .custom_items(&st.cfg.grocery_user_id)
            .await
            .map_err(grocery_error)?;
        return Ok(ok(json!({ "items": items })));
    }
    if resource == Some("shopping") && id == Some("lists") {
        if parts.len() > 4 {
            return Err(ApiError::not_found());
        }
        return shopping_lists(st, method, uri, extra, fourth, body).await;
    }
    // Grocery deals near home (crate::deals).
    if resource == Some("grocery")
        && id == Some("deals")
        && extra == Some("map")
        && fourth.is_none()
    {
        if method != Method::GET {
            return Err(ApiError::method_not_allowed(&["GET"]));
        }
        return crate::deals::map(st, uri).await;
    }
    // Lucina's Pokémon run for the apps (secretary-server /v1/game/*): live state, screen, start/pause.
    if resource == Some("game") && extra.is_none() {
        return game(st, method, id, body).await;
    }
    if extra.is_some() {
        return Err(ApiError::not_found());
    }
    let only = |allowed: &[&str]| -> Result<(), ApiError> {
        if allowed.contains(&method.as_str()) {
            Ok(())
        } else {
            Err(ApiError::method_not_allowed(allowed))
        }
    };
    match (resource, id) {
        (Some("health"), None) => {
            only(&["GET"])?;
            Ok(ok(json!({
                "persona": st.cfg.persona,
                "memoryProvider": "honcho",
                "timeZone": st.cfg.time_zone.name(),
                "now": now_iso(),
                "apiVersion": 1,
            })))
        }
        (Some("chat"), None) => {
            only(&["POST"])?;
            let body = read_json(body, MAX_BODY_BYTES).await?;
            let mut out = crate::chat::handle_chat(st.clone(), bearer, body).await?;
            // The reply sink (app, web and Lucina MCP all read it here).
            lucina_secrets::redactor().redact_json(&mut out);
            Ok(ok(out))
        }
        (Some("timeline"), None) => {
            only(&["GET"])?;
            timeline_list(st, uri)
        }
        (Some("timeline"), Some("stream")) => {
            only(&["GET"])?;
            timeline_stream(st, uri, headers)
        }
        (Some("jobs"), Some(id)) => {
            only(&["GET"])?;
            let job = if is_reminder_id(id) {
                st.jobs
                    .find(id)
                    .map_err(|e| ApiError::new(500, "internal_error", e.to_string()))?
            } else {
                None
            };
            let job = job.ok_or_else(|| ApiError::new(404, "job_not_found", "No such job"))?;
            Ok(ok(job.public()))
        }
        (Some("reminders"), None) => match method.as_str() {
            "GET" => list_reminders(st, uri),
            "POST" => create_reminder(st, body).await,
            _ => Err(ApiError::method_not_allowed(&["GET", "POST"])),
        },
        (Some("reminders"), Some(id)) => {
            only(&["DELETE"])?;
            cancel_reminder(st, id).await
        }
        (Some("agenda"), None) => {
            only(&["GET"])?;
            agenda(st, bearer, uri).await
        }
        (Some("meshy"), Some("confirm")) => {
            only(&["POST"])?;
            let body = read_json(body, MAX_BODY_BYTES).await?;
            Ok(ok(crate::chat::meshy_turn::confirm_by_id(st, &body).await?))
        }
        (Some("presence"), None) => {
            only(&["GET"])?;
            Ok(ok(presence_report(st, Utc::now())))
        }
        (Some("delegations"), None) => {
            only(&["GET"])?;
            let mut v = crate::delegation::api_status(st.delegator.as_ref()).await;
            lucina_secrets::redactor().redact_json(&mut v);
            Ok(ok(v))
        }
        (Some("delegations"), Some(id)) => {
            only(&["GET"])?;
            let job = st
                .delegator
                .as_ref()
                .and_then(|d| d.job(id))
                .ok_or_else(|| ApiError::new(404, "job_not_found", "No such delegation"))?;
            let mut v = serde_json::to_value(job).unwrap_or_default();
            lucina_secrets::redactor().redact_json(&mut v);
            Ok(ok(v))
        }
        (Some("delegate"), None) => {
            only(&["POST"])?;
            let d = st.delegator.clone().ok_or_else(|| {
                ApiError::new(
                    404,
                    "delegation_disabled",
                    "Delegation is off (LUCINA_DELEGATE)",
                )
            })?;
            let body = read_json(body, MAX_BODY_BYTES).await?;
            match crate::delegation::api_delegate(&d, &body).await {
                Ok(mut v) => {
                    lucina_secrets::redactor().redact_json(&mut v);
                    Ok(ok(v))
                }
                Err((status, code, msg)) => Err(ApiError::new(status, code, msg)),
            }
        }
        (Some("shopping"), id) => {
            let allowed: &[&str] = if id.is_some() {
                &["PATCH"]
            } else {
                &["GET", "POST"]
            };
            only(allowed)?;
            shopping_legacy(st, method, uri, id, body).await
        }
        _ => Err(ApiError::not_found()),
    }
}

// ---------------------------------------------------------------- presence

/// The owner's Discord presence (from the presence watcher's files) plus
/// today's online minutes. `available: false` when no watcher ever ran,
/// `fresh: false` when it is down.
pub fn presence_report(st: &AppState, now: chrono::DateTime<Utc>) -> Value {
    let r = lucina_discord::PresenceReport::build(
        &st.cfg.presence_dir,
        st.cfg.time_zone,
        now,
        st.cfg.presence_max_age,
    );
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    v["timeZone"] = json!(st.cfg.time_zone.name());
    v
}

// ---------------------------------------------------------------- reminders

pub fn owner_reminders(st: &AppState) -> Result<Vec<Reminder>, ApiError> {
    let Some(chat) = st.cfg.owner_chat_id.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(Vec::new());
    };
    Ok(st
        .reminders
        .list(true)
        .map_err(|e| ApiError::new(500, "internal_error", e.to_string()))?
        .into_iter()
        .filter(|r| r.chat_id == chat)
        .collect())
}

fn by_due(a: &Reminder, b: &Reminder) -> std::cmp::Ordering {
    a.due().cmp(&b.due())
}

fn flag01(v: Option<String>, name: &str) -> Result<bool, ApiError> {
    match v.as_deref().unwrap_or("0") {
        "0" | "false" | "" => Ok(false),
        "1" | "true" => Ok(true),
        _ => Err(ApiError::bad_request(format!("{name} must be 0 or 1"))),
    }
}

fn list_reminders(st: &AppState, uri: &Uri) -> ApiResult {
    let scope = query_param(uri, "scope")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "upcoming".into());
    if !["upcoming", "today", "all"].contains(&scope.as_str()) {
        return Err(ApiError::bad_request(
            "scope must be upcoming, today or all",
        ));
    }
    let include_sent = match query_param(uri, "includeSent").filter(|s| !s.is_empty()) {
        None => false,
        Some(v) if ["0", "1", "true", "false"].contains(&v.as_str()) => v == "1" || v == "true",
        Some(_) => return Err(ApiError::bad_request("includeSent must be 0 or 1")),
    };
    let tz = st.cfg.time_zone;
    let now = Utc::now();
    let all = owner_reminders(st)?;
    let mut list = filter_for_scope(&all, scope == "today", tz, now);
    if include_sent {
        let today = ymd_in_tz(now, tz);
        list.extend(
            all.iter()
                .filter(|r| r.status != "pending")
                .filter(|r| scope != "today" || r.due().is_some_and(|d| ymd_in_tz(d, tz) == today))
                .cloned(),
        );
    }
    list.sort_by(by_due);
    Ok(ok(
        json!({"scope": scope, "includeSent": include_sent, "timeZone": tz.name(), "reminders": list.iter().map(Reminder::public).collect::<Vec<_>>()}),
    ))
}

fn iso_with_offset(s: &str) -> bool {
    regex::Regex::new(
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d{1,9})?)?(?:Z|[+-]\d{2}:\d{2})$",
    )
    .map(|r| r.is_match(s))
    .unwrap_or(false)
}

async fn create_reminder(st: &AppState, body: Body) -> ApiResult {
    let body = read_json(body, MAX_BODY_BYTES).await?;
    if !st.cfg.reminders_enabled {
        return Err(ApiError::new(
            503,
            "reminders_disabled",
            "Reminders are disabled on this host",
        ));
    }
    let chat = st
        .cfg
        .owner_chat_id
        .clone()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                503,
                "owner_chat_unconfigured",
                "No owner Telegram chat is configured",
            )
        })?;
    let tz = st.cfg.time_zone;
    let natural = optional_string(&body, "naturalText", MAX_REMINDER_TEXT_CHARS)?;
    let text = optional_string(&body, "text", MAX_REMINDER_TEXT_CHARS)?;
    let due_raw = optional_string(&body, "dueAt", 40)?;
    let (reminder_text, due_at, forward_body) = if !natural.is_empty() {
        if !text.is_empty() || !due_raw.is_empty() {
            return Err(ApiError::bad_request(
                "send either {text, dueAt} or {naturalText}, not both",
            ));
        }
        let parsed = lucina_store::parser::parse_reminder_message(
            &natural,
            &lucina_store::parser::ParseOptions {
                tz,
                default_hour: st.cfg.reminder_default_hour,
                default_minute: st.cfg.reminder_default_minute,
                now: Utc::now(),
            },
        );
        match parsed {
            Ok(p) => (
                p.text.clone(),
                p.due_at_iso(),
                json!({"naturalText": natural}),
            ),
            Err("due_in_past") => {
                return Err(ApiError::new(
                    400,
                    "due_in_past",
                    "The reminder time is in the past",
                ))
            }
            Err(e) => {
                return Err(ApiError::new(
                    400,
                    "unparseable_reminder",
                    format!("Could not parse the reminder ({e})"),
                ))
            }
        }
    } else {
        if text.is_empty() {
            return Err(ApiError::bad_request(
                "text is required (or send naturalText)",
            ));
        }
        if due_raw.is_empty() {
            return Err(ApiError::bad_request(
                "dueAt is required (or send naturalText)",
            ));
        }
        let due = parse_js_date(&due_raw)
            .filter(|_| iso_with_offset(&due_raw))
            .ok_or_else(|| {
                ApiError::bad_request(
                    "dueAt must be ISO 8601 with a UTC offset, e.g. 2026-10-22T09:30:00+02:00",
                )
            })?;
        if due <= Utc::now() {
            return Err(ApiError::new(
                400,
                "due_in_past",
                "The reminder time is in the past",
            ));
        }
        (
            text.clone(),
            iso_ms(due),
            json!({"text": text, "dueAt": due_raw}),
        )
    };
    // Phase 1: the Node Lucina owns reminders.json; forward the request as is.
    if let Some(forwarded) = st.writer.forward_create(&forward_body).await {
        let (status, data) = forwarded.map_err(|e| ApiError::new(e.status, &e.code, e.message))?;
        return Ok(ok_status(status, data));
    }
    let (created, reminder) = st
        .writer
        .create(CreateReminder {
            text: reminder_text,
            due_at,
            chat_id: chat,
            user_peer_id: st.cfg.owner_peer_id.clone(),
            user_label: USER_LABEL.into(),
            persona: st.cfg.persona.clone(),
            time_zone: tz.name().into(),
            dedupe_key: None,
            source: "secretary".into(),
        })
        .await
        .map_err(|e| ApiError::new(e.status, &e.code, e.message))?;
    Ok(ok_status(
        if created { 201 } else { 200 },
        json!({"created": created, "reminder": reminder.public()}),
    ))
}

async fn cancel_reminder(st: &AppState, id: &str) -> ApiResult {
    let not_found = || ApiError::new(404, "reminder_not_found", "No such reminder");
    if !is_reminder_id(id) {
        return Err(not_found());
    }
    let chat = st
        .cfg
        .owner_chat_id
        .clone()
        .filter(|c| !c.is_empty())
        .ok_or_else(not_found)?;
    match st
        .writer
        .cancel(id, &chat)
        .await
        .map_err(|e| ApiError::new(e.status, &e.code, e.message))?
    {
        CancelOutcome::Cancelled(r) => Ok(ok(json!({"cancelled": true, "reminder": r.public()}))),
        CancelOutcome::AlreadyCancelled(r) => {
            Ok(ok(json!({"cancelled": false, "reminder": r.public()})))
        }
        CancelOutcome::AlreadySent(_) => Err(ApiError::new(
            409,
            "already_sent",
            "The reminder was already delivered",
        )),
        CancelOutcome::NotFound => Err(not_found()),
    }
}

// ---------------------------------------------------------------- agenda

pub struct Briefing {
    pub at: Instant,
    pub fingerprint: String,
    pub generated_at: String,
    pub text: Option<String>,
}

/// Node `clipSentences`: at most `max` sentences, whitespace collapsed.
pub fn clip_sentences(text: &str, max: usize) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = clean.chars().collect();
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '.' | '!' | '?') {
            let mut j = i;
            while j < chars.len() && matches!(chars[j], '.' | '!' | '?') {
                j += 1;
            }
            if j == chars.len() || chars[j].is_whitespace() {
                sentences.push(chars[start..j].iter().collect::<String>());
                start = j;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if start < chars.len() {
        sentences.push(chars[start..].iter().collect());
    }
    sentences
        .into_iter()
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

async fn agenda(st: &AppState, bearer: crate::surface::BearerVerified, uri: &Uri) -> ApiResult {
    let tz = st.cfg.time_zone;
    let date = query_param(uri, "date")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| ymd_in_tz(Utc::now(), tz));
    let valid = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok();
    if !valid {
        return Err(ApiError::bad_request("date must be YYYY-MM-DD"));
    }
    let refresh = match query_param(uri, "refresh") {
        None => false,
        Some(v) => flag01(Some(v), "refresh")?,
    };
    let mut reminders: Vec<Reminder> = owner_reminders(st)?
        .into_iter()
        .filter(|r| r.status == "pending" || r.status == "sent")
        .filter(|r| r.due().is_some_and(|d| ymd_in_tz(d, tz) == date))
        .collect();
    reminders.sort_by(by_due);
    // LUCINA_CALENDAR=true: the day's events join the briefing.
    let events = if st.cfg.calendar_enabled {
        Some(crate::bridges::events_for_briefing(&st.secretary, &date).await)
    } else {
        None
    };
    // LUCINA_TASKS=true: today's open and overdue tasks (today only).
    let tasks = if st.cfg.tasks_enabled && date == ymd_in_tz(Utc::now(), tz) {
        Some(crate::bridges::tasks_for_briefing(&st.secretary, tz).await)
    } else {
        None
    };
    let fingerprint = reminders
        .iter()
        .map(|r| format!("{}:{}:{}", r.id, r.status, r.due_at))
        .chain(events.iter().flatten().map(|e| {
            let s = |k: &str| {
                e[k].as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e[k].to_string())
            };
            format!("{}:{}:{}:{}", s("id"), s("start"), s("end"), s("title"))
        }))
        .chain(tasks.iter().flatten().cloned())
        .collect::<Vec<_>>()
        .join("|");
    let ttl = st.cfg.briefing_cache;
    let mut cache = st.briefings.lock().await;
    cache.retain(|_, b| b.at.elapsed() <= ttl);
    while cache.len() > BRIEFING_CACHE_KEEP {
        let oldest = cache
            .iter()
            .min_by_key(|(_, b)| b.at)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            cache.remove(&k);
        }
    }
    let fresh = cache
        .get(&date)
        .is_some_and(|b| b.fingerprint == fingerprint && b.at.elapsed() < ttl && b.text.is_some());
    let mut cached = fresh && !refresh;
    if !cached {
        let memory = st
            .memory
            .build_context(
                &crate::surface::app_surface(&st.cfg, bearer),
                &st.cfg.owner_peer_id,
                &lucina_memory::secretary_session_id("", &st.cfg.default_session),
                &format!("Daily agenda briefing for {date}"),
            )
            .await;
        let lines: Vec<String> = reminders
            .iter()
            .map(|r| {
                let t = r
                    .due()
                    .map(|d| d.with_timezone(&tz).format("%H:%M").to_string())
                    .unwrap_or_default();
                format!(
                    "- {t} {}{}",
                    r.text,
                    if r.status == "sent" {
                        " (already delivered)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        let mut skill_result = format!(
            "Date: {date} ({})\n{}",
            tz.name(),
            if lines.is_empty() {
                "Reminders for that day: none.".to_owned()
            } else {
                format!("Reminders for that day:\n{}", lines.join("\n"))
            }
        );
        if let Some(evs) = &events {
            skill_result.push('\n');
            skill_result.push_str(&if evs.is_empty() {
                "Calendar events for that day: none.".to_owned()
            } else {
                let l = lucina_intent::calendar::event_lines(evs, tz, "en");
                format!(
                    "Calendar events for that day:\n{}",
                    l.iter()
                        .map(|x| format!("- {x}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            });
        }
        if let Some(tl) = &tasks {
            skill_result.push('\n');
            skill_result.push_str(&if tl.is_empty() {
                "Tasks for today: none open.".to_owned()
            } else {
                format!("Tasks for today (open, overdue first):\n{}", tl.join("\n"))
            });
        }
        let persona = &st.cfg.persona;
        let instruction = [
            format!("Write a short daily briefing for {date} in at most {BRIEFING_MAX_SENTENCES} sentences, speaking as {persona}."),
            "Mention the reminders and calendar events in time order with their times, then the open tasks (overdue first) briefly. If there are none, say the day is clear.".into(),
            "You may add one grounded note from memory if it is relevant to that day. Do not invent appointments.".into(),
            "Plain text: no lists, no markdown, no greetings longer than a few words.".into(),
        ]
        .join(" ");
        let user_text = format!("Give me my daily briefing for {date}.");
        let t = crate::planner::TurnInput {
            user_text: &user_text,
            memory: &memory,
            user_label: USER_LABEL,
            platform: PLATFORM_LABEL,
        };
        let text = match st
            .planner
            .generate_skill_reply(&t, "Secretary daily agenda", &skill_result, &instruction)
            .await
        {
            Ok(t) => Some(clip_sentences(&t, BRIEFING_MAX_SENTENCES)).filter(|s| !s.is_empty()),
            Err(e) => {
                tracing::error!(%date, error = %e, "secretary briefing failed");
                None
            }
        };
        cache.insert(
            date.clone(),
            Briefing {
                at: Instant::now(),
                fingerprint,
                generated_at: now_iso(),
                text,
            },
        );
        cached = false;
    }
    let entry = cache.get(&date).expect("just inserted");
    let mut data = json!({
        "date": date,
        "timeZone": tz.name(),
        "reminders": reminders.iter().map(Reminder::public).collect::<Vec<_>>(),
        "briefing": entry.text,
        "briefingGeneratedAt": entry.text.as_ref().map(|_| entry.generated_at.clone()),
        "briefingCached": cached && entry.text.is_some(),
    });
    if entry.text.is_none() {
        data["briefingError"] = json!("briefing_unavailable");
        cache.remove(&date);
    }
    Ok(ok(data))
}

// ---------------------------------------------------------------- shopping

fn grocery_error(e: GroceryError) -> ApiError {
    match e {
        GroceryError::Shopping {
            code,
            message,
            revision,
            conflicts,
            duplicates,
        } => {
            let status = match code {
                "list_not_found" | "item_not_found" => 404,
                "default_list" | "revision_conflict" => 409,
                _ => 400,
            };
            let mut err = ApiError::new(status, code, message);
            if code == "revision_conflict" {
                err.extra = Some(
                    json!({"revision": revision, "conflicts": conflicts, "duplicates": duplicates}),
                );
            }
            err
        }
        GroceryError::Unavailable(_) => ApiError::new(
            503,
            "grocery_unavailable",
            "The grocery database is unreachable",
        ),
        GroceryError::Db(e) => ApiError::new(500, "internal_error", e.to_string()),
    }
}

fn shopping_channel(body: &Value) -> &'static str {
    match body["channel"].as_str() {
        Some("ios") => "ios",
        _ => "web",
    }
}

fn repo(st: &AppState) -> Result<&lucina_grocery::GroceryRepo, ApiError> {
    st.grocery.as_deref().ok_or_else(|| {
        ApiError::new(
            503,
            "grocery_unavailable",
            "The grocery service is not configured",
        )
    })
}

/// `?lang=it|en|da` (or the body's `lang`): the language of item names;
/// `None` = the owner's. Anything else is `400 bad_request`.
fn shopping_lang(uri: &Uri, body: Option<&Value>) -> Result<Option<String>, ApiError> {
    let v = query_param(uri, "lang")
        .filter(|s| !s.is_empty())
        .or_else(|| body.and_then(|b| b["lang"].as_str()).map(str::to_owned));
    match v {
        None => Ok(None),
        Some(l) if lucina_grocery::items::LANGS.contains(&l.as_str()) => Ok(Some(l)),
        Some(_) => Err(ApiError::bad_request(format!(
            "lang must be one of {}",
            lucina_grocery::items::LANGS.join(", ")
        ))),
    }
}

async fn shopping_legacy(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    item_id: Option<&str>,
    body: Body,
) -> ApiResult {
    let repo = repo(st)?;
    let user = &st.cfg.grocery_user_id;
    match (method.as_str(), item_id) {
        ("GET", None) => {
            let lang = shopping_lang(uri, None)?;
            Ok(ok(
                json!({"items": repo.legacy_list_in(user, lang.as_deref()).await.map_err(grocery_error)?}),
            ))
        }
        ("POST", None) => {
            let body = read_json(body, MAX_BODY_BYTES).await?;
            let raw = optional_string(&body, "rawName", MAX_SHOPPING_NAME_CHARS)?;
            if raw.is_empty() {
                return Err(ApiError::bad_request("rawName is required"));
            }
            let product = optional_string(&body, "product", MAX_SHOPPING_NAME_CHARS)?;
            let canonical = lucina_grocery::resolve_canonical_product(if product.is_empty() {
                &raw
            } else {
                &product
            });
            let lang = shopping_lang(uri, Some(&body))?;
            let item = repo
                .add_item_in(
                    user,
                    &canonical,
                    &raw,
                    shopping_channel(&body),
                    lang.as_deref(),
                )
                .await
                .map_err(grocery_error)?;
            Ok(ok_status(201, json!({"item": item})))
        }
        ("PATCH", Some(id)) => {
            if !is_item_id(id) {
                return Err(ApiError::new(
                    404,
                    "item_not_found",
                    "No such shopping-list item",
                ));
            }
            let body = read_json(body, MAX_BODY_BYTES).await?;
            let status = body["status"]
                .as_str()
                .filter(|s| *s == "needed" || *s == "purchased")
                .ok_or_else(|| {
                    ApiError::new(400, "invalid_status", "status must be needed or purchased")
                })?;
            let item = repo
                .legacy_set_status(user, id, status, shopping_channel(&body))
                .await
                .map_err(grocery_error)?;
            Ok(ok(json!({"item": item})))
        }
        _ => Err(ApiError::not_found()),
    }
}

async fn shopping_lists(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    list_id: Option<&str>,
    sub: Option<&str>,
    body: Body,
) -> ApiResult {
    let repo = repo(st)?;
    let user = &st.cfg.grocery_user_id;
    if let Some(id) = list_id {
        if id != "default" && !is_item_id(id) {
            return Err(ApiError::new(
                404,
                "list_not_found",
                "No such shopping list",
            ));
        }
    }
    let Some(id) = list_id else {
        return match method.as_str() {
            "GET" => Ok(ok(
                json!({"lists": repo.list_lists(user).await.map_err(grocery_error)?}),
            )),
            "POST" => {
                let body = read_json(body, MAX_BODY_BYTES).await?;
                Ok(ok_status(
                    201,
                    json!({"list": repo.create_list(user, &body).await.map_err(grocery_error)?}),
                ))
            }
            _ => Err(ApiError::method_not_allowed(&["GET", "POST"])),
        };
    };
    if sub == Some("ops") {
        if method != Method::POST {
            return Err(ApiError::method_not_allowed(&["POST"]));
        }
        let body = read_json(body, MAX_BODY_BYTES).await?;
        let expected = match &body["expectedRevision"] {
            Value::Null => None,
            Value::Number(n) if n.as_i64().is_some() => n.as_i64(),
            Value::Number(n)
                if n.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() < 9e15) =>
            {
                n.as_f64().map(|f| f as i64)
            }
            _ => return Err(ApiError::bad_request("expectedRevision must be an integer")),
        };
        let lang = shopping_lang(uri, Some(&body))?;
        let out = repo
            .apply_ops_in(
                user,
                id,
                &body["ops"],
                expected,
                shopping_channel(&body),
                lang.as_deref(),
            )
            .await
            .map_err(grocery_error)?;
        return Ok(ok(out.to_json()));
    }
    if sub.is_some() {
        return Err(ApiError::not_found());
    }
    match method.as_str() {
        "GET" => {
            let lang = shopping_lang(uri, None)?;
            Ok(ok(repo
                .list_state_in(user, id, lang.as_deref())
                .await
                .map_err(grocery_error)?))
        }
        "PATCH" => {
            let body = read_json(body, MAX_BODY_BYTES).await?;
            if body.get("name").is_none() && body.get("theme").is_none() {
                return Err(ApiError::bad_request("send name and/or theme"));
            }
            Ok(ok(
                json!({"list": repo.update_list(user, id, &body).await.map_err(grocery_error)?}),
            ))
        }
        "DELETE" => Ok(ok(repo
            .delete_list(user, id)
            .await
            .map_err(grocery_error)?)),
        _ => Err(ApiError::method_not_allowed(&["GET", "PATCH", "DELETE"])),
    }
}

// ---------------------------------------------------------------- timeline

fn timeline_id_param(v: Option<String>, name: &str) -> Result<Option<String>, ApiError> {
    match v.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(id) if is_timeline_id(&id) => Ok(Some(id)),
        Some(_) => Err(ApiError::bad_request(format!(
            "{name} must be a timeline item id"
        ))),
    }
}

fn raw_array(items: &[lucina_store::timeline::StoredItem]) -> String {
    let mut s = String::from("[");
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&it.raw);
    }
    s.push(']');
    s
}

fn timeline_list(st: &AppState, uri: &Uri) -> ApiResult {
    let before = timeline_id_param(query_param(uri, "before"), "before")?;
    let limit = match query_param(uri, "limit").filter(|s| !s.is_empty()) {
        None => 50,
        Some(l) => {
            let n = (l.len() <= 3 && l.bytes().all(|b| b.is_ascii_digit()))
                .then(|| l.parse::<usize>().ok())
                .flatten();
            match n {
                Some(n) if (1..=MAX_LIST_LIMIT).contains(&n) => n,
                _ => {
                    return Err(ApiError::bad_request(format!(
                        "limit must be an integer from 1 to {MAX_LIST_LIMIT}"
                    )))
                }
            }
        }
    };
    // `conversation=selftest`: the self-test conversation (crate::selftest), never mixed in.
    let store =
        if query_param(uri, "conversation").as_deref() == Some(crate::selftest::CONVERSATION) {
            st.selftest_timeline.clone()
        } else {
            st.timeline.clone()
        };
    let (items, next) = store
        .list(before.as_deref(), limit)
        .map_err(|e| ApiError::new(500, "internal_error", e.to_string()))?;
    // Items are served as stored: Node-written lines reach the client verbatim.
    let body = format!(
        r#"{{"ok":true,"data":{{"items":{},"nextBefore":{}}}}}"#,
        raw_array(&items),
        serde_json::to_string(&next).unwrap_or_else(|_| "null".into())
    );
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

struct StreamGuard(AppState);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.streams.fetch_sub(1, Ordering::SeqCst);
    }
}

fn sse_item(it: &lucina_store::timeline::StoredItem) -> Bytes {
    Bytes::from(format!("event: item\nid: {}\ndata: {}\n\n", it.id, it.raw))
}

/// SSE: `retry: 5000` + `: connected`, replay after `Last-Event-ID` (<=500),
/// then live items (this process's appends at once, other writers' lines
/// within ~1 s by tail-following the file) and a `: ping` every ping period.
fn timeline_stream(st: &AppState, uri: &Uri, headers: &HeaderMap) -> ApiResult {
    let max = st.cfg.timeline_max_streams;
    if st.streams.fetch_add(1, Ordering::SeqCst) >= max {
        st.streams.fetch_sub(1, Ordering::SeqCst);
        return Err(ApiError::new(
            429,
            "too_many_streams",
            format!("At most {max} timeline streams at once"),
        ));
    }
    let guard = StreamGuard(st.clone());
    let header_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let last = timeline_id_param(
        header_id.or_else(|| query_param(uri, "lastEventId")),
        "Last-Event-ID",
    )?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
    let store = st.timeline.clone();
    let ping = st.cfg.timeline_ping;
    // Index what other writers appended so far, then listen: only lines that
    // arrive after the connection are live items.
    let _ = store.poll();
    let mut live = store.subscribe();
    tokio::spawn(async move {
        let _guard = guard;
        if tx
            .send(Bytes::from_static(b"retry: 5000\n: connected\n\n"))
            .await
            .is_err()
        {
            return;
        }
        // Resume by arrival (file) order, not by id (docs/verification.md, T1
        // fix A): a line another writer committed late can carry a smaller id
        // than the client's Last-Event-ID and still be new to it. The cursor
        // is the last item sent, in arrival order; replayed and live items
        // both go into `seen`, so nothing is sent twice.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut seen_order: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut cursor: Option<String> = match &last {
            Some(id) => {
                let Ok(items) = store.after_arrival(Some(id), TIMELINE_REPLAY_MAX) else {
                    return;
                };
                let mut cur = id.clone();
                for it in &items {
                    if tx.send(sse_item(it)).await.is_err() {
                        return;
                    }
                    cur = it.id.clone();
                    seen.insert(it.id.clone());
                    seen_order.push_back(it.id.clone());
                }
                Some(cur)
            }
            None => store.newest_arrival().ok().flatten(),
        };
        let mut ping_tick = tokio::time::interval_at(tokio::time::Instant::now() + ping, ping);
        let mut follow = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        loop {
            let batch: Vec<lucina_store::timeline::StoredItem> = tokio::select! {
                msg = live.recv() => match msg {
                    Ok(it) => vec![it],
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => store.after_arrival(cursor.as_deref(), TIMELINE_REPLAY_MAX).unwrap_or_default(),
                    Err(_) => return,
                },
                _ = follow.tick() => {
                    // Publishes other writers' new lines (they arrive on `live`), then
                    // catches up on anything that arrived after the cursor and was missed.
                    let _ = store.poll();
                    store.after_arrival(cursor.as_deref(), TIMELINE_REPLAY_MAX).unwrap_or_default()
                }
                _ = ping_tick.tick() => {
                    if tx.send(Bytes::from_static(b": ping\n\n")).await.is_err() { return; }
                    continue;
                }
                _ = tx.closed() => return,
            };
            for it in batch {
                // Replayed or already sent. Live items and the follow batch both
                // come in arrival order, so the last one sent is the cursor.
                if seen.contains(&it.id) {
                    continue;
                }
                if tx.send(sse_item(&it)).await.is_err() {
                    return;
                }
                cursor = Some(it.id.clone());
                seen.insert(it.id.clone());
                seen_order.push_back(it.id.clone());
                if seen_order.len() > 4096 {
                    if let Some(old) = seen_order.pop_front() {
                        seen.remove(&old);
                    }
                }
            }
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>);
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(resp)
}

// ---------------------------------------------------------------- device bridge

fn is_command_id(s: &str) -> bool {
    s.len() == 36 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

async fn device(
    st: &AppState,
    method: &Method,
    sub: Option<&str>,
    command_id: Option<&str>,
    action: Option<&str>,
    body: Body,
) -> ApiResult {
    let internal =
        |e: lucina_store::StoreError| ApiError::new(500, "internal_error", e.to_string());
    let now = Utc::now();
    match (sub, command_id) {
        (Some("events"), None) => {
            if method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let body = read_json(body, MAX_DEVICE_BODY_BYTES).await?;
            let events = body["events"]
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| ApiError::bad_request("events must be a non-empty array"))?;
            if events.len() > MAX_EVENTS_PER_BATCH {
                return Err(ApiError::bad_request(format!(
                    "at most {MAX_EVENTS_PER_BATCH} events per request"
                )));
            }
            let out = st.device.events.ingest(events, now).map_err(internal)?;
            tracing::info!(
                accepted = out["accepted"].as_array().map(Vec::len).unwrap_or(0),
                "secretary device events"
            );
            Ok(ok(out))
        }
        (Some("state"), None) => match method.as_str() {
            "GET" => Ok(ok(json!({"state": st.device.read_state()}))),
            "POST" => {
                let body = read_json(body, MAX_DEVICE_BODY_BYTES).await?;
                let state = validate_state(&body, now).map_err(ApiError::bad_request)?;
                st.device.save_state(&state).map_err(internal)?;
                let n = |k: &str| state[k].as_array().map(Vec::len).unwrap_or(0);
                Ok(ok(json!({
                    "receivedAt": state["receivedAt"],
                    "counts": {"alarms": n("alarms"), "timers": n("timers"), "reminders": n("reminders"), "events": n("events")},
                    "focus": state["focus"],
                })))
            }
            _ => Err(ApiError::method_not_allowed(&["GET", "POST"])),
        },
        (Some("commands"), None) => {
            if method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            Ok(ok(
                json!({"commands": st.device.pending_commands(now).iter().map(|c| public_command(c, now)).collect::<Vec<_>>()}),
            ))
        }
        (Some("commands"), Some(id)) => {
            let not_found = || ApiError::new(404, "command_not_found", "No such device command");
            if !is_command_id(id) {
                return Err(not_found());
            }
            match action {
                None => {
                    if method != Method::GET {
                        return Err(ApiError::method_not_allowed(&["GET"]));
                    }
                    let c = st.device.find_command(id).ok_or_else(not_found)?;
                    Ok(ok(json!({"command": public_command(&c, now)})))
                }
                Some("result") => {
                    if method != Method::POST {
                        return Err(ApiError::method_not_allowed(&["POST"]));
                    }
                    let body = read_json(body, MAX_BODY_BYTES).await?;
                    let okv = body["ok"]
                        .as_bool()
                        .ok_or_else(|| ApiError::bad_request("ok must be a boolean"))?;
                    for key in ["error", "deviceObjectId"] {
                        if !(body[key].is_null() || body[key].is_string()) {
                            return Err(ApiError::bad_request(format!("{key} must be a string")));
                        }
                    }
                    let (mut command, duplicate) = st
                        .device
                        .record_result(
                            id,
                            okv,
                            body["error"].as_str().unwrap_or(""),
                            body["deviceObjectId"].as_str().unwrap_or(""),
                            now,
                        )
                        .map_err(|e| match e {
                            ResultError::NotFound => not_found(),
                            ResultError::Expired => ApiError::new(
                                409,
                                "command_expired",
                                "The command expired before a result arrived",
                            ),
                            ResultError::AlreadyResolved => ApiError::new(
                                409,
                                "already_resolved",
                                "A different result was already recorded",
                            ),
                        })?;
                    if !duplicate {
                        let item = st.timeline.record(NewItem {
                            channel: command["channel"].as_str().unwrap_or("other").into(),
                            role: "lucina".into(),
                            text: result_text(&command),
                            reply_to: command["timelineItemId"].as_str().map(str::to_owned),
                            meta: Some(Meta {
                                command_id: command["id"].as_str().map(str::to_owned),
                                ok: command["result"]["ok"].as_bool(),
                                ..Meta::kind("device_command_result")
                            }),
                            ..NewItem::default()
                        });
                        if let Some(item) = item {
                            if let Ok(Some(c)) = st
                                .device
                                .update_command(id, json!({"confirmationItemId": item.id}))
                            {
                                command = c;
                            }
                        }
                    }
                    Ok(ok(json!({
                        "command": public_command(&command, now),
                        "confirmationItemId": command["confirmationItemId"].as_str(),
                        "duplicate": duplicate,
                    })))
                }
                Some(_) => Err(ApiError::not_found()),
            }
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_and_ids() {
        assert_eq!(
            clip_sentences("Uno. Due!  Tre? Quattro. Cinque. Sei. Sette.", 6),
            "Uno. Due! Tre? Quattro. Cinque. Sei."
        );
        assert_eq!(clip_sentences("v1.2 is out. ok", 6), "v1.2 is out. ok");
        assert!(
            iso_with_offset("2026-10-22T09:30:00+02:00")
                && iso_with_offset("2026-10-22T07:30Z")
                && !iso_with_offset("2026-10-22T09:30:00")
        );
        assert_eq!(decode("a%20b+c"), "a b c");
        assert!(is_item_id("3d0b8a52-1f7e") && !is_item_id("zz"));
    }
}

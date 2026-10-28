//! Lucina's own computer for the apps and the MCP (`/api/secretary/computer/*`).
//!
//! Two isolated Cua sandboxes on nezopt: the Linux desktop `lucina-workspace` and the
//! Android `lucina-android` (crate `lucina_computer_use`). Nothing here runs a host
//! command: tasks go to the Hermes engine pinned to the workspace, direct calls go to
//! the Cua MCP proxy, and guest paths that name host locations are refused.
//!
//! - `GET  /computer`                  status (task engine, sandboxes, who drives each)
//! - `POST /computer/task`             `{task, async?}`: Lucina's engine does the whole task;
//!   with `async: true` it answers 202 `{jobId}` at once (MCP clients time out long before 200 s)
//! - `GET  /computer/task/{id}`        a job's state and, once done, its receipts
//! - `POST /computer/task/{id}/cancel` stop a job
//! - `POST /computer/call`             `{sandbox?, op, ...}`: `spaces`, `bash`, `write`, `tools`, `tool`, `screenshot`
//! - `GET  /computer/screenshot?sandbox=` the current screen as PNG (the apps' fallback view)
//! - `GET  /computer/control?sandbox=` who drives, plus the recent takeover/input log
//! - `POST /computer/control`          `{sandbox, action: take|release|heartbeat, device}`
//! - `POST /computer/input`            `{sandbox, device, events}`: the owner's input batch (fallback view)
//! - `POST /computer/view`             `{sandbox, mode: watch|control, device}`: a live-view ticket
//!   and the view id the proxy (`crate::computer_view`) serves it under
//!
//! While the owner drives a sandbox (`control` take), Lucina's and agents' mutating calls on
//! it are refused with 409 `owner_driving`; looking (screenshot, tool lists) still works.
//! The caller is named by secretary-server in `x-lucina-caller` (`mcp:claude`, `app`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine as _;
use lucina_computer_use::viewer::{self, ViewMode, ViewerConfig, VIEW_TTL};
use lucina_computer_use::{
    control, image_of, run_task, shared_client, text_of, Cancel, Sandbox, SandboxId,
};
use serde_json::{json, Value};

use crate::api::{ok, ok_status, read_json, ApiError};

type ApiResult = Result<axum::response::Response, ApiError>;

const BODY_LIMIT: usize = 512 * 1024;
/// Events in one owner input batch.
const MAX_EVENTS: usize = 50;
/// Characters one `type` event may carry.
const MAX_TYPE_CHARS: usize = 2000;
/// Finished jobs are kept this long for `GET /computer/task/{id}`.
const JOB_KEEP: Duration = Duration::from_secs(3600);
const MAX_JOBS: usize = 64;

/// Driver tools that only look: allowed while the owner drives.
const LOOK_TOOLS: [&str; 18] = [
    "get_desktop_state",
    "get_screen_size",
    "list_apps",
    "list_windows",
    "get_window_state",
    "get_accessibility_tree",
    "get_cursor_position",
    "get_browser_state",
    "zoom",
    "health_report",
    "get_config",
    "get_guide",
    "list_sessions",
    "get_session",
    "get_session_state",
    "get_recording_state",
    "get_agent_cursor_state",
    "check_permissions",
];

fn sandbox_id(name: Option<&str>) -> Result<SandboxId, ApiError> {
    let name = name.unwrap_or("workspace");
    SandboxId::parse(name).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown sandbox {name:?} (use \"workspace\" or \"android\")"
        ))
    })
}

fn sandbox(id: SandboxId) -> Sandbox {
    match id {
        SandboxId::Workspace => Sandbox::workspace(),
        SandboxId::Android => Sandbox::android(),
    }
}

pub(crate) fn cu_error(e: lucina_computer_use::Error) -> ApiError {
    let status = match e.code() {
        "not_enabled"
        | "launcher_unavailable"
        | "sandbox_unavailable"
        | "recovery_required"
        | "viewer_unavailable" => 503,
        "bad_request" | "output_limit" => 400,
        "busy" | "owner_driving" | "not_driving" | "owner_idle" => 409,
        "quota_exhausted" => 429,
        "deadline_exceeded" => 504,
        _ => 502,
    };
    ApiError::new(status, e.code(), e.to_string())
}

/// `clipboard_read` / `clipboard_write`: owner actions that are not input.
fn is_clipboard_event(e: &Value) -> bool {
    e.get("kind")
        .and_then(Value::as_str)
        .is_some_and(|k| k.starts_with("clipboard_"))
}

/// `x-lucina-caller` from secretary-server: `[a-z0-9:-]{1,40}`, else `app`.
pub(crate) fn caller(headers: &HeaderMap) -> String {
    headers
        .get("x-lucina-caller")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| {
            (1..=40).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b':' || b == b'-')
        })
        .unwrap_or("app")
        .to_owned()
}

/// A device label for the audit log: printable, at most 60 chars.
fn device(v: &Value) -> String {
    let d: String = v
        .get("device")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    if d.trim().is_empty() {
        "owner".into()
    } else {
        d.trim().to_owned()
    }
}

fn query_sandbox(uri: &Uri) -> Option<&str> {
    uri.query()
        .unwrap_or("")
        .split('&')
        .find_map(|kv| kv.strip_prefix("sandbox="))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The screen of a sandbox through the driver's `get_desktop_state`.
/// A shopping drive stopped at the checkout (or a sign-in): the owner decides
/// and pays himself. Creates an owner decision (owner-only by its words), keeps
/// the screenshot under the data dir, and notifies his app with a link to the
/// Computer view, where the basket's tab is open.
pub async fn hand_over(st: &crate::AppState, h: lucina_computer_use::jev_drive::Handover) {
    let total = h.total.clone().unwrap_or_else(|| "total not shown".into());
    let question = format!(
        "Your basket at {} is at the checkout ({total}). Place the order yourself?",
        lucina_jev::clip(&h.site, 120)
    );
    let mut shot = None;
    if let Some(b64) = &h.screenshot_b64 {
        if let Ok(png) = base64::engine::general_purpose::STANDARD.decode(b64) {
            let dir = st.cfg.data_dir.join("lucina-rs/checkout");
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let path = dir.join(format!("{ms}.png"));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, png).is_ok() {
                shot = Some(path.display().to_string());
            }
        }
    }
    let body = json!({
        "question": question,
        "options": ["I'll pay in Lucina's open tab", "Cancel it"],
        "context": format!("{}\n{}\nScreenshot: {}", h.reason, h.reply, shot.as_deref().unwrap_or("none")),
        "reversible": true,
        "impact": "medium",
        "urgency": "high",
        "topic": "computer-checkout",
        "asking_client": "lucina",
    });
    match crate::decide::create(st, &body).await {
        Ok(d) => tracing::info!(target: "lucina_jev::drive", decision = %d.decision_id, owner_only = d.owner_only, "checkout handed to the owner"),
        Err(e) => tracing::warn!(target: "lucina_jev::drive", error = ?e, "checkout decision failed; notifying only"),
    }
    let note = json!({
        "category": "lucina_message",
        "title": "Basket ready at the checkout",
        "body": lucina_jev::clip(&h.reply, 480),
        "route": "/computer",
        "dedupKey": format!("computer-checkout:{}", lucina_jev::clip(&h.site, 60)),
    });
    let _ = crate::bridges::notify(&st.secretary, &note).await;
}

async fn screen(sb: &Sandbox) -> Result<(Option<(String, String)>, String), ApiError> {
    let r = sb
        .call_tool("get_desktop_state", json!({}))
        .await
        .map_err(cu_error)?;
    Ok((image_of(&r), text_of(&r)))
}

// ---------------------------------------------------------------- jobs

struct Job {
    who: String,
    task: String,
    started_ms: u64,
    finished: Option<Instant>,
    finished_ms: Option<u64>,
    result: Option<Result<Value, (String, String)>>,
    cancel: Cancel,
}

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static J: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

fn job_json(id: &str, j: &Job) -> Value {
    let (status, result, error) = match &j.result {
        None => ("running", None, None),
        Some(Ok(v)) => ("done", Some(v.clone()), None),
        Some(Err((code, msg))) if code == "cancelled" => (
            "cancelled",
            None,
            Some(json!({"code": code, "message": msg})),
        ),
        Some(Err((code, msg))) => ("failed", None, Some(json!({"code": code, "message": msg}))),
    };
    json!({
        "jobId": id,
        "status": status,
        "who": j.who,
        "task": j.task,
        "startedAt": j.started_ms,
        "finishedAt": j.finished_ms,
        "elapsedMs": j.finished_ms.unwrap_or_else(now_ms).saturating_sub(j.started_ms),
        "result": result,
        "error": error,
    })
}

fn job_not_found() -> ApiError {
    ApiError::new(
        404,
        "job_not_found",
        "no such computer job (jobs are kept for an hour)",
    )
}

fn prune_jobs(map: &mut HashMap<String, Job>) {
    map.retain(|_, j| j.finished.is_none_or(|t| t.elapsed() < JOB_KEEP));
}

fn start_job(who: String, task: String) -> Result<String, ApiError> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let cancel = Cancel::new();
    {
        let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
        prune_jobs(&mut map);
        if map.len() >= MAX_JOBS {
            return Err(ApiError::new(
                429,
                "too_many_jobs",
                "too many computer jobs kept; try later",
            ));
        }
        if map.values().any(|j| j.result.is_none()) {
            return Err(cu_error(lucina_computer_use::Error::Busy));
        }
        map.insert(
            id.clone(),
            Job {
                who: who.clone(),
                task: task.chars().take(300).collect(),
                started_ms: now_ms(),
                finished: None,
                finished_ms: None,
                result: None,
                cancel: cancel.clone(),
            },
        );
    }
    let jid = id.clone();
    tokio::spawn(async move {
        tracing::info!(job = %jid, who = %who, chars = task.chars().count(), "computer job started");
        let out = run_task(&who, &task, &cancel)
            .await
            .map(|r| r.to_json())
            .map_err(|e| (e.code().to_owned(), e.to_string()));
        let status = match &out {
            Ok(v) => v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            Err((code, _)) => code.clone(),
        };
        tracing::info!(job = %jid, status = %status, "computer job finished");
        let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = map.get_mut(&jid) {
            j.result = Some(out);
            j.finished = Some(Instant::now());
            j.finished_ms = Some(now_ms());
        }
    });
    Ok(id)
}

// ---------------------------------------------------------------- input

/// Driver tools that take `delivery_mode`. The owner's input is real, focused input:
/// the driver's default (`background`) is refused by Chromium/Electron windows.
const FOREGROUND_TOOLS: [&str; 4] = ["click", "scroll", "type_text", "press_key"];

/// One fallback input event → `(driver tool, arguments)`, delivered in the foreground.
pub(crate) fn input_event(e: &Value) -> Result<(&'static str, Value), ApiError> {
    let (tool, mut args) = driver_input(e)?;
    if FOREGROUND_TOOLS.contains(&tool) {
        args["delivery_mode"] = json!("foreground");
    }
    Ok((tool, args))
}

fn driver_input(e: &Value) -> Result<(&'static str, Value), ApiError> {
    let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
    let coord = |k: &str| -> Result<i64, ApiError> {
        e.get(k)
            .and_then(Value::as_i64)
            .filter(|v| (0..=10_000).contains(v))
            .ok_or_else(|| ApiError::bad_request(format!("{kind}: {k} must be a pixel 0..10000")))
    };
    let text = |k: &str| -> Result<String, ApiError> {
        let t = e.get(k).and_then(Value::as_str).unwrap_or("");
        if t.is_empty() || t.chars().count() > MAX_TYPE_CHARS {
            return Err(ApiError::bad_request(format!(
                "{kind}: {k} must be 1..{MAX_TYPE_CHARS} chars"
            )));
        }
        Ok(t.to_owned())
    };
    match kind {
        "click" => {
            let button = e.get("button").and_then(Value::as_str).unwrap_or("left");
            if !["left", "right", "middle"].contains(&button) {
                return Err(ApiError::bad_request(
                    "click: button must be left, right or middle",
                ));
            }
            let count = e
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 3);
            Ok((
                "click",
                json!({"scope": "desktop", "x": coord("x")?, "y": coord("y")?, "button": button, "count": count}),
            ))
        }
        "down" | "up" => {
            let button = e.get("button").and_then(Value::as_str).unwrap_or("left");
            if !["left", "right", "middle"].contains(&button) {
                return Err(ApiError::bad_request(format!("{kind}: button must be left, right or middle")));
            }
            let tool = if kind == "down" { "mouse_button_down" } else { "mouse_button_up" };
            Ok((tool, json!({"scope": "desktop", "x": coord("x")?, "y": coord("y")?, "button": button})))
        }
        "scroll" => {
            let dir = e.get("direction").and_then(Value::as_str).unwrap_or("");
            if !["up", "down", "left", "right"].contains(&dir) {
                return Err(ApiError::bad_request(
                    "scroll: direction must be up, down, left or right",
                ));
            }
            let amount = e
                .get("amount")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 20);
            Ok((
                "scroll",
                json!({"scope": "desktop", "x": coord("x")?, "y": coord("y")?, "direction": dir, "amount": amount}),
            ))
        }
        "move" => Ok(("move_cursor", json!({"x": coord("x")?, "y": coord("y")?}))),
        "type" => Ok(("type_text", json!({"text": text("text")?}))),
        "key" => {
            let key = e.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() || key.len() > 32 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(ApiError::bad_request("key: key must be a key name"));
            }
            let mods: Vec<&str> = e
                .get("modifiers")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if mods
                .iter()
                .any(|m| !["ctrl", "shift", "alt", "super"].contains(m))
            {
                return Err(ApiError::bad_request(
                    "key: modifiers are ctrl, shift, alt, super",
                ));
            }
            let mut args = json!({"key": key});
            if !mods.is_empty() {
                args["modifiers"] = json!(mods);
            }
            Ok(("press_key", args))
        }
        "clipboard_write" => Ok(("clipboard_write", json!({"text": text("text")?}))),
        "clipboard_read" => Ok(("clipboard_read", json!({}))),
        _ => Err(ApiError::bad_request(
            "event kind must be click, down, up, scroll, move, type, key, clipboard_write or clipboard_read",
        )),
    }
}

/// What the audit log keeps of a batch: kinds and sizes, never typed text.
fn batch_summary(events: &[Value]) -> Value {
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e.get("kind").and_then(Value::as_str).unwrap_or("?"))
        .collect();
    let text_chars: usize = events
        .iter()
        .filter_map(|e| e.get("text").and_then(Value::as_str))
        .map(|t| t.chars().count())
        .sum();
    json!({"via": "fallback", "events": events.len(), "kinds": kinds, "textChars": text_chars})
}

// ---------------------------------------------------------------- routes

pub async fn computer(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    rest: &[String],
    body: Body,
) -> ApiResult {
    let p = |i: usize| rest.get(i).map(String::as_str);
    let only = |m: Method| -> Result<(), ApiError> {
        if *method == m {
            Ok(())
        } else {
            Err(ApiError::method_not_allowed(&[m.as_str()]))
        }
    };
    match (p(0), p(1), p(2), rest.len()) {
        (None, ..) => {
            only(Method::GET)?;
            let cfg = shared_client().config();
            Ok(ok(json!({
                "taskEngine": {"enabled": cfg.enabled, "ready": cfg.ready(), "timeoutSecs": cfg.timeout.as_secs()},
                // TypeSafe Jev: intent routing, the first-tool hint and the Jev engine (tried before Hermes).
                "jev": lucina_jev::shared().stats(),
                "jevEngine": lucina_computer_use::jev_drive::enabled(),
                "sandboxes": [
                    {"name": "workspace", "space": lucina_computer_use::WORKSPACE_SPACE, "kind": "linux-desktop", "control": control().state(SandboxId::Workspace)},
                    {"name": "android", "space": lucina_computer_use::ANDROID_SPACE, "kind": "android", "control": control().state(SandboxId::Android)},
                ],
            })))
        }
        (Some("task"), None, _, 1) => {
            only(Method::POST)?;
            let v = read_json(body, BODY_LIMIT).await?;
            let task = v
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ApiError::bad_request("task is required"))?
                .to_owned();
            if !shared_client().ready() {
                return Err(ApiError::new(
                    503,
                    "computer_use_disabled",
                    "Lucina's computer-use engine is not enabled (LUCINA_COMPUTER_USE_ENABLED) or its launcher is missing",
                ));
            }
            let who = caller(headers);
            if v.get("async").and_then(Value::as_bool) == Some(true) {
                if control().owner_driving(SandboxId::Workspace) {
                    return Err(cu_error(lucina_computer_use::Error::OwnerDriving));
                }
                let id = start_job(who, task)?;
                let map = jobs().lock().unwrap_or_else(|e| e.into_inner());
                let j = map
                    .get(&id)
                    .map(|j| job_json(&id, j))
                    .unwrap_or(json!({"jobId": id}));
                return Ok(ok_status(202, j));
            }
            tracing::info!(who = %who, chars = task.chars().count(), "computer task started");
            let result = run_task(&who, &task, &Cancel::new())
                .await
                .map_err(cu_error)?;
            let out = result.to_json();
            let status = out
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            tracing::info!(status = %status, "computer task finished");
            Ok(ok(out))
        }
        (Some("task"), Some(id), None, 2) => {
            only(Method::GET)?;
            let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
            prune_jobs(&mut map);
            let j = map.get(id).ok_or_else(job_not_found)?;
            Ok(ok(job_json(id, j)))
        }
        (Some("task"), Some(id), Some("cancel"), 3) => {
            only(Method::POST)?;
            let map = jobs().lock().unwrap_or_else(|e| e.into_inner());
            let j = map.get(id).ok_or_else(job_not_found)?;
            j.cancel.cancel();
            tracing::info!(job = %id, who = %caller(headers), "computer job cancel requested");
            Ok(ok(job_json(id, j)))
        }
        (Some("call"), None, _, 1) => {
            only(Method::POST)?;
            let v = read_json(body, BODY_LIMIT).await?;
            let id = sandbox_id(v.get("sandbox").and_then(Value::as_str))?;
            let sb = sandbox(id);
            let op = v.get("op").and_then(Value::as_str).unwrap_or("");
            let s = |k: &str| v.get(k).and_then(Value::as_str);
            let who = caller(headers);
            tracing::info!(sandbox = %sb.space(), op, tool = s("tool").unwrap_or(""), who = %who, "computer call");
            let mutating = match op {
                "bash" | "write" => true,
                "tool" => !s("tool").is_some_and(|t| LOOK_TOOLS.contains(&t)),
                _ => false,
            };
            if mutating {
                control().agent_call(id, &who).map_err(cu_error)?;
            }
            let r = match op {
                "spaces" => sb.spaces().await,
                "bash" => {
                    let cmd =
                        s("command").ok_or_else(|| ApiError::bad_request("command is required"))?;
                    sb.bash(cmd, v.get("timeoutSecs").and_then(Value::as_u64))
                        .await
                }
                "write" => {
                    let path =
                        s("path").ok_or_else(|| ApiError::bad_request("path is required"))?;
                    sb.write(path, s("content").unwrap_or("")).await
                }
                "tools" => sb.tools(s("tool")).await,
                "tool" => {
                    let tool =
                        s("tool").ok_or_else(|| ApiError::bad_request("tool is required"))?;
                    sb.call_tool(
                        tool,
                        v.get("arguments").cloned().unwrap_or_else(|| json!({})),
                    )
                    .await
                }
                "screenshot" => {
                    let (img, text) = screen(&sb).await?;
                    return Ok(ok(json!({
                        "text": text,
                        "image": img.map(|(mime, data)| json!({"mimeType": mime, "data": data})),
                    })));
                }
                _ => {
                    return Err(ApiError::bad_request(
                        "op must be one of spaces, bash, write, tools, tool, screenshot",
                    ))
                }
            }
            .map_err(cu_error)?;
            Ok(ok(json!({
                "text": text_of(&r),
                "image": image_of(&r).map(|(mime, data)| json!({"mimeType": mime, "data": data})),
            })))
        }
        (Some("lan"), None, _, 1) => {
            // The home-network door (lan.rs): where it is and which public IP the house has,
            // so secretary-server can offer it to the owner when they are at home.
            only(Method::GET)?;
            let Some(cfg) = crate::lan::config().filter(|_| crate::lan::ready()) else {
                return Ok(ok(json!({"url": null})));
            };
            Ok(ok(
                json!({"url": cfg.url, "publicIp": crate::lan::public_ip().await}),
            ))
        }
        (Some("screenshot"), None, _, 1) => {
            only(Method::GET)?;
            let sb = sandbox(sandbox_id(query_sandbox(uri))?);
            let (img, _) = tokio::time::timeout(Duration::from_secs(30), screen(&sb))
                .await
                .map_err(|_| ApiError::new(504, "timeout", "screenshot timed out"))??;
            let (mime, data) = img
                .ok_or_else(|| ApiError::new(502, "no_image", "the sandbox returned no screen"))?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| ApiError::new(502, "bad_image", e.to_string()))?;
            let mut resp = (StatusCode::OK, png).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("image/png")),
            );
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(resp)
        }
        (Some("control"), None, _, 1) => {
            if *method == Method::GET {
                let id = sandbox_id(query_sandbox(uri))?;
                // The viewer asset version rides along so the apps can preload
                // `viewer.js` while the view is still being minted.
                return Ok(ok(json!({
                    "state": control().state(id),
                    "recent": control().recent(id, 20),
                    "viewer": {"assets": crate::computer_view::viewer_version().await},
                })));
            }
            only(Method::POST)?;
            let v = read_json(body, BODY_LIMIT).await?;
            let id = sandbox_id(v.get("sandbox").and_then(Value::as_str))?;
            let dev = device(&v);
            match v.get("action").and_then(Value::as_str).unwrap_or("") {
                "take" => {
                    let t = control().take(id, &dev);
                    Ok(ok(json!({
                        "epoch": t.epoch,
                        "stoppedTask": t.stopped_task,
                        "state": control().state(id),
                    })))
                }
                "heartbeat" => {
                    // `active`: real input since the last heartbeat (newer apps say;
                    // without it the heartbeat counts as input, as before).
                    let beat = match v.get("active").and_then(Value::as_bool) {
                        Some(active) => control().heartbeat_idle(id, active),
                        None => control().heartbeat(id),
                    };
                    if let Err(e) = beat {
                        if e.code() == "owner_idle" {
                            crate::computer_view::drop_control_views(id);
                        }
                        return Err(cu_error(e));
                    }
                    Ok(ok(json!({"state": control().state(id)})))
                }
                "release" => {
                    control().release(id, &dev);
                    crate::computer_view::drop_control_views(id);
                    Ok(ok(json!({"state": control().state(id)})))
                }
                _ => Err(ApiError::bad_request(
                    "action must be take, release or heartbeat",
                )),
            }
        }
        (Some("input"), None, _, 1) => {
            only(Method::POST)?;
            let v = read_json(body, BODY_LIMIT).await?;
            let id = sandbox_id(v.get("sandbox").and_then(Value::as_str))?;
            let dev = device(&v);
            let events = v
                .get("events")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_EVENTS)
                .ok_or_else(|| {
                    ApiError::bad_request(format!("events must be 1..{MAX_EVENTS} items"))
                })?;
            let calls = events
                .iter()
                .map(input_event)
                .collect::<Result<Vec<_>, _>>()?;
            control()
                .owner_input(id, &dev, batch_summary(events))
                .map_err(cu_error)?;
            // Clipboard reads and writes are allowed while driving but are not input:
            // they never keep an idle owner's lease.
            if events.iter().any(|e| !is_clipboard_event(e)) {
                control().active(id).map_err(cu_error)?;
            }
            let sb = sandbox(id);
            let mut done = 0usize;
            let mut outputs = Vec::new();
            for (tool, args) in calls {
                match sb.call_tool(tool, args).await {
                    Ok(r) => {
                        done += 1;
                        if tool == "clipboard_read" {
                            outputs.push(json!({"tool": tool, "text": text_of(&r)}));
                        }
                    }
                    Err(e) => {
                        return Ok(ok(json!({
                            "done": done,
                            "error": {"code": e.code(), "message": e.to_string(), "tool": tool},
                            "outputs": outputs,
                        })))
                    }
                }
            }
            Ok(ok(json!({"done": done, "outputs": outputs})))
        }
        (Some("view"), None, _, 1) => {
            only(Method::POST)?;
            let v = read_json(body, BODY_LIMIT).await?;
            let id = sandbox_id(v.get("sandbox").and_then(Value::as_str))?;
            let mode = ViewMode::parse(v.get("mode").and_then(Value::as_str).unwrap_or("watch"))
                .ok_or_else(|| ApiError::bad_request("mode must be watch or control"))?;
            let dev = device(&v);
            if mode == ViewMode::Control && !control().owner_driving(id) {
                return Err(cu_error(lucina_computer_use::Error::NotDriving));
            }
            let epoch = control().epoch(id);
            let (ticket, left) = viewer::mint_cached(&ViewerConfig::from_env(), id, mode, VIEW_TTL)
                .await
                .map_err(cu_error)?;
            let view_id = crate::computer_view::open(id, mode, &dev, epoch, &ticket.machine);
            control().note(
                id,
                "view_open",
                json!({"device": dev, "mode": mode.as_str()}),
            );
            Ok(ok(json!({
                "viewId": view_id,
                "sandbox": id.as_str(),
                "mode": mode.as_str(),
                "ticket": ticket.ticket,
                "expiresAt": ticket.expires_at,
                "ttlSecs": left.as_secs(),
                "epoch": epoch,
            })))
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_events_are_not_input() {
        assert!(is_clipboard_event(&json!({"kind": "clipboard_read"})));
        assert!(is_clipboard_event(
            &json!({"kind": "clipboard_write", "text": "x"})
        ));
        assert!(!is_clipboard_event(
            &json!({"kind": "click", "x": 1, "y": 2})
        ));
        assert!(!is_clipboard_event(&json!({"kind": "key", "key": "Enter"})));
        assert!(!is_clipboard_event(&json!({})));
    }

    #[test]
    fn input_events_map_to_driver_tools() {
        let fg = "foreground";
        let (t, a) = input_event(&json!({"kind": "click", "x": 10, "y": 20})).unwrap();
        assert_eq!(t, "click");
        assert_eq!(
            a,
            json!({"scope": "desktop", "x": 10, "y": 20, "button": "left", "count": 1, "delivery_mode": fg})
        );
        let (t, a) =
            input_event(&json!({"kind": "scroll", "x": 1, "y": 2, "direction": "down"})).unwrap();
        assert_eq!(
            (t, a["amount"].clone(), a["delivery_mode"].clone()),
            ("scroll", json!(3), json!(fg))
        );
        let (t, a) =
            input_event(&json!({"kind": "key", "key": "Return", "modifiers": ["ctrl"]})).unwrap();
        assert_eq!(
            (t, a),
            (
                "press_key",
                json!({"key": "Return", "modifiers": ["ctrl"], "delivery_mode": fg})
            )
        );
        let (t, a) = input_event(&json!({"kind": "type", "text": "hi"})).unwrap();
        assert_eq!(
            (t, a),
            ("type_text", json!({"text": "hi", "delivery_mode": fg}))
        );
        // The tools without the parameter do not get it (the driver rejects unknown arguments).
        assert!(input_event(&json!({"kind": "move", "x": 1, "y": 1}))
            .unwrap()
            .1
            .get("delivery_mode")
            .is_none());
        let (t, a) = input_event(&json!({"kind": "down", "x": 5, "y": 6})).unwrap();
        assert_eq!(
            (t, a),
            (
                "mouse_button_down",
                json!({"scope": "desktop", "x": 5, "y": 6, "button": "left"})
            )
        );
        let (t, a) = input_event(&json!({"kind": "up", "x": 7, "y": 8, "button": "left"})).unwrap();
        assert_eq!(
            (t, a),
            (
                "mouse_button_up",
                json!({"scope": "desktop", "x": 7, "y": 8, "button": "left"})
            )
        );
        for bad in [
            json!({"kind": "click", "x": -1, "y": 0}),
            json!({"kind": "click", "x": 1}),
            json!({"kind": "click", "x": 1, "y": 1, "button": "x"}),
            json!({"kind": "scroll", "x": 1, "y": 1, "direction": "in"}),
            json!({"kind": "key", "key": "a b"}),
            json!({"kind": "key", "key": "a", "modifiers": ["hyper"]}),
            json!({"kind": "type", "text": ""}),
            json!({"kind": "type", "text": "x".repeat(MAX_TYPE_CHARS + 1)}),
            json!({"kind": "bash", "command": "id"}),
            json!({"kind": "down", "x": 1, "y": 1, "button": "x"}),
            json!({"kind": "up", "x": -1, "y": 1}),
        ] {
            assert!(input_event(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_audit_summary_never_carries_typed_text() {
        let s = batch_summary(&[
            json!({"kind": "type", "text": "hunter2"}),
            json!({"kind": "key", "key": "Return"}),
        ]);
        assert_eq!(s["textChars"], 7);
        assert_eq!(s["kinds"], json!(["type", "key"]));
        assert!(!s.to_string().contains("hunter2"));
    }

    #[test]
    fn caller_header_is_sanitized() {
        let mut h = HeaderMap::new();
        assert_eq!(caller(&h), "app");
        h.insert("x-lucina-caller", HeaderValue::from_static("mcp:claude"));
        assert_eq!(caller(&h), "mcp:claude");
        h.insert("x-lucina-caller", HeaderValue::from_static("Evil Name"));
        assert_eq!(caller(&h), "app");
    }
}

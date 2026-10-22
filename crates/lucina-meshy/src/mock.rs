//! A local stand-in for the Meshy API (`meshy-mock` binary, and the tests).
//!
//! Meshy retired its shared test-mode key on 2026-09-16 (it now answers
//! `401 Invalid API key`), so end-to-end runs use this mock: the same paths
//! under `/openapi`, `202 {"result": id}` on create, tasks that go PENDING →
//! IN_PROGRESS → SUCCEEDED over a few polls with `Retry-After`, the error
//! shapes of the OpenAPI spec (400/401/402/404/429/5xx), `consumed_credits`
//! from the price table and a credit balance. Like the old test mode it
//! returns sample results: GLBs from a fixtures directory (a chest for
//! prompts about chests/boxes, a rigged knight for everything else).
//!
//! Prompt markers simulate failures: `[fail]` (task FAILED), `[never]` (stays
//! IN_PROGRESS), `[402]`, `[429]` (queue full), `[500]` (create fails).
//! `GET /_mock/requests` lists every request (method, path, body) for tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::api::TaskKind;
use crate::pricing::{self, TextureRes};

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub bind: SocketAddr,
    /// Where `chest.glb`, `knight.glb`, … live.
    pub assets: PathBuf,
    /// Polls until a task succeeds (default 3).
    pub polls_to_finish: u32,
    /// `Retry-After` seconds on running tasks (0 = header omitted).
    pub retry_after: u64,
    pub balance: i64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:0".parse().expect("addr"),
            assets: PathBuf::from("fixtures"),
            polls_to_finish: 3,
            retry_after: 0,
            balance: 1000,
        }
    }
}

#[derive(Debug, Clone)]
struct MockTask {
    kind: TaskKind,
    family: &'static str,
    created_at: i64,
    polls: u32,
    body: Value,
    outcome: &'static str,
    credits: u32,
}

#[derive(Default)]
pub struct MockState {
    cfg: Mutex<Option<MockConfig>>,
    base: Mutex<String>,
    tasks: Mutex<HashMap<String, MockTask>>,
    balance: Mutex<i64>,
    pub requests: Mutex<Vec<Value>>,
}

pub struct MockHandle {
    pub base_url: String,
    pub state: Arc<MockState>,
}

impl MockHandle {
    /// The API base to give the client (`…/openapi`).
    pub fn api_base(&self) -> String {
        format!("{}/openapi", self.base_url)
    }

    /// Create requests seen for a path (e.g. `/openapi/v1/rigging`).
    pub fn creates(&self, path: &str) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == path)
            .count()
    }
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"message": msg}))).into_response()
}

fn authorized(h: &HeaderMap) -> bool {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|k| crate::key::valid_key(k) && !k.contains("invalid"))
}

fn kind_of(path: &str) -> Option<TaskKind> {
    Some(match path {
        "v2/text-to-3d" => TaskKind::TextTo3d,
        "v1/image-to-3d" => TaskKind::ImageTo3d,
        "v1/multi-image-to-3d" => TaskKind::MultiImageTo3d,
        "v1/remesh" => TaskKind::Remesh,
        "v1/retexture" => TaskKind::Retexture,
        "v1/rigging" => TaskKind::Rigging,
        "v1/animations" => TaskKind::Animation,
        "v1/convert" => TaskKind::Convert,
        _ => return None,
    })
}

fn family_of(text: &str) -> &'static str {
    let t = text.to_lowercase();
    if [
        "chest", "box", "forziere", "cassa", "baule", "crate", "kiste",
    ]
    .iter()
    .any(|w| t.contains(w))
    {
        "chest"
    } else {
        "knight"
    }
}

fn texture_of(b: &Value) -> TextureRes {
    b["texture_resolution"]
        .as_str()
        .and_then(TextureRes::parse)
        .unwrap_or(TextureRes::K2)
}

impl MockState {
    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn parent(&self, id: Option<&str>) -> Option<MockTask> {
        self.tasks.lock().unwrap().get(id?).cloned()
    }

    fn asset(&self, file: &str, task: &str) -> String {
        format!(
            "{}/assets/{file}?task={task}&Expires=9999999999&Signature=mock",
            self.base.lock().unwrap()
        )
    }

    fn task_json(&self, id: &str, t: &MockTask) -> Value {
        let finish = self
            .cfg
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.polls_to_finish)
            .unwrap_or(3);
        let (status, progress) = match (t.outcome, t.polls) {
            (_, 0) => ("PENDING", 0),
            ("never", p) => ("IN_PROGRESS", (p * 10).min(95)),
            (_, p) if p < finish => ("IN_PROGRESS", (p * 100 / finish).min(99)),
            ("fail", _) => ("FAILED", 0),
            _ => ("SUCCEEDED", 100),
        };
        let mut v = json!({
            "id": id, "type": t.kind.as_str(), "status": status, "progress": progress,
            "created_at": t.created_at, "started_at": if t.polls > 0 { t.created_at + 1000 } else { 0 },
            "finished_at": if status == "SUCCEEDED" || status == "FAILED" { t.created_at + 60_000 } else { 0 },
            "expires_at": t.created_at + 3 * 86_400_000,
            "task_error": if status == "FAILED" { json!({"message": "Mock failure: the model could not be generated"}) } else { Value::Null },
            "consumed_credits": if status == "SUCCEEDED" { json!(t.credits) } else if status == "FAILED" { json!(0) } else { Value::Null },
            "preceding_tasks": 0,
        });
        if status != "SUCCEEDED" {
            return v;
        }
        let f = t.family;
        let model = |name: &str| self.asset(name, id);
        let formats: Vec<String> = t.body["target_formats"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut urls = serde_json::Map::new();
        let glb = match (t.kind, t.body["mode"].as_str()) {
            (TaskKind::TextTo3d, Some("preview")) if t.body["should_texture"] != json!(true) => {
                format!("{f}_preview.glb")
            }
            _ => format!("{f}.glb"),
        };
        urls.insert("glb".into(), json!(model(&glb)));
        let defaults: &[&str] = match (t.kind, t.body["mode"].as_str()) {
            (TaskKind::TextTo3d, Some("refine"))
            | (TaskKind::ImageTo3d, _)
            | (TaskKind::MultiImageTo3d, _) => &["fbx", "obj", "usdz"],
            _ => &[],
        };
        for fmt in defaults.iter().map(|s| s.to_string()).chain(formats) {
            if fmt != "glb" {
                urls.insert(fmt.clone(), json!(model(&format!("{f}.{fmt}"))));
            }
        }
        match t.kind {
            TaskKind::Rigging => {
                v["result"] = json!({
                    "rigged_character_glb_url": model("knight.glb"),
                    "rigged_character_fbx_url": model("knight.fbx"),
                    "basic_animations": {
                        "walking_glb_url": model("knight_walk.glb"),
                        "running_glb_url": model("knight_walk.glb"),
                    }
                });
            }
            TaskKind::Animation => {
                v["result"] = json!({
                    "animation_glb_url": model("knight_anim.glb"),
                    "animation_fbx_url": model("knight.fbx"),
                });
            }
            _ => {
                v["model_urls"] = Value::Object(urls);
                v["thumbnail_url"] = json!(self.asset(&format!("thumb_{f}.png"), id));
            }
        }
        v
    }
}

async fn create(
    State(st): State<Arc<MockState>>,
    Path((ver, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("{ver}/{name}");
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    st.requests
        .lock()
        .unwrap()
        .push(json!({"method": "POST", "path": format!("/openapi/{path}"), "body": b}));
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "Invalid API key");
    }
    let Some(kind) = kind_of(&path) else {
        return err(StatusCode::NOT_FOUND, "Not found");
    };
    if !b.is_object() {
        return err(StatusCode::BAD_REQUEST, "Invalid JSON body");
    }
    let text = [
        &b["prompt"],
        &b["text_style_prompt"],
        &b["texture_prompt"],
        &b["name"],
    ]
    .iter()
    .filter_map(|v| v.as_str())
    .collect::<Vec<_>>()
    .join(" ");
    if text.contains("[500]") {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
    }
    if text.contains("[402]") {
        return err(StatusCode::PAYMENT_REQUIRED, "Insufficient credits");
    }
    if text.contains("[429]") {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "NoMorePendingTasks: too many queued tasks",
        );
    }
    let input = b["input_task_id"]
        .as_str()
        .or(b["preview_task_id"].as_str())
        .or(b["rig_task_id"].as_str());
    let parent = st.parent(input);
    let (credits, family) = match kind {
        TaskKind::TextTo3d => match b["mode"].as_str() {
            Some("preview") => {
                let Some(p) = b["prompt"].as_str().filter(|p| !p.trim().is_empty()) else {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "prompt is required when mode is preview",
                    );
                };
                let model = b["ai_model"].as_str().unwrap_or("latest");
                (
                    pricing::text_preview(
                        model,
                        b["geometry_resolution"]
                            .as_str()
                            .is_some_and(|g| g != "standard"),
                    ),
                    family_of(p),
                )
            }
            Some("refine") => match &parent {
                Some(p) if p.kind == TaskKind::TextTo3d => {
                    (pricing::refine(texture_of(&b)), p.family)
                }
                _ => {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "preview_task_id must be a Text to 3D preview task",
                    )
                }
            },
            _ => return err(StatusCode::BAD_REQUEST, "mode must be preview or refine"),
        },
        TaskKind::ImageTo3d | TaskKind::MultiImageTo3d => {
            let has = b["image_url"].is_string()
                || b["image_urls"].as_array().is_some_and(|a| !a.is_empty());
            if !has {
                return err(StatusCode::BAD_REQUEST, "image_url is required");
            }
            let model = b["ai_model"].as_str().unwrap_or("latest");
            let tex = (b["should_texture"] != json!(false)).then(|| texture_of(&b));
            (
                pricing::image_to_3d(model, tex, false).unwrap_or(30),
                family_of(&text),
            )
        }
        TaskKind::Remesh | TaskKind::Retexture | TaskKind::Convert | TaskKind::Rigging => {
            let Some(p) = &parent else {
                return err(
                    StatusCode::BAD_REQUEST,
                    "input_task_id does not name a finished task",
                );
            };
            let c = match kind {
                TaskKind::Remesh => pricing::REMESH,
                TaskKind::Retexture => pricing::retexture(texture_of(&b)),
                TaskKind::Convert => pricing::CONVERT,
                _ => pricing::RIG,
            };
            (c, p.family)
        }
        TaskKind::Animation => {
            if parent.as_ref().map(|p| p.kind) != Some(TaskKind::Rigging) {
                return err(
                    StatusCode::BAD_REQUEST,
                    "rig_task_id must be a completed Rigging task",
                );
            }
            let n = if b["action_id"].is_i64() {
                1
            } else {
                b["action_ids"].as_array().map(|a| a.len()).unwrap_or(0)
            };
            if n == 0 || n > 10 {
                return err(StatusCode::BAD_REQUEST, "give action_id or 1-10 action_ids");
            }
            (pricing::ANIMATION_PER_ACTION * n as u32, "knight")
        }
    };
    {
        let mut bal = st.balance.lock().unwrap();
        if *bal < i64::from(credits) {
            return err(StatusCode::PAYMENT_REQUIRED, "Insufficient credits");
        }
        *bal -= i64::from(credits);
    }
    let outcome = if text.contains("[fail]") {
        "fail"
    } else if text.contains("[never]") {
        "never"
    } else {
        "ok"
    };
    let id = uuid::Uuid::new_v4().to_string();
    st.tasks.lock().unwrap().insert(
        id.clone(),
        MockTask {
            kind,
            family,
            created_at: MockState::now_ms(),
            polls: 0,
            body: b,
            outcome,
            credits,
        },
    );
    (StatusCode::ACCEPTED, Json(json!({"result": id}))).into_response()
}

async fn get_task(
    State(st): State<Arc<MockState>>,
    Path((ver, name, id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let path = format!("{ver}/{name}");
    st.requests
        .lock()
        .unwrap()
        .push(json!({"method": "GET", "path": format!("/openapi/{path}/{id}")}));
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "Invalid API key");
    }
    if path == "v1/animations" && id == "library" {
        return library(Query(HashMap::new())).await;
    }
    let Some(kind) = kind_of(&path) else {
        return err(StatusCode::NOT_FOUND, "Not found");
    };
    let task = {
        let mut tasks = st.tasks.lock().unwrap();
        match tasks.get_mut(&id) {
            Some(t) if t.kind == kind => {
                t.polls += 1;
                // A FAILED task refunds its credits.
                let finish = st
                    .cfg
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|c| c.polls_to_finish)
                    .unwrap_or(3);
                if t.outcome == "fail" && t.polls == finish {
                    *st.balance.lock().unwrap() += i64::from(t.credits);
                }
                t.clone()
            }
            _ => return err(StatusCode::NOT_FOUND, "Task not found"),
        }
    };
    // Show the state *before* this poll, so the first GET is PENDING.
    let shown = MockTask {
        polls: task.polls - 1,
        ..task
    };
    let v = st.task_json(&id, &shown);
    let ra = st
        .cfg
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| c.retry_after)
        .unwrap_or(0);
    let mut resp = Json(v.clone()).into_response();
    if ra > 0
        && !matches!(
            v["status"].as_str(),
            Some("SUCCEEDED" | "FAILED" | "CANCELED")
        )
    {
        resp.headers_mut()
            .insert("retry-after", ra.to_string().parse().expect("header"));
    }
    resp
}

async fn list_tasks(
    State(st): State<Arc<MockState>>,
    Path((ver, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "Invalid API key");
    }
    let path = format!("{ver}/{name}");
    if path == "v1/balance" {
        let b = *st.balance.lock().unwrap();
        return Json(json!({"balance": b, "monthly_credits": b, "permanent_credits": 0, "monthly_resets_at": null})).into_response();
    }
    let Some(kind) = kind_of(&path) else {
        return err(StatusCode::NOT_FOUND, "Not found");
    };
    let tasks = st.tasks.lock().unwrap().clone();
    let mut out: Vec<(i64, Value)> = tasks
        .iter()
        .filter(|(_, t)| t.kind == kind)
        .map(|(id, t)| (t.created_at, st.task_json(id, t)))
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    Json(out.into_iter().map(|(_, v)| v).collect::<Vec<_>>()).into_response()
}

async fn library(Query(q): Query<HashMap<String, String>>) -> Response {
    let search = q
        .get("search")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let items: Vec<Value> = crate::anim::library()
        .iter()
        .filter(|c| search.is_empty() || c.name.to_lowercase().contains(&search) || c.key.to_lowercase().contains(&search))
        .map(|c| {
            let (cat, sub) = c.category.split_once('/').unwrap_or((&c.category, ""));
            json!({"action_id": c.id, "name": c.name, "key": c.key, "category": cat, "sub_category": sub,
                   "preview_url": format!("https://cdn.meshy.ai/mock/{}.gif", c.key)})
        })
        .collect();
    Json(items).into_response()
}

/// A 1×1 PNG when a fixture thumbnail is missing.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

async fn asset(State(st): State<Arc<MockState>>, Path(file): Path<String>) -> Response {
    if !file
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || file.contains("..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = st
        .cfg
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| c.assets.clone())
        .unwrap_or_default();
    match std::fs::read(dir.join(&file)) {
        Ok(bytes) => bytes.into_response(),
        Err(_) if file.starts_with("thumb_") => TINY_PNG.to_vec().into_response(),
        // Formats the fixtures lack: placeholder bytes, like a real file to store.
        Err(_) if file.contains('.') && !file.ends_with(".glb") => {
            format!("mock {file}\n").into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn requests(State(st): State<Arc<MockState>>) -> Json<Value> {
    Json(json!(st.requests.lock().unwrap().clone()))
}

pub fn router(state: Arc<MockState>) -> Router {
    Router::new()
        .route("/openapi/{ver}/{name}", post(create).get(list_tasks))
        .route("/openapi/{ver}/{name}/{id}", get(get_task))
        .route("/openapi/v1/animations/library", get(library))
        .route("/assets/{file}", get(asset))
        .route("/_mock/requests", get(requests))
        .with_state(state)
}

/// Start the mock; the handle has its base URL.
pub async fn spawn(cfg: MockConfig) -> std::io::Result<MockHandle> {
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(MockState::default());
    let base_url = format!("http://{addr}");
    *state.base.lock().unwrap() = base_url.clone();
    *state.balance.lock().unwrap() = cfg.balance;
    *state.cfg.lock().unwrap() = Some(cfg);
    let app = router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(MockHandle { base_url, state })
}

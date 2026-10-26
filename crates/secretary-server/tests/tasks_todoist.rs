//! Tasks and the Todoist sync end to end: the real router against a mock
//! Todoist API v1 (`/api/v1/sync` with sync tokens, commands, temp ids and
//! `sync_status`; completed tasks; user; OAuth).
//!
//! Covers: connecting with a personal token (rejected and accepted), the
//! token sealed at rest and never returned, the full import then
//! incremental syncs, local edits pushed (quick add with a recurring
//! phrase, temp id → real id), recurring completion advanced by Todoist,
//! a conflict (Todoist wins, with a note), a rejected command, 429 with
//! Retry-After, a revoked token (Links shows reconnect, a new token fixes
//! it), OAuth connect with refresh, Lucina's service token, signals and
//! disconnect.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Client;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::todoist::TodoistOAuth;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const TOKEN: &str = "0123456789abcdef0123456789abcdef01234567";

fn today() -> chrono::NaiveDate {
    Utc::now()
        .with_timezone(&chrono_tz::Europe::Copenhagen)
        .date_naive()
}
fn day(n: i64) -> String {
    (today() + Duration::days(n)).format("%Y-%m-%d").to_string()
}

#[derive(Default)]
struct M {
    version: u64,
    /// id → (item, version when last changed)
    items: BTreeMap<String, (Value, u64)>,
    processed: HashSet<String>,
    next_id: u64,
    revoked: bool,
    rate_limit_once: bool,
    reject_next_update: bool,
    /// sync_token of every /sync call
    tokens: Vec<String>,
    commands: Vec<Value>,
    access: String,
    refresh: String,
    refreshes: u32,
    revokes: Vec<String>,
    completed: Vec<Value>,
}

type Mock = Arc<Mutex<M>>;

fn stamp(v: u64) -> String {
    format!("2026-10-22T08:{:02}:{:02}Z", (v / 60) % 60, v % 60)
}

impl M {
    fn put(&mut self, mut item: Value) {
        self.version += 1;
        item["updated_at"] = json!(stamp(self.version));
        let id = item["id"].as_str().unwrap().to_owned();
        self.items.insert(id, (item, self.version));
    }
    fn get(&self, id: &str) -> Option<Value> {
        self.items.get(id).map(|(v, _)| v.clone())
    }
    fn seed(&mut self) {
        let base = |id: &str, content: &str, due: Value| {
            json!({"id": id, "content": content, "description": "", "project_id": "inbox1",
                   "section_id": null, "parent_id": null, "priority": 1, "due": due, "labels": ["lingue"],
                   "child_order": 1, "day_order": -1, "checked": false, "is_deleted": false,
                   "added_at": "2026-10-01T08:00:00Z", "completed_at": null})
        };
        self.put(base(
            "a1",
            "15 min shadowing",
            json!({"date": day(0), "string": "every day", "lang": "en", "is_recurring": true}),
        ));
        self.put(base(
            "a2",
            "10 min anki/memrise",
            json!({"date": day(-2), "string": "every day", "lang": "en", "is_recurring": true}),
        ));
        self.put(base(
            "a3",
            "20min speaking",
            json!({"date": day(1), "string": "tomorrow", "lang": "en", "is_recurring": false}),
        ));
        self.put(base("a4", "Brain Training", Value::Null));
    }
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned()
}

fn check(m: &M, h: &HeaderMap) -> Option<Response> {
    let b = bearer(h);
    if m.revoked || (b != TOKEN && b != m.access) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Unauthorized", "error_code": 401})),
            )
                .into_response(),
        );
    }
    None
}

async fn sync(State(m): State<Mock>, h: HeaderMap, body: String) -> Response {
    let mut g = m.lock().unwrap();
    if let Some(r) = check(&g, &h) {
        return r;
    }
    if g.rate_limit_once {
        g.rate_limit_once = false;
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1")],
            Json(json!({"error": "Too many requests", "error_tag": "TOO_MANY_REQUESTS", "http_code": 429})),
        )
            .into_response();
    }
    let f = form(&body);
    assert_eq!(f["resource_types"], secretary_server::todoist::RESOURCES);
    let token = f["sync_token"].clone();
    g.tokens.push(token.clone());
    let since: u64 = if token == "*" {
        0
    } else {
        token.trim_start_matches('s').parse().expect("our token")
    };
    let mut status = serde_json::Map::new();
    let mut mapping = serde_json::Map::new();
    if let Some(cmds) = f.get("commands") {
        let cmds: Vec<Value> = serde_json::from_str(cmds).unwrap();
        for c in cmds {
            g.commands.push(c.clone());
            let uuid = c["uuid"].as_str().unwrap().to_owned();
            if !g.processed.insert(uuid.clone()) {
                status.insert(uuid, json!("ok"));
                continue;
            }
            let args = &c["args"];
            let resolve = |g: &M, mapping: &serde_json::Map<String, Value>, id: &str| -> String {
                mapping
                    .get(id)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        let _ = g;
                        id.to_owned()
                    })
            };
            match c["type"].as_str().unwrap() {
                "item_add" => {
                    g.next_id += 1;
                    let id = format!("n{}", g.next_id);
                    let mut due = args.get("due").cloned().unwrap_or(Value::Null);
                    if let Some(s) = due.get("string").and_then(Value::as_str) {
                        // Todoist parses the phrase; the mock knows two
                        let (date, rec) = match s {
                            "domani alle 9" => (format!("{}T09:00:00", day(1)), false),
                            "ogni lunedì" => (day(3), true),
                            _ => (day(0), s.starts_with("every") || s.starts_with("ogni")),
                        };
                        due = json!({"date": date, "string": s, "lang": due["lang"], "is_recurring": rec});
                    }
                    let item = json!({"id": id, "content": args["content"], "description": args.get("description").cloned().unwrap_or(json!("")),
                        "project_id": args.get("project_id").cloned().unwrap_or(json!("inbox1")), "section_id": args.get("section_id").cloned().unwrap_or(Value::Null),
                        "parent_id": args.get("parent_id").cloned().unwrap_or(Value::Null), "priority": args.get("priority").cloned().unwrap_or(json!(1)),
                        "due": due, "labels": args.get("labels").cloned().unwrap_or(json!([])), "child_order": 9, "day_order": -1,
                        "checked": false, "is_deleted": false, "added_at": "2026-10-22T08:00:00Z", "completed_at": null});
                    g.put(item);
                    mapping.insert(c["temp_id"].as_str().unwrap().to_owned(), json!(id));
                    status.insert(uuid, json!("ok"));
                }
                "item_update" => {
                    if g.reject_next_update {
                        g.reject_next_update = false;
                        status.insert(uuid, json!({"error_tag": "INVALID_ARGUMENT_VALUE", "error_code": 20, "error": "Invalid argument value", "http_code": 400}));
                        continue;
                    }
                    let id = resolve(&g, &mapping, args["id"].as_str().unwrap());
                    let Some(mut item) = g.get(&id) else {
                        status.insert(uuid, json!({"error_tag": "ITEM_NOT_FOUND", "error": "Item not found", "http_code": 400}));
                        continue;
                    };
                    for (k, v) in args.as_object().unwrap() {
                        if k == "due" && !v.is_null() {
                            let rec = v
                                .get("is_recurring")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let string = v.get("string").cloned().unwrap_or(v["date"].clone());
                            item["due"] = json!({"date": v["date"], "string": string, "lang": "en", "is_recurring": rec});
                        } else if k != "id" {
                            item[k] = v.clone();
                        }
                    }
                    g.put(item);
                    status.insert(uuid, json!("ok"));
                }
                "item_close" => {
                    let id = resolve(&g, &mapping, args["id"].as_str().unwrap());
                    let mut item = g.get(&id).unwrap();
                    let recurring = item["due"]["is_recurring"].as_bool().unwrap_or(false);
                    g.completed.push(json!({"id": id, "content": item["content"], "project_id": "inbox1",
                        "completed_at": Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(), "due": item["due"]}));
                    if recurring {
                        item["due"]["date"] = json!(day(1));
                    } else {
                        item["checked"] = json!(true);
                        item["completed_at"] = json!(Utc::now().to_rfc3339());
                    }
                    g.put(item);
                    status.insert(uuid, json!("ok"));
                }
                "item_delete" => {
                    let id = resolve(&g, &mapping, args["id"].as_str().unwrap());
                    let mut item = g.get(&id).unwrap();
                    item["is_deleted"] = json!(true);
                    g.put(item);
                    status.insert(uuid, json!("ok"));
                }
                other => {
                    status.insert(uuid, json!("ok"));
                    let _ = other;
                }
            }
        }
    }
    let full = since == 0;
    let items: Vec<Value> = g
        .items
        .values()
        .filter(|(it, v)| {
            if full {
                !it["checked"].as_bool().unwrap() && !it["is_deleted"].as_bool().unwrap()
            } else {
                *v > since
            }
        })
        .map(|(it, _)| it.clone())
        .collect();
    let mut out = json!({
        "sync_token": format!("s{}", g.version),
        "full_sync": full,
        "items": items,
        "user": {"email": "owner@example.com", "daily_goal": 5, "lang": "it", "next_week": 1, "start_day": 1, "tz_info": {"timezone": "Europe/Copenhagen"}},
        "stats": {"days_items": [{"date": day(0), "total_completed": g.completed.len()}]},
    });
    if full {
        out["projects"] = json!([
            {"id": "inbox1", "name": "Inbox", "inbox_project": true, "child_order": 0},
            {"id": "lang", "name": "Lingue", "child_order": 1, "color": "blue"}
        ]);
        out["sections"] = json!([]);
        out["labels"] = json!([{"id": "l1", "name": "lingue", "item_order": 1}]);
        out["filters"] = json!([]);
    }
    if !status.is_empty() {
        out["sync_status"] = Value::Object(status);
        out["temp_id_mapping"] = Value::Object(mapping);
    }
    Json(out).into_response()
}

async fn user(State(m): State<Mock>, h: HeaderMap) -> Response {
    let g = m.lock().unwrap();
    if let Some(r) = check(&g, &h) {
        return r;
    }
    Json(json!({"id": "777", "email": "owner@example.com", "full_name": "Owner", "daily_goal": 5}))
        .into_response()
}

async fn completed(State(m): State<Mock>, h: HeaderMap) -> Response {
    let g = m.lock().unwrap();
    if let Some(r) = check(&g, &h) {
        return r;
    }
    Json(json!({"items": g.completed, "next_cursor": null})).into_response()
}

async fn oauth_token(State(m): State<Mock>, body: String) -> Response {
    let f = form(&body);
    let mut g = m.lock().unwrap();
    assert_eq!(f["client_id"], "todo-client");
    assert_eq!(f["client_secret"], "todo-secret");
    if f.get("grant_type").map(String::as_str) == Some("refresh_token") {
        if f["refresh_token"] != g.refresh {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_grant"})),
            )
                .into_response();
        }
        g.refreshes += 1;
        g.access = format!("oauth-access-{}-0123456789abcdef", g.refreshes);
        g.refresh = format!("oauth-refresh-{}", g.refreshes);
        return Json(json!({"access_token": g.access, "token_type": "Bearer", "expires_in": 3600, "refresh_token": g.refresh})).into_response();
    }
    if f["code"] != "good-code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "bad_authorization_code"})),
        )
            .into_response();
    }
    assert_eq!(
        f["redirect_uri"],
        "https://example.test/v1/links/todoist/callback"
    );
    g.access = "oauth-access-0-0123456789abcdef".into();
    g.refresh = "oauth-refresh-0".into();
    // expires in 60 s: the next sync refreshes first (two-minute margin)
    Json(json!({"access_token": g.access, "token_type": "Bearer", "expires_in": 60, "refresh_token": g.refresh, "scope": "data:read_write,data:delete"})).into_response()
}

async fn revoke(State(m): State<Mock>, h: HeaderMap, body: String) -> StatusCode {
    assert!(h
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("Basic "));
    m.lock().unwrap().revokes.push(form(&body)["token"].clone());
    StatusCode::OK
}

async fn spawn(r: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    base
}

struct T {
    base: String,
    http: Client,
    token: String,
    svc: String,
    mock: Mock,
    data: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl T {
    async fn new() -> Self {
        let mock: Mock = Arc::default();
        mock.lock().unwrap().seed();
        let base = spawn(
            Router::new()
                .route("/api/v1/sync", post(sync))
                .route("/api/v1/user", get(user))
                .route("/api/v1/tasks/completed/by_completion_date", get(completed))
                .route("/api/v1/revoke", post(revoke))
                .route("/oauth/access_token", post(oauth_token))
                .with_state(mock.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.todoist.api_base = base.clone();
        cfg.todoist.auth_base = base;
        cfg.todoist.sync_every = std::time::Duration::ZERO; // syncs on demand only
        cfg.todoist.oauth = Some(TodoistOAuth {
            client_id: "todo-client".into(),
            client_secret: "todo-secret".into(),
            redirect_uri: "https://example.test/v1/links/todoist/callback".into(),
        });
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let svc = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let state = AppState::new(&cfg).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            serve_on(l, app(state), std::future::pending())
                .await
                .unwrap()
        });
        Self {
            base,
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token,
            svc,
            mock,
            data: cfg.data_dir.clone(),
            _dir: dir,
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> (StatusCode, Value, HeaderMap) {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        rb = rb.bearer_auth(bearer.unwrap_or(&self.token));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let s = StatusCode::from_u16(r.status().as_u16()).unwrap();
        let mut h = HeaderMap::new();
        for (k, v) in r.headers() {
            if let (Ok(k), Ok(v)) = (
                axum::http::HeaderName::from_bytes(k.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(v.as_bytes()),
            ) {
                h.insert(k, v);
            }
        }
        (s, r.json().await.unwrap_or(Value::Null), h)
    }
    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (s, v, _) = self.call(reqwest::Method::GET, path, None, None).await;
        (s, v)
    }
    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, v, _) = self
            .call(reqwest::Method::POST, path, Some(body), None)
            .await;
        (s, v)
    }
    async fn cmd(&self, op: &str, command: Value) -> Value {
        let (s, v) = self
            .post(
                "/v1/tasks/command",
                json!({"op_id": op, "command": command}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }
    async fn sync(&self) -> (StatusCode, Value) {
        self.post("/v1/tasks/sync", json!({})).await
    }
    async fn view(&self, q: &str) -> Value {
        let (s, v) = self.get(&format!("/v1/tasks/state?{q}")).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["view"].clone()
    }
    async fn card(&self) -> Value {
        let (s, links) = self.get("/v1/links").await;
        assert_eq!(s, StatusCode::OK);
        links["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == "tasks")
            .unwrap()["cards"][0]
            .clone()
    }
    async fn connect_token(&self) {
        let (s, v) = self
            .post("/v1/links/todoist/token", json!({"token": TOKEN}))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["account"], "owner@example.com");
        let (s, v) = self.sync().await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
}

fn contents(view: &Value, group: &str) -> Vec<String> {
    view["list"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == group)
        .map(|g| {
            g["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["content"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn connect_full_then_incremental_sync() {
    let t = T::new().await;
    let card = t.card().await;
    assert_eq!(card["status"], "disconnected");
    assert_eq!(card["actions"], json!(["connect", "token"]));
    let (s, _) = t.sync().await;
    assert_eq!(s, StatusCode::CONFLICT, "not connected yet");
    let (s, v) = t
        .post("/v1/links/todoist/token", json!({"token": "x".repeat(40)}))
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("todoist_token_rejected")
        )
    );
    t.connect_token().await;
    // sealed at rest, never returned
    let raw = std::fs::read_to_string(t.data.join("links.json")).unwrap();
    assert!(!raw.contains(TOKEN) && raw.contains("enc1:"));
    let (_, links) = t.get("/v1/links").await;
    assert!(!links.to_string().contains(TOKEN));
    let card = t.card().await;
    assert_eq!(card["status"], "connected");
    assert_eq!(card["account"], "owner@example.com");
    assert_eq!(card["count"], 4);

    let v = t.view("view=today").await;
    assert_eq!(contents(&v, "overdue"), vec!["10 min anki/memrise"]);
    assert_eq!(contents(&v, &day(0)), vec!["15 min shadowing"]);
    assert_eq!(v["counts"]["inbox"], 4);
    assert_eq!(v["sync"]["status"], "ok");
    assert_eq!(v["projects"][1]["name"], "Lingue");

    // a change in the Todoist app arrives with the next incremental sync
    {
        let mut g = t.mock.lock().unwrap();
        let mut it = g.get("a4").unwrap();
        it["content"] = json!("Brain Training (Lumosity)");
        g.put(it);
    }
    let (s, r) = t.sync().await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["report"]["full"], false);
    let v = t.view("view=inbox").await;
    assert!(contents(&v, "none").contains(&"Brain Training (Lumosity)".to_string()));
    let tokens = t.mock.lock().unwrap().tokens.clone();
    assert_eq!(tokens.iter().filter(|x| *x == "*").count(), 1, "{tokens:?}");
    assert!(tokens.last().unwrap().starts_with('s'));
}

#[tokio::test]
async fn local_edits_push_with_temp_ids_and_recurring_completion() {
    let t = T::new().await;
    t.connect_token().await;
    let r = t
        .cmd(
            "q1",
            json!({"type": "quick_add", "text": "Ripasso verbi domani alle 9 #Lingue @lingue p2"}),
        )
        .await;
    let tmp = r["task_id"].as_str().unwrap().to_owned();
    assert!(tmp.starts_with("tmp-"));
    assert_eq!(r["queued"], true);
    t.cmd(
        "q2",
        json!({"type": "add", "content": "Grammatica", "due_string": "ogni lunedì"}),
    )
    .await;
    t.cmd("c1", json!({"type": "complete", "task_id": "a1"}))
        .await;
    let (s, rep) = t.sync().await;
    assert_eq!(s, StatusCode::OK, "{rep}");
    assert_eq!(rep["report"]["pushed"], 3);
    let cmds = t.mock.lock().unwrap().commands.clone();
    assert_eq!(cmds[0]["type"], "item_add");
    assert_eq!(
        cmds[0]["args"]["due"],
        json!({"string": "domani alle 9", "lang": "it"})
    );
    assert_eq!(cmds[0]["args"]["project_id"], "lang");
    assert_eq!(cmds[0]["args"]["priority"], 3);
    assert_eq!(
        cmds[1]["args"]["due"],
        json!({"string": "ogni lunedì", "lang": "it"})
    );
    assert_eq!(cmds[2]["type"], "item_close");
    let v = t.view("view=project&id=lang").await;
    let task = &v["list"]["groups"][0]["tasks"][0];
    assert_eq!(task["content"], "Ripasso verbi");
    assert!(task["id"].as_str().unwrap().starts_with('n'), "{task}");
    assert_eq!(task["due"]["time"], "09:00");
    assert_eq!(task["pending"], false);
    // Todoist advanced the routine to tomorrow; the guess is replaced
    let v = t.view("view=upcoming").await;
    assert!(contents(&v, &day(1)).contains(&"15 min shadowing".to_string()));
    let shadow = v["list"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["tasks"].as_array().unwrap().clone())
        .find(|x| x["id"] == "a1")
        .unwrap();
    assert_eq!(shadow["due"]["is_recurring"], true);
    assert!(shadow["due"].get("provisional").is_none());
    let g = t.view("view=completed").await;
    assert_eq!(g["goal"]["completed_today"], 1);
    assert_eq!(contents(&g, &day(0)), vec!["15 min shadowing"]);
    // a recurring task is still recurring in Todoist after the push
    assert_eq!(
        t.mock.lock().unwrap().get("a1").unwrap()["due"]["string"],
        "every day"
    );
}

#[tokio::test]
async fn conflict_rejection_rate_limit_and_revocation() {
    let t = T::new().await;
    t.connect_token().await;
    // conflict: edited here, changed in Todoist before the push
    t.cmd(
        "u1",
        json!({"type": "update", "task_id": "a3", "content": "20 min speaking (here)"}),
    )
    .await;
    {
        let mut g = t.mock.lock().unwrap();
        let mut it = g.get("a3").unwrap();
        it["content"] = json!("20 min speaking with Marco");
        g.put(it);
    }
    let (s, r) = t.sync().await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["report"]["conflicts"], 1);
    let v = t.view("view=upcoming").await;
    assert_eq!(contents(&v, &day(1)), vec!["20 min speaking with Marco"]);
    assert_eq!(v["conflicts"][0]["kind"], "changed_remotely");
    assert!(
        t.mock.lock().unwrap().commands.is_empty(),
        "the stale edit was not pushed"
    );

    // a rejected command: dropped with a note, then a full resync
    t.mock.lock().unwrap().reject_next_update = true;
    t.cmd(
        "u2",
        json!({"type": "update", "task_id": "a4", "content": "x"}),
    )
    .await;
    let (_, r) = t.sync().await;
    assert_eq!(r["report"]["rejected"], 1);
    let (s, _) = t.sync().await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.mock.lock().unwrap().tokens.last().unwrap(), "*");
    let v = t.view("view=inbox").await;
    assert!(contents(&v, "none").contains(&"Brain Training".to_string()));

    // 429 with Retry-After: the sync waits
    t.mock.lock().unwrap().rate_limit_once = true;
    let (s, _, h) = t
        .call(
            reqwest::Method::POST,
            "/v1/tasks/sync",
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(h.get("retry-after").is_some());
    let v = t.view("view=today").await;
    assert_eq!(v["sync"]["status"], "rate_limited");
    let (s, _) = t.sync().await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "still inside the wait");
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (s, _) = t.sync().await;
    assert_eq!(s, StatusCode::OK);

    // revoked token: Links shows reconnect; edits keep queueing
    t.mock.lock().unwrap().revoked = true;
    let (s, v) = t.sync().await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("todoist_reconnect"))
    );
    let card = t.card().await;
    assert_eq!(card["status"], "reconnect");
    assert_eq!(card["actions"], json!(["reconnect", "token", "disconnect"]));
    assert_eq!(t.view("view=today").await["sync"]["status"], "reconnect");
    t.cmd("c9", json!({"type": "complete", "task_id": "a4"}))
        .await;
    // a new token fixes it and the queued completion goes out
    t.mock.lock().unwrap().revoked = false;
    t.connect_token().await;
    assert_eq!(t.card().await["status"], "connected");
    assert_eq!(t.mock.lock().unwrap().get("a4").unwrap()["checked"], true);
}

#[tokio::test]
async fn oauth_connect_refresh_service_token_signals_and_disconnect() {
    let t = T::new().await;
    let (s, v) = t.post("/v1/links/todoist/connect", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let url = reqwest::Url::parse(v["authorizeUrl"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(url.path(), "/oauth/authorize");
    assert_eq!(q["client_id"], "todo-client");
    assert_eq!(q["scope"], "data:read_write,data:delete");
    let r = t
        .http
        .get(format!(
            "{}/v1/links/todoist/callback?code=good-code&state={}",
            t.base, q["state"]
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 303);
    assert_eq!(r.headers()["location"], "/links?todoist=connected");
    // the state is single use
    let r = t
        .http
        .get(format!(
            "{}/v1/links/todoist/callback?code=good-code&state={}",
            t.base, q["state"]
        ))
        .send()
        .await
        .unwrap();
    assert!(r.headers()["location"]
        .to_str()
        .unwrap()
        .contains("todoist=error"));
    let (s, v) = t.sync().await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        t.mock.lock().unwrap().refreshes >= 1,
        "the 60 s token was refreshed"
    );
    assert_eq!(t.card().await["method"], "oauth");

    // Lucina's service token: read, add, signals
    let (s, v, _) = t
        .call(
            reqwest::Method::GET,
            "/v1/tasks/state?view=today",
            None,
            Some(&t.svc),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["view"]["counts"]["overdue"], 1);
    let (s, v, _) = t
        .call(
            reqwest::Method::POST,
            "/v1/tasks/command",
            Some(json!({"op_id": "lucina-1", "command": {"type": "quick_add", "text": "chiamare Marco oggi"}})),
            Some(&t.svc),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, sig, _) = t
        .call(
            reqwest::Method::GET,
            "/v1/tasks/signals",
            None,
            Some(&t.svc),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sig["overdue"][0]["content"], "10 min anki/memrise");
    assert_eq!(sig["overdue"][0]["days_overdue"], 2);
    assert_eq!(sig["today_open"], 2);
    assert_eq!(sig["load_minutes"], 25);
    assert!(sig["routines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["content"] == "15 min shadowing"));
    let (s, ins, _) = t
        .call(
            reqwest::Method::GET,
            "/v1/insights/summary",
            None,
            Some(&t.svc),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ins["tasks"]["overdue_open"], 1);
    // the service token cannot manage the link
    let (s, _, _) = t
        .call(
            reqwest::Method::DELETE,
            "/v1/links/todoist",
            None,
            Some(&t.svc),
        )
        .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED,
        "{s}"
    );

    // bad requests
    let (s, _) = t
        .post(
            "/v1/tasks/command",
            json!({"op_id": "b1", "command": {"type": "complete", "task_id": "nope"}}),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = t
        .call(reqwest::Method::GET, "/v1/tasks/state", None, Some("bogus"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // disconnect: revoked at Todoist, synced data dropped, local-only stays
    let (s, v, _) = t
        .call(reqwest::Method::DELETE, "/v1/links/todoist", None, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revokedAtTodoist"], true);
    assert_eq!(t.card().await["status"], "disconnected");
    let v = t.view("view=inbox").await;
    assert_eq!(v["sync"]["status"], "disconnected");
    assert_eq!(v["projects"], json!([]));
}

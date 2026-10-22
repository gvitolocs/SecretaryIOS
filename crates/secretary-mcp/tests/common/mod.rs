//! Test harness: a mock secretary-server and a running secretary-mcp.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub const DEVICE_TOKEN: &str = "sec_test_device_token";
pub const SERVICE_TOKEN: &str = "svc_test_service_token";
pub const OWNER_EMAIL: &str = "owner@example.com";
pub const OWNER_PASSWORD: &str = "correct horse battery";

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Default)]
pub struct MockState {
    pub log: Vec<Recorded>,
    pub v2: bool,
    pub items: Vec<Value>,
    pub legacy: Vec<Value>,
    pub revision: u64,
}

#[derive(Clone)]
pub struct Mock {
    pub state: Arc<Mutex<MockState>>,
    pub url: String,
}

impl Mock {
    pub fn requests(&self, path: &str) -> Vec<Recorded> {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }
    pub fn set_v2(&self, on: bool) {
        self.state.lock().unwrap().v2 = on;
    }
}

fn err(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": code}})),
    )
        .into_response()
}

fn device_ok(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {DEVICE_TOKEN}"))
}

async fn handle(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or("").to_owned();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.state.lock().unwrap().log.push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        headers: headers.clone(),
        body: body.clone(),
    });
    let mut st = m.state.lock().unwrap();
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), seg.as_slice()) {
        ("GET", ["v1", "health"]) => {
            Json(json!({"ok": true, "version": "0.1.0", "lucina": {"reachable": true, "latencyMs": 12, "apiVersion": 1}}))
                .into_response()
        }
        ("POST", ["v1", "session"]) => {
            if body["email"] == OWNER_EMAIL && body["password"] == OWNER_PASSWORD {
                (
                    [("set-cookie", "sec_session=sess123; HttpOnly; Secure; SameSite=Strict; Path=/")],
                    Json(json!({"authenticated": true, "deviceName": "web"})),
                )
                    .into_response()
            } else {
                err(StatusCode::UNAUTHORIZED, "invalid_credentials")
            }
        }
        ("DELETE", ["v1", "session"]) => Json(json!({"authenticated": false, "deviceName": null})).into_response(),
        ("GET", ["v1", "insights", "summary"]) => {
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {SERVICE_TOKEN}")) {
                return err(StatusCode::UNAUTHORIZED, "unauthorized");
            }
            Json(json!({"generated_ms": 1, "timezone": "Europe/Rome", "gym": {
                "unit": "kg", "weekly_goal": 3, "workouts_this_week": 2,
                "recent_workouts": [{"id": "w1", "title": "Push", "date": "2026-10-21", "duration_min": 55,
                    "completed_sets": 18, "volume_label": "4,120 kg", "exercises": ["Bench Press", "Dips"],
                    "prs": [{"exercise_id": "bench-press", "name": "Bench Press", "label": "82.5 kg × 5"}]}],
                "weekly_volume": [{"week_start": "2026-10-19", "workouts": 2, "volume_label": "8,000 kg"}]},
                "body": {}}))
            .into_response()
        }
        _ if !device_ok(&headers) => err(StatusCode::UNAUTHORIZED, "unauthorized"),
        ("GET", ["v1", "session"]) => Json(json!({"authenticated": true, "deviceName": "lucina-mcp"})).into_response(),
        ("POST", ["v1", "chat"]) => {
            let msg = body["message"].as_str().unwrap_or("");
            Json(json!({"reply": format!("Ciao! You said: {msg}"), "action": "reply", "job": null,
                "sessionId": format!("secretary_{}", body["sessionId"].as_str().unwrap_or("app")),
                "clientMessageId": body["clientMessageId"], "userItemId": "tl_u", "lucinaItemId": "tl_l"}))
            .into_response()
        }
        ("GET", ["v1", "timeline"]) => Json(json!({"items": [
            {"id": "tl_3", "ts": 1_792_602_906_000_i64, "channel": "telegram", "role": "lucina", "text": "Fatto, latte aggiunto."},
            {"id": "tl_2", "ts": 1_792_602_900_000_i64, "channel": "telegram", "role": "user", "text": "Aggiungi il latte"},
            {"id": "tl_1", "ts": 1_792_602_000_000_i64, "channel": "web", "role": "user", "text": "Ciao"}
        ], "nextBefore": "tl_1"}))
        .into_response(),
        ("GET", ["v1", "shopping", "lists"]) => {
            if !st.v2 {
                return err(StatusCode::NOT_FOUND, "not_found");
            }
            Json(json!([{"id": "default", "name": "Casa", "theme": "home", "needed": 2, "recent": 1},
                        {"id": "work", "name": "Ufficio", "theme": "work", "needed": 0, "recent": 0}]))
            .into_response()
        }
        ("GET", ["v1", "shopping", "lists", id]) => {
            if !st.v2 {
                return err(StatusCode::BAD_GATEWAY, "lucina_error");
            }
            if *id != "default" && *id != "work" {
                return err(StatusCode::NOT_FOUND, "list_not_found");
            }
            let items = if *id == "default" { st.items.clone() } else { vec![] };
            Json(json!({"list": {"id": id, "name": if *id == "default" {"Casa"} else {"Ufficio"}, "theme": "home"},
                "revision": st.revision, "items": items}))
            .into_response()
        }
        ("POST", ["v1", "shopping", "lists", _id, "ops"]) => {
            if !st.v2 {
                return err(StatusCode::BAD_GATEWAY, "lucina_error");
            }
            let mut applied = vec![];
            for op in body["ops"].as_array().cloned().unwrap_or_default() {
                applied.push(op["opId"].clone());
                match op["type"].as_str() {
                    Some("add") => {
                        let n = st.items.len();
                        st.items.push(json!({"id": format!("item{n}"), "name": op["name"], "spec": op.get("spec").cloned().unwrap_or(Value::Null), "status": "needed"}));
                    }
                    Some("purchase") => {
                        for it in st.items.iter_mut() {
                            if it["id"] == op["itemId"] {
                                it["status"] = json!("recent");
                            }
                        }
                    }
                    _ => {}
                }
            }
            st.revision += 1;
            Json(json!({"revision": st.revision, "applied": applied, "duplicates": [], "items": st.items})).into_response()
        }
        ("GET", ["v1", "shopping"]) => Json(Value::Array(st.legacy.clone())).into_response(),
        ("POST", ["v1", "shopping"]) => {
            let n = st.legacy.len();
            let item = json!({"id": format!("l{n}"), "rawName": body["rawName"], "product": body["rawName"], "status": "needed"});
            st.legacy.push(item.clone());
            (StatusCode::CREATED, Json(item)).into_response()
        }
        ("PATCH", ["v1", "shopping", id]) => {
            for it in st.legacy.iter_mut() {
                if it["id"] == *id {
                    it["status"] = body["status"].clone();
                }
            }
            Json(json!({"id": id, "product": "x", "status": body["status"]})).into_response()
        }
        ("GET", ["v1", "reminders"]) => Json(json!({"scope": "upcoming", "includeSent": false, "timeZone": "Europe/Rome",
            "reminders": [{"id": "r1", "text": "Call the dentist", "dueAt": "2026-10-22T07:30:00.000Z", "timeZone": "Europe/Rome",
                           "status": "pending", "source": "secretary"}]}))
        .into_response(),
        ("POST", ["v1", "reminders"]) => {
            if body.get("naturalText").is_some_and(|t| t.as_str().unwrap_or("").contains("never")) {
                return err(StatusCode::BAD_REQUEST, "unparseable_reminder");
            }
            let text = body.get("text").or(body.get("naturalText")).cloned().unwrap_or(Value::Null);
            (StatusCode::CREATED, Json(json!({"created": true, "reminder": {"id": "r2", "text": text,
                "dueAt": body.get("dueAt").cloned().unwrap_or(json!("2026-10-22T07:30:00.000Z")), "timeZone": "Europe/Rome", "status": "pending"}})))
            .into_response()
        }
        ("DELETE", ["v1", "reminders", id]) => {
            if *id != "r1" {
                return err(StatusCode::NOT_FOUND, "reminder_not_found");
            }
            Json(json!({"cancelled": true, "reminder": {"id": "r1", "text": "Call the dentist", "status": "cancelled"}})).into_response()
        }
        ("GET", ["v1", "agenda"]) => Json(json!({"date": "2026-10-22", "timeZone": "Europe/Rome",
            "reminders": [{"id": "r1", "text": "Call the dentist", "dueAt": "2026-10-22T07:30:00.000Z", "status": "pending"}],
            "briefing": "Tomorrow starts at 9:30 with the dentist call.", "briefingCached": false}))
        .into_response(),
        ("GET", ["v1", "calendar", "events"]) => Json(json!({"from": "2026-10-21T22:00:00.000Z", "to": "2026-10-28T23:00:00.000Z",
            "timeZone": "Europe/Rome", "events": [
                {"id": "google:abc", "source": "google", "sourceId": "abc", "calendar": "Google", "title": "Dentist",
                 "start": "2026-10-22T07:30:00.000Z", "end": "2026-10-22T08:00:00.000Z", "allDay": false, "location": "Via Roma 1",
                 "startMs": 1, "endMs": 2, "recurring": false, "editable": true},
                {"id": "iphone:EK1", "source": "iphone", "sourceId": "EK1", "calendar": "Casa", "title": "Birthday",
                 "start": "2026-10-23", "end": "2026-10-24", "allDay": true, "startMs": 3, "endMs": 4, "recurring": false, "editable": false}],
            "sources": {"google": {"status": "reconnect"}}}))
        .into_response(),
        ("POST", ["v1", "calendar", "proposals"]) => {
            if body["start"] == "bad" {
                return err(StatusCode::BAD_REQUEST, "bad_request");
            }
            (StatusCode::CREATED, Json(json!({"id": "prop_1", "event": body, "from": body["from"]}))).into_response()
        }
        ("POST", ["v1", "calendar", "events"]) => panic!("the MCP must never create events directly"),
        ("GET", ["v1", "gym", "state"]) => Json(json!({"ok": true, "replayed": false, "view": {"summary": {
            "workouts": 10, "completed_sets": 200, "weekly_workouts": 1, "weekly_goal": 3, "volume": "40,000 kg"},
            "records": [{"name": "Squat", "weight": "100 kg", "reps": 5, "estimated_1rm": "116.7 kg"}], "state": {"active": null}}}))
        .into_response(),
        ("GET", ["v1", "body", "state"]) => Json(json!({"ok": true, "replayed": false, "view": {
            "stats": {"count": 2, "goal_reached": false},
            "stat_labels": {"current": "80.5 kg", "avg7": "80.6 kg", "goal": "78.0 kg", "goal_distance": "+2.6 kg"},
            "entries": [{"id": "e1", "weight_grams": 80500, "measured_ms": 1_792_602_906_883_i64, "note": "after run"}],
            "labels": {"e1": "80.5 kg"}}}))
        .into_response(),
        _ => err(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// CIMD document served by the mock (for the CIMD test).
async fn cimd(State(m): State<Mock>) -> Json<Value> {
    Json(json!({
        "client_id": format!("{}/oauth/client.json", m.url),
        "client_name": "ChatGPT",
        "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
        "token_endpoint_auth_method": "private_key_jwt",
        "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"]
    }))
}

pub async fn start_mock() -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let mock = Mock {
        state: Arc::new(Mutex::new(MockState {
            v2: true,
            items: vec![
                json!({"id": "i1", "name": "Latte", "status": "needed"}),
                json!({"id": "i2", "name": "Banane", "spec": "6, mature", "status": "needed"}),
                json!({"id": "i3", "name": "Pane", "status": "recent"}),
            ],
            legacy: vec![
                json!({"id": "l0", "rawName": "latte", "product": "milk", "status": "needed"}),
            ],
            revision: 42,
            ..Default::default()
        })),
        url: format!("http://{addr}"),
    };
    let app = Router::new()
        .route("/oauth/client.json", get(cimd))
        .fallback(any(handle))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    mock
}

pub struct Harness {
    pub mock: Mock,
    pub base: String,
    pub dir: tempfile::TempDir,
    pub ct: CancellationToken,
    pub http: reqwest::Client,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.ct.cancel();
    }
}

pub async fn start_with(f: impl FnOnce(&mut secretary_mcp::Config, &Mock)) -> Harness {
    let mock = start_mock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg =
        secretary_mcp::Config::new("https://getsecretary.tech", dir.path().join("data")).unwrap();
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    cfg.backend_url = mock.url.clone();
    cfg.backend_token = Some(DEVICE_TOKEN.into());
    cfg.service_token = Some(SERVICE_TOKEN.into());
    f(&mut cfg, &mock);
    let (addr, ct, _h) = secretary_mcp::spawn(cfg).await.unwrap();
    Harness {
        mock,
        base: format!("http://{addr}"),
        dir,
        ct,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    }
}

pub async fn start() -> Harness {
    start_with(|_, _| {}).await
}

impl Harness {
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("data")
    }

    pub fn issue(&self, name: &str, scopes: &str) -> String {
        let store = secretary_mcp::tokens::TokenStore::new(self.data_dir());
        store
            .issue(
                name,
                secretary_mcp::scope::Scopes::parse_list(scopes).unwrap(),
            )
            .unwrap()
            .1
    }

    /// One JSON-RPC request to /mcp; returns (HTTP status, JSON-RPC message or Null).
    pub async fn rpc_raw(
        &self,
        token: Option<&str>,
        method: &str,
        params: Value,
    ) -> (u16, HeaderMap, Value) {
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base))
            .header(CONTENT_TYPE, "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let ct = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let text = resp.text().await.unwrap();
        let v = if ct.contains("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                .rfind(|m| m.get("id").is_some())
                .unwrap_or(Value::Null)
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        (status, headers, v)
    }

    pub async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let (status, _, v) = self.rpc_raw(Some(token), method, params).await;
        assert_eq!(status, 200, "{method}: {v}");
        assert!(v.get("error").is_none(), "{method}: {v}");
        v["result"].clone()
    }

    /// tools/call → (is_error, text).
    pub async fn call(&self, token: &str, tool: &str, args: Value) -> (bool, String) {
        let r = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": args}),
            )
            .await;
        let text = r["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        (r["isError"].as_bool().unwrap_or(false), text)
    }

    pub async fn tool_names(&self, token: &str) -> Vec<String> {
        let r = self.rpc(token, "tools/list", json!({})).await;
        r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// Small helper for query strings in Location headers.
pub fn query_param(url: &str, key: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

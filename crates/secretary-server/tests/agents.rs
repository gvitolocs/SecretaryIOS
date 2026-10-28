//! `/v1/agents/*` against the published contract (`docs/design/agents-api.md`), end to
//! end: this server → a mock Lucina that forwards `/api/secretary/agentd/*` to a real
//! lucina-agentd router on a unix socket (as lucina-server's `agentd` module does).
//! Auth and ownership negatives, the MCP and broker limits, external agents and their
//! commands, and stream resume.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::StreamExt;
use hyper_util::rt::TokioIo;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const LUCINA_TOKEN: &str = "lucina-server-secret";
const MCP_TOKEN: &str = "svc_mcp_agents_0123456789abcdefghijklmnopqrstuv";
const BROKER_TOKEN: &str = "svc_broker_0123456789abcdefghijklmnopqrstuvwxyz";
const LUCINA_SERVICE_TOKEN: &str = "svc_lucina_0123456789abcdefghijklmnopqrstuvwxyz";

/// What the mock Lucina saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    origin: Option<String>,
    auth: Option<String>,
}

struct Lucina {
    socket: PathBuf,
    seen: Mutex<Vec<Seen>>,
    refuse_token: AtomicBool,
}

/// lucina-server's forwarder, in short: check the bearer, strip the prefix, pass the
/// origin and the three content headers, stream the answer back.
async fn lucina(State(l): State<Arc<Lucina>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    l.seen.lock().unwrap().push(Seen { path: uri.path().to_owned(), origin: h("x-lucina-origin"), auth: h("authorization") });
    if l.refuse_token.load(Ordering::SeqCst) || h("authorization").as_deref() != Some(&format!("Bearer {LUCINA_TOKEN}")) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "unauthorized"}))).into_response();
    }
    let Some(path) = uri.path().strip_prefix("/api/secretary/agentd") else {
        return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not_found"}))).into_response();
    };
    let target = match uri.query() {
        Some(q) => format!("{path}?{q}"),
        None => path.to_owned(),
    };
    let mut req = axum::http::Request::builder().method(method).uri(target).header("host", "lucina-agentd");
    for name in ["x-lucina-origin", "content-type", "accept", "last-event-id"] {
        if let Some(v) = headers.get(name) {
            req = req.header(name, v);
        }
    }
    let req = req.header("content-length", body.len()).body(http_body_util::Full::new(body)).unwrap();
    let stream = tokio::net::UnixStream::connect(&l.socket).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
    tokio::spawn(conn);
    let resp = sender.send_request(req).await.unwrap();
    let (parts, incoming) = resp.into_parts();
    let mut out = Response::new(Body::new(incoming));
    *out.status_mut() = parts.status;
    if let Some(ct) = parts.headers.get("content-type") {
        out.headers_mut().insert("content-type", ct.clone());
    }
    out
}

async fn agentd(dir: &Path) -> PathBuf {
    use lucina_agentd::lane::LaneConfig;
    use lucina_agentd::manager::Manager;
    use lucina_agentd::peer::Peer;
    use lucina_agentd::store::Store;
    let manager = Manager::open(Store::open(&dir.join("state")).unwrap(), Vec::new(), LaneConfig::default()).unwrap();
    let socket = dir.join("agentd.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let router = lucina_agentd::api::router(manager);
    tokio::spawn(async move { axum::serve(listener, router.into_make_service_with_connect_info::<Peer>()).await.unwrap() });
    socket
}

struct H {
    base: String,
    http: reqwest::Client,
    mac: String,
    phone: String,
    mac_id: String,
    phone_id: String,
    mcp_device: String,
    lucina: Arc<Lucina>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let lucina = Arc::new(Lucina { socket: agentd(dir.path()).await, seen: Mutex::default(), refuse_token: AtomicBool::new(false) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(self::lucina).with_state(lucina.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("server");
    let store = DeviceStore::new(&cfg.data_dir);
    let (mac_dev, mac) = store.issue("MacBook (Secretary Code)").unwrap();
    let (phone_dev, phone) = store.issue("iPhone").unwrap();
    let (mcp_dev, mcp_device) = store.issue("secretary-mcp").unwrap();
    cfg.service_token = Some(ServiceToken::from_secret(LUCINA_SERVICE_TOKEN).unwrap());
    cfg.agents.mcp_token = Some(ServiceToken::from_secret(MCP_TOKEN).unwrap());
    cfg.agents.broker_token = Some(ServiceToken::from_secret(BROKER_TOKEN).unwrap());
    cfg.agents.mcp_devices = vec![mcp_dev.id.clone()];
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = app(state);
    tokio::spawn(async move { serve_on(listener, router, std::future::pending()).await.unwrap() });
    H {
        base,
        http: reqwest::Client::new(),
        mac,
        phone,
        mac_id: mac_dev.id,
        phone_id: phone_dev.id,
        mcp_device,
        lucina,
        _dir: dir,
    }
}

impl H {
    fn req(&self, method: Method, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{path}", self.base));
        match token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    async fn json(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn call(&self, method: Method, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
        let mut rb = self.req(method, path, Some(token));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        self.json(rb).await
    }

    async fn register(&self, token: &str, key: &str) -> (u16, Value) {
        let body = json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/Users/giuseppe/Projects/app", "idempotencyKey": key});
        self.call(Method::POST, "/v1/agents", token, Some(body)).await
    }

    fn last_origin(&self) -> Option<String> {
        self.lucina.seen.lock().unwrap().last().and_then(|s| s.origin.clone())
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn the_contract_paths_reach_the_daemon() {
    let h = harness().await;
    for path in ["/v1/agents", "/v1/agents/items", "/v1/agents/permissions", "/v1/agents/workspaces", "/v1/agents/projects", "/v1/agents/commands"] {
        let (status, v) = h.call(Method::GET, path, &h.mac, None).await;
        assert_eq!(status, 200, "{path}: {v}");
    }
    let (status, v) = h.call(Method::GET, "/v1/agents/providers", &h.phone, None).await;
    assert_eq!(status, 200);
    assert!(v.as_array().unwrap().iter().any(|p| p["id"] == "secretary-code"), "{v}");
    // Daemon errors pass through in the one error shape.
    let (status, v) = h.call(Method::GET, "/v1/agents/0000-missing", &h.phone, None).await;
    assert_eq!((status, code(&v)), (404, "AGENT_NOT_FOUND"), "{v}");
    let (status, v) = h.call(Method::GET, "/v1/agents/permissions/ext_nope", &h.phone, None).await;
    assert_eq!((status, code(&v)), (404, "PERMISSION_NOT_FOUND"));
    // What Lucina got: the agentd prefix, the server's token, the device's origin.
    let seen = h.lucina.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.path == "/api/secretary/agentd/agents/items"));
    assert!(seen.iter().any(|s| s.path == "/api/secretary/agentd/permissions/ext_nope"));
    assert!(seen.iter().all(|s| s.auth.as_deref() == Some(&format!("Bearer {LUCINA_TOKEN}"))));
    assert_eq!(seen.last().unwrap().origin.as_deref(), Some(format!("device:{}", h.phone_id).as_str()));
    // Bad paths never leave the server.
    let (status, _) = h.call(Method::GET, "/v1/agents/a%2F..%2Fx/../../etc", &h.mac, None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn auth_negatives() {
    let h = harness().await;
    let (status, v) = h.json(h.req(Method::GET, "/v1/agents", None)).await;
    assert_eq!((status, code(&v)), (401, "unauthorized"));
    let (status, _) = h.json(h.req(Method::GET, "/v1/agents", Some("sec_not_a_real_token_000000000000000"))).await;
    assert_eq!(status, 401);
    let (status, _) = h.json(h.req(Method::GET, "/v1/agents", Some("svc_unknown_0123456789abcdefghijklmnopqrstuvw"))).await;
    assert_eq!(status, 401);
    let (status, v) = h.json(h.req(Method::GET, "/v1/agents", Some(LUCINA_SERVICE_TOKEN))).await;
    assert_eq!(status, 403, "Lucina's service token is not an agents caller: {v}");
    // A web session: reads work, writes need the CSRF header.
    let login = h.http.post(format!("{}/v1/session", h.base)).json(&json!({"token": h.phone})).send().await.unwrap();
    let cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let (status, _) = h.json(h.http.get(format!("{}/v1/agents", h.base)).header("cookie", &cookie)).await;
    assert_eq!(status, 200);
    let body = json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/x"});
    let (status, v) = h.json(h.http.post(format!("{}/v1/agents", h.base)).header("cookie", &cookie).json(&body)).await;
    assert_eq!((status, code(&v)), (403, "csrf"));
    let (status, v) = h.json(h.http.post(format!("{}/v1/agents", h.base)).header("cookie", &cookie).header("x-secretary-csrf", "1").json(&body)).await;
    assert_eq!(status, 200, "{v}");
    // Lucina refusing the server's own token is a 502, not the client's 401.
    h.lucina.refuse_token.store(true, Ordering::SeqCst);
    let (status, v) = h.call(Method::GET, "/v1/agents", &h.mac, None).await;
    assert_eq!((status, code(&v)), (502, "lucina_auth"));
}

#[tokio::test]
async fn a_client_cannot_choose_its_origin() {
    let h = harness().await;
    let rb = h
        .req(Method::POST, "/v1/agents", Some(&h.phone))
        .header("x-lucina-origin", format!("device:{}", h.mac_id))
        .json(&json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/x", "ownerDeviceId": h.mac_id}));
    let (status, v) = h.json(rb).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["ownerDeviceId"], h.phone_id.as_str(), "the authenticated device owns it");
    assert_eq!(h.last_origin(), Some(format!("device:{}", h.phone_id)));
    let rb = h.req(Method::GET, "/v1/agents", Some(&h.phone)).header("x-lucina-origin", "local");
    h.json(rb).await;
    assert_eq!(h.last_origin(), Some(format!("device:{}", h.phone_id)));
}

#[tokio::test]
async fn external_agents_belong_to_the_registering_device() {
    let h = harness().await;
    let (status, a) = h.register(&h.mac, "mac-session-1").await;
    assert_eq!(status, 200, "{a}");
    assert_eq!((a["external"].clone(), a["ownerDeviceId"].clone(), a["host"].clone()), (json!(true), json!(h.mac_id), json!("mac")));
    let id = a["id"].as_str().unwrap().to_owned();
    assert_eq!(h.register(&h.mac, "mac-session-1").await.1["id"], a["id"], "idempotent");

    let events = json!({"events": [
        {"type": "turn_started", "provider": "secretary-code", "turnId": "t1"},
        {"type": "timeline", "provider": "secretary-code", "turnId": "t1", "item": {"type": "user_message", "text": "refactor"}},
    ]});
    let (status, v) = h.call(Method::POST, &format!("/v1/agents/{id}/events"), &h.phone, Some(events.clone())).await;
    assert_eq!((status, code(&v)), (403, "NOT_OWNER"), "another device");
    let (status, v) = h.call(Method::POST, &format!("/v1/agents/{id}/events"), MCP_TOKEN, Some(events.clone())).await;
    assert_eq!((status, code(&v)), (403, "NOT_OWNER"), "the MCP");
    let (status, v) = h.call(Method::POST, &format!("/v1/agents/{id}/events"), &h.mac, Some(events)).await;
    assert_eq!((status, v["applied"].clone()), (200, json!(2)), "{v}");
    let (_, snap) = h.call(Method::GET, &format!("/v1/agents/{id}"), &h.phone, None).await;
    assert_eq!(snap["status"], "running");
    let (_, tl) = h.call(Method::GET, &format!("/v1/agents/{id}/timeline?afterSeq=0"), &h.phone, None).await;
    assert_eq!(tl["rows"][0]["item"]["text"], "refactor");

    // The phone steers: a command for the Mac only.
    let (status, v) = h.call(Method::POST, &format!("/v1/agents/{id}/send"), &h.phone, Some(json!({"prompt": "also tests", "activeTurnBehavior": "steer"}))).await;
    assert_eq!((status, v["disposition"].clone()), (200, json!("queued")), "{v}");
    let (_, mine) = h.call(Method::GET, "/v1/agents/commands", &h.mac, None).await;
    let cmds = mine["commands"].as_array().unwrap().clone();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["command"], json!({"kind": "steer", "prompt": "also tests"}));
    let (_, theirs) = h.call(Method::GET, "/v1/agents/commands", &h.phone, None).await;
    assert_eq!(theirs["commands"], json!([]));
    let (status, _) = h.call(Method::GET, "/v1/agents/commands", MCP_TOKEN, None).await;
    assert_eq!(status, 403);
    let cid = cmds[0]["commandId"].as_str().unwrap();
    let ack = format!("/v1/agents/{id}/commands/{cid}/ack");
    assert_eq!(h.call(Method::POST, &ack, &h.phone, None).await.0, 403);
    let (status, v) = h.call(Method::POST, &ack, &h.mac, None).await;
    assert_eq!((status, v["acked"].clone()), (200, json!(true)));
    let (_, mine) = h.call(Method::GET, "/v1/agents/commands", &h.mac, None).await;
    assert_eq!(mine["commands"], json!([]));
    // Rename (the mirror's PATCH).
    let (status, v) = h.call(Method::PATCH, &format!("/v1/agents/{id}"), &h.mac, Some(json!({"title": "Refactor"}))).await;
    assert_eq!((status, v["title"].clone()), (200, json!("Refactor")));
}

#[tokio::test]
async fn mcp_callers_never_answer_and_never_own() {
    let h = harness().await;
    // The three ways the MCP shows up all get an `mcp:` origin.
    let as_mcp = [
        h.req(Method::GET, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-mcp-caller", "tok_Ab9:hermes").header("x-lucina-delegation-depth", "1"),
        h.req(Method::GET, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-origin", "device:spoofed"),
        h.req(Method::GET, "/v1/agents", Some(&h.mcp_device)),
        h.req(Method::GET, "/v1/agents", Some(&h.phone)).header("x-secretary-caller", "mcp:claude"),
    ];
    let mut origins = Vec::new();
    for rb in as_mcp {
        assert_eq!(h.json(rb).await.0, 200);
        origins.push(h.last_origin().unwrap());
    }
    assert_eq!(origins, ["mcp:tok_Ab9", "mcp:unknown", "mcp:device", "mcp:claude"]);
    // Not an owner device: it can't register an external agent.
    let (status, v) = h.register(&h.mcp_device, "k").await;
    assert_eq!((status, code(&v)), (403, "NOT_OWNER"), "{v}");
    let (status, _) = h.json(h.req(Method::POST, "/v1/agents", Some(&h.phone)).header("x-secretary-caller", "mcp:claude")
        .json(&json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/x"}))).await;
    assert_eq!(status, 403);
    // An external permission request: the MCP may read it, never answer it.
    let req = json!({"kind": "external", "source": "infisical-brokerd", "title": "Read GITHUB_TOKEN"});
    let (status, p) = h.call(Method::POST, "/v1/agents/permissions", BROKER_TOKEN, Some(req)).await;
    assert_eq!(status, 201, "{p}");
    let pid = p["id"].as_str().unwrap();
    let (status, v) = h.call(Method::GET, "/v1/agents/permissions", MCP_TOKEN, None).await;
    assert_eq!(status, 200);
    assert_eq!(v["permissions"][0]["id"], pid);
    for token in [MCP_TOKEN, h.mcp_device.as_str()] {
        let (status, _) = h.call(Method::POST, &format!("/v1/agents/permissions/{pid}"), token, Some(json!({"behavior": "allow"}))).await;
        assert_eq!(status, 403);
    }
    let (status, _) = h.call(Method::POST, "/v1/agents/abc/permissions/r1", MCP_TOKEN, Some(json!({"behavior": "allow"}))).await;
    assert_eq!(status, 403, "refused before it reaches the daemon");
    // The owner's phone answers it.
    let (status, v) = h.call(Method::POST, &format!("/v1/agents/permissions/{pid}"), &h.phone, Some(json!({"behavior": "allow"}))).await;
    assert_eq!((status, v["status"].clone()), (200, json!("allowed")), "{v}");
}

#[tokio::test]
async fn the_broker_only_asks_and_waits() {
    let h = harness().await;
    let req = json!({"kind": "external", "source": "infisical-brokerd", "title": "Read OPENAI_API_KEY", "expiresInSec": 60});
    let (status, p) = h.call(Method::POST, "/v1/agents/permissions", BROKER_TOKEN, Some(req)).await;
    assert_eq!(status, 201, "{p}");
    assert_eq!(h.last_origin().as_deref(), Some("service:infisical-broker"));
    let pid = p["id"].as_str().unwrap().to_owned();
    for (method, path) in [
        (Method::GET, "/v1/agents".to_owned()),
        (Method::GET, "/v1/agents/permissions".to_owned()),
        (Method::GET, "/v1/agents/stream".to_owned()),
        (Method::POST, format!("/v1/agents/permissions/{pid}")),
        (Method::POST, "/v1/agents".to_owned()),
    ] {
        let (status, v) = h.call(method.clone(), &path, BROKER_TOKEN, Some(json!({"behavior": "allow"}))).await;
        assert_eq!(status, 403, "{method} {path}: {v}");
    }
    // Long-poll while the phone answers.
    let waiter = {
        let rb = h.req(Method::GET, &format!("/v1/agents/permissions/{pid}?waitMs=10000"), Some(BROKER_TOKEN));
        tokio::spawn(async move { rb.send().await.unwrap().json::<Value>().await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, _) = h.call(Method::POST, &format!("/v1/agents/permissions/{pid}"), &h.phone, Some(json!({"behavior": "deny", "message": "not now"}))).await;
    assert_eq!(status, 200);
    let answered = tokio::time::timeout(Duration::from_secs(5), waiter).await.unwrap().unwrap();
    assert_eq!(answered["status"], "denied");
    assert_eq!(answered["resolution"]["by"], "owner_device");
}

/// Read SSE frames `(id, event, data)` until `done` or 4 s.
async fn frames(resp: reqwest::Response, done: impl Fn(&[(Option<String>, Option<String>, Value)]) -> bool) -> Vec<(Option<String>, Option<String>, Value)> {
    let mut body = resp.bytes_stream();
    let mut buf = String::new();
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while !done(&out) {
        let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await else { break };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let block: String = buf.drain(..end + 2).collect();
            let (mut id, mut event, mut data) = (None, None, String::new());
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("id:") {
                    id = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
            if !data.is_empty() {
                out.push((id, event, serde_json::from_str(&data).unwrap_or(Value::Null)));
            }
        }
    }
    out
}

#[tokio::test]
async fn the_stream_resumes_and_keeps_commands_to_their_device() {
    let h = harness().await;
    let id = h.register(&h.mac, "k").await.1["id"].as_str().unwrap().to_owned();
    let mac_stream = h.req(Method::GET, "/v1/agents/stream", Some(&h.mac)).send().await.unwrap();
    assert_eq!(mac_stream.status(), 200);
    assert!(mac_stream.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let phone_stream = h.req(Method::GET, "/v1/agents/stream", Some(&h.phone)).send().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.call(Method::POST, &format!("/v1/agents/{id}/send"), &h.phone, Some(json!({"prompt": "go"}))).await;
    h.call(Method::PATCH, &format!("/v1/agents/{id}"), &h.mac, Some(json!({"title": "after the command"}))).await;
    let is_state = |f: &(Option<String>, Option<String>, Value)| f.2["type"] == "agent_state";
    let mac = frames(mac_stream, |f| f.iter().any(is_state)).await;
    let phone = frames(phone_stream, |f| f.iter().any(is_state)).await;
    let cmd = mac.iter().find(|f| f.2["type"] == "agent_command").expect("the Mac gets its command");
    assert_eq!(cmd.2["command"]["prompt"], "go");
    assert!(!phone.iter().any(|f| f.2["type"] == "agent_command"), "{phone:?}");
    assert!(phone.iter().any(is_state));
    // Reconnect after the command: the daemon replays what came after it.
    let cmd_id = cmd.0.clone().expect("numbered");
    h.call(Method::PATCH, &format!("/v1/agents/{id}"), &h.mac, Some(json!({"title": "while away"}))).await;
    let again = h.req(Method::GET, "/v1/agents/stream", Some(&h.mac)).header("last-event-id", &cmd_id).send().await.unwrap();
    let replay = frames(again, |f| f.iter().any(|f| f.2["agent"]["title"] == "while away")).await;
    let ids: Vec<u64> = replay.iter().filter_map(|f| f.0.as_deref()?.parse().ok()).collect();
    assert_eq!(ids.first().copied(), Some(cmd_id.parse::<u64>().unwrap() + 1), "{replay:?}");
    assert!(replay.iter().any(|f| f.2["agent"]["title"] == "while away"));
    // The same through `?lastEventId=` (a new EventSource), and a reset for a foreign id.
    let q = h.req(Method::GET, &format!("/v1/agents/stream?lastEventId={cmd_id}"), Some(&h.mac)).send().await.unwrap();
    let replay = frames(q, |f| !f.is_empty()).await;
    assert_eq!(replay[0].0.as_deref().map(|s| s.parse::<u64>().unwrap()), Some(cmd_id.parse::<u64>().unwrap() + 1));
    let reset = h.req(Method::GET, "/v1/agents/stream", Some(&h.mac)).header("last-event-id", "12").send().await.unwrap();
    let r = frames(reset, |f| !f.is_empty()).await;
    assert_eq!((r[0].1.as_deref(), r[0].2["type"].as_str()), (Some("stream.reset"), Some("stream.reset")));
}

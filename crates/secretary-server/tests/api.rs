//! End-to-end tests: the real router on an ephemeral port in front of a mock
//! Lucina (axum, ephemeral port) that records what it receives.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::{Config, UpstreamConfig};
use secretary_server::devices::DeviceStore;
use secretary_server::{app, AppState};
use serde_json::{json, Value};

const LUCINA_TOKEN: &str = "lucina-server-secret";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    auth: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<Seen>>,
    health_hits: AtomicUsize,
}

impl Mock {
    fn last(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("mock saw a request")
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn ok(data: Value) -> Response {
    Json(json!({"ok": true, "data": data})).into_response()
}

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": code, "message": message})),
    )
        .into_response()
}

async fn mock_handler(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let seen = Seen {
        method: method.to_string(),
        path: path.clone(),
        query: uri.query().map(str::to_owned),
        auth: headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    let echo = json!({
        "method": seen.method, "path": seen.path, "query": seen.query, "body": seen.body,
    });
    if path == "/api/secretary/health" {
        m.health_hits.fetch_add(1, Ordering::SeqCst);
        return ok(json!({"persona": "Lucina", "memoryProvider": "honcho",
                         "timeZone": "Europe/Rome", "now": "2026-10-21T12:00:00+02:00"}));
    }
    m.seen.lock().unwrap().push(seen.clone());
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/secretary/chat") => ok(json!({
            "reply": format!("echo: {}", seen.body["message"].as_str().unwrap_or("")),
            "action": "chat", "job": {"id": "j1", "status": "queued"},
        })),
        ("GET", "/api/secretary/jobs/slow") => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            ok(json!({}))
        }
        ("GET", "/api/secretary/jobs/boom") => {
            fail(StatusCode::INTERNAL_SERVER_ERROR, "internal", "kaboom")
        }
        ("GET", "/api/secretary/jobs/denied") => {
            fail(StatusCode::UNAUTHORIZED, "unauthorized", "bad token")
        }
        ("GET", p) if p.starts_with("/api/secretary/jobs/") => ok(json!({
            "id": p.rsplit('/').next(), "status": "done", "action": "reminder.create",
            "result": {"ok": 1}, "createdAt": "t0", "updatedAt": "t1",
        })),
        ("DELETE", "/api/secretary/reminders/missing") => {
            fail(StatusCode::NOT_FOUND, "not_found", "no such reminder")
        }
        ("DELETE", "/api/secretary/reminders/sent") => fail(
            StatusCode::CONFLICT,
            "already_sent",
            "reminder already sent",
        ),
        _ => ok(echo),
    }
}

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

struct Harness {
    base: String,
    http: reqwest::Client,
    token: String,
    device_id: String,
    store: DeviceStore,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(tweak: impl FnOnce(&mut Config, &str)) -> Self {
        let mock = Arc::new(Mock::default());
        let lucina = spawn(
            Router::new()
                .fallback(mock_handler)
                .with_state(mock.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new(lucina.clone(), Some(LUCINA_TOKEN.into()));
        cfg.data_dir = dir.path().join("state");
        tweak(&mut cfg, &lucina);
        let store = DeviceStore::new(&cfg.data_dir);
        let (dev, token) = store.issue("test iPhone").unwrap();
        let base = spawn(app(AppState::new(&cfg).unwrap())).await;
        Self {
            base,
            http: reqwest::Client::new(),
            token,
            device_id: dev.id,
            store,
            mock,
            _dir: dir,
        }
    }

    async fn default() -> Self {
        Self::new(|_, _| {}).await
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value, HeaderMap) {
        let resp = rb.send().await.unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        let headers = resp.headers().clone();
        let body = resp.json::<Value>().await.unwrap_or(Value::Null);
        (status, body, headers)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (s, b, _) = self.send(self.req(Method::GET, path)).await;
        (s, b)
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("<none>")
}

// -- auth -------------------------------------------------------------------

#[tokio::test]
async fn rejects_missing_malformed_unknown_and_revoked_tokens() {
    let h = Harness::default().await;
    let url = format!("{}/v1/shopping", h.base);

    let (s, b, hd) = h.send(h.http.get(&url)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&b), "unauthorized");
    assert_eq!(hd["www-authenticate"], "Bearer");

    let (s, _, _) = h
        .send(h.http.get(&url).header("authorization", "Basic abc"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h
        .send(h.http.get(&url).bearer_auth("sec_notarealtoken"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // valid token works ...
    assert_eq!(h.get("/v1/shopping").await.0, StatusCode::OK);
    // ... until revoked by the CLI-side store, without a server restart
    h.store.revoke(&h.device_id).unwrap().unwrap();
    let (s, b) = h.get("/v1/shopping").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["error"]["message"], "device token revoked");
    // nothing reached Lucina without auth
    assert_eq!(h.mock.count(), 1);
}

#[tokio::test]
async fn health_needs_no_auth_and_is_cached() {
    let h = Harness::default().await;
    let resp = h
        .http
        .get(format!("{}/v1/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let b: Value = resp.json().await.unwrap();
    assert_eq!(b["ok"], true);
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(b["lucina"]["reachable"], true);
    assert!(b["lucina"]["latencyMs"].is_u64());
    let _ = h
        .http
        .get(format!("{}/v1/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        h.mock.health_hits.load(Ordering::SeqCst),
        1,
        "second probe cached"
    );
}

#[tokio::test]
async fn rate_limit_returns_429() {
    let h = Harness::new(|c, _| c.rate_limit_per_minute = 3).await;
    for _ in 0..3 {
        assert_eq!(h.get("/v1/shopping").await.0, StatusCode::OK);
    }
    let (s, b, hd) = h.send(h.req(Method::GET, "/v1/shopping")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&b), "rate_limited");
    assert!(hd.contains_key("retry-after"));
}

// -- forwarding ---------------------------------------------------------------

#[tokio::test]
async fn forwards_every_route_with_the_server_token() {
    let h = Harness::default().await;

    // chat
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/chat").json(&json!({
            "message": "remind me to call mum", "sessionId": "s1", "clientMessageId": "c1"
        })))
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["reply"], "echo: remind me to call mum");
    assert_eq!(b["job"], json!({"id": "j1", "status": "queued"}));
    let seen = h.mock.last();
    assert_eq!(
        (seen.method.as_str(), seen.path.as_str()),
        ("POST", "/api/secretary/chat")
    );
    assert_eq!(seen.auth.as_deref(), Some("Bearer lucina-server-secret"));
    assert_eq!(seen.body["sessionId"], "s1");
    assert_eq!(seen.body["clientMessageId"], "c1");

    // job
    let (s, b) = h.get("/v1/jobs/j1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["id"], "j1");
    assert_eq!(b["status"], "done");

    // reminders list with normalised query
    let (s, b) = h.get("/v1/reminders?scope=today&includeSent=true").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["path"], "/api/secretary/reminders");
    assert_eq!(b["query"], "scope=today&includeSent=1");

    // reminder create, both forms
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/reminders").json(&json!({
            "text": "call mum", "dueAt": "2026-10-22T09:00:00+02:00"
        })))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b["body"],
        json!({"text": "call mum", "dueAt": "2026-10-22T09:00:00+02:00"})
    );
    let (s, b, _) = h
        .send(
            h.req(Method::POST, "/v1/reminders")
                .json(&json!({"naturalText": "tomorrow 9am call mum"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["body"], json!({"naturalText": "tomorrow 9am call mum"}));

    // reminder cancel
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/r42")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (b["method"].as_str(), b["path"].as_str()),
        (Some("DELETE"), Some("/api/secretary/reminders/r42"))
    );

    // agenda
    let (s, b) = h.get("/v1/agenda?date=2026-10-22&refresh=1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["query"], "date=2026-10-22&refresh=1");
    let (s, b) = h.get("/v1/agenda").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["query"], Value::Null);

    // shopping
    let (s, b) = h.get("/v1/shopping").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["path"], "/api/secretary/shopping");
    let (s, b, _) = h
        .send(
            h.req(Method::POST, "/v1/shopping")
                .json(&json!({"name": "milk", "spec": "2l"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["body"], json!({"name": "milk", "spec": "2l"}));
    let (s, b, _) = h
        .send(
            h.req(Method::PATCH, "/v1/shopping/item-7")
                .json(&json!({"checked": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["method"], "PATCH");
    assert_eq!(b["path"], "/api/secretary/shopping/item-7");
    assert_eq!(b["body"], json!({"checked": true}));

    // the device token never reaches Lucina
    for s in h.mock.seen.lock().unwrap().iter() {
        assert_eq!(s.auth.as_deref(), Some("Bearer lucina-server-secret"));
    }
}

#[tokio::test]
async fn invalid_input_is_rejected_before_lucina() {
    let h = Harness::default().await;
    let cases = [
        h.req(Method::POST, "/v1/chat")
            .json(&json!({"message": "  "})),
        h.req(Method::POST, "/v1/chat").json(&json!({"nope": 1})),
        h.req(Method::POST, "/v1/chat").body("not json"),
        h.req(Method::POST, "/v1/reminders")
            .json(&json!({"text": "x", "dueAt": "soon"})),
        h.req(Method::POST, "/v1/shopping").json(&json!(["milk"])),
        h.req(Method::GET, "/v1/reminders?scope=yesterday"),
        h.req(Method::GET, "/v1/agenda?date=22-10-2026"),
        h.req(Method::GET, "/v1/jobs/a%2F..%2Fadmin"),
    ];
    for rb in cases {
        let (s, b, _) = h.send(rb).await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{s} {b}"
        );
        assert!(
            code(&b) == "invalid_request" || code(&b) == "unsupported_media_type",
            "{b}"
        );
    }
    assert_eq!(h.mock.count(), 0);

    // 256 KiB body limit
    let big = "a".repeat(300 * 1024);
    let (s, b, _) = h
        .send(
            h.req(Method::POST, "/v1/chat")
                .json(&json!({"message": big})),
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&b), "payload_too_large");

    // unknown route: JSON 404
    let (s, b) = h.get("/v1/nope").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(code(&b), "not_found");
}

// -- error mapping --------------------------------------------------------------

#[tokio::test]
async fn lucina_4xx_passes_through() {
    let h = Harness::default().await;
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/missing")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        b,
        json!({"error": {"code": "not_found", "message": "no such reminder"}})
    );
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/sent")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(code(&b), "already_sent");
}

#[tokio::test]
async fn lucina_5xx_and_auth_failures_become_502() {
    let h = Harness::default().await;
    let (s, b) = h.get("/v1/jobs/boom").await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(code(&b), "lucina_error");
    // Lucina rejecting the *server* token must not look like a device 401.
    let (s, b) = h.get("/v1/jobs/denied").await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(code(&b), "lucina_error");
}

#[tokio::test]
async fn lucina_timeout_is_504() {
    let h = Harness::new(|c, _| c.timeouts.default = Duration::from_millis(300)).await;
    let (s, b) = h.get("/v1/jobs/slow").await;
    assert_eq!(s, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(code(&b), "lucina_timeout");
}

#[tokio::test]
async fn lucina_unreachable_is_502() {
    // A port that was free a moment ago: connection refused.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let h = Harness::new(|c, _| c.lucina_url = dead).await;
    let (s, b) = h.get("/v1/shopping").await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(code(&b), "lucina_unreachable");

    let b: Value = h
        .http
        .get(format!("{}/v1/health", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(b["lucina"]["reachable"], false);
    assert_eq!(b["lucina"]["error"], "lucina_unreachable");
}

#[tokio::test]
async fn data_routes_503_without_lucina_token() {
    let h = Harness::new(|c, _| c.lucina_token = None).await;
    let (s, b) = h.get("/v1/shopping").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&b), "lucina_not_configured");
    let resp = h
        .http
        .get(format!("{}/v1/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

// -- upstream shopping routes ------------------------------------------------------

#[tokio::test]
async fn upstream_routes_only_when_configured_and_behind_auth() {
    let h = Harness::default().await;
    assert_eq!(
        h.get("/v1/upstream/lists/u1").await.0,
        StatusCode::NOT_FOUND
    );

    let h = Harness::new(|c, mock| {
        c.upstream = Some(UpstreamConfig {
            legacy: Some(format!("{mock}/rest")),
            services: None,
            catalog: None,
        })
    })
    .await;
    let url = format!("{}/v1/upstream/lists/u1", h.base);
    assert_eq!(h.send(h.http.get(&url)).await.0, StatusCode::UNAUTHORIZED);

    let (s, b, _) = h
        .send(
            h.req(Method::GET, "/v1/upstream/lists/u1")
                .header("x-upstream-authorization", "Bearer upstream-user"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let seen = h.mock.last();
    assert_eq!(seen.path, "/rest/bringusers/u1/lists");
    assert_eq!(seen.auth.as_deref(), Some("Bearer upstream-user"));

    // escape hatch + traversal guard + unconfigured family
    let (s, _, _) = h
        .send(h.req(Method::GET, "/v1/upstream/any/legacy/v2/bringlists/l1"))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.mock.last().path, "/rest/v2/bringlists/l1");
    let (s, _, _) = h
        .send(h.req(Method::GET, "/v1/upstream/lists/a%2F..%2Fx"))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b, _) = h.send(h.req(Method::GET, "/v1/upstream/offers")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&b), "upstream_not_configured");
}

// -- token CLI ---------------------------------------------------------------------

fn cli(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_secretary-server"))
        .env("SECRETARY_DATA_DIR", dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[tokio::test]
async fn token_cli_issue_list_revoke_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("secretary");

    let (ok, out) = cli(&data, &["token", "issue", "--name", "Giuseppe's iPhone"]);
    assert!(ok, "{out}");
    let token = out
        .lines()
        .find(|l| l.starts_with("sec_"))
        .expect("token printed")
        .to_owned();
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("device id: "))
        .unwrap()
        .to_owned();
    let raw = std::fs::read_to_string(data.join("devices.json")).unwrap();
    assert!(!raw.contains(&token));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(data.join("devices.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let (ok, out) = cli(&data, &["token", "list"]);
    assert!(ok);
    assert!(out.contains(&id) && out.contains("active") && out.contains("Giuseppe's iPhone"));
    assert!(!out.contains(&token));

    // the issued token authenticates against a server on that data dir
    let mut cfg = Config::new("http://127.0.0.1:9", None);
    cfg.data_dir = data.clone();
    let base = spawn(app(AppState::new(&cfg).unwrap())).await;
    let http = reqwest::Client::new();
    let status = |t: String| {
        let http = http.clone();
        let url = format!("{base}/v1/shopping");
        async move {
            http.get(url)
                .bearer_auth(t)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(
        status(token.clone()).await,
        503,
        "authenticated; Lucina not configured"
    );

    let (ok, out) = cli(&data, &["token", "revoke", &id]);
    assert!(ok, "{out}");
    let (_, out) = cli(&data, &["token", "list"]);
    assert!(out.contains("revoked"));
    assert_eq!(status(token).await, 401);

    let (ok, _) = cli(&data, &["token", "revoke", "dev_nope"]);
    assert!(!ok);
}

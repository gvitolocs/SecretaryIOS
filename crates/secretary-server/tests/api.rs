//! End-to-end tests: the real router on an ephemeral port (with peer
//! addresses, like production) in front of a mock Lucina (axum, ephemeral
//! port) that follows Lucina's docs/secretary-api.md and records requests.

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
use secretary_server::{app, serve_on, AppState};
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

fn ok_with(status: StatusCode, data: Value) -> Response {
    (status, Json(json!({"ok": true, "data": data}))).into_response()
}

fn ok(data: Value) -> Response {
    ok_with(StatusCode::OK, data)
}

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": code, "message": message})),
    )
        .into_response()
}

fn reminder(id: &str, text: &str, status: &str) -> Value {
    json!({"id": id, "text": text, "dueAt": "2026-10-22T07:30:00.000Z", "timeZone": "Europe/Rome",
           "status": status, "source": "secretary", "createdAt": "2026-10-21T15:02:11.481Z",
           "sentAt": null, "cancelledAt": null})
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
                         "timeZone": "Europe/Rome", "now": "2026-10-21T12:00:00.000Z",
                         "apiVersion": 1}));
    }
    m.seen.lock().unwrap().push(seen.clone());
    let text = seen.body["text"].as_str().unwrap_or("");
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/secretary/chat") => ok(json!({
            "reply": format!("echo: {}", seen.body["message"].as_str().unwrap_or("")),
            "action": "graph_task", "job": {"id": "j1", "status": "queued"},
            "sessionId": "secretary_iphone", "clientMessageId": seen.body["clientMessageId"],
        })),
        ("GET", "/api/secretary/jobs/slow") => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            ok(json!({}))
        }
        ("GET", "/api/secretary/jobs/boom") => fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "kaboom",
        ),
        ("GET", "/api/secretary/jobs/denied") => {
            fail(StatusCode::UNAUTHORIZED, "unauthorized", "bad token")
        }
        ("GET", "/api/secretary/jobs/disabled") => {
            fail(StatusCode::NOT_FOUND, "not_found", "disabled")
        }
        ("GET", "/api/secretary/jobs/missing") => {
            fail(StatusCode::NOT_FOUND, "job_not_found", "no such job")
        }
        ("GET", p) if p.starts_with("/api/secretary/jobs/") => ok(json!({
            "id": p.rsplit('/').next(), "status": "done", "action": "generate_image",
            "result": "here you go", "error": null, "imageUrl": "https://img.example/x.png",
            "createdAt": "t0", "updatedAt": "t1",
        })),
        ("GET", "/api/secretary/reminders") => ok(json!({
            "scope": "today", "includeSent": true, "timeZone": "Europe/Rome",
            "reminders": [reminder("r1", "call mum", "pending")], "query": seen.query,
        })),
        ("POST", "/api/secretary/reminders") if text == "disabled" => fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "reminders_disabled",
            "reminders are off",
        ),
        ("POST", "/api/secretary/reminders") if text == "past" => {
            fail(StatusCode::BAD_REQUEST, "due_in_past", "not in the future")
        }
        ("POST", "/api/secretary/reminders") if text == "dup" => ok(json!({
            "created": false, "reminder": reminder("r1", "dup", "pending"), "body": seen.body,
        })),
        ("POST", "/api/secretary/reminders") => ok_with(
            StatusCode::CREATED,
            json!({"created": true, "reminder": reminder("r2", "x", "pending"), "body": seen.body}),
        ),
        ("DELETE", "/api/secretary/reminders/missing") => fail(
            StatusCode::NOT_FOUND,
            "reminder_not_found",
            "no such reminder",
        ),
        ("DELETE", "/api/secretary/reminders/sent") => fail(
            StatusCode::CONFLICT,
            "already_sent",
            "reminder already sent",
        ),
        ("DELETE", p) if p.starts_with("/api/secretary/reminders/") => ok(json!({
            "cancelled": true, "reminder": reminder(p.rsplit('/').next().unwrap(), "x", "cancelled"),
        })),
        ("GET", "/api/secretary/shopping") => ok(json!({"items": [
            {"id": "i1", "product": "milk", "rawName": "latte", "category": "dairy",
             "status": "needed", "createdAt": "t0", "updatedAt": "t0"}
        ]})),
        ("POST", "/api/secretary/shopping") => ok_with(
            StatusCode::CREATED,
            json!({"item": {"id": "i2", "product": seen.body["rawName"], "status": "needed"}}),
        ),
        ("PATCH", "/api/secretary/shopping/missing") => {
            fail(StatusCode::NOT_FOUND, "item_not_found", "no such item")
        }
        ("PATCH", p) if p.starts_with("/api/secretary/shopping/") => ok(json!({"item": {
            "id": p.rsplit('/').next(), "product": "latte", "status": seen.body["status"],
        }})),
        _ => ok(echo),
    }
}

async fn spawn_plain(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// The app is served exactly like production (with ConnectInfo).
async fn spawn_app(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        serve_on(listener, app(state), std::future::pending())
            .await
            .unwrap()
    });
    format!("http://{addr}")
}

struct Harness {
    base: String,
    http: reqwest::Client,
    token: String,
    device_id: String,
    store: DeviceStore,
    data_dir: std::path::PathBuf,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(tweak: impl FnOnce(&mut Config, &str)) -> Self {
        let mock = Arc::new(Mock::default());
        let lucina = spawn_plain(
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
        let base = spawn_app(AppState::new(&cfg).unwrap()).await;
        Self {
            base,
            http: reqwest::Client::new(),
            token,
            device_id: dev.id,
            store,
            data_dir: cfg.data_dir.clone(),
            mock,
            _dir: dir,
        }
    }

    async fn default() -> Self {
        Self::new(|_, _| {}).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
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

    /// Log in as a browser; returns the `sec_session` cookie value.
    async fn login(&self) -> String {
        let (s, b, h) = self
            .send(
                self.http
                    .post(self.url("/v1/session"))
                    .json(&json!({"token": self.token})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let cookie = h["set-cookie"].to_str().unwrap().to_owned();
        cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix("sec_session=")
            .unwrap()
            .to_owned()
    }

    fn browser(&self, method: Method, path: &str, sid: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("cookie", format!("sec_session={sid}"))
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("<none>")
}

// -- auth -------------------------------------------------------------------

#[tokio::test]
async fn rejects_missing_malformed_unknown_and_revoked_tokens() {
    let h = Harness::default().await;
    let url = h.url("/v1/shopping");

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
    let resp = h.http.get(h.url("/v1/health")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let b: Value = resp.json().await.unwrap();
    assert_eq!(b["ok"], true);
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(b["lucina"]["reachable"], true);
    assert_eq!(b["lucina"]["apiVersion"], 1);
    assert!(b["lucina"]["latencyMs"].is_u64());
    let _ = h.http.get(h.url("/v1/health")).send().await.unwrap();
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
    let (s, b, hd) = h
        .send(h.req(Method::POST, "/v1/chat").json(&json!({
            "message": "remind me to call mum", "sessionId": "iphone", "clientMessageId": "c1"
        })))
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(hd["cache-control"], "no-store");
    assert_eq!(b["reply"], "echo: remind me to call mum");
    assert_eq!(b["job"], json!({"id": "j1", "status": "queued"}));
    assert_eq!(b["sessionId"], "secretary_iphone");
    let seen = h.mock.last();
    assert_eq!(
        (seen.method.as_str(), seen.path.as_str()),
        ("POST", "/api/secretary/chat")
    );
    assert_eq!(seen.auth.as_deref(), Some("Bearer lucina-server-secret"));
    assert_eq!(seen.body["sessionId"], "iphone");
    assert_eq!(seen.body["clientMessageId"], "c1");

    // job (unknown fields like imageUrl pass through)
    let (s, b) = h.get("/v1/jobs/j1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["id"], "j1");
    assert_eq!(b["imageUrl"], "https://img.example/x.png");

    // reminders list: Lucina's object, query normalised
    let (s, b) = h.get("/v1/reminders?scope=today&includeSent=true").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["timeZone"], "Europe/Rome");
    assert_eq!(b["reminders"][0]["id"], "r1");
    assert_eq!(b["query"], "scope=today&includeSent=1");

    // reminder create: 201 when new, 200 for a duplicate
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/reminders").json(&json!({
            "text": "call mum", "dueAt": "2026-10-22T09:00:00+02:00"
        })))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b["created"], true);
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
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b["body"], json!({"naturalText": "tomorrow 9am call mum"}));
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/reminders").json(&json!({
            "text": "dup", "dueAt": "2026-10-22T09:00:00+02:00"
        })))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["created"], false);

    // reminder cancel
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/r42")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["cancelled"], true);
    assert_eq!(b["reminder"]["id"], "r42");
    assert_eq!(h.mock.last().path, "/api/secretary/reminders/r42");

    // agenda
    let (s, b) = h.get("/v1/agenda?date=2026-10-22&refresh=1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["query"], "date=2026-10-22&refresh=1");
    let (s, b) = h.get("/v1/agenda").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["query"], Value::Null);

    // shopping: {items} -> array, {item} -> item, 201 on create
    let (s, b) = h.get("/v1/shopping").await;
    assert_eq!(s, StatusCode::OK);
    assert!(b.is_array(), "{b}");
    assert_eq!(b[0]["product"], "milk");
    let (s, b, _) = h
        .send(
            h.req(Method::POST, "/v1/shopping")
                .json(&json!({"rawName": "latte", "product": "milk"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        b,
        json!({"id": "i2", "product": "latte", "status": "needed"})
    );
    assert_eq!(
        h.mock.last().body,
        json!({"rawName": "latte", "product": "milk"})
    );
    let (s, b, _) = h
        .send(
            h.req(Method::PATCH, "/v1/shopping/item-7")
                .json(&json!({"status": "purchased"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b,
        json!({"id": "item-7", "product": "latte", "status": "purchased"})
    );
    assert_eq!(h.mock.last().method, "PATCH");

    // the device token never reaches Lucina
    for s in h.mock.seen.lock().unwrap().iter() {
        assert_eq!(s.auth.as_deref(), Some("Bearer lucina-server-secret"));
    }
}

#[tokio::test]
async fn invalid_input_is_rejected_before_lucina() {
    let h = Harness::default().await;
    let cases = [
        (
            h.req(Method::POST, "/v1/chat")
                .json(&json!({"message": "  "})),
            "bad_request",
        ),
        (
            h.req(Method::POST, "/v1/chat").json(&json!({"nope": 1})),
            "bad_request",
        ),
        (
            h.req(Method::POST, "/v1/chat")
                .json(&json!({"message": "hi", "sessionId": "a.b"})),
            "bad_request",
        ),
        (
            h.req(Method::POST, "/v1/chat")
                .header("content-type", "application/json")
                .body("{not json"),
            "invalid_json",
        ),
        (
            h.req(Method::POST, "/v1/chat").body("x"),
            "unsupported_media_type",
        ),
        (
            h.req(Method::POST, "/v1/reminders")
                .json(&json!({"text": "x", "dueAt": "soon"})),
            "bad_request",
        ),
        (
            h.req(Method::POST, "/v1/shopping")
                .json(&json!({"name": "milk"})),
            "bad_request",
        ),
        (
            h.req(Method::POST, "/v1/shopping").json(&json!(["milk"])),
            "bad_request",
        ),
        (
            h.req(Method::GET, "/v1/reminders?scope=yesterday"),
            "bad_request",
        ),
        (
            h.req(Method::GET, "/v1/agenda?date=22-10-2026"),
            "bad_request",
        ),
        (h.req(Method::GET, "/v1/jobs/a%2F..%2Fadmin"), "bad_request"),
    ];
    for (rb, want) in cases {
        let (s, b, _) = h.send(rb).await;
        assert!(s.is_client_error(), "{s} {b}");
        assert_eq!(code(&b), want, "{b}");
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
async fn lucina_4xx_and_feature_503_pass_through() {
    let h = Harness::default().await;
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/missing")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        b,
        json!({"error": {"code": "reminder_not_found", "message": "no such reminder"}})
    );
    let (s, b, _) = h.send(h.req(Method::DELETE, "/v1/reminders/sent")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(code(&b), "already_sent");
    let (s, b) = h.get("/v1/jobs/missing").await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "job_not_found"));
    let (s, b, _) = h
        .send(
            h.req(Method::PATCH, "/v1/shopping/missing")
                .json(&json!({"status": "needed"})),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "item_not_found"));
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/reminders").json(&json!({
            "text": "past", "dueAt": "2020-01-01T00:00:00Z"
        })))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, "due_in_past"));
    let (s, b, _) = h
        .send(h.req(Method::POST, "/v1/reminders").json(&json!({
            "text": "disabled", "dueAt": "2030-01-01T00:00:00Z"
        })))
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, "reminders_disabled")
    );
}

#[tokio::test]
async fn lucina_5xx_auth_and_disabled_become_502() {
    let h = Harness::default().await;
    for path in ["/v1/jobs/boom", "/v1/jobs/denied", "/v1/jobs/disabled"] {
        let (s, b) = h.get(path).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "{path}");
        assert_eq!(code(&b), "lucina_error", "{path}");
    }
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
        .get(h.url("/v1/health"))
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
    let resp = h.http.get(h.url("/v1/health")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

// -- browser sessions ----------------------------------------------------------

#[tokio::test]
async fn session_login_cookie_flags_and_status() {
    let h = Harness::default().await;
    let (s, b, hd) = h
        .send(
            h.http
                .post(h.url("/v1/session"))
                .json(&json!({"token": h.token})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b,
        json!({"authenticated": true, "deviceName": "test iPhone"})
    );
    let cookie = hd["set-cookie"].to_str().unwrap();
    assert!(cookie.starts_with("sec_session="));
    for flag in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=2592000",
    ] {
        assert!(cookie.contains(flag), "{cookie} lacks {flag}");
    }
    let sid = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("sec_session=");
    assert_eq!(sid.len(), 43, "32 random bytes, base64url");
    let raw = std::fs::read_to_string(h.data_dir.join("sessions.json")).unwrap();
    assert!(!raw.contains(sid), "only the hash is stored");
    assert!(raw.contains(&h.device_id));

    let (s, b, _) = h.send(h.browser(Method::GET, "/v1/session", sid)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b,
        json!({"authenticated": true, "deviceName": "test iPhone"})
    );
    let (_, b, _) = h.send(h.http.get(h.url("/v1/session"))).await;
    assert_eq!(b, json!({"authenticated": false, "deviceName": null}));
    let (_, b, _) = h.send(h.req(Method::GET, "/v1/session")).await;
    assert_eq!(b["authenticated"], true, "Bearer counts too");

    // bad tokens
    let (s, b, hd) = h
        .send(
            h.http
                .post(h.url("/v1/session"))
                .json(&json!({"token": "sec_wrong"})),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::UNAUTHORIZED, "unauthorized"));
    assert!(!hd.contains_key("set-cookie"));
    let (s, _, _) = h
        .send(h.http.post(h.url("/v1/session")).json(&json!({"nope": 1})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cookie_auth_on_v1_routes_with_csrf_for_writes() {
    let h = Harness::default().await;
    let sid = h.login().await;

    // reads work with the cookie alone
    let (s, b, _) = h.send(h.browser(Method::GET, "/v1/shopping", &sid)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(b.is_array());

    // writes need X-Secretary-CSRF: 1
    let write = |csrf: Option<&str>| {
        let mut rb = h
            .browser(Method::POST, "/v1/shopping", &sid)
            .json(&json!({"rawName": "pane"}));
        if let Some(v) = csrf {
            rb = rb.header("x-secretary-csrf", v);
        }
        rb
    };
    let before = h.mock.count();
    let (s, b, _) = h.send(write(None)).await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "csrf"));
    let (s, _, _) = h.send(write(Some("0"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h.mock.count(), before, "rejected before Lucina");
    let (s, _, _) = h.send(write(Some("1"))).await;
    assert_eq!(s, StatusCode::CREATED);

    // Bearer writes are exempt
    let (s, _, _) = h
        .send(
            h.req(Method::POST, "/v1/shopping")
                .json(&json!({"rawName": "pane"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);

    // garbage cookie
    let (s, b, _) = h.send(h.browser(Method::GET, "/v1/shopping", "nope")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["error"]["message"], "session expired or invalid");
}

#[tokio::test]
async fn logout_and_revoke_end_sessions() {
    let h = Harness::default().await;
    let sid = h.login().await;

    // logout needs CSRF too, then clears the cookie and the session
    let (s, b, _) = h.send(h.browser(Method::DELETE, "/v1/session", &sid)).await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "csrf"));
    let (s, b, hd) = h
        .send(
            h.browser(Method::DELETE, "/v1/session", &sid)
                .header("x-secretary-csrf", "1"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["authenticated"], false);
    assert!(hd["set-cookie"].to_str().unwrap().contains("Max-Age=0"));
    let (s, _, _) = h.send(h.browser(Method::GET, "/v1/shopping", &sid)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // revoking the device kills a live session immediately
    let sid = h.login().await;
    assert_eq!(
        h.send(h.browser(Method::GET, "/v1/shopping", &sid)).await.0,
        StatusCode::OK
    );
    let (_, n) = secretary_server::sessions::revoke_device(&h.data_dir, &h.device_id)
        .unwrap()
        .unwrap();
    assert_eq!(n, 1);
    let (s, _, _) = h.send(h.browser(Method::GET, "/v1/shopping", &sid)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h
        .send(
            h.http
                .post(h.url("/v1/session"))
                .json(&json!({"token": h.token})),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "revoked token cannot log in");
}

#[tokio::test]
async fn revoked_device_session_is_dead_even_if_still_on_disk() {
    let h = Harness::default().await;
    let sid = h.login().await;
    h.store.revoke(&h.device_id).unwrap().unwrap(); // sessions.json untouched
    let (s, _, _) = h.send(h.browser(Method::GET, "/v1/shopping", &sid)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_is_rate_limited_per_ip() {
    let h = Harness::default().await;
    let attempt = |xff: Option<&str>| {
        let mut rb = h
            .http
            .post(h.url("/v1/session"))
            .json(&json!({"token": "sec_wrong"}));
        if let Some(ip) = xff {
            rb = rb.header("x-forwarded-for", ip);
        }
        rb
    };
    for _ in 0..5 {
        assert_eq!(h.send(attempt(None)).await.0, StatusCode::UNAUTHORIZED);
    }
    let (s, b, hd) = h.send(attempt(None)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(hd.contains_key("retry-after"));
    // without SECRETARY_TRUST_PROXY, X-Forwarded-For is ignored
    assert_eq!(
        h.send(attempt(Some("203.0.113.9"))).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn login_rate_limit_uses_forwarded_ip_behind_proxy() {
    let h = Harness::new(|c, _| c.trust_proxy = true).await;
    let attempt = |xff: &str| {
        h.http
            .post(h.url("/v1/session"))
            .header("x-forwarded-for", xff)
            .json(&json!({"token": "sec_wrong"}))
    };
    for _ in 0..5 {
        assert_eq!(
            h.send(attempt("198.51.100.1, 203.0.113.7")).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // the right-most hop (appended by our proxy) is the key
    assert_eq!(
        h.send(attempt("10.0.0.1, 203.0.113.7")).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.send(attempt("203.0.113.8")).await.0,
        StatusCode::UNAUTHORIZED,
        "another client has its own bucket"
    );
}

// -- web app + headers -------------------------------------------------------------

fn web_dir(root: &std::path::Path) -> std::path::PathBuf {
    let web = root.join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<!doctype html><title>Secretary</title>",
    )
    .unwrap();
    std::fs::write(web.join("assets/app-3f2a1c.js"), "console.log(1)").unwrap();
    std::fs::write(web.join("robots.txt"), "User-agent: *").unwrap();
    web
}

#[tokio::test]
async fn serves_web_app_with_spa_fallback_and_cache_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let web = web_dir(tmp.path());
    let h = Harness::new(move |c, _| c.web_dir = Some(web)).await;
    let get = |p: &str| h.http.get(h.url(p)).send();

    let r = get("/").await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.headers()["cache-control"], "no-cache");
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert!(r.text().await.unwrap().contains("Secretary"));

    let r = get("/assets/app-3f2a1c.js").await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );

    let r = get("/robots.txt").await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.headers()["cache-control"], "no-cache");

    // client-side routes get index.html
    for p in ["/agenda", "/reminders/r1"] {
        let r = get(p).await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "{p}");
        assert_eq!(r.headers()["cache-control"], "no-cache");
        assert!(r.text().await.unwrap().contains("Secretary"));
    }
    // missing files and API paths do not
    assert_eq!(
        get("/assets/missing.js").await.unwrap().status().as_u16(),
        404
    );
    assert_eq!(get("/logo.png").await.unwrap().status().as_u16(), 404);
    let r = get("/v1/nope").await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let b: Value = r.json().await.unwrap();
    assert_eq!(code(&b), "not_found");
    // path traversal stays inside the web dir
    let r = get("/..%2F..%2Fetc%2Fpasswd").await.unwrap();
    assert!(!r.text().await.unwrap().contains("root:"));
    // non-GET outside /v1 is not served
    let r = h.http.post(h.url("/agenda")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[tokio::test]
async fn security_headers_on_every_response() {
    let tmp = tempfile::tempdir().unwrap();
    let web = web_dir(tmp.path());
    let h = Harness::new(move |c, _| c.web_dir = Some(web)).await;
    const STRICT: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";
    // The web app (3D stage: WebAssembly decoder, blob textures/audio/workers).
    // `WEB` mirrors `WEB_CONTENT_SECURITY_POLICY`: the app's own policy plus
    // exactly what Google's allowlist CSP guide requires for the Maps
    // JavaScript API (deals map).
    const WEB: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-eval' blob: https://*.googleapis.com https://*.gstatic.com *.google.com https://*.ggpht.com *.googleusercontent.com; img-src 'self' data: blob: https://*.googleapis.com https://*.gstatic.com *.google.com *.googleusercontent.com; connect-src 'self' blob: data: https://*.googleapis.com *.google.com https://*.gstatic.com; media-src 'self' blob:; worker-src 'self' blob:; manifest-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; frame-ancestors 'none'; frame-src *.google.com; base-uri 'none'";
    let responses = [
        (
            STRICT,
            h.http.get(h.url("/v1/health")).send().await.unwrap(),
        ),
        (
            STRICT,
            h.http.get(h.url("/v1/shopping")).send().await.unwrap(),
        ), // 401
        (STRICT, h.http.get(h.url("/v1/nope")).send().await.unwrap()), // 404
        (
            STRICT,
            h.http
                .get(h.url("/assets/missing.js"))
                .send()
                .await
                .unwrap(),
        ), // web 404 (JSON)
        (WEB, h.http.get(h.url("/")).send().await.unwrap()),           // index.html
        (WEB, h.http.get(h.url("/agenda")).send().await.unwrap()),     // SPA fallback
        (
            WEB,
            h.http
                .get(h.url("/assets/app-3f2a1c.js"))
                .send()
                .await
                .unwrap(),
        ), // asset
        (
            STRICT,
            h.req(Method::GET, "/v1/shopping").send().await.unwrap(),
        ), // 200 forwarded
    ];
    for (csp, r) in responses {
        let hd = r.headers();
        assert_eq!(hd["content-security-policy"], csp, "{}", r.url());
        assert_eq!(hd.get_all("content-security-policy").iter().count(), 1);
        assert_eq!(hd["x-content-type-options"], "nosniff");
        assert_eq!(hd["referrer-policy"], "no-referrer");
        assert!(!hd.contains_key("access-control-allow-origin"));
    }
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
    let url = h.url("/v1/upstream/lists/u1");
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

    // the issued token authenticates against a server on that data dir,
    // both as Bearer and through a browser session
    let mut cfg = Config::new("http://127.0.0.1:9", None);
    cfg.data_dir = data.clone();
    let base = spawn_app(AppState::new(&cfg).unwrap()).await;
    let http = reqwest::Client::new();
    let login = http
        .post(format!("{base}/v1/session"))
        .json(&json!({"token": token}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status().as_u16(), 200);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let bearer = |t: String| {
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
    let browser = || {
        let http = http.clone();
        let url = format!("{base}/v1/shopping");
        let cookie = cookie.clone();
        async move {
            http.get(url)
                .header("cookie", cookie)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(
        bearer(token.clone()).await,
        503,
        "authenticated; Lucina not configured"
    );
    assert_eq!(browser().await, 503);

    let (ok, out) = cli(&data, &["token", "revoke", &id]);
    assert!(ok, "{out}");
    assert!(out.contains("1 browser session(s) ended"), "{out}");
    let (_, out) = cli(&data, &["token", "list"]);
    assert!(out.contains("revoked"));
    assert_eq!(bearer(token).await, 401);
    assert_eq!(browser().await, 401);
    let sessions = std::fs::read_to_string(data.join("sessions.json")).unwrap();
    assert!(
        !sessions.contains(&id),
        "revoke deleted the device's sessions"
    );

    let (ok, _) = cli(&data, &["token", "revoke", "dev_nope"]);
    assert!(!ok);
}

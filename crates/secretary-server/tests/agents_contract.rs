//! `/v1/agents/*` against the contract (`docs/design/agents-api.md`), with a mock
//! Lucina that echoes what reached `/api/secretary/agentd/*`: the daemon path each
//! public path maps to, the origin this server sets for each kind of caller, the
//! headers passed on, auth negatives, the broker's and the MCP's limits, error and
//! stream pass-through. (The same routes against the real daemon:
//! `tests/agents.rs` on the agents branch line.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::StreamExt;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const LUCINA_TOKEN: &str = "lucina-server-secret";
const MCP_TOKEN: &str = "svc_mcp_agents_0123456789abcdefghijklmnopqrstuv";
const BROKER_TOKEN: &str = "svc_broker_0123456789abcdefghijklmnopqrstuvwxyz";
const LUCINA_SERVICE_TOKEN: &str = "svc_lucina_0123456789abcdefghijklmnopqrstuvwxyz";

#[derive(Debug, Clone, Default)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    origin: Option<String>,
    depth: Option<String>,
    last_event_id: Option<String>,
    other_lucina: Vec<String>,
    body: String,
}

#[derive(Default)]
struct Lucina {
    seen: Mutex<Vec<Seen>>,
    refuse_token: AtomicBool,
}

async fn lucina(State(l): State<Arc<Lucina>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let seen = Seen {
        method: method.to_string(),
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        origin: h("x-lucina-origin"),
        depth: h("x-lucina-delegation-depth"),
        last_event_id: h("last-event-id"),
        other_lucina: headers
            .keys()
            .map(|k| k.as_str().to_owned())
            .filter(|k| k.starts_with("x-lucina-") && k != "x-lucina-origin" && k != "x-lucina-delegation-depth")
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    l.seen.lock().unwrap().push(seen.clone());
    if l.refuse_token.load(Ordering::SeqCst) || h("authorization").as_deref() != Some(&format!("Bearer {LUCINA_TOKEN}")) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "unauthorized"}))).into_response();
    }
    match seen.path.as_str() {
        "/api/secretary/agentd/stream" => {
            let first = format!("id: 101\ndata: {{\"type\":\"agent_state\",\"after\":\"{}\"}}\n\n", seen.last_event_id.unwrap_or_default());
            let events = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(first))])
                .chain(futures_util::stream::pending());
            ([("content-type", "text/event-stream")], Body::from_stream(events)).into_response()
        }
        "/api/secretary/agentd/terminals/term_1/stream" => {
            let first = format!("event: output\nid: 64\ndata: {{\"b64\":\"aGk=\",\"after\":\"{}\"}}\n\n", seen.last_event_id.unwrap_or_default());
            let events = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(first))]).chain(futures_util::stream::pending());
            ([("content-type", "text/event-stream")], Body::from_stream(events)).into_response()
        }
        p if p.starts_with("/api/secretary/agentd/workspaces/wks_1/services/web/") => (
            [
                ("content-type", "application/javascript"),
                ("etag", "\"abc\""),
                ("set-cookie", "dev=1; Path=/"),
                ("location", "/v1/agents/workspaces/wks_1/services/web/next"),
                ("x-dev-secret", "nope"),
            ],
            format!("// {p} if-none-match={}", h("if-none-match").unwrap_or_default()),
        )
            .into_response(),
        "/api/secretary/agentd/agents/missing" => {
            (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "AGENT_NOT_FOUND", "message": "No agent found matching: missing"}})))
                .into_response()
        }
        _ => (StatusCode::OK, Json(json!({"echo": seen.path}))).into_response(),
    }
}

struct H {
    base: String,
    http: reqwest::Client,
    mac: String,
    mac_id: String,
    phone_id: String,
    mcp_device: String,
    lucina: Arc<Lucina>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let lucina = Arc::new(Lucina::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(self::lucina).with_state(lucina.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("server");
    let store = DeviceStore::new(&cfg.data_dir);
    let (mac_dev, mac) = store.issue("MacBook").unwrap();
    let (phone_dev, _) = store.issue("iPhone").unwrap();
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
    H { base, http: reqwest::Client::new(), mac, mac_id: mac_dev.id, phone_id: phone_dev.id, mcp_device, lucina, _dir: dir }
}

impl H {
    fn req(&self, method: Method, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{path}", self.base));
        match token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn last(&self) -> Seen {
        self.lucina.seen.lock().unwrap().last().cloned().unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.lucina.seen.lock().unwrap().len()
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn public_paths_map_to_daemon_paths() {
    let h = harness().await;
    for (method, public, daemon) in [
        (Method::GET, "/v1/agents", "/agents"),
        (Method::POST, "/v1/agents", "/agents"),
        (Method::GET, "/v1/agents/items", "/agents/items"),
        (Method::GET, "/v1/agents/0b6e", "/agents/0b6e"),
        (Method::PATCH, "/v1/agents/0b6e", "/agents/0b6e"),
        (Method::DELETE, "/v1/agents/0b6e", "/agents/0b6e"),
        (Method::POST, "/v1/agents/0b6e/send", "/agents/0b6e/send"),
        (Method::POST, "/v1/agents/0b6e/stop", "/agents/0b6e/stop"),
        (Method::POST, "/v1/agents/0b6e/archive", "/agents/0b6e/archive"),
        (Method::POST, "/v1/agents/0b6e/wait", "/agents/0b6e/wait"),
        (Method::GET, "/v1/agents/0b6e/timeline", "/agents/0b6e/timeline"),
        (Method::POST, "/v1/agents/0b6e/permissions/r1", "/agents/0b6e/permissions/r1"),
        (Method::POST, "/v1/agents/0b6e/events", "/agents/0b6e/events"),
        (Method::POST, "/v1/agents/0b6e/commands/cmd_1/ack", "/agents/0b6e/commands/cmd_1/ack"),
        (Method::GET, "/v1/agents/commands", "/commands"),
        (Method::GET, "/v1/agents/permissions", "/permissions"),
        (Method::POST, "/v1/agents/permissions/ext_1", "/permissions/ext_1"),
        (Method::GET, "/v1/agents/projects", "/projects"),
        (Method::GET, "/v1/agents/workspaces/wks_1/diff", "/workspaces/wks_1/diff"),
        (Method::POST, "/v1/agents/clone", "/clone"),
        (Method::GET, "/v1/agents/providers", "/providers"),
    ] {
        let (status, v) = h.send(h.req(method.clone(), &format!("{public}?x=1"), Some(&h.mac)).json(&json!({"k": 1}))).await;
        assert_eq!(status, 200, "{method} {public}: {v}");
        let seen = h.last();
        assert_eq!(seen.path, format!("/api/secretary/agentd{daemon}"), "{method} {public}");
        assert_eq!((seen.method.as_str(), seen.query.as_deref()), (method.as_str(), Some("x=1")));
        assert_eq!(seen.origin, Some(format!("device:{}", h.mac_id)));
        assert_eq!(seen.body, r#"{"k":1}"#);
    }
    let before = h.count();
    for bad in ["/v1/agents//x", "/v1/agents/a$b", "/v1/agents/a,b", "/v1/agentsx"] {
        let (status, _) = h.send(h.req(Method::GET, bad, Some(&h.mac))).await;
        assert_eq!(status, 404, "{bad}");
    }
    assert_eq!(h.count(), before, "bad paths never leave the server");
    // Daemon errors pass through with their status.
    let (status, v) = h.send(h.req(Method::GET, "/v1/agents/missing", Some(&h.mac))).await;
    assert_eq!((status, code(&v)), (404, "AGENT_NOT_FOUND"));
}

#[tokio::test]
async fn auth_negatives() {
    let h = harness().await;
    let (status, v) = h.send(h.req(Method::GET, "/v1/agents", None)).await;
    assert_eq!((status, code(&v)), (401, "unauthorized"));
    let (status, _) = h.send(h.req(Method::GET, "/v1/agents", Some("sec_not_a_real_token_000000000000000"))).await;
    assert_eq!(status, 401);
    let (status, _) = h.send(h.req(Method::GET, "/v1/agents", Some("svc_unknown_0123456789abcdefghijklmnopqrstuvw"))).await;
    assert_eq!(status, 401);
    let (status, _) = h.send(h.req(Method::GET, "/v1/agents", Some(LUCINA_SERVICE_TOKEN))).await;
    assert_eq!(status, 403, "Lucina's service token is not an agents caller");
    assert_eq!(h.count(), 0, "nothing reached Lucina");
    // A web session: reads, and writes only with the CSRF header.
    let login = h.http.post(format!("{}/v1/session", h.base)).json(&json!({"token": h.mac})).send().await.unwrap();
    let cookie = login.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let (status, _) = h.send(h.http.get(format!("{}/v1/agents", h.base)).header("cookie", &cookie)).await;
    assert_eq!(status, 200);
    let (status, v) = h.send(h.http.post(format!("{}/v1/agents", h.base)).header("cookie", &cookie).json(&json!({}))).await;
    assert_eq!((status, code(&v)), (403, "csrf"));
    let (status, _) = h.send(h.http.post(format!("{}/v1/agents", h.base)).header("cookie", &cookie).header("x-secretary-csrf", "1").json(&json!({}))).await;
    assert_eq!(status, 200);
    // Lucina refusing the server's token is a 502, never the client's 401.
    h.lucina.refuse_token.store(true, Ordering::SeqCst);
    let (status, v) = h.send(h.req(Method::GET, "/v1/agents", Some(&h.mac))).await;
    assert_eq!((status, code(&v)), (502, "lucina_auth"));
}

#[tokio::test]
async fn origins_by_caller_and_no_client_lucina_headers() {
    let h = harness().await;
    let cases: Vec<(reqwest::RequestBuilder, String)> = vec![
        (h.req(Method::GET, "/v1/agents", Some(&h.mac)).header("x-lucina-origin", format!("device:{}", h.phone_id)), format!("device:{}", h.mac_id)),
        (h.req(Method::GET, "/v1/agents", Some(&h.mac)).header("x-lucina-origin", "local"), format!("device:{}", h.mac_id)),
        (h.req(Method::GET, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-mcp-caller", "tok_Ab9:hermes"), "mcp:tok_Ab9".into()),
        (h.req(Method::GET, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-origin", "device:spoof"), "mcp:unknown".into()),
        (h.req(Method::GET, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-mcp-caller", "bad caller"), "mcp:unknown".into()),
        (h.req(Method::GET, "/v1/agents", Some(&h.mcp_device)), "mcp:device".into()),
        (h.req(Method::GET, "/v1/agents", Some(&h.mac)).header("x-secretary-caller", "mcp:claude"), "mcp:claude".into()),
        (h.req(Method::POST, "/v1/agents/permissions", Some(BROKER_TOKEN)).json(&json!({})), "service:infisical-broker".into()),
    ];
    for (rb, want) in cases {
        let (status, v) = h.send(rb.header("x-lucina-anything", "1")).await;
        assert_eq!(status, 200, "{want}: {v}");
        let seen = h.last();
        assert_eq!(seen.origin.as_deref(), Some(want.as_str()));
        assert!(seen.other_lucina.is_empty(), "{:?}", seen.other_lucina);
    }
    // The delegation depth passes when it is 0 or 1.
    for (depth, passed) in [("1", Some("1")), ("0", Some("0")), ("7", None), ("x", None)] {
        h.send(h.req(Method::POST, "/v1/agents", Some(MCP_TOKEN)).header("x-lucina-delegation-depth", depth).json(&json!({}))).await;
        assert_eq!(h.last().depth.as_deref(), passed, "{depth}");
    }
}

#[tokio::test]
async fn the_broker_only_asks_and_the_mcp_never_answers() {
    let h = harness().await;
    for (method, path, ok) in [
        (Method::POST, "/v1/agents/permissions", true),
        (Method::GET, "/v1/agents/permissions/ext_1?waitMs=1000", true),
        (Method::GET, "/v1/agents/permissions", false),
        (Method::POST, "/v1/agents/permissions/ext_1", false),
        (Method::GET, "/v1/agents", false),
        (Method::POST, "/v1/agents", false),
        (Method::GET, "/v1/agents/stream", false),
        (Method::POST, "/v1/agents/a/permissions/r", false),
    ] {
        let (status, _) = h.send(h.req(method.clone(), path, Some(BROKER_TOKEN)).json(&json!({}))).await;
        assert_eq!(status == 200, ok, "broker {method} {path}: {status}");
    }
    let before = h.count();
    for token in [MCP_TOKEN, h.mcp_device.as_str()] {
        for path in ["/v1/agents/permissions/ext_1", "/v1/agents/a/permissions/r1"] {
            let (status, v) = h.send(h.req(Method::POST, path, Some(token)).json(&json!({"behavior": "allow"}))).await;
            assert_eq!(status, 403, "{path}: {v}");
        }
    }
    assert_eq!(h.count(), before, "refused before Lucina");
    let (status, _) = h.send(h.req(Method::POST, "/v1/agents/permissions/ext_1", Some(&h.mac)).json(&json!({"behavior": "allow"}))).await;
    assert_eq!(status, 200, "an owner device answers");
}

/// What a stream sends, up to and including `needle` (5 s at most).
async fn read_until(body: &mut (impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin), needle: &str) -> String {
    let mut text = String::new();
    while !text.contains(needle) {
        let chunk = tokio::time::timeout(Duration::from_secs(5), body.next()).await.expect(&text).unwrap().unwrap();
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}

#[tokio::test]
async fn streams_pass_last_event_id_and_are_limited() {
    let h = harness().await;
    let resp = h.req(Method::GET, "/v1/agents/stream", Some(&h.mac)).header("last-event-id", "100").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    let mut body = resp.bytes_stream();
    let text = read_until(&mut body, "id: 101").await;
    // The comment comes first, before anything the daemon has to say (STREAM_OPEN explains why).
    assert!(text.starts_with(": connected\n\n"), "{text}");
    assert!(text.contains("\"after\":\"100\""), "{text}");
    // `?lastEventId=` passes as it is.
    h.req(Method::GET, "/v1/agents/stream?lastEventId=55", Some(&h.mac)).send().await.unwrap();
    assert_eq!(h.last().query.as_deref(), Some("lastEventId=55"));
    // Four per caller: the fifth is refused while the others are open.
    let mut open = vec![body];
    for _ in 0..3 {
        open.push(h.req(Method::GET, "/v1/agents/stream", Some(&h.mac)).send().await.unwrap().bytes_stream());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, v) = h.send(h.req(Method::GET, "/v1/agents/stream", Some(&h.mac))).await;
    assert_eq!((status, code(&v)), (429, "too_many_streams"));
    drop(open);
}

#[tokio::test]
async fn terminal_streams_and_owner_only_services() {
    let h = harness().await;
    // A terminal's output streams like /v1/agents/stream, with Last-Event-ID.
    let resp = h.req(Method::GET, "/v1/agents/terminals/term_1/stream", Some(&h.mac)).header("last-event-id", "10").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    let mut body = resp.bytes_stream();
    let text = read_until(&mut body, "\"after\":\"10\"").await;
    assert!(text.starts_with(": connected\n\n"), "{text}");
    drop(body);
    for (public, daemon) in [
        ("/v1/agents/terminals?workspaceId=wks_1", "/api/secretary/agentd/terminals"),
        ("/v1/agents/terminals/term_1/capture", "/api/secretary/agentd/terminals/term_1/capture"),
        ("/v1/agents/workspaces/wks_1/scripts", "/api/secretary/agentd/workspaces/wks_1/scripts"),
    ] {
        let (status, _) = h.send(h.req(Method::GET, public, Some(MCP_TOKEN)).header("x-lucina-mcp-caller", "tok_1:hermes")).await;
        assert_eq!(status, 200, "{public}");
        assert_eq!(h.last().path, daemon);
    }

    // A service: the owner's device gets it sandboxed, without its cookies or other headers.
    let r = h
        .req(Method::GET, "/v1/agents/workspaces/wks_1/services/web/assets/index-a+b=.js", Some(&h.mac))
        .header("if-none-match", "\"abc\"")
        .header("cookie", "secretary_session=s3cret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-security-policy"].to_str().unwrap().starts_with("sandbox allow-scripts"));
    assert_eq!(r.headers()["etag"], "\"abc\"");
    assert_eq!(r.headers()["location"], "/v1/agents/workspaces/wks_1/services/web/next");
    assert!(r.headers().get("set-cookie").is_none() && r.headers().get("x-dev-secret").is_none());
    let text = r.text().await.unwrap();
    assert!(text.contains("/services/web/assets/index-a+b=.js") && text.contains("if-none-match=\"abc\""), "{text}");
    assert_eq!(h.last().origin.as_deref(), Some(format!("device:{}", h.mac_id).as_str()));
    let (status, _) = h.send(h.req(Method::GET, "/v1/agents/workspaces/wks_1/services/web/", Some(&h.mac))).await;
    assert_eq!(status, 200, "a trailing slash is the app's root");
    // Not for the MCP.
    let before = h.count();
    let (status, _) = h.send(h.req(Method::GET, "/v1/agents/workspaces/wks_1/services/web/", Some(MCP_TOKEN)).header("x-lucina-mcp-caller", "tok_1:hermes")).await;
    assert_eq!(status, 403);
    // (Dot segments are refused by `daemon_path`'s unit tests: an HTTP client resolves them first.)
    assert_eq!(h.count(), before, "refused before Lucina");
}

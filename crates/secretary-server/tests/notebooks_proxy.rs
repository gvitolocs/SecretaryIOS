//! `/v1/notebooks/*` against the contract (`docs/design/notebooks-api.md`), with a mock
//! Lucina that records what reached `/api/secretary/notebooks*`: the path each public
//! path maps to, the origin this server sets for each kind of caller, the headers passed
//! each way, auth negatives, the body limits (a long upload streams through, a long JSON
//! body is refused), SSE and Range pass-through.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::StreamExt;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const LUCINA_TOKEN: &str = "lucina-server-secret";
const AGENTS_MCP_TOKEN: &str = "svc_mcp_agents_0123456789abcdefghijklmnopqrstuv";
const LUCINA_SERVICE_TOKEN: &str = "svc_lucina_0123456789abcdefghijklmnopqrstuvwxyz";
const MIB: usize = 1024 * 1024;

#[derive(Debug, Clone, Default)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    accept: Option<String>,
    range: Option<String>,
    last_event_id: Option<String>,
    authorization: Option<String>,
    leaked: Vec<String>,
    body_len: usize,
    body_sum: u64,
    body: String,
}

#[derive(Default)]
struct Lucina {
    seen: Mutex<Vec<Seen>>,
    refuse_token: AtomicBool,
}

const AUDIO: &[u8] = b"ID3-fake-mp3-bytes";

async fn lucina(
    State(l): State<Arc<Lucina>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let seen = Seen {
        method: method.to_string(),
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        origin: h("x-lucina-origin"),
        content_type: h("content-type"),
        accept: h("accept"),
        range: h("range"),
        last_event_id: h("last-event-id"),
        authorization: h("authorization"),
        leaked: [
            "cookie",
            "x-secretary-csrf",
            "x-secretary-caller",
            "x-lucina-mcp-caller",
            "x-lucina-delegation-depth",
            "if-none-match",
        ]
        .into_iter()
        .filter(|n| headers.contains_key(*n))
        .map(str::to_owned)
        .collect(),
        body_len: body.len(),
        body_sum: body.iter().map(|b| u64::from(*b)).sum(),
        body: if body.len() < 4096 {
            String::from_utf8_lossy(&body).into_owned()
        } else {
            String::new()
        },
    };
    l.seen.lock().unwrap().push(seen.clone());
    if l.refuse_token.load(Ordering::SeqCst)
        || seen.authorization.as_deref() != Some(&format!("Bearer {LUCINA_TOKEN}"))
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "unauthorized"})),
        )
            .into_response();
    }
    match seen.path.as_str() {
        "/api/secretary/notebooks/chats/c0123456789/messages"
            if seen.accept.as_deref() == Some("text/event-stream") =>
        {
            let first = format!(
                "event: delta\nid: 1\ndata: {{\"text\":\"Hi\",\"after\":\"{}\"}}\n\n",
                seen.last_event_id.unwrap_or_default()
            );
            let events =
                futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(first))])
                    .chain(futures_util::stream::pending());
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(events),
            )
                .into_response()
        }
        "/api/secretary/notebooks/podcasts/p0123456789/audio" => {
            let (status, slice, extra) = match seen.range.as_deref() {
                Some("bytes=4-8") => (
                    StatusCode::PARTIAL_CONTENT,
                    &AUDIO[4..9],
                    Some(("content-range", "bytes 4-8/18")),
                ),
                _ => (StatusCode::OK, AUDIO, None),
            };
            let mut r = (
                status,
                [
                    ("content-type", "audio/mpeg"),
                    ("accept-ranges", "bytes"),
                    ("cache-control", "private, max-age=3600"),
                    ("set-cookie", "lucina=1; Path=/"),
                    ("x-internal", "nope"),
                ],
                slice.to_vec(),
            )
                .into_response();
            if let Some((k, v)) = extra {
                r.headers_mut().insert(k, v.parse().unwrap());
            }
            r
        }
        "/api/secretary/notebooks/sources/s0a9be3c112/file" => (
            [
                ("content-type", "application/pdf"),
                (
                    "content-disposition",
                    "attachment; filename=\"RM Class 2.pdf\"",
                ),
            ],
            "%PDF-1.7",
        )
            .into_response(),
        "/api/secretary/notebooks/nb0000000000" => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "not_found", "message": "no such notebook"}})),
        )
            .into_response(),
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
    let router = Router::new()
        .fallback(self::lucina)
        .layer(DefaultBodyLimit::disable())
        .with_state(lucina.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("server");
    let store = DeviceStore::new(&cfg.data_dir);
    let (mac_dev, mac) = store.issue("MacBook").unwrap();
    let (phone_dev, _) = store.issue("iPhone").unwrap();
    let (mcp_dev, mcp_device) = store.issue("secretary-mcp").unwrap();
    cfg.service_token = Some(ServiceToken::from_secret(LUCINA_SERVICE_TOKEN).unwrap());
    cfg.agents.mcp_token = Some(ServiceToken::from_secret(AGENTS_MCP_TOKEN).unwrap());
    cfg.agents.mcp_devices = vec![mcp_dev.id.clone()];
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = app(state);
    tokio::spawn(async move {
        serve_on(listener, router, std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: reqwest::Client::new(),
        mac,
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

    async fn send(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn last(&self) -> Seen {
        self.lucina
            .seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.lucina.seen.lock().unwrap().len()
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn public_paths_map_one_to_one() {
    let h = harness().await;
    for (method, public, upstream) in [
        (Method::GET, "/v1/notebooks", "/api/secretary/notebooks"),
        (Method::POST, "/v1/notebooks", "/api/secretary/notebooks"),
        (Method::GET, "/v1/notebooks/", "/api/secretary/notebooks"),
        (
            Method::GET,
            "/v1/notebooks/nb4f1c09a2e7",
            "/api/secretary/notebooks/nb4f1c09a2e7",
        ),
        (
            Method::PATCH,
            "/v1/notebooks/nb4f1c09a2e7",
            "/api/secretary/notebooks/nb4f1c09a2e7",
        ),
        (
            Method::DELETE,
            "/v1/notebooks/nb4f1c09a2e7",
            "/api/secretary/notebooks/nb4f1c09a2e7",
        ),
        (
            Method::POST,
            "/v1/notebooks/nb4f1c09a2e7/sources",
            "/api/secretary/notebooks/nb4f1c09a2e7/sources",
        ),
        (
            Method::PATCH,
            "/v1/notebooks/nb4f1c09a2e7/sources/s0a9be3c112",
            "/api/secretary/notebooks/nb4f1c09a2e7/sources/s0a9be3c112",
        ),
        (
            Method::GET,
            "/v1/notebooks/sources/s0a9be3c112/chunks",
            "/api/secretary/notebooks/sources/s0a9be3c112/chunks",
        ),
        (
            Method::POST,
            "/v1/notebooks/sources/s0a9be3c112/insights",
            "/api/secretary/notebooks/sources/s0a9be3c112/insights",
        ),
        (
            Method::POST,
            "/v1/notebooks/nb4f1c09a2e7/notes",
            "/api/secretary/notebooks/nb4f1c09a2e7/notes",
        ),
        (
            Method::DELETE,
            "/v1/notebooks/nb4f1c09a2e7/notes/rm-class-2-summary",
            "/api/secretary/notebooks/nb4f1c09a2e7/notes/rm-class-2-summary",
        ),
        (
            Method::GET,
            "/v1/notebooks/nb4f1c09a2e7/search",
            "/api/secretary/notebooks/nb4f1c09a2e7/search",
        ),
        (
            Method::GET,
            "/v1/notebooks/search",
            "/api/secretary/notebooks/search",
        ),
        (
            Method::POST,
            "/v1/notebooks/chats/c0123456789/messages",
            "/api/secretary/notebooks/chats/c0123456789/messages",
        ),
        (
            Method::POST,
            "/v1/notebooks/chats/c0123456789/messages/m0123456789/note",
            "/api/secretary/notebooks/chats/c0123456789/messages/m0123456789/note",
        ),
        (
            Method::PATCH,
            "/v1/notebooks/transformations/t-dense-summary",
            "/api/secretary/notebooks/transformations/t-dense-summary",
        ),
        (
            Method::GET,
            "/v1/notebooks/podcast-profiles",
            "/api/secretary/notebooks/podcast-profiles",
        ),
        (
            Method::POST,
            "/v1/notebooks/nb4f1c09a2e7/podcasts",
            "/api/secretary/notebooks/nb4f1c09a2e7/podcasts",
        ),
        (
            Method::GET,
            "/v1/notebooks/health",
            "/api/secretary/notebooks/health",
        ),
    ] {
        let (status, v) = h
            .send(
                h.req(
                    method.clone(),
                    &format!("{public}?x=1&q=a%20b"),
                    Some(&h.mac),
                )
                .json(&json!({"k": 1})),
            )
            .await;
        assert_eq!(status, 200, "{method} {public}: {v}");
        let seen = h.last();
        assert_eq!(seen.path, upstream, "{method} {public}");
        let bodies = matches!(method, Method::POST | Method::PATCH);
        assert_eq!(
            (seen.method.as_str(), seen.query.as_deref()),
            (method.as_str(), Some("x=1&q=a%20b"))
        );
        assert_eq!(seen.origin, Some(format!("device:{}", h.mac_id)));
        assert_eq!(
            seen.body,
            if bodies { r#"{"k":1}"# } else { "" },
            "{method} {public}"
        );
        assert!(seen
            .content_type
            .as_deref()
            .is_some_and(|c| c.starts_with("application/json")));
    }
    let before = h.count();
    for bad in [
        "/v1/notebooks//x",
        "/v1/notebooks/nb1/a$b",
        "/v1/notebooks/nb1/a,b",
        "/v1/notebooksx",
        "/v1/notebooks/nb1/",
    ] {
        let (status, _) = h.send(h.req(Method::GET, bad, Some(&h.mac))).await;
        assert_eq!(status, 404, "{bad}");
    }
    assert_eq!(h.count(), before, "bad paths never leave the server");
    // Lucina's errors pass through with their status.
    let (status, v) = h
        .send(h.req(Method::GET, "/v1/notebooks/nb0000000000", Some(&h.mac)))
        .await;
    assert_eq!((status, code(&v)), (404, "not_found"));
}

#[tokio::test]
async fn auth_negatives() {
    let h = harness().await;
    let (status, v) = h.send(h.req(Method::GET, "/v1/notebooks", None)).await;
    assert_eq!((status, code(&v)), (401, "unauthorized"));
    for token in [
        "sec_not_a_real_token_000000000000000",
        AGENTS_MCP_TOKEN,
        LUCINA_SERVICE_TOKEN,
    ] {
        let (status, _) = h
            .send(h.req(Method::GET, "/v1/notebooks", Some(token)))
            .await;
        assert_eq!(
            status, 401,
            "{token}: service tokens are not notebooks callers"
        );
    }
    assert_eq!(h.count(), 0, "nothing reached Lucina");
    // A web session: reads, and writes only with the CSRF header.
    let login = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.mac}))
        .send()
        .await
        .unwrap();
    let cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (status, _) = h
        .send(
            h.http
                .get(format!("{}/v1/notebooks", h.base))
                .header("cookie", &cookie),
        )
        .await;
    assert_eq!(status, 200);
    let (status, v) = h
        .send(
            h.http
                .post(format!("{}/v1/notebooks", h.base))
                .header("cookie", &cookie)
                .json(&json!({"name": "CNS"})),
        )
        .await;
    assert_eq!((status, code(&v)), (403, "csrf"));
    let (status, _) = h
        .send(
            h.http
                .post(format!("{}/v1/notebooks", h.base))
                .header("cookie", &cookie)
                .header("x-secretary-csrf", "1")
                .json(&json!({"name": "CNS"})),
        )
        .await;
    assert_eq!(status, 200);
    assert!(
        h.last().leaked.is_empty(),
        "no cookie or CSRF header reaches Lucina: {:?}",
        h.last().leaked
    );
    // Lucina refusing the server's token is a 502, never the client's 401.
    h.lucina.refuse_token.store(true, Ordering::SeqCst);
    let (status, v) = h
        .send(h.req(Method::GET, "/v1/notebooks", Some(&h.mac)))
        .await;
    assert_eq!((status, code(&v)), (502, "lucina_auth"));
}

#[tokio::test]
async fn origins_by_caller_and_no_client_headers() {
    let h = harness().await;
    let cases: Vec<(reqwest::RequestBuilder, String)> = vec![
        (
            h.req(Method::GET, "/v1/notebooks", Some(&h.mac))
                .header("x-lucina-origin", format!("device:{}", h.phone_id)),
            format!("device:{}", h.mac_id),
        ),
        (
            h.req(Method::GET, "/v1/notebooks", Some(&h.mac))
                .header("x-lucina-origin", "local"),
            format!("device:{}", h.mac_id),
        ),
        (
            h.req(Method::GET, "/v1/notebooks", Some(&h.mcp_device)),
            "mcp:device".into(),
        ),
        (
            h.req(Method::GET, "/v1/notebooks", Some(&h.mac))
                .header("x-secretary-caller", "mcp:claude"),
            "mcp:claude".into(),
        ),
        (
            h.req(Method::GET, "/v1/notebooks", Some(&h.mac))
                .header("x-secretary-caller", "mcp:Bad Name"),
            format!("device:{}", h.mac_id),
        ),
        (
            h.req(Method::POST, "/v1/notebooks", Some(&h.mac))
                .header("x-secretary-caller", "mcp:grok")
                .json(&json!({"name": "x"})),
            "mcp:grok".into(),
        ),
    ];
    for (rb, want) in cases {
        let (status, v) = h
            .send(
                rb.header("x-lucina-anything", "1")
                    .header("x-lucina-delegation-depth", "1")
                    .header("if-none-match", "\"a\""),
            )
            .await;
        assert_eq!(status, 200, "{want}: {v}");
        let seen = h.last();
        assert_eq!(seen.origin.as_deref(), Some(want.as_str()));
        assert!(
            seen.leaked.is_empty(),
            "only the four contract headers are passed on: {:?}",
            seen.leaked
        );
    }
}

#[tokio::test]
async fn headers_pass_both_ways() {
    let h = harness().await;
    // Request: Content-Type, Accept, Range, Last-Event-ID.
    let r = h
        .req(
            Method::GET,
            "/v1/notebooks/podcasts/p0123456789/audio",
            Some(&h.mac),
        )
        .header("range", "bytes=4-8")
        .header("accept", "audio/mpeg")
        .send()
        .await
        .unwrap();
    let seen = h.last();
    assert_eq!(
        (seen.range.as_deref(), seen.accept.as_deref()),
        (Some("bytes=4-8"), Some("audio/mpeg"))
    );
    // Response: Content-Type, Content-Length, Content-Range, Accept-Ranges, Cache-Control; nothing else.
    assert_eq!(r.status(), 206);
    let hd = r.headers().clone();
    assert_eq!(hd["content-type"], "audio/mpeg");
    assert_eq!(hd["content-range"], "bytes 4-8/18");
    assert_eq!(hd["accept-ranges"], "bytes");
    assert_eq!(hd["content-length"], "5");
    assert_eq!(hd["cache-control"], "private, max-age=3600");
    assert!(hd.get("set-cookie").is_none() && hd.get("x-internal").is_none());
    assert_eq!(r.bytes().await.unwrap().as_ref(), &AUDIO[4..9]);
    // Without Range: all of it.
    let r = h
        .req(
            Method::GET,
            "/v1/notebooks/podcasts/p0123456789/audio",
            Some(&h.mac),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            r.headers()["content-length"].to_str().unwrap()
        ),
        (200, "18")
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), AUDIO);
    // The original file: Content-Disposition, and no-store when Lucina says nothing.
    let r = h
        .req(
            Method::GET,
            "/v1/notebooks/sources/s0a9be3c112/file",
            Some(&h.mac),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()["content-disposition"],
        "attachment; filename=\"RM Class 2.pdf\""
    );
    assert_eq!(r.headers()["content-type"], "application/pdf");
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "%PDF-1.7");
}

#[tokio::test]
async fn chat_streams_pass_last_event_id_and_are_limited() {
    let h = harness().await;
    let path = "/v1/notebooks/chats/c0123456789/messages";
    let ask = || {
        h.req(Method::POST, path, Some(&h.mac))
            .header("accept", "text/event-stream")
            .header("last-event-id", "7")
            .json(&json!({"text": "What is a ring?"}))
    };
    let resp = ask().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    let seen = h.last();
    assert_eq!(
        (seen.accept.as_deref(), seen.last_event_id.as_deref()),
        (Some("text/event-stream"), Some("7"))
    );
    assert_eq!(seen.body, r#"{"text":"What is a ring?"}"#);
    let mut body = resp.bytes_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(
        text.contains("event: delta") && text.contains("\"after\":\"7\""),
        "the first event arrives while the stream is open: {text}"
    );
    // Four per caller: the fifth is refused while the others are open.
    let mut open = vec![body];
    for _ in 0..3 {
        open.push(ask().send().await.unwrap().bytes_stream());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, v) = h.send(ask()).await;
    assert_eq!((status, code(&v)), (429, "too_many_streams"));
    // A plain JSON answer is not a stream and takes no slot.
    let (status, _) = h
        .send(
            h.req(Method::POST, path, Some(&h.mac))
                .json(&json!({"text": "again"})),
        )
        .await;
    assert_eq!(status, 200);
    drop(open);
}

/// A body of `n` bytes that is not all the same (`sum` and `len` catch a cut or a reorder).
fn blob(n: usize) -> (Vec<u8>, u64) {
    let v: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
    let sum = v.iter().map(|b| u64::from(*b)).sum();
    (v, sum)
}

#[tokio::test]
async fn uploads_stream_through_and_other_bodies_are_limited() {
    let h = harness().await;
    let (data, sum) = blob(20 * MIB);
    let multipart = "multipart/form-data; boundary=----nb";
    // 20 MB is far over the JSON limit but fine for an upload; it arrives whole, with its content type.
    let (status, v) = h
        .send(
            h.req(
                Method::POST,
                "/v1/notebooks/nb4f1c09a2e7/sources/upload",
                Some(&h.mac),
            )
            .header("content-type", multipart)
            .body(data.clone()),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let seen = h.last();
    assert_eq!(
        (seen.path.as_str(), seen.body_len, seen.body_sum),
        (
            "/api/secretary/notebooks/nb4f1c09a2e7/sources/upload",
            20 * MIB,
            sum
        )
    );
    assert_eq!(seen.content_type.as_deref(), Some(multipart));
    // The same body anywhere else is refused before it reaches Lucina.
    let before = h.count();
    let (status, v) = h
        .send(
            h.req(
                Method::POST,
                "/v1/notebooks/nb4f1c09a2e7/sources",
                Some(&h.mac),
            )
            .header("content-type", "application/json")
            .body(data),
        )
        .await;
    assert_eq!((status, code(&v)), (413, "payload_too_large"));
    let (status, _) = h
        .send(
            h.req(
                Method::POST,
                "/v1/notebooks/nb4f1c09a2e7/sources/upload/x",
                Some(&h.mac),
            )
            .header("content-type", "application/json")
            .body(vec![b' '; 9 * MIB]),
        )
        .await;
    assert_eq!(
        status, 413,
        "only the exact upload route has the long limit"
    );
    // A JSON text source of a few MB is fine.
    let text = format!(r#"{{"kind":"text","text":"{}"}}"#, "a".repeat(5 * MIB));
    let (status, _) = h
        .send(
            h.req(
                Method::POST,
                "/v1/notebooks/nb4f1c09a2e7/sources",
                Some(&h.mac),
            )
            .header("content-type", "application/json")
            .body(text),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        h.count(),
        before + 1,
        "only the 5 MB text source got through"
    );
    assert_eq!(h.last().body_len, 5 * MIB + 25);
}

#[tokio::test]
async fn a_declared_length_over_the_upload_limit_is_refused_at_once() {
    let h = harness().await;
    let addr = h.base.trim_start_matches("http://").to_owned();
    for (path, length) in [
        ("/v1/notebooks/nb4f1c09a2e7/sources/upload", 200 * MIB + 1),
        ("/v1/notebooks/nb4f1c09a2e7/sources", 8 * MIB + 1),
    ] {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nContent-Type: multipart/form-data; boundary=x\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
            h.mac
        );
        s.write_all(head.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
            .await
            .unwrap()
            .ok();
        let out = String::from_utf8_lossy(&out);
        assert!(out.starts_with("HTTP/1.1 413"), "{path}: {out}");
        assert!(out.contains("payload_too_large"), "{out}");
    }
    assert_eq!(h.count(), 0, "nothing was sent on");
}

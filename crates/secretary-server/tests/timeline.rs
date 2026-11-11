//! Unified-chat timeline: `GET /v1/timeline`, the SSE relay
//! `GET /v1/timeline/stream`, and the chat `channel` forwarding, against a
//! mock Lucina on an ephemeral port.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::stream::{self, StreamExt};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const LUCINA_TOKEN: &str = "lucina-server-secret";

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: Option<String>,
    auth: Option<String>,
    last_event_id: Option<String>,
    accept: Option<String>,
    body: Value,
}

/// What the mock stream does after its events.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// End the response normally.
    Close,
    /// Abort the connection mid-response.
    Drop,
    /// Stay open, pinging every 50 ms.
    Hold,
    /// Answer 404 not_found (secretary API disabled).
    Disabled,
}

struct Mock {
    seen: Mutex<Vec<Seen>>,
    mode: Mutex<Mode>,
}

impl Mock {
    fn seen(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }
}

fn item(id: &str, ts: i64, channel: &str, role: &str, text: &str) -> Value {
    json!({"id": id, "ts": ts, "channel": channel, "role": role, "text": text})
}

fn sse_event(id: &str, text: &str) -> String {
    let data = item(id, 1_761_048_000_000, "telegram", "user", text);
    format!("event: item\nid: {id}\ndata: {data}\n\n")
}

const EVENT_IDS: [&str; 3] = ["e1", "e2", "e3"];

async fn mock_handler(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
    let seen = Seen {
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        auth: h("authorization"),
        last_event_id: h("last-event-id"),
        accept: h("accept"),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    m.seen.lock().unwrap().push(seen.clone());
    match (method.as_str(), uri.path()) {
        ("GET", "/api/secretary/timeline/conversations") => {
            Json(json!({"ok": true, "data": {
                "items": [{"key": "device:dev_1", "kind": "device", "title": "iPhone",
                    "channel": "ios", "device": "dev_1", "deviceName": "iPhone",
                    "count": 2, "lastId": "tl_3", "lastTs": 1761048003000_i64,
                    "lastRole": "lucina", "lastText": "Ciao!", "future": true},
                    {"key": "agent:claude", "kind": "agent", "title": "Claude",
                    "channel": "other", "caller": "claude", "count": 1,
                    "lastId": "tl_1", "lastTs": 1, "lastRole": "user", "lastText": "hi"},
                    {"key": "voice", "kind": "voice", "title": "Lucina Voice",
                    "channel": "voice", "count": 4,
                    "lastId": "tl_20", "lastTs": 1761048009000_i64, "lastRole": "lucina",
                    "lastText": "Sono le dieci."}],
                "futurePage": 42
            }}))
            .into_response()
        }
        ("GET", "/api/secretary/timeline") => {
            if seen.query.as_deref() == Some("after=expired") {
                return (
                    StatusCode::GONE,
                    Json(json!({"ok": false, "error": "cursor_expired", "message": "cursor evicted"})),
                )
                    .into_response();
            }
            let before = seen
                .query
                .as_deref()
                .unwrap_or("")
                .split('&')
                .find_map(|kv| kv.strip_prefix("before="))
                .map(str::to_owned);
            let data = match before.as_deref() {
                None => json!({
                    "items": [
                        {"id": "tl_3", "ts": 1_761_048_003_000_i64, "channel": "web", "role": "lucina",
                         "text": "Ciao!", "replyTo": "tl_2", "meta": {"kind": "share", "shareId": "share-test-1234", "intent": "ask", "transcript": "ciao", "voiceReply": true},
                         "attachments": [{"kind": "voice", "waveform": "AAEC", "transcript": "ciao", "durationMs": 4200, "name": "a.m4a", "mime": "audio/mp4", "mediaId": "itm_123", "url": "https://example.com/a.png", "bytes": 123}], "futureField": {"x": 1}},
                        {"id": "tl_2", "ts": 1_761_048_002_000_i64, "channel": "ios", "role": "user",
                         "text": "Ciao", "clientMessageId": "c-2"},
                    ],
                    "nextBefore": "tl_2", "cursor": "arrival_4", "hasMore": true,
                }),
                Some("tl_2") => json!({
                    "items": [{"id": 1, "ts": "2026-10-21T12:00:00.000Z", "channel": "whatsapp",
                               "role": "user", "text": "old", "backfilled": true}],
                    "nextBefore": null, "cursor": null, "hasMore": false,
                }),
                Some(_) => json!({"items": [], "nextBefore": null}),
            };
            Json(json!({"ok": true, "data": data})).into_response()
        }
        ("GET", "/api/secretary/timeline/stream") => {
            let mode = *m.mode.lock().unwrap();
            if mode == Mode::Disabled {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "error": "not_found"})),
                )
                    .into_response();
            }
            // Replay after Last-Event-ID, like Lucina.
            let skip = seen
                .last_event_id
                .as_deref()
                .and_then(|l| EVENT_IDS.iter().position(|e| *e == l))
                .map_or(0, |i| i + 1);
            let mut chunks = vec![": ping\n\n".to_owned()];
            chunks.extend(EVENT_IDS[skip..].iter().map(|id| sse_event(id, id)));
            let head = stream::iter(chunks).then(|c| async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok::<_, std::io::Error>(Bytes::from(c))
            });
            let tail = match mode {
                Mode::Close | Mode::Disabled => stream::empty().boxed(),
                // (the pause lets the last event reach the wire before the abort)
                Mode::Drop => stream::once(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Err(std::io::Error::other("lucina crashed"))
                })
                .boxed(),
                Mode::Hold => stream::unfold((), |()| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Some((Ok(Bytes::from_static(b": ping\n\n")), ()))
                })
                .boxed(),
            };
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(head.chain(tail)),
            )
                .into_response()
        }
        ("POST", "/api/secretary/chat") => Json(json!({"ok": true, "data": {
            "reply": "ok", "action": "reply", "job": null, "sessionId": "secretary_web",
            "userItemId": "tl_10", "lucinaItemId": "tl_11", "channel": seen.body["channel"],
        }}))
        .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not_found"})),
        )
            .into_response(),
    }
}

struct H {
    base: String,
    http: reqwest::Client,
    token: String,
    mock: Arc<Mock>,
    state: AppState,
    store: DeviceStore,
    _dir: tempfile::TempDir,
}

async fn harness(tweak: impl FnOnce(&mut Config)) -> H {
    let mock = Arc::new(Mock {
        seen: Mutex::default(),
        mode: Mutex::new(Mode::Close),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .fallback(mock_handler)
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    tweak(&mut cfg);
    let store = DeviceStore::new(&cfg.data_dir);
    let (_, token) = store.issue("test iPhone").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = app(state.clone());
    tokio::spawn(async move {
        serve_on(listener, router, std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: reqwest::Client::new(),
        token,
        mock,
        state,
        store,
        _dir: dir,
    }
}

impl H {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path)).bearer_auth(&self.token)
    }

    fn set_mode(&self, mode: Mode) {
        *self.mock.mode.lock().unwrap() = mode;
    }

    async fn json(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    /// Wait until the server holds `n` streams for any device.
    async fn wait_open(&self, n: usize) {
        for _ in 0..200 {
            if self.state.streams.open("").0 == n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "expected {n} open streams, have {}",
            self.state.streams.open("").0
        );
    }
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

/// Read until `needle` shows up in the stream (or fail after 5 s).
async fn read_until(resp: &mut reqwest::Response, buf: &mut String, needle: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !buf.contains(needle) {
            let chunk = resp.chunk().await.unwrap().expect("stream ended early");
            buf.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} within 5 s; got {buf:?}"));
}

/// Read the rest of the body; `Ok` only if the server ended it cleanly.
async fn read_to_end(resp: reqwest::Response) -> Result<String, reqwest::Error> {
    tokio::time::timeout(Duration::from_secs(5), resp.text())
        .await
        .expect("stream did not end within 5 s")
}

fn event_order(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("id: "))
        .map(str::to_owned)
        .collect()
}

// -- GET /v1/timeline ----------------------------------------------------------

#[tokio::test]
async fn timeline_pages_pass_through() {
    let h = harness(|_| {}).await;
    let (s, b) = h.json(h.get("/v1/timeline?limit=50")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["nextBefore"], "tl_2");
    let page: secretary_core::models::TimelinePage = serde_json::from_value(b.clone()).unwrap();
    assert_eq!(page.cursor.as_deref(), Some("arrival_4"));
    assert_eq!(page.has_more, Some(true));
    assert_eq!(b["items"][0]["id"], "tl_3");
    assert_eq!(b["items"][0]["ts"], 1_761_048_003_000_i64);
    assert_eq!(b["items"][0]["replyTo"], "tl_2");
    assert_eq!(
        b["items"][0]["meta"],
        json!({"kind":"share", "shareId":"share-test-1234", "intent":"ask", "transcript":"ciao", "voiceReply":true})
    );
    assert_eq!(
        b["items"][0]["attachments"],
        json!([{"kind":"voice", "waveform":"AAEC", "transcript":"ciao", "durationMs":4200, "name":"a.m4a", "mime":"audio/mp4", "mediaId":"itm_123", "url":"https://example.com/a.png", "bytes":123}])
    );
    assert_eq!(
        b["items"][0]["futureField"],
        json!({"x": 1}),
        "unknown fields kept"
    );
    assert_eq!(b["items"][1]["clientMessageId"], "c-2");
    let seen = h.mock.seen("/api/secretary/timeline");
    assert_eq!(seen[0].query.as_deref(), Some("limit=50"));
    assert_eq!(seen[0].auth.as_deref(), Some("Bearer lucina-server-secret"));

    // next page: cursor forwarded, numeric id / ISO ts / unknown channel kept
    let (s, b) = h.json(h.get("/v1/timeline?before=tl_2&limit=2")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["nextBefore"], Value::Null);
    assert_eq!(b.get("cursor"), Some(&Value::Null));
    assert_eq!(b["hasMore"], false);
    assert_eq!(b["items"][0]["id"], 1);
    assert_eq!(b["items"][0]["ts"], "2026-10-21T12:00:00.000Z");
    assert_eq!(b["items"][0]["channel"], "whatsapp");
    assert_eq!(b["items"][0]["backfilled"], true);
    let seen = h.mock.seen("/api/secretary/timeline");
    assert_eq!(seen[1].query.as_deref(), Some("before=tl_2&limit=2"));

    // no query at all: nothing added
    let (s, _) = h.json(h.get("/v1/timeline")).await;
    assert_eq!(s, 200);
    assert_eq!(h.mock.seen("/api/secretary/timeline")[2].query, None);
}

#[tokio::test]
async fn timeline_query_is_validated_before_lucina() {
    let h = harness(|_| {}).await;
    for q in [
        "limit=0",
        "limit=201",
        "limit=-1",
        "limit=ten",
        "limit=",
        "before=a%2Fb",
        "before=..",
        "before=a%20b",
        "after=a%2Fb",
        "after=..",
        "after=",
        "before=tl_1&after=tl_2",
        "scope=all",
        "scope=",
        "scope=mine&conversation=lucina",
        "conversation=Device:abc",
        "conversation=device:",
        "conversation=device:a.b",
        "conversation=device:a:b",
        "conversation=",
        "conversation=device:%2F",

    ] {
        let (s, b) = h.json(h.get(&format!("/v1/timeline?{q}"))).await;
        assert_eq!(s, 400, "{q}: {b}");
        assert_eq!(code(&b), "bad_request", "{q}");
    }
    assert!(h.mock.seen("/api/secretary/timeline").is_empty());
    let (s, _) = h.json(h.get("/v1/timeline?limit=200&before=tl_1")).await;
    assert_eq!(s, 200);
}

// -- GET /v1/timeline/stream ---------------------------------------------------

#[tokio::test]
async fn stream_relays_events_in_order_with_pings_and_ends_with_upstream() {
    let h = harness(|_| {}).await;
    let resp = h.get("/v1/timeline/stream").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let hd = resp.headers();
    assert_eq!(hd["content-type"], "text/event-stream; charset=utf-8");
    assert_eq!(hd["cache-control"], "no-store");
    assert_eq!(hd["x-accel-buffering"], "no");
    let text = read_to_end(resp).await.expect("clean end of stream");
    assert!(text.starts_with(": ping\n\n"), "{text:?}");
    assert_eq!(event_order(&text), ["e1", "e2", "e3"]);
    assert_eq!(text.matches("event: item\n").count(), 3);
    let first_data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    let v: Value = serde_json::from_str(first_data).unwrap();
    assert_eq!(v["id"], "e1");

    let seen = &h.mock.seen("/api/secretary/timeline/stream")[0];
    assert_eq!(seen.auth.as_deref(), Some("Bearer lucina-server-secret"));
    assert_eq!(seen.accept.as_deref(), Some("text/event-stream"));
    assert_eq!(seen.last_event_id, None);
    h.wait_open(0).await;
}

#[tokio::test]
async fn stream_forwards_last_event_id() {
    let h = harness(|_| {}).await;
    let resp = h
        .get("/v1/timeline/stream")
        .header("Last-Event-ID", "e1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = read_to_end(resp).await.unwrap();
    assert_eq!(event_order(&text), ["e2", "e3"], "Lucina replayed after e1");
    let seen = h.mock.seen("/api/secretary/timeline/stream");
    assert_eq!(seen[0].last_event_id.as_deref(), Some("e1"));

    // a fresh EventSource can pass it in the query; the header wins
    let text = read_to_end(
        h.get("/v1/timeline/stream?lastEventId=e2")
            .send()
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(event_order(&text), ["e3"]);
    let _ = read_to_end(
        h.get("/v1/timeline/stream?lastEventId=e2")
            .header("Last-Event-ID", "e1")
            .send()
            .await
            .unwrap(),
    )
    .await;
    let seen = h.mock.seen("/api/secretary/timeline/stream");
    assert_eq!(seen[1].last_event_id.as_deref(), Some("e2"));
    assert_eq!(seen[2].last_event_id.as_deref(), Some("e1"));

    // ids from outside the id charset never reach Lucina
    let (s, b) = h
        .json(h.get("/v1/timeline/stream").header("Last-Event-ID", "a/b"))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    assert_eq!(h.mock.seen("/api/secretary/timeline/stream").len(), 3);
}

#[tokio::test]
async fn stream_is_not_buffered() {
    let h = harness(|_| {}).await;
    h.set_mode(Mode::Hold);
    let mut resp = h.get("/v1/timeline/stream").send().await.unwrap();
    let mut buf = String::new();
    // Every event arrives while Lucina's response is still open.
    read_until(&mut resp, &mut buf, "id: e3\n").await;
    assert_eq!(event_order(&buf), ["e1", "e2", "e3"]);
    h.wait_open(1).await;
    drop(resp);
    // the slot is released once the server notices the client is gone
    h.wait_open(0).await;
}

#[tokio::test]
async fn upstream_failure_ends_the_stream_cleanly() {
    let h = harness(|_| {}).await;
    h.set_mode(Mode::Drop);
    let resp = h.get("/v1/timeline/stream").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let text = read_to_end(resp)
        .await
        .expect("an upstream crash still ends the client response cleanly");
    assert_eq!(event_order(&text), ["e1", "e2", "e3"]);
    h.wait_open(0).await;
}

#[tokio::test]
async fn upstream_error_status_is_mapped_before_streaming() {
    let h = harness(|_| {}).await;
    h.set_mode(Mode::Disabled);
    let (s, b) = h.json(h.get("/v1/timeline/stream")).await;
    assert_eq!((s, code(&b)), (502, "lucina_error"));
    h.wait_open(0).await;
}

#[tokio::test]
async fn stream_concurrency_is_capped_per_device() {
    let h = harness(|_| {}).await;
    h.set_mode(Mode::Hold);
    let mut open = Vec::new();
    for _ in 0..4 {
        let mut r = h.get("/v1/timeline/stream").send().await.unwrap();
        assert_eq!(r.status(), 200);
        let mut buf = String::new();
        read_until(&mut r, &mut buf, "id: e1\n").await;
        open.push(r);
    }
    let r = h.get("/v1/timeline/stream").send().await.unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "5");
    let b: Value = r.json().await.unwrap();
    assert_eq!(code(&b), "too_many_streams");
    // the 429 did not reach Lucina
    assert_eq!(h.mock.seen("/api/secretary/timeline/stream").len(), 4);

    // another device still gets one
    let (_, other) = h.store.issue("test iPad").unwrap();
    let r = h
        .http
        .get(h.url("/v1/timeline/stream"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    drop(r);

    // closing one frees its slot
    drop(open.pop());
    h.wait_open(3).await;
    let r = h.get("/v1/timeline/stream").send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn stream_counts_once_against_the_rate_limit() {
    let h = harness(|c| c.rate_limit_per_minute = 2).await;
    h.set_mode(Mode::Hold);
    let mut r = h.get("/v1/timeline/stream").send().await.unwrap();
    let mut buf = String::new();
    // many pings later...
    read_until(&mut r, &mut buf, "id: e3\n").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    read_until(&mut r, &mut buf, ": ping\n\n: ping\n\n: ping").await;
    // ...one token is still left
    let (s, _) = h.json(h.get("/v1/timeline")).await;
    assert_eq!(s, 200);
    let (s, b) = h.json(h.get("/v1/timeline")).await;
    assert_eq!((s, code(&b)), (429, "rate_limited"));
}

#[tokio::test]
async fn shutdown_ends_open_streams() {
    let h = harness(|_| {}).await;
    h.set_mode(Mode::Hold);
    let mut r = h.get("/v1/timeline/stream").send().await.unwrap();
    let mut buf = String::new();
    read_until(&mut r, &mut buf, "id: e1\n").await;
    h.state.begin_shutdown();
    read_to_end(r).await.expect("clean end on shutdown");
    h.wait_open(0).await;
}

// -- auth ----------------------------------------------------------------------

#[tokio::test]
async fn timeline_routes_need_auth_and_accept_a_cookie_session() {
    let h = harness(|_| {}).await;
    for path in [
        "/v1/timeline",
        "/v1/timeline/stream",
        "/v1/timeline/conversations",
    ] {
        let r = h.http.get(h.url(path)).send().await.unwrap();
        assert_eq!(r.status(), 401, "{path}");
        assert_eq!(r.headers()["www-authenticate"], "Bearer");
        let r = h
            .http
            .get(h.url(path))
            .bearer_auth("sec_nope")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path}");
    }
    assert!(h.mock.seen.lock().unwrap().is_empty());

    // a browser session (EventSource sends the cookie; GET needs no CSRF)
    let r = h
        .http
        .post(h.url("/v1/session"))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for path in [
        "/v1/timeline",
        "/v1/timeline/stream",
        "/v1/timeline/conversations",
    ] {
        let r = h
            .http
            .get(h.url(path))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let _ = r.text().await;
    }
}

// -- POST /v1/chat channel ---------------------------------------------------------

#[tokio::test]
async fn chat_channel_from_header_or_body_defaults_to_web() {
    let h = harness(|_| {}).await;
    let chat = |header: Option<&str>, body: Value| {
        let mut rb = h
            .http
            .post(h.url("/v1/chat"))
            .bearer_auth(&h.token)
            .json(&body);
        if let Some(v) = header {
            rb = rb.header("X-Secretary-Client", v);
        }
        rb
    };
    let last_channel =
        || h.mock.seen("/api/secretary/chat").last().unwrap().body["channel"].clone();

    let (s, b) = h.json(chat(None, json!({"message": "ciao"}))).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(last_channel(), "web", "default");
    assert_eq!(b["userItemId"], "tl_10");
    assert_eq!(b["lucinaItemId"], "tl_11");

    let (s, _) = h.json(chat(Some("ios"), json!({"message": "ciao"}))).await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "ios", "header");

    let (s, _) = h
        .json(chat(None, json!({"message": "ciao", "channel": "ios"})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "ios", "body");

    let (s, _) = h
        .json(chat(
            Some("web"),
            json!({"message": "ciao", "channel": "ios"}),
        ))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "web", "the header wins");

    let before = h.mock.seen("/api/secretary/chat").len();
    for (header, body) in [
        (Some("android"), json!({"message": "ciao"})),
        (None, json!({"message": "ciao", "channel": "telegram"})),
        (None, json!({"message": "ciao", "channel": ""})),
        // `voice` is the speaker's thread: only the voice pipeline sends it, no client may claim it
        (Some("voice"), json!({"message": "ciao"})),
        (None, json!({"message": "ciao", "channel": "voice"})),
    ] {
        let (s, b) = h.json(chat(header, body.clone())).await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{header:?} {body}");
    }
    assert_eq!(h.mock.seen("/api/secretary/chat").len(), before);
}

#[tokio::test]
async fn chat_channel_dsi_and_mac_are_forwarded() {
    let h = harness(|_| {}).await;
    let last_channel =
        || h.mock.seen("/api/secretary/chat").last().unwrap().body["channel"].clone();
    let chat = |header: Option<&str>, agent: Option<&str>, body: Value| {
        let mut rb = h
            .http
            .post(h.url("/v1/chat"))
            .bearer_auth(&h.token)
            .json(&body);
        if let Some(v) = header {
            rb = rb.header("X-Secretary-Client", v);
        }
        if let Some(v) = agent {
            rb = rb.header("User-Agent", v);
        }
        rb
    };

    // The firmware installed today says web: the User-Agent, source and session id say DSi.
    let (s, b) = h
        .json(chat(
            Some("web"),
            Some("Fortuna/1.62 (Nintendo DSi)"),
            json!({"message": "ciao"}),
        ))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(last_channel(), "dsi", "Fortuna user agent");
    let (s, _) = h
        .json(chat(
            Some("web"),
            None,
            json!({"message": "ciao", "source": "fortuna-chat"}),
        ))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "dsi", "fortuna-chat source");
    let (s, _) = h
        .json(chat(
            None,
            None,
            json!({"message": "ciao", "sessionId": "fortuna-abc"}),
        ))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "dsi", "fortuna- session");

    let (s, _) = h
        .json(chat(Some("mac"), None, json!({"message": "ciao"})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "mac");
    let (s, _) = h
        .json(chat(Some("dsi"), None, json!({"message": "ciao"})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "dsi");
    let (s, _) = h
        .json(chat(Some("web"), None, json!({"message": "ciao"})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(last_channel(), "web", "a plain web client stays web");

    let before = h.mock.seen("/api/secretary/chat").len();
    let (s, b) = h
        .json(chat(Some("nope"), None, json!({"message": "ciao"})))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    assert_eq!(h.mock.seen("/api/secretary/chat").len(), before);
}

// -- POST /v1/chat replyTo ---------------------------------------------------------

#[tokio::test]
async fn chat_forwards_reply_to_and_rejects_a_bad_one() {
    let h = harness(|_| {}).await;
    let chat = |body: Value| {
        h.http
            .post(h.url("/v1/chat"))
            .bearer_auth(&h.token)
            .json(&body)
    };
    let last = || {
        h.mock
            .seen("/api/secretary/chat")
            .last()
            .unwrap()
            .body
            .clone()
    };

    let (s, b) = h
        .json(chat(
            json!({"message": "e questo?", "replyTo": "01K83ZK4Q6X1V9W2D3E4F5G6H7"}),
        ))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(last()["replyTo"], "01K83ZK4Q6X1V9W2D3E4F5G6H7");
    assert_eq!(last()["message"], "e questo?");

    // Absent: the forwarded body has no replyTo key at all.
    let (s, _) = h.json(chat(json!({"message": "ciao"}))).await;
    assert_eq!(s, 200);
    assert!(last().get("replyTo").is_none(), "{}", last());

    let before = h.mock.seen("/api/secretary/chat").len();
    for bad in [
        json!({"message": "ciao", "replyTo": "../x"}),
        json!({"message": "ciao", "replyTo": ""}),
        json!({"message": "ciao", "replyTo": "x".repeat(65)}),
        json!({"message": "ciao", "replyTo": 5}),
    ] {
        let (s, b) = h.json(chat(bad.clone())).await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{bad}");
    }
    assert_eq!(h.mock.seen("/api/secretary/chat").len(), before);
}

#[tokio::test]
async fn chat_stamps_authenticated_device() {
    let h = harness(|_| {}).await;
    let (_, session) = h.json(h.get("/v1/session")).await;
    assert!(session["deviceId"].as_str().unwrap().starts_with("dev_"));
    for stamp in [
        json!({}),
        json!({"device": "spoofed", "deviceName": "spoofed"}),
    ] {
        let mut body = stamp;
        body["message"] = json!("ciao");
        let (s, b) = h
            .json(
                h.http
                    .post(h.url("/v1/chat"))
                    .bearer_auth(&h.token)
                    .json(&body),
            )
            .await;
        assert_eq!(s, 200, "{b}");
        let seen = h.mock.seen("/api/secretary/chat");
        let forwarded = &seen.last().unwrap().body;
        assert_eq!(forwarded["device"], session["deviceId"]);
        assert_eq!(forwarded["deviceName"], "test iPhone");
    }
}

#[tokio::test]
async fn mine_uses_identity_and_header_channel() {
    let h = harness(|_| {}).await;
    let (_, session) = h.json(h.get("/v1/session")).await;
    for (header, channel) in [(None, "web"), (Some(" ios "), "ios")] {
        let mut req = h.get("/v1/timeline?scope=mine&after=tl_1&device=spoofed&channel=discord");
        if let Some(header) = header {
            req = req.header("X-Secretary-Client", header);
        }
        let (s, b) = h.json(req).await;
        assert_eq!(s, 200, "{b}");
        let seen = h.mock.seen("/api/secretary/timeline");
        let query = seen.last().unwrap().query.as_deref().unwrap();
        let params: std::collections::HashMap<_, _> =
            reqwest::Url::parse(&format!("http://test/?{query}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(params.len(), 4);
        assert_eq!(params["after"], "tl_1");
        assert_eq!(params["scope"], "mine");
        assert_eq!(params["device"], session["deviceId"].as_str().unwrap());
        assert_eq!(params["channel"], channel);
    }
    let (s, b) = h
        .json(
            h.get("/v1/timeline?scope=mine")
                .header("X-Secretary-Client", "android"),
        )
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    assert_eq!(h.mock.seen("/api/secretary/timeline").len(), 2);
    let (s, _) = h
        .json(h.get("/v1/timeline?device=spoofed&channel=ios"))
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        h.mock.seen("/api/secretary/timeline").last().unwrap().query,
        None
    );
}

#[tokio::test]
async fn conversation_filters_and_incremental_cursor_are_forwarded() {
    let h = harness(|_| {}).await;
    for key in [
        "selftest".to_owned(),
        "lucina".into(),
        "device:D_1-x".into(),
        "agent:claude".into(),
        "legacy:web".into(),
        format!("device:{}", "x".repeat(64)),
    ] {
        let (s, b) = h
            .json(
                h.get("/v1/timeline")
                    .query(&[("conversation", key.as_str()), ("after", "tl_1")]),
            )
            .await;
        assert_eq!(s, 200, "{key}: {b}");
        let seen = h.mock.seen("/api/secretary/timeline");
        let query = seen.last().unwrap().query.as_deref().unwrap();
        let url = reqwest::Url::parse(&format!("http://test/?{query}")).unwrap();
        assert!(url
            .query_pairs()
            .any(|(k, v)| k == "conversation" && v == key));
        assert!(url.query_pairs().any(|(k, v)| k == "after" && v == "tl_1"));
    }
    for q in [
        format!("conversation=device:{}", "x".repeat(65)),
        format!("after={}", "x".repeat(129)),
        format!("before={}", "x".repeat(129)),
    ] {
        let (s, b) = h.json(h.get(&format!("/v1/timeline?{q}"))).await;
        assert_eq!((s, code(&b)), (400, "bad_request"));
    }
    assert_eq!(h.mock.seen("/api/secretary/timeline").len(), 6);
}

#[tokio::test]
async fn expired_cursor_is_gone() {
    let h = harness(|_| {}).await;
    let (s, b) = h.json(h.get("/v1/timeline?after=expired")).await;
    assert_eq!((s, code(&b)), (410, "cursor_expired"));
    assert_eq!(b["error"]["message"], "cursor evicted");
    assert_eq!(h.mock.seen("/api/secretary/timeline").len(), 1);
}

#[tokio::test]
async fn conversations_are_typed_and_keep_unknown_fields() {
    let h = harness(|_| {}).await;
    let (s, b) = h.json(h.get("/v1/timeline/conversations")).await;
    assert_eq!(s, 200, "{b}");
    let data: secretary_core::models::TimelineConversations =
        serde_json::from_value(b.clone()).unwrap();
    assert_eq!(data.items[0].device_name.as_deref(), Some("iPhone"));
    assert_eq!(data.items[0].count, 2);
    assert_eq!(data.items[1].caller.as_deref(), Some("claude"));
    assert_eq!(data.items[1].device, None);
    // The shared LucinaVoice thread is a conversation like any other (protocol section 7).
    assert_eq!(data.items[2].key, "voice");
    assert_eq!(data.items[2].title, "Lucina Voice");
    assert_eq!(data.items[2].channel, secretary_core::models::Channel::Voice);
    assert_eq!(b["items"][2]["channel"], "voice");
    assert_eq!(serde_json::to_value(data).unwrap(), b);
    assert_eq!(b["futurePage"], 42);
    assert_eq!(b["items"][0]["future"], true);
    let seen = h.mock.seen("/api/secretary/timeline/conversations");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].auth.as_deref(), Some("Bearer lucina-server-secret"));
    assert_eq!(seen[0].query, None);
}

#[tokio::test]
async fn conversations_share_the_device_rate_limit() {
    let h = harness(|cfg| cfg.rate_limit_per_minute = 1).await;
    let (s, _) = h.json(h.get("/v1/timeline")).await;
    assert_eq!(s, 200);
    let (s, b) = h.json(h.get("/v1/timeline/conversations")).await;
    assert_eq!((s, code(&b)), (429, "rate_limited"));
    assert!(h
        .mock
        .seen("/api/secretary/timeline/conversations")
        .is_empty());
}

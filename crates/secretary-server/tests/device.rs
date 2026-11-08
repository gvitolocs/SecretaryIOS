//! Device bridge routes against a mock Lucina: event batches (validation,
//! dedup, own rate limit, big body limit), command results and state.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<(String, Value)>>,
    fail_events: Mutex<bool>,
}

fn ok(data: Value) -> Response {
    Json(json!({"ok": true, "data": data})).into_response()
}

async fn mock_handler(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_owned();
    m.seen.lock().unwrap().push((path.clone(), body.clone()));
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/secretary/device/events") => {
            let boom = body["events"]
                .as_array()
                .is_some_and(|e| e.iter().any(|e| e["id"] == "boom"));
            if *m.fail_events.lock().unwrap() || boom {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": "store_failed"})),
                )
                    .into_response();
            }
            ok(json!({"stored": body["events"].as_array().map_or(0, Vec::len)}))
        }
        ("POST", "/api/secretary/device/commands/missing/result") => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "command_not_found", "message": "no such command"})),
        )
            .into_response(),
        ("POST", p) if p.starts_with("/api/secretary/device/commands/") => {
            ok(json!({"acked": true, "ok": body["ok"]}))
        }
        ("POST", "/api/secretary/device/state") => ok(json!({"saved": true})),
        ("POST", "/api/secretary/device/juno/next") => {
            if body["waitSecs"] == 7 {
                ok(
                    json!({"call": {"id": "call-1", "tool": "screenshot", "arguments": {}, "deadlineMs": 123}, "events": []}),
                )
            } else {
                ok(json!({"call": null, "events": []}))
            }
        }
        ("GET", "/api/secretary/juno/screenshots/shot1.png") => (
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            b"\x89PNG\r\n\x1a\nscreen".to_vec(),
        )
            .into_response(),
        ("GET", p) if p.starts_with("/api/secretary/juno/screenshots/") => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "screenshot_not_found"})),
        )
            .into_response(),
        ("POST", "/api/secretary/device/juno/result") => {
            if body["id"] == "gone" {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "error": "unknown_call"})),
                )
                    .into_response()
            } else {
                ok(json!({"delivered": true}))
            }
        }
        ("POST", "/api/secretary/device/juno/events") => {
            ok(json!({"stored": body["events"].as_array().map_or(0, Vec::len)}))
        }
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
    juno_token: String,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

async fn harness(tweak: impl FnOnce(&mut Config)) -> H {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina = format!("http://{}", listener.local_addr().unwrap());
    // Lucina caps request bodies at 64 KiB, except the juno result route
    // (a base64 PNG): big enough for secretary-server's 12 MiB to be the limit,
    // and the juno events route (320 KiB, above secretary-server's 256 KiB).
    let router = Router::new()
        .route(
            "/api/secretary/device/juno/result",
            axum::routing::post(mock_handler)
                .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/api/secretary/device/juno/events",
            axum::routing::post(mock_handler)
                .layer(axum::extract::DefaultBodyLimit::max(320 * 1024)),
        )
        .fallback(mock_handler)
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    tweak(&mut cfg);
    let store = DeviceStore::new(&cfg.data_dir);
    let (_, token) = store.issue("Giuseppe's iPhone").unwrap();
    let (_, juno_token) = store.issue("Juno (Galaxy Book Go)").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(listener, app(state), std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: reqwest::Client::new(),
        token,
        juno_token,
        mock,
        _dir: dir,
    }
}

impl H {
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn juno_post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.juno_token)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn seen(&self, path: &str) -> Vec<Value> {
        self.mock
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

const EVENTS: &str = "/api/secretary/device/events";

fn ev(id: &str, kind: &str) -> Value {
    json!({"id": id, "ts": 1_761_048_000_000_i64, "type": kind, "source": "ios",
           "data": {"domain": "example.com"}})
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn events_are_forwarded_with_the_device() {
    let h = harness(|_| {}).await;
    let (s, b) = h
        .send(h.post("/v1/device/events").json(&json!({
            "events": [ev("e1", "web_visit"), ev("e2", "location_visit")],
            "device": {"id": "spoofed", "name": "nope"}
        })))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b,
        json!({"accepted": 2, "duplicates": [], "results": [{"stored": 2}]})
    );
    let sent = &h.seen(EVENTS)[0];
    assert_eq!(sent["events"][0], ev("e1", "web_visit"));
    assert_eq!(sent["events"][1]["type"], "location_visit");
    assert_eq!(sent["device"]["name"], "Giuseppe's iPhone");
    assert_ne!(sent["device"]["id"], "spoofed");
}

#[tokio::test]
async fn duplicate_event_ids_are_dropped() {
    let h = harness(|_| {}).await;
    let batch = json!({"events": [ev("e1", "workout"), ev("e2", "workout")]});
    let (s, _) = h.send(h.post("/v1/device/events").json(&batch)).await;
    assert_eq!(s, 200);

    // the offline queue replays the same batch: nothing reaches Lucina
    let (s, b) = h.send(h.post("/v1/device/events").json(&batch)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b,
        json!({"accepted": 0, "duplicates": ["e1", "e2"], "results": []})
    );
    assert_eq!(h.seen(EVENTS).len(), 1);

    // old + new + a repeat inside the batch: only the new one is forwarded
    let (s, b) = h
        .send(h.post("/v1/device/events").json(&json!({"events": [
            ev("e2", "workout"), ev("e3", "alarm_fired"), ev("e3", "alarm_fired")
        ]})))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["accepted"], 1);
    assert_eq!(b["duplicates"], json!(["e2", "e3"]));
    let sent = h.seen(EVENTS);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["events"].as_array().unwrap().len(), 1);
    assert_eq!(sent[1]["events"][0]["id"], "e3");
}

#[tokio::test]
async fn a_failed_batch_is_not_remembered() {
    let h = harness(|_| {}).await;
    let batch = json!({"events": [ev("e1", "focus_change")]});
    *h.mock.fail_events.lock().unwrap() = true;
    let (s, b) = h.send(h.post("/v1/device/events").json(&batch)).await;
    assert_eq!((s, code(&b)), (502, "lucina_error"));
    *h.mock.fail_events.lock().unwrap() = false;
    let (s, b) = h.send(h.post("/v1/device/events").json(&batch)).await;
    assert_eq!((s, b["accepted"].clone()), (200, json!(1)), "{b}");
}

#[tokio::test]
async fn a_chunk_failure_keeps_the_chunks_before_it() {
    let h = harness(|_| {}).await;
    let data = "d".repeat(7_000);
    let mut events: Vec<Value> = (0..20)
        .map(|i| {
            json!({"id": format!("e{i}"), "ts": 1, "type": "workout", "source": "ios",
                   "data": {"blob": data}})
        })
        .collect();
    events[15]["id"] = json!("boom");
    let (s, b) = h
        .send(h.post("/v1/device/events").json(&json!({"events": events})))
        .await;
    assert_eq!((s, code(&b)), (502, "lucina_error"));
    let first_chunks = h.seen(EVENTS).len();
    assert!(first_chunks >= 2);

    // the retry of the same batch skips what Lucina already stored
    events[15]["id"] = json!("e15");
    let (s, b) = h
        .send(h.post("/v1/device/events").json(&json!({"events": events})))
        .await;
    assert_eq!(s, 200, "{b}");
    let accepted = b["accepted"].as_u64().unwrap();
    let dups = b["duplicates"].as_array().unwrap().len() as u64;
    assert_eq!(accepted + dups, 20);
    assert!(dups >= 6, "the chunks before the failure were kept: {b}");
    assert!(b["duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d != "e15"));
}

#[tokio::test]
async fn events_are_validated_before_lucina() {
    let h = harness(|_| {}).await;
    let big = "x".repeat(8 * 1024);
    let too_many: Vec<Value> = (0..501).map(|i| ev(&format!("e{i}"), "app_open")).collect();
    let cases = [
        json!({"events": []}),
        json!({"events": too_many}),
        json!({"events": [ev("e1", "keystrokes")]}),
        json!({"events": [{"id": "e1", "ts": 1, "type": "app_open", "source": "android"}]}),
        json!({"events": [{"id": "e1", "ts": 1, "type": "app_open", "source": "ios", "data": {"big": big}}]}),
        json!({"events": [{"id": "e1", "ts": 1, "type": "app_open", "source": "ios", "data": "text"}]}),
        json!({"events": [{"id": "a/b", "ts": 1, "type": "app_open", "source": "ios"}]}),
        json!({"events": [{"id": "e1", "ts": "today", "type": "app_open", "source": "ios"}]}),
        json!({"events": [{"id": "e1", "type": "app_open", "source": "ios"}]}),
        // one bad event fails the whole batch
        json!({"events": [ev("ok1", "workout"), ev("e2", "nope")]}),
    ];
    for body in cases {
        let (s, b) = h.send(h.post("/v1/device/events").json(&body)).await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{b}");
    }
    assert!(h.seen(EVENTS).is_empty());
}

#[tokio::test]
async fn a_full_batch_fits_the_events_body_limit_only() {
    let h = harness(|_| {}).await;
    // 500 events with ~7.5 KiB of data each: ~3.8 MB, over the 256 KiB default
    let data = "d".repeat(7_500);
    let events: Vec<Value> = (0..500)
        .map(|i| {
            json!({"id": format!("e{i}"), "ts": 1_761_048_000_000_i64, "type": "health_summary",
                   "source": "ios", "data": {"blob": data}})
        })
        .collect();
    let (s, b) = h
        .send(h.post("/v1/device/events").json(&json!({"events": events})))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["accepted"], 500);
    // forwarded in chunks under Lucina's 64 KiB, in order, nothing lost
    let sent = h.seen(EVENTS);
    assert!(sent.len() >= 60, "{} chunks", sent.len());
    assert_eq!(b["results"].as_array().unwrap().len(), sent.len());
    let ids: Vec<String> = sent
        .iter()
        .flat_map(|c| c["events"].as_array().unwrap().clone())
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, (0..500).map(|i| format!("e{i}")).collect::<Vec<_>>());
    for c in &sent {
        assert!(serde_json::to_vec(c).unwrap().len() <= 64 * 1024);
        assert_eq!(c["device"]["name"], "Giuseppe's iPhone");
    }

    // other routes keep the 256 KiB limit
    let alarms: Vec<Value> = (0..60)
        .map(|i| json!({"id": format!("a{i}"), "note": "n".repeat(5_000)}))
        .collect();
    let (s, b) = h
        .send(h.post("/v1/device/state").json(&json!({"alarms": alarms})))
        .await;
    assert_eq!((s, code(&b)), (413, "payload_too_large"));
}

#[tokio::test]
async fn events_have_their_own_rate_limit_bucket() {
    let h = harness(|c| {
        c.device_events_per_minute = 3;
        c.rate_limit_per_minute = 2;
    })
    .await;
    for i in 0..3 {
        let (s, b) = h
            .send(
                h.post("/v1/device/events")
                    .json(&json!({"events": [ev(&format!("e{i}"), "app_open")]})),
            )
            .await;
        assert_eq!(s, 200, "{b}");
    }
    let r = h
        .post("/v1/device/events")
        .json(&json!({"events": [ev("e9", "app_open")]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    // the general bucket (2/min) is untouched by the 3 event posts
    for _ in 0..2 {
        let (s, b) = h
            .send(h.post("/v1/device/state").json(&json!({"focus": null})))
            .await;
        assert_eq!(s, 200, "{b}");
    }
    let (s, b) = h
        .send(h.post("/v1/device/state").json(&json!({"focus": null})))
        .await;
    assert_eq!((s, code(&b)), (429, "rate_limited"));
}

#[tokio::test]
async fn command_results_and_state_are_forwarded() {
    let h = harness(|_| {}).await;
    let (s, b) = h
        .send(
            h.post("/v1/device/commands/cmd-7/result")
                .json(&json!({"ok": true, "deviceObjectId": "AB12-CD:34", "extra": 1})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b, json!({"acked": true, "ok": true}));
    let sent = &h.seen("/api/secretary/device/commands/cmd-7/result")[0];
    assert_eq!(sent["ok"], true);
    assert_eq!(sent["deviceObjectId"], "AB12-CD:34");
    assert_eq!(sent["device"]["name"], "Giuseppe's iPhone");
    assert!(sent.get("extra").is_none());

    let (s, b) = h
        .send(
            h.post("/v1/device/commands/cmd-8/result")
                .json(&json!({"ok": false, "error": "AlarmKit not authorized"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let (s, b) = h
        .send(
            h.post("/v1/device/commands/missing/result")
                .json(&json!({"ok": true})),
        )
        .await;
    assert_eq!((s, code(&b)), (404, "command_not_found"));
    for (path, body) in [
        ("/v1/device/commands/a%2Fb/result", json!({"ok": true})),
        ("/v1/device/commands/c1/result", json!({"error": "no ok"})),
        (
            "/v1/device/commands/c1/result",
            json!({"ok": false, "error": "e".repeat(501)}),
        ),
    ] {
        let (s, b) = h.send(h.post(path).json(&body)).await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{path} {body}: {b}");
    }

    let snapshot = json!({
        "alarms": [{"id": "al-1", "time": "07:00", "label": "Gym", "repeat": ["mon"]}],
        "timers": [], "reminders": [{"id": "r1", "title": "Latte"}],
        "events": [{"id": "ev1", "title": "Dentista", "start": "2026-10-22T09:00:00+02:00"}],
        "focus": {"mode": "work"}, "ts": 1_761_048_000_000_i64
    });
    let (s, b) = h.send(h.post("/v1/device/state").json(&snapshot)).await;
    assert_eq!((s, b), (200, json!({"saved": true})));
    let sent = &h.seen("/api/secretary/device/state")[0];
    assert_eq!(sent["alarms"], snapshot["alarms"]);
    assert_eq!(sent["focus"], json!({"mode": "work"}));
    assert_eq!(sent["device"]["name"], "Giuseppe's iPhone");
    let (s, b) = h
        .send(
            h.post("/v1/device/state")
                .json(&json!({"alarms": ["07:00"]})),
        )
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
}

const JUNO_NEXT: &str = "/api/secretary/device/juno/next";
const JUNO_RESULT: &str = "/api/secretary/device/juno/result";
const JUNO_EVENTS: &str = "/api/secretary/device/juno/events";

#[tokio::test]
async fn juno_device_routes_require_auth() {
    let h = harness(|_| {}).await;
    for path in ["/v1/device/juno/next", "/v1/device/juno/calls/c1/result"] {
        let r = h
            .http
            .post(format!("{}{path}", h.base))
            .json(&json!({"ok": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path}");
        let r = h
            .http
            .post(format!("{}{path}", h.base))
            .bearer_auth("not-a-token")
            .json(&json!({"ok": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path}");
    }
    let r = h
        .http
        .get(format!("{}/v1/device/juno/next", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(h.mock.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn juno_device_next_forwards_wait_and_device() {
    let h = harness(|_| {}).await;
    let get = |q: &str| {
        h.http
            .get(format!("{}/v1/device/juno/next{q}", h.base))
            .bearer_auth(&h.juno_token)
    };
    // default wait is 25, 99 clamps to 25, 7 passes through with the call
    for (q, wait) in [("", 25), ("?wait=99", 25), ("?wait=7", 7)] {
        let (s, b) = h.send(get(q)).await;
        assert_eq!(s, 200, "{q}: {b}");
        let seen = h.seen(JUNO_NEXT);
        let sent = seen.last().unwrap();
        assert_eq!(sent["waitSecs"], wait, "{q}");
        assert_eq!(sent["device"]["name"], "Juno (Galaxy Book Go)");
        assert_ne!(sent["device"]["id"], "");
        if wait == 7 {
            assert_eq!(
                b["call"]["id"], "call-1",
                "Lucina's data passes through: {b}"
            );
            assert_eq!(b["events"], json!([]));
        } else {
            assert_eq!(b, json!({"call": null, "events": []}));
        }
    }
    let (s, b) = h.send(get("?wait=soon")).await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
}

#[tokio::test]
async fn juno_device_result_forwards_and_ignores_client_device() {
    let h = harness(|_| {}).await;
    let (s, b) = h
        .send(
            h.juno_post("/v1/device/juno/calls/call-1/result")
                .json(&json!({
                    "ok": false,
                    "result": {"png": "aGk="},
                    "error": "no screen",
                    "device": {"id": "spoofed", "name": "nope"}
                })),
        )
        .await;
    assert_eq!((s, b), (200, json!({"delivered": true})));
    let sent = &h.seen(JUNO_RESULT)[0];
    assert_eq!(sent["id"], "call-1");
    assert_eq!(sent["ok"], false);
    assert_eq!(sent["result"], json!({"png": "aGk="}));
    assert_eq!(sent["error"], "no screen");
    assert_eq!(sent["device"]["name"], "Juno (Galaxy Book Go)");
    assert_ne!(sent["device"]["id"], "spoofed");

    let (s, b) = h
        .send(
            h.juno_post("/v1/device/juno/calls/call-2/result")
                .json(&json!({"ok": true})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let sent = &h.seen(JUNO_RESULT)[1];
    assert!(sent.get("result").is_none() && sent.get("error").is_none());
}

#[tokio::test]
async fn juno_device_result_rejects_bad_ids_before_lucina() {
    let h = harness(|_| {}).await;
    for id in ["a%2Fb", &"x".repeat(65), "a.b"] {
        let (s, b) = h
            .send(
                h.juno_post(&format!("/v1/device/juno/calls/{id}/result"))
                    .json(&json!({"ok": true})),
            )
            .await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{id}: {b}");
    }
    assert!(h.seen(JUNO_RESULT).is_empty());
}

#[tokio::test]
async fn juno_device_result_unknown_call_is_404() {
    let h = harness(|_| {}).await;
    let (s, b) = h
        .send(
            h.juno_post("/v1/device/juno/calls/gone/result")
                .json(&json!({"ok": false, "error": "too late"})),
        )
        .await;
    assert_eq!((s, code(&b)), (404, "unknown_call"), "{b}");
}

#[tokio::test]
async fn juno_device_result_body_limit_is_12_mib() {
    let h = harness(|_| {}).await;
    let big = |mib: usize| {
        h.juno_post("/v1/device/juno/calls/call-9/result")
            .json(&json!({"ok": true, "result": {"png": "x".repeat(mib * 1024 * 1024)}}))
    };
    let (s, b) = h.send(big(9)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(h.seen(JUNO_RESULT).len(), 1);
    let (s, b) = h.send(big(13)).await;
    assert_eq!((s, code(&b)), (413, "payload_too_large"), "{b}");
    assert_eq!(h.seen(JUNO_RESULT).len(), 1);
}

fn juno_ev(seq: u64, kind: &str) -> Value {
    json!({"seq": seq, "src": "devd", "src_seq": seq, "t_ms": 1000 + seq, "kind": kind,
           "app": "video", "subject": "Video", "fields": {"pid": 7}})
}

#[tokio::test]
async fn juno_device_events_drop_bad_ones_and_stamp_the_device() {
    let h = harness(|_| {}).await;
    let (s, b) = h
        .send(h.juno_post("/v1/device/juno/events").json(&json!({
            "events": [juno_ev(1, "app.focused"), juno_ev(2, "BAD KIND"), juno_ev(3, "media.started")],
            "lost": 3,
            "device": {"id": "x", "name": "y"}
        })))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["stored"], 2);
    let sent = &h.seen(JUNO_EVENTS)[0];
    let seqs: Vec<u64> = sent["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 3]);
    assert_eq!(sent["lost"], 4, "3 lost on the laptop and 1 dropped");
    assert_eq!(sent["device"]["name"], "Juno (Galaxy Book Go)");
    assert_ne!(sent["device"]["id"], "x");
}

#[tokio::test]
async fn juno_device_events_bad_bodies_and_limits() {
    let h = harness(|_| {}).await;
    let post = |body: Value| h.send(h.juno_post("/v1/device/juno/events").json(&body));
    let (s, b) =
        post(json!({"events": (0..65).map(|i| juno_ev(i, "net.up")).collect::<Vec<_>>()})).await;
    assert_eq!((s, code(&b)), (400, "too_many_events"), "{b}");
    let (s, b) = post(json!({"events": "nope"})).await;
    assert_eq!((s, code(&b)), (400, "bad_request"), "{b}");
    let (s, b) = post(json!({"events": [], "lost": -1})).await;
    assert_eq!((s, code(&b)), (400, "bad_request"), "{b}");
    assert!(h.seen(JUNO_EVENTS).is_empty());
    // 64 events of 3.5 KiB fields fit in 256 KiB; a bigger body is refused.
    let fat = |n: usize| {
        let mut e = juno_ev(1, "net.up");
        e["fields"] = json!({"blob": "x".repeat(n)});
        e
    };
    let (s, b) = post(json!({"events": vec![fat(3500); 64]})).await;
    assert_eq!(s, 200, "{b}");
    let (s, _) = post(json!({"events": vec![fat(4000); 64], "pad": "x".repeat(16 * 1024)})).await;
    assert_eq!(s, 413);
    assert_eq!(h.seen(JUNO_EVENTS).len(), 1);
}

#[tokio::test]
async fn juno_device_routes_require_juno_device() {
    let h = harness(|_| {}).await;
    let get = h
        .http
        .get(format!("{}/v1/device/juno/next?wait=0", h.base))
        .bearer_auth(&h.token);
    let (s, b) = h.send(get).await;
    assert_eq!((s, code(&b)), (403, "not_juno"), "{b}");
    for path in ["/v1/device/juno/calls/c1/result", "/v1/device/juno/events"] {
        let (s, b) = h
            .send(h.post(path).json(&json!({"ok": true, "events": []})))
            .await;
        assert_eq!((s, code(&b)), (403, "not_juno"), "{path}: {b}");
    }
    assert!(h.mock.seen.lock().unwrap().is_empty());
    // The Juno laptop's own token passes.
    let get = h
        .http
        .get(format!("{}/v1/device/juno/next?wait=0", h.base))
        .bearer_auth(&h.juno_token);
    assert_eq!(h.send(get).await.0, 200);
}

#[tokio::test]
async fn device_routes_need_auth_and_csrf_for_cookies() {
    let h = harness(|_| {}).await;
    let paths = [
        (
            "/v1/device/events",
            json!({"events": [ev("e1", "app_open")]}),
        ),
        ("/v1/device/commands/c1/result", json!({"ok": true})),
        ("/v1/device/state", json!({})),
    ];
    for (path, body) in &paths {
        let r = h
            .http
            .post(format!("{}{path}", h.base))
            .json(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path}");
    }
    assert!(h.mock.seen.lock().unwrap().is_empty());

    let r = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for (path, body) in &paths {
        let rb = || {
            h.http
                .post(format!("{}{path}", h.base))
                .header("cookie", &cookie)
                .json(body)
        };
        let (s, b) = h.send(rb()).await;
        assert_eq!((s, code(&b)), (403, "csrf"), "{path}");
        let (s, b) = h.send(rb().header("x-secretary-csrf", "1")).await;
        assert_eq!(s, 200, "{path}: {b}");
    }
}

#[tokio::test]
async fn juno_device_screenshot_is_relayed() {
    let h = harness(|_| {}).await;
    let get = |name: &str| {
        h.http
            .get(format!("{}/v1/juno/screenshots/{name}", h.base))
            .bearer_auth(&h.juno_token)
    };
    let r = get("shot1.png").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        b"\x89PNG\r\n\x1a\nscreen"
    );
    let (s, b) = h.send(get("gone.png")).await;
    assert_eq!((s, code(&b)), (404, "screenshot_not_found"), "{b}");
    let (s, b) = h.send(get("a.b.png")).await;
    assert_eq!((s, code(&b)), (400, "bad_request"), "{b}");
    let r = h
        .http
        .get(format!("{}/v1/juno/screenshots/shot1.png", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

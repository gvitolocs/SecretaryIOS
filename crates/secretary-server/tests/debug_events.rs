//! `POST` / `GET /v1/debug/events`: auth, the sanitizer on arrival, caps,
//! the per-device rate limit and the read filters.

use reqwest::Method;
use secretary_server::config::Config;
use secretary_server::debug_events::{BATCHES_PER_MINUTE, BODY_LIMIT, MAX_BATCH};
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const SERVICE: &str = "svc_debug_events_test_service_token_0123456789";

struct H {
    base: String,
    http: reqwest::Client,
    phone: String,
    phone_id: String,
    mac: String,
    dir: tempfile::TempDir,
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new("http://127.0.0.1:9", Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    cfg.service_token = Some(ServiceToken::from_secret(SERVICE).unwrap());
    let store = DeviceStore::new(&cfg.data_dir);
    let (phone_dev, phone) = store.issue("Giuseppe's iPhone").unwrap();
    let (_, mac) = store.issue("MacBook").unwrap();
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
        phone,
        phone_id: phone_dev.id,
        mac,
        dir,
    }
}

impl H {
    async fn send(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, events: Vec<Value>) -> (u16, Value) {
        self.send(
            Method::POST,
            "/v1/debug/events",
            Some(token),
            Some(json!({"events": events})),
        )
        .await
    }

    async fn get(&self, token: &str, query: &str) -> (u16, Value) {
        self.send(
            Method::GET,
            &format!("/v1/debug/events{query}"),
            Some(token),
            None,
        )
        .await
    }
}

fn ev(seq: u64, second: u32, kind: &str, fields: Value) -> Value {
    json!({"ts": format!("2026-11-01T10:00:{second:02}.000Z"), "seq": seq, "device": "spoofed",
           "build": "adfd5fe", "process": "app", "type": kind, "fields": fields})
}

fn seqs(v: &Value) -> Vec<u64> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn both_routes_need_auth() {
    let h = harness().await;
    let (s, _) = h.send(Method::GET, "/v1/debug/events", None, None).await;
    assert_eq!(s, 401);
    let (s, _) = h
        .send(
            Method::POST,
            "/v1/debug/events",
            None,
            Some(json!({"events": []})),
        )
        .await;
    assert_eq!(s, 401);
    let (s, _) = h.get("sec_not_a_token", "").await;
    assert_eq!(s, 401);
    let (s, _) = h.get("svc_wrong", "").await;
    assert_eq!(s, 401);
    // The service token reads but does not write.
    let (s, b) = h.get(SERVICE, "").await;
    assert_eq!(s, 200, "{b}");
    let (s, b) = h.post(SERVICE, vec![ev(1, 0, "ui.tap", json!({}))]).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (403, Some("devices_only"))
    );
}

#[tokio::test]
async fn events_are_stored_sanitized_and_read_back_newest_last() {
    let h = harness().await;
    let (s, b) = h
        .post(
            &h.phone,
            vec![
                ev(1, 1, "app.foreground", json!({})),
                ev(
                    2,
                    2,
                    "net.request",
                    json!({"method": "POST", "path": "/v1/approvals/ap_8f3a19c2d4e5/decide?x=1",
                    "status": 500, "duration_ms": 412}),
                ),
                ev(
                    3,
                    3,
                    "approval.decide",
                    json!({"id": "ap_1", "command": "rm -rf /"}),
                ),
                ev(
                    4,
                    4,
                    "auth.result",
                    json!({"ok": false, "error": "Bearer sec_leaked"}),
                ),
                json!({"seq": 5, "type": "ui.tap"}),
            ],
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["accepted"], 3);
    assert_eq!(b["max_seq"], 5);
    let reasons: Vec<&str> = b["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["forbidden_field:command", "bad_ts"]);

    let (s, b) = h.get(&h.mac, "").await;
    assert_eq!(s, 200);
    assert_eq!(seqs(&b), [1, 2, 4]);
    let net = &b["events"][1];
    assert_eq!(net["device"], h.phone_id.as_str());
    assert_eq!(net["device_name"], "Giuseppe's iPhone");
    assert_eq!(net["build"], "adfd5fe");
    assert_eq!(net["fields"]["path"], "/v1/approvals/{id}/decide");
    assert!(net["received_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(b["events"][2]["fields"]["error"], "[redacted]");

    // Nothing forbidden reached the disk.
    let raw = std::fs::read_to_string(h.dir.path().join("state/debug-events.jsonl")).unwrap();
    assert!(!raw.contains("rm -rf") && !raw.contains("sec_leaked") && !raw.contains("x=1"));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(h.dir.path().join("state/debug-events.jsonl"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn filters_device_type_time_and_limit() {
    let h = harness().await;
    h.post(
        &h.phone,
        (0..6)
            .map(|i| {
                ev(
                    i,
                    i as u32,
                    if i % 2 == 0 { "net.request" } else { "ui.tap" },
                    json!({}),
                )
            })
            .collect(),
    )
    .await;
    h.post(&h.mac, vec![ev(100, 30, "app.launch", json!({}))])
        .await;

    let (_, b) = h.get(SERVICE, "?device=iphone").await;
    assert_eq!(seqs(&b), [0, 1, 2, 3, 4, 5]);
    let (_, b) = h.get(SERVICE, &format!("?device={}", h.phone_id)).await;
    assert_eq!(seqs(&b).len(), 6);
    let (_, b) = h.get(SERVICE, "?device=macbook").await;
    assert_eq!(seqs(&b), [100]);
    let (_, b) = h.get(SERVICE, "?type=net").await;
    assert_eq!(seqs(&b), [0, 2, 4]);
    let (_, b) = h.get(SERVICE, "?type=ui.tap&limit=2").await;
    assert_eq!((seqs(&b), b["more"].as_bool()), (vec![3, 5], Some(true)));
    let (_, b) = h
        .get(
            SERVICE,
            "?since=2026-11-01T10:00:02Z&until=2026-11-01T11:00:04%2B01:00",
        )
        .await;
    assert_eq!(seqs(&b), [2, 3, 4]);
    let (_, b) = h.get(SERVICE, "?since=2026-10-31").await;
    assert_eq!(b["count"], 7);

    for bad in ["?since=soon", "?type=UI", "?limit=x", "?nope=1"] {
        let (s, _) = h.get(SERVICE, bad).await;
        assert_eq!(s, 400, "{bad}");
    }
}

#[tokio::test]
async fn batch_and_body_caps() {
    let h = harness().await;
    let too_many: Vec<Value> = (0..=MAX_BATCH as u64)
        .map(|i| ev(i, 0, "ui.tap", json!({})))
        .collect();
    let (s, _) = h.post(&h.phone, too_many).await;
    assert_eq!(s, 400);
    let big = "x".repeat(BODY_LIMIT);
    let (s, _) = h
        .post(&h.phone, vec![ev(1, 0, "ui.tap", json!({"pad": big}))])
        .await;
    assert_eq!(s, 413);
    let (s, _) = h
        .send(
            Method::POST,
            "/v1/debug/events",
            Some(&h.phone),
            Some(json!({"nope": []})),
        )
        .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn posts_are_rate_limited_per_device() {
    let h = harness().await;
    for _ in 0..BATCHES_PER_MINUTE {
        let (s, _) = h.post(&h.phone, vec![]).await;
        assert_eq!(s, 200);
    }
    let (s, b) = h.post(&h.phone, vec![]).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (429, Some("rate_limited"))
    );
    // Another device has its own bucket.
    let (s, _) = h.post(&h.mac, vec![]).await;
    assert_eq!(s, 200);
}

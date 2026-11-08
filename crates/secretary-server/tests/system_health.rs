//! Server health end to end: `POST /v1/system/health` (service token) and
//! `GET /v1/system/health` (device/session), plus the `system` alerts the
//! sentinel posts to `POST /v1/notify` with a replaceable `dedupKey`.
//!
//! Covers: auth on both routes, body validation and the 512 KB cap, event
//! derivation across ok → crit → ok, replays ignored, the stale flag, and
//! "failed" then "recovered" for one check leaving one inbox entry.

use reqwest::{Client, RequestBuilder};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

struct T {
    base: String,
    http: Client,
    token: String,
    svc: String,
    _dir: tempfile::TempDir,
}

impl T {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    /// `seed` may write files into the data dir before the server starts.
    async fn with(seed: impl FnOnce(&std::path::Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        seed(&cfg.data_dir);
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
            http: Client::new(),
            token,
            svc,
            _dir: dir,
        }
    }

    async fn send(rb: RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, bearer: Option<&str>, body: &Value) -> (u16, Value) {
        let mut rb = self
            .http
            .post(format!("{}/v1/system/health", self.base))
            .json(body);
        if let Some(b) = bearer {
            rb = rb.bearer_auth(b);
        }
        Self::send(rb).await
    }

    async fn snapshot(&self, body: &Value) -> (u16, Value) {
        self.post(Some(&self.svc), body).await
    }

    async fn get(&self, bearer: Option<&str>) -> (u16, Value) {
        let mut rb = self.http.get(format!("{}/v1/system/health", self.base));
        if let Some(b) = bearer {
            rb = rb.bearer_auth(b);
        }
        Self::send(rb).await
    }

    async fn view(&self) -> Value {
        let (s, v) = self.get(Some(&self.token)).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    async fn notify(&self, body: Value) -> (u16, Value) {
        Self::send(
            self.http
                .post(format!("{}/v1/notify", self.base))
                .bearer_auth(&self.svc)
                .json(&body),
        )
        .await
    }
}

fn check(id: &str, state: &str, detail: &str) -> Value {
    json!({
        "id": id, "kind": "unit", "name": id.trim_start_matches("unit:").trim_end_matches(".service"),
        "group": "Lucina", "state": state, "detail": detail,
        "since": "2026-10-27T09:00:00Z", "restarts_1h": 0
    })
}

fn snap(at: &str, checks: Vec<Value>) -> Value {
    let overall = if checks.iter().any(|c| c["state"] == "crit") {
        "crit"
    } else {
        "ok"
    };
    json!({
        "host": "nezopt",
        "at": at,
        "overall": overall,
        "metrics": {
            "disks": [{"mount": "/", "used_pct": 59.1, "free_gb": 736.0}],
            "cpu_temp_c": 71.5,
            "load": [1.2, 1.4, 1.6],
            "mem_used_pct": 62.0,
            "swap_used_pct": 10.0
        },
        "checks": checks
    })
}

#[tokio::test]
async fn auth_is_required_on_both_routes() {
    let t = T::new().await;
    let body = snap(
        "2026-10-27T10:00:00Z",
        vec![check("unit:a.service", "ok", "active")],
    );
    assert_eq!(t.post(None, &body).await.0, 401);
    // A device token cannot post snapshots…
    assert_eq!(t.post(Some(&t.token), &body).await.0, 401);
    assert_eq!(
        t.post(Some("svc_wrongwrongwrongwrongwrongwrongwrong"), &body)
            .await
            .0,
        401
    );
    // …and the service token cannot read them.
    assert_eq!(t.get(None).await.0, 401);
    assert_eq!(t.get(Some(&t.svc)).await.0, 401);
    assert_eq!(t.snapshot(&body).await.0, 200);
    let v = t.view().await;
    assert_eq!(v["hosts"].as_array().unwrap().len(), 1);
    assert_eq!(v["hosts"][0]["host"], "nezopt");
    assert_eq!(v["hosts"][0]["snapshot"], body);
    assert_eq!(v["hosts"][0]["stale"], false);
    assert_eq!(v["events"], json!([]));
}

#[tokio::test]
async fn body_validation_and_size_cap() {
    let t = T::new().await;
    let good = snap(
        "2026-10-27T10:00:00Z",
        vec![check("unit:a.service", "ok", "active")],
    );
    let mut bad = Vec::new();
    for (path, value) in [
        ("/host", json!("bad host")),
        ("/host", json!("")),
        ("/at", json!("yesterday")),
        ("/overall", json!("fine")),
        ("/metrics/mem_used_pct", json!(140.0)),
        ("/metrics/load", json!([1.0, 2.0])),
        ("/metrics/disks/0/used_pct", json!(-1)),
        ("/checks/0/state", json!("down")),
        ("/checks/0/kind", json!("process")),
        ("/checks/0/group", json!("Games")),
        ("/checks/0/since", json!("soon")),
        ("/checks/0/id", json!("")),
    ] {
        let mut b = good.clone();
        *b.pointer_mut(path).unwrap() = value;
        bad.push(b);
    }
    let mut dup = good.clone();
    dup["checks"] = json!([check("unit:a", "ok", ""), check("unit:a", "ok", "")]);
    bad.push(dup);
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("checks");
    bad.push(missing);
    for b in &bad {
        let (s, v) = t.snapshot(b).await;
        assert_eq!(s, 400, "{b} → {v}");
    }
    // cpu_temp_c may be null.
    let mut nul = good.clone();
    nul["metrics"]["cpu_temp_c"] = Value::Null;
    assert_eq!(t.snapshot(&nul).await.0, 200);

    // Over 512 KB → 413; just under the cap is fine.
    let pad = |n: usize| {
        let mut b = snap("2026-10-27T10:10:00Z", vec![]);
        b["checks"] = json!((0..n)
            .map(|i| check(&format!("unit:svc-{i:05}.service"), "ok", &"x".repeat(200)))
            .collect::<Vec<_>>());
        b
    };
    let big = pad(1500);
    assert!(serde_json::to_vec(&big).unwrap().len() > 512 * 1024);
    let (s, v) = t.snapshot(&big).await;
    assert_eq!(s, 413, "{v}");
    let fits = pad(1000);
    assert!(serde_json::to_vec(&fits).unwrap().len() < 512 * 1024);
    assert_eq!(t.snapshot(&fits).await.0, 200);
}

#[tokio::test]
async fn events_follow_state_changes() {
    let t = T::new().await;
    let a = "unit:lucina-rs.service";
    let b = "unit:kokoro.service";
    let s1 = snap(
        "2026-10-27T10:00:00Z",
        vec![check(a, "ok", "active"), check(b, "ok", "active")],
    );
    let (s, v) = t.snapshot(&s1).await;
    assert_eq!((s, v["events"].as_u64()), (200, Some(0)));
    let s2 = snap(
        "2026-10-27T10:05:00Z",
        vec![
            check(a, "crit", "restarted 12 times in 1 h"),
            check(b, "ok", "active"),
        ],
    );
    let (s, v) = t.snapshot(&s2).await;
    assert_eq!((s, v["events"].as_u64()), (200, Some(1)));
    // The same snapshot again (a retry) and an older one change nothing.
    assert_eq!(t.snapshot(&s2).await.1["stored"], false);
    assert_eq!(t.snapshot(&s1).await.1["stored"], false);
    let s3 = snap(
        "2026-10-27T10:10:00Z",
        vec![check(a, "ok", "active"), check(b, "warn", "slow")],
    );
    assert_eq!(t.snapshot(&s3).await.1["events"], 2);

    let v = t.view().await;
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "{v}");
    // Newest first; the two from 10:10 then the crit from 10:05.
    assert_eq!(events[2]["check_id"], a);
    assert_eq!(events[2]["name"], "lucina-rs");
    assert_eq!(
        (&events[2]["from"], &events[2]["to"]),
        (&json!("ok"), &json!("crit"))
    );
    assert_eq!(events[2]["detail"], "restarted 12 times in 1 h");
    assert_eq!(events[2]["host"], "nezopt");
    assert_eq!(events[2]["at"], "2026-10-27T10:05:00Z");
    let recovered = events[..2]
        .iter()
        .find(|e| e["check_id"] == a)
        .expect("recovery event");
    assert_eq!(
        (&recovered["from"], &recovered["to"]),
        (&json!("crit"), &json!("ok"))
    );
    assert!(events[..2]
        .iter()
        .any(|e| e["check_id"] == b && e["to"] == "warn"));
    assert_eq!(v["hosts"][0]["snapshot"], s3);
}

#[tokio::test]
async fn stale_after_fifteen_minutes() {
    let old = (chrono::Utc::now() - chrono::Duration::minutes(16)).to_rfc3339();
    let fresh = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    let t = T::with(|dir| {
        let entry = |host: &str, received: &str| {
            let mut s = snap(
                "2026-10-27T10:00:00Z",
                vec![check("unit:a", "ok", "active")],
            );
            s["host"] = json!(host);
            json!({"received_at": received, "snapshot": s})
        };
        let state = json!({
            "hosts": {"nezopt": entry("nezopt", &old), "p1": entry("p1", &fresh)},
            "events": []
        });
        std::fs::write(dir.join("system-health.json"), state.to_string()).unwrap();
    })
    .await;
    let v = t.view().await;
    let hosts = v["hosts"].as_array().unwrap();
    let stale = |h: &str| hosts.iter().find(|x| x["host"] == h).unwrap()["stale"].clone();
    assert_eq!(stale("nezopt"), true);
    assert_eq!(stale("p1"), false);
    // A new snapshot makes it fresh again.
    assert_eq!(
        t.snapshot(&snap(
            "2026-10-27T10:30:00Z",
            vec![check("unit:a", "ok", "active")]
        ))
        .await
        .0,
        200
    );
    let v = t.view().await;
    let n = v["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["host"] == "nezopt")
        .unwrap()
        .clone();
    assert_eq!(n["stale"], false);
}

#[tokio::test]
async fn system_alert_is_replaced_by_its_recovery() {
    let t = T::new().await;
    // System notifications are off by default; turn them on.
    let (s, settings) = T::send(
        t.http
            .get(format!("{}/v1/notifications/settings", t.base))
            .bearer_auth(&t.token),
    )
    .await;
    assert_eq!(s, 200);
    let mut settings = settings["settings"].clone();
    settings["categories"]["system"] = json!(true);
    let (s, v) = T::send(
        t.http
            .put(format!("{}/v1/notifications/settings", t.base))
            .bearer_auth(&t.token)
            .json(&settings),
    )
    .await;
    assert_eq!(s, 200, "{v}");

    let alert = |title: &str| {
        json!({"category": "system", "title": title, "body": "restarted 12 times in 1 h",
               "dedupKey": "nezopt:unit:lucina-rs.service", "route": "/server-health"})
    };
    let (s, v) = t.notify(alert("nezopt: lucina-rs failed")).await;
    assert_eq!((s, v["status"].as_str()), (201, Some("created")), "{v}");
    assert_eq!(v["notification"]["route"], "/server-health");
    let (s, v) = t.notify(alert("nezopt: lucina-rs recovered")).await;
    assert_eq!((s, v["status"].as_str()), (201, Some("created")), "{v}");
    // Failing again later is a new alert, not a duplicate.
    let (s, _) = t.notify(alert("nezopt: lucina-rs failed")).await;
    assert_eq!(s, 201);

    let (_, inbox) = T::send(
        t.http
            .get(format!("{}/v1/notifications", t.base))
            .bearer_auth(&t.token),
    )
    .await;
    let items = inbox["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{inbox}");
    assert_eq!(items[0]["title"], "nezopt: lucina-rs failed");

    // Other categories still drop a repeated key.
    let r = json!({"category": "reminder", "body": "x", "dedupKey": "reminder:1"});
    assert_eq!(t.notify(r.clone()).await.0, 201);
    assert_eq!(t.notify(r).await.1["status"], "duplicate");
}

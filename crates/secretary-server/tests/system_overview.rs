//! Server overview end to end: `GET /v1/system/overview` needs an owner device,
//! and with Lucina (so agentd's catalog and the ops record) unreachable or not
//! configured it still answers 200 with the sentinel's part, the failures in
//! `sources`; the response is cached and `?fresh=1` refetches.

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
    /// `lucina_token`: `None` leaves Lucina unconfigured; with one, Lucina is
    /// a closed port.
    async fn new(lucina_token: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", lucina_token.map(str::to_owned));
        cfg.data_dir = dir.path().join("state");
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
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

    async fn overview(&self, bearer: Option<&str>, query: &str) -> (u16, Value) {
        let mut rb = self
            .http
            .get(format!("{}/v1/system/overview{query}", self.base));
        if let Some(b) = bearer {
            rb = rb.bearer_auth(b);
        }
        Self::send(rb).await
    }

    async fn snapshot(&self, checks: Vec<Value>) {
        let body = json!({
            "host": "nezopt",
            "at": chrono::Utc::now().to_rfc3339(),
            "overall": "crit",
            "metrics": {
                "disks": [{"mount": "/", "used_pct": 59.1, "free_gb": 736.0}],
                "cpu_temp_c": 71.5,
                "load": [1.2, 1.4, 1.6],
                "mem_used_pct": 62.0
            },
            "checks": checks
        });
        let (s, v) = Self::send(
            self.http
                .post(format!("{}/v1/system/health", self.base))
                .bearer_auth(&self.svc)
                .json(&body),
        )
        .await;
        assert_eq!(s, 200, "{v}");
    }
}

fn check(name: &str, state: &str, detail: &str) -> Value {
    json!({
        "id": format!("unit:{name}"), "kind": "unit", "name": name, "group": "Lucina",
        "state": state, "detail": detail, "since": "2026-10-27T09:00:00Z"
    })
}

#[tokio::test]
async fn needs_an_owner_device() {
    let t = T::new(None).await;
    assert_eq!(t.overview(None, "").await.0, 401);
    assert_eq!(t.overview(Some(&t.svc), "").await.0, 401);
    assert_eq!(t.overview(Some("sec_wrong"), "").await.0, 401);
    assert_eq!(t.overview(Some(&t.token), "").await.0, 200);
}

#[tokio::test]
async fn sentinel_only_when_lucina_is_unreachable() {
    for token in [None, Some("lucina-token")] {
        let t = T::new(token).await;
        t.snapshot(vec![
            check("gmaps-server", "crit", "inactive (dead) token=abc123"),
            check("honcho", "ok", "active"),
        ])
        .await;
        let (s, v) = t.overview(Some(&t.token), "").await;
        assert_eq!(s, 200, "{v}");
        let want = if token.is_some() { "Lucina unreachable" } else { "Lucina is not configured" };
        assert_eq!(v["sources"]["catalog"]["ok"], false);
        assert_eq!(v["sources"]["catalog"]["error"], want);
        assert_eq!(v["sources"]["ops"]["ok"], false);
        assert_eq!(v["sources"]["sentinel"]["ok"], true);
        assert_eq!(v["overall"], "crit");
        assert_eq!((v["checks_ok"].as_u64(), v["checks_total"].as_u64()), (Some(1), Some(2)));
        assert_eq!(v["attention_count"], 1);
        let a = &v["attention"][0];
        assert_eq!(a["key"], "sentinel:nezopt:unit:gmaps-server");
        assert_eq!(a["sources"], json!(["sentinel"]));
        assert_eq!(a["summary"], "Not running");
        assert_eq!(a["details"], json!(["sentinel: inactive (dead) token=•••"]));
        let h = &v["hosts"][0];
        assert_eq!((h["id"].as_str(), h["state"].as_str()), (Some("nezopt"), Some("ok")));
        assert_eq!(h["load"], json!([1.2, 1.4, 1.6]));
        assert_eq!(v["services"], json!([]));
        assert_eq!(v["timeline"], json!([]));
    }
}

#[tokio::test]
async fn cached_until_fresh() {
    let t = T::new(None).await;
    t.snapshot(vec![check("a", "ok", "active")]).await;
    let (_, first) = t.overview(Some(&t.token), "").await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    // A new snapshot is not seen until the cache is bypassed.
    t.snapshot(vec![check("a", "crit", "failed")]).await;
    let (_, cached) = t.overview(Some(&t.token), "").await;
    assert_eq!(cached, first);
    let (_, fresh) = t.overview(Some(&t.token), "?fresh=1").await;
    assert_ne!(fresh["generated_at"], first["generated_at"]);
    assert_eq!(fresh["attention_count"], 1);
}

#[tokio::test]
async fn nothing_cached_when_every_source_failed() {
    let t = T::new(None).await;
    let (s, v) = t.overview(Some(&t.token), "").await;
    assert_eq!(s, 200);
    assert_eq!(v["sources"]["sentinel"]["ok"], false);
    t.snapshot(vec![check("a", "ok", "active")]).await;
    let (_, v) = t.overview(Some(&t.token), "").await;
    assert_eq!(v["sources"]["sentinel"]["ok"], true, "the empty answer was not cached");
}

//! `/v1/avatar` and `/v1/avatar/wardrobe` on the real router and real files:
//! defaults, partial updates, persistence across a restart, the wardrobe
//! check, CSRF for cookie callers and the service token for Lucina.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct H {
    base: String,
    stop: Option<oneshot::Sender<()>>,
    http: Client,
    cfg: Config,
    token: String,
    service: String,
    _dir: tempfile::TempDir,
}

async fn start(cfg: &Config) -> (String, oneshot::Sender<()>) {
    let mut state = AppState::new(cfg);
    for _ in 0..100 {
        if state.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        state = AppState::new(cfg);
    }
    let state = state.expect("state");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_on(listener, app(state), async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    (base, tx)
}

const WARDROBE: &str = r#"[
  {"id": "uniform", "name": {"it": "Divisa", "en": "Uniform", "da": "Uniform"}, "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["default"]},
  {"id": "gym", "name": {"it": "Palestra", "en": "Gym", "da": "Træning"}, "glb": "lucina-gym.glb", "tags": ["sport"]},
  {"id": "chef", "name": {"en": "Chef"}, "glb": "lucina-chef.glb", "tags": ["work"]},
  {"id": "maid", "name": {"en": "Maid"}, "glb": "lucina-maid.glb", "tags": ["work"]},
  {"id": "broken", "glb": "../../etc/passwd"}
]"#;

impl H {
    async fn new(with_wardrobe: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.private_assets_dir = dir.path().join("assets");
        cfg.rate_limit_per_minute = 10_000;
        cfg.login_rate_per_minute = 10_000;
        std::fs::create_dir_all(&cfg.private_assets_dir).unwrap();
        if with_wardrobe {
            std::fs::write(cfg.private_assets_dir.join("wardrobe.json"), WARDROBE).unwrap();
        }
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let service = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let (base, stop) = start(&cfg).await;
        Self {
            base,
            stop: Some(stop),
            http: Client::builder().pool_max_idle_per_host(0).build().unwrap(),
            cfg,
            token,
            service,
            _dir: dir,
        }
    }

    async fn restart(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (base, stop) = start(&self.cfg).await;
        self.base = base;
        self.stop = Some(stop);
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{path}", self.base))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(Method::GET, path).bearer_auth(&self.token))
            .await
    }

    async fn put(&self, body: Value) -> (StatusCode, Value) {
        self.send(
            self.req(Method::PUT, "/v1/avatar")
                .bearer_auth(&self.token)
                .header("x-secretary-client", "ios")
                .json(&body),
        )
        .await
    }

    async fn cookie(&self) -> String {
        let r = self
            .req(Method::POST, "/v1/session")
            .json(&json!({"token": self.token}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let c = r.headers()["set-cookie"].to_str().unwrap().to_owned();
        c.split(';').next().unwrap().to_owned()
    }
}

#[tokio::test]
async fn defaults_partial_updates_and_persistence() {
    let mut h = H::new(true).await;
    let (s, v) = h.get("/v1/avatar").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outfit"], Value::Null);
    assert_eq!(v["auto"], true);
    assert_eq!(
        v["rules"],
        json!({"gym": "gym", "shopping": "chef", "default": "uniform"})
    );
    assert_eq!(v["updatedMs"], 0);

    let (s, v) = h.put(json!({"rules": {"shopping": "maid"}})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["rules"]["shopping"], "maid");
    assert_eq!(v["rules"]["gym"], "gym");
    assert_eq!(v["updatedBy"], "ios");
    assert!(v["updatedMs"].as_u64().unwrap() > 0);

    let (s, v) = h.put(json!({"outfit": "maid", "auto": false})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["outfit"].as_str(), v["auto"].as_bool()),
        (Some("maid"), Some(false))
    );

    h.restart().await;
    let (_, v) = h.get("/v1/avatar").await;
    assert_eq!(v["outfit"], "maid", "persisted across a restart");
    assert_eq!(v["rules"]["shopping"], "maid");
    let file = h.cfg.data_dir.join("avatar.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let (s, v) = h.put(json!({"outfit": null})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["outfit"], Value::Null, "null unpins");
    assert_eq!(v["auto"], false, "absent fields are kept");
}

#[tokio::test]
async fn validation_and_the_wardrobe() {
    let h = H::new(true).await;
    let (s, v) = h.get("/v1/avatar/wardrobe").await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["uniform", "gym", "chef", "maid"],
        "invalid entries are dropped"
    );
    assert_eq!(v[0]["name"]["it"], "Divisa");

    let (s, v) = h.put(json!({"outfit": "pirate"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "unknown_outfit");
    assert!(v["error"]["message"].as_str().unwrap().contains("maid"));
    let (s, v) = h.put(json!({"rules": {"gym": "pirate"}})).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_outfit"))
    );
    for bad in [
        json!({"outfit": "Not An Id"}),
        json!({"outfit": 3}),
        json!({"auto": "yes"}),
        json!({"rules": {"chat": "maid"}}),
        json!({"colour": "red"}),
    ] {
        let (s, v) = h.put(bad.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} → {v}");
    }
    let (_, v) = h.get("/v1/avatar").await;
    assert_eq!(v["updatedMs"], 0, "refused writes change nothing");
}

#[tokio::test]
async fn any_well_formed_id_without_a_wardrobe() {
    let h = H::new(false).await;
    let (s, v) = h.get("/v1/avatar/wardrobe").await;
    assert_eq!((s, v), (StatusCode::OK, json!([])));
    let (s, v) = h.put(json!({"outfit": "pirate"})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outfit"], "pirate");
}

#[tokio::test]
async fn auth_csrf_and_the_service_token() {
    let h = H::new(true).await;
    let (s, _) = h.send(h.req(Method::GET, "/v1/avatar")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .send(
            h.req(Method::PUT, "/v1/avatar")
                .bearer_auth("sec_nope")
                .json(&json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Browser: the cookie reads freely, writes need the CSRF header.
    let cookie = h.cookie().await;
    let (s, _) = h
        .send(h.req(Method::GET, "/v1/avatar").header("cookie", &cookie))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .send(
            h.req(Method::PUT, "/v1/avatar")
                .header("cookie", &cookie)
                .json(&json!({"outfit": "gym"})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("csrf"))
    );
    let (s, v) = h
        .send(
            h.req(Method::PUT, "/v1/avatar")
                .header("cookie", &cookie)
                .header("x-secretary-csrf", "1")
                .header("x-secretary-client", "web")
                .json(&json!({"outfit": "gym"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["updatedBy"], "web");

    // Lucina: the service token reads and writes here...
    let (s, v) = h
        .send(
            h.req(Method::PUT, "/v1/avatar")
                .bearer_auth(&h.service)
                .json(&json!({"outfit": "maid"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["outfit"].as_str(), v["updatedBy"].as_str()),
        (Some("maid"), Some("lucina"))
    );
    let (s, v) = h
        .send(
            h.req(Method::GET, "/v1/avatar/wardrobe")
                .bearer_auth(&h.service),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 4);
    let (s, _) = h
        .send(
            h.req(Method::PUT, "/v1/avatar")
                .bearer_auth("svc_wrong_wrong_wrong_wrong_wrong_wrong")
                .json(&json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // ...and still nowhere else.
    let (s, _) = h
        .send(h.req(Method::GET, "/v1/gym/state").bearer_auth(&h.service))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

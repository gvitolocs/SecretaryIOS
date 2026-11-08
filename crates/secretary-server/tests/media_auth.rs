//! `GET /v1/auth/media`, the forward_auth target of `/v1/media`: 204 for a
//! device session or Lucina's service token, 401 otherwise, and a bucket of its
//! own so a screen of thumbnails does not eat the app's 60 requests a minute.

use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::{Device, DeviceStore};
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct H {
    base: String,
    _stop: oneshot::Sender<()>,
    http: Client,
    store: DeviceStore,
    device: Device,
    token: String,
    service: String,
    _dir: tempfile::TempDir,
}

impl H {
    async fn new(media_per_minute: u32, general_per_minute: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.rate_limit_per_minute = general_per_minute;
        cfg.media_auth_per_minute = media_per_minute;
        cfg.login_rate_per_minute = 10_000;
        let store = DeviceStore::new(&cfg.data_dir);
        let (device, token) = store.issue("test iPhone").unwrap();
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let service = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let state = AppState::new(&cfg).expect("state");
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
        Self {
            base,
            _stop: tx,
            http: Client::builder().pool_max_idle_per_host(0).build().unwrap(),
            store,
            device,
            token,
            service,
            _dir: dir,
        }
    }

    fn check(&self) -> reqwest::RequestBuilder {
        self.http
            .request(Method::GET, format!("{}/v1/auth/media", self.base))
    }

    async fn status(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn no_or_bad_credentials_are_401() {
    let h = H::new(600, 600).await;
    assert_eq!(h.status(h.check()).await.0, StatusCode::UNAUTHORIZED);
    for token in [
        "nope",
        "sec_notarealtoken",
        "svc_0123456789012345678901234567890123456789",
    ] {
        assert_eq!(
            h.status(h.check().bearer_auth(token)).await.0,
            StatusCode::UNAUTHORIZED,
            "{token}"
        );
    }
    assert_eq!(
        h.status(h.check().header("cookie", "sec_session=bogus"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_revoked_device_is_401() {
    let h = H::new(600, 600).await;
    assert_eq!(
        h.status(h.check().bearer_auth(&h.token)).await.0,
        StatusCode::NO_CONTENT
    );
    // the registry re-reads on an mtime/size change; give the clock a tick
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    h.store.revoke(&h.device.id).unwrap().unwrap();
    assert_eq!(
        h.status(h.check().bearer_auth(&h.token)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn devices_cookies_and_lucina_pass_as_they_do_on_the_old_check() {
    let h = H::new(600, 600).await;
    let r = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()["set-cookie"].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    for rb in [
        h.check().bearer_auth(&h.token),
        h.check().header("cookie", &cookie),
        h.check().bearer_auth(&h.service),
    ] {
        assert_eq!(h.status(rb).await.0, StatusCode::NO_CONTENT);
    }
    // HEAD too: Caddy may ask either way
    let r = h
        .http
        .request(Method::HEAD, format!("{}/v1/auth/media", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_screen_of_thumbnails_leaves_the_apps_own_requests_alone() {
    // 3 general requests a minute, 500 for media
    let h = H::new(500, 3).await;
    for _ in 0..100 {
        assert_eq!(
            h.status(h.check().bearer_auth(&h.token)).await.0,
            StatusCode::NO_CONTENT
        );
    }
    for _ in 0..3 {
        let rb = h
            .http
            .get(format!("{}/v1/shopping", h.base))
            .bearer_auth(&h.token);
        assert_ne!(h.status(rb).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
}

#[tokio::test]
async fn the_media_bucket_still_limits() {
    let h = H::new(3, 600).await;
    for _ in 0..3 {
        assert_eq!(
            h.status(h.check().bearer_auth(&h.token)).await.0,
            StatusCode::NO_CONTENT
        );
    }
    let (s, b) = h.status(h.check().bearer_auth(&h.token)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(b["error"]["code"], "rate_limited");
}

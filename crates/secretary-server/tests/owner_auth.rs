//! `GET /v1/auth/owner`, the forward_auth target of the owner-only plugins:
//! 204 for a device session (Bearer or cookie), 401 without or with revoked
//! credentials, 403 `owner_only` for Lucina's service token, and a bucket of
//! its own so plugin calls do not eat the app's 60 requests a minute.

use std::time::Duration;

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
    async fn new(owner_per_minute: u32, general_per_minute: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.rate_limit_per_minute = general_per_minute;
        cfg.owner_auth_per_minute = owner_per_minute;
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

    fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{path}", self.base))
    }

    async fn status(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
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
async fn no_auth_is_401() {
    let h = H::new(600, 600).await;
    let (s, b) = h.status(h.req(Method::GET, "/v1/auth/owner")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["error"]["code"], "unauthorized");
    // garbage and unknown tokens too
    let (s, _) = h
        .status(h.req(Method::GET, "/v1/auth/owner").bearer_auth("nope"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .status(
            h.req(Method::GET, "/v1/auth/owner")
                .bearer_auth("sec_notarealtoken"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .status(
            h.req(Method::GET, "/v1/auth/owner")
                .header("cookie", "sec_session=bogus"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn service_token_is_403_owner_only() {
    let h = H::new(600, 600).await;
    let (s, b) = h
        .status(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.service))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(b["error"]["code"], "owner_only");
    // a wrong service token is just unauthenticated
    let (s, _) = h
        .status(
            h.req(Method::GET, "/v1/auth/owner")
                .bearer_auth("svc_0123456789012345678901234567890123456789"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn device_bearer_is_204_for_get_and_head() {
    let h = H::new(600, 600).await;
    for m in [Method::GET, Method::HEAD] {
        let r = h
            .req(m, "/v1/auth/owner")
            .bearer_auth(&h.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(r.bytes().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn cookie_session_is_204() {
    let h = H::new(600, 600).await;
    let cookie = h.cookie().await;
    let (s, _) = h
        .status(
            h.req(Method::GET, "/v1/auth/owner")
                .header("cookie", &cookie),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn revoked_device_is_401_for_bearer_and_cookie() {
    let h = H::new(600, 600).await;
    let cookie = h.cookie().await;
    let get = |rb: reqwest::RequestBuilder| h.status(rb);
    assert_eq!(
        get(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.token))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // the registry re-reads on an mtime/size change; give the clock a tick
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.store.revoke(&h.device.id).unwrap().unwrap();
    let (s, b) = get(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.token)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["error"]["message"], "device token revoked");
    let (s, _) = get(h
        .req(Method::GET, "/v1/auth/owner")
        .header("cookie", &cookie))
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn own_bucket_does_not_touch_the_general_limit() {
    // 3 general requests a minute, 50 for the owner check
    let h = H::new(50, 3).await;
    for _ in 0..20 {
        let (s, _) = h
            .status(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.token))
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    // the app's own bucket is untouched: its 3 requests still go through
    for _ in 0..3 {
        let (s, _) = h
            .status(h.req(Method::GET, "/v1/shopping").bearer_auth(&h.token))
            .await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS);
    }
}

#[tokio::test]
async fn the_owner_bucket_still_limits() {
    let h = H::new(3, 600).await;
    for _ in 0..3 {
        let (s, _) = h
            .status(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.token))
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (s, b) = h
        .status(h.req(Method::GET, "/v1/auth/owner").bearer_auth(&h.token))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(b["error"]["code"], "rate_limited");
}

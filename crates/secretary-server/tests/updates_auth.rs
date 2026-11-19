//! `GET /v1/auth/updates`, the forward_auth target of `/v1/updates` (lucina-updates): the same
//! check as `/v1/auth/media` (204 for a device session, 401 for no, unknown or revoked
//! credentials), in a bucket of its own.

use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::{Device, DeviceStore};
use secretary_server::{app, serve_on, AppState};
use tokio::sync::oneshot;

struct H {
    base: String,
    _stop: oneshot::Sender<()>,
    http: Client,
    store: DeviceStore,
    device: Device,
    token: String,
    _dir: tempfile::TempDir,
}

impl H {
    async fn new(per_minute: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.media_auth_per_minute = per_minute;
        cfg.login_rate_per_minute = 10_000;
        let store = DeviceStore::new(&cfg.data_dir);
        let (device, token) = store.issue("test iPhone").unwrap();
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
            _dir: dir,
        }
    }

    fn check(&self) -> reqwest::RequestBuilder {
        self.http
            .request(Method::GET, format!("{}/v1/auth/updates", self.base))
    }
}

#[tokio::test]
async fn no_token_is_401() {
    let h = H::new(600).await;
    assert_eq!(
        h.check().send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_unknown_token_is_401() {
    let h = H::new(600).await;
    for token in [
        "nope",
        "sec_notarealtoken",
        "svc_0123456789012345678901234567890123456789",
    ] {
        let r = h.check().bearer_auth(token).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{token}");
    }
    let r = h
        .check()
        .header("cookie", "sec_session=bogus")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_revoked_token_is_401_and_a_valid_one_passes() {
    let h = H::new(600).await;
    let r = h.check().bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = h
        .http
        .request(Method::HEAD, format!("{}/v1/auth/updates", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    // the registry re-reads on an mtime/size change; give the clock a tick
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    h.store.revoke(&h.device.id).unwrap().unwrap();
    let r = h.check().bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_updates_bucket_is_its_own() {
    let h = H::new(3).await;
    for _ in 0..3 {
        let r = h.check().bearer_auth(&h.token).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }
    let r = h.check().bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    // the media bucket is untouched
    let r = h
        .http
        .get(format!("{}/v1/auth/media", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
}

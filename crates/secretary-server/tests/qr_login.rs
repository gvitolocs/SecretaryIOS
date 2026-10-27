//! QR sign-in: a signed-out browser asks for a code, a signed-in device
//! approves it, the browser's poll collects the session cookie.

use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::owner::OwnerStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const EMAIL: &str = "owner@example.com";
const PASSWORD: &str = "correct horse battery staple";
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

struct H {
    base: String,
    http: Client,
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
}

async fn harness(tweak: impl FnOnce(&mut Config)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("state");
    let mut cfg = Config::new("http://127.0.0.1:9", None);
    cfg.data_dir = data.clone();
    cfg.login_rate_per_minute = 1000;
    cfg.public_url = "https://secretary.example".into();
    tweak(&mut cfg);
    OwnerStore::new(&data).set(EMAIL, PASSWORD, false).unwrap();
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
        http: Client::new(),
        _dir: dir,
        data,
    }
}

struct Started {
    code: String,
    poll_token: String,
}

fn sid_from(h: &HeaderMap) -> Option<String> {
    let cookie = h.get("set-cookie")?.to_str().ok()?;
    cookie
        .split(';')
        .next()?
        .strip_prefix("sec_session=")
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

impl H {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value, HeaderMap) {
        let r = rb.send().await.unwrap();
        let (s, h) = (r.status(), r.headers().clone());
        (s, r.json().await.unwrap_or(Value::Null), h)
    }

    /// The signed-out browser's `POST /v1/session/qr`.
    async fn start(&self) -> Started {
        let (s, v, h) = self
            .send(
                self.http
                    .post(self.url("/v1/session/qr"))
                    .header("user-agent", BROWSER_UA),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert!(h.get("set-cookie").is_none());
        let code = v["code"].as_str().unwrap().to_owned();
        assert_eq!(
            v["approveUrl"],
            format!("https://secretary.example/approve?c={code}")
        );
        assert_eq!(v["expiresIn"], 120);
        Started {
            code,
            poll_token: v["pollToken"].as_str().unwrap().to_owned(),
        }
    }

    /// `(status, session id if a cookie was set)`.
    async fn poll(&self, token: &str) -> (String, Option<String>) {
        let (s, v, h) = self
            .send(
                self.http
                    .get(self.url("/v1/session/qr/poll"))
                    .query(&[("t", token)]),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        (v["status"].as_str().unwrap().to_owned(), sid_from(&h))
    }

    fn device_token(&self) -> String {
        DeviceStore::new(&self.data).issue("Phone").unwrap().1
    }

    async fn approve_with_bearer(&self, token: &str, code: &str, approve: bool) -> (StatusCode, Value) {
        let (s, v, _) = self
            .send(
                self.http
                    .post(self.url("/v1/session/qr/approve"))
                    .bearer_auth(token)
                    .json(&json!({"code": code, "approve": approve})),
            )
            .await;
        (s, v)
    }

    async fn session_with(&self, sid: &str) -> Value {
        self.send(
            self.http
                .get(self.url("/v1/session"))
                .header("cookie", format!("sec_session={sid}")),
        )
        .await
        .1
    }
}

#[tokio::test]
async fn approved_request_signs_the_browser_in() {
    let h = harness(|_| {}).await;
    let req = h.start().await;

    // The QR image is an SVG of the approve link.
    let r = h
        .http
        .get(h.url("/v1/session/qr/image"))
        .query(&[("c", &req.code)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "image/svg+xml");
    assert!(r.text().await.unwrap().contains("<svg"));

    assert_eq!(h.poll(&req.poll_token).await, ("pending".into(), None));

    // The phone sees where the request came from.
    let phone = h.device_token();
    let (s, info, _) = h
        .send(
            h.http
                .get(h.url("/v1/session/qr/info"))
                .bearer_auth(&phone)
                .query(&[("c", &req.code)]),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{info}");
    assert_eq!(info["ip"], "127.0.0.1");
    assert_eq!(info["userAgent"], BROWSER_UA);
    assert!(info["createdAt"].as_str().unwrap().contains('T'));

    let (s, v) = h.approve_with_bearer(&phone, &req.code, true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"status": "approved"}));

    let (status, sid) = h.poll(&req.poll_token).await;
    assert_eq!(status, "approved");
    let sid = sid.expect("approved poll sets the session cookie");
    let session = h.session_with(&sid).await;
    assert_eq!(session["authenticated"], true);
    assert_eq!(session["deviceName"], "web");
    // The session works on protected routes.
    let r = h
        .http
        .get(h.url("/v1/body/state"))
        .header("cookie", format!("sec_session={sid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_request_is_collected_only_once() {
    let h = harness(|_| {}).await;
    let req = h.start().await;
    let phone = h.device_token();
    h.approve_with_bearer(&phone, &req.code, true).await;
    assert!(h.poll(&req.poll_token).await.1.is_some());
    assert_eq!(h.poll(&req.poll_token).await, ("expired".into(), None));
    // Neither can it be answered again.
    let (s, v) = h.approve_with_bearer(&phone, &req.code, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "qr_not_found");
}

#[tokio::test]
async fn an_answered_request_cannot_be_answered_again() {
    let h = harness(|_| {}).await;
    let req = h.start().await;
    let phone = h.device_token();
    h.approve_with_bearer(&phone, &req.code, false).await;
    let (s, v) = h.approve_with_bearer(&phone, &req.code, true).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "qr_already_decided");
    assert_eq!(h.poll(&req.poll_token).await, ("denied".into(), None));
}

#[tokio::test]
async fn requests_expire() {
    let h = harness(|c| c.qr_login_ttl = Duration::from_millis(300)).await;
    let r = h
        .http
        .post(h.url("/v1/session/qr"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let (code, token) = (r["code"].as_str().unwrap(), r["pollToken"].as_str().unwrap());
    tokio::time::sleep(Duration::from_millis(400)).await;
    let phone = h.device_token();
    let (s, _) = h.approve_with_bearer(&phone, code, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = h
        .send(
            h.http
                .get(h.url("/v1/session/qr/image"))
                .query(&[("c", code)]),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(h.poll(token).await, ("expired".into(), None));
}

#[tokio::test]
async fn a_wrong_poll_token_learns_nothing() {
    let h = harness(|_| {}).await;
    let req = h.start().await;
    let phone = h.device_token();
    h.approve_with_bearer(&phone, &req.code, true).await;
    // Not a token, the QR code itself, or a missing parameter.
    assert_eq!(h.poll("AAAAAAAAAAAAAAAAAAAAAA").await, ("expired".into(), None));
    assert_eq!(h.poll(&req.code).await, ("expired".into(), None));
    let (s, _, h2) = h.send(h.http.get(h.url("/v1/session/qr/poll"))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(h2.get("set-cookie").is_none());
    // The real token still collects it.
    assert!(h.poll(&req.poll_token).await.1.is_some());
}

#[tokio::test]
async fn approving_needs_a_signed_in_device() {
    let h = harness(|_| {}).await;
    let req = h.start().await;
    let (s, _, _) = h
        .send(
            h.http
                .post(h.url("/v1/session/qr/approve"))
                .header("x-secretary-csrf", "1")
                .json(&json!({"code": req.code, "approve": true})),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h
        .send(
            h.http
                .get(h.url("/v1/session/qr/info"))
                .query(&[("c", &req.code)]),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // An unknown or revoked device token is no better.
    let (s, _) = h
        .approve_with_bearer("sec_notarealtoken", &req.code, true)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(h.poll(&req.poll_token).await, ("pending".into(), None));
}

#[tokio::test]
async fn cookie_approvals_need_csrf() {
    let h = harness(|_| {}).await;
    // The phone is a browser signed in with the password.
    let (s, _, headers) = h
        .send(
            h.http
                .post(h.url("/v1/session"))
                .json(&json!({"email": EMAIL, "password": PASSWORD})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let phone_sid = sid_from(&headers).unwrap();
    let req = h.start().await;
    let approve = |csrf: bool| {
        let mut rb = h
            .http
            .request(Method::POST, h.url("/v1/session/qr/approve"))
            .header("cookie", format!("sec_session={phone_sid}"))
            .json(&json!({"code": req.code, "approve": true}));
        if csrf {
            rb = rb.header("x-secretary-csrf", "1");
        }
        rb
    };
    let (s, v, _) = h.send(approve(false)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "csrf");
    assert_eq!(h.poll(&req.poll_token).await, ("pending".into(), None));
    let (s, v, _) = h.send(approve(true)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(h.poll(&req.poll_token).await.1.is_some());
}

#[tokio::test]
async fn a_denied_request_never_yields_a_session() {
    let h = harness(|_| {}).await;
    let req = h.start().await;
    let phone = h.device_token();
    let (s, v) = h.approve_with_bearer(&phone, &req.code, false).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"status": "denied"}));
    assert_eq!(h.poll(&req.poll_token).await, ("denied".into(), None));
    assert_eq!(h.poll(&req.poll_token).await, ("expired".into(), None));
}

#[tokio::test]
async fn starting_a_request_counts_as_a_login_attempt() {
    let h = harness(|c| c.login_rate_per_minute = 5).await;
    for _ in 0..5 {
        h.start().await;
    }
    let (s, v, _) = h.send(h.http.post(h.url("/v1/session/qr"))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    // Polling has its own bucket.
    assert_eq!(h.poll("AAAAAAAAAAAAAAAAAAAAAA").await.0, "expired");
}

//! Email + password login for the single owner account.

use reqwest::{Client, StatusCode};
use secretary_server::cli::{run_user, UserAction};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::owner::OwnerStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const EMAIL: &str = "owner@example.com";
const PASSWORD: &str = "correct horse battery staple";

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

fn generous(cfg: &mut Config) {
    cfg.login_rate_per_minute = 1000;
}

impl H {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }

    async fn post(
        &self,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let r = self
            .http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap();
        let (s, h) = (r.status(), r.headers().clone());
        (s, r.json().await.unwrap_or(Value::Null), h)
    }

    /// Password login; returns the session cookie value.
    async fn login(&self, password: &str) -> Result<String, (StatusCode, Value)> {
        let (s, v, h) = self
            .post("/v1/session", json!({"email": EMAIL, "password": password}))
            .await;
        if s != StatusCode::OK {
            return Err((s, v));
        }
        let device_id = DeviceStore::new(&self.data)
            .list().unwrap().into_iter().find(|d| d.web_login).unwrap().id;
        assert_eq!(v, json!({"authenticated": true, "deviceId": device_id, "deviceName": "web"}));
        let cookie = h["set-cookie"].to_str().unwrap();
        Ok(cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix("sec_session=")
            .unwrap()
            .to_owned())
    }

    async fn with_cookie(
        &self,
        method: reqwest::Method,
        path: &str,
        sid: &str,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("cookie", format!("sec_session={sid}"))
    }
}

fn invalid(v: &Value) {
    assert_eq!(v["error"]["code"], "invalid_credentials", "{v}");
}

#[tokio::test]
async fn password_login_sets_a_web_session_cookie() {
    let h = harness(generous).await;
    let sid = h.login(PASSWORD).await.unwrap();
    let r = h
        .with_cookie(reqwest::Method::GET, "/v1/body/state", &sid)
        .await
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = h
        .with_cookie(reqwest::Method::GET, "/v1/session", &sid)
        .await
        .send()
        .await
        .unwrap();
    let device_id = DeviceStore::new(&h.data)
        .list().unwrap().into_iter().find(|d| d.web_login).unwrap().id;
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({"authenticated": true, "deviceId": device_id, "deviceName": "web"})
    );
    // a second login reuses the one "web" device
    let _ = h.login(PASSWORD).await.unwrap();
    let devices = DeviceStore::new(&h.data).list().unwrap();
    assert_eq!(devices.iter().filter(|d| d.web_login).count(), 1);
    // email is case-insensitive
    let (s, _, _) = h
        .post(
            "/v1/session",
            json!({"email": "OWNER@Example.COM", "password": PASSWORD}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn every_failure_is_the_same_401() {
    let h = harness(generous).await;
    let (s, v) = h.login("wrong password here").await.unwrap_err();
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    invalid(&v);
    let (s, wrong_email, _) = h
        .post(
            "/v1/session",
            json!({"email": "nobody@example.com", "password": PASSWORD}),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_email, v);
    let (s, v, _) = h.post("/v1/session", json!({"email": EMAIL})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    // no owner at all
    std::fs::remove_file(h.data.join("owner.json")).unwrap();
    let (s, v) = h.login(PASSWORD).await.unwrap_err();
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    invalid(&v);
}

#[tokio::test]
async fn device_token_login_keeps_working() {
    let h = harness(generous).await;
    let (_, token) = DeviceStore::new(&h.data).issue("script").unwrap();
    let (s, v, _) = h.post("/v1/session", json!({"token": token})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["deviceName"], "script");
}

#[tokio::test]
async fn per_ip_login_limit_still_applies() {
    let h = harness(|_| {}).await;
    for _ in 0..5 {
        let (s, _) = h.login("wrong password here").await.unwrap_err();
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, v) = h.login(PASSWORD).await.unwrap_err();
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "rate_limited");
    let (s, _, _) = h
        .post(
            "/v1/auth/device",
            json!({"email": EMAIL, "password": PASSWORD, "deviceName": "iPhone"}),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn ten_failures_lock_the_account_but_not_device_tokens() {
    let h = harness(generous).await;
    for i in 0..10 {
        // wrong emails count too
        let email = if i % 2 == 0 { EMAIL } else { "x@example.com" };
        let (s, v, _) = h
            .post(
                "/v1/session",
                json!({"email": email, "password": "wrong password here"}),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "attempt {i}");
        invalid(&v);
    }
    let (s, v) = h.login(PASSWORD).await.unwrap_err();
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "account_locked");
    let (_, _, headers) = h
        .post("/v1/session", json!({"email": EMAIL, "password": PASSWORD}))
        .await;
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((800..=900).contains(&retry), "{retry}");
    let (s, v, _) = h
        .post(
            "/v1/auth/device",
            json!({"email": EMAIL, "password": PASSWORD, "deviceName": "iPhone"}),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "account_locked");
    let (_, token) = DeviceStore::new(&h.data).issue("script").unwrap();
    let (s, _, _) = h.post("/v1/session", json!({"token": token})).await;
    assert_eq!(s, StatusCode::OK);
    let owner = OwnerStore::new(&h.data).load().unwrap().unwrap();
    assert_eq!(owner.failed_count, 10);
    assert!(owner.locked_until.is_some());
}

#[tokio::test]
async fn native_apps_trade_the_password_for_a_device_token() {
    let h = harness(generous).await;
    let (s, v, _) = h
        .post(
            "/v1/auth/device",
            json!({"email": EMAIL, "password": PASSWORD, "deviceName": "Giuseppe's iPhone"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let token = v["token"].as_str().unwrap();
    assert!(token.starts_with("sec_"));
    assert!(v["deviceId"].as_str().unwrap().starts_with("dev_"));
    assert_eq!(v["deviceName"], "Giuseppe's iPhone");
    let raw = std::fs::read_to_string(h.data.join("devices.json")).unwrap();
    assert!(!raw.contains(token));
    let r = h
        .http
        .get(h.url("/v1/gym/state"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let (s, v, _) = h
        .post(
            "/v1/auth/device",
            json!({"email": EMAIL, "password": "wrong password here", "deviceName": "x"}),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    invalid(&v);
    let (s, v, _) = h
        .post(
            "/v1/auth/device",
            json!({"email": EMAIL, "password": PASSWORD, "deviceName": "  "}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn changing_the_password_ends_other_sessions() {
    let h = harness(generous).await;
    let a = h.login(PASSWORD).await.unwrap();
    let b = h.login(PASSWORD).await.unwrap();
    let new_pw = "an even better passphrase";
    let change = |sid: &str, current: &str, new: &str, csrf: bool| {
        let mut rb = h
            .http
            .post(h.url("/v1/session/password"))
            .header("cookie", format!("sec_session={sid}"))
            .json(&json!({"currentPassword": current, "newPassword": new}));
        if csrf {
            rb = rb.header("x-secretary-csrf", "1");
        }
        rb
    };
    let r = change(&a, PASSWORD, new_pw, false).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "csrf");
    let r = change(&a, "wrong password here", new_pw, true)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_credentials"
    );
    let r = change(&a, PASSWORD, "short", true).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = change(&a, PASSWORD, new_pw, true).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({"changed": true, "sessionsRevoked": 1})
    );
    let status = |sid: String| {
        let rb = h
            .http
            .get(h.url("/v1/body/state"))
            .header("cookie", format!("sec_session={sid}"));
        async move { rb.send().await.unwrap().status() }
    };
    assert_eq!(status(a.clone()).await, StatusCode::OK);
    assert_eq!(status(b).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        h.login(PASSWORD).await.unwrap_err().0,
        StatusCode::UNAUTHORIZED
    );
    let c = h.login(new_pw).await.unwrap();

    // with a device token: every browser session ends
    let (_, token) = DeviceStore::new(&h.data).issue("script").unwrap();
    let r = h
        .http
        .post(h.url("/v1/session/password"))
        .bearer_auth(&token)
        .json(&json!({"currentPassword": new_pw, "newPassword": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.json::<Value>().await.unwrap()["sessionsRevoked"], 2);
    assert_eq!(status(a).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(c).await, StatusCode::UNAUTHORIZED);
    // unauthenticated → 401
    let r = h
        .http
        .post(h.url("/v1/session/password"))
        .json(&json!({"currentPassword": PASSWORD, "newPassword": new_pw}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn user_cli_set_show_and_single_user_rule() {
    let dir = tempfile::tempdir().unwrap();
    let data = Some(dir.path().to_path_buf());
    let set = |email: &str, password: &str, replace: bool| {
        let mut out = Vec::new();
        let r = run_user(
            data.clone(),
            UserAction::Set {
                email: email.into(),
                password_stdin: true,
                replace,
            },
            &mut format!("{password}\n").as_bytes(),
            &mut out,
        );
        r.map(|()| String::from_utf8(out).unwrap())
    };
    let out = set(EMAIL, PASSWORD, false).unwrap();
    assert!(out.contains(EMAIL) && !out.contains(PASSWORD));
    assert!(set(EMAIL, "short12", false).is_err());
    let err = set("other@example.com", PASSWORD, false).unwrap_err();
    assert!(format!("{err:#}").contains("--replace"));
    set("other@example.com", PASSWORD, true).unwrap();
    let mut out = Vec::new();
    run_user(data.clone(), UserAction::Show, &mut &b""[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("other@example.com"));
    assert!(!out.contains("argon2"));
    let store = OwnerStore::new(dir.path());
    assert_eq!(
        store.verify("other@example.com", PASSWORD).unwrap(),
        secretary_server::owner::Verdict::Ok
    );
}

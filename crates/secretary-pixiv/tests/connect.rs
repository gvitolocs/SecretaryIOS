//! Every network interaction here uses a temporary Unix socket or a local wiremock server.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use secretary_pixiv::{
    app_api::AppBackend,
    backend::{Backend, SwitchBackend},
    broker,
    cache::DiskCache,
    connection::{Config, ConnectError, Connection},
    http::{router, Http},
    model::{parse_illust, parse_ranking, parse_search},
    oauth::{self, Secret, TokenError},
    service::{Config as ServiceConfig, SearchParams, Service},
    store::Store,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

struct FakeBroker {
    cfg: broker::Config,
    calls: Arc<Mutex<Vec<Value>>>,
    approve: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for FakeBroker {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl FakeBroker {
    async fn new(stored: bool, outcome: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("broker.sock");
        let token_file = dir.path().join("caller");
        std::fs::write(&token_file, "caller-secret\n").unwrap();
        let cfg = broker::Config {
            socket: socket.clone(),
            token_file: Some(token_file),
            ..Default::default()
        };
        let listener = UnixListener::bind(socket).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let approve = Arc::new(AtomicBool::new(false));
        let ready = approve.clone();
        let stored = Arc::new(AtomicBool::new(stored));
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let seen = seen.clone();
                let ready = ready.clone();
                let stored = stored.clone();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    let v: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(v["token"], "caller-secret");
                    seen.lock().unwrap().push(v.clone());
                    let result = match v["op"].as_str().unwrap() {
                        "exec" if stored.load(Ordering::SeqCst) => {
                            json!({"ok":true,"result":{"env":{"PIXIV_REFRESH_TOKEN":"stored-refresh"}}})
                        }
                        "exec" | "describe" if !stored.load(Ordering::SeqCst) => {
                            json!({"ok":false,"error":{"code":"not_found","message":"not found"}})
                        }
                        "describe" => json!({"ok":true,"result":{}}),
                        "mutate" if v["confirm_id"].is_null() => {
                            json!({"ok":true,"result":{"pending":{"id":"approval-1","kind":if outcome=="code" {"code"} else {"approval"},"expires_at":chrono::Utc::now().timestamp()+300,"message":"Approve"}}})
                        }
                        "mutate" if outcome == "denied" => {
                            json!({"ok":false,"error":{"code":"confirm_denied","message":"Denied"}})
                        }
                        "mutate" if outcome == "expired" => {
                            json!({"ok":false,"error":{"code":"confirm_expired","message":"Expired"}})
                        }
                        "mutate" if outcome == "failed" => {
                            json!({"ok":false,"error":{"code":"upstream","message":"failed"}})
                        }
                        "mutate" if ready.load(Ordering::SeqCst) => {
                            stored.store(v["action"] != "delete", Ordering::SeqCst);
                            json!({"ok":true,"result":{"ok":true}})
                        }
                        "mutate" => {
                            json!({"ok":false,"error":{"code":"confirm_pending","message":"Waiting"}})
                        }
                        _ => panic!("unknown operation"),
                    };
                    stream
                        .get_mut()
                        .write_all(format!("{result}\n").as_bytes())
                        .await
                        .unwrap();
                });
            }
        });
        Self {
            cfg,
            calls,
            approve,
            task,
            _dir: dir,
        }
    }
    fn release(&self) {
        self.approve.store(true, Ordering::SeqCst);
    }
}
fn token(premium: bool, r18: i64, expires: u64) -> Value {
    json!({"access_token":"test-access-secret","refresh_token":"test-refresh-secret","expires_in":expires,"token_type":"bearer","user":{"id":"123","name":"Owner","account":"owner","is_premium":premium,"x_restrict":r18}})
}
async fn endpoint(server: &MockServer, reply: Value) {
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply))
        .mount(server)
        .await;
}
fn config(server: &MockServer, broker: &FakeBroker) -> Config {
    Config {
        broker: broker.cfg.clone(),
        oauth: oauth::Config {
            token_url: format!("{}/auth/token", server.uri()),
        },
    }
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("state transition timed out");
}
async fn connect(
    server: &MockServer,
    broker: &FakeBroker,
    premium: bool,
    r18: i64,
    expires: u64,
) -> Arc<Connection> {
    endpoint(server, token(premium, r18, expires)).await;
    let c = Connection::new(config(server, broker));
    let start = c.start().unwrap();
    let result = c
        .finish(start["state"].as_str().unwrap(), "test-code")
        .await
        .unwrap();
    assert_eq!(result["state"], "connected");
    assert_eq!(result["store"], "waiting_approval");
    c
}
async fn service(c: Arc<Connection>, base: String) -> (Service, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ServiceConfig::default();
    cfg.gate.interval = Duration::ZERO;
    cfg.gate.attempts = 1;
    let s = Service::new(
        Arc::new(SwitchBackend::new(c, base)),
        DiskCache::new(dir.path().join("cache"), 1000000).unwrap(),
        dir.path().join("spool"),
        cfg,
    )
    .unwrap();
    (s, dir)
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/app_illust.json")).unwrap()
}
fn work(id: u64, r18: bool) -> Value {
    let mut v = fixture();
    v["id"] = json!(id);
    v["x_restrict"] = json!(i64::from(r18));
    v
}

#[test]
fn pkce_and_redaction() {
    let v = oauth::verifier().unwrap();
    assert_eq!(v.expose().len(), 43);
    assert!(!v.expose().contains('='));
    let known = Secret::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(
        oauth::challenge(&known),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(oauth::login_url(&known),"https://app-api.pixiv.net/web/v1/login?code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&client=pixiv-android");
    assert_eq!(format!("{v:?}"), "[redacted]");
    assert_eq!(format!("{v}"), "[redacted]");
}
#[tokio::test]
async fn oauth_exchange_refresh_forms_headers_and_debug() {
    use md5::{Digest, Md5};
    let server = MockServer::start().await;
    endpoint(&server, token(true, 2, 3600)).await;
    let cfg = oauth::Config {
        token_url: format!("{}/auth/token", server.uri()),
    };
    let t = cfg
        .exchange("code-123", &Secret::new("verifier-123"))
        .await
        .unwrap();
    assert_eq!(t.user.id, "123");
    assert!(t.user.premium);
    assert_eq!(t.user.x_restrict, 2);
    let debug = format!("{t:?}");
    assert!(!debug.contains("test-access-secret"));
    assert!(!debug.contains("test-refresh-secret"));
    tokio::task::spawn_blocking(move || cfg.refresh(&t.refresh))
        .await
        .unwrap()
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for r in &requests {
        let form: HashMap<_, _> = url::form_urlencoded::parse(&r.body).into_owned().collect();
        assert_eq!(form["client_id"], oauth::CLIENT_ID);
        assert_eq!(form["client_secret"], oauth::CLIENT_SECRET);
        assert_eq!(form["include_policy"], "true");
        assert_eq!(r.headers["app-os"], "android");
        assert_eq!(r.headers["app-os-version"], "11");
        assert_eq!(
            r.headers["user-agent"],
            "PixivAndroidApp/5.0.234 (Android 11; Pixel 5)"
        );
        let time = r.headers["x-client-time"].to_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(time).is_ok());
        assert_eq!(
            r.headers["x-client-hash"],
            hex::encode(Md5::digest(format!("{time}{}", oauth::HASH_SECRET)))
        );
        if form["grant_type"] == "authorization_code" {
            assert_eq!(form["code"], "code-123");
            assert_eq!(form["code_verifier"], "verifier-123");
            assert_eq!(form["redirect_uri"], oauth::REDIRECT_URI);
        } else {
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["refresh_token"], "test-refresh-secret");
            assert_eq!(form["get_secure_url"], "1");
        }
    }
}
#[tokio::test]
async fn oauth_rejected_unavailable_and_numeric_id() {
    for (status, body, want) in [
        (
            400,
            json!({"error":"invalid_grant","message":"test-refresh-secret"}),
            TokenError::Rejected,
        ),
        (
            400,
            json!({"has_error":true,"errors":{"system":{"code":1508,"message":"Invalid refresh token"}}}),
            TokenError::Rejected,
        ),
        (503, json!({}), TokenError::Unavailable),
        (401, json!({}), TokenError::Other),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let cfg = oauth::Config {
            token_url: server.uri(),
        };
        assert_eq!(
            cfg.exchange("c", &Secret::new("v")).await.unwrap_err(),
            want
        );
    }
    let server = MockServer::start().await;
    let mut reply = token(false, 0, 3600);
    reply["user"]["id"] = json!(456);
    endpoint(&server, reply).await;
    assert_eq!(
        oauth::Config {
            token_url: format!("{}/auth/token", server.uri())
        }
        .exchange("c", &Secret::new("v"))
        .await
        .unwrap()
        .user
        .id,
        "456"
    );
}
#[tokio::test]
async fn broker_reads_and_mutation_repeats_exact_request() {
    let b = FakeBroker::new(true, "ok").await;
    assert_eq!(
        b.cfg.read().await.unwrap().unwrap().expose(),
        "stored-refresh"
    );
    assert!(b.cfg.exists().await.unwrap());
    let cfg = b.cfg.clone();
    let task = tokio::spawn(async move {
        cfg.mutate("update", Some(&Secret::new("new-refresh")))
            .await
    });
    wait_for(|| {
        b.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["op"] == "mutate")
            .count()
            >= 3
    })
    .await;
    b.release();
    task.await.unwrap().unwrap();
    let calls = b.calls.lock().unwrap().clone();
    let mutations: Vec<_> = calls.iter().filter(|v| v["op"] == "mutate").collect();
    assert!(mutations[0]["confirm_id"].is_null());
    for r in &mutations[1..] {
        assert_eq!(r["confirm_id"], "approval-1");
        for field in ["ref", "action", "value", "token"] {
            assert_eq!(r[field], mutations[0][field]);
        }
    }
    let missing = FakeBroker::new(false, "ok").await;
    assert!(missing.cfg.read().await.unwrap().is_none());
    assert!(!missing.cfg.exists().await.unwrap());
}
#[tokio::test]
async fn broker_approval_errors() {
    for (outcome, want) in [
        ("denied", broker::Error::Denied),
        ("expired", broker::Error::Expired),
        ("code", broker::Error::ApprovalUnavailable),
        ("failed", broker::Error::Failed("upstream".into())),
    ] {
        let b = FakeBroker::new(false, outcome).await;
        assert_eq!(
            b.cfg.mutate("create", Some(&Secret::new("secret"))).await,
            Err(want)
        );
    }
}
#[tokio::test]
async fn connection_save_busy_disconnect_and_generation_fence() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 3600).await;
    assert_eq!(c.start().unwrap_err(), ConnectError(409, "busy"));
    assert!(c.account().usable());
    assert_eq!(c.json()["r18"], true);
    b.release();
    wait_for(|| c.json()["store"] == "saved").await;
    b.approve.store(false, Ordering::SeqCst);
    let v = c.disconnect();
    assert_eq!(v["state"], "disconnected");
    assert_eq!(v["user"], Value::Null);
    assert_eq!(v["removal"], "waiting_approval");
    assert!(!c.account().usable());
    assert_eq!(c.start().unwrap_err().0, 409);
    b.release();
    wait_for(|| c.json()["removal"].is_null()).await;
    assert_eq!(c.json()["store"], Value::Null);
    let c = connect(&server, &b, false, 1, 3600).await; // disconnect while save is pending; its late success cannot reconnect
    c.disconnect();
    wait_for(|| c.json()["removal"].is_null()).await;
    assert_eq!(c.json()["state"], "disconnected");
    assert_eq!(c.json()["store"], Value::Null);
}
#[tokio::test]
async fn connection_failed_approvals_drop_tokens() {
    for (outcome, error) in [
        ("denied", "approval_denied"),
        ("expired", "approval_expired"),
        ("code", "approval_unavailable"),
        ("failed", "save_failed"),
    ] {
        let server = MockServer::start().await;
        let b = FakeBroker::new(false, outcome).await;
        let c = connect(&server, &b, true, 2, 3600).await;
        wait_for(|| c.json()["state"] == "disconnected").await;
        assert_eq!(c.json()["error"], error);
        assert_eq!(c.json()["user"], Value::Null);
        assert!(!c.account().usable());
    }
}
#[tokio::test]
async fn connection_unknown_state_and_login_refusal() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = Connection::new(config(&server, &b));
    assert_eq!(
        c.finish("unknown", "code").await.unwrap_err(),
        ConnectError(400, "connect_expired")
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"})))
        .mount(&server)
        .await;
    let start = c.start().unwrap();
    assert_eq!(
        c.finish(start["state"].as_str().unwrap(), "code")
            .await
            .unwrap_err(),
        ConnectError(502, "pixiv_login_failed")
    );
    assert_eq!(
        Connection::new(Default::default()).start().unwrap_err(),
        ConnectError(503, "broker_unconfigured")
    );
    // Only five verifiers survive.
    let first = c.start().unwrap();
    for _ in 0..5 {
        c.start().unwrap();
    }
    assert_eq!(
        c.finish(first["state"].as_str().unwrap(), "code")
            .await
            .unwrap_err()
            .1,
        "connect_expired"
    );
}
#[tokio::test]
async fn startup_found_missing_and_rejected() {
    for stored in [true, false] {
        let server = MockServer::start().await;
        endpoint(&server, token(true, 2, 3600)).await;
        let b = FakeBroker::new(stored, "ok").await;
        let c = Connection::new(config(&server, &b));
        c.startup();
        wait_for(|| {
            if stored {
                c.json()["user"]["id"] == "123"
            } else {
                c.json()["state"] == "disconnected"
            }
        })
        .await;
        assert_eq!(
            c.json()["state"],
            if stored { "connected" } else { "disconnected" }
        );
        assert_eq!(
            c.json()["store"],
            if stored { json!("saved") } else { Value::Null }
        );
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"})))
        .mount(&server)
        .await;
    let b = FakeBroker::new(true, "ok").await;
    let c = Connection::new(config(&server, &b));
    c.startup();
    wait_for(|| c.json()["state"] == "expired").await;
    assert_eq!(c.json()["error"], "token_rejected");
}
#[tokio::test]
async fn app_ranking_search_preview_and_detail() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 3600).await;
    Mock::given(path("/v1/illust/ranking")).and(query_param("mode","week_original")).and(query_param("offset","30")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"illusts":[fixture(),work(2,false)],"next_url":"https://app-api.pixiv.net/v1/illust/ranking?offset=60"}))).mount(&server).await;
    Mock::given(path("/v1/search/illust"))
        .and(query_param("word", "sky R-18"))
        .and(query_param("sort", "date_asc"))
        .and(query_param("offset", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"illusts":[fixture(),{"id":3,"type":"ugoira"}],"next_url":"next"}),
        ))
        .mount(&server)
        .await;
    Mock::given(path("/v1/search/popular-preview/illust"))
        .and(query_param("word", "sky"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"illusts":[fixture()],"next_url":null})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/v1/illust/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"illust":fixture()})))
        .mount(&server)
        .await;
    let base = server.uri();
    tokio::task::spawn_blocking(move || {
        let app = AppBackend::new(c, base);
        let raw = app.ranking("original", 2).unwrap();
        let l = parse_ranking(&raw, 2, false);
        assert!(l.has_more);
        assert!(l.items[0].r18);
        assert!(!l.items[1].r18);
        assert_eq!(raw["items"][0]["bookmarks"], 890);
        let args = [
            "sky", "--order", "date", "--mode", "r18", "--type", "manga", "--page", "2",
        ]
        .map(str::to_string);
        let l = parse_search(&app.search(&args).unwrap(), 2);
        assert_eq!(l.items.len(), 1);
        assert!(l.items[0].r18);
        assert!(l.has_more);
        assert!(l.total.is_none());
        assert!(l.num_pages.is_none());
        let args = ["sky", "--order", "popular_preview"].map(str::to_string);
        let l = parse_search(&app.search(&args).unwrap(), 1);
        assert_eq!(l.items[0].id, "12345");
        let i = parse_illust(&app.illust("12345").unwrap()).unwrap();
        assert_eq!(i.kind, 1);
        assert!(i.r18);
        assert_eq!(i.pages.len(), 2);
        assert_eq!(i.page_count, 2);
        assert_eq!(
            i.pages[1].orig,
            "https://i.pximg.net/img-original/12345_p1.png"
        );
        assert_eq!(i.pages[1].regular, "https://i.pximg.net/p1_large.jpg");
        assert_eq!(i.stats.bookmarks, 890);
        assert_eq!(i.stats.views, 4567);
        assert_eq!(i.stats.likes, 0);
        assert_eq!(i.tags[0].en, "sky");
        assert_eq!(i.description, "Hello & welcome\nSky ★\n");
        let args = ["sky", "--page", "168"].map(str::to_string);
        let empty = app.search(&args).unwrap();
        assert!(empty["items"].as_array().unwrap().is_empty());
    })
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    let app_requests: Vec<_> = requests.iter().filter(|r| r.method == "GET").collect();
    assert_eq!(app_requests.len(), 4);
    for r in app_requests {
        assert_eq!(r.headers["authorization"], "Bearer test-access-secret");
        assert_eq!(r.headers["app-os"], "ios");
        assert_eq!(r.headers["app-os-version"], "14.6");
        assert_eq!(
            r.headers["user-agent"],
            "PixivIOSApp/7.13.3 (iOS 14.6; iPhone13,2)"
        );
        assert_eq!(r.headers["accept-language"], "en-us");
        assert!(r
            .url
            .query_pairs()
            .any(|(k, v)| k == "filter" && v == "for_ios"));
    }
}
#[tokio::test]
async fn app_single_page_ugoira_and_image_fallbacks() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 3600).await;
    let mut v = fixture();
    v["type"] = json!("ugoira");
    v["meta_pages"] = json!([]);
    v["page_count"] = json!(1);
    v["meta_single_page"] = json!({"original_image_url":"https://i.pximg.net/original.png"});
    v["image_urls"] = json!({"medium":"https://i.pximg.net/medium.jpg"});
    Mock::given(path("/v1/illust/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"illust":v})))
        .mount(&server)
        .await;
    let base = server.uri();
    let i = tokio::task::spawn_blocking(move || {
        parse_illust(&AppBackend::new(c, base).illust("12345").unwrap()).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(i.kind, 2);
    assert_eq!(i.pages.len(), 1);
    assert_eq!(i.pages[0].orig, "https://i.pximg.net/original.png");
    assert_eq!(i.pages[0].regular, "https://i.pximg.net/medium.jpg");
}
#[tokio::test]
async fn app_refreshes_expired_access_and_retries_invalid_access_once() {
    for expired in [true, false] {
        let server = MockServer::start().await;
        let b = FakeBroker::new(false, "ok").await;
        let c = connect(&server, &b, false, 1, if expired { 0 } else { 3600 }).await;
        server.reset().await;
        let mut refreshed = token(false, 1, 3600);
        refreshed["access_token"] = json!("fresh-access");
        endpoint(&server, refreshed).await;
        Mock::given(path("/v1/illust/ranking"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer fresh-access",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"illusts":[fixture()],"next_url":null})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/v1/illust/ranking"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer test-access-secret",
            ))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(
                    json!({"error":{"message":"Error occurred at the OAuth process."}}),
                ),
            )
            .mount(&server)
            .await;
        let base = server.uri();
        tokio::task::spawn_blocking(move || AppBackend::new(c, base).ranking("day", 1))
            .await
            .unwrap()
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
        assert_eq!(requests.len(), if expired { 2 } else { 3 });
    }
}
#[tokio::test]
async fn refresh_rejection_expires_connection_and_maps_to_other() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 0).await;
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"})))
        .mount(&server)
        .await;
    let cc = c.clone();
    let error = tokio::task::spawn_blocking(move || cc.access(false))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        secretary_pixiv::backend::classify(&error),
        secretary_pixiv::backend::Fail::Other
    );
    assert_eq!(c.json()["state"], "expired");
    assert_eq!(c.json()["error"], "token_rejected");
    assert!(c.has_session());
    let (s, _dir) = service(c, server.uri()).await;
    assert_eq!(
        s.ranking("day_r18", 1, true)
            .await
            .unwrap()
            .notice
            .as_deref(),
        Some("r18_needs_session")
    );
}
#[tokio::test]
async fn app_error_strings_classify_without_echoing_bodies() {
    use secretary_pixiv::backend::{classify, Fail};
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 3600).await;
    for (status, expected) in [
        (429, Fail::RateLimited),
        (404, Fail::NotFound),
        (503, Fail::Transient),
        (400, Fail::NotFound),
        (403, Fail::Other),
    ] {
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(if status==400 {ResponseTemplate::new(status).set_body_json(json!({"error":{"message":"illust not found","user_message":"test-access-secret"}}))} else {ResponseTemplate::new(status).set_body_string("test-access-secret")})
            .mount(&server)
            .await;
        let c = c.clone();
        let base = server.uri();
        let error = tokio::task::spawn_blocking(move || AppBackend::new(c, base).illust("12345"))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(classify(&error), expected);
        assert!(!error.contains("test-access-secret"));
    }
}
fn search_params(page: u32) -> SearchParams {
    SearchParams {
        q: "sky".into(),
        order: "popular".into(),
        mode: "all".into(),
        kind: "all".into(),
        page,
    }
}
#[tokio::test]
async fn connected_premium_popular_uses_app_popular_order() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, true, 1, 3600).await;
    Mock::given(path("/v1/search/illust"))
        .and(query_param("sort", "popular_desc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"illusts":[fixture()],"next_url":null})),
        )
        .mount(&server)
        .await;
    let (s, _dir) = service(c, server.uri()).await;
    let l = s.search(&search_params(1), true).await.unwrap();
    assert_eq!(l.sort.unwrap().used, "pixiv_popular");
    assert_eq!(l.items[0].id, "12345");
}
#[tokio::test]
async fn connected_nonpremium_preview_then_plain_without_preview_ids() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    let c = connect(&server, &b, false, 1, 3600).await;
    Mock::given(path("/v1/search/popular-preview/illust"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"illusts":[work(9,false),work(3,true)],"next_url":null})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/v1/search/illust")).and(query_param("sort","date_desc")).and(query_param("offset","0")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"illusts":[work(10,false),work(9,false),work(3,true),work(2,false)],"next_url":"next"}))).mount(&server).await;
    let (s, _dir) = service(c, server.uri()).await;
    let l = s.search(&search_params(1), true).await.unwrap();
    assert_eq!(l.sort.unwrap().used, "pixiv_popular_preview");
    assert_eq!(
        l.items.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["9", "3"]
    );
    assert!(l.has_more);
    assert!(l.total.is_none());
    let l = s.search(&search_params(2), true).await.unwrap();
    assert_eq!(l.sort.unwrap().used, "pixiv_popular_preview");
    assert_eq!(
        l.items.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["10", "2"]
    );
    assert!(l.has_more);
    assert_eq!(l.page, 2);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method == "GET")
            .count(),
        2,
        "both search pages cached independently"
    );
}
#[tokio::test]
async fn connected_r18_rankings_and_immediate_account_changes() {
    for r18 in [0, 1] {
        let server = MockServer::start().await;
        let b = FakeBroker::new(false, "ok").await;
        let c = connect(&server, &b, false, r18, 3600).await;
        Mock::given(path("/v1/illust/ranking"))
            .and(query_param("mode", "day_r18"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"illusts":[fixture()],"next_url":null})),
            )
            .mount(&server)
            .await;
        let (s, _dir) = service(c.clone(), server.uri()).await;
        let l = s.ranking("day_r18", 1, true).await.unwrap();
        if r18 == 1 {
            assert_eq!(l.items.len(), 1);
            assert!(l.notice.is_none());
        } else {
            assert!(l.items.is_empty());
            assert_eq!(l.notice.as_deref(), Some("r18_disabled_in_account"));
        }
        c.disconnect();
        assert!(!s.account().await.usable());
        assert_eq!(
            s.ranking("day_r18", 1, true)
                .await
                .unwrap()
                .notice
                .as_deref(),
            Some("r18_needs_session")
        );
    }
}
async fn request(
    app: &axum::Router,
    method: &str,
    route: &str,
    auth: bool,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/v1/pixiv{route}"));
    if auth {
        req = req.header("x-lucina-gateway", "gateway");
    }
    let body = if let Some(v) = body {
        req = req.header("content-type", "application/json");
        Body::from(v.to_string())
    } else {
        Body::empty()
    };
    let r = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = r.status();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&b).unwrap())
}
#[tokio::test]
async fn http_connect_settings_validation_and_gateway() {
    let server = MockServer::start().await;
    let b = FakeBroker::new(false, "ok").await;
    endpoint(&server, token(false, 1, 3600)).await;
    let c = Connection::new(config(&server, &b));
    let (service, dir) = service(c.clone(), server.uri()).await;
    let store = Store::open(dir.path(), "owner").unwrap();
    let app = router(Arc::new(Http {
        service,
        store,
        gateway_secret: b"gateway".to_vec(),
        signing_key: vec![0; 32],
    }));
    for (method, route) in [
        ("POST", "/connect/start"),
        ("POST", "/connect/finish"),
        ("POST", "/disconnect"),
        ("GET", "/settings"),
        ("PUT", "/settings"),
    ] {
        assert_eq!(
            request(&app, method, route, false, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(&app, "GET", "/settings", true, None).await.1["data"],
        json!({"defaultContent":"safe","defaultSort":"date_d"})
    );
    assert_eq!(
        request(
            &app,
            "PUT",
            "/settings",
            true,
            Some(json!({"defaultContent":"r18"}))
        )
        .await
        .1["data"],
        json!({"defaultContent":"r18","defaultSort":"date_d"})
    );
    request(
        &app,
        "PUT",
        "/settings",
        true,
        Some(json!({"defaultSort":"popular"})),
    )
    .await;
    for body in [
        json!({"defaultContent":"weird"}),
        json!({"defaultSort":"date"}),
        json!({"defaultContent":null}),
        json!({"unknown":true}),
        json!([]),
    ] {
        let r = request(&app, "PUT", "/settings", true, Some(body)).await;
        assert_eq!(r.0, StatusCode::BAD_REQUEST);
        assert_eq!(r.1["error"], "bad_request");
    }
    assert_eq!(
        Store::open(dir.path(), "owner")
            .unwrap()
            .settings()
            .default_sort,
        "popular"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("settings.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for body in [
        json!({"state":"","code":"c"}),
        json!({"state":"abc","code":"bad&code"}),
        json!({"state":"abc","code":"a".repeat(513)}),
        json!({}),
    ] {
        let r = request(&app, "POST", "/connect/finish", true, Some(body)).await;
        assert_eq!(r.0, StatusCode::BAD_REQUEST);
        assert_eq!(r.1["error"], "bad_request");
    }
    let start = request(&app, "POST", "/connect/start", true, None).await.1;
    let state = start["data"]["state"].as_str().unwrap();
    let r = request(
        &app,
        "POST",
        "/connect/finish",
        true,
        Some(json!({"state":state,"code":"auth-code"})),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK);
    assert_eq!(r.1["data"]["store"], "waiting_approval");
    assert!(!r.1.to_string().contains("test-access-secret"));
    assert!(!r.1.to_string().contains("test-refresh-secret"));
    assert_eq!(
        request(&app, "POST", "/connect/start", true, None).await.0,
        StatusCode::CONFLICT
    );
    b.release();
    wait_for(|| c.json()["store"] == "saved").await;
    let status = request(&app, "GET", "/status", true, None).await.1;
    assert_eq!(status["data"]["connection"]["state"], "connected");
    assert_eq!(status["data"]["account"]["session"], "ok");
    assert_eq!(status["data"]["settings"]["defaultSort"], "popular");
    assert_eq!(
        request(&app, "POST", "/disconnect", true, None).await.1["data"]["state"],
        "disconnected"
    );
}

#[tokio::test]
async fn startup_transient_refresh_retries_on_first_use() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let b = FakeBroker::new(true, "ok").await;
    let c = Connection::new(config(&server, &b));
    c.startup();
    wait_for(|| c.json()["state"] == "connected").await;
    // Wait for the failed startup refresh to complete, then let the next access recover.
    let cc = c.clone();
    tokio::task::spawn_blocking(move || cc.access(false))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(c.json()["state"], "connected");
    assert_eq!(c.json()["store"], "saved");
    server.reset().await;
    endpoint(&server, token(true, 2, 3600)).await;
    let cc = c.clone();
    tokio::task::spawn_blocking(move || cc.access(false))
        .await
        .unwrap()
        .unwrap();
    assert!(c.account().premium);
    assert_eq!(c.json()["user"]["id"], "123");
}
#[tokio::test]
async fn disconnect_denial_leaves_saved_secret_but_stays_disconnected() {
    let server = MockServer::start().await;
    endpoint(&server, token(true, 1, 3600)).await;
    let b = FakeBroker::new(true, "denied").await;
    let c = Connection::new(config(&server, &b));
    c.startup();
    wait_for(|| c.json()["user"]["id"] == "123").await;
    c.disconnect();
    wait_for(|| c.json()["removal"].is_null()).await;
    assert_eq!(c.json()["state"], "disconnected");
    assert_eq!(c.json()["error"], "remove_denied");
    assert_eq!(c.json()["store"], "saved");
    assert_eq!(c.json()["premium"], Value::Null);
}

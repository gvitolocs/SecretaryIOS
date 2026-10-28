//! Notifications end to end: the inbox and settings routes, `POST /v1/notify`
//! (service token), the timeline ingester against a mock Lucina, and Web
//! Push against a local mock push service that checks the VAPID signature
//! and decrypts the RFC 8291 payload like a browser would.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::{self, ServiceToken};
use secretary_server::{app, notify, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::broadcast;

// -- mock push service ---------------------------------------------------------

/// A browser's side of one subscription: its keys and what arrived.
#[derive(Clone)]
struct Ua {
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Ua {
    fn new(seed: u8) -> Self {
        Self {
            secret: p256::SecretKey::from_slice(&[seed; 32]).unwrap(),
            auth: [seed; 16],
        }
    }

    fn keys(&self) -> Value {
        json!({
            "p256dh": URL_SAFE_NO_PAD.encode(self.secret.public_key().to_encoded_point(false).as_bytes()),
            "auth": URL_SAFE_NO_PAD.encode(self.auth),
        })
    }
}

#[derive(Debug, Clone)]
struct Delivered {
    id: String,
    headers: HashMap<String, String>,
    payload: Value,
}

#[derive(Default)]
struct PushService {
    /// Subscription path id → browser keys.
    uas: Mutex<HashMap<String, Ua>>,
    /// Path id → statuses to answer, in order (then 201).
    script: Mutex<HashMap<String, Vec<u16>>>,
    delivered: Mutex<Vec<Delivered>>,
    attempts: Mutex<HashMap<String, usize>>,
    vapid_errors: Mutex<Vec<String>>,
}

fn check_vapid(headers: &HeaderMap, audience: &str) -> Result<(), String> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or("no Authorization")?;
    let rest = auth.strip_prefix("vapid ").ok_or("not vapid")?;
    let mut t = None;
    let mut k = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("t=") {
            t = Some(v.to_owned());
        }
        if let Some(v) = part.strip_prefix("k=") {
            k = Some(v.to_owned());
        }
    }
    let (t, k) = (t.ok_or("no t=")?, k.ok_or("no k=")?);
    let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&k).unwrap())
        .map_err(|_| "bad k")?;
    let parts: Vec<&str> = t.split('.').collect();
    let sig = p256::ecdsa::Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap())
        .map_err(|_| "bad sig")?;
    vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .map_err(|_| "signature does not verify")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    if claims["aud"] != audience {
        return Err(format!("aud {}", claims["aud"]));
    }
    if claims["sub"] != "mailto:owner@example.test" {
        return Err(format!("sub {}", claims["sub"]));
    }
    // RFC 8292: `exp` in the future, at most 24 hours out.
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let exp = claims["exp"].as_i64().unwrap_or(0);
    if exp <= now || exp > now + 24 * 3600 {
        return Err("exp".into());
    }
    Ok(())
}

async fn push_endpoint(
    State(ps): State<Arc<PushService>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    *ps.attempts.lock().unwrap().entry(id.clone()).or_default() += 1;
    let aud = format!("http://{}", headers["host"].to_str().unwrap());
    if let Err(e) = check_vapid(&headers, &aud) {
        ps.vapid_errors.lock().unwrap().push(e);
        return StatusCode::FORBIDDEN.into_response();
    }
    let next = {
        let mut script = ps.script.lock().unwrap();
        script
            .get_mut(&id)
            .and_then(|v| (!v.is_empty()).then(|| v.remove(0)))
    };
    if let Some(status) = next {
        let mut r = StatusCode::from_u16(status).unwrap().into_response();
        if status == 429 {
            r.headers_mut().insert("retry-after", "0".parse().unwrap());
        }
        return r;
    }
    let Some(ua) = ps.uas.lock().unwrap().get(&id).cloned() else {
        return StatusCode::GONE.into_response();
    };
    let auth = web_push_native::Auth::clone_from_slice(&ua.auth);
    let plain = match web_push_native::decrypt(body.to_vec(), &ua.secret, &auth) {
        Ok(p) => p,
        Err(e) => {
            ps.vapid_errors
                .lock()
                .unwrap()
                .push(format!("decrypt: {e}"));
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let h: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    ps.delivered.lock().unwrap().push(Delivered {
        id,
        headers: h,
        payload: serde_json::from_slice(&plain).unwrap(),
    });
    StatusCode::CREATED.into_response()
}

// -- mock Lucina ---------------------------------------------------------------

struct Lucina {
    items: broadcast::Sender<Value>,
    streams: Mutex<Vec<Option<String>>>,
}

async fn lucina_stream(State(l): State<Arc<Lucina>>, headers: HeaderMap) -> Response {
    let last = headers
        .get("last-event-id")
        .map(|v| v.to_str().unwrap().to_owned());
    l.streams.lock().unwrap().push(last);
    let rx = l.items.subscribe();
    let head = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(Bytes::from(": connected\n\n"))
    });
    let items = futures_util::stream::unfold(rx, |mut rx| async move {
        let item = rx.recv().await.ok()?;
        let chunk = format!(
            "event: item\nid: {}\ndata: {}\n\n",
            item["id"].as_str().unwrap(),
            item
        );
        Some((Ok(Bytes::from(chunk)), rx))
    });
    let mut r =
        axum::body::Body::from_stream(futures_util::StreamExt::chain(head, items)).into_response();
    r.headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    r
}

// -- harness -----------------------------------------------------------------------

struct H {
    base: String,
    push_base: String,
    http: reqwest::Client,
    token: String,
    svc: String,
    ps: Arc<PushService>,
    lucina: Arc<Lucina>,
    state: AppState,
    _dir: tempfile::TempDir,
}

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

async fn harness(ingest: bool) -> H {
    let ps = Arc::new(PushService::default());
    let push_base = spawn(
        Router::new()
            .route("/push/{id}", post(push_endpoint))
            .with_state(ps.clone()),
    )
    .await;
    let lucina = Arc::new(Lucina {
        items: broadcast::channel(64).0,
        streams: Mutex::default(),
    });
    let lucina_base = spawn(
        Router::new()
            .route("/api/secretary/timeline/stream", get(lucina_stream))
            .with_state(lucina.clone()),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_base, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let svc = service::generate().unwrap();
    cfg.service_token = Some(ServiceToken::from_secret(&svc).unwrap());
    cfg.notify.ingest = ingest;
    cfg.notify.tick = Duration::from_millis(100);
    cfg.notify.push.allow_http = true;
    cfg.notify.push.hosts = vec!["127.0.0.1".into()];
    cfg.notify.push.subject = "mailto:owner@example.test".into();
    cfg.notify.push.retry_delays = vec![Duration::from_millis(30), Duration::from_millis(30)];
    cfg.rate_limit_per_minute = 1000;
    let (_, token) = DeviceStore::new(&cfg.data_dir)
        .issue("Giuseppe's iPhone")
        .unwrap();
    let state = AppState::new(&cfg).unwrap();
    notify::spawn_workers(&state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let st = state.clone();
    tokio::spawn(async move {
        serve_on(listener, app(st), std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        push_base,
        http: reqwest::Client::new(),
        token,
        svc,
        ps,
        lucina,
        state,
        _dir: dir,
    }
}

impl H {
    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn notify(&self, body: Value) -> (u16, Value) {
        self.send(
            self.http
                .post(format!("{}/v1/notify", self.base))
                .bearer_auth(&self.svc)
                .json(&body),
        )
        .await
    }

    /// Register a browser at the mock push service and subscribe it.
    async fn subscribe(&self, path_id: &str, seed: u8, lang: &str) -> Value {
        let ua = Ua::new(seed);
        self.ps
            .uas
            .lock()
            .unwrap()
            .insert(path_id.into(), ua.clone());
        let (s, b) = self
            .send(
                self.req(reqwest::Method::POST, "/v1/push/subscribe")
                    .json(&json!({
                        "endpoint": format!("{}/push/{path_id}", self.push_base),
                        "expirationTime": null,
                        "keys": ua.keys(),
                        "label": "iPhone · Safari",
                        "lang": lang,
                    })),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        b
    }

    async fn wait_delivered(&self, n: usize) -> Vec<Delivered> {
        for _ in 0..100 {
            let d = self.ps.delivered.lock().unwrap().clone();
            if d.len() >= n {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "expected {n} pushes, got {:?}; vapid errors {:?}",
            self.ps.delivered.lock().unwrap(),
            self.ps.vapid_errors.lock().unwrap()
        );
    }

    async fn inbox(&self) -> Value {
        let (s, b) = self
            .send(self.req(reqwest::Method::GET, "/v1/notifications"))
            .await;
        assert_eq!(s, 200, "{b}");
        b
    }
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

// -- tests -----------------------------------------------------------------------------

#[tokio::test]
async fn web_push_end_to_end_encrypted_and_vapid_signed() {
    let h = harness(false).await;
    let sub = h.subscribe("a", 3, "it").await;
    assert_eq!(sub["service"], "127.0.0.1");
    assert!(
        sub.get("auth").is_none() && sub.get("endpoint").is_none(),
        "secrets stay on the server"
    );

    let (s, b) = h
        .notify(json!({"category": "reminder", "body": "Chiama il dentista", "dedupKey": "reminder:42"}))
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["pushed"], 1);
    let d = h.wait_delivered(1).await;
    assert_eq!(d[0].id, "a");
    let p = &d[0].payload;
    assert_eq!(p["category"], "reminder");
    assert_eq!(p["title"], "Promemoria", "subscription language");
    assert_eq!(p["body"], "Chiama il dentista");
    assert_eq!(p["route"], "/");
    assert_eq!(p["unread"], 1);
    assert_eq!(p["id"], b["notification"]["id"]);
    let hd = &d[0].headers;
    assert_eq!(hd["content-encoding"], "aes128gcm");
    assert_eq!(hd["urgency"], "high");
    assert_eq!(hd["ttl"], "21600");
    assert!(h.ps.vapid_errors.lock().unwrap().is_empty());

    // The same reminder again (e.g. from the timeline): no second push.
    let (s, b) = h
        .notify(json!({"category": "reminder", "body": "Chiama il dentista", "dedupKey": "reminder:42"}))
        .await;
    assert_eq!((s, b["status"].as_str()), (200, Some("duplicate")));

    // Lucina messages collapse under one topic and count in the title.
    for text in ["uno", "due"] {
        let (s, _) = h
            .notify(json!({"category": "lucina_message", "body": text}))
            .await;
        assert_eq!(s, 201);
    }
    let d = h.wait_delivered(3).await;
    let last = d.iter().find(|x| x.payload["body"] == "due").unwrap();
    assert_eq!(last.headers["topic"], "lucina");
    assert_eq!(last.headers["urgency"], "normal");
    assert_eq!(last.payload["title"], "2 messaggi da Lucina");

    // Previews off: no text leaves the server, even encrypted.
    let (s, mut settings) = h
        .send(h.req(reqwest::Method::GET, "/v1/notifications/settings"))
        .await;
    assert_eq!(s, 200);
    settings["settings"]["previews"] = json!(false);
    let (s, _) = h
        .send(
            h.req(reqwest::Method::PUT, "/v1/notifications/settings")
                .json(&settings["settings"]),
        )
        .await;
    assert_eq!(s, 200);
    h.notify(json!({"category": "shopping", "body": "Latte aggiunto", "route": "/shopping"}))
        .await;
    let d = h.wait_delivered(4).await;
    assert_eq!(d[3].payload["body"], "Apri Secretary per leggerlo.");
    assert_eq!(d[3].payload["route"], "/shopping");
}

#[tokio::test]
async fn delivery_retries_and_drops_expired_subscriptions() {
    let h = harness(false).await;
    h.subscribe("flaky", 4, "en").await;
    h.subscribe("gone", 5, "en").await;
    h.ps.script
        .lock()
        .unwrap()
        .insert("flaky".into(), vec![503, 429]);
    h.ps.script.lock().unwrap().insert("gone".into(), vec![410]);

    let (s, b) = h
        .notify(json!({"category": "device_confirm", "body": "Delete the 7:00 alarm?"}))
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["pushed"], 2);
    let d = h.wait_delivered(1).await;
    assert_eq!(d[0].id, "flaky");
    assert_eq!(d[0].payload["title"], "Lucina needs your OK");
    assert_eq!(
        h.ps.attempts.lock().unwrap()["flaky"],
        3,
        "503, 429, then 201"
    );
    // 410 removed the subscription; the retried one recorded its success.
    for _ in 0..100 {
        let subs = h.state.notify.pusher.list();
        if subs.len() == 1 && subs[0].last_success_at.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let subs = h.state.notify.pusher.list();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].failures, 0);
    assert!(subs[0].last_success_at.is_some());
    assert_eq!(
        h.ps.attempts.lock().unwrap()["gone"],
        1,
        "410 is not retried"
    );

    // A test push reports per subscription.
    let (s, b) = h.send(h.req(reqwest::Method::POST, "/v1/push/test")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["results"][0]["ok"], true);
    assert_eq!(b["results"][0]["delivery"]["result"], "sent");
}

#[tokio::test]
async fn subscribe_validates_endpoint_and_keys() {
    let h = harness(false).await;
    let ua = Ua::new(9);
    let post = |body: Value| {
        h.req(reqwest::Method::POST, "/v1/push/subscribe")
            .json(&body)
    };
    let (s, b) = h
        .send(post(
            json!({"endpoint": "https://evil.example/x", "keys": ua.keys()}),
        ))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    assert!(b["error"]["message"]
        .as_str()
        .unwrap()
        .contains("push service"));
    let (s, b) = h
        .send(post(json!({"endpoint": format!("{}/push/x", h.push_base), "keys": {"p256dh": "AAAA", "auth": "AAAA"}})))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    // Upsert by endpoint: same id twice.
    let a = h.subscribe("same", 9, "en").await;
    let b2 = h.subscribe("same", 9, "da").await;
    assert_eq!(a["id"], b2["id"]);
    assert_eq!(h.state.notify.pusher.list().len(), 1);
    // Unsubscribe by endpoint, then by id → 404.
    let (s, b) = h
        .send(
            h.req(reqwest::Method::DELETE, "/v1/push/subscribe")
                .json(&json!({"endpoint": format!("{}/push/same", h.push_base)})),
        )
        .await;
    assert_eq!((s, b["removed"].as_u64()), (200, Some(1)));
    let (s, b) = h
        .send(h.req(
            reqwest::Method::DELETE,
            &format!("/v1/push/subscriptions/{}", a["id"].as_str().unwrap()),
        ))
        .await;
    assert_eq!((s, code(&b)), (404, "subscription_not_found"));
    // APNs is off without a paid team.
    let (s, b) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/push/apns")
                .json(&json!({"token": "ab".repeat(32)})),
        )
        .await;
    assert_eq!((s, code(&b)), (503, "apns_disabled"));
    let (s, b) = h.send(h.req(reqwest::Method::GET, "/v1/push/key")).await;
    assert_eq!(s, 200);
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(b["publicKey"].as_str().unwrap())
            .unwrap()
            .len(),
        65
    );
}

#[tokio::test]
async fn inbox_settings_and_auth() {
    let h = harness(false).await;
    // Device routes need a device; /v1/notify needs the service token.
    let r = h
        .http
        .get(format!("{}/v1/notifications", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let (s, b) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/notify")
                .json(&json!({"category": "system", "body": "x"})),
        )
        .await;
    assert_eq!(
        (s, code(&b)),
        (401, "unauthorized"),
        "a device token is not a service token"
    );
    let r = h
        .http
        .get(format!("{}/v1/notifications", h.base))
        .bearer_auth(&h.svc)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "a service token is not a device token");

    // /v1/notify validation.
    for bad in [
        json!({"category": "weather", "body": "x"}),
        json!({"category": "system", "body": ""}),
        json!({"category": "system", "body": "x", "route": "//evil.example"}),
        json!({"category": "system", "body": "x", "extra": 1}),
        json!({"category": "system", "body": "x", "timelineId": "bad id"}),
    ] {
        let (s, _) = h.notify(bad.clone()).await;
        assert_eq!(s, 400, "{bad}");
    }
    // `system` is off by default.
    let (s, b) = h
        .notify(json!({"category": "system", "body": "hello"}))
        .await;
    assert_eq!((s, b["status"].as_str()), (200, Some("disabled")));

    for i in 0..3 {
        let (s, _) = h
            .notify(json!({"category": "lucina_message", "title": "Lucina", "body": format!("m{i}"), "channel": "telegram"}))
            .await;
        assert_eq!(s, 201);
    }
    let b = h.inbox().await;
    assert_eq!(b["unread"], 3);
    let items = b["items"].as_array().unwrap();
    assert_eq!(items[0]["body"], "m2", "newest first");
    assert_eq!(items[0]["channel"], "telegram");
    let (s, page) = h
        .send(h.req(
            reqwest::Method::GET,
            &format!(
                "/v1/notifications?limit=1&before={}",
                items[0]["id"].as_str().unwrap()
            ),
        ))
        .await;
    assert_eq!(s, 200);
    assert_eq!(page["items"][0]["body"], "m1");
    let (s, _) = h
        .send(h.req(reqwest::Method::GET, "/v1/notifications?limit=0"))
        .await;
    assert_eq!(s, 400);

    let (s, r) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/notifications/read")
                .json(&json!({"ids": [items[0]["id"]]})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(r, json!({"updated": 1, "unread": 2}));
    let (s, unread) = h
        .send(h.req(reqwest::Method::GET, "/v1/notifications?unread=1"))
        .await;
    assert_eq!(s, 200);
    assert_eq!(unread["items"].as_array().unwrap().len(), 2);
    let (s, r) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/notifications/read")
                .json(&json!({"all": true})),
        )
        .await;
    assert_eq!((s, r["unread"].as_u64()), (200, Some(0)));
    let (s, _) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/notifications/read")
                .json(&json!({})),
        )
        .await;
    assert_eq!(s, 400);

    // Settings round trip and validation.
    let (s, v) = h
        .send(h.req(reqwest::Method::GET, "/v1/notifications/settings"))
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["settings"]["categories"]["presence"], false);
    assert_eq!(v["webPush"]["enabled"], true);
    assert_eq!(v["apns"]["enabled"], false);
    assert_eq!(v["categories"].as_array().unwrap().len(), 9, "7 + agent + approval");
    assert!(v["currentDevice"]["id"]
        .as_str()
        .unwrap()
        .starts_with("dev_"));
    let mut next = v["settings"].clone();
    next["quietHours"] = json!({"enabled": true, "start": "23:00", "end": "07:00"});
    next["devices"] = json!({"ps_abc": {"enabled": true, "categories": {"calendar": false}}});
    next["language"] = json!("da");
    let (s, v) = h
        .send(
            h.req(reqwest::Method::PUT, "/v1/notifications/settings")
                .json(&next),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["settings"]["quietHours"]["start"], "23:00");
    assert_eq!(
        v["settings"]["devices"]["ps_abc"]["categories"]["calendar"],
        false
    );
    let mut bad = next.clone();
    bad["quietHours"]["end"] = json!("7");
    let (s, b) = h
        .send(
            h.req(reqwest::Method::PUT, "/v1/notifications/settings")
                .json(&bad),
        )
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));

    // Presence.
    let (s, b) = h
        .send(
            h.req(reqwest::Method::POST, "/v1/notifications/presence")
                .json(&json!({"active": true})),
        )
        .await;
    assert_eq!((s, b["present"].as_bool()), (200, Some(true)));
    let (s, b) = h
        .notify(json!({"category": "lucina_message", "body": "you are here"}))
        .await;
    assert_eq!((s, b["status"].as_str()), (200, Some("present")));
}

#[tokio::test]
async fn cookie_writes_need_csrf() {
    let h = harness(false).await;
    let r = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let get = h
        .http
        .get(format!("{}/v1/notifications", h.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 200);
    let post = |csrf: bool| {
        let mut rb = h
            .http
            .post(format!("{}/v1/notifications/read", h.base))
            .header("cookie", &cookie)
            .json(&json!({"all": true}));
        if csrf {
            rb = rb.header("x-secretary-csrf", "1");
        }
        rb
    };
    let (s, b) = h.send(post(false)).await;
    assert_eq!((s, code(&b)), (403, "csrf"));
    let (s, _) = h.send(post(true)).await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn timeline_ingestion_creates_and_pushes_lucina_items() {
    let h = harness(true).await;
    h.subscribe("phone", 7, "en").await;
    // Wait for the ingester to connect.
    for _ in 0..100 {
        if !h.lucina.streams.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        h.lucina.streams.lock().unwrap()[0],
        None,
        "fresh start: no replay"
    );
    let now = chrono_now_iso();
    let send = |v: Value| {
        h.lucina.items.send(v).unwrap();
    };
    send(
        json!({"id": "01JA0000000000000000000001", "ts": now, "channel": "telegram", "role": "user", "text": "ricordami"}),
    );
    send(
        json!({"id": "01JA0000000000000000000002", "ts": now, "channel": "telegram", "role": "lucina",
                "text": "Promemoria: chiama il dentista", "meta": {"kind": "reminder", "isDm": true}}),
    );
    send(
        json!({"id": "01JA0000000000000000000003", "ts": now, "channel": "discord", "role": "lucina", "text": "Ciao!"}),
    );
    send(
        json!({"id": "01JA0000000000000000000004", "ts": "2020-01-01T00:00:00Z", "channel": "web", "role": "lucina", "text": "old"}),
    );
    let d = h.wait_delivered(2).await;
    let cats: Vec<&str> = d
        .iter()
        .map(|x| x.payload["category"].as_str().unwrap())
        .collect();
    assert!(
        cats.contains(&"reminder") && cats.contains(&"lucina_message"),
        "{cats:?}"
    );
    let b = h.inbox().await;
    let items = b["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{b}");
    assert!(items
        .iter()
        .any(|i| i["timelineId"] == "01JA0000000000000000000002" && i["category"] == "reminder"));

    // Lucina's own /v1/notify for the same delivered reminder: deduplicated.
    let (s, b) = h
        .notify(json!({"category": "reminder", "body": "Promemoria", "timelineId": "01JA0000000000000000000002"}))
        .await;
    assert_eq!((s, b["status"].as_str()), (200, Some("duplicate")));

    // The resume point is saved.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let last = h.state.notify.state.read(|s| s.last_event_id.clone());
    assert_eq!(last.as_deref(), Some("01JA0000000000000000000004"));
}

fn chrono_now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

#[tokio::test]
async fn proactive_feedback_and_service_settings() {
    let h = harness(false).await;
    let id = "po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F";
    let path = format!("/v1/proactive/outreach/{id}/feedback");
    let (s, b) = h
        .send(
            h.req(reqwest::Method::POST, &path)
                .json(&json!({"action": "stop"})),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["feedback"]["action"], "stop");
    assert_eq!(b["feedback"]["outreachId"], id);
    let first = b["feedback"]["id"].as_str().unwrap().to_owned();
    // The same tap again: the stored one.
    let (s, b) = h
        .send(
            h.req(reqwest::Method::POST, &path)
                .json(&json!({"action": "stop"})),
        )
        .await;
    assert_eq!(
        (s, b["feedback"]["id"].as_str().unwrap()),
        (200, first.as_str())
    );
    for (p, body) in [
        (path.as_str(), json!({"action": "delete_everything"})),
        (
            "/v1/proactive/outreach/nope/feedback",
            json!({"action": "useful"}),
        ),
        (path.as_str(), json!({"action": "useful", "extra": 1})),
    ] {
        let (s, _) = h.send(h.req(reqwest::Method::POST, p).json(&body)).await;
        assert_eq!(s, 400, "{p} {body}");
    }
    // The service token is not a device token, and the other way round.
    let r = h
        .http
        .post(format!("{}{path}", h.base))
        .bearer_auth(&h.svc)
        .json(&json!({"action": "useful"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let (s, _) = h
        .send(h.req(reqwest::Method::GET, "/v1/proactive/feedback"))
        .await;
    assert_eq!(s, 401);

    // The engine pulls with the service token.
    let pull = |after: Option<&str>| {
        let url = match after {
            Some(a) => format!("{}/v1/proactive/feedback?after={a}", h.base),
            None => format!("{}/v1/proactive/feedback", h.base),
        };
        h.http.get(url).bearer_auth(&h.svc)
    };
    let (s, b) = h.send(pull(None)).await;
    assert_eq!(s, 200);
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    assert_eq!(b["cursor"], first.as_str());
    let (_, b) = h.send(pull(Some(&first))).await;
    assert!(b["items"].as_array().unwrap().is_empty());
    assert_eq!(b["cursor"], first.as_str(), "the cursor stays");

    let (s, b) = h
        .send(
            h.http
                .get(format!("{}/v1/notify/settings", h.base))
                .bearer_auth(&h.svc),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(b["settings"]["quietHours"]["start"], "22:30");
    assert_eq!(b["present"], false);
    assert!(
        b.get("subscriptions").is_none(),
        "no device list for the service"
    );
}

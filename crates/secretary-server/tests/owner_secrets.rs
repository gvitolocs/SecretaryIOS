//! "Add secret" (`/v1/secrets*`): only the owner's devices reach it, each
//! request carries a fresh signed grant that names exactly what the owner
//! confirmed, and the value never appears in the server's logs or in a grant.

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const VALUE: &str = "sk-proj-OwnerTypedValue0123456789abcdefXYZ";
const SVC: &str = "svc_0123456789abcdef0123456789abcdef0123";

/// Everything the server logs, for the whole test binary.
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static L: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    L.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = buf.clone();
        let make = move || LogWriter(w.clone());
        let _ = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_writer(make)
            .with_ansi(false)
            .try_init();
        buf
    })
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Mock {
    /// (path, body) of each request Lucina got.
    seen: Mutex<Vec<(String, Value)>>,
}

async fn lucina(State(m): State<Arc<Mock>>, _method: Method, uri: Uri, body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.seen
        .lock()
        .unwrap()
        .push((uri.path().to_owned(), v.clone()));
    if uri.path() == "/api/secretary/secrets/grant" {
        let act = payload(v["grant"].as_str().unwrap())["act"].clone();
        return Json(json!({"ok": true, "data": {"act": act, "fingerprint": "abcd1234"}}))
            .into_response();
    }
    Json(json!({"ok": false, "error": "not_found"})).into_response()
}

fn payload(grant: &str) -> Value {
    let p64 = grant
        .strip_prefix("g1.")
        .unwrap()
        .split('.')
        .next()
        .unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p64).unwrap()).unwrap()
}

struct H {
    base: String,
    token: String,
    device_id: String,
    mock: Arc<Mock>,
    http: reqwest::Client,
    dir: tempfile::TempDir,
}

async fn start() -> H {
    logs();
    let mock = Arc::new(Mock::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    let router = Router::new().fallback(lucina).with_state(mock.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    cfg.service_token = Some(ServiceToken::from_secret(SVC).unwrap());
    let (device, token) = DeviceStore::new(&cfg.data_dir)
        .issue("Giuseppe iPhone")
        .unwrap();
    let device_id = device.id.clone();
    let state = AppState::new(&cfg).unwrap();
    let s = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", s.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(s, app(state), std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        token,
        device_id,
        mock,
        http: reqwest::Client::new(),
        dir,
    }
}

impl H {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }

    async fn post(&self, auth: Option<&str>, path: &str, body: Value) -> (u16, Value) {
        let mut r = self.http.post(self.url(path)).json(&body);
        if let Some(a) = auth {
            r = r.bearer_auth(a);
        }
        let resp = r.send().await.unwrap();
        (
            resp.status().as_u16(),
            resp.json().await.unwrap_or(Value::Null),
        )
    }

    async fn get(&self, auth: Option<&str>, path: &str) -> (u16, Value) {
        let mut r = self.http.get(self.url(path));
        if let Some(a) = auth {
            r = r.bearer_auth(a);
        }
        let resp = r.send().await.unwrap();
        (
            resp.status().as_u16(),
            resp.json().await.unwrap_or(Value::Null),
        )
    }

    fn grants(&self) -> Vec<Value> {
        self.mock
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == "/api/secretary/secrets/grant")
            .map(|(_, b)| b.clone())
            .collect()
    }

    async fn public_key(&self) -> Vec<u8> {
        let (s, v) = self.get(Some(&self.token), "/v1/secrets/status").await;
        assert_eq!(s, 200, "{v}");
        STANDARD.decode(v["grantKey"].as_str().unwrap()).unwrap()
    }
}

fn put_body() -> Value {
    json!({"name": "POKO_API_KEY", "value": VALUE, "project": "proj-poko", "environment": "prod",
           "path": "Pokobot/", "createFolder": true, "confirmed": true})
}

fn verify(pk: &[u8], grant: &str) {
    let rest = grant.strip_prefix("g1.").unwrap();
    let (p64, s64) = rest.split_once('.').unwrap();
    let mut signed = b"infisical-broker-grant-v1\n".to_vec();
    signed.extend_from_slice(&URL_SAFE_NO_PAD.decode(p64).unwrap());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pk)
        .verify(&signed, &URL_SAFE_NO_PAD.decode(s64).unwrap())
        .expect("grant signature verifies");
}

#[tokio::test]
async fn only_the_owners_devices_get_in() {
    let h = start().await;
    for (auth, path) in [
        (None, "/v1/secrets"),
        (Some(SVC), "/v1/secrets"),
        (Some("dev_unknown-token-0000000000000000"), "/v1/secrets"),
    ] {
        let (s, _) = h.post(auth, path, put_body()).await;
        assert_eq!(s, 401, "{auth:?}");
    }
    let (s, _) = h
        .post(
            Some(SVC),
            "/v1/secrets/projects",
            json!({"name": "X", "confirmed": true}),
        )
        .await;
    assert_eq!(s, 401);
    let (s, _) = h.get(Some(SVC), "/v1/secrets/catalog").await;
    assert_eq!(s, 401);
    let (s, _) = h
        .get(None, "/v1/secrets/names?project=p&environment=prod")
        .await;
    assert_eq!(s, 401);
    // A browser session needs the CSRF header to write.
    let resp = h
        .http
        .post(h.url("/v1/session"))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let r = h
        .http
        .post(h.url("/v1/secrets"))
        .header("cookie", &cookie)
        .json(&put_body())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = h
        .http
        .post(h.url("/v1/secrets"))
        .header("cookie", &cookie)
        .header("x-secretary-csrf", "1")
        .json(&put_body())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        h.grants().len(),
        1,
        "only the device request reached Lucina"
    );
}

#[tokio::test]
async fn a_put_carries_a_signed_single_use_grant_for_exactly_that_secret() {
    let h = start().await;
    let pk = h.public_key().await;
    let (s, v) = h.post(Some(&h.token), "/v1/secrets", put_body()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["fingerprint"], "abcd1234");
    let (s, _) = h.post(Some(&h.token), "/v1/secrets", put_body()).await;
    assert_eq!(s, 200);
    let sent = h.grants();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0]["value"], VALUE,
        "the value goes to Lucina beside the grant"
    );
    let g = sent[0]["grant"].as_str().unwrap();
    verify(&pk, g);
    let p = payload(g);
    assert_eq!(p["act"], "put_secret");
    assert_eq!(p["aud"], "infisical-brokerd");
    assert_eq!(p["project"], "proj-poko");
    assert_eq!(p["environment"], "prod");
    assert_eq!(p["path"], "/Pokobot");
    assert_eq!(p["key"], "POKO_API_KEY");
    assert_eq!(p["overwrite"], false);
    assert_eq!(p["create_folder"], true);
    assert_eq!(p["sub"], format!("device:{}", h.device_id));
    let sha: String = Sha256::digest(VALUE.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(p["value_sha256"], sha);
    assert!(p["exp"].as_u64().unwrap() - p["iat"].as_u64().unwrap() <= 120);
    assert!(!g.contains(VALUE) && !p.to_string().contains(VALUE));
    // A fresh id per request.
    assert_ne!(p["jti"], payload(sent[1]["grant"].as_str().unwrap())["jti"]);

    // Catalog, names and projects get their own grants.
    assert_eq!(h.get(Some(&h.token), "/v1/secrets/catalog").await.0, 200);
    assert_eq!(
        h.get(
            Some(&h.token),
            "/v1/secrets/names?project=proj-poko&environment=prod&path=/Pokobot"
        )
        .await
        .0,
        200
    );
    let (s, _) = h
        .post(
            Some(&h.token),
            "/v1/secrets/projects",
            json!({"name": "Poko Shorts", "confirmed": true}),
        )
        .await;
    assert_eq!(s, 200);
    let sent = h.grants();
    let acts: Vec<Value> = sent[2..]
        .iter()
        .map(|b| payload(b["grant"].as_str().unwrap()))
        .collect();
    assert_eq!(acts[0]["act"], "catalog");
    assert!(acts[0].get("project").is_none());
    assert_eq!(acts[1]["path"], "/Pokobot");
    assert_eq!(acts[2]["act"], "create_project");
    assert_eq!(acts[2]["project_name"], "Poko Shorts");
    for b in &sent[2..] {
        assert!(b.get("value").is_none());
        verify(&pk, b["grant"].as_str().unwrap());
    }

    // The key file is private.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(h.dir.path().join("state/secret-grant.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn bad_requests_are_refused_before_anything_is_signed() {
    let h = start().await;
    let cases = [
        json!({"name": "K", "value": VALUE, "project": "p", "environment": "prod"}), // not confirmed
        json!({"name": "has space", "value": VALUE, "project": "p", "environment": "prod", "confirmed": true}),
        json!({"name": "K", "value": VALUE, "project": "p", "environment": "prod", "path": "/a/../b", "confirmed": true}),
        json!({"name": "K", "value": VALUE, "held": "held-12345678", "project": "p", "environment": "prod", "confirmed": true}),
        json!({"name": "K", "value": 12345, "project": "p", "environment": "prod", "confirmed": true}),
        json!({"name": "K", "value": "", "project": "p", "environment": "prod", "confirmed": true}),
        json!({"name": "K", "value": VALUE, "project": "p/x", "environment": "prod", "confirmed": true}),
    ];
    for c in cases {
        let (s, v) = h.post(Some(&h.token), "/v1/secrets", c.clone()).await;
        assert_eq!(s, 400, "{c}");
        assert!(
            !v.to_string().contains(VALUE),
            "errors never echo the value: {v}"
        );
    }
    let (s, _) = h
        .post(
            Some(&h.token),
            "/v1/secrets/projects",
            json!({"name": " padded", "confirmed": true}),
        )
        .await;
    assert_eq!(s, 400);
    assert!(h.grants().is_empty());
    // A held value (masked by Lucina) is named by id instead.
    let (s, _) = h
        .post(Some(&h.token), "/v1/secrets", json!({"name": "K", "held": "held-12345678", "project": "p", "environment": "prod", "confirmed": true}))
        .await;
    assert_eq!(s, 200);
    let b = &h.grants()[0];
    assert_eq!(b["held"], "held-12345678");
    assert_eq!(
        payload(b["grant"].as_str().unwrap())["held"],
        "held-12345678"
    );
    assert!(payload(b["grant"].as_str().unwrap())
        .get("value_sha256")
        .is_none());
}

#[tokio::test]
async fn the_value_never_reaches_the_logs() {
    let h = start().await;
    let (s, _) = h.post(Some(&h.token), "/v1/secrets", put_body()).await;
    assert_eq!(s, 200);
    let _ = h
        .post(
            Some(&h.token),
            "/v1/secrets",
            json!({"name": "bad name", "value": VALUE, "confirmed": true}),
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let text = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
    assert!(text.contains("owner grant minted"), "logging is captured");
    assert!(!text.contains(VALUE), "value in the logs");
    assert!(!text.contains("g1."), "grant in the logs");
}

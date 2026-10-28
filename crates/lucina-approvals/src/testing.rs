//! An in-process mock of secretary-server's requester API (feature
//! `testing`): create, long-poll wait, public key, and Ed25519 grants signed
//! exactly as the contract says. Tests decide requests with
//! [`Mock::approve`], [`Mock::deny`] and [`Mock::expire`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};

use crate::grant::SIGN_PREFIX;
use crate::now_unix;

pub const SERVICE_TOKEN: &str = "svc_test_approvals_token_0123456789";

struct Item {
    id: String,
    body: Value,
    status: &'static str,
    decided_by: Option<String>,
    grant: Option<String>,
    expires_at: i64,
}

struct St {
    key: Ed25519KeyPair,
    public_b64: String,
    items: Mutex<Vec<Item>>,
    /// A long poll returns "pending" after at most this long.
    poll_cap: Duration,
    create_status: AtomicU16,
    waits: AtomicUsize,
    withdrawn: Mutex<Vec<String>>,
}

/// How to build a grant (defaults: a correct one).
#[derive(Debug, Clone)]
pub struct GrantSpec {
    pub action: Option<String>,
    pub payload_hash: Option<String>,
    pub id: Option<String>,
    pub biometric: bool,
    /// Seconds from now (negative: already expired).
    pub exp_in: i64,
    /// Sign with a different key.
    pub wrong_key: bool,
    /// The grant's `decidedBy` (default `dev_main`, a device id).
    pub decided_by: Option<String>,
    /// The grant's `requester.kind` (default: the one the request was made with).
    pub requester_kind: Option<String>,
}

impl Default for GrantSpec {
    fn default() -> Self {
        Self {
            action: None,
            payload_hash: None,
            id: None,
            biometric: true,
            exp_in: 120,
            wrong_key: false,
            decided_by: None,
            requester_kind: None,
        }
    }
}

pub struct Mock {
    pub url: String,
    st: Arc<St>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {SERVICE_TOKEN}"))
}

fn is_sensitive(action: &str, flag: bool) -> bool {
    flag || [
        "secret.",
        "root.",
        "sudo.",
        "message.send",
        "email.send",
        "payment.",
        "account.",
        "broker.",
    ]
    .iter()
    .any(|p| action.starts_with(p))
}

async fn create(
    State(st): State<Arc<St>>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authed(&h) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }
    let forced = st.create_status.load(Ordering::SeqCst);
    if forced != 0 {
        return (
            StatusCode::from_u16(forced).unwrap(),
            Json(json!({"error": "forced"})),
        );
    }
    let mut items = st.items.lock().unwrap();
    let id = format!("apr_mock{:018}", items.len());
    let ttl = body["ttlSecs"].as_i64().unwrap_or(300).clamp(30, 900);
    let expires_at = now_unix() + ttl;
    let sensitive = is_sensitive(
        body["action"].as_str().unwrap_or(""),
        body["sensitive"].as_bool().unwrap_or(false),
    );
    items.push(Item {
        id: id.clone(),
        body,
        status: "pending",
        decided_by: None,
        grant: None,
        expires_at,
    });
    (
        StatusCode::CREATED,
        Json(json!({
            "id": id, "status": "pending", "sensitive": sensitive,
            "expiresAt": chrono::DateTime::from_timestamp(expires_at, 0).unwrap().to_rfc3339(),
        })),
    )
}

async fn wait(
    State(st): State<Arc<St>>,
    h: HeaderMap,
    AxPath(id): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if !authed(&h) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }
    st.waits.fetch_add(1, Ordering::SeqCst);
    let secs: u64 = q.get("timeout").and_then(|t| t.parse().ok()).unwrap_or(25);
    let hold = Duration::from_secs(secs).min(st.poll_cap);
    let started = std::time::Instant::now();
    loop {
        {
            let mut items = st.items.lock().unwrap();
            let Some(it) = items.iter_mut().find(|i| i.id == id) else {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "approval_not_found"})),
                );
            };
            if it.status == "pending" && now_unix() >= it.expires_at {
                it.status = "expired";
            }
            if it.status != "pending" || started.elapsed() >= hold {
                let mut out = json!({"id": it.id, "status": it.status});
                if let Some(d) = &it.decided_by {
                    out["decidedBy"] = json!(d);
                }
                if it.status == "approved" {
                    out["grant"] = json!(it.grant);
                }
                return (StatusCode::OK, Json(out));
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn withdraw(
    State(st): State<Arc<St>>,
    h: HeaderMap,
    AxPath(id): AxPath<String>,
) -> (StatusCode, Json<Value>) {
    if !authed(&h) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }
    let mut items = st.items.lock().unwrap();
    let Some(it) = items.iter_mut().find(|i| i.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "approval_not_found"})),
        );
    };
    st.withdrawn.lock().unwrap().push(id.clone());
    if it.status == "pending" {
        it.status = "expired";
    }
    (StatusCode::OK, Json(json!({"id": id, "status": it.status})))
}

async fn public_key(State(st): State<Arc<St>>) -> Json<Value> {
    Json(json!({"alg": "ed25519", "publicKey": st.public_b64, "keyId": "0123abcd"}))
}

fn sign_with(key: &Ed25519KeyPair, payload: &Value) -> String {
    let p = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
    let sig = key.sign(format!("{SIGN_PREFIX}{p}").as_bytes());
    format!("{p}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
}

impl Mock {
    pub async fn start() -> Self {
        Self::start_with(Duration::from_millis(200)).await
    }

    pub async fn start_with(poll_cap: Duration) -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let st = Arc::new(St {
            public_b64: STANDARD.encode(key.public_key().as_ref()),
            key,
            items: Mutex::new(Vec::new()),
            poll_cap,
            create_status: AtomicU16::new(0),
            waits: AtomicUsize::new(0),
            withdrawn: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/v1/approvals", post(create))
            .route("/v1/approvals/public-key", get(public_key))
            .route("/v1/approvals/{id}/wait", get(wait))
            .route("/v1/approvals/{id}/withdraw", post(withdraw))
            .with_state(st.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url, st, task }
    }

    /// Make `POST /v1/approvals` answer this status (0: back to normal).
    pub fn fail_create_with(&self, status: u16) {
        self.st.create_status.store(status, Ordering::SeqCst);
    }

    /// Ids that were withdrawn, one entry per call (a second call adds a second entry).
    pub fn withdrawn(&self) -> Vec<String> {
        self.st.withdrawn.lock().unwrap().clone()
    }

    /// The status the server holds for `id` (`pending`, `approved`, `denied`, `expired`).
    pub fn status_of(&self, id: &str) -> String {
        let items = self.st.items.lock().unwrap();
        items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.status.to_owned())
            .unwrap_or_default()
    }

    /// How many wait polls the server got.
    pub fn waits(&self) -> usize {
        self.st.waits.load(Ordering::SeqCst)
    }

    /// The `/v1/approvals/public-key` JSON, as a file the verifier pins.
    pub fn write_pubkey_json(&self, path: &Path) {
        let v = json!({"alg": "ed25519", "publicKey": self.st.public_b64, "keyId": "0123abcd"});
        std::fs::write(path, v.to_string()).unwrap();
    }

    /// The raw base64 key as a file.
    pub fn write_pubkey_raw(&self, path: &Path) {
        std::fs::write(path, format!("{}\n", self.st.public_b64)).unwrap();
    }

    pub fn write_token(&self, path: &Path) {
        std::fs::write(path, format!("{SERVICE_TOKEN}\n")).unwrap();
    }

    /// Request bodies created so far, in order.
    pub fn created(&self) -> Vec<Value> {
        self.st
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|i| i.body.clone())
            .collect()
    }

    pub fn ids(&self) -> Vec<String> {
        self.st
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|i| i.id.clone())
            .collect()
    }

    /// Wait until `n` requests exist; returns their ids.
    pub async fn wait_created(&self, n: usize) -> Vec<String> {
        for _ in 0..500 {
            let ids = self.ids();
            if ids.len() >= n {
                return ids;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {n} approvals, have {}", self.ids().len());
    }

    pub fn sign(&self, payload: &Value) -> String {
        sign_with(&self.st.key, payload)
    }

    /// Approve with a correct grant.
    pub fn approve(&self, id: &str) -> String {
        self.approve_with(id, &GrantSpec::default())
    }

    /// Approve with a grant built from `spec` (to test the verifier).
    pub fn approve_with(&self, id: &str, spec: &GrantSpec) -> String {
        let mut items = self.st.items.lock().unwrap();
        let it = items.iter_mut().find(|i| i.id == id).expect("approval id");
        let now = now_unix();
        let payload = json!({
            "v": 1,
            "id": spec.id.clone().unwrap_or_else(|| it.id.clone()),
            "action": spec.action.clone().unwrap_or_else(|| it.body["action"].as_str().unwrap_or("").to_owned()),
            "requester": {
                "kind": spec.requester_kind.clone().map(Value::from).unwrap_or_else(|| it.body["requester"]["kind"].clone()),
                "name": it.body["requester"]["name"],
            },
            "payloadHash": spec.payload_hash.clone().unwrap_or_else(|| it.body["payloadHash"].as_str().unwrap_or("").to_owned()),
            "decision": "approved",
            "decidedBy": spec.decided_by.clone().unwrap_or_else(|| "dev_main".to_owned()),
            "biometric": spec.biometric,
            "iat": now,
            "exp": now + spec.exp_in,
        });
        let grant = if spec.wrong_key {
            let other = Ed25519KeyPair::from_pkcs8(
                Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .unwrap()
                    .as_ref(),
            )
            .unwrap();
            sign_with(&other, &payload)
        } else {
            sign_with(&self.st.key, &payload)
        };
        it.status = "approved";
        it.decided_by = Some("iPhone".into());
        it.grant = Some(grant.clone());
        grant
    }

    pub fn deny(&self, id: &str) {
        let mut items = self.st.items.lock().unwrap();
        let it = items.iter_mut().find(|i| i.id == id).expect("approval id");
        it.status = "denied";
        it.decided_by = Some("iPhone".into());
    }

    pub fn expire(&self, id: &str) {
        let mut items = self.st.items.lock().unwrap();
        let it = items.iter_mut().find(|i| i.id == id).expect("approval id");
        it.status = "expired";
    }
}

/// A fake `nezopt-sudo`: logs each invocation's arguments (one `<arg>`-wrapped
/// line per call) to `log`, then runs the command itself (as the test user).
pub fn write_fake_sudo(path: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nline=''\nfor a in \"$@\"; do line=\"$line<$a>\"; done\nprintf '%s\\n' \"$line\" >> '{}'\nexec \"$@\"\n",
        log.display()
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A ready test setup: mock server, token, pinned key, fake sudo, config.
pub struct Fixture {
    pub mock: Mock,
    pub dir: tempfile::TempDir,
    pub cfg: crate::ApprovalsConfig,
    pub sudo_log: PathBuf,
}

impl Fixture {
    pub async fn new() -> Self {
        let mock = Mock::start().await;
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        mock.write_token(&p("token"));
        mock.write_pubkey_json(&p("approval-grant.pub"));
        write_fake_sudo(&p("fake-sudo"), &p("sudo.log"));
        let cfg = crate::ApprovalsConfig::from_lookup(&|k| match k {
            "LUCINA_SECRETARY_SERVER_URL" => Some(mock.url.clone()),
            "LUCINA_SECRETARY_SERVICE_TOKEN_FILE" => Some(p("token").display().to_string()),
            "LUCINA_APPROVAL_PUBKEY_FILE" => Some(p("approval-grant.pub").display().to_string()),
            "LUCINA_APPROVAL_USED_FILE" => Some(p("used.jsonl").display().to_string()),
            "LUCINA_ROOT_SUDO" => Some(p("fake-sudo").display().to_string()),
            "LUCINA_ROOT_AUDIT_FILE" => Some(p("audit.jsonl").display().to_string()),
            "LUCINA_ROOT_CMD_TIMEOUT_SECS" => Some("5".into()),
            _ => None,
        });
        let sudo_log = p("sudo.log");
        Self {
            mock,
            dir,
            cfg,
            sudo_log,
        }
    }

    pub fn approvals(&self) -> crate::Approvals {
        crate::Approvals::new(self.cfg.clone()).expect("fixture is configured")
    }

    /// What the fake sudo was called with, one entry per call.
    pub fn sudo_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.sudo_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn audit_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.cfg.root_audit)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

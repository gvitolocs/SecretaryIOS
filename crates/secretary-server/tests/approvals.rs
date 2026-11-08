//! Owner approvals end to end: requesters (service token, device tokens),
//! approvers (devices, a browser cookie), the main device and its 30 s
//! escalation, biometric proofs, grants and the audit log.

use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use reqwest::{Client, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::notify::push::{Kind, Subscription};
use secretary_server::owner::OwnerStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const EMAIL: &str = "owner@example.com";
const PASSWORD: &str = "correct horse battery staple";
const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct H {
    base: String,
    http: Client,
    state: AppState,
    data: std::path::PathBuf,
    svc: String,
    _dir: tempfile::TempDir,
}

struct Dev {
    id: String,
    token: String,
}

async fn harness(tweak: impl FnOnce(&mut Config)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("state");
    let mut cfg = Config::new("http://127.0.0.1:9", None);
    cfg.data_dir = data.clone();
    cfg.login_rate_per_minute = 1000;
    cfg.rate_limit_per_minute = 10_000;
    cfg.notify.ingest = false;
    let svc_path = dir.path().join("service.token");
    secretary_server::service::write_new(&svc_path, false).unwrap();
    cfg.service_token = Some(ServiceToken::read_file(svc_path.to_str().unwrap()).unwrap());
    let svc = std::fs::read_to_string(&svc_path)
        .unwrap()
        .trim()
        .to_owned();
    tweak(&mut cfg);
    OwnerStore::new(&data).set(EMAIL, PASSWORD, false).unwrap();
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = app(state.clone());
    tokio::spawn(async move {
        serve_on(listener, router, std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: Client::new(),
        state,
        data,
        svc,
        _dir: dir,
    }
}

fn sign(sk: &SigningKey, id: &str, hash: &str, decision: &str) -> String {
    let msg = format!("secretary-approval-v1\n{id}\n{hash}\n{decision}");
    let sig: Signature = sk.sign(msg.as_bytes());
    STANDARD.encode(sig.to_der().as_bytes())
}

/// The proof of possession for registering `sk` on `device_id`.
fn key_proof(sk: &SigningKey, device_id: &str, pk_b64: &str) -> Value {
    let msg = format!("secretary-approval-key-v1\n{device_id}\n{pk_b64}");
    let sig: Signature = sk.sign(msg.as_bytes());
    json!({"signature": STANDARD.encode(sig.to_der().as_bytes())})
}

/// The proof that lets a device change its PIN without the current one.
fn pin_proof(sk: &SigningKey, device_id: &str) -> Value {
    let sig: Signature = sk.sign(format!("secretary-approval-pin-v1\n{device_id}").as_bytes());
    json!({"signature": STANDARD.encode(sig.to_der().as_bytes())})
}

fn pk_b64(sk: &SigningKey) -> String {
    STANDARD.encode(sk.verifying_key().to_encoded_point(false).as_bytes())
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_slice(&[seed; 32]).unwrap()
}

fn proof(sk: &SigningKey, key_id: &str, id: &str, hash: &str, decision: &str) -> Value {
    json!({"keyId": key_id, "signature": sign(sk, id, hash, decision)})
}

impl H {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }

    fn device(&self, name: &str) -> Dev {
        let (d, token) = DeviceStore::new(&self.data).issue(name).unwrap();
        Dev { id: d.id, token }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut rb = self.http.request(m, self.url(path));
        if let Some(b) = bearer {
            rb = rb.bearer_auth(b);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn req_body(action: &str) -> Value {
        json!({
            "requester": {"kind": "lucina", "name": "Lucina", "detail": "ask_lucina from Claude Code"},
            "action": action,
            "summary": "Install the fixed oracle-poko-tunnel unit and restart it",
            "details": "install -m 644 a b\nsystemctl daemon-reload",
            "risk": "medium",
            "payloadHash": HASH,
        })
    }

    /// Create as the service token; returns the id.
    async fn ask(&self, action: &str) -> String {
        self.ask_with(action, Self::req_body(action)).await
    }

    async fn ask_with(&self, _action: &str, body: Value) -> String {
        let (s, v) = self
            .call("POST", "/v1/approvals", Some(&self.svc), Some(body))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn decide(
        &self,
        dev: &Dev,
        id: &str,
        decision: &str,
        proof: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = json!({"decision": decision, "payloadHash": HASH});
        if let Some(p) = proof {
            b["proof"] = p;
        }
        self.call(
            "POST",
            &format!("/v1/approvals/{id}/decide"),
            Some(&dev.token),
            Some(b),
        )
        .await
    }

    async fn wait(&self, id: &str, timeout: u64) -> (StatusCode, Value) {
        self.call(
            "GET",
            &format!("/v1/approvals/{id}/wait?timeout={timeout}"),
            Some(&self.svc),
            None,
        )
        .await
    }

    /// Register `sk` as `dev`'s key, with a valid proof of possession.
    async fn register_key(&self, dev: &Dev, sk: &SigningKey) -> (StatusCode, Value) {
        let pk = pk_b64(sk);
        self.call(
            "POST",
            "/v1/approvals/keys",
            Some(&dev.token),
            Some(json!({
                "publicKey": pk,
                "label": "Face ID on iPhone",
                "proof": key_proof(sk, &dev.id, &pk),
            })),
        )
        .await
    }

    /// Register `sk` on `dev` while another device already has a key: the registration opens an
    /// `approver.enroll` request, which `approver` approves with its key. Returns the new key id.
    async fn enroll(
        &self,
        dev: &Dev,
        sk: &SigningKey,
        approver: &Dev,
        approver_key: &SigningKey,
        approver_key_id: &str,
    ) -> String {
        let (s, v) = self.register_key(dev, sk).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["status"], "pending_approval", "{v}");
        let key_id = v["keyId"].as_str().unwrap().to_owned();
        let id = v["approvalId"].as_str().unwrap().to_owned();
        let (_, item) = self
            .call(
                "GET",
                &format!("/v1/approvals/{id}"),
                Some(&approver.token),
                None,
            )
            .await;
        assert_eq!(
            (item["action"].as_str(), item["sensitive"].clone()),
            (Some("approver.enroll"), json!(true))
        );
        let hash = item["payloadHash"].as_str().unwrap().to_owned();
        let (s, v) = self
            .call(
                "POST",
                &format!("/v1/approvals/{id}/decide"),
                Some(&approver.token),
                Some(json!({"decision": "approve", "payloadHash": hash,
                            "proof": proof(approver_key, approver_key_id, &id, &hash, "approve")})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        key_id
    }

    /// Approve with the app PIN instead of a proof.
    async fn decide_pin(&self, dev: &Dev, id: &str, pin: &str) -> (StatusCode, Value) {
        self.call(
            "POST",
            &format!("/v1/approvals/{id}/decide"),
            Some(&dev.token),
            Some(json!({"decision": "approve", "payloadHash": HASH, "pin": pin})),
        )
        .await
    }

    async fn put_pin(&self, dev: &Dev, body: Value) -> (StatusCode, Value) {
        self.call("PUT", "/v1/approvals/pin", Some(&dev.token), Some(body))
            .await
    }

    async fn has_pin(&self, dev: &Dev) -> bool {
        let (_, v) = self
            .call("GET", "/v1/approvals/settings", Some(&dev.token), None)
            .await;
        v["hasPin"].as_bool().unwrap()
    }

    /// Inbox bodies of the approval-category notices (not prompts).
    async fn notices(&self, dev: &Dev) -> Vec<String> {
        let (_, v) = self
            .call("GET", "/v1/notifications", Some(&dev.token), None)
            .await;
        v["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|n| n["category"] == "approval")
                    .map(|n| n["body"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(self.data.join("approvals-audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn audit_events(&self, id: &str) -> Vec<String> {
        self.audit()
            .iter()
            .filter(|e| e["id"] == id)
            .map(|e| e["event"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The Ed25519 public key from the public route.
    async fn grant_key(&self) -> Vec<u8> {
        let (s, v) = self
            .call("GET", "/v1/approvals/public-key", None, None)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["alg"], "ed25519");
        let pk = STANDARD.decode(v["publicKey"].as_str().unwrap()).unwrap();
        assert_eq!(v["keyId"].as_str().unwrap().len(), 8);
        pk
    }

    /// Verify a grant like a requester's verifier would; returns its payload.
    async fn verify_grant(&self, grant: &str) -> Value {
        let pk = self.grant_key().await;
        let (p64, s64) = grant.split_once('.').unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pk)
            .verify(
                format!("secretary-approval-v1.{p64}").as_bytes(),
                &URL_SAFE_NO_PAD.decode(s64).unwrap(),
            )
            .expect("grant signature verifies");
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p64).unwrap()).unwrap()
    }

    fn subscribe(&self, dev: &Dev, n: u8) {
        self.state
            .notify
            .pusher
            .upsert(Subscription {
                id: format!("ps_t{n}"),
                kind: Kind::Apns,
                device_id: dev.id.clone(),
                device_name: "t".into(),
                label: String::new(),
                endpoint: format!("{n:02x}").repeat(32),
                p256dh: String::new(),
                auth: String::new(),
                lang: None,
                created_at: 0,
                last_success_at: None,
                failures: 0,
                last_error: None,
            })
            .unwrap();
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn full_flow_ask_list_wait_approve_grant() {
    let h = harness(|_| {}).await;
    let phone = h.device("iPhone");

    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals",
            Some(&h.svc),
            Some(H::req_body("agent.permission")),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "pending");
    assert_eq!(v["sensitive"], false);
    let id = v["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("apr_") && id.len() == 4 + 22, "{id}");
    let exp = time::OffsetDateTime::parse(
        v["expiresAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
    .unix_timestamp();
    assert!((exp - now_unix() - 300).abs() <= 2, "default TTL is 300 s");

    // The phone sees it, with everything the prompt shows.
    let (s, v) = h
        .call(
            "GET",
            "/v1/approvals?status=pending",
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let item = &v["approvals"][0];
    assert_eq!(item["id"], id.as_str());
    assert_eq!(
        item["requester"],
        json!({"kind": "lucina", "name": "Lucina", "detail": "ask_lucina from Claude Code"})
    );
    assert_eq!(item["action"], "agent.permission");
    assert_eq!(
        item["details"],
        "install -m 644 a b\nsystemctl daemon-reload"
    );
    assert_eq!(item["payloadHash"], HASH);
    assert_eq!(item["risk"], "medium");
    assert_eq!(item["sensitive"], false);
    assert_eq!(item["escalated"], true, "no main device: open to all");
    assert_eq!(item["youAreMain"], false);

    // Nothing yet: a short wait says pending.
    let (s, v) = h.wait(&id, 1).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("pending")));
    assert!(v.get("grant").is_none());

    // A long poll started now is woken by the decision.
    let waiter = {
        let (http, url, svc) = (
            h.http.clone(),
            h.url(&format!("/v1/approvals/{id}/wait?timeout=20")),
            h.svc.clone(),
        );
        tokio::spawn(async move {
            let t = std::time::Instant::now();
            let v: Value = http
                .get(url)
                .bearer_auth(svc)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            (v, t.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (s, v) = h.decide(&phone, &id, "approve", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));
    let (w, took) = waiter.await.unwrap();
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(w["status"], "approved");
    assert_eq!(w["decidedBy"], "iPhone");
    assert!(w["decidedAt"].as_str().unwrap().contains('T'));

    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(g["v"], 1);
    assert_eq!(g["id"], id.as_str());
    assert_eq!(g["action"], "agent.permission");
    assert_eq!(g["requester"], json!({"kind": "lucina", "name": "Lucina"}));
    assert_eq!(g["payloadHash"], HASH);
    assert_eq!(g["decision"], "approved");
    assert_eq!(g["decidedBy"], phone.id.as_str());
    assert_eq!(g["biometric"], false);
    assert_eq!(g["exp"].as_i64().unwrap(), g["iat"].as_i64().unwrap() + 120);
    assert!((g["iat"].as_i64().unwrap() - now_unix()).abs() <= 2);

    // It left the pending list, and the id still tells "already answered".
    let (_, v) = h
        .call("GET", "/v1/approvals", Some(&phone.token), None)
        .await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 0);
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}"),
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str(), v["decidedBy"].as_str()),
        (StatusCode::OK, Some("approved"), Some("iPhone"))
    );

    // The audit trail names every step and never the details.
    let events = h.audit_events(&id);
    for e in ["created", "notified", "decided", "waited"] {
        assert!(events.iter().any(|x| x == e), "{e} in {events:?}");
    }
    let raw = std::fs::read_to_string(h.data.join("approvals-audit.jsonl")).unwrap();
    assert!(
        !raw.contains("systemctl"),
        "details stay out of the audit log"
    );
    use std::os::unix::fs::PermissionsExt;
    for f in ["approvals-audit.jsonl", "approval-grant.key"] {
        let mode = std::fs::metadata(h.data.join(f))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{f}");
    }

    // The approval is in the inbox with the web route.
    let items = h.state.notify.inbox.page(None, 50, false).items;
    let n = items
        .iter()
        .find(|n| n.dedup_key.as_deref() == Some(&format!("approval:{id}")))
        .unwrap();
    assert_eq!(n.category.as_str(), "approval");
    assert_eq!(n.route, format!("/approvals/{id}"));
}

#[tokio::test]
async fn a_device_never_answers_its_own_request() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals",
            Some(&a.token),
            Some(H::req_body("agent.permission")),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();

    // A device that asks shows as a device, whatever it claims to be.
    let (_, item) = h
        .call("GET", &format!("/v1/approvals/{id}"), Some(&b.token), None)
        .await;
    assert_eq!(item["requester"]["kind"], "device");
    assert_eq!(item["requester"]["name"], "iPhone");

    let (s, v) = h.decide(&a, &id, "approve", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("self_approval"))
    );
    let (s, v) = h.decide(&a, &id, "deny", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("self_approval"))
    );
    let (_, v) = h.call("GET", "/v1/approvals", Some(&a.token), None).await;
    assert_eq!(
        v["approvals"].as_array().unwrap().len(),
        0,
        "not in its own list"
    );

    // Only the asker can wait; the other device cannot.
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}/wait?timeout=0"),
            Some(&b.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("approval_not_found"))
    );
    let (s, _) = h.wait(&id, 0).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nor the service token");
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}/wait?timeout=0"),
            Some(&a.token),
            None,
        )
        .await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("pending")));

    let (s, _) = h.decide(&b, &id, "approve", None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}/wait?timeout=0"),
            Some(&a.token),
            None,
        )
        .await;
    assert_eq!(v["status"], "approved");
    assert!(v["grant"].is_string());
}

#[tokio::test]
async fn the_service_token_asks_but_never_decides() {
    let h = harness(|_| {}).await;
    let phone = h.device("iPhone");
    let id = h.ask("agent.permission").await;
    let body = json!({"decision": "approve", "payloadHash": HASH});
    for (m, p, b) in [
        (
            "POST",
            format!("/v1/approvals/{id}/decide"),
            Some(body.clone()),
        ),
        ("GET", "/v1/approvals".to_owned(), None),
        ("GET", format!("/v1/approvals/{id}"), None),
        ("GET", "/v1/approvals/settings".to_owned(), None),
        (
            "PUT",
            "/v1/approvals/settings".to_owned(),
            Some(json!({"mainDeviceId": phone.id})),
        ),
        ("DELETE", "/v1/approvals/keys".to_owned(), None),
    ] {
        let (s, v) = h.call(m, &p, Some(&h.svc), b).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}: {v}");
    }
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals/keys",
            Some(&h.svc),
            Some(json!({"publicKey": "x", "proof": {"signature": "x"}})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    for (m, p, b) in [
        ("PUT", "/v1/approvals/pin", Some(json!({"pin": "123456"}))),
        ("DELETE", "/v1/approvals/pin", None),
    ] {
        let (s, v) = h.call(m, p, Some(&h.svc), b).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}: {v}");
    }
    // Still undecided.
    let (_, v) = h.wait(&id, 0).await;
    assert_eq!(v["status"], "pending");
    // Strangers cannot even ask.
    for bearer in [
        None,
        Some("sec_nope_nope_nope_nope_nope_nope_nope_nope"),
        Some("svc_wrong_wrong_wrong_wrong_wrong_wrong"),
    ] {
        let (s, _) = h
            .call(
                "POST",
                "/v1/approvals",
                bearer,
                Some(H::req_body("agent.permission")),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bearer:?}");
    }
}

#[tokio::test]
async fn requests_expire() {
    let h = harness(|c| c.approvals.min_ttl = Duration::from_secs(1)).await;
    let phone = h.device("iPhone");
    let mut b = H::req_body("agent.permission");
    b["ttlSecs"] = json!(1);
    let id = h.ask_with("", b).await;
    let t = std::time::Instant::now();
    let (s, v) = h.wait(&id, 10).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("expired")),
        "{v}"
    );
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "the wait ends at the expiry: {:?}",
        t.elapsed()
    );
    assert!(v.get("grant").is_none());

    let (s, v) = h.decide(&phone, &id, "approve", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::GONE, Some("approval_expired"))
    );
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}"),
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("expired")));
    let (_, v) = h
        .call("GET", "/v1/approvals", Some(&phone.token), None)
        .await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 0);
    assert!(h.audit_events(&id).contains(&"expired".to_owned()));

    // The real minimum is 30 s, and the maximum 900 s.
    let h2 = harness(|_| {}).await;
    let mut b = H::req_body("agent.permission");
    b["ttlSecs"] = json!(1);
    let (_, v) = h2
        .call("POST", "/v1/approvals", Some(&h2.svc), Some(b))
        .await;
    let exp = time::OffsetDateTime::parse(
        v["expiresAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
    .unix_timestamp();
    assert!((exp - now_unix() - 30).abs() <= 2);
    let mut b = H::req_body("agent.permission");
    b["ttlSecs"] = json!(99_999);
    let (_, v) = h2
        .call("POST", "/v1/approvals", Some(&h2.svc), Some(b))
        .await;
    let exp = time::OffsetDateTime::parse(
        v["expiresAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
    .unix_timestamp();
    assert!((exp - now_unix() - 900).abs() <= 2);
}

#[tokio::test]
async fn a_requester_can_withdraw_its_pending_request() {
    let h = harness(|_| {}).await;
    let phone = h.device("iPhone");
    let other = h.device("Other requester");
    let id = h.ask("agent.permission").await;
    let path = format!("/v1/approvals/{id}/withdraw");

    // A long poll that is already waiting wakes up with `expired`.
    let waiter = {
        let (http, url, svc, id) = (
            h.http.clone(),
            h.url(&format!("/v1/approvals/{id}/wait?timeout=20")),
            h.svc.clone(),
            id.clone(),
        );
        tokio::spawn(async move {
            let t = std::time::Instant::now();
            let v: Value = http
                .get(url)
                .bearer_auth(svc)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            (v, t.elapsed(), id)
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Anyone but the requester sees nothing: 404, and the request stays open.
    let (s, v) = h.call("POST", &path, Some(&other.token), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    let (s, _) = h.call("POST", &path, Some(&phone.token), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.call("POST", &path, None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, v) = h.wait(&id, 0).await;
    assert_eq!(v["status"], "pending");
    let (_, v) = h
        .call(
            "GET",
            "/v1/approvals?status=pending",
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 1);

    // The requester withdraws; again is the same answer.
    for _ in 0..2 {
        let (s, v) = h.call("POST", &path, Some(&h.svc), None).await;
        assert_eq!(
            (s, v["status"].as_str()),
            (StatusCode::OK, Some("expired")),
            "{v}"
        );
    }
    let (v, took, wid) = waiter.await.unwrap();
    assert_eq!(wid, id);
    assert_eq!(v["status"], "expired", "{v}");
    assert!(took < Duration::from_secs(5), "the poll woke: {took:?}");
    assert!(v.get("grant").is_none());

    // Waiting afterwards says expired; GET says expired; the pending list drops it.
    let (_, v) = h.wait(&id, 0).await;
    assert_eq!(v["status"], "expired");
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}"),
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("expired")));
    let (_, v) = h
        .call(
            "GET",
            "/v1/approvals?status=pending",
            Some(&phone.token),
            None,
        )
        .await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 0);

    // Deciding it afterwards: 410, for approve and deny alike.
    for d in ["approve", "deny"] {
        let (s, v) = h.decide(&phone, &id, d, None).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::GONE, Some("approval_expired")),
            "{d}: {v}"
        );
    }

    // One `withdrawn` line, no `expired` one, never decided.
    let ev = h.audit_events(&id);
    assert_eq!(ev.iter().filter(|e| *e == "withdrawn").count(), 1, "{ev:?}");
    assert!(!ev.contains(&"expired".to_owned()) && !ev.contains(&"decided".to_owned()));

    // An unknown id is 404 too.
    let (s, _) = h
        .call(
            "POST",
            "/v1/approvals/apr_nope/withdraw",
            Some(&h.svc),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn withdrawing_a_decided_request_changes_nothing() {
    let h = harness(|_| {}).await;
    let phone = h.device("iPhone");

    let approved = h.ask("agent.permission").await;
    let (s, _) = h.decide(&phone, &approved, "approve", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .call(
            "POST",
            &format!("/v1/approvals/{approved}/withdraw"),
            Some(&h.svc),
            None,
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{v}"
    );
    let (_, v) = h.wait(&approved, 0).await;
    assert_eq!(v["status"], "approved");
    assert!(v["grant"].is_string(), "the grant is still handed out");

    let denied = h.ask("agent.permission").await;
    let (s, _) = h.decide(&phone, &denied, "deny", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .call(
            "POST",
            &format!("/v1/approvals/{denied}/withdraw"),
            Some(&h.svc),
            None,
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("denied")),
        "{v}"
    );
    let (_, v) = h.wait(&denied, 0).await;
    assert_eq!(v["status"], "denied");
    assert!(!h.audit_events(&denied).contains(&"withdrawn".to_owned()));
    assert!(!h.audit_events(&approved).contains(&"withdrawn".to_owned()));
}

#[tokio::test]
async fn a_second_decision_is_refused_and_the_grant_is_stable() {
    let h = harness(|c| c.approvals.min_ttl = Duration::from_secs(1)).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let id = h.ask("agent.permission").await;
    assert_eq!(h.decide(&a, &id, "approve", None).await.0, StatusCode::OK);
    // The same device's same answer again (a retried notification action) is a no-op 200.
    let (s, v) = h.decide(&a, &id, "approve", None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "approved", "repeated": true})
        )
    );
    for (dev, decision) in [(&a, "deny"), (&b, "approve"), (&b, "deny")] {
        let (s, v) = h.decide(dev, &id, decision, None).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("approval_decided")),
            "{decision}"
        );
    }
    // The same grant on every wait, with the exp it was minted with.
    let (_, w1) = h.wait(&id, 0).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, w2) = h.wait(&id, 0).await;
    assert_eq!(w1["status"], "approved");
    assert_eq!(w1["grant"], w2["grant"]);
    let g = h.verify_grant(w1["grant"].as_str().unwrap()).await;
    assert_eq!(g["exp"].as_i64().unwrap() - g["iat"].as_i64().unwrap(), 120);
    assert_eq!(g["decidedBy"], a.id.as_str());
    assert_eq!(g["id"], id.as_str());

    // Until the request itself expires: then there is no grant any more.
    let mut body = H::req_body("agent.permission");
    body["ttlSecs"] = json!(1);
    let id2 = h.ask_with("", body).await;
    assert_eq!(h.decide(&a, &id2, "approve", None).await.0, StatusCode::OK);
    let (_, v) = h.wait(&id2, 0).await;
    assert_eq!(v["status"], "approved");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (_, v) = h.wait(&id2, 0).await;
    assert_eq!(v["status"], "expired");
    assert!(v.get("grant").is_none());
}

#[tokio::test]
async fn only_the_main_device_decides_until_the_escalation() {
    let h = harness(|c| c.approvals.escalate_after = Duration::from_millis(1500)).await;
    let (main, other) = (h.device("iPhone"), h.device("iPad"));
    h.subscribe(&main, 1);
    h.subscribe(&other, 2);

    // Settings: unknown devices are refused; the picker lists the devices.
    let (s, v) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&other.token),
            Some(json!({"mainDeviceId": "dev_unknown"})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("device_not_found"))
    );
    let (s, v) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&other.token),
            Some(json!({"mainDeviceId": main.id})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mainDeviceId"], main.id.as_str());
    assert_eq!(v["thisDeviceId"], other.id.as_str());
    let devices = v["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let me = devices
        .iter()
        .find(|d| d["id"] == main.id.as_str())
        .unwrap();
    assert_eq!(
        (
            me["name"].as_str(),
            me["isMain"].clone(),
            me["hasBiometricKey"].clone()
        ),
        (Some("iPhone"), json!(true), json!(false))
    );
    let (_, v) = h
        .call("GET", "/v1/approvals/settings", Some(&main.token), None)
        .await;
    assert_eq!(
        (v["mainDeviceId"].as_str(), v["escalateAfterSecs"].clone()),
        (Some(main.id.as_str()), json!(1))
    );
    let (s, _) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&main.token),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "the field is required");

    let id = h.ask("agent.permission").await;
    // The main device sees it at once; the other does not (it can still read it by id).
    let (_, v) = h
        .call("GET", "/v1/approvals", Some(&main.token), None)
        .await;
    assert_eq!(v["approvals"][0]["id"], id.as_str());
    assert_eq!(
        (
            v["approvals"][0]["youAreMain"].clone(),
            v["approvals"][0]["escalated"].clone()
        ),
        (json!(true), json!(false))
    );
    let (_, v) = h
        .call("GET", "/v1/approvals", Some(&other.token), None)
        .await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 0);
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{id}"),
            Some(&other.token),
            None,
        )
        .await;
    assert_eq!((s, v["escalated"].clone()), (StatusCode::OK, json!(false)));
    let (s, v) = h.decide(&other, &id, "approve", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_main_device"))
    );
    // Not even a deny.
    let (s, _) = h.decide(&other, &id, "deny", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // After the escalation every device sees it and may decide.
    tokio::time::sleep(Duration::from_millis(1700)).await;
    let (_, v) = h
        .call("GET", "/v1/approvals", Some(&other.token), None)
        .await;
    assert_eq!(v["approvals"][0]["id"], id.as_str());
    assert_eq!(
        (
            v["approvals"][0]["youAreMain"].clone(),
            v["approvals"][0]["escalated"].clone()
        ),
        (json!(false), json!(true))
    );
    let (s, v) = h.decide(&other, &id, "approve", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));

    // Pushed to the main device first, then (still pending then) to the rest.
    let notified: Vec<Value> = h
        .audit()
        .into_iter()
        .filter(|e| e["event"] == "notified" && e["id"] == id.as_str())
        .collect();
    assert_eq!(notified.len(), 2, "{notified:?}");
    assert_eq!(
        (
            notified[0]["scope"].as_str(),
            notified[0]["targets"].clone()
        ),
        (Some("main"), json!(1))
    );
    assert_eq!(
        (
            notified[1]["scope"].as_str(),
            notified[1]["targets"].clone()
        ),
        (Some("escalated"), json!(1))
    );

    // A request decided inside the head start is not escalated.
    let id2 = h.ask("agent.permission").await;
    assert_eq!(h.decide(&main, &id2, "deny", None).await.0, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1700)).await;
    let ev = h.audit_events(&id2);
    assert_eq!(ev.iter().filter(|e| *e == "notified").count(), 1, "{ev:?}");
}

#[tokio::test]
async fn sensitive_approval_needs_this_devices_biometric_key() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let (ka, kb) = (key(1), key(2));

    // Registering a key takes a proof of possession by the new key, no password.
    let pka = pk_b64(&ka);
    let register = |dev: &Dev, body: Value| {
        let (h, token) = (&h, dev.token.clone());
        async move {
            h.call("POST", "/v1/approvals/keys", Some(&token), Some(body))
                .await
        }
    };
    let invalid = |r: (StatusCode, Value)| {
        assert_eq!(
            (r.0, r.1["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("proof_invalid")),
            "{}",
            r.1
        );
    };
    // No proof (a password is ignored); a proof by another key; a proof for
    // another device; a proof over another public key; garbage.
    invalid(register(&a, json!({"publicKey": pka, "password": PASSWORD})).await);
    invalid(
        register(
            &a,
            json!({"publicKey": pka, "proof": key_proof(&kb, &a.id, &pka)}),
        )
        .await,
    );
    invalid(
        register(
            &a,
            json!({"publicKey": pka, "proof": key_proof(&ka, &b.id, &pka)}),
        )
        .await,
    );
    invalid(
        register(
            &a,
            json!({"publicKey": pka, "proof": key_proof(&ka, &a.id, &pk_b64(&kb))}),
        )
        .await,
    );
    invalid(
        register(
            &a,
            json!({"publicKey": pka, "proof": {"signature": "AAAA"}}),
        )
        .await,
    );
    assert!(h.notices(&a).await.is_empty(), "nothing was set up yet");
    let (_, v) = h
        .call("GET", "/v1/approvals/settings", Some(&a.token), None)
        .await;
    assert!(v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["hasBiometricKey"] == false));
    assert_eq!(v["hasPin"], false);
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals/keys",
            Some(&a.token),
            Some(json!({"publicKey": "AAAA", "proof": {"signature": "AAAA"}})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request")),
        "{v}"
    );
    let (s, v) = h.register_key(&a, &ka).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (ia, ib) = {
        let ia = v["keyId"].as_str().unwrap().to_owned();
        assert_eq!(ia.len(), 8);
        // The second device's key waits for the first device's Face ID.
        let ib = h.enroll(&b, &kb, &a, &ka, &ia).await;
        (ia, ib)
    };
    assert_ne!(ia, ib);
    let (_, v) = h
        .call("GET", "/v1/approvals/settings", Some(&a.token), None)
        .await;
    assert!(v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["hasBiometricKey"] == true));
    // Both devices were told, and the audit log has the registrations.
    assert_eq!(
        h.notices(&a).await,
        [
            "iPad can now approve requests (added on Face ID on iPhone)",
            "Add iPad as an approver device (Face ID on iPhone)",
            "Approvals set up on iPhone"
        ]
    );
    assert_eq!(
        h.audit()
            .iter()
            .filter(|e| e["event"] == "key_registered")
            .count(),
        2
    );

    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals",
            Some(&h.svc),
            Some(H::req_body("root.exec")),
        )
        .await;
    assert_eq!(
        (s, v["sensitive"].clone()),
        (StatusCode::CREATED, json!(true))
    );
    let id = v["id"].as_str().unwrap().to_owned();
    let denied = |r: (StatusCode, Value)| {
        assert_eq!(
            (r.0, r.1["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("biometric_required")),
            "{}",
            r.1
        );
    };

    // No proof; garbage; another device's key id; the right id but a foreign
    // signature; a proof for the other decision or another request.
    denied(h.decide(&a, &id, "approve", None).await);
    denied(
        h.decide(
            &a,
            &id,
            "approve",
            Some(json!({"keyId": ia, "signature": "AAAA"})),
        )
        .await,
    );
    denied(
        h.decide(
            &a,
            &id,
            "approve",
            Some(proof(&kb, &ib, &id, HASH, "approve")),
        )
        .await,
    );
    denied(
        h.decide(
            &a,
            &id,
            "approve",
            Some(proof(&kb, &ia, &id, HASH, "approve")),
        )
        .await,
    );
    denied(
        h.decide(&a, &id, "approve", Some(proof(&ka, &ia, &id, HASH, "deny")))
            .await,
    );
    denied(
        h.decide(
            &a,
            &id,
            "approve",
            Some(proof(&ka, &ia, "apr_other", HASH, "approve")),
        )
        .await,
    );
    // The web (cookie) has no key at all.
    let (_, ws) = h.call("GET", "/v1/approvals", Some(&a.token), None).await;
    assert_eq!(
        ws["approvals"].as_array().unwrap().len(),
        1,
        "still pending"
    );

    // This device's own key works, and the grant says so.
    let (s, v) = h
        .decide(
            &a,
            &id,
            "approve",
            Some(proof(&ka, &ia, &id, HASH, "approve")),
        )
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));
    let (_, w) = h.wait(&id, 0).await;
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(g["biometric"], true);
    assert_eq!(g["verifiedBy"], "device_key");
    assert_eq!(g["action"], "root.exec");
    assert_eq!(g["decidedBy"], a.id.as_str());
    let decided = h
        .audit()
        .into_iter()
        .find(|e| e["event"] == "decided" && e["id"] == id.as_str())
        .unwrap();
    assert_eq!(decided["biometric"], true);

    // A key can be replaced and removed.
    let (s, v) = h
        .call("DELETE", "/v1/approvals/keys", Some(&a.token), None)
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"removed": true})));
    let id2 = h.ask("secret.reveal").await;
    denied(
        h.decide(
            &a,
            &id2,
            "approve",
            Some(proof(&ka, &ia, &id2, HASH, "approve")),
        )
        .await,
    );
    // The other device's key still works for it.
    assert_eq!(
        h.decide(
            &b,
            &id2,
            "approve",
            Some(proof(&kb, &ib, &id2, HASH, "approve"))
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn deny_needs_no_proof_and_hands_out_no_grant() {
    let h = harness(|_| {}).await;
    let a = h.device("iPhone");
    let id = h.ask("sudo.run").await;
    let (s, v) = h.decide(&a, &id, "deny", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "denied"})));
    let (_, w) = h.wait(&id, 0).await;
    assert_eq!(w["status"], "denied");
    assert_eq!(w["decidedBy"], "iPhone");
    assert!(w.get("grant").is_none());
    let (s, v) = h.decide(&a, &id, "maybe", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request")),
        "{v}"
    );
}

#[tokio::test]
async fn a_changed_payload_is_a_conflict() {
    let h = harness(|_| {}).await;
    let a = h.device("iPhone");
    let id = h.ask("agent.permission").await;
    let other = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    for decision in ["approve", "deny"] {
        let (s, v) = h
            .call(
                "POST",
                &format!("/v1/approvals/{id}/decide"),
                Some(&a.token),
                Some(json!({"decision": decision, "payloadHash": other})),
            )
            .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("payload_mismatch")),
            "{decision}"
        );
    }
    let (_, w) = h.wait(&id, 0).await;
    assert_eq!(w["status"], "pending");
    assert_eq!(h.decide(&a, &id, "approve", None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn browser_decisions_need_csrf_and_cannot_approve_sensitive_requests() {
    let h = harness(|_| {}).await;
    // The owner signs in on the web.
    let r = h
        .http
        .post(h.url("/v1/session"))
        .json(&json!({"email": EMAIL, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let post = |path: String, csrf: bool, body: Value| {
        let mut rb = h
            .http
            .post(h.url(&path))
            .header("cookie", &cookie)
            .json(&body);
        if csrf {
            rb = rb.header("x-secretary-csrf", "1");
        }
        async move {
            let r = rb.send().await.unwrap();
            (r.status(), r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let id = h.ask("agent.permission").await;
    let body = json!({"decision": "approve", "payloadHash": HASH});
    let (s, v) = post(format!("/v1/approvals/{id}/decide"), false, body.clone()).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("csrf")),
        "{v}"
    );
    let (_, w) = h.wait(&id, 0).await;
    assert_eq!(w["status"], "pending", "the CSRF-less request did nothing");
    let (s, v) = post(format!("/v1/approvals/{id}/decide"), true, body.clone()).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));

    // The web has no Secure Enclave key: sensitive requests can be denied, not approved.
    let id2 = h.ask("secret.reveal").await;
    let (s, v) = post(format!("/v1/approvals/{id2}/decide"), true, body).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("biometric_required")),
        "{v}"
    );
    let (s, v) = post(
        format!("/v1/approvals/{id2}/decide"),
        true,
        json!({"decision": "deny", "payloadHash": HASH}),
    )
    .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "denied"})));
}

#[tokio::test]
async fn requests_are_validated_and_rate_limited() {
    let h = harness(|c| c.approvals.creates_per_minute = 3).await;
    let mut seen = Vec::new();
    for (field, value) in [
        ("action", json!("Root.Exec")),
        ("summary", json!("")),
        ("summary", json!("x".repeat(201))),
        ("details", json!("x".repeat(4001))),
        ("risk", json!("extreme")),
        ("payloadHash", json!("sha256:abc")),
    ] {
        let mut b = H::req_body("agent.permission");
        b[field] = value;
        let (s, v) = h.call("POST", "/v1/approvals", Some(&h.svc), Some(b)).await;
        seen.push((field, s));
        if s == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(v["error"]["code"], "rate_limited");
            continue;
        }
        assert_eq!(s, StatusCode::BAD_REQUEST, "{field}: {v}");
    }
    // Three tries used the bucket; the rest were limited, per requester.
    assert_eq!(
        seen.iter()
            .filter(|(_, s)| *s == StatusCode::TOO_MANY_REQUESTS)
            .count(),
        3,
        "{seen:?}"
    );
    let dev = h.device("iPhone");
    let (s, _) = h
        .call(
            "POST",
            "/v1/approvals",
            Some(&dev.token),
            Some(H::req_body("agent.permission")),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "another requester has its own bucket"
    );
    // The public key needs no sign-in and is stable.
    assert_eq!(h.grant_key().await, h.grant_key().await);
    assert_eq!(h.grant_key().await.len(), 32);
}

const PIN: &str = "123456";

fn code(r: &(StatusCode, Value)) -> (StatusCode, Option<&str>) {
    (r.0, r.1["error"]["code"].as_str())
}

#[tokio::test]
async fn the_pin_is_set_replaced_and_removed() {
    use std::os::unix::fs::PermissionsExt;
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let ka = key(1);
    assert!(!h.has_pin(&a).await);

    // Exactly six ASCII digits.
    for bad in ["12345", "1234567", "12a456", "", "١٢٣٤٥٦", " 12345"] {
        let r = h.put_pin(&a, json!({"pin": bad})).await;
        assert_eq!(
            code(&r),
            (StatusCode::BAD_REQUEST, Some("bad_request")),
            "{bad:?}"
        );
    }
    let r = h.put_pin(&a, json!({})).await;
    assert_eq!(r.0, StatusCode::BAD_REQUEST);
    assert!(!h.has_pin(&a).await);

    // The first PIN needs nothing beyond the device itself.
    let r = h.put_pin(&a, json!({"pin": PIN})).await;
    assert_eq!(r, (StatusCode::OK, json!({"hasPin": true})));
    assert!(h.has_pin(&b).await);
    let path = h.data.join("approval-pin.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.contains("$argon2id$") && !file.contains(PIN), "{file}");

    // Replacing needs the current PIN or a proof from this device's key.
    let r = h.put_pin(&b, json!({"pin": "654321"})).await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, Some("pin_auth_required")),
        "{}",
        r.1
    );
    let r = h
        .put_pin(&b, json!({"pin": "654321", "currentPin": "000000"}))
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, Some("pin_wrong")),
        "{}",
        r.1
    );
    assert_eq!(r.1["error"]["attemptsLeft"], 4);
    let r = h
        .put_pin(&b, json!({"pin": "654321", "currentPin": PIN}))
        .await;
    assert_eq!(r, (StatusCode::OK, json!({"hasPin": true})));
    // The old PIN is gone; the count was reset by the right one.
    let id = h.ask("root.exec").await;
    let r = h.decide_pin(&a, &id, PIN).await;
    assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("pin_wrong")));
    assert_eq!(r.1["error"]["attemptsLeft"], 4);

    // A proof needs a registered key on this device, made by that key.
    let proof_a = pin_proof(&ka, &a.id);
    let r = h
        .put_pin(&a, json!({"pin": "111222", "proof": proof_a}))
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, Some("pin_auth_required")),
        "no key yet"
    );
    assert_eq!(h.register_key(&a, &ka).await.0, StatusCode::OK);
    for proof in [
        pin_proof(&key(2), &a.id),
        pin_proof(&ka, &b.id),
        json!({"signature": "AAAA"}),
    ] {
        let r = h
            .put_pin(&a, json!({"pin": "111222", "proof": proof}))
            .await;
        assert_eq!(
            code(&r),
            (StatusCode::FORBIDDEN, Some("pin_auth_required")),
            "{}",
            r.1
        );
    }
    // b has no key of its own: a's proof does not carry over.
    let r = h
        .put_pin(&b, json!({"pin": "111222", "proof": pin_proof(&ka, &b.id)}))
        .await;
    assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("pin_auth_required")));
    let r = h
        .put_pin(&a, json!({"pin": "111222", "proof": proof_a}))
        .await;
    assert_eq!(r, (StatusCode::OK, json!({"hasPin": true})));
    let id2 = h.ask("root.exec").await;
    assert_eq!(h.decide_pin(&b, &id2, "111222").await.0, StatusCode::OK);

    // Removing follows the same rule.
    let del = |dev: &Dev, body: Option<Value>| {
        let (h, token) = (&h, dev.token.clone());
        async move {
            h.call("DELETE", "/v1/approvals/pin", Some(&token), body)
                .await
        }
    };
    assert_eq!(
        code(&del(&b, None).await),
        (StatusCode::FORBIDDEN, Some("pin_auth_required"))
    );
    assert_eq!(
        code(&del(&b, Some(json!({"currentPin": "999999"}))).await),
        (StatusCode::FORBIDDEN, Some("pin_wrong"))
    );
    assert!(h.has_pin(&a).await);
    let r = del(&b, Some(json!({"currentPin": "111222"}))).await;
    assert_eq!(r, (StatusCode::OK, json!({"hasPin": false})));
    assert!(!h.has_pin(&a).await);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("argon2"));
    // Nothing to remove any more, and it needs no proof.
    assert_eq!(
        del(&a, None).await,
        (StatusCode::OK, json!({"hasPin": false}))
    );
    // A proof removes it too.
    assert_eq!(h.put_pin(&b, json!({"pin": PIN})).await.0, StatusCode::OK);
    let r = del(&a, Some(json!({"proof": proof_a}))).await;
    assert_eq!(r, (StatusCode::OK, json!({"hasPin": false})));
    // With no PIN, a PIN decides nothing.
    let id3 = h.ask("root.exec").await;
    let r = h.decide_pin(&a, &id3, PIN).await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, Some("biometric_required"))
    );

    // Every change told the owner's devices.
    let notices = h.notices(&a).await;
    assert_eq!(
        notices
            .iter()
            .filter(
                |n| *n == "Approval PIN changed on iPad" || *n == "Approval PIN changed on iPhone"
            )
            .count(),
        6,
        "{notices:?}"
    );
    let ev: Vec<String> = h
        .audit()
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        ev.iter().any(|e| e == "pin_set") && ev.iter().any(|e| e == "pin_removed"),
        "{ev:?}"
    );
}

#[tokio::test]
async fn a_sensitive_approve_can_use_the_pin_and_the_grant_says_so() {
    let h = harness(|_| {}).await;
    let a = h.device("iPhone");
    assert_eq!(h.put_pin(&a, json!({"pin": PIN})).await.0, StatusCode::OK);
    let id = h.ask("root.exec").await;

    let r = h.decide_pin(&a, &id, "000000").await;
    assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("pin_wrong")));
    assert_eq!(
        r.1,
        json!({"error": {"code": "pin_wrong", "message": "the PIN is wrong", "attemptsLeft": 4}})
    );
    let r = h.decide_pin(&a, &id, "111111").await;
    assert_eq!(r.1["error"]["attemptsLeft"], 3);
    // A malformed PIN is a 400 and costs no attempt.
    let r = h.decide_pin(&a, &id, "12").await;
    assert_eq!(code(&r), (StatusCode::BAD_REQUEST, Some("bad_request")));
    // Neither does a denial (it needs nothing).
    assert_eq!(
        h.decide(&a, &h.ask("secret.reveal").await, "deny", None)
            .await
            .0,
        StatusCode::OK
    );
    let (_, w) = h.wait(&id, 0).await;
    assert_eq!(w["status"], "pending");

    let r = h.decide_pin(&a, &id, PIN).await;
    assert_eq!(r, (StatusCode::OK, json!({"status": "approved"})));
    let (_, w) = h.wait(&id, 0).await;
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(g["biometric"], true);
    assert_eq!(g["verifiedBy"], "pin");
    assert_eq!(g["decidedBy"], a.id.as_str());
    let decided = h
        .audit()
        .into_iter()
        .find(|e| e["event"] == "decided" && e["id"] == id.as_str())
        .unwrap();
    assert_eq!(
        (decided["biometric"].clone(), decided["verifiedBy"].clone()),
        (json!(true), json!("pin"))
    );
    let (_, item) = h
        .call("GET", &format!("/v1/approvals/{id}"), Some(&a.token), None)
        .await;
    assert_eq!(item["verifiedBy"], "pin");

    // The right PIN reset the count.
    let id2 = h.ask("secret.reveal").await;
    let r = h.decide_pin(&a, &id2, "000000").await;
    assert_eq!(r.1["error"]["attemptsLeft"], 4);

    // A valid device-key proof wins: the wrong PIN next to it costs nothing.
    let ka = key(1);
    let (_, v) = h.register_key(&a, &ka).await;
    let ia = v["keyId"].as_str().unwrap().to_owned();
    let r = h
        .call(
            "POST",
            &format!("/v1/approvals/{id2}/decide"),
            Some(&a.token),
            Some(json!({
                "decision": "approve", "payloadHash": HASH, "pin": "000000",
                "proof": proof(&ka, &ia, &id2, HASH, "approve"),
            })),
        )
        .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    let (_, w) = h.wait(&id2, 0).await;
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(
        (g["biometric"].clone(), g["verifiedBy"].clone()),
        (json!(true), json!("device_key"))
    );
    let id3 = h.ask("root.exec").await;
    let r = h.decide_pin(&a, &id3, "000000").await;
    assert_eq!(
        r.1["error"]["attemptsLeft"], 3,
        "the id2 wrong PIN counted, the proof one did not"
    );

    // An unverified approve of a plain request has no verifiedBy.
    let id4 = h.ask("agent.permission").await;
    assert_eq!(h.decide(&a, &id4, "approve", None).await.0, StatusCode::OK);
    let (_, w) = h.wait(&id4, 0).await;
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(g["biometric"], false);
    assert!(g.get("verifiedBy").is_none());
}

#[tokio::test]
async fn five_wrong_pins_lock_it_for_fifteen_minutes_and_the_lock_persists() {
    let h = harness(|_| {}).await;
    let a = h.device("iPhone");
    let ka = key(1);
    assert_eq!(h.put_pin(&a, json!({"pin": PIN})).await.0, StatusCode::OK);
    let (_, v) = h.register_key(&a, &ka).await;
    let ia = v["keyId"].as_str().unwrap().to_owned();
    let id = h.ask("root.exec").await;

    for left in [4, 3, 2, 1] {
        let r = h.decide_pin(&a, &id, "000000").await;
        assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("pin_wrong")));
        assert_eq!(r.1["error"]["attemptsLeft"], left);
    }
    // The fifth locks it: 429 with Retry-After, even for the right PIN next.
    let raw = |base: &str, pin: &str| {
        let rb = h
            .http
            .post(format!("{base}/v1/approvals/{id}/decide"))
            .bearer_auth(&a.token)
            .json(&json!({"decision": "approve", "payloadHash": HASH, "pin": pin}));
        async move {
            let r = rb.send().await.unwrap();
            let retry = r
                .headers()
                .get("retry-after")
                .map(|v| v.to_str().unwrap().parse::<u64>().unwrap());
            (r.status(), retry, r.json::<Value>().await.unwrap())
        }
    };
    let (s, retry, v) = raw(&h.base, "000000").await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("pin_locked"))
    );
    assert!((890..=900).contains(&retry.unwrap()), "{retry:?}");
    assert!(v["error"].get("attemptsLeft").is_none());
    let (s, retry, v) = raw(&h.base, PIN).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("pin_locked"))
    );
    assert!(retry.unwrap() <= 900);

    // The lock is on disk (the unit tests reopen the store and see it).
    let state = std::fs::read_to_string(h.data.join("approval-pin.json")).unwrap();
    assert!(state.contains("lockedUntilMs"), "{state}");

    // The device key is not locked out, and the PIN cannot be changed with the PIN meanwhile.
    let r = h
        .put_pin(&a, json!({"pin": "222333", "currentPin": PIN}))
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::TOO_MANY_REQUESTS, Some("pin_locked"))
    );
    let r = h
        .decide(
            &a,
            &id,
            "approve",
            Some(proof(&ka, &ia, &id, HASH, "approve")),
        )
        .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
}

#[tokio::test]
async fn a_pin_never_helps_with_the_other_checks() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    assert_eq!(h.put_pin(&a, json!({"pin": PIN})).await.0, StatusCode::OK);

    // Its own request: refused before the PIN is looked at (right or wrong).
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals",
            Some(&a.token),
            Some(H::req_body("root.exec")),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let own = v["id"].as_str().unwrap().to_owned();
    for pin in [PIN, "000000"] {
        let r = h.decide_pin(&a, &own, pin).await;
        assert_eq!(
            code(&r),
            (StatusCode::FORBIDDEN, Some("self_approval")),
            "{pin}"
        );
    }
    // Another device may still answer it with the PIN: no attempt was spent.
    let id = h.ask("root.exec").await;
    let r = h.decide_pin(&a, &id, "000000").await;
    assert_eq!(r.1["error"]["attemptsLeft"], 4);

    // Payload mismatch.
    let r = h
        .call(
            "POST",
            &format!("/v1/approvals/{id}/decide"),
            Some(&a.token),
            Some(json!({"decision": "approve", "payloadHash": format!("sha256:{}", "00".repeat(32)), "pin": PIN})),
        )
        .await;
    assert_eq!(code(&r), (StatusCode::CONFLICT, Some("payload_mismatch")));

    // The main-device window.
    let (s, _) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&b.token),
            Some(json!({"mainDeviceId": b.id})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let r = h.decide_pin(&a, &id, PIN).await;
    assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("not_main_device")));
    let r = h.decide_pin(&a, &id, "000000").await;
    assert_eq!(code(&r), (StatusCode::FORBIDDEN, Some("not_main_device")));

    // None of that spent an attempt (one wrong PIN earlier), and a decided request stays decided.
    let (s, _) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&b.token),
            Some(json!({"mainDeviceId": null})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.decide_pin(&a, &id, PIN).await.0, StatusCode::OK);
    // The same answer again is a no-op (no PIN checked); another answer is refused.
    let r = h.decide_pin(&a, &id, "000000").await;
    assert_eq!(
        (r.0, r.1["repeated"].clone()),
        (StatusCode::OK, json!(true))
    );
    let r = h.decide(&a, &id, "deny", None).await;
    assert_eq!(code(&r), (StatusCode::CONFLICT, Some("approval_decided")));
}

/// The proof that revokes `target`'s key, signed by the caller's key.
fn revoke_proof(sk: &SigningKey, caller: &str, target: &str) -> Value {
    let sig: Signature =
        sk.sign(format!("secretary-approval-revoke-v1\n{caller}\n{target}").as_bytes());
    json!({"signature": STANDARD.encode(sig.to_der().as_bytes())})
}

#[tokio::test]
async fn a_mac_becomes_an_approver_only_with_the_iphones_face_id_and_can_be_revoked() {
    let h = harness(|c| {
        c.approvals.escalate_after = Duration::from_secs(60);
        c.approvals.min_ttl = Duration::from_secs(1);
    })
    .await;
    let (phone, mac) = (h.device("iPhone"), h.device("MacBook Air"));
    let (kp, km, kx) = (key(11), key(12), key(13));
    // The iPhone is the first approver and the main device.
    let (s, v) = h.register_key(&phone, &kp).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ip = v["keyId"].as_str().unwrap().to_owned();
    let (s, _) = h
        .call(
            "PUT",
            "/v1/approvals/settings",
            Some(&phone.token),
            Some(json!({"mainDeviceId": phone.id})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.put_pin(
            &phone,
            json!({"pin": PIN, "proof": pin_proof(&kp, &phone.id)})
        )
        .await
        .0,
        StatusCode::OK
    );

    // The Mac registers a key: it is only pending, and useless until the iPhone approves.
    let pkm = pk_b64(&km);
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals/keys",
            Some(&mac.token),
            Some(json!({"publicKey": pkm, "label": "Touch ID on MacBook Air",
                          "proof": key_proof(&km, &mac.id, &pkm)})),
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("pending_approval")),
        "{v}"
    );
    let im = v["keyId"].as_str().unwrap().to_owned();
    let enroll = v["approvalId"].as_str().unwrap().to_owned();
    let (_, item) = h
        .call(
            "GET",
            &format!("/v1/approvals/{enroll}"),
            Some(&phone.token),
            None,
        )
        .await;
    let ehash = item["payloadHash"].as_str().unwrap().to_owned();
    assert_eq!(
        item["summary"],
        "Add MacBook Air as an approver device (Touch ID on MacBook Air)"
    );
    let root = h.ask("root.exec").await;
    let (s, v) = h
        .decide(
            &mac,
            &root,
            "approve",
            Some(proof(&km, &im, &root, HASH, "approve")),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a pending key approves nothing: {v}"
    );

    // The Mac can't approve its own enrollment; the PIN can't either; nor a proof by the wrong key.
    let decide_enroll = |dev: &Dev, body: Value| {
        let (h, token, enroll) = (&h, dev.token.clone(), enroll.clone());
        async move {
            h.call(
                "POST",
                &format!("/v1/approvals/{enroll}/decide"),
                Some(&token),
                Some(body),
            )
            .await
        }
    };
    let (s, v) = decide_enroll(
        &mac,
        json!({"decision": "approve", "payloadHash": ehash,
        "proof": proof(&km, &im, &enroll, &ehash, "approve")}),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("self_approval")),
        "{v}"
    );
    let (s, v) = decide_enroll(
        &phone,
        json!({"decision": "approve", "payloadHash": ehash, "pin": PIN}),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("biometric_required")),
        "{v}"
    );
    let (s, _) = decide_enroll(
        &phone,
        json!({"decision": "approve", "payloadHash": ehash,
        "proof": proof(&kx, &ip, &enroll, &ehash, "approve")}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a foreign key under the iPhone's key id"
    );

    // Face ID on the iPhone adds it; the Mac learns it from its own wait.
    let (s, v) = decide_enroll(
        &phone,
        json!({"decision": "approve", "payloadHash": ehash,
        "proof": proof(&kp, &ip, &enroll, &ehash, "approve")}),
    )
    .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));
    let (s, v) = h
        .call(
            "GET",
            &format!("/v1/approvals/{enroll}/wait?timeout=0"),
            Some(&mac.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str(), v["decidedVia"].as_str()),
        (StatusCode::OK, Some("approved"), Some("Face ID on iPhone")),
        "{v}"
    );
    let ev: Vec<Value> = h
        .audit()
        .into_iter()
        .filter(|e| e["event"] == "key_registered")
        .collect();
    assert_eq!(ev.last().unwrap()["approvedVia"], "Face ID on iPhone");

    // An approver device sees requests at once, without the main device's head start (60 s here).
    let (_, list) = h.call("GET", "/v1/approvals", Some(&mac.token), None).await;
    assert!(
        list["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == root.as_str()),
        "{list}"
    );

    // Good signature: the Mac's own key approves, and every record names the Mac.
    // Wrong device first: the iPhone's key (or key id) does not work from the Mac.
    let (s, _) = h
        .decide(
            &mac,
            &root,
            "approve",
            Some(proof(&kp, &ip, &root, HASH, "approve")),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .decide(
            &mac,
            &root,
            "approve",
            Some(proof(&kp, &im, &root, HASH, "approve")),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let good = proof(&km, &im, &root, HASH, "approve");
    let (s, v) = h.decide(&mac, &root, "approve", Some(good.clone())).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));
    let (_, w) = h.wait(&root, 0).await;
    assert_eq!(
        (w["decidedBy"].as_str(), w["decidedVia"].as_str()),
        (Some("MacBook Air"), Some("Touch ID on MacBook Air"))
    );
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(
        (g["decidedBy"].as_str(), g["verifiedBy"].as_str()),
        (Some(mac.id.as_str()), Some("device_key"))
    );
    let decided = h
        .audit()
        .into_iter()
        .find(|e| e["event"] == "decided" && e["id"] == root.as_str())
        .unwrap();
    assert_eq!(decided["via"], "Touch ID on MacBook Air");
    // First answer wins: the iPhone's late answer is refused, and its card closes.
    let (s, v) = h
        .decide(
            &phone,
            &root,
            "approve",
            Some(proof(&kp, &ip, &root, HASH, "approve")),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("approval_decided"))
    );
    let (_, list) = h
        .call("GET", "/v1/approvals", Some(&phone.token), None)
        .await;
    assert!(list["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["id"] != root.as_str()));

    // Replay: the same proof again, or on another request, does nothing (the same answer again: a no-op 200).
    let (s, v) = h.decide(&mac, &root, "approve", Some(good.clone())).await;
    assert_eq!((s, v["repeated"].clone()), (StatusCode::OK, json!(true)));
    assert_eq!(
        h.audit_events(&root)
            .iter()
            .filter(|e| *e == "decided")
            .count(),
        1
    );
    let other = h.ask("root.exec").await;
    let (s, v) = h.decide(&mac, &other, "approve", Some(good.clone())).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("biometric_required"))
    );

    // Expired: a valid proof is too late.
    let short = h
        .ask_with("root.exec", {
            let mut b = H::req_body("root.exec");
            b["ttlSecs"] = json!(1);
            b
        })
        .await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let (s, v) = h
        .decide(
            &mac,
            &short,
            "approve",
            Some(proof(&km, &im, &short, HASH, "approve")),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::GONE, Some("approval_expired"))
    );

    // Revoking: needs the caller's own key or the PIN; a device without a key can't revoke anyone.
    let revoke = |by: &Dev, target: &str, body: Value| {
        let (h, token, path) = (&h, by.token.clone(), format!("/v1/approvals/keys/{target}"));
        async move { h.call("DELETE", &path, Some(&token), Some(body)).await }
    };
    let (s, _) = revoke(&phone, &mac.id, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = revoke(
        &phone,
        &mac.id,
        json!({"proof": revoke_proof(&km, &phone.id, &mac.id)}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "signed by the wrong key");
    let (s, v) = revoke(
        &phone,
        &mac.id,
        json!({"proof": revoke_proof(&kp, &phone.id, &mac.id)}),
    )
    .await;
    assert_eq!(
        (s, v["removed"].clone()),
        (StatusCode::OK, json!(true)),
        "{v}"
    );
    // Revoked device: its key no longer approves, and it no longer sees requests early.
    let (s, v) = h
        .decide(
            &mac,
            &other,
            "approve",
            Some(proof(&km, &im, &other, HASH, "approve")),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_main_device")),
        "{v}"
    );
    let (s, _) = revoke(
        &mac,
        &phone.id,
        json!({"proof": revoke_proof(&km, &mac.id, &phone.id)}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a revoked device can't revoke back"
    );
    assert!(h
        .notices(&phone)
        .await
        .iter()
        .any(|n| n == "MacBook Air can no longer approve requests (revoked on iPhone)"));
    assert!(h
        .audit()
        .iter()
        .any(|e| e["event"] == "key_revoked" && e["device"] == mac.id.as_str()));

    // A denied enrollment leaves no key behind.
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals/keys",
            Some(&mac.token),
            Some(json!({"publicKey": pkm, "label": "Touch ID on MacBook Air",
                          "proof": key_proof(&km, &mac.id, &pkm)})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let again = v["approvalId"].as_str().unwrap().to_owned();
    let (_, item) = h
        .call(
            "GET",
            &format!("/v1/approvals/{again}"),
            Some(&phone.token),
            None,
        )
        .await;
    let (s, _) = h
        .call(
            "POST",
            &format!("/v1/approvals/{again}/decide"),
            Some(&phone.token),
            Some(json!({"decision": "deny", "payloadHash": item["payloadHash"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, st) = h
        .call("GET", "/v1/approvals/settings", Some(&mac.token), None)
        .await;
    let me = st["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == mac.id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (me["hasBiometricKey"].clone(), me.get("pendingApprovalId")),
        (json!(false), None),
        "{me}"
    );
}

impl H {
    /// `GET /v1/approvals?since=…&wait=…` as `dev`, timed.
    async fn list_poll(&self, dev: &Dev, since: &str, wait: u64) -> (Value, Duration) {
        let t = std::time::Instant::now();
        let (s, v) = self
            .call(
                "GET",
                &format!("/v1/approvals?status=pending&since={since}&wait={wait}"),
                Some(&dev.token),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        (v, t.elapsed())
    }
}

#[tokio::test]
async fn the_pending_list_long_polls_until_it_changes() {
    let h = harness(|c| c.approvals.min_ttl = Duration::from_secs(1)).await;
    let phone = h.device("iPhone");

    // Without `since` it answers at once, with a cursor; an old cursor answers at once too.
    let (v, took) = h.list_poll(&phone, "", 5).await;
    assert!(took < Duration::from_secs(1), "{took:?}");
    let empty = v["cursor"].as_str().unwrap().to_owned();
    assert_eq!(v["approvals"], json!([]));
    let (_, took) = h.list_poll(&phone, "0.0", 5).await;
    assert!(took < Duration::from_secs(1), "{took:?}");

    // Nothing happens: it holds until `wait`, and says the same cursor.
    let (v, took) = h.list_poll(&phone, &empty, 1).await;
    assert!(took >= Duration::from_millis(900), "{took:?}");
    assert_eq!(v["cursor"], empty.as_str());

    // A new request wakes a waiting poll.
    let poll = |since: String| {
        let (http, url, token) = (
            h.http.clone(),
            h.url(&format!(
                "/v1/approvals?status=pending&since={since}&wait=20"
            )),
            phone.token.clone(),
        );
        tokio::spawn(async move {
            let t = std::time::Instant::now();
            let v: Value = http
                .get(url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            (v, t.elapsed())
        })
    };
    let waiter = poll(empty.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let id = h.ask("agent.permission").await;
    let (v, took) = waiter.await.unwrap();
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(v["approvals"][0]["id"], id.as_str());
    let one = v["cursor"].as_str().unwrap().to_owned();
    assert_ne!(one, empty);

    // So does a decision.
    let waiter = poll(one.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (s, _) = h.decide(&phone, &id, "deny", None).await;
    assert_eq!(s, StatusCode::OK);
    let (v, took) = waiter.await.unwrap();
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(v["approvals"], json!([]));

    // And an expiry, which changes nothing but the clock.
    let mut b = H::req_body("agent.permission");
    b["ttlSecs"] = json!(1);
    h.ask_with("", b).await;
    let (v, _) = h.list_poll(&phone, "", 0).await;
    assert_eq!(v["approvals"].as_array().unwrap().len(), 1);
    let (v, took) = h.list_poll(&phone, v["cursor"].as_str().unwrap(), 20).await;
    assert!(
        took < Duration::from_secs(4),
        "the poll ends at the expiry: {took:?}"
    );
    assert_eq!(v["approvals"], json!([]));
}

// -- the unlock key ------------------------------------------------------------

/// The biometric key's proof that `unlock` may become `device_id`'s unlock key.
fn unlock_proof(bio: &SigningKey, device_id: &str, pk_b64: &str) -> Value {
    let sig: Signature =
        bio.sign(format!("secretary-unlock-key-v1\n{device_id}\n{pk_b64}").as_bytes());
    json!({"signature": STANDARD.encode(sig.to_der().as_bytes())})
}

impl H {
    /// Enroll `unlock` as `dev`'s unlock key, proved by `bio` (its biometric key).
    async fn enroll_unlock(
        &self,
        dev: &Dev,
        bio: &SigningKey,
        unlock: &SigningKey,
    ) -> (StatusCode, Value) {
        let pk = pk_b64(unlock);
        self.call(
            "POST",
            "/v1/approvals/unlock-key",
            Some(&dev.token),
            Some(json!({"publicKey": pk, "proof": unlock_proof(bio, &dev.id, &pk)})),
        )
        .await
    }

    /// `dev`'s entry in the settings' device list.
    async fn device_settings(&self, dev: &Dev) -> Value {
        let (_, v) = self
            .call("GET", "/v1/approvals/settings", Some(&dev.token), None)
            .await;
        v["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == dev.id.as_str())
            .unwrap()
            .clone()
    }

    fn audit_named(&self, event: &str) -> Vec<Value> {
        self.audit()
            .into_iter()
            .filter(|e| e["event"] == event)
            .collect()
    }
}

fn refused(r: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(
        (r.0, r.1["error"]["code"].as_str()),
        (status, Some(code)),
        "{}",
        r.1
    );
}

#[tokio::test]
async fn an_unlock_key_is_enrolled_with_face_id_and_approves_a_root_command() {
    let h = harness(|_| {}).await;
    let a = h.device("iPhone");
    let (ka, ua, other) = (key(1), key(11), key(12));

    // Only a device with a biometric key, which proves the new key.
    refused(
        &h.enroll_unlock(&a, &ka, &ua).await,
        StatusCode::FORBIDDEN,
        "biometric_key_required",
    );
    let (s, v) = h.register_key(&a, &ka).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pua = pk_b64(&ua);
    for (why, body) in [
        ("no proof", json!({"publicKey": pua})),
        (
            "signed by the unlock key itself",
            json!({"publicKey": pua, "proof": unlock_proof(&ua, &a.id, &pua)}),
        ),
        (
            "signed by another key",
            json!({"publicKey": pua, "proof": unlock_proof(&other, &a.id, &pua)}),
        ),
        (
            "over another public key",
            json!({"publicKey": pua, "proof": unlock_proof(&ka, &a.id, &pk_b64(&other))}),
        ),
        (
            "the biometric key's registration proof",
            json!({"publicKey": pua, "proof": key_proof(&ka, &a.id, &pua)}),
        ),
    ] {
        let r = h
            .call(
                "POST",
                "/v1/approvals/unlock-key",
                Some(&a.token),
                Some(body),
            )
            .await;
        assert_eq!(
            (r.0, r.1["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("proof_invalid")),
            "{why}: {}",
            r.1
        );
    }
    assert_eq!(h.device_settings(&a).await["hasUnlockKey"], false);
    let (s, v) = h.enroll_unlock(&a, &ka, &ua).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let uid = v["keyId"].as_str().unwrap().to_owned();
    assert_eq!(h.device_settings(&a).await["hasUnlockKey"], true);
    assert_eq!(h.audit_named("unlock_key.enrolled").len(), 1);

    // A root command, approved with the unlock key: no Face ID at signing time, and every record says so.
    let id = h.ask("root.exec").await;
    let (s, v) = h
        .decide(
            &a,
            &id,
            "approve",
            Some(proof(&ua, &uid, &id, HASH, "approve")),
        )
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"status": "approved"})));
    let decided: Vec<Value> = h
        .audit_named("decided")
        .into_iter()
        .filter(|e| e["id"] == id.as_str())
        .collect();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["verifiedBy"], "unlock_key");
    assert_eq!(decided[0]["biometric"], false);
    assert_eq!(
        decided[0]["via"],
        "unlock key (notification/card) on iPhone"
    );
    let (_, item) = h
        .call("GET", &format!("/v1/approvals/{id}"), Some(&a.token), None)
        .await;
    assert_eq!(item["verifiedBy"], "unlock_key");
    assert_eq!(item["biometric"], false);
    assert_eq!(
        item["decidedVia"],
        "unlock key (notification/card) on iPhone"
    );
    // The grant says the owner was verified (what the root verifier needs) and how.
    let (_, w) = h.wait(&id, 0).await;
    let g = h.verify_grant(w["grant"].as_str().unwrap()).await;
    assert_eq!(
        (g["verifiedBy"].clone(), g["biometric"].clone()),
        (json!("unlock_key"), json!(true))
    );

    // Repeated (iOS retries the action): 200, one "decided" line, still one grant. The opposite answer: 409.
    let (s, v) = h
        .decide(
            &a,
            &id,
            "approve",
            Some(proof(&ua, &uid, &id, HASH, "approve")),
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "approved", "repeated": true})
        )
    );
    assert_eq!(
        h.audit_events(&id)
            .iter()
            .filter(|e| *e == "decided")
            .count(),
        1
    );
    let (_, w2) = h.wait(&id, 0).await;
    assert_eq!(w["grant"], w2["grant"]);
    refused(
        &h.decide(&a, &id, "deny", None).await,
        StatusCode::CONFLICT,
        "approval_decided",
    );

    // A replacement unlock key takes over; the old one no longer verifies.
    let ua2 = key(13);
    let (s, v) = h.enroll_unlock(&a, &ka, &ua2).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id2 = h.ask("root.exec").await;
    refused(
        &h.decide(
            &a,
            &id2,
            "approve",
            Some(proof(&ua, &uid, &id2, HASH, "approve")),
        )
        .await,
        StatusCode::FORBIDDEN,
        "biometric_required",
    );
    let uid2 = v["keyId"].as_str().unwrap();
    assert_eq!(
        h.decide(
            &a,
            &id2,
            "approve",
            Some(proof(&ua2, uid2, &id2, HASH, "approve"))
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn another_devices_keys_never_verify_this_devices_decision() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let (ka, kb, ua) = (key(1), key(2), key(11));
    let (_, v) = h.register_key(&a, &ka).await;
    let ia = v["keyId"].as_str().unwrap().to_owned();
    h.enroll(&b, &kb, &a, &ka, &ia).await;
    let (_, v) = h.enroll_unlock(&a, &ka, &ua).await;
    let uid = v["keyId"].as_str().unwrap().to_owned();

    // The iPad answers with the iPhone's unlock key, or the iPhone's biometric key: refused, nothing decided.
    let id = h.ask("root.exec").await;
    for (why, sk, kid) in [
        ("the iPhone's unlock key", &ua, &uid),
        ("the iPhone's biometric key", &ka, &ia),
    ] {
        let r = h
            .decide(
                &b,
                &id,
                "approve",
                Some(proof(sk, kid, &id, HASH, "approve")),
            )
            .await;
        assert_eq!(
            (r.0, r.1["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("biometric_required")),
            "{why}: {}",
            r.1
        );
    }
    assert!(!h.audit_events(&id).contains(&"decided".to_owned()));
    // An unlock key can't be enrolled with another device's biometric key either.
    let ub = key(21);
    refused(
        &h.enroll_unlock(&b, &ka, &ub).await,
        StatusCode::BAD_REQUEST,
        "proof_invalid",
    );
    // The iPhone's own unlock key works for the iPhone.
    assert_eq!(
        h.decide(
            &a,
            &id,
            "approve",
            Some(proof(&ua, &uid, &id, HASH, "approve"))
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_unlock_key_never_adds_an_approver() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("Mac"));
    let (ka, kb, ua) = (key(1), key(2), key(11));
    let (_, v) = h.register_key(&a, &ka).await;
    let ia = v["keyId"].as_str().unwrap().to_owned();
    let (_, v) = h.enroll_unlock(&a, &ka, &ua).await;
    let uid = v["keyId"].as_str().unwrap().to_owned();
    let (_, v) = h.register_key(&b, &kb).await;
    let enroll = v["approvalId"].as_str().unwrap().to_owned();
    let (_, item) = h
        .call(
            "GET",
            &format!("/v1/approvals/{enroll}"),
            Some(&a.token),
            None,
        )
        .await;
    let hash = item["payloadHash"].as_str().unwrap().to_owned();
    let answer = |sk: &SigningKey, kid: &str| json!({"decision": "approve", "payloadHash": hash, "proof": proof(sk, kid, &enroll, &hash, "approve")});
    let path = format!("/v1/approvals/{enroll}/decide");
    let r = h
        .call("POST", &path, Some(&a.token), Some(answer(&ua, &uid)))
        .await;
    refused(&r, StatusCode::FORBIDDEN, "biometric_required");
    assert_eq!(h.device_settings(&b).await["hasBiometricKey"], false);
    let r = h
        .call("POST", &path, Some(&a.token), Some(answer(&ka, &ia)))
        .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    assert_eq!(h.device_settings(&b).await["hasBiometricKey"], true);
}

#[tokio::test]
async fn removing_the_unlock_key_or_the_biometric_key_ends_it() {
    let h = harness(|_| {}).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let (ka, kb, ua) = (key(1), key(2), key(11));
    let (_, v) = h.register_key(&a, &ka).await;
    let ia = v["keyId"].as_str().unwrap().to_owned();
    h.enroll(&b, &kb, &a, &ka, &ia).await;
    let approve_with_unlock = |uid: String| {
        let h = &h;
        let (a, ua) = (&a, &ua);
        async move {
            let id = h.ask("root.exec").await;
            h.decide(
                a,
                &id,
                "approve",
                Some(proof(ua, &uid, &id, HASH, "approve")),
            )
            .await
        }
    };

    // DELETE: gone on the server, audited; it approves nothing any more.
    let (_, v) = h.enroll_unlock(&a, &ka, &ua).await;
    let uid = v["keyId"].as_str().unwrap().to_owned();
    let r = h
        .call("DELETE", "/v1/approvals/unlock-key", Some(&a.token), None)
        .await;
    assert_eq!(r, (StatusCode::OK, json!({"removed": true})));
    assert_eq!(
        h.call("DELETE", "/v1/approvals/unlock-key", Some(&a.token), None)
            .await
            .1,
        json!({"removed": false})
    );
    assert_eq!(h.audit_named("unlock_key.removed").len(), 1);
    refused(
        &approve_with_unlock(uid.clone()).await,
        StatusCode::FORBIDDEN,
        "biometric_required",
    );

    // Another device revokes the iPhone's biometric key: its unlock key goes with it.
    h.enroll_unlock(&a, &ka, &ua).await;
    let r = h
        .call(
            "DELETE",
            &format!("/v1/approvals/keys/{}", a.id),
            Some(&b.token),
            Some(json!({"proof": revoke_proof(&kb, &b.id, &a.id)})),
        )
        .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    assert_eq!(h.device_settings(&a).await["hasUnlockKey"], false);
    refused(
        &approve_with_unlock(uid.clone()).await,
        StatusCode::FORBIDDEN,
        "biometric_required",
    );

    // The iPad removes its own biometric key: same.
    let ub = key(21);
    let (s, v) = h.enroll_unlock(&b, &kb, &ub).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        h.call("DELETE", "/v1/approvals/keys", Some(&b.token), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(h.device_settings(&b).await["hasUnlockKey"], false);
    assert_eq!(h.audit_named("unlock_key.removed").len(), 3);
    // The unlock-key routes are a device's, never the service token's.
    assert_eq!(
        h.call("DELETE", "/v1/approvals/unlock-key", Some(&h.svc), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

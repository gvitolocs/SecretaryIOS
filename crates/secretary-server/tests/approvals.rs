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

    async fn register_key(
        &self,
        dev: &Dev,
        sk: &SigningKey,
        password: &str,
    ) -> (StatusCode, Value) {
        let pk = STANDARD.encode(sk.verifying_key().to_encoded_point(false).as_bytes());
        self.call(
            "POST",
            "/v1/approvals/keys",
            Some(&dev.token),
            Some(json!({"publicKey": pk, "label": "Face ID on iPhone", "password": password})),
        )
        .await
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
            Some(json!({"publicKey": "x", "password": PASSWORD})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
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
async fn a_second_decision_is_refused_and_the_grant_is_stable() {
    let h = harness(|c| c.approvals.min_ttl = Duration::from_secs(1)).await;
    let (a, b) = (h.device("iPhone"), h.device("iPad"));
    let id = h.ask("agent.permission").await;
    assert_eq!(h.decide(&a, &id, "approve", None).await.0, StatusCode::OK);
    for (dev, decision) in [(&a, "approve"), (&b, "approve"), (&b, "deny")] {
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

    // Registering a key takes the owner's password, not just a device token.
    let (s, v) = h.register_key(&a, &ka, "wrong password").await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("invalid_credentials")),
        "{v}"
    );
    let (_, v) = h
        .call("GET", "/v1/approvals/settings", Some(&a.token), None)
        .await;
    assert!(v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["hasBiometricKey"] == false));
    let (s, v) = h
        .call(
            "POST",
            "/v1/approvals/keys",
            Some(&a.token),
            Some(json!({"publicKey": "AAAA", "password": PASSWORD})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request")),
        "{v}"
    );
    let (s, v) = h.register_key(&a, &ka, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (ia, ib) = {
        let ia = v["keyId"].as_str().unwrap().to_owned();
        assert_eq!(ia.len(), 8);
        let (s, v) = h.register_key(&b, &kb, PASSWORD).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        (ia, v["keyId"].as_str().unwrap().to_owned())
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
    let keys_file = std::fs::read_to_string(h.data.join("approval-keys.json")).unwrap();
    assert!(!keys_file.contains(PASSWORD));

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

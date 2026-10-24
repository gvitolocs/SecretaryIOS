//! Host tests against a mock relay (`tiny_http`) over a test-only plain-HTTP
//! transport. All codes and tokens are synthetic.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use lucina_phone_core::proto::{Incoming, OtpSource, CODE_TTL_MS, ORIGIN};
use serde_json::{json, Value};

use crate::bridge::{now_ms, Bridge, Forward, Probe, DEDUP_MS, MAX_QUEUE, QUEUE_FILE, STATE_FILE};
use crate::net::{classify, Outcome, Transport};
use crate::store::{event_id, StoreError, Vault};

const KEY: [u8; 32] = [7; 32];
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTP_SMS: &str = "Your verification code is 731942. Do not share it.";
const OTP_CODE: &str = "731942";
const PLAIN_SMS: &str = "Your parcel 731942 left the depot at 12:30.";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "lucina-android-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn lucina(&self) -> PathBuf {
        self.0.join("lucina")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
struct Hit {
    path: String,
    auth: String,
    body: Value,
}

/// A mock relay: answers each request with the next scripted reply (then
/// `200 {"accepted":true}`) and records what it received.
struct Mock {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    replies: Arc<Mutex<VecDeque<(u16, String)>>>,
    server: Arc<tiny_http::Server>,
}

impl Mock {
    fn new() -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!(
            "http://{}/v1/phone/otp",
            server.server_addr().to_ip().unwrap()
        );
        let hits = Arc::new(Mutex::new(Vec::new()));
        let replies: Arc<Mutex<VecDeque<(u16, String)>>> = Arc::default();
        let (s, h, r) = (server.clone(), hits.clone(), replies.clone());
        thread::spawn(move || {
            for mut req in s.incoming_requests() {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("authorization"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                h.lock().unwrap().push(Hit {
                    path: req.url().to_owned(),
                    auth,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let (status, text) = r
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((200, r#"{"accepted":true}"#.into()));
                let mut resp = tiny_http::Response::from_string(text).with_status_code(status);
                if status == 302 {
                    resp.add_header(
                        tiny_http::Header::from_bytes("Location", "http://127.0.0.1:9/elsewhere")
                            .unwrap(),
                    );
                }
                let _ = req.respond(resp);
            }
        });
        Self {
            base,
            hits,
            replies,
            server,
        }
    }

    fn reply(&self, status: u16, body: &str) {
        self.replies
            .lock()
            .unwrap()
            .push_back((status, body.into()));
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn transport(&self) -> Transport {
        Transport::plain_http_for_tests(self.base.clone())
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// A base URL nothing listens on.
fn dead_base() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/v1/phone/otp")
}

fn paired(dir: &TempDir, transport: Transport) -> Bridge {
    let mut b = Bridge::open_with(&dir.0, &KEY, None, transport).unwrap();
    b.configure(&json!({ "token": TOKEN }).to_string()).unwrap();
    b
}

fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn parse_first_no_network_no_disk() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = paired(&dir, mock.transport());
    let before = files_in(&dir.lucina());
    let now = now_ms();
    let r = b.on_sms_at("Shop", PLAIN_SMS, now as i64, now);
    assert_eq!(r.result, Forward::NotOtp);
    b.configure(r#"{"notifications":true}"#).unwrap();
    let before_notification = files_in(&dir.lucina());
    let r = b.on_notification_at("com.shop", "Shop", PLAIN_SMS, now as i64, now);
    assert_eq!(r.result, Forward::NotOtp);
    assert!(
        mock.hits().is_empty(),
        "no request for a message without a code"
    );
    assert_eq!(
        files_in(&dir.lucina()),
        before_notification,
        "no disk write"
    );
    assert_eq!(before.len(), 1, "only state.bin: {before:?}");
}

#[test]
fn sends_protocol_two_and_dedups_across_sources() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = paired(&dir, mock.transport());
    b.configure(r#"{"notifications":true}"#).unwrap();
    let now = now_ms();
    let r = b.on_sms_at("Bank", OTP_SMS, (now - 1_000) as i64, now);
    assert_eq!(r.result, Forward::Sent);
    let hits = mock.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/v1/phone/otp/incoming");
    assert_eq!(hits[0].auth, format!("Bearer {TOKEN}"));
    let sent: Incoming = serde_json::from_value(hits[0].body.clone()).unwrap();
    assert_eq!(sent.code, OTP_CODE);
    assert_eq!(sent.sender, "Bank");
    assert_eq!(sent.window_id, None, "protocol 2 has no windowId");
    assert_eq!(sent.received_at, Some(now - 1_000));
    assert_eq!(sent.source, OtpSource::Sms);
    assert!(!sent.test && sent.validate().is_ok());

    // The same code as a notification, and as a second SMS: sent once.
    let r = b.on_notification_at(
        "com.samsung.android.messaging",
        "Bank",
        OTP_SMS,
        now as i64,
        now + 500,
    );
    assert_eq!(r.result, Forward::Duplicate);
    let r = b.on_sms_at("Bank", OTP_SMS, now as i64, now + 1_000);
    assert_eq!(r.result, Forward::Duplicate);
    assert_eq!(mock.hits().len(), 1);

    // Dedup survives a restart (fingerprints are in state.bin).
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    let r = b.on_sms_at("Bank", OTP_SMS, now as i64, now + 2_000);
    assert_eq!(r.result, Forward::Duplicate);

    // After the window it is a new code.
    let later = now + DEDUP_MS + 1;
    let r = b.on_sms_at("Bank", OTP_SMS, later as i64, later);
    assert_eq!(r.result, Forward::Sent);
    assert_eq!(mock.hits().len(), 2);
    let st = b.status();
    assert_eq!(st.last_result, Some(Forward::Sent));
    assert_eq!(st.last_forward_at_ms, Some(later));
    assert!(st.last_latency_ms.is_some());

    // Notifications report the source and use the title as sender.
    let r = b.on_notification_at(
        "com.bank",
        "MyBank",
        "Your security code is 99887766",
        later as i64,
        later,
    );
    assert_eq!(r.result, Forward::Sent);
    let n: Incoming = serde_json::from_value(mock.hits()[2].body.clone()).unwrap();
    assert_eq!(
        (n.source, n.sender.as_str(), n.code.as_str()),
        (OtpSource::Notification, "MyBank", "99887766")
    );
}

#[test]
fn status_code_mapping() {
    for (status, body, want, pending) in [
        (409, "{}", Forward::Paused, 0),
        (401, "{}", Forward::Unpaired, 0),
        (410, "{}", Forward::Expired, 0),
        (400, "{}", Forward::Error, 0),
        (404, "", Forward::Error, 0),
        (302, "", Forward::Error, 0),
        (500, "", Forward::Queued, 1),
        (503, "", Forward::Queued, 1),
        (429, "", Forward::Queued, 1),
        (200, r#"{"accepted":false}"#, Forward::Error, 0),
        (
            200,
            r#"{"accepted":true,"duplicate":true}"#,
            Forward::Sent,
            0,
        ),
        (200, "not json", Forward::Error, 0),
    ] {
        let dir = TempDir::new();
        let mock = Mock::new();
        let mut b = paired(&dir, mock.transport());
        mock.reply(status, body);
        let now = now_ms();
        let r = b.on_sms_at("Bank", OTP_SMS, now as i64, now);
        assert_eq!(r.result, want, "{status} {body}");
        assert_eq!(b.status().pending, pending, "{status}");
        assert_eq!(b.status().last_result, Some(want));
        assert_eq!(mock.hits().len(), 1, "{status}: no retry, no redirect");
        assert_eq!(dir.lucina().join(QUEUE_FILE).exists(), pending == 1);
    }
    assert_eq!(classify(200, None), Outcome::Retry);
    assert_eq!(classify(502, Some(b"")), Outcome::Retry);
}

#[test]
fn oversized_response_is_refused() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = paired(&dir, mock.transport());
    mock.reply(
        200,
        &format!(r#"{{"accepted":true,"pad":"{}"}}"#, "x".repeat(5000)),
    );
    let now = now_ms();
    // Unreadable 2xx: queued; the resend carries the same event id.
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Queued
    );
    let report = b.flush_at(now + 1);
    assert_eq!((report.sent, report.pending, report.retry), (1, 0, false));
    let hits = mock.hits();
    assert_eq!(hits[0].body["eventId"], hits[1].body["eventId"]);
}

#[test]
fn production_transport_refuses_plain_http() {
    let dir = TempDir::new();
    let mock = Mock::new();
    assert_eq!(Transport::production().base(), ORIGIN);
    let mut b = paired(&dir, Transport::https_only_for_tests(mock.base.clone()));
    let now = now_ms();
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Queued
    );
    assert!(mock.hits().is_empty());
}

#[test]
fn queue_persists_flushes_and_expires() {
    let dir = TempDir::new();
    let now = now_ms();
    {
        let mut b = paired(&dir, Transport::plain_http_for_tests(dead_base()));
        let r = b.on_sms_at("Bank", OTP_SMS, now as i64, now);
        assert_eq!(r.result, Forward::Queued);
        let r = b.on_sms_at("Shop", "Your login code: Q7Z3K9.", now as i64, now);
        assert_eq!(r.result, Forward::Queued);
        let f = b.flush_at(now + 10);
        assert_eq!((f.sent, f.dropped, f.pending, f.retry), (0, 0, 2, true));
    }
    // A new process with the same key sees the queue.
    let mock = Mock::new();
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    assert_eq!(b.status().pending, 2);
    let f = b.flush_at(now + 20);
    assert_eq!((f.sent, f.dropped, f.pending, f.retry), (2, 0, 0, false));
    let codes: Vec<_> = mock.hits().iter().map(|h| h.body["code"].clone()).collect();
    assert_eq!(codes, [json!(OTP_CODE), json!("Q7Z3K9")]);
    assert!(!dir.lucina().join(QUEUE_FILE).exists());

    // Expired entries are dropped, never sent.
    let mut b = paired(&dir, Transport::plain_http_for_tests(dead_base()));
    assert_eq!(
        b.on_sms_at("Bank", "Your security code is 112233.", now as i64, now)
            .result,
        Forward::Queued
    );
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    let f = b.flush_at(now + CODE_TTL_MS);
    assert_eq!((f.sent, f.dropped, f.pending), (0, 1, 0));
    assert_eq!(mock.hits().len(), 2);
}

#[test]
fn queue_holds_at_most_eight() {
    let dir = TempDir::new();
    let mut b = paired(&dir, Transport::plain_http_for_tests(dead_base()));
    let now = now_ms();
    for i in 0..10u32 {
        let body = format!("Your verification code is {}.", 400_000 + i);
        let at = now + u64::from(i);
        assert_eq!(
            b.on_sms_at("Bank", &body, at as i64, at).result,
            Forward::Queued
        );
    }
    assert_eq!(b.status().pending as usize, MAX_QUEUE);
    let mock = Mock::new();
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    assert_eq!(b.flush_at(now + 20).sent as usize, MAX_QUEUE);
    assert_eq!(
        mock.hits()[0].body["code"],
        "400002",
        "oldest dropped first"
    );
}

#[test]
fn old_codes_expire_without_network() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = paired(&dir, mock.transport());
    let now = now_ms();
    let r = b.on_sms_at("Bank", OTP_SMS, (now - CODE_TTL_MS) as i64, now);
    assert_eq!(r.result, Forward::Expired);
    assert!(mock.hits().is_empty());
}

#[test]
fn switches_and_pairing() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    let now = now_ms();
    assert!(!b.status().paired);
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Unpaired
    );
    b.configure(&json!({ "token": TOKEN, "enabled": false }).to_string())
        .unwrap();
    assert_eq!(
        b.on_sms_at("Bank", "Your security code is 445566.", now as i64, now)
            .result,
        Forward::Disabled
    );
    assert_eq!(
        b.on_notification_at(
            "com.bank",
            "Bank",
            "Your security code is 445566.",
            now as i64,
            now
        )
        .result,
        Forward::Disabled,
        "notifications are off by default"
    );
    b.configure(r#"{"notifications":true}"#).unwrap();
    assert_eq!(
        b.on_notification_at(
            crate::bridge::OWN_PACKAGE,
            "Lucina",
            OTP_SMS,
            now as i64,
            now
        )
        .result,
        Forward::NotOtp
    );
    assert!(mock.hits().is_empty());

    for bad in [
        r#"{"url":"https://evil.example/v1/phone/otp"}"#,
        r#"{"token":"ABC"}"#,
        r#"{"token":"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
        r#"{"enabled":"yes"}"#,
        r#"{"windowId":"x"}"#,
        "[]",
    ] {
        assert!(b.configure(bad).is_err(), "{bad}");
    }
    let st = b
        .configure(&json!({ "url": ORIGIN, "enabled": true }).to_string())
        .unwrap();
    assert!(st.enabled && st.notifications && st.paired);

    // Settings survive a restart.
    let b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    let st = b.status();
    assert!(st.enabled && st.notifications && st.paired);
}

#[test]
fn turning_a_source_off_drops_its_queue() {
    let dir = TempDir::new();
    let mut b = paired(&dir, Transport::plain_http_for_tests(dead_base()));
    let now = now_ms();
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Queued
    );
    assert_eq!(b.configure(r#"{"enabled":false}"#).unwrap().pending, 0);
}

#[test]
fn legacy_token_import() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let b = Bridge::open_with(&dir.0, &KEY, Some("not-a-token"), mock.transport()).unwrap();
    assert!(!b.status().paired);
    drop(b);
    let b = Bridge::open_with(&dir.0, &KEY, Some(TOKEN), mock.transport()).unwrap();
    assert!(b.status().paired);
    drop(b);
    // Imported once: a stored token wins over a later legacy value.
    let other = "f".repeat(64);
    let mut b = Bridge::open_with(&dir.0, &KEY, Some(&other), mock.transport()).unwrap();
    let now = now_ms();
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Sent
    );
    assert_eq!(mock.hits()[0].auth, format!("Bearer {TOKEN}"));
}

#[test]
fn secrets_never_in_plaintext_and_files_private() {
    let dir = TempDir::new();
    let mut b = paired(&dir, Transport::plain_http_for_tests(dead_base()));
    let now = now_ms();
    assert_eq!(
        b.on_sms_at("Bank", OTP_SMS, now as i64, now).result,
        Forward::Queued
    );
    let files = files_in(&dir.lucina());
    let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, [QUEUE_FILE, STATE_FILE]);
    for (name, bytes) in &files {
        assert!(bytes.starts_with(b"LUC1"), "{name}");
        for secret in [TOKEN, &TOKEN[..16], OTP_CODE, "Bank", "verification"] {
            assert!(
                !contains(bytes, secret.as_bytes()),
                "{name} leaks {secret:?}"
            );
        }
        let mode = fs::metadata(dir.lucina().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
    let mode = fs::metadata(dir.lucina()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn encryption_round_trip_and_tamper_detection() {
    let dir = TempDir::new();
    let vault = Vault::open(&dir.lucina(), &KEY).unwrap();
    vault.write("x.bin", b"{\"hello\":1}").unwrap();
    assert_eq!(&vault.read("x.bin").unwrap().unwrap()[..], b"{\"hello\":1}");
    assert!(vault.read("missing.bin").unwrap().is_none());
    let a = vault.seal("x.bin", b"same").unwrap();
    let b = vault.seal("x.bin", b"same").unwrap();
    assert_ne!(a, b, "fresh nonce per write");

    let sealed = fs::read(dir.lucina().join("x.bin")).unwrap();
    for i in [0, 5, 20, sealed.len() - 1] {
        let mut t = sealed.clone();
        t[i] ^= 1;
        assert!(
            matches!(vault.unseal("x.bin", &t), Err(StoreError::Tampered)),
            "byte {i}"
        );
    }
    assert!(matches!(
        vault.unseal("x.bin", &sealed[..sealed.len() - 1]),
        Err(StoreError::Tampered)
    ));
    assert!(
        matches!(vault.unseal("y.bin", &sealed), Err(StoreError::Tampered)),
        "file name is AAD"
    );
    let other = Vault::open(&dir.lucina(), &[8; 32]).unwrap();
    assert!(matches!(other.read("x.bin"), Err(StoreError::Tampered)));
    assert!(Vault::open(&dir.lucina(), &[1; 31]).is_err());
    assert_ne!(vault.fingerprint("123456"), other.fingerprint("123456"));

    // A tampered state.bin starts over instead of failing every call.
    drop(paired(&dir, Transport::plain_http_for_tests(dead_base())));
    let path = dir.lucina().join(STATE_FILE);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x80;
    fs::write(&path, bytes).unwrap();
    let b = Bridge::open_with(
        &dir.0,
        &KEY,
        None,
        Transport::plain_http_for_tests(dead_base()),
    )
    .unwrap();
    assert!(!b.status().paired);
    assert!(Bridge::open(&dir.0, &[0; 16], None).is_err());
}

#[test]
fn status_and_self_test() {
    let dir = TempDir::new();
    let mock = Mock::new();
    let mut b = Bridge::open_with(&dir.0, &KEY, None, mock.transport()).unwrap();
    let st = serde_json::to_value(b.status()).unwrap();
    let mut keys: Vec<_> = st.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "enabled",
            "lastForwardAtMs",
            "lastLatencyMs",
            "lastResult",
            "notifications",
            "paired",
            "pending",
            "protocol",
            "selfTest",
            "version"
        ]
    );
    assert_eq!(st["protocol"], 2);
    assert_eq!(st["selfTest"], true);
    assert_eq!(st["lastResult"], Value::Null);

    let t = b.self_test(false);
    assert!(t.parse);
    assert_eq!(t.result, Probe::Skipped);
    assert_eq!(b.self_test(true).result, Probe::Unpaired);
    assert!(mock.hits().is_empty());

    b.configure(&json!({ "token": TOKEN }).to_string()).unwrap();
    mock.reply(200, r#"{"accepted":true,"test":true}"#);
    let t = b.self_test(true);
    assert_eq!(t.result, Probe::Sent);
    let hit = &mock.hits()[0];
    assert_eq!(hit.body["test"], true);
    let event: Incoming = serde_json::from_value(hit.body.clone()).unwrap();
    assert!(event.validate().is_ok());
    // A relay that stored it as a real code is reported as an error.
    assert_eq!(b.self_test(true).result, Probe::Error);
    mock.reply(409, "{}");
    assert_eq!(b.self_test(true).result, Probe::Paused);
    assert_eq!(b.status().last_result, None, "self-tests are not forwards");

    let mut b = paired(
        &TempDir::new(),
        Transport::plain_http_for_tests(dead_base()),
    );
    assert_eq!(b.self_test(true).result, Probe::Offline);

    let r = serde_json::to_value(crate::Report {
        result: Forward::NotOtp,
        ms: 3,
    })
    .unwrap();
    assert_eq!(r, json!({"result": "not_otp", "ms": 3}));
    let t = serde_json::to_value(b.self_test(false)).unwrap();
    assert_eq!(t["result"], "skipped");
    let f = serde_json::to_value(b.flush()).unwrap();
    assert_eq!(
        f,
        json!({"sent": 0, "dropped": 0, "pending": 0, "retry": false})
    );
}

#[test]
fn event_ids_are_uuids() {
    let a = event_id().unwrap();
    assert_eq!(a.len(), 36);
    assert_eq!(&a[14..15], "4");
    assert!(lucina_phone_core::proto::valid_event_id(&a));
    assert_ne!(a, event_id().unwrap());
}

#[test]
fn jni_init_is_first_call_wins() {
    let dir = TempDir::new();
    assert!(crate::jni_api::init(dir.0.to_str().unwrap(), &[1; 3], None).is_none());
    let st: Value = serde_json::from_str(
        &crate::jni_api::init(dir.0.to_str().unwrap(), &KEY, Some(TOKEN)).unwrap(),
    )
    .unwrap();
    assert_eq!(st["paired"], true);
    let other = TempDir::new();
    let again: Value =
        serde_json::from_str(&crate::jni_api::init(other.0.to_str().unwrap(), &KEY, None).unwrap())
            .unwrap();
    assert_eq!(
        again["paired"], true,
        "later calls return the first bridge's status"
    );
    assert!(!other.lucina().exists());
}

/// The client-only provider against Let's Encrypt's public test sites (RSA
/// ISRG Root X1 and ECDSA P-384 ISRG Root X2 chains). Needs the internet:
/// `cargo test -p lucina-android -- --ignored`.
#[test]
#[ignore = "network"]
fn tls_provider_verifies_lets_encrypt_chains() {
    let t = Transport::https_only_for_tests(ORIGIN.to_owned());
    for url in [
        "https://valid-isrgrootx1.letsencrypt.org/",
        "https://valid-isrgrootx2.letsencrypt.org/",
    ] {
        let r = t.agent().get(url).call();
        assert!(r.is_ok(), "{url}: {r:?}");
    }
    for url in [
        "https://expired-isrgrootx2.letsencrypt.org/",
        "https://revoked-isrgrootx1.letsencrypt.org/",
    ] {
        let r = t.agent().get(url).call();
        eprintln!("{url}: {}", if r.is_ok() { "accepted" } else { "refused" });
    }
    assert!(t
        .agent()
        .get("https://expired-isrgrootx2.letsencrypt.org/")
        .call()
        .is_err());
}

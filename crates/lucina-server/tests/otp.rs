//! OTP requests in `/api/secretary/chat` against a mock relay: answered
//! before the model, never stored, refused on Lucina MCP. Codes and tokens
//! here are synthetic.

mod common;

use common::{harness, script};
use lucina_phone_core::reply;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A synthetic owner token (64 hex).
const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CODE: &str = "482917";

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}

fn auth() -> wiremock::matchers::HeaderExactMatcher {
    header("authorization", format!("Bearer {OWNER}").as_str())
}

/// The fake DeepSeek answers anything; OTP turns must never ask it.
fn any_script() -> lucina_server::fake::Script {
    script(
        json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"model\"}"}]}),
    )
}

/// A harness whose relay is `relay` (None: no token file configured).
async fn otp_harness(relay: Option<String>) -> common::H {
    harness(any_script(), move |c| {
        if let Some(url) = relay {
            let file = c.data_dir.join("otp-owner.token");
            std::fs::write(&file, format!("{OWNER}\n")).unwrap();
            c.otp.owner_token_file = Some(file);
            c.otp.relay_url = url;
        }
    })
    .await
}

async fn chat(h: &common::H, body: Value) -> Value {
    let (s, b) = h.post("/chat", body).await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

async fn ask(h: &common::H, message: &str) -> Value {
    chat(
        h,
        json!({"message": message, "sessionId": "test", "channel": "ios"}),
    )
    .await
}

/// Nothing of the turn reached the model, the timeline or any file.
fn assert_untouched(h: &common::H) {
    assert!(h.fake.requests().is_empty(), "the model was called");
    assert!(h.timeline_lines().is_empty(), "the timeline was written");
    assert!(
        h.state
            .client_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty(),
        "the reply is in the replay cache"
    );
    fn scan(dir: &std::path::Path) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                assert!(!text.contains(CODE), "{} holds the code", p.display());
            }
        }
    }
    scan(h.dir.path());
}

async fn mount_status(relay: &MockServer, armed: bool, continuous: bool) {
    Mock::given(method("GET"))
        .and(path("/status"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": "1.0.0", "transport": "internet", "armed": armed, "continuous": continuous, "protocol": 2
        })))
        .mount(relay)
        .await;
}

#[tokio::test]
async fn take_delivers_once_and_stores_nothing() {
    let relay = MockServer::start().await;
    let expires = now_ms() + 100_000;
    Mock::given(method("GET"))
        .and(path("/otp"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sender": "<Bank>", "code": CODE, "receivedAt": now_ms() - 20_000, "expiresAt": expires, "source": "sms"
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&relay)
        .await;
    Mock::given(method("GET"))
        .and(path("/otp"))
        .and(auth())
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .mount(&relay)
        .await;
    mount_status(&relay, true, true).await;
    let h = otp_harness(Some(relay.uri())).await;
    let body =
        json!({"message": "/otp", "sessionId": "test", "clientMessageId": "c-1", "channel": "ios"});
    let d = chat(&h, body.clone()).await;
    assert_eq!(
        d,
        json!({
            "reply": format!("OTP from Bank: {CODE}\nCheck it matches the service you requested. Delivered once; expires shortly."),
            "action": "private_otp",
            "sensitive": true,
            "expiresAt": expires,
            "sessionId": "secretary_test",
            "clientMessageId": "c-1",
            "job": null,
            "userItemId": null,
            "lucinaItemId": null,
        })
    );
    // A retry is not replayed from a cache: the relay already gave the code away.
    let again = chat(&h, body).await;
    assert_eq!(again["reply"], reply::waiting(true));
    assert!(again.get("duplicate").is_none() && again.get("expiresAt").is_none());
    // Free text means "take" too.
    let d = ask(&h, "dimmi il codice di verifica").await;
    assert_eq!(
        (d["action"].as_str(), d["reply"].as_str()),
        (Some("private_otp"), Some(reply::waiting(true)))
    );
    assert_untouched(&h);
}

#[tokio::test]
async fn waiting_arms_a_disarmed_relay() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/otp"))
        .and(auth())
        .respond_with(ResponseTemplate::new(204))
        .mount(&relay)
        .await;
    mount_status(&relay, false, false).await;
    Mock::given(method("POST"))
        .and(path("/arm"))
        .and(auth())
        .and(body_json(json!({"seconds": 300})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"armed": true, "expiresAt": null, "continuous": true})),
        )
        .expect(1)
        .mount(&relay)
        .await;
    let h = otp_harness(Some(relay.uri())).await;
    let d = ask(&h, "what's my otp?").await;
    assert_eq!(d["reply"], reply::waiting(true));
    assert_eq!(d["sensitive"], true);
    assert_untouched(&h);
}

#[tokio::test]
async fn waiting_while_armed_does_not_rearm() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/otp"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&relay)
        .await;
    mount_status(&relay, true, false).await;
    Mock::given(method("POST"))
        .and(path("/arm"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&relay)
        .await;
    let h = otp_harness(Some(relay.uri())).await;
    assert_eq!(ask(&h, "/otp").await["reply"], reply::waiting(false));
}

#[tokio::test]
async fn status_arm_and_disarm() {
    let relay = MockServer::start().await;
    mount_status(&relay, true, true).await;
    Mock::given(method("POST"))
        .and(path("/arm"))
        .and(auth())
        .and(body_json(json!({"seconds": 300})))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"armed": true, "windowId": "0123456789abcdef0123", "expiresAt": now_ms() + 300_000, "continuous": false}),
        ))
        .expect(1)
        .mount(&relay)
        .await;
    Mock::given(method("POST"))
        .and(path("/disarm"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"disarmed": true})))
        .expect(1)
        .mount(&relay)
        .await;
    let h = otp_harness(Some(relay.uri())).await;
    assert_eq!(ask(&h, "/otp status").await["reply"], reply::status(true));
    assert_eq!(ask(&h, "/otp arm").await["reply"], reply::armed(false));
    let d = ask(&h, "/OTP stop").await;
    assert_eq!(d["reply"], reply::DISARMED);
    assert_eq!(d["action"], "private_otp");
    assert!(d.get("expiresAt").is_none());
    assert_untouched(&h);
}

#[tokio::test]
async fn unavailable_relay() {
    // No token file configured: no request at all.
    let relay = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&relay)
        .await;
    let h = otp_harness(None).await;
    let d = ask(&h, "/otp").await;
    assert_eq!(d["reply"], reply::UNAVAILABLE);
    assert_eq!(d["action"], "private_otp");
    assert_eq!(d["sensitive"], true);
    assert_untouched(&h);

    // Unreachable.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let h = otp_harness(Some(format!("http://127.0.0.1:{port}"))).await;
    assert_eq!(ask(&h, "/otp status").await["reply"], reply::UNAVAILABLE);

    // 5xx, a redirect (never followed), an expired code, a wrong token.
    let bad = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"armed": true})))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&bad)
        .await;
    Mock::given(method("POST"))
        .and(path("/arm"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/arm", elsewhere.uri()).as_str()),
        )
        .mount(&bad)
        .await;
    Mock::given(method("GET"))
        .and(path("/otp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sender": "Bank", "code": CODE, "receivedAt": 1, "expiresAt": 2
        })))
        .mount(&bad)
        .await;
    let h = otp_harness(Some(bad.uri())).await;
    for m in ["/otp status", "/otp arm", "/otp"] {
        let d = ask(&h, m).await;
        assert_eq!(d["reply"], reply::UNAVAILABLE, "{m}");
        assert!(d.get("expiresAt").is_none(), "{m}");
    }
    assert_untouched(&h);

    let strict = MockServer::start().await;
    mount_status(&strict, true, true).await; // only answers the right token
    let h = harness(any_script(), |c| {
        let file = c.data_dir.join("otp-owner.token");
        std::fs::write(&file, "f".repeat(64)).unwrap();
        c.otp.owner_token_file = Some(file);
        c.otp.relay_url = strict.uri();
    })
    .await;
    assert_eq!(ask(&h, "/otp status").await["reply"], reply::UNAVAILABLE);
}

#[tokio::test]
async fn mcp_callers_are_refused() {
    let relay = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&relay)
        .await;
    let h = otp_harness(Some(relay.uri())).await;
    let d = chat(
        &h,
        json!({"message": "[via Lucina MCP, from claude] /otp", "sessionId": "test", "channel": "web"}),
    )
    .await;
    assert_eq!(
        d,
        json!({"reply": reply::NOT_PRIVATE, "action": "reply", "sessionId": "secretary_test", "job": null})
    );
    let d = chat(
        &h,
        json!({"message": "what is my otp", "sessionId": "mcp-chatgpt", "channel": "web"}),
    )
    .await;
    assert_eq!(d["reply"], reply::NOT_PRIVATE);
    assert_untouched(&h);
}

#[tokio::test]
async fn other_messages_still_reach_the_model_without_codes() {
    let h = otp_harness(None).await;
    // Not a request for a code: the model answers, but the code in it is masked.
    let d = ask(&h, "ricordati: il codice di verifica 482917 era sbagliato").await;
    assert_ne!(d["action"], "private_otp");
    let reqs = h.fake.requests();
    assert!(!reqs.is_empty(), "the model answers ordinary turns");
    for r in &reqs {
        assert!(!r.to_string().contains(CODE), "the model saw the code: {r}");
    }
    assert!(reqs.iter().any(|r| r
        .to_string()
        .contains("codice di verifica [otp] era sbagliato")));
}

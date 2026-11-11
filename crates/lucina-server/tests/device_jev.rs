//! Jev's state carries `device` and `device_connected` for a turn from the DSi or the Mac, and
//! neither for any other. Its own test binary: everyday turns use the process-wide Jev client,
//! which reads its settings from the environment once.

mod common;

use common::{harness, script};
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn device_jev_state_has_the_device_facts_only_for_known_devices() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": {
                "intent": {"type": "choice", "choice": "chat", "confidence": 0.95, "probabilities": {"chat": 0.95, "unknown": 0.05}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.9, "probabilities": {"all": 0.9, "today": 0.1}},
                "needs_memory": {"type": "noul", "noul": 0.1},
                "needs_tools": {"type": "noul", "noul": 0.1},
            }
        })))
        .mount(&server)
        .await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", server.uri()));
    assert!(lucina_server_jev_available());

    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "reply", "content": "Ok."},
        ]})),
        |_| {},
    )
    .await;
    let last_state = || async {
        let seen = server.received_requests().await.unwrap();
        let last: Value = serde_json::from_slice(&seen.last().unwrap().body).unwrap();
        last["state"].clone()
    };
    let say = |channel: &'static str, session: &'static str, text: &'static str| {
        let h = &h;
        async move {
            let (s, b) = h
                .post(
                    "/chat",
                    json!({"message": text, "sessionId": session, "channel": channel}),
                )
                .await;
            assert_eq!(s, 200, "{b}");
        }
    };

    say("dsi", "fortuna-d1", "che tempo fa domani").await;
    let s = last_state().await;
    assert_eq!(s["device"], "nintendo_dsi", "{s}");
    assert_eq!(s["device_connected"], true);

    say("mac", "mac-1", "che tempo fa dopodomani").await;
    let s = last_state().await;
    assert_eq!(s["device"], "mac_app", "{s}");
    assert_eq!(s["device_connected"], true);

    // Not known: neither key.
    say("ios", "i-1", "che tempo fa stasera").await;
    let s = last_state().await;
    assert!(
        s.get("device").is_none() && s.get("device_connected").is_none(),
        "{s}"
    );
}

fn lucina_server_jev_available() -> bool {
    lucina_jev::shared().available()
}

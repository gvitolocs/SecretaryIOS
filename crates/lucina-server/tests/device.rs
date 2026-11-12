//! The channel and device of an app turn: the Nintendo DSi (`dsi`) and the Mac app (`mac`) are
//! recorded as such, and the turn tells the model which device he is talking through.

mod common;

use std::time::Duration;

use common::{harness, script, H};
use reqwest::multipart::Form;
use serde_json::{json, Value};

async fn setup() -> H {
    harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "agent", "content": "Ok, ci sono."},
            {"kind": "reply", "content": "Ok, ci sono."},
        ]})),
        |c| {
            c.share.dir = c.data_dir.join("shares");
            c.share.key_file = Some(c.data_dir.join("share.key"));
            std::fs::write(c.share.key_file.as_ref().unwrap(), "gateway-test-secret\n").unwrap();
        },
    )
    .await
}

fn prompts(h: &H) -> String {
    h.fake
        .requests()
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn device_share_from_fortuna_is_recorded_as_dsi_and_the_turn_knows_the_dsi() {
    let h = setup().await;
    // The firmware installed today says `web`; its source and session id say it is Fortuna.
    let r = h
        .http
        .post(format!("{}/v1/share", h.base))
        .header("x-share-gateway", "gateway-test-secret")
        .multipart(
            Form::new()
                .text("shareId", "share-device-dsi-1")
                .text("channel", "web")
                .text("source", "fortuna-chat")
                .text("sessionId", "fortuna-abc123")
                .text(
                    "text",
                    "Accendi i Anespici e passami Pokemon Diamante su questo DSi",
                )
                .text(
                    "note",
                    "Accendi i Anespici e passami Pokemon Diamante su questo DSi",
                ),
        )
        .send()
        .await
        .unwrap();
    let status = r.status();
    assert!(
        status.is_success(),
        "{status} {}",
        r.text().await.unwrap_or_default()
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let v: Value = h
                .http
                .get(format!("{}/v1/share/share-device-dsi-1", h.base))
                .header("x-share-gateway", "gateway-test-secret")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if v["status"] != "processing" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("share must finish");
    let lines = h.timeline_lines();
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|l| l["channel"] == "dsi"), "{lines:?}");
    assert_eq!(lines[0]["role"], "user");
    let all = prompts(&h);
    assert!(
        all.contains("Nintendo DSi"),
        "the turn's system prompt has the DSi fact"
    );
    assert!(all.contains("Never say the DSi is unreachable"), "{all}");

    // A plain web share stays web, and says nothing about a DSi.
    let h = setup().await;
    let r = h
        .http
        .post(format!("{}/v1/share", h.base))
        .header("x-share-gateway", "gateway-test-secret")
        .multipart(
            Form::new()
                .text("shareId", "share-device-web-1")
                .text("channel", "web")
                .text("source", "web-share")
                .text("text", "ciao")
                .text("note", "ciao"),
        )
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.timeline_lines().iter().all(|l| l["channel"] == "web"));
    assert!(!prompts(&h).contains("Nintendo DSi"));
}

#[tokio::test]
async fn device_chat_channels_dsi_and_mac_are_recorded_with_their_fact() {
    let h = setup().await;
    let chat = |channel: &str, session: &str| {
        h.post(
            "/chat",
            json!({"message": "ciao come va", "sessionId": session, "channel": channel}),
        )
    };
    let (s, b) = chat("mac", "mac-1").await;
    assert_eq!(s, 200, "{b}");
    let lines = h.timeline_lines();
    assert!(lines.iter().all(|l| l["channel"] == "mac"), "{lines:?}");
    assert!(
        prompts(&h).contains("Lucina app on his Mac"),
        "{}",
        prompts(&h)
    );
    assert!(!prompts(&h).contains("Nintendo DSi"));

    // `web` with Fortuna's session id comes out as `dsi`; an explicit `dsi` too.
    let before = h.timeline_lines().len();
    let (s, b) = chat("web", "fortuna-dev9").await;
    assert_eq!(s, 200, "{b}");
    let (s, b) = chat("dsi", "plain").await;
    assert_eq!(s, 200, "{b}");
    let lines = h.timeline_lines();
    assert!(
        lines[before..].iter().all(|l| l["channel"] == "dsi"),
        "{lines:?}"
    );
    assert!(prompts(&h).contains("Nintendo DSi"));

    // A real web or iPhone turn is untouched; an unknown channel is refused.
    let (s, _) = chat("ios", "i-1").await;
    assert_eq!(s, 200);
    let (s, b) = chat("android", "a-1").await;
    assert_eq!(s, 400, "{b}");
}

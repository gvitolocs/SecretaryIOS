//! A follow-up in his app chat is about his own last topic: a file he shared from the share sheet
//! (the app's default session) counts as this conversation, and the facts about him reach the turn.

mod common;

use common::{harness, script};
use serde_json::json;

#[tokio::test]
async fn a_shared_file_and_his_facts_reach_the_follow_up() {
    let h = harness(script(json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}]})), |_| {}).await;
    std::fs::write(
        h.state.cfg.data_dir.join("owner-facts.md"),
        "- He gets SU as an EU worker: a job of 10-12 h every week.\n",
    )
    .unwrap();
    let now = chrono::Utc::now();
    let default = h.state.cfg.default_session.clone();
    h.state.recent.append(
        &default,
        "user",
        "Save it and wrap it up for me <employment contract, 12 h/week>",
        now - chrono::Duration::minutes(9),
    );
    h.state.recent.append(
        &default,
        "assistant",
        "Saved as a note: employment contract",
        now - chrono::Duration::minutes(9),
    );
    // Older than the window: not this conversation any more.
    h.state.recent.append(
        &default,
        "user",
        "an old shared recipe",
        now - chrono::Duration::hours(5),
    );
    let (s, b) = h.post("/chat", json!({"message": "So I have to do something? And what about the su?", "sessionId": "ios-test", "channel": "ios"})).await;
    assert_eq!(s, 200, "{b}");
    let asked = serde_json::to_string(&h.fake.requests()).unwrap();
    assert!(
        asked.contains("employment contract, 12 h/week"),
        "the shared file is not in the recent turns"
    );
    assert!(!asked.contains("an old shared recipe"));
    assert!(asked.contains("Facts about Giuseppe"), "no facts block");
    assert!(asked.contains("10-12 h every week"));
}

#[tokio::test]
async fn a_juno_device_keeps_its_own_conversation() {
    let h = harness(script(json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}]})), |_| {}).await;
    let default = h.state.cfg.default_session.clone();
    h.state
        .recent
        .append(&default, "user", "a shared contract", chrono::Utc::now());
    let (s, _) = h
        .post(
            "/chat",
            json!({"message": "and now?", "sessionId": "juno-dev_x", "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200);
    let asked = serde_json::to_string(&h.fake.requests()).unwrap();
    assert!(!asked.contains("a shared contract"));
}

#[test]
fn delivered_reminders_say_they_are_not_the_topic() {
    let r: lucina_store::reminders::Reminder = serde_json::from_value(json!({
        "id": "r1", "chatId": "c", "text": "Workshop link", "status": "sent", "sentAt": chrono::Utc::now().to_rfc3339(),
        "dueAt": chrono::Utc::now().to_rfc3339(), "createdAt": chrono::Utc::now().to_rfc3339()
    }))
    .unwrap();
    let block = lucina_store::reminders::delivered_context(
        &[r],
        "c",
        chrono_tz::Europe::Copenhagen,
        chrono::Utc::now(),
    );
    assert!(block.contains("not something he said"), "{block}");
    assert!(block.contains("Workshop link"));
}

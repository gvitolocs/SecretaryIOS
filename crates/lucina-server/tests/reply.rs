//! Swipe to reply (`replyTo` on `/api/secretary/chat`): the quoted item
//! reaches the model as a marked block, the owner's item records `replyTo`.

mod common;

use common::{harness, script, H};
use lucina_server::PlannerMode;
use lucina_store::timeline::{Attachment, Meta, NewItem};
use serde_json::{json, Value};

const HEADER: &str = "The owner is replying to this earlier message";

fn rules() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "contains": "gatti", "content": "I gatti sono QUOTE_MARK indipendenti."},
        {"kind": "agent", "contains": "gatti", "content": "I gatti sono QUOTE_MARK indipendenti."},
        {"kind": "reply", "content": "Certo."},
        {"kind": "agent", "content": "Certo."}
    ]}))
}

async fn setup(planner: PlannerMode) -> H {
    harness(rules(), |c| c.planner = planner).await
}

async fn chat(h: &H, body: Value) -> Value {
    let (s, b) = h.post("/chat", body).await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

/// The user parts of the model requests made since `before`.
fn user_parts(h: &H, before: usize) -> Vec<String> {
    h.fake.requests()[before..]
        .iter()
        .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str().map(str::to_owned))
        .collect()
}

fn line(h: &H, id: &Value) -> Value {
    h.timeline_lines()
        .into_iter()
        .find(|l| l["id"] == *id)
        .unwrap()
}

#[tokio::test]
async fn reply_to_a_lucina_item_quotes_it_in_both_planners() {
    for planner in [PlannerMode::Json, PlannerMode::Tools] {
        let h = setup(planner).await;
        let first = chat(
            &h,
            json!({"message": "che ne pensi dei gatti?", "sessionId": "t", "channel": "ios"}),
        )
        .await;
        assert_eq!(first["reply"], "I gatti sono QUOTE_MARK indipendenti.");
        let quoted = first["lucinaItemId"].clone();
        let before = h.fake.requests().len();
        let d = chat(
            &h,
            json!({"message": "perché dici così?", "sessionId": "t", "channel": "ios",
                   "clientMessageId": "r-1", "replyTo": quoted}),
        )
        .await;
        assert_eq!(d["reply"], "Certo.", "{planner:?}");
        // Same response shape as any turn.
        assert!(d.get("replyTo").is_none());
        let parts = user_parts(&h, before);
        let turn = parts
            .iter()
            .find(|p| p.contains(HEADER))
            .unwrap_or_else(|| panic!("{planner:?}: {parts:?}"));
        assert!(turn.contains("(from Lucina, "), "{planner:?}: {turn}");
        assert!(
            turn.contains("«I gatti sono QUOTE_MARK indipendenti.»"),
            "{turn}"
        );
        // The block sits right before the message, which stays last.
        let at = turn.find(HEADER).unwrap();
        let msg = turn.rfind("Message:\nperché dici così?").unwrap();
        assert!(at < msg && turn.ends_with("perché dici così?"), "{turn}");
        // The owner's item records what it replies to; Lucina's answers it.
        let user = line(&h, &d["userItemId"]);
        assert_eq!(user["replyTo"], quoted);
        assert_eq!(user["clientMessageId"], "r-1");
        assert_eq!(line(&h, &d["lucinaItemId"])["replyTo"], d["userItemId"]);
        // The next turn without replyTo quotes nothing.
        let before = h.fake.requests().len();
        let plain = chat(
            &h,
            json!({"message": "ok grazie", "sessionId": "t", "channel": "ios"}),
        )
        .await;
        assert!(!user_parts(&h, before).iter().any(|p| p.contains(HEADER)));
        assert!(line(&h, &plain["userItemId"]).get("replyTo").is_none());
    }
}

#[tokio::test]
async fn reply_to_a_voice_note_quotes_its_transcript() {
    let h = setup(PlannerMode::Json).await;
    let tl = h.state.timeline.clone();
    let voice = |transcript: Option<&str>| Attachment {
        kind: "voice".into(),
        name: Some("note.m4a".into()),
        duration_ms: Some(1000),
        transcript: transcript.map(str::to_owned),
        ..Attachment::default()
    };
    // Whisper ran after the item was written: only Lucina's reply has the words.
    let note = tl
        .append(NewItem {
            channel: "ios".into(),
            role: "user".into(),
            attachments: vec![voice(None)],
            meta: Some(Meta {
                share_id: Some("voice-reply-0001".into()),
                ..Meta::kind("share")
            }),
            ..NewItem::default()
        })
        .unwrap();
    tl.append(NewItem {
        channel: "ios".into(),
        role: "lucina".into(),
        text: "Va bene.".into(),
        reply_to: Some(note.id.clone()),
        meta: Some(Meta {
            transcript: Some("HEARD_TEXT compra il latte".into()),
            voice_reply: Some(true),
            ..Meta::default()
        }),
        ..NewItem::default()
    })
    .unwrap();
    // On-device transcript: on the attachment itself.
    let on_device = tl
        .append(NewItem {
            channel: "ios".into(),
            role: "user".into(),
            attachments: vec![voice(Some("DEVICE_TEXT chiama Marco"))],
            ..NewItem::default()
        })
        .unwrap();
    for (id, words) in [
        (&note.id, "«HEARD_TEXT compra il latte»"),
        (&on_device.id, "«DEVICE_TEXT chiama Marco»"),
    ] {
        let before = h.fake.requests().len();
        let d = chat(
            &h,
            json!({"message": "fatto?", "sessionId": "t", "channel": "ios", "replyTo": id}),
        )
        .await;
        let parts = user_parts(&h, before).join("\n---\n");
        assert!(
            parts.contains(&format!("{HEADER} (from the owner, ")),
            "{parts}"
        );
        assert!(parts.contains("- voice \"note.m4a\""), "{parts}");
        assert!(
            parts.contains(&format!("voice note transcript: {words}")),
            "{parts}"
        );
        assert_eq!(line(&h, &d["userItemId"])["replyTo"], json!(id));
    }
}

#[tokio::test]
async fn unknown_reply_to_is_a_normal_turn_and_a_malformed_one_is_refused() {
    let h = setup(PlannerMode::Json).await;
    let before = h.fake.requests().len();
    let d = chat(
        &h,
        json!({"message": "ciao", "sessionId": "t", "channel": "ios",
               "replyTo": "01K83ZK4Q6X1V9W2D3E4F5G6H7"}),
    )
    .await;
    assert_eq!(d["reply"], "Certo.");
    assert!(!user_parts(&h, before).iter().any(|p| p.contains(HEADER)));
    assert!(line(&h, &d["userItemId"]).get("replyTo").is_none());
    // Empty means absent.
    chat(
        &h,
        json!({"message": "ciao", "sessionId": "t", "channel": "ios", "replyTo": ""}),
    )
    .await;
    let count = h.timeline_lines().len();
    for bad in [json!("a/b"), json!("x".repeat(65)), json!(5)] {
        let (s, b) = h
            .post("/chat", json!({"message": "ciao", "replyTo": bad}))
            .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (400, Some("bad_request")),
            "{bad}"
        );
    }
    assert_eq!(h.timeline_lines().len(), count, "refused before any write");
}

#[tokio::test]
async fn a_long_quoted_message_is_clipped() {
    let h = setup(PlannerMode::Json).await;
    let long = h
        .state
        .timeline
        .append(NewItem {
            channel: "web".into(),
            role: "lucina".into(),
            text: format!("START {} END_MARK", "parola ".repeat(1500)),
            ..NewItem::default()
        })
        .unwrap();
    let before = h.fake.requests().len();
    chat(
        &h,
        json!({"message": "riassumi", "sessionId": "t", "replyTo": long.id}),
    )
    .await;
    let parts = user_parts(&h, before);
    let turn = parts.iter().find(|p| p.contains(HEADER)).unwrap();
    let start = turn.find(HEADER).unwrap();
    let end = turn.rfind("\n\nMessage:\n").unwrap();
    let block = &turn[start..end];
    assert!(block.chars().count() <= 4000, "{}", block.chars().count());
    assert!(block.contains("«START parola"));
    assert!(!block.contains("END_MARK"));
    assert!(block.ends_with("The owner's new message below is about that message."));
}

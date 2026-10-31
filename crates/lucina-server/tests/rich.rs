//! Lucina message format v1 (`LUCINA_RICH_REPLIES=true`) through `/chat`,
//! against the fake DeepSeek: the owner's 2026-10-22 "inventario" text wall
//! now arrives as at most 3 bubbles plus widgets and buttons.

mod common;

use common::{harness, script};
use serde_json::json;

const QUESTION: &str = "Mi serve SOLO un inventario fattuale e breve di ciò che la tua app/backend supporta ORA. Distingui presente da ipotesi.";

fn wall() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/inventario-reply-2026-10-22.txt"
    ))
    .unwrap()
}

fn deepseek_json() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/inventario-rich-v1.json"
    ))
    .unwrap()
}

fn rules(rich: Option<&str>) -> lucina_server::fake::Script {
    let mut rules = vec![
        json!({"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"}),
        json!({"kind": "reply", "content": wall()}),
    ];
    if let Some(r) = rich {
        rules.push(json!({"kind": "rich", "content": r}));
    }
    script(json!({ "rules": rules }))
}

#[tokio::test]
async fn the_inventario_reply_is_bubbles_widgets_and_buttons() {
    let h = harness(rules(Some(&deepseek_json())), |c| c.rich_replies = true).await;
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": QUESTION, "clientMessageId": "inv-1"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let d = &b["data"];
    assert_eq!(d["format"], "v1");
    assert_eq!(d["lang"], "it");
    let bubbles = d["bubbles"].as_array().unwrap();
    assert!((1..=3).contains(&bubbles.len()), "{bubbles:?}");
    assert!(!d["blocks"].as_array().unwrap().is_empty());
    let actions: Vec<&str> = d["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions[0], "hint-reminder",
        "the 💡 footer became a button: {actions:?}"
    );
    let reply = d["reply"].as_str().unwrap();
    assert!(!reply.contains("Posso anche"), "{reply}");
    assert!(reply.contains("↪︎ rispondi:"), "{reply}");
    // DeepSeek was asked in JSON output mode, with the v1 schema.
    let reqs = h.fake.requests();
    let composer = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "rich")
        .expect("a composer call");
    assert_eq!(composer["response_format"], json!({"type": "json_object"}));
    // The timeline stores the plain fallback plus the validated parts.
    let lines = h.timeline_lines();
    let item = lines.iter().find(|l| l["id"] == d["lucinaItemId"]).unwrap();
    assert_eq!(item["text"], d["reply"]);
    assert_eq!(item["meta"]["format"], "v1");
    assert_eq!(item["meta"]["bubbles"], d["bubbles"]);
    assert_eq!(item["meta"]["actions"], d["actions"]);
}

#[tokio::test]
async fn invalid_composer_json_falls_back_to_split_bubbles() {
    let h = harness(rules(Some("this is not json")), |c| c.rich_replies = true).await;
    let (_, b) = h.post("/chat", json!({"message": QUESTION})).await;
    let d = &b["data"];
    assert_eq!(d["format"], "v1");
    assert!(d["bubbles"].as_array().unwrap().len() <= 3);
    assert!(
        d["blocks"].as_array().unwrap().len() >= 3,
        "{}",
        d["blocks"]
    );
    let composer_calls = h
        .fake
        .requests()
        .iter()
        .filter(|r| lucina_server::fake::kind_of(r) == "rich")
        .count();
    assert_eq!(composer_calls, 2, "one retry, then the fallback");
}

#[tokio::test]
async fn off_by_default_the_reply_is_unchanged() {
    let h = harness(rules(None), |_| {}).await;
    let (_, b) = h.post("/chat", json!({"message": QUESTION})).await;
    let d = &b["data"];
    assert_eq!(d.get("format"), None);
    assert!(d["reply"].as_str().unwrap().contains("**Canali**"));
    assert!(h
        .fake
        .requests()
        .iter()
        .all(|r| r.get("response_format").is_none()));
    let system = h
        .fake
        .requests()
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .map(|r| r["messages"][0]["content"].as_str().unwrap().to_owned())
        .unwrap();
    assert!(!system.contains("How you talk (every channel)"));
}

#[tokio::test]
async fn style_rules_reach_the_prompts_when_on() {
    let h = harness(rules(Some(&deepseek_json())), |c| c.rich_replies = true).await;
    let _ = h.post("/chat", json!({"message": QUESTION})).await;
    let reply_req = h
        .fake
        .requests()
        .into_iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .unwrap();
    let system = reply_req["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("How you talk (every channel)"), "{system}");
}

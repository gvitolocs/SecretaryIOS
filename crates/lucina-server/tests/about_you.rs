//! "About you" end to end: his own words reach the memory through a fake local model, agents' and MCP
//! callers' never do, a self-test writes its own store, the memory rides in every turn, corrections go
//! through `about_me`, the page's routes work, and a secret is refused everywhere.

mod common;

use common::{harness, script};
use lucina_store::about::Status;
use serde_json::{json, Value};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn qwen_says(q: &MockServer, content: Value) {
    q.reset().await;
    Mock::given(method("POST"))
        .and(path_regex("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": content.to_string()}}]})),
        )
        .mount(q)
        .await;
}

async fn setup(rules: Value) -> (common::H, MockServer, tempfile::TempDir) {
    let side = tempfile::tempdir().unwrap();
    let key = side.path().join("qwen.key");
    std::fs::write(&key, "local-key\n").unwrap();
    let qwen = MockServer::start().await;
    let url = format!("{}/v1", qwen.uri());
    let h = harness(script(rules), move |c| {
        c.about.enabled = true;
        c.about.local.url = url;
        c.about.local.key_file = Some(key);
        c.about.local.timeout = std::time::Duration::from_secs(2);
    })
    .await;
    (h, qwen, side)
}

fn plain() -> Value {
    json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}]})
}

async fn chat(h: &common::H, session: &str, message: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": message, "sessionId": session, "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

#[tokio::test]
async fn his_own_words_reach_the_memory_through_the_local_model() {
    let (h, qwen, _side) = setup(plain()).await;
    qwen_says(&qwen, json!({"changes": [{"op": "add", "text": "He started Danish lessons on Tuesdays.", "category": "schedule",
                                         "validUntil": "2026-12-20", "confidence": 0.92}]})).await;
    chat(
        &h,
        "ios-a",
        "I started Danish lessons on Tuesdays, until Christmas",
    )
    .await;
    lucina_server::about::extract::run_queue(&h.state).await;
    let facts = h.state.about.owner.active();
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(facts[0].source.kind, "chat");
    assert_eq!(
        facts[0]
            .valid_until
            .as_ref()
            .unwrap()
            .date
            .unwrap()
            .to_string(),
        "2026-12-20"
    );
    // The local model read his words and what she knew; the cloud model's prompt never had the extractor's.
    let asked =
        String::from_utf8_lossy(&qwen.received_requests().await.unwrap()[0].body).into_owned();
    assert!(asked.contains("Danish lessons") && asked.contains("FACTS:"));
    // Next turn: the memory rides along.
    chat(&h, "ios-a", "so what do I do on tuesday").await;
    let prompts = serde_json::to_string(&h.fake.requests()).unwrap();
    assert!(
        prompts.contains("About Giuseppe")
            && prompts.contains("He started Danish lessons on Tuesdays.")
    );
}

#[tokio::test]
async fn a_self_test_writes_only_its_own_store_and_an_mcp_turn_nothing() {
    let (h, qwen, _side) = setup(plain()).await;
    qwen_says(&qwen, json!({"changes": [{"op": "add", "text": "He keeps bees.", "category": "other", "confidence": 0.95}]})).await;
    chat(&h, "selftest-x", "I keep bees").await;
    let (s, _) = h.post("/chat", json!({"message": "[via Lucina MCP, from claude] the owner keeps bees", "sessionId": "test", "channel": "web"})).await;
    assert_eq!(s, 200);
    lucina_server::about::extract::run_queue(&h.state).await;
    assert!(h.state.about.owner.all().is_empty(), "his memory changed");
    assert_eq!(h.state.about.selftest.active().len(), 1);
    assert_eq!(
        qwen.received_requests().await.unwrap().len(),
        1,
        "only the self-test's own turn was read"
    );
}

#[tokio::test]
async fn the_local_model_down_keeps_the_queue() {
    let (h, qwen, _side) = setup(plain()).await;
    qwen.reset().await;
    chat(&h, "ios-a", "I moved to Aarhus").await;
    lucina_server::about::extract::run_queue(&h.state).await;
    assert!(h.state.about.owner.all().is_empty());
    assert_eq!(
        h.state.about.queue.lock().unwrap().len(),
        1,
        "the item waits"
    );
    assert!(
        h.fake
            .requests()
            .iter()
            .all(|r| !r.to_string().contains("You keep Lucina's memory")),
        "no cloud fallback"
    );
}

#[tokio::test]
async fn a_correction_goes_through_about_me() {
    let rules = json!({"rules": [
        {"kind": "agent", "contains": "wrong", "afterTool": false, "toolCalls": [{"name": "about_me", "arguments": {
            "action": "retract", "id": "ID", "reason": "he never lived there"}}]},
        {"kind": "agent", "contains": "wrong", "afterTool": true, "content": "Fixed: I took that out."},
        {"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}
    ]});
    // The id is only known after the add, so the rule is rewritten once it is.
    let (h0, _q0, _s0) = setup(plain()).await;
    drop(h0);
    let d = lucina_store::about::Draft {
        text: Some("He lives in Odense.".into()),
        category: Some("home".into()),
        ..Default::default()
    };
    let side = tempfile::tempdir().unwrap();
    let store = lucina_store::about::AboutStore::new(side.path().join("x.json"));
    let f = store
        .add(
            &d,
            lucina_store::about::Source {
                kind: "seed".into(),
                reference: String::new(),
            },
            "seed",
            chrono::Utc::now(),
        )
        .unwrap();
    let rules: Value = serde_json::from_str(
        &rules
            .to_string()
            .replace("\"ID\"", &format!("\"{}\"", f.id)),
    )
    .unwrap();
    let (h, _q, _s) = setup(rules).await;
    std::fs::copy(store.path(), h.state.about.owner.path()).unwrap();
    let out = chat(&h, "ios-a", "That's wrong, I never lived in Odense").await;
    assert_eq!(out["reply"], "Fixed: I took that out.", "{out}");
    assert_eq!(
        h.state.about.owner.get(&f.id).unwrap().status,
        Status::Retracted
    );
}

#[tokio::test]
async fn the_page_routes_and_a_refused_secret() {
    let (h, _q, _s) = setup(plain()).await;
    let (s, f) = h
        .post(
            "/about",
            json!({"text": "He is an EU citizen studying in Denmark.", "category": "identity"}),
        )
        .await;
    assert_eq!(s, 200, "{f}");
    let id = f["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(f["data"]["source"]["kind"], "owner-edit");
    let (s, u, _) = h
        .call(
            reqwest::Method::PATCH,
            &format!("/about/{id}"),
            Some(json!({"validUntilCondition": "until he graduates"})),
        )
        .await;
    assert_eq!(s, 200, "{u}");
    let (s, e) = h
        .post(&format!("/about/{id}/end"), json!({"reason": "graduated"}))
        .await;
    assert_eq!((s, e["data"]["status"].as_str()), (200, Some("expired")));
    let (_, all) = h.get("/about").await;
    assert_eq!(all["data"]["facts"].as_array().unwrap().len(), 0);
    assert_eq!(all["data"]["history"].as_array().unwrap().len(), 1);
    let (s, r) = h
        .post(
            "/about",
            json!({"text": "His gmail password is Gatto-778", "category": "identity"}),
        )
        .await;
    assert_eq!(s, 400, "{r}");
    assert_eq!(r["error"], "secret_refused");
}

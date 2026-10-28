//! The ops record over `/api/secretary/ops/*` and in chat (crate::ops,
//! chat::ops_turn): records, search, Map/ pages, the switch, redaction, and
//! Lucina answering "what happened" and recording the owner's decisions.

mod common;

use common::{harness, script, TOKEN};
use serde_json::{json, Value};

async fn with_ops(rules: Value) -> common::H {
    harness(script(rules), |c| {
        let vault = c.data_dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        c.ops.vault_dir = Some(vault);
    })
    .await
}

fn vault_text(h: &common::H) -> String {
    let mut out = String::new();
    let mut stack = vec![h.dir.path().join("vault")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    out
}

#[tokio::test]
async fn records_search_and_pages_over_http() {
    let h = with_ops(json!({"rules": []})).await;
    let rec = json!({
        "kind": "deploy", "project": "lucina", "title": "lucina-rs deployed with the ops record",
        "body": "the key ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234 must not leak", "commit": "abcdef1", "host": "nezopt",
        "links": ["https://example.com/pr/1"]
    });
    let (s, b, _) = h
        .call(reqwest::Method::POST, "/ops/records", Some(rec.clone()))
        .await;
    assert_eq!(s, 201, "{b}");
    let id = b["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(b["data"]["duplicate"], false);
    assert_eq!(b["data"]["redacted"], true);
    assert_eq!(b["data"]["honcho"], "off");
    assert_eq!(b["data"]["paths"]["project"], "Projects/lucina.md");
    // A repeat is a duplicate (200), not a second record.
    let (s, b) = h.post("/ops/records", rec).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["duplicate"], true);
    // The reporter comes from x-lucina-caller.
    let resp = h
        .http
        .post(h.url("/ops/records"))
        .bearer_auth(TOKEN)
        .header("x-lucina-caller", "mcp:claude")
        .json(&json!({"kind": "finding", "project": "nezopt", "title": "fans spin at 88 C"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let (s, b) = h.get(&format!("/ops/records/{id}")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["commit"], "abcdef1");
    let (s, b) = h.get("/ops/search?q=fans&project=nezopt").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["items"][0]["source"], "mcp:claude");
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 1);
    let (s, _) = h.get("/ops/search?kind=party").await;
    assert_eq!(s, 400);
    let (s, _) = h.get("/ops/records/0000000000000000").await;
    assert_eq!(s, 404);
    // Bad input names the field; unknown fields are refused.
    let (s, b) = h
        .post(
            "/ops/records",
            json!({"kind": "decision", "project": "Lucina Agents", "title": "x"}),
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("lucina-agents"), "{b}");
    let (s, _) = h
        .post(
            "/ops/records",
            json!({"kind": "decision", "project": "x", "title": "x", "nope": 1}),
        )
        .await;
    assert_eq!(s, 400);
    // Map pages: only under Map/, idempotent.
    let (s, b, _) = h
        .call(
            reqwest::Method::PUT,
            "/ops/page",
            Some(json!({"path": "Map/index.md", "content": "# Map\n"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["changed"], true);
    let (_, b, _) = h
        .call(
            reqwest::Method::PUT,
            "/ops/page",
            Some(json!({"path": "Map/index.md", "content": "# Map\n"})),
        )
        .await;
    assert_eq!(b["data"]["changed"], false);
    for bad in [
        "Projects/lucina.md",
        "Map/../README.md",
        "Map/.hidden.md",
        "../x.md",
    ] {
        let (s, b, _) = h
            .call(
                reqwest::Method::PUT,
                "/ops/page",
                Some(json!({"path": bad, "content": "x"})),
            )
            .await;
        assert_eq!(s, 400, "{bad}: {b}");
    }
    // Without the bearer: nothing.
    let s = h
        .http
        .get(h.url("/ops/search?q=x"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(s, 401);
    let text = vault_text(&h);
    assert!(text.contains("lucina-rs deployed with the ops record"));
    assert!(!text.contains("ghp_ABCDEFGHIJKLMNOPQRST"), "{text}");
}

#[tokio::test]
async fn the_routes_say_when_the_record_is_off() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = h.get("/ops/search?q=x").await;
    assert_eq!(s, 404, "{b}");
    assert_eq!(b["error"], "ops_disabled", "{b}");
}

#[tokio::test]
async fn lucina_answers_what_happened_from_the_record() {
    let h = with_ops(json!({"rules": [
        {"kind": "reply", "content": "On 28 October the Pi lost power during the Optane benchmark; it rebooted at 12:04."}
    ]}))
    .await;
    let (s, _) = h
        .post("/ops/records", json!({
            "kind": "incident", "project": "pi", "title": "Pi lost power during the Optane benchmark",
            "cause": "power loss", "fix": "rebooted", "severity": "high", "at": "2026-10-28T11:04:33Z"
        }))
        .await;
    assert_eq!(s, 201);
    let d = h.chat("What happened with the pi?").await;
    assert_eq!(d["action"], "ops", "{d}");
    assert!(d["reply"].as_str().unwrap().contains("lost power"), "{d}");
    // The record reached the reply prompt.
    let reqs = h.fake.state.requests.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r
            .to_string()
            .contains("Pi lost power during the Optane benchmark")),
        "the knowledge block is missing"
    );
}

#[tokio::test]
async fn a_question_about_something_else_is_a_normal_turn() {
    let h = with_ops(json!({"rules": [
        {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Your order shipped yesterday.\"}"},
        {"kind": "reply", "content": "Your order shipped yesterday."}
    ]}))
    .await;
    let d = h.chat("what happened with my order?").await;
    assert_ne!(d["action"], "ops", "{d}");
}

#[tokio::test]
async fn the_owners_decision_is_recorded() {
    let h = with_ops(json!({"rules": [
        {"kind": "reply", "content": "Noted: Syncthing it is for the Lucina vault. It's in the record."}
    ]}))
    .await;
    let d = h
        .chat("We decided to keep Syncthing for the lucina vault, with only the Lucina folder synced.")
        .await;
    assert_eq!(d["action"], "ops", "{d}");
    let (_, b) = h.get("/ops/search?kind=decision&project=lucina").await;
    let items = b["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{b}");
    assert_eq!(
        items[0]["title"],
        "keep Syncthing for the lucina vault, with only the Lucina folder synced"
    );
    assert_eq!(items[0]["source"], "lucina");
    assert_eq!(items[0]["decided_by"], "owner");
    let page = std::fs::read_to_string(h.dir.path().join("vault/Projects/lucina.md")).unwrap();
    assert!(page.contains("## Decisions\n\n- "), "{page}");
}

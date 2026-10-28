//! Self-tests and diagnostics never reach the owner's chat: a turn sent with
//! `conversation: "selftest"` (or a `selftest…` session, or a `[selftest]` message) runs in its
//! own conversation and timeline file, on both planners.

mod common;

use common::{harness, script, H};
use serde_json::{json, Value};

fn owner_items(h: &H) -> Vec<Value> {
    h.timeline_lines()
}

fn selftest_items(h: &H) -> Vec<Value> {
    std::fs::read_to_string(h.dir.path().join("selftest-timeline.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn a_self_test_turn_stays_out_of_the_owner_timeline() {
    for tools in [false, true] {
        let rules = json!({"rules": [{"kind": "agent", "content": "pong"}],
                           "default": {"content": "{\"action\":\"reply\",\"text\":\"pong\"}"}});
        let h = harness(script(rules), |c| {
            if tools {
                c.planner = lucina_server::PlannerMode::Tools;
            }
        })
        .await;
        let requests = [
            json!({"message": "ping", "conversation": "selftest", "channel": "ios"}),
            json!({"message": "ping", "sessionId": "selftest-maps", "channel": "web"}),
            json!({"message": "[selftest] ping", "sessionId": "app", "channel": "ios"}),
        ];
        for body in requests {
            let (s, b) = h.post("/chat", body.clone()).await;
            assert_eq!(s, 200, "{b}");
            assert_eq!(
                b["data"]["conversation"], "selftest",
                "tools={tools} {body}"
            );
            assert_eq!(b["data"]["sessionId"], "secretary_selftest");
            assert!(!b["data"]["reply"].as_str().unwrap_or("").is_empty(), "{b}");
        }
        assert!(
            owner_items(&h).is_empty(),
            "tools={tools}: {:?}",
            owner_items(&h)
        );
        assert_eq!(selftest_items(&h).len(), 6, "tools={tools}");
        let (_, owner) = h.get("/timeline").await;
        assert_eq!(owner["data"]["items"], json!([]));
        let (_, own) = h.get("/timeline?conversation=selftest").await;
        assert_eq!(own["data"]["items"].as_array().unwrap().len(), 6);

        // The owner's own turn still lands in the owner's timeline, and only there.
        let d = h.chat("ping").await;
        assert!(d.get("conversation").is_none(), "{d}");
        assert_eq!(owner_items(&h).len(), 2);
        assert_eq!(selftest_items(&h).len(), 6);
    }
}

#[tokio::test]
async fn a_self_test_reply_quotes_its_own_conversation_only() {
    const HEADER: &str = "The owner is replying to this earlier message";
    let rules = json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "contains": "gatti", "content": "I gatti dormono QUOTE_MARK molto."},
        {"kind": "reply", "content": "Certo."}
    ]});
    let h = harness(script(rules), |_| {}).await;
    let user_parts = |before: usize| -> Vec<String> {
        h.fake.requests()[before..]
            .iter()
            .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
            .filter(|m| m["role"] == "user")
            .filter_map(|m| m["content"].as_str().map(str::to_owned))
            .collect()
    };
    let post = |body: Value| async { h.post("/chat", body).await };

    let (_, seed) =
        post(json!({"message": "[selftest] parlami dei gatti", "channel": "ios"})).await;
    let quoted = seed["data"]["lucinaItemId"].clone();
    assert!(quoted.is_string(), "{seed}");
    let owner = h.chat("parlami dei gatti").await;

    // A self-test reply to a self-test item: quoted, and recorded in the self-test timeline.
    let before = h.fake.requests().len();
    let (s, d) = post(
        json!({"message": "[selftest] di cosa parlava?", "channel": "ios",
                             "clientMessageId": "st-r1", "replyTo": quoted}),
    )
    .await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["data"]["conversation"], "selftest");
    let parts = user_parts(before);
    assert!(
        parts
            .iter()
            .any(|p| p.contains(HEADER) && p.contains("QUOTE_MARK")),
        "{parts:?}"
    );
    let user = selftest_items(&h)
        .into_iter()
        .find(|l| l["id"] == d["data"]["userItemId"])
        .unwrap();
    assert_eq!(user["replyTo"], quoted);
    assert!(owner_items(&h)
        .iter()
        .all(|l| l["id"] != d["data"]["userItemId"]));

    // A self-test turn naming an owner item quotes nothing: the owner's chat stays out of it.
    let before = h.fake.requests().len();
    let (s, d) = post(json!({"message": "[selftest] e questo?", "channel": "ios",
                             "replyTo": owner["lucinaItemId"]}))
    .await;
    assert_eq!(s, 200, "{d}");
    assert!(user_parts(before).iter().all(|p| !p.contains(HEADER)));
    let user = selftest_items(&h)
        .into_iter()
        .find(|l| l["id"] == d["data"]["userItemId"])
        .unwrap();
    assert!(user.get("replyTo").is_none(), "{user}");
}

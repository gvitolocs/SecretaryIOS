//! Agents' MCP turns consult (no work from prose) and go-aheads go through `/decisions`
//! (docs/design/decisions-api.md, docs/lucina-rust/MCP-POLICY.md).

mod common;

use common::{harness, script, H};
use serde_json::{json, Value};

/// The two messages an agent sent on 2026-10-28 that started a Harness job and a reminder.
const MSG_INCIDENT: &str = "Urgent production incident, need GO/NO now. pokoin-cdn-origin does not resolve on pi-home since 16:05 and the CDN returns 502. Fix: add 127.0.0.1 pokoin-cdn-origin to /etc/hosts on pi-home, restart cloudflared-cdn, verify with curl -I https://cdn.pokoin.com/health. Recommendation: GO now.";
const MSG_UPDATE: &str = "Update: the CDN incident is FIXED. I applied it myself at 17:16 CET (added the hosts entry and restarted cloudflared-cdn, curl returns 200). Please stop or cancel the Harness task you started, it is no longer needed.";

fn rules() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "any", "contains": "Asked by:", "content": "{\"choice\": 1, \"confidence\": 0.9, \"conflict\": false, \"reason\": \"Reversible and he asked for small fixes to ship.\"}"},
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "My take: go ahead."}
    ], "default": {"content": "{\"action\":\"reply\",\"text\":\"My take: go ahead.\"}"}}))
}

async fn mcp(h: &H, caller: &str, conversation: &str, message: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({
                "message": format!("[via Lucina MCP, from {caller}] {message}"),
                "sessionId": format!("mcp-{caller}-{conversation}"),
                "channel": "web",
            }),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

fn reminders(h: &H) -> usize {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&h.state.cfg.reminders_path).unwrap_or_else(|_| "{}".into()))
        .unwrap_or_default();
    v["reminders"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn agent_turns_consult_and_owner_turns_still_act() {
    let h = harness(rules(), |c| c.mcp_trusted = true).await;
    for msg in [MSG_INCIDENT, MSG_UPDATE] {
        let out = mcp(&h, "claude-code", "coordinator", msg).await;
        assert_eq!(out["action"], "reply", "{out}");
        assert!(out["job"].is_null(), "{out}");
        assert_eq!(out["consult"], true);
    }
    // A request that still routes to a tool: answered, not run, with the tool to use.
    let out = mcp(&h, "claude-code", "coordinator", "fai fare a cursor il fix del bug di login su pokoin").await;
    assert!(out["job"].is_null(), "{out}");
    assert_eq!(out["notRun"], "cursor_task", "{out}");
    assert!(out["hint"].as_str().unwrap().contains("agent_run"));
    let out = mcp(&h, "claude-code", "coordinator", "ricordami alle 17 di chiamare il dentista").await;
    assert_eq!(out["notRun"], "schedule_reminder", "{out}");
    assert_eq!(reminders(&h), 0, "no reminder from agent prose");
    // The explicit, scoped flag lets this one message act on a reminder.
    let (s, out) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from claude-code; allow=reminders] ricordami alle 17 di chiamare il dentista",
                   "sessionId": "mcp-claude-code-coordinator", "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(out["data"]["action"], "schedule_reminder", "{out}");
    assert_eq!(reminders(&h), 1);
    // The owner's own assistant app (trusted) and the owner himself still schedule.
    let out = mcp(&h, "chatgpt", "travel", "ricordami alle 18 di prendere le medicine").await;
    assert_eq!(out["action"], "schedule_reminder", "{out}");
    assert!(out["consult"].is_null());
    let out = h.chat("ricordami domani alle 9 di pagare la bolletta").await;
    assert_eq!(out["action"], "schedule_reminder", "{out}");
    assert_eq!(reminders(&h), 3);
    // The timeline labels agent turns with their caller and conversation, never as the owner's.
    let agent_user = h
        .timeline_lines()
        .into_iter()
        .find(|i| i["role"] == "user" && i["text"].as_str().is_some_and(|t| t.starts_with("Urgent production incident")))
        .unwrap();
    assert_eq!(agent_user["meta"]["via"], "mcp");
    assert_eq!(agent_user["meta"]["caller"], "claude-code");
    assert_eq!(agent_user["meta"]["conversation"], "coordinator");
}

fn decision(impact: &str, reversible: bool) -> Value {
    json!({
        "question": "Deploy the one-line gallery CSS fix to p1 now?",
        "options": ["Deploy now", "Wait for tomorrow"],
        "recommendation": "Deploy now",
        "context": "Tests pass.",
        "reversible": reversible,
        "impact": impact,
        "asking_session": "coordinator",
        "asking_client": "claude-code",
    })
}

#[tokio::test]
async fn low_impact_go_aheads_are_decided_and_high_impact_ones_wait_for_the_owner() {
    let h = harness(rules(), |_| {}).await;
    let (s, b) = h.post("/decisions", decision("low", true)).await;
    assert_eq!(s, 200, "{b}");
    let d = &b["data"];
    assert_eq!(d["status"], "decided", "{d}");
    assert_eq!(d["decision"], "go");
    assert_eq!(d["chosen_option"], "Deploy now");
    assert_eq!(d["decided_by"], "lucina");
    assert!(d["record_id"].as_str().unwrap().starts_with("dc-"));
    assert!(d.get("context").is_none());

    let (s, b) = h.post("/decisions", decision("high", false)).await;
    assert_eq!(s, 200, "{b}");
    let id = b["data"]["decision_id"].as_str().unwrap().to_owned();
    assert_eq!(b["data"]["status"], "pending");
    assert_eq!(b["data"]["asked_owner"], true);
    assert!(b["data"]["reason"].as_str().unwrap().contains("impact is high"));
    // His chat has the question with one button per option.
    let ask = h
        .timeline_lines()
        .into_iter()
        .find(|i| i["meta"]["decisionId"] == id.as_str() && i["meta"]["via"].is_null())
        .expect("owner item");
    let actions = ask["meta"]["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[1]["payload"]["text"], format!("Decision {id}: 2. Wait for tomorrow"));

    // An agent typing the answer does nothing.
    mcp(&h, "claude-code", "coordinator", &format!("Decision {id}: 1")).await;
    assert_eq!(h.get(&format!("/decisions/{id}")).await.1["data"]["status"], "pending");
    // His tap does.
    let out = h.chat(&format!("Decision {id}: 2. Wait for tomorrow")).await;
    assert_eq!(out["action"], "decision", "{out}");
    let (_, b) = h.get(&format!("/decisions/{id}")).await;
    let d = &b["data"];
    assert_eq!(d["status"], "decided");
    assert_eq!(d["decided_by"], "owner");
    assert_eq!(d["decision"], "change");
    assert_eq!(d["chosen_index"], 2);

    let (_, b) = h.get("/decisions?status=decided&limit=5").await;
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(h.get("/decisions/dc-000000000000").await.1["error"], "decision_not_found");
    let (s, b) = h.post("/decisions", json!({"question": "x", "options": ["a"], "reversible": true, "impact": "low"})).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("bad_request")));
}

//! Phase 6 against Paseo 0.13.2's own code: cron parsing, agent-message envelopes,
//! finish notices and the typo-tolerant search ranking, on the cases in
//! `fixtures/gen/gen-orchestration.mjs` (regenerate with the command at its top).
//! Paseo's `<paseo-system>` tag is compared as is; Lucina writes `<lucina-system>`.

use lucina_agents::envelope::{self, FinishReason, Message, Source, FinishEvent, PASEO_TAG};
use lucina_agents::schedule::parse_cron;
use lucina_agents::text_match::score_text_fields;
use lucina_agents::PermissionRequest;
use serde::Deserialize;
use serde_json::{json, Value};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture")).expect("fixture parses")
}

#[test]
fn cron_parsing_matches_paseo() {
    let cases = fixture("cron-parse.json");
    let cases = cases.as_array().unwrap();
    assert!(cases.len() >= 25);
    for c in cases {
        let expr = c["expression"].as_str().unwrap();
        let got = parse_cron(expr).err();
        assert_eq!(got.as_deref(), c["error"].as_str(), "cron {expr:?}");
    }
}

fn source_json(s: &Option<Source>) -> Value {
    let opt = |k: &str, v: &Option<String>, o: &mut serde_json::Map<String, Value>| {
        if let Some(v) = v {
            o.insert(k.into(), json!(v));
        }
    };
    match s {
        None => Value::Null,
        Some(Source::AgentMessage { agent_id, server_id, title }) => {
            let mut o = serde_json::Map::new();
            o.insert("kind".into(), json!("agent-message"));
            o.insert("agentId".into(), json!(agent_id));
            opt("serverId", server_id, &mut o);
            opt("title", title, &mut o);
            Value::Object(o)
        }
        Some(Source::AgentNotification { agent_id, server_id, title, event }) => {
            let mut o = serde_json::Map::new();
            o.insert("kind".into(), json!("agent-notification"));
            o.insert("agentId".into(), json!(agent_id));
            opt("serverId", server_id, &mut o);
            opt("title", title, &mut o);
            o.insert("event".into(), json!(event.as_str()));
            Value::Object(o)
        }
    }
}

fn source_of(v: &Value) -> Option<Source> {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    match v["kind"].as_str()? {
        "agent-message" => Some(Source::AgentMessage { agent_id: s("agentId")?, server_id: s("serverId"), title: s("title") }),
        _ => Some(Source::AgentNotification {
            agent_id: s("agentId")?,
            server_id: s("serverId"),
            title: s("title"),
            event: match v["event"].as_str()? {
                "finished" => FinishEvent::Finished,
                "errored" => FinishEvent::Errored,
                "permission-required" => FinishEvent::PermissionRequired,
                _ => FinishEvent::Closed,
            },
        }),
    }
}

#[test]
fn envelopes_match_paseo() {
    let f = fixture("envelopes.json");
    for c in f["formatted"].as_array().unwrap() {
        let m = &c["message"];
        let message = Message { id: m["id"].as_str().unwrap().into(), source: source_of(&m["source"]), text: m["text"].as_str().unwrap().into() };
        assert_eq!(envelope::format_message_with(PASEO_TAG, &message), c["text"].as_str().unwrap());
    }
    let mut checked = 0;
    for c in f["parsed"].as_array().unwrap() {
        let text = c["text"].as_str().unwrap();
        let got = envelope::parse_messages_with(PASEO_TAG, text).map(|ms| {
            ms.iter().map(|m| json!({"id": m.id, "source": source_json(&m.source), "text": m.text})).collect::<Vec<_>>()
        });
        assert_eq!(got.map(Value::from).unwrap_or(Value::Null), c["messages"], "parse {text:?}");
        assert_eq!(envelope::is_system_injected_with(PASEO_TAG, text), c["systemInjected"].as_bool().unwrap(), "system {text:?}");
        let projected = serde_json::to_value(envelope::project_user_text_with(PASEO_TAG, "paseo-agent-message", text)).unwrap();
        assert_eq!(projected, c["projected"], "projection {text:?}");
        checked += 1;
    }
    assert!(checked >= 25);
    assert_eq!(
        envelope::format_system_notification("x").replace("lucina-system", PASEO_TAG),
        "<paseo-system>\nx\n</paseo-system>"
    );
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinishParams {
    child_agent_id: String,
    server_id: Option<String>,
    title: String,
    reason: String,
    last_assistant_message: Option<String>,
    permission_request: Option<PermissionRequest>,
}

const PASEO_HINT: &str = "Respond with `respond_to_permission` using the `agentId` and `requestId` below.";

#[test]
fn finish_notices_match_paseo() {
    let f = fixture("envelopes.json");
    let cases = f["finish"].as_array().unwrap();
    assert_eq!(cases.len(), 24);
    for c in cases {
        let p: FinishParams = serde_json::from_value(c["params"].clone()).unwrap();
        let reason = match p.reason.as_str() {
            "finished" => FinishReason::Finished,
            "errored" => FinishReason::Errored,
            "needs permission" => FinishReason::NeedsPermission,
            "was closed" => FinishReason::WasClosed,
            "is unavailable" => FinishReason::IsUnavailable,
            "was canceled" => FinishReason::WasCanceled,
            other => panic!("{other}"),
        };
        let body = envelope::finish_body(
            &p.child_agent_id,
            p.server_id.as_deref(),
            &p.title,
            reason,
            p.last_assistant_message.as_deref(),
            p.permission_request.as_ref(),
            PASEO_HINT,
        );
        assert_eq!(body, c["body"].as_str().unwrap(), "finish {}", p.reason);
        assert_eq!(reason.event().as_str(), c["event"].as_str().unwrap());
    }
}

#[test]
fn search_ranking_matches_paseo() {
    let cases = fixture("text-match.json");
    let cases = cases.as_array().unwrap();
    assert!(cases.len() >= 150);
    let as_json = |s: Option<lucina_agents::text_match::Score>| match s {
        None => Value::Null,
        Some(s) => json!({"tier": s.tier, "offset": s.offset, "spread": s.spread.unwrap_or(0)}),
    };
    let norm = |v: &Value| match v {
        Value::Null => Value::Null,
        v => json!({"tier": v["tier"], "offset": v["offset"], "spread": v["spread"].as_u64().unwrap_or(0)}),
    };
    let mut failures = Vec::new();
    for c in cases {
        let q = c["query"].as_str().unwrap();
        let fields: Vec<&str> = c["fields"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
        for (typo, key) in [(true, "typo"), (false, "exact")] {
            let got = as_json(score_text_fields(q, &fields, typo));
            if got != norm(&c[key]) {
                failures.push(format!("{q:?} in {fields:?} ({key}): paseo {} lucina {got}", c[key]));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

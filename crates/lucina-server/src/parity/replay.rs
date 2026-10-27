//! `lucina shadow-replay`: the owner's real app/web chat turns, read from
//! the live `timeline.jsonl` (read only), replayed one by one against a
//! Rust Lucina, with Node's answer next to Rust's.
//!
//! The timeline does not store the action Node took, so it is inferred from
//! the reply and the item meta (`kind`, `jobId`, fixed reply texts). Lucina
//! MCP turns (meta `via: "mcp"`) are replayed with their prefix and an
//! `mcp-<caller>` session, so the MCP guard runs again.
//!
//! Point it at a sandbox (data copies), not at the deployed shadow: a
//! replayed reminder, shopping or note turn writes.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Turn {
    pub ts: String,
    pub channel: String,
    pub text: String,
    pub caller: Option<String>,
    pub backfilled: bool,
    pub node_reply: String,
    pub node_meta: Value,
}

/// Owner turns on `channels` with Node's first reply, oldest first; the
/// last `last` of them.
pub fn turns(timeline: &Path, channels: &[String], last: usize) -> anyhow::Result<Vec<Turn>> {
    let items: Vec<Value> = std::fs::read_to_string(timeline)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let ch = it["channel"].as_str().unwrap_or("");
        if it["role"] != "user" || !channels.iter().any(|c| c == ch) {
            continue;
        }
        let id = it["id"].as_str().unwrap_or("");
        // The reply: the first Lucina item answering it; for backfilled items
        // (no replyTo) the next Lucina item on the channel before the next owner item.
        let mut reply = items[i + 1..]
            .iter()
            .find(|x| x["role"] == "lucina" && x["replyTo"] == id);
        if reply.is_none() {
            for x in &items[i + 1..] {
                if x["channel"] != ch {
                    continue;
                }
                if x["role"] == "user" {
                    break;
                }
                if x["role"] == "lucina" {
                    reply = Some(x);
                    break;
                }
            }
        }
        let Some(reply) = reply else { continue };
        let caller = (it["meta"]["via"] == "mcp").then(|| {
            it["meta"]["caller"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned()
        });
        out.push(Turn {
            ts: it["ts"].as_str().unwrap_or("").to_owned(),
            channel: ch.to_owned(),
            text: it["text"].as_str().unwrap_or("").to_owned(),
            caller,
            backfilled: it["backfilled"] == true,
            node_reply: reply["text"].as_str().unwrap_or("").to_owned(),
            node_meta: reply["meta"].clone(),
        });
    }
    let skip = out.len().saturating_sub(last);
    Ok(out.split_off(skip))
}

/// Node's action, inferred from its reply and meta.
pub fn infer_node_action(reply: &str, meta: &Value, persona: &str) -> &'static str {
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| reply.starts_with(p));
    if !meta["jobId"].is_null() {
        return "job";
    }
    match meta["kind"].as_str() {
        Some("shopping") => return "grocery",
        Some("note" | "notes") => return "notes",
        Some("calendar") => return "calendar",
        Some("avatar_outfit") => return "avatar_outfit",
        _ => {}
    }
    if reply.contains("needs Giuseppe's own confirmation, so it does not run from Lucina MCP") {
        return "needs_confirmation";
    }
    if starts(&["Reminder set for ", "That reminder was already scheduled. "])
        || reply.starts_with(&format!("{persona}: I could not parse that reminder"))
        || reply.starts_with(&format!("{persona}: timed reminders are disabled"))
        || reply.starts_with("I could not work out when to remind you")
        || reply.starts_with("Timed reminders are disabled")
    {
        return "schedule_reminder";
    }
    if reply.starts_with(&format!("{persona}: promemoria "))
        || reply.starts_with(&format!("{persona}: non hai promemoria"))
    {
        return "list_reminders";
    }
    if starts(&[
        "Added ",
        "Removed ",
        "Shopping list: ",
        "Your shopping list is empty",
        "You have spent ",
        "Pantry: ",
        "Your pantry is empty",
        "Marked ",
        "Updated your pantry",
        "Which supermarket",
        "Was this from ",
        "What did you buy?",
        "I found ",
        "Live grocery catalog",
        "Offers on your shopping list",
        "Nothing on your shopping list",
        "I could not find a current offer",
        "Grocery accounting is not configured",
        "I could not update grocery accounting",
        "Which pantry item",
    ]) {
        return "grocery";
    }
    "reply"
}

fn rust_action(data: &Value) -> String {
    if !data["job"].is_null() {
        return "job".into();
    }
    match data["action"].as_str().unwrap_or("") {
        "calendar" | "calendar_create" | "calendar_confirm" => "calendar".into(),
        a => a.to_owned(),
    }
}

/// Lowercased word sets' Jaccard similarity (0..1) for LLM replies.
fn similarity(a: &str, b: &str) -> f64 {
    let words = |s: &str| -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_owned)
            .collect()
    };
    let (x, y) = (words(a), words(b));
    if x.is_empty() && y.is_empty() {
        return 1.0;
    }
    x.intersection(&y).count() as f64 / x.union(&y).count().max(1) as f64
}

/// Replay and compare. Returns the full report (with texts).
pub async fn run(
    turns: &[Turn],
    rust_url: &str,
    token: &str,
    persona: &str,
    pause: Duration,
) -> Value {
    let http = reqwest::Client::new();
    let mut rows = Vec::new();
    for (i, t) in turns.iter().enumerate() {
        let (message, session) = match &t.caller {
            Some(c) => (
                format!("[via Lucina MCP, from {c}] {}", t.text),
                format!("mcp-{}-replay", crate::parity::replay::slug(c)),
            ),
            None => (t.text.clone(), format!("replay-{}", t.channel)),
        };
        let started = std::time::Instant::now();
        let resp = http
            .post(format!("{rust_url}/api/secretary/chat"))
            .bearer_auth(token)
            .json(&json!({"message": message, "sessionId": session, "channel": t.channel, "clientMessageId": format!("replay-{i}")}))
            .timeout(Duration::from_secs(120))
            .send()
            .await;
        let ms = started.elapsed().as_millis() as u64;
        let (status, data) = match resp {
            Ok(r) => {
                let s = r.status().as_u16();
                let v: Value = r.json().await.unwrap_or(Value::Null);
                (s, v["data"].clone())
            }
            Err(e) => (0, json!({"error": e.to_string()})),
        };
        let node_action = infer_node_action(&t.node_reply, &t.node_meta, persona);
        let ra = rust_action(&data);
        let rust_reply = data["reply"].as_str().unwrap_or("").to_owned();
        let exact = rust_reply.trim() == t.node_reply.trim();
        rows.push(json!({
            "i": i, "ts": t.ts, "channel": t.channel, "mcpCaller": t.caller, "backfilled": t.backfilled,
            "status": status, "ms": ms,
            "nodeAction": node_action, "rustAction": ra, "actionMatch": node_action == ra,
            "exactReply": exact, "similarity": (similarity(&t.node_reply, &rust_reply) * 100.0).round() / 100.0,
            "nodeMetaKind": t.node_meta["kind"], "rustBlocked": data["blockedAction"],
            "message": t.text, "nodeReply": t.node_reply, "rustReply": rust_reply,
        }));
        tokio::time::sleep(pause).await;
    }
    let n = rows.len();
    let ok_status = rows.iter().filter(|r| r["status"] == 200).count();
    let action_match = rows.iter().filter(|r| r["actionMatch"] == true).count();
    let det: Vec<&Value> = rows.iter().filter(|r| r["nodeAction"] != "reply").collect();
    let det_exact = det.iter().filter(|r| r["exactReply"] == true).count();
    json!({
        "turns": n, "http200": ok_status, "actionMatch": action_match,
        "deterministicTurns": det.len(), "deterministicExact": det_exact,
        "rows": rows,
    })
}

/// `mcp-<caller>` session part.
pub fn slug(caller: &str) -> String {
    let s: String = caller
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    s.trim_matches('_').chars().take(24).collect()
}

/// Console summary without message texts (they are the owner's).
pub fn summary(report: &Value, show_text: bool) -> String {
    let mut out = vec![format!(
        "shadow-replay: {} turns, HTTP 200 {}, action match {}/{}, deterministic replies identical {}/{}",
        report["turns"], report["http200"], report["actionMatch"], report["turns"], report["deterministicExact"], report["deterministicTurns"]
    )];
    for r in report["rows"].as_array().into_iter().flatten() {
        let mut line = format!(
            "#{:<2} {} {:<4}{} node={:<18} rust={:<18} {} sim={} {}ms",
            r["i"],
            r["ts"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(16)
                .collect::<String>(),
            r["channel"].as_str().unwrap_or(""),
            r["mcpCaller"]
                .as_str()
                .map(|c| format!(" mcp:{c}"))
                .unwrap_or_default(),
            r["nodeAction"].as_str().unwrap_or(""),
            r["rustAction"].as_str().unwrap_or(""),
            if r["actionMatch"] == true {
                "same"
            } else {
                "DIFF"
            },
            r["similarity"],
            r["ms"],
        );
        if show_text {
            line.push_str(&format!(
                "\n    message: {}\n    node:    {}\n    rust:    {}",
                r["message"], r["nodeReply"], r["rustReply"]
            ));
        }
        out.push(line);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_node_actions() {
        assert_eq!(
            infer_node_action(
                "Reminder set for Thursday, 22 October 2026 at 23:59 (Europe/Rome): x",
                &Value::Null,
                "Lucina"
            ),
            "schedule_reminder"
        );
        assert_eq!(
            infer_node_action(
                "Added milk to your shopping list.",
                &json!({"kind": "shopping"}),
                "Lucina"
            ),
            "grocery"
        );
        assert_eq!(
            infer_node_action(
                "Lucina: promemoria futuri (2, Europe/Rome):\n1. …",
                &Value::Null,
                "Lucina"
            ),
            "list_reminders"
        );
        assert_eq!(
            infer_node_action("x", &json!({"kind": "note", "slug": "a"}), "Lucina"),
            "notes"
        );
        assert_eq!(infer_node_action("Lucina: I did not do that. \"poko_news_scout_retry\" needs Giuseppe's own confirmation, so it does not run from Lucina MCP. Ask me in the Secretary app or on Telegram.", &Value::Null, "Lucina"), "needs_confirmation");
        assert_eq!(infer_node_action("Ciao!", &Value::Null, "Lucina"), "reply");
        assert!(similarity("Ciao Giuseppe, tutto bene?", "ciao giuseppe tutto bene") > 0.9);
        assert_eq!(slug("Claude Code"), "claude_code");
    }
}

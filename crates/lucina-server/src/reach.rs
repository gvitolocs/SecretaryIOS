//! Lucina's messages to Claude, Grok and Lucina Agents in the server (`lucina_tools::tools::reach`):
//! where replies land, the mailbox API Claude sessions answer through, and the `lucina-rs reach`
//! CLI.
//!
//! - A reply goes back into the conversation the message came from: the owner's timeline under
//!   that channel (the self-test timeline for a self-test) and the session's recent turns (so her
//!   next answer knows it). It is shown as information from that assistant ("💬 Claude replied …"),
//!   never run as an instruction, and it alerts nobody: the owner's phone rings for messages to
//!   him, not for her chats with agents (owner, 2026-11-02). An agent that needs him uses
//!   `lucina_decide`.
//! - `GET  /api/secretary/reach/threads?to=claude&status=open&limit=20`  the threads, newest first
//! - `GET  /api/secretary/reach/threads/<id>`                            one thread
//! - `POST /api/secretary/reach/threads/<id>/reply` `{"from", "text"}`   a reply (≤ 8000 chars)
//!
//! The Secretary bearer guards these like every `/api/secretary` route.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Uri};
use axum::response::Response;
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{Meta, NewItem, TimelineStore, CHANNELS};
use lucina_tools::tools::reach::{self, Reach, ReachConfig, ReachSink, Reply, Target, Thread};
use serde_json::json;

use crate::api::{ok, query_param, read_json, ApiError};
use crate::AppState;

/// Where replies go.
pub struct ServerSink {
    pub timeline: Arc<TimelineStore>,
    pub selftest_timeline: Arc<TimelineStore>,
    pub recent: Arc<RecentChat>,
}

/// What the owner reads for a reply.
pub fn reply_text(t: &Thread, r: &Reply) -> String {
    let who = match t.to {
        Target::Agents => format!("Lucina Agents ({})", r.from),
        Target::Mistral => "Mistral".to_owned(),
        _ => r.from.clone(),
    };
    format!(
        "💬 {who} replied to my message \"{}\" ({}):\n\n{}",
        t.subject, t.id, r.text
    )
}

#[async_trait::async_trait]
impl ReachSink for ServerSink {
    async fn deliver(&self, t: &Thread, r: &Reply) {
        let text = reply_text(t, r);
        let channel = if CHANNELS.contains(&t.channel.as_str()) {
            t.channel.clone()
        } else {
            "other".to_owned()
        };
        let timeline = if t.test {
            &self.selftest_timeline
        } else {
            &self.timeline
        };
        timeline.record(NewItem {
            channel,
            role: "lucina".into(),
            text: text.clone(),
            meta: Some(Meta {
                tool: Some(format!("reach_{}", t.to.as_str())),
                ok: Some(true),
                ..Meta::kind("reach")
            }),
            ..Default::default()
        });
        if t.to == Target::Mistral {
            // The exchange has its own conversation in All chats: the agent's words as a caller's turn.
            timeline.record(NewItem {
                channel: "other".into(),
                role: "user".into(),
                text: r.text.clone(),
                meta: Some(mistral_meta(t)),
                ..Default::default()
            });
        }
        // Her next turn in that conversation sees the reply as something said to her.
        self.recent
            .append(&t.session, "assistant", &text, chrono::Utc::now());
    }

    async fn sent(&self, t: &Thread, text: &str) {
        if t.to == Target::Mistral {
            let timeline = if t.test { &self.selftest_timeline } else { &self.timeline };
            timeline.record(NewItem {
                channel: "other".into(),
                role: "lucina".into(),
                text: text.to_owned(),
                meta: Some(mistral_meta(t)),
                ..Default::default()
            });
        }
    }
}

/// An item of the `agent:mistral` conversation. `via` and the `reach` kind keep it off his phone.
fn mistral_meta(t: &Thread) -> Meta {
    Meta {
        via: Some("mcp".into()),
        caller: Some("mistral".into()),
        tool: Some(format!("reach_{}", t.to.as_str())),
        ok: Some(true),
        ..Meta::kind("reach")
    }
}

/// The reach state from the environment (`LUCINA_REACH`, default on), its sink still unset.
pub fn build(data_dir: &std::path::Path, ops: Option<Arc<lucina_ops::Ops>>) -> Option<Arc<Reach>> {
    let cfg = ReachConfig::from_env(data_dir)?;
    tracing::info!(
        claude_channel = cfg.claude_channel.as_deref().unwrap_or("-"),
        grok = %cfg.grok_target,
        agents = %cfg.agents_provider,
        "reach: Lucina can message Claude (mailbox), Grok and Lucina Agents"
    );
    let r = Arc::new(Reach::new(cfg));
    if let Some(ops) = ops {
        r.set_ops(ops);
    }
    Some(r)
}

fn reach_of(st: &AppState) -> Result<Arc<Reach>, ApiError> {
    st.tool_env.reach.clone().ok_or_else(|| {
        ApiError::new(404, "reach_disabled", "Messaging is off on this host (LUCINA_REACH)")
    })
}

/// `/api/secretary/reach/…` (`parts` without `reach`).
pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    let r = reach_of(st)?;
    match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["threads"] => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let to = match query_param(uri, "to") {
                Some(t) => Some(
                    Target::parse(&t)
                        .ok_or_else(|| ApiError::bad_request("to is claude, grok or agents"))?,
                ),
                None => None,
            };
            let status = query_param(uri, "status");
            let limit = query_param(uri, "limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(20);
            let mut v = json!({"threads": r
                .list(to, status.as_deref(), limit)
                .iter()
                .map(Thread::view)
                .collect::<Vec<_>>()});
            lucina_secrets::redactor().redact_json(&mut v);
            Ok(ok(v))
        }
        ["threads", id] => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let t = thread(&r, id)?;
            let mut v = t.view();
            v["envelope"] = json!(r.envelope(&t));
            lucina_secrets::redactor().redact_json(&mut v);
            Ok(ok(v))
        }
        ["threads", id, "reply"] => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            thread(&r, id)?;
            let b = read_json(body, crate::api::MAX_BODY_BYTES).await?;
            let text = b["text"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ApiError::bad_request("text is required"))?;
            if text.chars().count() > 8000 {
                return Err(ApiError::bad_request("text is at most 8000 characters"));
            }
            let from = b["from"].as_str().unwrap_or("Claude");
            let t = r
                .add_reply(id, from, text)
                .await
                .ok_or_else(ApiError::not_found)?;
            tracing::info!(thread = %t.id, to = t.to.as_str(), "reach: reply delivered");
            Ok(ok(json!({"thread": t.id, "status": t.status, "delivered": true})))
        }
        _ => Err(ApiError::not_found()),
    }
}

fn thread(r: &Reach, id: &str) -> Result<Thread, ApiError> {
    if !reach::valid_id(id) {
        return Err(ApiError::bad_request("a thread id is rt- and 12 hex digits"));
    }
    r.get(id).ok_or_else(ApiError::not_found)
}

// ---------------------------------------------------------------- CLI

/// `lucina-rs reach inbox|show|reply`, through the running server.
pub mod cli {
    use serde_json::{json, Value};

    use crate::decide::cli::Client;

    /// The open (unanswered) messages to `to` (default claude).
    pub async fn inbox(to: &str, all: bool, limit: u32) -> anyhow::Result<Value> {
        let c = Client::from_env()?;
        let status = if all { "all" } else { "open" };
        c.get_path(&format!("reach/threads?to={to}&status={status}&limit={limit}"))
            .await
    }

    pub async fn show(id: &str) -> anyhow::Result<Value> {
        if !lucina_tools::tools::reach::valid_id(id) {
            anyhow::bail!("a thread id is rt- and 12 hex digits");
        }
        Client::from_env()?
            .get_path(&format!("reach/threads/{id}"))
            .await
    }

    pub async fn reply(id: &str, from: &str, text: &str) -> anyhow::Result<Value> {
        if !lucina_tools::tools::reach::valid_id(id) {
            anyhow::bail!("a thread id is rt- and 12 hex digits");
        }
        Client::from_env()?
            .post(
                &format!("reach/threads/{id}/reply"),
                &json!({"from": from, "text": text}),
            )
            .await
    }

    /// One line per thread for the console.
    pub fn lines(v: &Value) -> String {
        v["threads"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        format!(
                            "{}  {:<8} {:<8} {}  {}",
                            t["id"].as_str().unwrap_or(""),
                            t["to"].as_str().unwrap_or(""),
                            t["status"].as_str().unwrap_or(""),
                            t["created"].as_str().unwrap_or("").get(..16).unwrap_or(""),
                            t["subject"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(dir: &std::path::Path) -> (ServerSink, Arc<TimelineStore>) {
        let timeline = Arc::new(TimelineStore::new(dir.join("t.jsonl"), 1 << 20, true));
        let selftest = Arc::new(TimelineStore::new(dir.join("s.jsonl"), 1 << 20, true));
        let recent = Arc::new(RecentChat::new(None));
        (ServerSink { timeline: timeline.clone(), selftest_timeline: selftest, recent }, timeline)
    }

    fn mistral_thread() -> Thread {
        Thread {
            id: "rt-abc".into(),
            to: Target::Mistral,
            kind: "message".into(),
            subject: "a 404".into(),
            text: "the report".into(),
            channel: "ios".into(),
            session: "sess".into(),
            created: chrono::Utc::now(),
            status: "sent".into(),
            route: "mistral-agent".into(),
            detail: Some("agent-1".into()),
            replies: vec![],
            test: false,
            session_url: None,
        }
    }

    #[tokio::test]
    async fn a_mistral_exchange_is_one_conversation_in_all_chats_and_stays_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, timeline) = sink(dir.path());
        let t = mistral_thread();
        sink.sent(&t, "the report").await;
        let reply = Reply { from: "mistral-agent".into(), text: "the cause".into(), at: chrono::Utc::now() };
        sink.deliver(&t, &reply).await;

        let all = timeline.conversations().unwrap();
        let c = all.iter().find(|c| c.key == "agent:mistral").expect("a Mistral conversation");
        assert_eq!(c.title, "Mistral");
        assert_eq!(c.count, 2, "what she sent and what came back");
        // Her own chat gets the reply too, as a reach item.
        assert!(all.iter().any(|c| c.key != "agent:mistral"), "{all:?}");
        // What keeps it off his phone: secretary-server's ingester skips `meta.via` and the `reach` kind.
        let items = timeline.read_all().unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.raw.contains(r#""kind":"reach""#)), "{:?}", items.iter().map(|i| i.raw.to_string()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn other_targets_get_no_mistral_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, timeline) = sink(dir.path());
        let mut t = mistral_thread();
        t.to = Target::Claude;
        sink.sent(&t, "x").await;
        let reply = Reply { from: "c".into(), text: "y".into(), at: chrono::Utc::now() };
        sink.deliver(&t, &reply).await;
        assert!(timeline.conversations().unwrap().iter().all(|c| c.key != "agent:mistral"));
    }

    #[test]
    fn replies_read_as_information() {
        let t = Thread {
            id: "rt-0123456789ab".into(),
            to: Target::Claude,
            kind: "issue".into(),
            subject: "Share link opens without sign-in".into(),
            text: "…".into(),
            channel: "ios".into(),
            session: "secretary_ios-1".into(),
            created: chrono::Utc::now(),
            status: "answered".into(),
            route: "lucina-mailbox".into(),
            detail: None,
            replies: Vec::new(),
            test: false,
            session_url: None,
        };
        let r = Reply {
            from: "coordinator".into(),
            text: "Fixed: share links now need a session.".into(),
            at: chrono::Utc::now(),
        };
        let s = reply_text(&t, &r);
        assert!(s.starts_with("💬 coordinator replied to my message"));
        assert!(s.contains("rt-0123456789ab") && s.ends_with("Fixed: share links now need a session."));
    }
}

//! `memos`: the owner's voice memos (lucina-memo on nezopt, `/v1/memos`,
//! SecretaryIOS `docs/memos.md`). Each memo has Whisper's timed transcript
//! and Lucina's summary (overview, key points, decisions, action items).
//!
//! - `list`: recent memos (newest first), optionally the last `days`.
//! - `search`: "what did I say in yesterday's memo about X": a date range
//!   (`from`/`to` as YYYY-MM-DD in the owner's zone, or `days`) and words;
//!   returns each match's summary and the transcript lines with their times.
//! - `read`: one memo's summary and transcript.
//! - `resummarize`: a new summary (optionally with the owner's instructions);
//!   the PDF, Obsidian note and notification follow. Lucina's own data: no
//!   confirmation.
//!
//! Action items become tasks with the `tasks` tool (`add`) when the owner asks.
//!
//! Configuration: `LUCINA_MEMO_URL` (default `http://127.0.0.1:8795`) and
//! `LUCINA_MEMO_SERVICE_TOKEN_FILE` (read per call).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Transcript characters a `read` returns.
const READ_CHARS: usize = 14_000;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Search,
    Read,
    Resummarize,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// search: words to find (any language of the memo), e.g. "preventivo cucina". Optional: without it, every memo in the range.
    query: Option<String>,
    /// search: first day, YYYY-MM-DD (owner's time zone). "yesterday" = that date.
    from: Option<String>,
    /// search: last day, YYYY-MM-DD (inclusive).
    to: Option<String>,
    /// list/search: only the last N days.
    days: Option<i64>,
    /// read/resummarize: the memo id (mm_...) from list or search.
    id: Option<String>,
    /// resummarize: what the new summary should focus on or how to write it.
    instructions: Option<String>,
}

pub struct MemosTool;

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    let url = ctx
        .env
        .memo_url
        .clone()
        .ok_or_else(|| ToolError::new("voice memos are not configured (LUCINA_MEMO_URL)"))?;
    let file = ctx.env.memo_token_file.as_ref().ok_or_else(|| {
        ToolError::new("voice memos are not configured (LUCINA_MEMO_SERVICE_TOKEN_FILE)")
    })?;
    let token = std::fs::read_to_string(file)
        .map_err(|_| ToolError::new("voice memos: service token unreadable"))?
        .trim()
        .to_owned();
    Ok((url.trim_end_matches('/').to_owned(), token))
}

async fn send(req: reqwest::RequestBuilder) -> Result<Value, ToolError> {
    let resp = req
        .send()
        .await
        .map_err(|e| ToolError::new(format!("lucina-memo unreachable: {}", e.without_url())))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let message = body["error"]["message"].as_str().unwrap_or_default();
    Err(ToolError::new(format!(
        "lucina-memo answered {}: {message}",
        status.as_u16()
    )))
}

fn valid_id(id: &str) -> bool {
    id.len() == 19 && id.starts_with("mm_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// One memo of a list, compact for the model.
fn compact(m: &Value) -> Value {
    json!({
        "event": m["event"],
        "id": m["id"],
        "title": m["title"],
        "recordedAt": m["recordedAt"],
        "seconds": m["durationSec"].as_f64().map(|s| s.round()),
        "status": m["status"],
        "language": m["language"],
        "summary": m["preview"],
        "actionItems": m["actionItems"],
    })
}

#[async_trait::async_trait]
impl Tool for MemosTool {
    fn name(&self) -> &'static str {
        "memos"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's voice memos recorded in the Secretary app (transcribed on his server, with your summary, \
         a PDF and an Obsidian note): list recent ones, search by date range and words (\"what did I say in \
         yesterday's memo about X\": search with from/to = yesterday's date and the topic words; without words \
         you get every memo of those days with its summary), read one (summary and transcript with times), or \
         resummarize one with his instructions. Quote what he said with the time (e.g. at 1:05). To turn an \
         action item into a task, use the tasks tool."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "memos {} {}",
            args["action"].as_str().unwrap_or(""),
            args["query"].as_str().or(args["id"].as_str()).unwrap_or("")
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (base, token) = server(ctx)?;
        let http = &ctx.env.http;
        let id = || -> Result<String, ToolError> {
            let id = a.id.as_deref().map(str::trim).unwrap_or_default();
            if valid_id(id) {
                Ok(id.to_owned())
            } else {
                Err(ToolError::new(
                    "id: a memo id like mm_0123456789abcdef (from list or search)",
                ))
            }
        };
        match a.action {
            Action::List => {
                let body = send(
                    http.get(format!("{base}/v1/memos"))
                        .bearer_auth(&token)
                        .query(&[("limit", "30")]),
                )
                .await?;
                let cutoff = a
                    .days
                    .map(|d| chrono::Utc::now() - chrono::Duration::days(d.clamp(1, 3650)));
                let memos: Vec<Value> = body["memos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| {
                        cutoff.is_none_or(|c| {
                            m["recordedAt"]
                                .as_str()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .is_some_and(|t| t >= c)
                        })
                    })
                    .map(compact)
                    .collect();
                Ok(json!({"memos": memos, "timeZone": ctx.env.time_zone.name()}))
            }
            Action::Search => {
                let mut q: Vec<(&str, String)> = vec![("limit", "10".into())];
                if let Some(s) = a.query.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    q.push(("q", s.to_owned()));
                }
                if let Some(f) = &a.from {
                    q.push(("from", f.trim().to_owned()));
                }
                if let Some(t) = &a.to {
                    q.push(("to", t.trim().to_owned()));
                }
                if let Some(d) = a.days {
                    q.push(("days", d.to_string()));
                }
                let body = send(
                    http.get(format!("{base}/v1/memos/search"))
                        .bearer_auth(&token)
                        .query(&q),
                )
                .await?;
                let hits = body["memos"].as_array().cloned().unwrap_or_default();
                Ok(
                    json!({"memos": hits, "found": hits.len(), "timeZone": ctx.env.time_zone.name()}),
                )
            }
            Action::Read => {
                let id = id()?;
                let detail = send(
                    http.get(format!("{base}/v1/memos/{id}"))
                        .bearer_auth(&token),
                )
                .await?;
                let transcript = send(
                    http.get(format!("{base}/v1/memos/{id}/transcript"))
                        .bearer_auth(&token),
                )
                .await
                .ok();
                let mut text = String::new();
                if let Some(t) = &transcript {
                    for s in t["transcript"]["segments"].as_array().into_iter().flatten() {
                        let at = s["start"].as_f64().unwrap_or(0.0) as u64;
                        let line = format!(
                            "[{}:{:02}] {}\n",
                            at / 60,
                            at % 60,
                            s["text"].as_str().unwrap_or("")
                        );
                        if text.chars().count() + line.chars().count() > READ_CHARS {
                            text.push_str("[…transcript cut]\n");
                            break;
                        }
                        text.push_str(&line);
                    }
                }
                Ok(json!({
                    "memo": compact(&detail["memo"]),
                    "summary": detail["summary"],
                    "transcript": if text.is_empty() { Value::Null } else { Value::String(text) },
                    "url": detail["url"],
                }))
            }
            Action::Resummarize => {
                let id = id()?;
                let instructions = a
                    .instructions
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let body = send(
                    http.post(format!("{base}/v1/memos/{id}/resummarize"))
                        .bearer_auth(&token)
                        .json(&json!({"instructions": instructions})),
                )
                .await?;
                Ok(json!({
                    "queued": true,
                    "memo": compact(&body["memo"]),
                    "note": "The new summary, PDF and Obsidian note are on their way; Giuseppe gets a notification when they are ready."
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_schema() {
        assert!(valid_id("mm_0123456789abcdef"));
        assert!(!valid_id("mm_../../etc"));
        let schema = MemosTool.parameters();
        assert_eq!(
            schema["properties"]["action"]["enum"]
                .as_array()
                .map(Vec::len),
            Some(4)
        );
    }
}

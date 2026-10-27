//! `diagnostics`: crash reports and logs from the owner's Lucina clients
//! (lucina-diagnostics on nezopt, `/v1/diagnostics/reports`, SecretaryIOS
//! `docs/diagnostics.md`). Lets Lucina answer "why did Secretary Code crash?"
//! with the report's message, stack, context and the end of its log.
//!
//! - `recent`: reports newest first, filtered by app, kind and `since`.
//! - `get`: one report with the end of its main log.
//! - `file`: a text attachment (the end by default).
//!
//! Everything was scrubbed of secrets on arrival; minidumps are not readable
//! here. Configuration: `LUCINA_DIAGNOSTICS_URL` (default
//! `http://127.0.0.1:8794`) and `LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE`
//! (read per call).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Log characters `get` returns.
const LOG_CHARS: usize = 6_000;
/// Characters `file` returns at most.
const FILE_CHARS: usize = 30_000;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Action {
    Recent,
    Get,
    File,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// recent: only this client: secretary-code (the Mac app), ios, android, web or server.
    app: Option<String>,
    /// recent: only crash, fatal_error, error, user_report or hang.
    kind: Option<String>,
    /// recent: only reports seen since "30m", "24h", "7d" or a date YYYY-MM-DD.
    since: Option<String>,
    /// get/file: the report id (dg_...) from recent.
    id: Option<String>,
    /// file: the attachment name from get, e.g. "main.log".
    name: Option<String>,
    /// file: read from the start instead of the end.
    from_start: Option<bool>,
}

pub struct DiagnosticsTool;

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    let url =
        ctx.env.diagnostics_url.clone().ok_or_else(|| {
            ToolError::new("diagnostics are not configured (LUCINA_DIAGNOSTICS_URL)")
        })?;
    let file = ctx.env.diagnostics_token_file.as_ref().ok_or_else(|| {
        ToolError::new("diagnostics are not configured (LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE)")
    })?;
    let token = std::fs::read_to_string(file)
        .map_err(|_| ToolError::new("diagnostics: service token unreadable"))?
        .trim()
        .to_owned();
    Ok((url.trim_end_matches('/').to_owned(), token))
}

async fn send(req: reqwest::RequestBuilder) -> Result<reqwest::Response, ToolError> {
    let resp = req.send().await.map_err(|e| {
        ToolError::new(format!(
            "lucina-diagnostics unreachable: {}",
            e.without_url()
        ))
    })?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    Err(ToolError::new(format!(
        "lucina-diagnostics answered {}: {message}",
        status.as_u16()
    )))
}

async fn json_of(req: reqwest::RequestBuilder) -> Result<Value, ToolError> {
    send(req)
        .await?
        .json()
        .await
        .map_err(|_| ToolError::new("lucina-diagnostics: unreadable answer"))
}

fn valid_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("dg_")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 100
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn local_time(v: &Value, ctx: &Ctx) -> Value {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| {
            json!(t
                .with_timezone(&ctx.env.time_zone)
                .format("%Y-%m-%d %H:%M")
                .to_string())
        })
        .unwrap_or(Value::Null)
}

fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_owned()
    } else {
        s.chars().skip(n - max).collect()
    }
}

/// One report of a list, compact for the model.
fn compact(r: &Value, ctx: &Ctx) -> Value {
    json!({
        "id": r["id"],
        "app": r["app"],
        "kind": r["kind"],
        "version": r["app_version"],
        "os": r["os"],
        "occurred": local_time(&r["occurred_at"], ctx),
        "times": r["count"],
        "lastSeen": local_time(&r["last_seen_at"], ctx),
        "message": r["message"],
        "topFrame": r["top_frame"],
        "attachments": r["attachments"],
    })
}

#[async_trait::async_trait]
impl Tool for DiagnosticsTool {
    fn name(&self) -> &'static str {
        "diagnostics"
    }
    fn description(&self) -> &'static str {
        "Crash reports, errors and hangs from Giuseppe's Lucina apps (Secretary Code on the Mac, the iOS and \
         Android apps, the web app, the servers), with stack, context and logs. recent: newest first, \
         optionally by app/kind/since; get: one report plus the end of its log; file: read a text attachment. \
         Use it when he asks why something crashed or failed, then explain plainly: what crashed and when \
         (e.g. \"Secretary Code crashed at 19:44: the background service timed out\"), the likely cause from \
         the stack and log, and how often it happened. Times are in his time zone."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "diagnostics {} {}",
            args["action"].as_str().unwrap_or(""),
            args["id"].as_str().or(args["app"].as_str()).unwrap_or("")
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
                    "id: a report id like dg_0123456789abcdef (from recent)",
                ))
            }
        };
        match a.action {
            Action::Recent => {
                let mut q: Vec<(&str, String)> = vec![("limit", "15".into())];
                for (k, v) in [("app", &a.app), ("kind", &a.kind), ("since", &a.since)] {
                    if let Some(v) = v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        q.push((k, v.to_owned()));
                    }
                }
                let body = json_of(
                    http.get(format!("{base}/v1/diagnostics/reports"))
                        .bearer_auth(&token)
                        .query(&q),
                )
                .await?;
                let reports: Vec<Value> = body["reports"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| compact(r, ctx))
                    .collect();
                Ok(json!({"reports": reports, "timeZone": ctx.env.time_zone.name()}))
            }
            Action::Get => {
                let id = id()?;
                let d = json_of(
                    http.get(format!("{base}/v1/diagnostics/reports/{id}"))
                        .bearer_auth(&token),
                )
                .await?;
                let atts = d["attachments"].as_array().cloned().unwrap_or_default();
                let is_text = |x: &&Value| x["text"] == Value::Bool(true);
                let log = atts
                    .iter()
                    .filter(is_text)
                    .find(|x| {
                        x["name"]
                            .as_str()
                            .is_some_and(|n| n.to_ascii_lowercase().contains("log"))
                    })
                    .or_else(|| atts.iter().find(is_text))
                    .and_then(|x| x["name"].as_str())
                    .map(str::to_owned);
                let mut log_tail = Value::Null;
                if let Some(name) = &log {
                    if let Ok(r) = send(
                        http.get(format!("{base}/v1/diagnostics/reports/{id}/files/{name}"))
                            .bearer_auth(&token)
                            .query(&[("tail", (LOG_CHARS * 4).to_string())]),
                    )
                    .await
                    {
                        if let Ok(t) = r.text().await {
                            log_tail = json!({"name": name, "end": tail(&t, LOG_CHARS)});
                        }
                    }
                }
                let r = &d["report"];
                let stack: String = r["stack"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(6000)
                    .collect();
                Ok(json!({
                    "id": r["id"],
                    "app": r["app"],
                    "kind": r["kind"],
                    "version": r["app_version"],
                    "os": r["os"],
                    "device": r["device"],
                    "occurred": local_time(&r["occurred_at"], ctx),
                    "times": d["count"],
                    "lastSeen": local_time(&d["last_seen_at"], ctx),
                    "message": r["message"],
                    "stack": stack,
                    "context": r["context"],
                    "attachments": atts.iter().map(|x| json!({"name": x["name"], "size": x["size"], "text": x["text"]})).collect::<Vec<_>>(),
                    "log": log_tail,
                    "timeZone": ctx.env.time_zone.name(),
                }))
            }
            Action::File => {
                let id = id()?;
                let name = a.name.as_deref().map(str::trim).unwrap_or_default();
                if !valid_name(name) {
                    return Err(ToolError::new("name: an attachment name from get"));
                }
                let from_start = a.from_start.unwrap_or(false);
                let resp = send(
                    http.get(format!("{base}/v1/diagnostics/reports/{id}/files/{name}"))
                        .bearer_auth(&token)
                        .query(&[(
                            if from_start { "head" } else { "tail" },
                            (FILE_CHARS * 4).to_string(),
                        )]),
                )
                .await?;
                let full = resp
                    .headers()
                    .get("x-full-size")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let t = resp
                    .text()
                    .await
                    .map_err(|_| ToolError::new("attachment unreadable"))?;
                let text = if from_start {
                    t.chars().take(FILE_CHARS).collect()
                } else {
                    tail(&t, FILE_CHARS)
                };
                Ok(json!({"name": name, "fullBytes": full, "fromStart": from_start, "text": text}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_names_and_schema() {
        assert!(valid_id("dg_0123456789abcdef"));
        assert!(!valid_id("dg_../../etc/passw"));
        assert!(valid_name("main.log"));
        assert!(!valid_name("../index.sqlite"));
        let schema = DiagnosticsTool.parameters();
        assert_eq!(
            schema["properties"]["action"]["enum"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(tail("abcdef", 2), "ef");
    }
}

//! `diagnostics`: crash reports and logs from the owner's clients
//! (lucina-diagnostics on nezopt, `/v1/diagnostics`, SecretaryIOS
//! `docs/diagnostics.md`): Secretary Code on the Mac, the iPhone, Android and
//! web apps, the servers.
//!
//! - `recent`: the latest reports (app, kind, since), most recently seen first.
//! - `get`: one report (message, stack, context, attachments) and the end of
//!   its main log, so Lucina can say "Secretary Code crashed at 19:44: the
//!   background service timed out; here is why".
//! - `file`: a text attachment, cut.
//!
//! Read only. Secrets were removed by the service before storage.
//!
//! Configuration: `LUCINA_DIAGNOSTICS_URL` (default `http://127.0.0.1:8850`)
//! and `LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE` (read per call).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Log characters `get` and `file` return.
const LOG_CHARS: usize = 8_000;
const FILE_CHARS: usize = 16_000;
const STACK_CHARS: usize = 4_000;

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
    /// recent: only this client: secretary-code (the Mac app), ios, android, web, server (comma-separated for several).
    app: Option<String>,
    /// recent: only these kinds: crash, fatal_error, error, user_report, hang (comma-separated).
    kind: Option<String>,
    /// recent: only reports seen since, e.g. "24h", "7d" or a date YYYY-MM-DD.
    since: Option<String>,
    /// get/file: the report id (dg_...) from recent or a crash notification.
    id: Option<String>,
    /// file: the attachment name from get, e.g. "main.log".
    name: Option<String>,
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
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    Err(ToolError::new(format!(
        "lucina-diagnostics answered {}: {message}",
        status.as_u16()
    )))
}

fn valid_id(id: &str) -> bool {
    id.len() == 23 && id.starts_with("dg_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 120
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The last `max` characters, from a line start when cut.
fn tail(s: &str, max: usize) -> (String, bool) {
    let n = s.chars().count();
    if n <= max {
        return (s.to_owned(), false);
    }
    let t: String = s.chars().skip(n - max).collect();
    let t = match t.find('\n') {
        Some(i) if i + 1 < t.len() => t[i + 1..].to_owned(),
        _ => t,
    };
    (t, true)
}

async fn text_file(
    ctx: &Ctx,
    base: &str,
    token: &str,
    id: &str,
    name: &str,
    max: usize,
) -> Result<Value, ToolError> {
    let resp = send(
        ctx.env
            .http
            .get(format!("{base}/v1/diagnostics/reports/{id}/files/{name}"))
            .bearer_auth(token)
            .query(&[("tail", (max * 4).to_string())]),
    )
    .await?;
    let text_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/"));
    if !text_type {
        return Err(ToolError::new(format!(
            "{name} is binary (e.g. a minidump) and cannot be read here"
        )));
    }
    let size = resp
        .headers()
        .get("x-diagnostics-size")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let body = resp
        .text()
        .await
        .map_err(|_| ToolError::new("download cut off"))?;
    let (text, cut) = tail(&body, max);
    let cut = cut || size.is_some_and(|s| s > body.len() as u64);
    Ok(json!({"name": name, "bytes": size, "endOnly": cut, "text": text}))
}

#[async_trait::async_trait]
impl Tool for DiagnosticsTool {
    fn name(&self) -> &'static str {
        "diagnostics"
    }
    fn description(&self) -> &'static str {
        "Crash reports, errors, hangs and problem reports from Giuseppe's apps (Secretary Code on the Mac, \
         the iPhone, Android and web apps) and servers, with their logs. Use it when he says something broke \
         or crashed, or after a crash notification: recent lists them (filter by app, kind, since), get reads \
         one (what failed, when, version, device, stack, the end of its log), file reads a log attachment. \
         Explain in plain words what failed and why (\"Secretary Code crashed at 19:44: the background \
         service timed out because ...\"), with the time in his zone, instead of asking for screenshots."
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
                    "id: a report id like dg_0123456789abcdef0123 (from recent)",
                ))
            }
        };
        match a.action {
            Action::Recent => {
                let mut q: Vec<(&str, String)> = vec![("limit", "20".into())];
                for (k, v) in [("app", &a.app), ("kind", &a.kind), ("since", &a.since)] {
                    if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                        q.push((k, v.to_owned()));
                    }
                }
                let body = send(
                    http.get(format!("{base}/v1/diagnostics/reports"))
                        .bearer_auth(&token)
                        .query(&q),
                )
                .await?
                .json::<Value>()
                .await
                .unwrap_or(Value::Null);
                Ok(json!({
                    "reports": body["reports"],
                    "timeZone": ctx.env.time_zone.name(),
                    "note": "times are UTC; say them in the owner's time zone",
                }))
            }
            Action::Get => {
                let id = id()?;
                let d = send(
                    http.get(format!("{base}/v1/diagnostics/reports/{id}"))
                        .bearer_auth(&token),
                )
                .await?
                .json::<Value>()
                .await
                .unwrap_or(Value::Null);
                let r = &d["report"];
                let files = d["attachments"].as_array().cloned().unwrap_or_default();
                let log = files
                    .iter()
                    .filter(|f| f["text"] == true)
                    .find(|f| f["name"].as_str().is_some_and(|n| n.contains("log")))
                    .or_else(|| files.iter().find(|f| f["text"] == true))
                    .and_then(|f| f["name"].as_str())
                    .map(str::to_owned);
                let log_tail = match log {
                    Some(n) => text_file(ctx, &base, &token, &id, &n, LOG_CHARS).await.ok(),
                    None => None,
                };
                let (stack, _) = tail(
                    &r["stack"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(STACK_CHARS)
                        .collect::<String>(),
                    STACK_CHARS,
                );
                Ok(json!({
                    "report": {
                        "id": r["id"], "app": r["app"], "appVersion": r["app_version"], "os": r["os"],
                        "device": r["device"], "kind": r["kind"], "message": r["message"], "stack": stack,
                        "context": r["context"], "occurredAt": r["occurred_at"], "lastSeenAt": r["last_seen_at"],
                        "count": r["count"], "recent": r["recent"],
                    },
                    "attachments": files.iter().map(|f| json!({"name": f["name"], "kind": f["kind"], "bytes": f["bytes"], "text": f["text"]})).collect::<Vec<_>>(),
                    "logTail": log_tail,
                    "timeZone": ctx.env.time_zone.name(),
                }))
            }
            Action::File => {
                let id = id()?;
                let name = a.name.as_deref().map(str::trim).unwrap_or_default();
                if !safe_name(name) {
                    return Err(ToolError::new(
                        "name: an attachment name from get, e.g. main.log",
                    ));
                }
                text_file(ctx, &base, &token, &id, name, FILE_CHARS).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_names_and_schema() {
        assert!(valid_id("dg_0123456789abcdef0123"));
        assert!(!valid_id("dg_../../etc"));
        assert!(safe_name("main.log") && !safe_name("../x") && !safe_name("a/b"));
        assert_eq!(tail("a\nbb\nccc\n", 6), ("ccc\n".to_owned(), true));
        let schema = DiagnosticsTool.parameters();
        assert_eq!(
            schema["properties"]["action"]["enum"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }
}

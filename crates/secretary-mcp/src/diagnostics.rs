//! The `diagnostics_*` tools: crash reports and logs from every Lucina client,
//! read from lucina-diagnostics on nezopt (`/v1/diagnostics`, SecretaryIOS
//! `docs/diagnostics.md`) with its service token.
//!
//! - `diagnostics_recent`: the latest reports (filters: app, kind, since, limit).
//! - `diagnostics_get`: one report (message, stack, context, attachments) and the
//!   tail of its main log.
//! - `diagnostics_file`: a text attachment, cut to a size a model can read.
//!
//! Configuration: `SECRETARY_MCP_DIAGNOSTICS_URL` (e.g. `http://100.107.164.33:8850`)
//! and `SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE`. Reports are scrubbed of secrets
//! by the service before they are stored.

use std::time::Duration;

use chrono_tz::Tz;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::backend::seg;
use crate::format::local_time;
use crate::tools::{ToolError, ToolResult};
use crate::App;

const APPS: [&str; 5] = ["secretary-code", "ios", "android", "web", "server"];
const KINDS: [&str; 5] = ["crash", "fatal_error", "error", "user_report", "hang"];
const STACK_LINES: usize = 40;
const CONTEXT_CHARS: usize = 3000;

/// Arguments of `diagnostics_recent`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecentArgs {
    /// Only this client: secretary-code (the Mac app), ios, android, web or server. Several: comma-separated.
    #[serde(default)]
    pub app: Option<String>,
    /// Only these kinds: crash, fatal_error, error, user_report, hang (comma-separated).
    #[serde(default)]
    pub kind: Option<String>,
    /// Only reports seen since: a span like "30m", "24h", "7d", a date YYYY-MM-DD (owner's zone) or an RFC 3339 time. Default: all kept reports.
    #[serde(default)]
    pub since: Option<String>,
    /// How many (1-100, default 20), most recently seen first.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `diagnostics_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// The report id (dg_…), from diagnostics_recent or a crash notification.
    pub id: String,
    /// Lines of the main log's end to include (0-400, default 80).
    #[serde(default)]
    pub log_lines: Option<u32>,
}

/// Arguments of `diagnostics_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    /// The report id (dg_…).
    pub id: String,
    /// The attachment name as diagnostics_get lists it, e.g. "main.log".
    pub name: String,
    /// "tail" (default: the end, where the error usually is) or "head".
    #[serde(default)]
    pub from: Option<String>,
    /// Characters to return (1000-60000, default 20000).
    #[serde(default)]
    pub max_chars: Option<u32>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn valid_id(id: &str) -> bool {
    id.len() == 23 && id.starts_with("dg_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

impl Client {
    pub fn new(base: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("secretary-mcp/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    async fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<reqwest::Response, ToolError> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await
            .map_err(|e| {
                bad(format!(
                    "Lucina diagnostics is not reachable right now ({}). Try again later.",
                    if e.is_timeout() {
                        "timeout".to_owned()
                    } else {
                        e.without_url().to_string()
                    }
                ))
            })?;
        if r.status().is_success() {
            return Ok(r);
        }
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        let msg = v["error"]["message"].as_str().unwrap_or("error").to_owned();
        Err(bad(match status {
            404 => "No such diagnostics report or file (reports expire after the retention period, 90 days by default).".to_owned(),
            400 => format!("Lucina diagnostics refused the request: {msg}"),
            _ => format!("Lucina diagnostics answered HTTP {status}: {msg}"),
        }))
    }

    async fn json(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
        self.get(path, query)
            .await?
            .json()
            .await
            .map_err(|_| bad("Lucina diagnostics sent an unreadable answer."))
    }

    /// A text attachment: (text, size of the whole file, binary?).
    async fn text(
        &self,
        id: &str,
        name: &str,
        from: &str,
        bytes: u64,
    ) -> Result<(String, u64, bool), ToolError> {
        let path = format!("/v1/diagnostics/reports/{}/files/{}", seg(id), seg(name));
        let r = self.get(&path, &[(from, bytes.to_string())]).await?;
        let size = r
            .headers()
            .get("x-diagnostics-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let binary = !r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/"));
        let body = r.bytes().await.map_err(|_| bad("download cut off"))?;
        Ok((String::from_utf8_lossy(&body).into_owned(), size, binary))
    }
}

fn label(app: &str) -> &str {
    match app {
        "secretary-code" => "Secretary Code",
        "ios" => "iPhone app",
        "android" => "Android app",
        "web" => "web app",
        "server" => "server",
        other => other,
    }
}

fn check_list(field: &str, v: Option<&str>, allowed: &[&str]) -> Result<Option<String>, ToolError> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let parts: Vec<String> = v
        .split(',')
        .map(|p| p.trim().to_lowercase().replace(' ', "_"))
        .filter(|p| !p.is_empty())
        .collect();
    for p in &parts {
        if !allowed.contains(&p.as_str()) {
            return Err(bad(format!(
                "`{field}` must be one of {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(Some(parts.join(",")))
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / 1048576.0),
        n if n >= 1024 => format!("{} KB", n / 1024),
        n => format!("{n} B"),
    }
}

fn clip_chars(s: &str, max: usize, keep_end: bool) -> (String, bool) {
    let n = s.chars().count();
    if n <= max {
        return (s.to_owned(), false);
    }
    let out = if keep_end {
        s.chars().skip(n - max).collect()
    } else {
        s.chars().take(max).collect()
    };
    (out, true)
}

/// One list row, two lines.
fn row(r: &Value, tz: Tz) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("");
    let mut head = format!(
        "- {} · {} {} · {} · {}",
        s("id"),
        label(s("app")),
        s("app_version"),
        s("kind"),
        local_time(r.get("occurred_at"), tz)
    );
    let count = r["count"].as_u64().unwrap_or(1);
    if count > 1 {
        head.push_str(&format!(
            " (×{count}, last {})",
            local_time(r.get("last_seen_at"), tz)
        ));
    }
    for k in ["os", "device"] {
        if !s(k).is_empty() {
            head.push_str(&format!(" · {}", s(k)));
        }
    }
    if let Some(n) = r["attachments"].as_u64().filter(|n| *n > 0) {
        head.push_str(&format!(" · {n} file{}", if n == 1 { "" } else { "s" }));
    }
    format!("{head}\n  {}", s("message"))
}

pub fn format_list(v: &Value, tz: Tz) -> String {
    let rows = v["reports"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "No diagnostics reports match.".into();
    }
    let mut out = format!(
        "{} report{} (most recently seen first; times in {}):\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        tz.name()
    );
    for r in &rows {
        out.push_str(&row(r, tz));
        out.push('\n');
    }
    out.push_str("Details: diagnostics_get with the id.");
    out
}

fn verb(kind: &str) -> &'static str {
    match kind {
        "crash" => "crashed",
        "fatal_error" => "hit a fatal error",
        "hang" => "hung",
        "user_report" => "problem reported by the owner",
        _ => "error",
    }
}

/// The report for a model: what failed, where, the evidence.
pub fn format_report(d: &Value, tz: Tz, log: Option<(&str, &str, bool)>) -> String {
    let r = &d["report"];
    let s = |k: &str| r[k].as_str().unwrap_or("");
    let mut out = format!(
        "{} {} {}: {} — report {}\n",
        label(s("app")),
        s("app_version"),
        verb(s("kind")),
        local_time(r.get("occurred_at"), tz),
        s("id")
    );
    let mut meta = vec![format!("kind {}", s("kind"))];
    for (k, l) in [("os", "os"), ("device", "device")] {
        if !s(k).is_empty() {
            meta.push(format!("{l} {}", s(k)));
        }
    }
    let count = r["count"].as_u64().unwrap_or(1);
    meta.push(if count > 1 {
        format!(
            "seen {count} times (last {})",
            local_time(r.get("last_seen_at"), tz)
        )
    } else {
        "seen once".into()
    });
    meta.push(format!("fingerprint {}", s("fingerprint")));
    out.push_str(&meta.join(" · "));
    out.push_str(&format!(" · times in {}\n", tz.name()));
    out.push_str(&format!("\nMessage:\n{}\n", s("message")));
    let stack: Vec<&str> = s("stack").lines().collect();
    if !stack.is_empty() {
        out.push_str(&format!(
            "\nStack (top {} of {} lines):\n",
            stack.len().min(STACK_LINES),
            stack.len()
        ));
        for l in stack.iter().take(STACK_LINES) {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !r["context"].is_null() {
        let c = serde_json::to_string_pretty(&r["context"]).unwrap_or_default();
        let (c, cut) = clip_chars(&c, CONTEXT_CHARS, false);
        out.push_str(&format!(
            "\nContext:\n{c}{}\n",
            if cut { "\n[…context cut]" } else { "" }
        ));
    }
    if let Some(recent) = r["recent"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("\nLatest repeats:\n");
        for o in recent.iter().rev().take(5) {
            out.push_str(&format!(
                "- {} · {} · {} · {}\n",
                local_time(o.get("occurred_at"), tz),
                o["app_version"].as_str().unwrap_or(""),
                o["os"].as_str().unwrap_or(""),
                o["device"].as_str().unwrap_or("")
            ));
        }
    }
    let files = d["attachments"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        out.push_str("\nNo attachments.\n");
    } else {
        out.push_str("\nAttachments:\n");
        for f in &files {
            out.push_str(&format!(
                "- {} ({}, {}{})\n",
                f["name"].as_str().unwrap_or(""),
                f["kind"].as_str().unwrap_or(""),
                size(f["bytes"].as_u64().unwrap_or(0)),
                if f["text"] == true {
                    "; read it with diagnostics_file"
                } else {
                    "; binary, not readable here"
                }
            ));
        }
    }
    if let Some((name, text, cut)) = log {
        out.push_str(&format!(
            "\nEnd of {name}{}:\n{text}",
            if cut { " (earlier lines left out)" } else { "" }
        ));
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    let redactions = r["redactions"].as_u64().unwrap_or(0);
    if redactions > 0 {
        out.push_str(&format!(
            "\n({redactions} secrets were replaced with [redacted] before storage.)\n"
        ));
    }
    out
}

/// The log `diagnostics_get` shows the end of: a text file named like a log first.
fn main_log(d: &Value) -> Option<String> {
    let files = d["attachments"].as_array()?;
    let text: Vec<&Value> = files.iter().filter(|f| f["text"] == true).collect();
    text.iter()
        .find(|f| f["name"].as_str().is_some_and(|n| n.contains("log")))
        .or(text.first())
        .and_then(|f| f["name"].as_str())
        .map(str::to_owned)
}

fn client(app: &App) -> Result<&Client, ToolError> {
    app.diagnostics.as_ref().ok_or_else(|| {
        bad("Diagnostics are not configured on this MCP server (SECRETARY_MCP_DIAGNOSTICS_URL and SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE).")
    })
}

pub async fn run(app: &App, name: &str, args: &JsonObject) -> ToolResult {
    let tz = app.cfg.timezone;
    match name {
        "diagnostics_recent" => {
            let a: RecentArgs = parse(args)?;
            let c = client(app)?;
            let mut q = vec![("limit", a.limit.unwrap_or(20).clamp(1, 100).to_string())];
            if let Some(v) = check_list("app", a.app.as_deref(), &APPS)? {
                q.push(("app", v));
            }
            if let Some(v) = check_list("kind", a.kind.as_deref(), &KINDS)? {
                q.push(("kind", v));
            }
            if let Some(s) = a.since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if s.len() > 40 {
                    return Err(bad("`since` is too long"));
                }
                q.push(("since", s.to_owned()));
            }
            Ok(format_list(
                &c.json("/v1/diagnostics/reports", &q).await?,
                tz,
            ))
        }
        "diagnostics_get" => {
            let a: GetArgs = parse(args)?;
            let id = a.id.trim();
            if !valid_id(id) {
                return Err(bad("`id` must be a report id like dg_0123456789abcdef0123 (from diagnostics_recent)"));
            }
            let c = client(app)?;
            let d = c
                .json(&format!("/v1/diagnostics/reports/{}", seg(id)), &[])
                .await?;
            let lines = a.log_lines.unwrap_or(80).min(400) as usize;
            let mut tail = None;
            if lines > 0 {
                if let Some(name) = main_log(&d) {
                    // ~200 bytes a line, at most 64 KiB.
                    let want = (lines as u64 * 200).min(64 * 1024);
                    if let Ok((text, total, false)) = c.text(id, &name, "tail", want).await {
                        let all: Vec<&str> = text.lines().collect();
                        let keep = &all[all.len().saturating_sub(lines)..];
                        let cut = keep.len() < all.len() || (text.len() as u64) < total;
                        tail = Some((name, keep.join("\n"), cut));
                    }
                }
            }
            Ok(format_report(
                &d,
                tz,
                tail.as_ref().map(|(n, t, c)| (n.as_str(), t.as_str(), *c)),
            ))
        }
        "diagnostics_file" => {
            let a: FileArgs = parse(args)?;
            let id = a.id.trim();
            if !valid_id(id) {
                return Err(bad("`id` must be a report id like dg_0123456789abcdef0123"));
            }
            let name = a.name.trim();
            if name.is_empty() || name.len() > 120 || name.contains(['/', '\\']) {
                return Err(bad(
                    "`name` must be an attachment name from diagnostics_get",
                ));
            }
            let from = match a.from.as_deref().map(str::trim).unwrap_or("tail") {
                "tail" | "end" | "" => "tail",
                "head" | "start" => "head",
                _ => return Err(bad("`from` must be \"tail\" or \"head\"")),
            };
            let max = a.max_chars.unwrap_or(20_000).clamp(1000, 60_000) as usize;
            let c = client(app)?;
            let (text, total, binary) = c.text(id, name, from, max as u64 * 4).await?;
            if binary {
                return Err(bad(format!(
                    "{name} is a binary file ({}); it cannot be shown as text. Open it from the report directory on nezopt.",
                    size(total)
                )));
            }
            let (text, cut) = clip_chars(&text, max, from == "tail");
            let partial = cut || (text.len() as u64) < total;
            let mut out = if partial {
                format!(
                    "{name} ({}), {} {} characters:\n",
                    size(total),
                    if from == "tail" { "last" } else { "first" },
                    text.chars().count()
                )
            } else {
                format!("{name} ({}), whole file:\n", size(total))
            };
            out.push_str(&text);
            Ok(out)
        }
        _ => Err(bad(format!("unknown tool {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_and_ids() {
        assert_eq!(
            check_list("kind", Some("crash, fatal error"), &KINDS)
                .unwrap()
                .as_deref(),
            Some("crash,fatal_error")
        );
        assert!(check_list("app", Some("fridge"), &APPS).is_err());
        assert!(valid_id("dg_0123456789abcdef0123"));
        assert!(!valid_id("dg_../x"));
        assert_eq!(clip_chars("abcdef", 3, true), ("def".to_owned(), true));
    }

    #[test]
    fn report_text() {
        let d = json!({"report": {
            "id": "dg_0123456789abcdef0123", "app": "secretary-code", "app_version": "1.4.2", "os": "macOS 15.1",
            "device": "nez-mbp", "kind": "crash", "message": "Background service timed out",
            "stack": "Error: x\n    at startSidecar (index.js:1:2)", "context": {"window": "main"},
            "occurred_at": "2026-10-26T18:44:00Z", "last_seen_at": "2026-10-26T19:10:00Z",
            "fingerprint": "ab12", "count": 3, "redactions": 1,
            "recent": [{"occurred_at": "2026-10-26T19:10:00Z", "app_version": "1.4.2", "os": "macOS 15.1", "device": "nez-mbp"}]},
            "attachments": [{"name": "main.log", "kind": "log", "bytes": 2048, "text": true}]});
        let t = format_report(
            &d,
            chrono_tz::Europe::Copenhagen,
            Some(("main.log", "ERROR sidecar timeout", false)),
        );
        assert!(
            t.starts_with(
                "Secretary Code 1.4.2 crashed: 2026-10-26 19:44 — report dg_0123456789abcdef0123\n"
            ),
            "{t}"
        );
        assert!(t.contains("seen 3 times (last 2026-10-26 20:10)"), "{t}");
        assert!(
            t.contains("- main.log (log, 2 KB; read it with diagnostics_file)"),
            "{t}"
        );
        assert!(
            t.contains("End of main.log:\nERROR sidecar timeout\n"),
            "{t}"
        );
        assert_eq!(main_log(&d).as_deref(), Some("main.log"));
    }
}

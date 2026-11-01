//! Crash reports and logs from every Lucina client: `diagnostics_recent`,
//! `diagnostics_get`, `diagnostics_file` against lucina-diagnostics on
//! nezopt (`/v1/diagnostics/reports*`, SecretaryIOS `docs/diagnostics.md`),
//! with its service token (`SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE`).
//!
//! Everything the service stores was scrubbed of secrets on arrival; binary
//! attachments (minidumps) are never returned here. The tools are listed only
//! when `SECRETARY_MCP_DIAGNOSTICS_URL` is set.

use std::time::Duration;

use chrono::DateTime;
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::tools::{ToolError, ToolResult};

/// Characters of a log a `diagnostics_get` shows by default.
pub const GET_LOG_CHARS: usize = 6_000;
/// Characters a `diagnostics_file` returns at most.
pub const FILE_MAX_CHARS: usize = 60_000;
const STACK_CHARS: usize = 6_000;
const CONTEXT_CHARS: usize = 3_000;

#[derive(Clone)]
pub struct Diagnostics {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AppArg {
    SecretaryCode,
    Ios,
    Android,
    Web,
    Server,
}

impl AppArg {
    fn as_str(self) -> &'static str {
        match self {
            AppArg::SecretaryCode => "secretary-code",
            AppArg::Ios => "ios",
            AppArg::Android => "android",
            AppArg::Web => "web",
            AppArg::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KindArg {
    Crash,
    FatalError,
    Error,
    UserReport,
    Hang,
}

impl KindArg {
    fn as_str(self) -> &'static str {
        match self {
            KindArg::Crash => "crash",
            KindArg::FatalError => "fatal_error",
            KindArg::Error => "error",
            KindArg::UserReport => "user_report",
            KindArg::Hang => "hang",
        }
    }
}

/// Arguments of `diagnostics_recent`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecentArgs {
    /// Only this client.
    #[serde(default)]
    pub app: Option<AppArg>,
    /// Only this kind of report.
    #[serde(default)]
    pub kind: Option<KindArg>,
    /// Only reports seen since: "30m", "24h", "7d", a date YYYY-MM-DD (owner's time zone) or an RFC 3339 time. Default: everything kept (90 days).
    #[serde(default)]
    pub since: Option<String>,
    /// How many, newest first (1-50, default 15).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Pagination cursor: the `next` value printed at the end of the previous page.
    #[serde(default)]
    pub before: Option<String>,
}

/// Arguments of `diagnostics_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// The report id (dg_ and 16 hex digits) from diagnostics_recent.
    pub id: String,
    /// Characters of the end of the report's main log to include (0-20000, default 6000; 0 = none).
    #[serde(default)]
    pub log_tail: Option<u32>,
}

/// Arguments of `diagnostics_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    /// The report id (dg_…).
    pub id: String,
    /// The attachment name, as diagnostics_get lists it, e.g. "main.log".
    pub name: String,
    /// Read from the end of the file (default true: the end of a log is where the failure is); false reads from the start.
    #[serde(default)]
    pub from_end: Option<bool>,
    /// How many characters (1-60000, default 20000).
    #[serde(default)]
    pub max_chars: Option<u32>,
}

fn bad(m: impl Into<String>) -> ToolError {
    ToolError(m.into())
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

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    format!(
        "{}\n[… {} more characters]",
        s.chars().take(max).collect::<String>(),
        s.chars().count() - max
    )
}

/// The last `max` characters.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    s.chars().skip(n - max).collect()
}

fn when(v: &Value, tz: Tz) -> String {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| {
            t.with_timezone(&tz)
                .format("%a %-d %b %Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

fn app_label(app: &str) -> &str {
    match app {
        "secretary-code" => "Secretary Code",
        "ios" => "iOS app",
        "android" => "Android app",
        "web" => "web app",
        "server" => "server",
        other => other,
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

impl Diagnostics {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("secretary-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    async fn request(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<reqwest::Response, ToolError> {
        let token = self.token.as_deref().ok_or_else(|| {
            bad("Diagnostics are not configured (SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE).")
        })?;
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| {
                bad(format!(
                    "The diagnostics service is not reachable right now ({}). Try again later.",
                    if e.is_timeout() {
                        "timeout"
                    } else {
                        "connection failed"
                    }
                ))
            })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let msg = body["error"]["message"].as_str().unwrap_or("");
        Err(bad(match status.as_u16() {
            404 => format!("Not found: {msg}. diagnostics_recent lists the reports."),
            400 => format!("The diagnostics service refused the request: {msg}"),
            s => format!("The diagnostics service answered {s}: {msg}"),
        }))
    }

    async fn json(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
        self.request(path, query)
            .await?
            .json()
            .await
            .map_err(|_| bad("The diagnostics service sent an unreadable answer."))
    }

    /// A text attachment (or its end); returns the text and the file's full size.
    async fn text(
        &self,
        id: &str,
        name: &str,
        from_end: bool,
        bytes: usize,
    ) -> Result<(String, u64), ToolError> {
        let q = [(if from_end { "tail" } else { "head" }, bytes.to_string())];
        let r = self
            .request(&format!("/v1/diagnostics/reports/{id}/files/{name}"), &q)
            .await?;
        let full = r
            .headers()
            .get("x-full-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let t = r
            .text()
            .await
            .map_err(|_| bad("The attachment could not be read."))?;
        Ok((t, full))
    }

    pub async fn recent(&self, a: RecentArgs, tz: Tz) -> ToolResult {
        let limit = a.limit.unwrap_or(15).clamp(1, 50);
        let mut q = vec![("limit", limit.to_string())];
        if let Some(app) = a.app {
            q.push(("app", app.as_str().into()));
        }
        if let Some(k) = a.kind {
            q.push(("kind", k.as_str().into()));
        }
        if let Some(s) = a.since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if s.len() > 40 {
                return Err(bad("`since`: like 24h, 7d, 2026-10-26 or an RFC 3339 time"));
            }
            q.push(("since", s.to_owned()));
        }
        if let Some(b) = a.before.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if b.len() > 40 || !b.bytes().all(|c| c.is_ascii_digit() || c == b'_') {
                return Err(bad("`before`: the `next` value of the previous page"));
            }
            q.push(("before", b.to_owned()));
        }
        let v = self.json("/v1/diagnostics/reports", &q).await?;
        Ok(format_recent(&v, tz))
    }

    pub async fn get(&self, a: GetArgs, tz: Tz) -> ToolResult {
        let id = a.id.trim();
        if !valid_id(id) {
            return Err(bad(
                "`id`: a report id like dg_0123456789abcdef (from diagnostics_recent)",
            ));
        }
        let log_chars = a.log_tail.unwrap_or(GET_LOG_CHARS as u32).min(20_000) as usize;
        let v = self
            .json(&format!("/v1/diagnostics/reports/{id}"), &[])
            .await?;
        let mut out = format_report(&v, tz);
        if log_chars > 0 {
            if let Some(log) = main_log(&v) {
                // Bytes ≥ characters; the service cuts to whole characters.
                match self.text(id, &log, true, log_chars * 4).await {
                    Ok((t, full)) => {
                        let t = tail(&t, log_chars);
                        out.push_str(&format!(
                            "\n\nEnd of {log} (last {} of {full} bytes):\n{t}",
                            t.len().min(full as usize)
                        ));
                    }
                    Err(e) => out.push_str(&format!("\n\n({log} unavailable: {})", e.0)),
                }
            }
        }
        Ok(out)
    }

    pub async fn file(&self, a: FileArgs) -> ToolResult {
        let id = a.id.trim();
        let name = a.name.trim();
        if !valid_id(id) {
            return Err(bad("`id`: a report id like dg_0123456789abcdef"));
        }
        if !valid_name(name) {
            return Err(bad(
                "`name`: an attachment name as diagnostics_get lists it",
            ));
        }
        let max = a
            .max_chars
            .unwrap_or(20_000)
            .clamp(1, FILE_MAX_CHARS as u32) as usize;
        let from_end = a.from_end.unwrap_or(true);
        let meta = self
            .json(&format!("/v1/diagnostics/reports/{id}"), &[])
            .await?;
        let att = meta["attachments"]
            .as_array()
            .and_then(|l| l.iter().find(|x| x["name"] == name))
            .ok_or_else(|| bad(format!("Report {id} has no attachment {name:?}.")))?;
        if att["text"] != Value::Bool(true) {
            return Err(bad(format!(
                "{name} is a binary attachment (e.g. a minidump, {} bytes); it is not readable here. \
                 Download it from GET /v1/diagnostics/reports/{id}/files/{name} and open it with a minidump tool.",
                att["size"]
            )));
        }
        let (t, full) = self.text(id, name, from_end, max * 4).await?;
        let t = if from_end {
            tail(&t, max)
        } else {
            t.chars().take(max).collect()
        };
        let shown = t.len() as u64;
        let header = if shown < full {
            format!(
                "{name} of {id}: {} {shown} of {full} bytes",
                if from_end { "last" } else { "first" }
            )
        } else {
            format!("{name} of {id} ({full} bytes, complete)")
        };
        Ok(format!("{header}\n\n{t}"))
    }
}

/// The attachment `diagnostics_get` shows the end of: the first text file
/// whose name says log, else the first text file.
fn main_log(v: &Value) -> Option<String> {
    let list = v["attachments"].as_array()?;
    let text = |a: &&Value| a["text"] == Value::Bool(true);
    list.iter()
        .filter(text)
        .find(|a| str_of(a, "name").to_ascii_lowercase().contains("log"))
        .or_else(|| list.iter().find(text))
        .map(|a| str_of(a, "name").to_owned())
}

pub fn format_recent(v: &Value, tz: Tz) -> String {
    let reports = v["reports"].as_array().cloned().unwrap_or_default();
    if reports.is_empty() {
        return "No diagnostics reports match.".into();
    }
    let mut out = format!(
        "{} report(s), newest first (times in {}):\n",
        reports.len(),
        tz.name()
    );
    for r in &reports {
        let count = r["count"].as_i64().unwrap_or(1);
        let repeats = if count > 1 {
            format!(" ×{count}, last {}", when(&r["last_seen_at"], tz))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\n- {} · {} {} · {}{repeats} · v{}{}",
            str_of(r, "id"),
            app_label(str_of(r, "app")),
            str_of(r, "kind"),
            when(&r["occurred_at"], tz),
            str_of(r, "app_version"),
            match str_of(r, "os") {
                "" => String::new(),
                os => format!(" · {os}"),
            }
        ));
        out.push_str(&format!(
            "\n  {}",
            clip(str_of(r, "message").lines().next().unwrap_or(""), 300)
        ));
        if let Some(f) = r["top_frame"].as_str().filter(|f| !f.is_empty()) {
            out.push_str(&format!("\n  at {}", clip(f, 200)));
        }
        if let Some(n) = r["attachments"].as_i64().filter(|n| *n > 0) {
            out.push_str(&format!("\n  {n} attachment(s)"));
        }
    }
    if let Some(next) = v["next"].as_str() {
        out.push_str(&format!("\n\nMore: call again with before = \"{next}\"."));
    }
    out.push_str("\n\ndiagnostics_get <id> shows the full report and the end of its log.");
    out
}

pub fn format_report(v: &Value, tz: Tz) -> String {
    let r = &v["report"];
    let count = v["count"].as_i64().unwrap_or(1);
    let mut out = format!(
        "{} {} · {} {} · occurred {} (received {})",
        str_of(r, "id"),
        str_of(r, "kind"),
        app_label(str_of(r, "app")),
        str_of(r, "app_version"),
        when(&r["occurred_at"], tz),
        when(&r["received_at"], tz),
    );
    if count > 1 {
        out.push_str(&format!(
            "\nSeen {count} times; last {} on {}.",
            when(&v["last_seen_at"], tz),
            str_of(v, "last_device")
        ));
    }
    out.push_str(&format!(
        "\nDevice: {}{}",
        str_of(r, "device"),
        match str_of(r, "os") {
            "" => String::new(),
            os => format!(" · {os}"),
        }
    ));
    out.push_str(&format!(
        "\n\nMessage:\n{}",
        clip(str_of(r, "message"), 4000)
    ));
    let stack = str_of(r, "stack");
    if !stack.is_empty() {
        out.push_str(&format!("\n\nStack:\n{}", clip(stack, STACK_CHARS)));
    }
    if r["context"].is_object() && r["context"].as_object().is_some_and(|o| !o.is_empty()) {
        let ctx = serde_json::to_string_pretty(&r["context"]).unwrap_or_default();
        out.push_str(&format!("\n\nContext:\n{}", clip(&ctx, CONTEXT_CHARS)));
    }
    let atts = v["attachments"].as_array().cloned().unwrap_or_default();
    if !atts.is_empty() {
        out.push_str("\n\nAttachments:");
        for a in &atts {
            out.push_str(&format!(
                "\n- {} ({} bytes, {})",
                str_of(a, "name"),
                a["size"],
                if a["text"] == Value::Bool(true) {
                    "text: diagnostics_file reads it"
                } else {
                    "binary, not readable here"
                }
            ));
        }
    }
    if let Some(n) = r["redactions"].as_u64().filter(|n| *n > 0) {
        out.push_str(&format!(
            "\n\n({n} secret(s) were scrubbed before storage.)"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_and_names() {
        assert!(valid_id("dg_0123456789abcdef"));
        assert!(!valid_id("dg_0123456789ABCDEF"));
        assert!(!valid_id("mm_0123456789abcdef"));
        assert!(valid_name("main.log"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(".env"));
        assert!(!valid_name("a b"));
    }

    #[test]
    fn main_log_prefers_logs() {
        let v = json!({"attachments": [
            {"name": "crash.dmp", "text": false},
            {"name": "context.json", "text": true},
            {"name": "main.log", "text": true},
        ]});
        assert_eq!(main_log(&v).as_deref(), Some("main.log"));
        let v = json!({"attachments": [{"name": "crash.dmp", "text": false}]});
        assert_eq!(main_log(&v), None);
    }

    #[test]
    fn tails_and_clips() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
        assert_eq!(clip("abcdef", 3), "abc\n[… 3 more characters]");
    }
}

//! `ops_record`, `ops_search`, `ops_get`: Lucina's record of the owner's
//! projects (secretary-server `/v1/ops/*` → Lucina's `/api/secretary/ops/*`).
//!
//! Every Claude session and coding agent reports deploys, incidents, decisions,
//! status and findings; Lucina writes each record to the Obsidian folder (the
//! owner sees it under `Lucina/`) and to her memory. The MCP names its client
//! in `X-Secretary-Caller` (`mcp:claude`), which Lucina keeps as the reporter.
//!
//! `ops_record` is scope `write`, the two reads scope `read`; an agent-scoped
//! token may call all three too (`crate::tools::also_allowed`), so every coding
//! agent can report. What is cheap to check (slug, title, commit, host, sizes,
//! dates) is checked here so the client gets a precise error without a round
//! trip; Lucina checks everything again.

use chrono::{DateTime, NaiveDate};
use reqwest::Method;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::seg;
use crate::computer::caller_headers;
use crate::format;
use crate::tools::{ToolError, ToolResult, Tools};

/// Characters of a record body shown per search hit.
const SEARCH_BODY: usize = 600;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deploy,
    Incident,
    Decision,
    Status,
    Finding,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Deploy => "deploy",
            Kind::Incident => "incident",
            Kind::Decision => "decision",
            Kind::Status => "status",
            Kind::Finding => "finding",
        }
    }
}

/// How bad an incident is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Arguments of `ops_record` (Lucina's `NewRecord`). Only the fields given are sent.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordArgs {
    /// deploy, incident, decision, status (a piece of work finished) or finding (something worth keeping).
    pub kind: Kind,
    /// The project's short slug: lowercase letters, digits and '-', up to 48, e.g. lucina, lucina-agents, pokoin, orchestra, nezopt, p1.
    pub project: String,
    /// One line saying what happened, up to 200 characters, e.g. "secretary-server 0.9 deployed on p1".
    pub title: String,
    /// The details in Markdown, up to 20000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Related URLs (PRs, dashboards, logs), up to 20 of up to 500 characters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The git commit concerned: 7-40 lowercase hex digits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The host concerned, e.g. nezopt or p1 (letters, digits, '.', '_', '-', up to 64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Incidents: low, medium, high or critical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    /// Incidents: what happened when, one entry per step (up to 50), e.g. "10:05 restarted the service".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<String>,
    /// Incidents: the root cause, up to 4000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// Incidents: what fixed it, up to 4000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// What is left to do, up to 20 entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<String>,
    /// When it happened, RFC 3339 (e.g. 2026-10-28T09:30:00+01:00), when that was not just now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// A key that makes repeats of the same event one record (up to 200 characters), e.g. "deploy:secretary-server:3f2a9c1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
}

/// Arguments of `ops_search`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// Words to look for in titles, bodies, causes and fixes (up to 500 characters).
    #[serde(default)]
    pub q: Option<String>,
    /// Only this project (its slug, e.g. p1).
    #[serde(default)]
    pub project: Option<String>,
    /// Only this kind of record.
    #[serde(default)]
    pub kind: Option<Kind>,
    /// From this day on, YYYY-MM-DD.
    #[serde(default)]
    pub since: Option<String>,
    /// Up to and including this day, YYYY-MM-DD.
    #[serde(default)]
    pub until: Option<String>,
    /// How many records, 1-50 (default: Lucina's).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `ops_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// The record id (16 lowercase hex digits) that ops_record or ops_search returned.
    pub id: String,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

/// `[a-z0-9][a-z0-9-]{0,47}`.
fn slug_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=48).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn id_ok(s: &str) -> bool {
    s.len() == 16 && lower_hex(s)
}

fn max_chars(field: &str, s: &str, max: usize) -> Result<(), ToolError> {
    if s.chars().count() > max {
        return Err(bad(format!("`{field}` is longer than {max} characters")));
    }
    Ok(())
}

fn max_items(field: &str, v: &[String], items: usize, chars: usize) -> Result<(), ToolError> {
    if v.len() > items {
        return Err(bad(format!("`{field}` takes at most {items} entries")));
    }
    for (i, s) in v.iter().enumerate() {
        max_chars(&format!("{field}[{i}]"), s, chars)?;
    }
    Ok(())
}

fn date(field: &str, s: &str) -> Result<(), ToolError> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| bad(format!("`{field}` must be a date YYYY-MM-DD")))
}

/// The checks Lucina would make that need no round trip.
fn check_record(a: &RecordArgs) -> Result<(), ToolError> {
    if !slug_ok(&a.project) {
        return Err(bad(
            "`project` must be a short slug: lowercase letters, digits and '-', starting with a letter or digit, up to 48 (e.g. lucina-agents)",
        ));
    }
    if a.title.trim().is_empty() {
        return Err(bad("`title` must not be empty"));
    }
    max_chars("title", &a.title, 200)?;
    if let Some(b) = &a.body {
        max_chars("body", b, 20_000)?;
    }
    max_items("links", &a.links, 20, 500)?;
    if let Some(c) = &a.commit {
        if !(7..=40).contains(&c.len()) || !lower_hex(c) {
            return Err(bad("`commit` must be 7-40 lowercase hex digits"));
        }
    }
    if let Some(h) = &a.host {
        if !(1..=64).contains(&h.len())
            || !h
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(bad(
                "`host` must be 1-64 letters, digits, '.', '_' or '-' (e.g. nezopt)",
            ));
        }
    }
    max_items("timeline", &a.timeline, 50, 4000)?;
    if let Some(c) = &a.cause {
        max_chars("cause", c, 4000)?;
    }
    if let Some(f) = &a.fix {
        max_chars("fix", f, 4000)?;
    }
    max_items("follow_ups", &a.follow_ups, 20, 4000)?;
    if let Some(at) = &a.at {
        if DateTime::parse_from_rfc3339(at).is_err() {
            return Err(bad(
                "`at` must be an RFC 3339 time with offset, e.g. 2026-10-28T09:30:00+01:00",
            ));
        }
    }
    if let Some(k) = &a.dedup_key {
        max_chars("dedup_key", k, 200)?;
    }
    Ok(())
}

/// A refusal the model can act on.
fn refusal(e: crate::backend::BackendError) -> ToolError {
    match e.code.as_str() {
        "ops_disabled" => {
            bad("Lucina's ops record is switched off on this server; nothing was recorded.")
        }
        "record_not_found" => bad("No record with that id (ops_search finds records)."),
        _ => e.into(),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `Lucina/<project>, Lucina/<log>[, Lucina/<incident>]`: where the owner finds it in Obsidian.
fn paths(p: &Value) -> String {
    ["project", "log", "incident"]
        .iter()
        .filter_map(|k| p.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(|s| format!("Lucina/{s}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one line `ops_record` answers.
fn format_recorded(kind: Kind, project: &str, r: &Value) -> String {
    let mut out = format!(
        "Recorded {} {} for {project}: {}",
        kind.as_str(),
        str_of(r, "id"),
        paths(&r["paths"])
    );
    if r["duplicate"] == true {
        out.push_str(" (duplicate: already recorded)");
    }
    if r["redacted"] == true {
        out.push_str(" (secrets redacted)");
    }
    if r["honcho"] == "pending" {
        out.push_str(" (Honcho: pending)");
    }
    out
}

fn head(v: &Value, tz: chrono_tz::Tz) -> String {
    format!(
        "{} [{}] {}: {} ({})",
        format::local_time(v.get("at"), tz),
        str_of(v, "kind"),
        str_of(v, "project"),
        str_of(v, "title"),
        str_of(v, "id")
    )
}

fn clip(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

/// One block per hit: head line, clipped body, project page.
fn format_search(v: &Value, tz: chrono_tz::Tz) -> String {
    let items = v["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if items.is_empty() {
        return "Nothing in Lucina's record matches.".into();
    }
    items
        .iter()
        .map(|it| {
            let mut block = head(it, tz);
            let body = str_of(it, "body").trim();
            if !body.is_empty() {
                block.push('\n');
                block.push_str(&clip(body, SEARCH_BODY));
            }
            if let Some(p) = it["paths"]["project"].as_str().filter(|p| !p.is_empty()) {
                block.push_str(&format!("\nLucina/{p}"));
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The whole record.
fn format_record(v: &Value, tz: chrono_tz::Tz) -> String {
    let mut out = head(v, tz);
    let facts: Vec<String> = ["severity", "commit", "host", "source"]
        .iter()
        .filter_map(|k| {
            v.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| format!("{k}: {s}"))
        })
        .collect();
    if !facts.is_empty() {
        out.push('\n');
        out.push_str(&facts.join(" · "));
    }
    let body = str_of(v, "body").trim();
    if !body.is_empty() {
        out.push_str("\n\n");
        out.push_str(body);
    }
    let list = |key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| format!("- {s}"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let timeline = list("timeline");
    if !timeline.is_empty() {
        out.push_str(&format!("\n\nTimeline:\n{}", timeline.join("\n")));
    }
    for (key, title) in [("cause", "Cause"), ("fix", "Fix")] {
        let s = str_of(v, key).trim();
        if !s.is_empty() {
            out.push_str(&format!("\n\n{title}: {s}"));
        }
    }
    for (key, title) in [("follow_ups", "Follow-ups"), ("links", "Links")] {
        let l = list(key);
        if !l.is_empty() {
            out.push_str(&format!("\n\n{title}:\n{}", l.join("\n")));
        }
    }
    let p = paths(&v["paths"]);
    if !p.is_empty() {
        out.push_str(&format!("\n\n{p}"));
    }
    out
}

impl Tools<'_> {
    async fn ops_req(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, ToolError> {
        self.app
            .backend
            .send_device_query(method, path, query, body, caller_headers(&self.caller.name))
            .await
            .map_err(refusal)
    }

    /// `ops_record`, `ops_search`, `ops_get`.
    pub async fn ops(&self, name: &str, args: &JsonObject) -> ToolResult {
        let tz = self.app.cfg.timezone;
        match name {
            "ops_record" => {
                let a: RecordArgs = parse(args)?;
                check_record(&a)?;
                let body = serde_json::to_value(&a).map_err(|e| bad(e.to_string()))?;
                let r = self
                    .ops_req(Method::POST, "/v1/ops/records", &[], Some(&body))
                    .await?;
                Ok(format_recorded(a.kind, &a.project, &r))
            }
            "ops_search" => {
                let a: SearchArgs = parse(args)?;
                let mut q: Vec<(&str, String)> = Vec::new();
                if let Some(s) = a.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    max_chars("q", s, 500)?;
                    q.push(("q", s.to_owned()));
                }
                if let Some(p) = a.project.as_deref() {
                    if !slug_ok(p) {
                        return Err(bad("`project` is a project slug, e.g. p1"));
                    }
                    q.push(("project", p.to_owned()));
                }
                if let Some(k) = a.kind {
                    q.push(("kind", k.as_str().to_owned()));
                }
                for (field, d) in [("since", &a.since), ("until", &a.until)] {
                    if let Some(d) = d.as_deref() {
                        date(field, d)?;
                        q.push((field, d.to_owned()));
                    }
                }
                if let Some(n) = a.limit {
                    if !(1..=50).contains(&n) {
                        return Err(bad("`limit` must be 1-50"));
                    }
                    q.push(("limit", n.to_string()));
                }
                let v = self
                    .ops_req(Method::GET, "/v1/ops/search", &q, None)
                    .await?;
                Ok(format_search(&v, tz))
            }
            "ops_get" => {
                let a: GetArgs = parse(args)?;
                if !id_ok(&a.id) {
                    return Err(bad(
                        "`id` is the 16 lowercase hex digits ops_record or ops_search returned",
                    ));
                }
                let v = self
                    .ops_req(
                        Method::GET,
                        &format!("/v1/ops/records/{}", seg(&a.id)),
                        &[],
                        None,
                    )
                    .await?;
                Ok(format_record(&v, tz))
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(v: Value) -> Result<RecordArgs, ToolError> {
        let a: RecordArgs = parse(v.as_object().unwrap())?;
        check_record(&a)?;
        Ok(a)
    }

    #[test]
    fn only_given_fields_are_sent() {
        let a = record(json!({"kind": "deploy", "project": "p1", "title": "secretary-server deployed", "commit": "3f2a9c1"})).unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            json!({"kind": "deploy", "project": "p1", "title": "secretary-server deployed", "commit": "3f2a9c1"})
        );
        let a = record(json!({"kind": "incident", "project": "lucina-agents", "title": "x", "severity": "high",
            "timeline": ["10:00 down"], "follow_ups": ["alerting"], "at": "2026-10-28T09:30:00+01:00"}))
        .unwrap();
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["severity"], "high");
        assert_eq!(v["timeline"], json!(["10:00 down"]));
        assert!(v.get("links").is_none() && v.get("body").is_none());
    }

    #[test]
    fn cheap_checks_fail_before_the_round_trip() {
        let base = json!({"kind": "status", "project": "p1", "title": "done"});
        let with = |k: &str, v: Value| {
            let mut b = base.clone();
            b[k] = v;
            record(b).err().map(|e| e.0).unwrap_or_default()
        };
        for slug in ["P1", "-p1", "", "p_1", &"a".repeat(49)] {
            assert!(with("project", json!(slug)).contains("`project`"), "{slug}");
        }
        assert!(with("project", json!("a".repeat(48))).is_empty());
        for commit in ["3F2A9C1", "3f2a9c", "xyzxyzx", &"a".repeat(41)] {
            assert!(
                with("commit", json!(commit)).contains("`commit`"),
                "{commit}"
            );
        }
        assert!(with("title", json!(" ")).contains("`title`"));
        assert!(with("title", json!("t".repeat(201))).contains("200"));
        assert!(with("host", json!("bad host")).contains("`host`"));
        assert!(with("at", json!("yesterday")).contains("RFC 3339"));
        assert!(with("links", json!(vec!["u"; 21])).contains("20 entries"));
        assert!(with("kind", json!("outage")).contains("Invalid arguments"));
        assert!(with("severity", json!("urgent")).contains("Invalid arguments"));
        assert!(with("reporter", json!("me")).contains("unknown field"));
    }

    #[test]
    fn results_read_well() {
        let r = json!({"id": "0123456789abcdef", "duplicate": false, "redacted": false, "honcho": "ok",
            "paths": {"log": "Log/2026-10-28.md", "project": "Projects/p1.md"}});
        assert_eq!(
            format_recorded(Kind::Deploy, "p1", &r),
            "Recorded deploy 0123456789abcdef for p1: Lucina/Projects/p1.md, Lucina/Log/2026-10-28.md"
        );
        let r = json!({"id": "0123456789abcdef", "duplicate": true, "redacted": true, "honcho": "pending",
            "paths": {"log": "Log/2026-10-28.md", "project": "Projects/p1.md", "incident": "Incidents/2026-10-28-down.md"}});
        assert_eq!(
            format_recorded(Kind::Incident, "p1", &r),
            "Recorded incident 0123456789abcdef for p1: Lucina/Projects/p1.md, Lucina/Log/2026-10-28.md, \
             Lucina/Incidents/2026-10-28-down.md (duplicate: already recorded) (secrets redacted) (Honcho: pending)"
        );

        let tz: chrono_tz::Tz = "Europe/Rome".parse().unwrap();
        let hit = json!({"id": "0123456789abcdef", "kind": "deploy", "project": "p1", "title": "went live",
            "body": "b".repeat(700), "at": "2026-10-28T08:30:00Z", "paths": {"project": "Projects/p1.md"}});
        let t = format_search(&json!({"items": [hit], "honcho": "ok"}), tz);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "2026-10-28 09:30 [deploy] p1: went live (0123456789abcdef)"
        );
        assert_eq!(lines[1].chars().count(), SEARCH_BODY + 1);
        assert_eq!(lines[2], "Lucina/Projects/p1.md");
        assert_eq!(
            format_search(&json!({"items": []}), tz),
            "Nothing in Lucina's record matches."
        );

        let full = format_record(
            &json!({"id": "0123456789abcdef", "kind": "incident", "project": "p1", "title": "down",
                "at": "2026-10-28T08:30:00Z", "severity": "high", "cause": "disk full", "fix": "pruned",
                "timeline": ["09:30 alert"], "follow_ups": ["alert at 80%"], "paths": {"project": "Projects/p1.md"}}),
            tz,
        );
        for want in [
            "severity: high",
            "Timeline:\n- 09:30 alert",
            "Cause: disk full",
            "Fix: pruned",
            "Follow-ups:\n- alert at 80%",
            "Lucina/Projects/p1.md",
        ] {
            assert!(full.contains(want), "{want} in {full}");
        }
    }

    #[test]
    fn ids() {
        assert!(id_ok("0123456789abcdef"));
        for bad in ["0123456789ABCDEF", "0123", "../0123456789abc"] {
            assert!(!id_ok(bad), "{bad}");
        }
    }
}

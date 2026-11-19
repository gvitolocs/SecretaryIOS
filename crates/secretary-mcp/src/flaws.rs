//! `flaw_report`, `flaw_search`, `flaw_status`, `flaw_matrix`, `flaw_target`, `flaw_dispatch`:
//! Lucina's tracker of the owner's flaws (secretary-server `/v1/flaws/*` → Lucina's
//! `/api/secretary/flaws/*`). The MCP names its client in `X-Secretary-Caller`, which Lucina keeps
//! as the reporter. Lucina enforces the evidence rule; here it is only explained.

use reqwest::Method;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::backend::seg;
use crate::computer::caller_headers;
use crate::tools::{ToolError, ToolResult, Tools};

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn id_ok(id: &str) -> bool {
    id.strip_prefix("fl-").is_some_and(|h| {
        h.len() == 8
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn check_id(id: &str) -> Result<(), ToolError> {
    if id_ok(id) {
        Ok(())
    } else {
        Err(bad(
            "`id` is `fl-` and 8 lowercase hex digits (flaw_search finds it)",
        ))
    }
}

/// What a client's row can be set to, and which clients exist.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportArgs {
    /// The owner's exact words, up to 8000 characters; never rephrase them.
    pub words: String,
    /// A short title (default: the first line of the words).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// low, medium, high or critical (default low).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Clients where it was seen: web, mac, android, ios, juno.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<String>,
    /// The conversation or thread it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// The flaw whose work turned this one up (`fl-…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_from: Option<String>,
    /// Only when its fix can't be undone or needs the owner: irreversible, credentials or payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// Words in the title or the owner's words.
    #[serde(default)]
    pub q: Option<String>,
    /// open, fixing, resolved, duplicate or dismissed.
    #[serde(default)]
    pub state: Option<String>,
    /// web, mac, android, ios or juno.
    #[serde(default)]
    pub platform: Option<String>,
    /// A client's status: untriaged, affected, fixing, fixed, verified, not_affected, blocked.
    #[serde(default)]
    pub status: Option<String>,
    /// 1-200, default 50.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    /// `fl-` and 8 lowercase hex digits.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// The evidence behind a status.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceArgs {
    /// harness_run, debug_event or screenshot can close a target; code_reading and owner_report only open one.
    pub kind: String,
    /// Where to see it: the log, the event id, the screenshot's path.
    pub reference: String,
    /// The installed build it was seen on (required for harness_run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetArgs {
    /// `fl-` and 8 lowercase hex digits.
    #[serde(skip_serializing)]
    pub id: String,
    /// web, mac, android, ios or juno.
    #[serde(skip_serializing)]
    pub platform: String,
    /// affected, fixing, fixed, verified, not_affected or blocked. verified and not_affected are refused without closing evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// server, core or client. Setting it on an affected target starts its fix by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceArgs>,
    /// The workspace working on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// A session taking it on by hand (`local_…`): the dispatcher leaves it alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The commit that fixes it (7-40 hex digits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchArgs {
    /// `fl-` and 8 lowercase hex digits.
    pub id: String,
    /// web, mac, android, ios or juno.
    pub platform: String,
}

fn refusal(e: crate::backend::BackendError) -> ToolError {
    match e.code.as_str() {
        "flaws_disabled" => bad("Lucina's flaw tracker is switched off on this server; nothing was recorded."),
        "flaw_not_found" => bad("No flaw with that id (flaw_search finds them)."),
        "evidence_required" => bad(format!(
            "Refused: {}. Show it on an installed build, then call flaw_target again with that evidence.",
            e.message
        )),
        _ => e.into(),
    }
}

fn line(f: &Value) -> String {
    format!(
        "{} [{}, {}] {}",
        f["id"].as_str().unwrap_or(""),
        f["state"].as_str().unwrap_or(""),
        f["severity"].as_str().unwrap_or(""),
        f["title"].as_str().unwrap_or("")
    )
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// The matrix as a text table.
fn format_matrix(v: &Value) -> String {
    let platforms: Vec<&str> = v["platforms"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = String::new();
    let rows = v["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return "No live flaws.".into();
    }
    for r in rows {
        out.push_str(&line(r));
        out.push('\n');
        for p in &platforms {
            let c = &r["cells"][*p];
            out.push_str(&format!("  {p}: {}", c["status"].as_str().unwrap_or("-")));
            if let Some(l) = c["layer"].as_str() {
                out.push_str(&format!(" ({l})"));
            }
            if let Some(s) = c["commit"].as_str() {
                out.push_str(&format!(" {}", &s[..s.len().min(10)]));
            }
            if let Some(s) = c["deploy_tag"].as_str() {
                out.push_str(&format!(" {s}"));
            }
            out.push('\n');
        }
    }
    out
}

impl Tools<'_> {
    async fn flaw_req(
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

    pub async fn flaws(&self, name: &str, args: &JsonObject) -> ToolResult {
        match name {
            "flaw_report" => {
                let a: ReportArgs = parse(args)?;
                if a.words.trim().is_empty() {
                    return Err(bad("`words` is the owner's report, in his words"));
                }
                let body = serde_json::to_value(&a).map_err(|e| bad(e.to_string()))?;
                let r = self
                    .flaw_req(Method::POST, "/v1/flaws", &[], Some(&body))
                    .await?;
                let mut out = format!("Recorded {}.", line(&r["flaw"]));
                if let Some(d) = r["possible_duplicates"]
                    .as_array()
                    .filter(|d| !d.is_empty())
                {
                    out.push_str("\nIt may be the same as:");
                    for f in d {
                        out.push_str(&format!("\n- {}", line(f)));
                    }
                }
                Ok(out)
            }
            "flaw_search" => {
                let a: SearchArgs = parse(args)?;
                let mut q: Vec<(&str, String)> = Vec::new();
                for (k, v) in [
                    ("q", &a.q),
                    ("state", &a.state),
                    ("platform", &a.platform),
                    ("status", &a.status),
                ] {
                    if let Some(v) = v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        q.push((k, v.to_owned()));
                    }
                }
                if let Some(n) = a.limit {
                    if !(1..=200).contains(&n) {
                        return Err(bad("`limit` must be 1-200"));
                    }
                    q.push(("limit", n.to_string()));
                }
                let v = self.flaw_req(Method::GET, "/v1/flaws", &q, None).await?;
                let items = v["items"].as_array().map(Vec::as_slice).unwrap_or_default();
                Ok(if items.is_empty() {
                    "No flaws match.".into()
                } else {
                    items.iter().map(line).collect::<Vec<_>>().join("\n")
                })
            }
            "flaw_status" => {
                let a: StatusArgs = parse(args)?;
                check_id(&a.id)?;
                let v = self
                    .flaw_req(Method::GET, &format!("/v1/flaws/{}", seg(&a.id)), &[], None)
                    .await?;
                Ok(pretty(&v))
            }
            "flaw_matrix" => {
                let _: NoArgs = parse(args)?;
                let v = self
                    .flaw_req(Method::GET, "/v1/flaws/matrix", &[], None)
                    .await?;
                Ok(format_matrix(&v))
            }
            "flaw_target" => {
                let a: TargetArgs = parse(args)?;
                check_id(&a.id)?;
                let body = serde_json::to_value(&a).map_err(|e| bad(e.to_string()))?;
                let v = self
                    .flaw_req(
                        Method::PUT,
                        &format!("/v1/flaws/{}/targets/{}", seg(&a.id), seg(&a.platform)),
                        &[],
                        Some(&body),
                    )
                    .await?;
                Ok(pretty(&v))
            }
            "flaw_dispatch" => {
                let a: DispatchArgs = parse(args)?;
                check_id(&a.id)?;
                let body = json!({"platform": a.platform});
                let v = self
                    .flaw_req(
                        Method::POST,
                        &format!("/v1/flaws/{}/dispatch", seg(&a.id)),
                        &[],
                        Some(&body),
                    )
                    .await?;
                Ok(format!(
                    "Started a task on {}: agent {}.",
                    a.platform,
                    v["agent"].as_str().unwrap_or("?")
                ))
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

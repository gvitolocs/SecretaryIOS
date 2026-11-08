//! `flaw`: Lucina's tracker of the small flaws Giuseppe notices across his clients (crate
//! `lucina_flaws`). Owner's turns only: a report can start a coding agent, so outside callers
//! (Lucina MCP) never get it.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use lucina_flaws::{Evidence, EvidenceKind, FlawState, Flaws, Layer, ListQuery, NewFlaw, Platform, Severity, TargetStatus, TargetUpdate};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Record a flaw he noticed, in his exact words.
    Report,
    /// Find flaws by words, state or client.
    Search,
    /// One flaw: every client's state, and its history.
    Status,
    /// Every live flaw against every client.
    Matrix,
    /// Change one client's row: status, layer, evidence.
    Update,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Action,
    /// report: his exact words; never rephrase them.
    #[serde(default)]
    words: Option<String>,
    /// report: a short title (default: the first line of his words).
    #[serde(default)]
    title: Option<String>,
    /// report: low, medium, high or critical (default low).
    #[serde(default)]
    severity: Option<String>,
    /// report: the clients where he saw it: web, mac, android, ios, juno.
    #[serde(default)]
    affected: Vec<String>,
    /// search: words to look for.
    #[serde(default)]
    query: Option<String>,
    /// search: open, fixing, resolved, duplicate or dismissed.
    #[serde(default)]
    state: Option<String>,
    /// status, update: the flaw id (fl- and 8 hex digits).
    #[serde(default)]
    id: Option<String>,
    /// search, update: web, mac, android, ios or juno.
    #[serde(default)]
    platform: Option<String>,
    /// update: affected, fixing, fixed, verified, not_affected or blocked. verified and
    /// not_affected are refused without a harness run on an installed build, a debug event or a
    /// screenshot; what you read or were told never closes one.
    #[serde(default)]
    status: Option<String>,
    /// update: where the fix belongs: server, core or client.
    #[serde(default)]
    layer: Option<String>,
    /// update: harness_run, debug_event, screenshot, code_reading or owner_report.
    #[serde(default)]
    evidence_kind: Option<String>,
    /// update: where to see the evidence (log, event id, path).
    #[serde(default)]
    evidence_ref: Option<String>,
    /// update: the installed build it was seen on (required for a harness run).
    #[serde(default)]
    build: Option<String>,
    /// update: a note for the history.
    #[serde(default)]
    note: Option<String>,
}

pub struct FlawTool;

fn invalid(e: lucina_flaws::FlawError) -> ToolError {
    ToolError::new(e.to_string())
}

fn need<'a>(v: &'a Option<String>, what: &str) -> Result<&'a str, ToolError> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| ToolError::new(format!("this action needs {what}")))
}

/// Record the issue Lucina was asked to pass on, before anything goes out. `None`: the tracker is
/// off or refused it; the message goes out anyway.
pub fn record_issue(flaws: &Flaws, what: &str, owner_words: Option<&str>, thread: &str) -> Option<String> {
    let words = owner_words.map(str::trim).filter(|w| !w.is_empty()).unwrap_or(what);
    let new = NewFlaw { words: words.to_owned(), title: Some(what.to_owned()), thread: Some(thread.to_owned()), ..NewFlaw::default() };
    match flaws.report(new, "lucina") {
        Ok(r) => Some(r.flaw.flaw.id),
        Err(e) => {
            tracing::warn!(error = %e, "flaw: the issue was not recorded");
            None
        }
    }
}

#[async_trait::async_trait]
impl Tool for FlawTool {
    fn name(&self) -> &'static str {
        "flaw"
    }
    fn description(&self) -> &'static str {
        "Lucina's tracker of the small flaws Giuseppe notices in his clients (web, Mac, Android, iPhone, Juno). \
report: record one in his exact words (it then gets triaged and fixed by itself; you tell him the outcome). \
search: has this been reported? status: where one flaw stands on each client, its commits and deploys. \
matrix: every live flaw against every client. update: change a client's row. A row closes only with evidence \
from an installed build: never say something is fixed or verified unless status shows it."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "flaw {} {}",
            args["action"].as_str().unwrap_or(""),
            args["id"].as_str().or(args["query"].as_str()).or(args["title"].as_str()).unwrap_or("").chars().take(60).collect::<String>()
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let flaws = ctx.env.flaws.clone().ok_or_else(|| ToolError::new("the flaw tracker is off on this server"))?;
        match a.action {
            Action::Report => {
                let new = NewFlaw {
                    words: need(&a.words, "words")?.to_owned(),
                    title: a.title,
                    severity: a.severity.as_deref().map(Severity::parse).transpose().map_err(invalid)?,
                    affected: a.affected.iter().map(|p| Platform::parse(p)).collect::<Result<_, _>>().map_err(invalid)?,
                    thread: Some(format!("{}:{}", ctx.channel, ctx.session_id)),
                    ..NewFlaw::default()
                };
                let r = flaws.report(new, "lucina").map_err(invalid)?;
                Ok(json!({
                    "id": r.flaw.flaw.id,
                    "title": r.flaw.flaw.title,
                    "possible_duplicates": r.possible_duplicates.iter().map(|f| json!({"id": f.id, "title": f.title})).collect::<Vec<_>>(),
                }))
            }
            Action::Search => {
                let q = ListQuery {
                    q: a.query,
                    state: a.state.as_deref().map(FlawState::parse).transpose().map_err(invalid)?,
                    platform: a.platform.as_deref().map(Platform::parse).transpose().map_err(invalid)?,
                    status: None,
                    limit: Some(20),
                };
                let items = flaws.list(&q).map_err(invalid)?;
                Ok(json!({"items": items.iter().map(|f| json!({"id": f.id, "title": f.title, "severity": f.severity, "state": f.state})).collect::<Vec<_>>()}))
            }
            Action::Status => {
                let d = flaws.get(need(&a.id, "id")?).map_err(invalid)?;
                Ok(json!(d))
            }
            Action::Matrix => flaws.matrix().map_err(invalid),
            Action::Update => {
                let evidence = match a.evidence_kind.as_deref() {
                    None => None,
                    Some(k) => Some(Evidence {
                        kind: EvidenceKind::parse(k).map_err(invalid)?,
                        reference: a.evidence_ref.clone().unwrap_or_default(),
                        build: a.build.clone(),
                        note: None,
                    }),
                };
                let u = TargetUpdate {
                    status: a.status.as_deref().map(TargetStatus::parse).transpose().map_err(invalid)?,
                    layer: a.layer.as_deref().map(Layer::parse).transpose().map_err(invalid)?,
                    evidence,
                    build: a.build,
                    note: a.note,
                    ..TargetUpdate::default()
                };
                let p = Platform::parse(need(&a.platform, "platform")?).map_err(invalid)?;
                let t = flaws.set_target(need(&a.id, "id")?, p, u, "lucina").map_err(invalid)?;
                Ok(json!(t))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx() -> Ctx {
        let dir = std::env::temp_dir();
        let mut env = crate::ToolEnv::new(&dir, chrono_tz::Europe::Rome);
        env.flaws = Some(Arc::new(Flaws::in_memory().unwrap()));
        Ctx::new(Arc::new(env), "web", "s1")
    }

    #[tokio::test]
    async fn report_status_and_the_evidence_rule() {
        let c = ctx();
        let r = FlawTool.call(json!({"action": "report", "words": "emoji are boxes", "affected": ["juno"]}), &c).await.unwrap();
        let id = r["id"].as_str().unwrap().to_owned();
        let d = FlawTool.call(json!({"action": "status", "id": id}), &c).await.unwrap();
        assert_eq!(d["words"], "emoji are boxes");
        let e = FlawTool
            .call(json!({"action": "update", "id": id, "platform": "web", "status": "not_affected", "evidence_kind": "code_reading", "evidence_ref": "rich.rs:1"}), &c)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("never close"), "{e}");
        let m = FlawTool.call(json!({"action": "matrix"}), &c).await.unwrap();
        assert_eq!(m["rows"][0]["cells"]["juno"]["status"], "affected");
        let s = FlawTool.call(json!({"action": "search", "query": "boxes"}), &c).await.unwrap();
        assert_eq!(s["items"][0]["id"], id.as_str());
    }

    #[test]
    fn an_issue_is_recorded_with_his_words() {
        let f = Flaws::in_memory().unwrap();
        let id = record_issue(&f, "Emoji are boxes", Some("gli emoji sono quadratini"), "rt-1").unwrap();
        let d = f.get(&id).unwrap();
        assert_eq!(d.flaw.words, "gli emoji sono quadratini");
        assert_eq!(d.flaw.title, "Emoji are boxes");
        assert_eq!(d.flaw.thread.as_deref(), Some("rt-1"));
    }
}

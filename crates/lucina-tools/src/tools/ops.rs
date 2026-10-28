//! `ops`: Lucina's record of the owner's projects (crate `lucina_ops`):
//! deploys, incidents, decisions, status and findings, kept in the Obsidian
//! folder `Lucina/` and in Honcho. `record` writes one (source `lucina`),
//! `search` and `get` answer "what happened with X".

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Write one record.
    Record,
    /// Find records by words, project, kind and dates.
    Search,
    /// One record by id.
    Get,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum KindArg {
    Deploy,
    Incident,
    Decision,
    Status,
    Finding,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SeverityArg {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum DecidedByArg {
    Owner,
    Lucina,
    Session,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Action,
    /// record/search: deploy, incident, decision, status or finding.
    #[serde(default)]
    kind: Option<KindArg>,
    /// record (required), search: the project slug, e.g. lucina, lucina-agents, pokoin, orchestra, nezopt.
    #[serde(default)]
    project: Option<String>,
    /// record: one line, what happened or was decided.
    #[serde(default)]
    title: Option<String>,
    /// record: the details in markdown (for a decision: why, and the alternatives).
    #[serde(default)]
    body: Option<String>,
    /// record: URLs, commits, note slugs.
    #[serde(default)]
    links: Vec<String>,
    /// record: the git commit (7-40 hex).
    #[serde(default)]
    commit: Option<String>,
    /// record: the host (nezopt, p1, pi, mac).
    #[serde(default)]
    host: Option<String>,
    /// record (incidents): low, medium, high or critical.
    #[serde(default)]
    severity: Option<SeverityArg>,
    /// record (incidents): "HH:MM what happened" lines.
    #[serde(default)]
    timeline: Vec<String>,
    /// record (incidents): the cause, when known.
    #[serde(default)]
    cause: Option<String>,
    /// record (incidents): the fix, when known.
    #[serde(default)]
    fix: Option<String>,
    /// record: open items left to do (they go to the project's Open questions).
    #[serde(default)]
    follow_ups: Vec<String>,
    /// record: when it happened (RFC 3339), when that was not just now.
    #[serde(default)]
    at: Option<String>,
    /// record (decisions): who decided: owner, lucina (you, for a session that asked) or session.
    #[serde(default)]
    decided_by: Option<DecidedByArg>,
    /// record (decisions): the question that was decided.
    #[serde(default)]
    question: Option<String>,
    /// record (decisions): who asked (the session or agent name).
    #[serde(default)]
    asked_by: Option<String>,
    /// search: words to look for (empty with filters: newest first).
    #[serde(default)]
    query: Option<String>,
    /// search: only records on or after this day (YYYY-MM-DD).
    #[serde(default)]
    since: Option<String>,
    /// search: only records on or before this day (YYYY-MM-DD).
    #[serde(default)]
    until: Option<String>,
    /// search: at most this many (1-20, default 8).
    #[serde(default)]
    limit: Option<u32>,
    /// get: the record id (16 hex).
    #[serde(default)]
    id: Option<String>,
}

pub struct OpsTool;

fn date(v: &Option<String>, field: &str) -> Result<Option<chrono::NaiveDate>, ToolError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ToolError::new(format!("{field} must be YYYY-MM-DD"))),
    }
}

fn record_json(a: &Args) -> Result<Value, ToolError> {
    let kind = a.kind.ok_or_else(|| ToolError::new("record needs kind"))?;
    let mut v = json!({
        "kind": kind,
        "project": a.project.clone().unwrap_or_default(),
        "title": a.title.clone().unwrap_or_default(),
        "body": a.body.clone().unwrap_or_default(),
        "links": a.links,
        "timeline": a.timeline,
        "follow_ups": a.follow_ups,
    });
    let o = v.as_object_mut().expect("object");
    for (k, val) in [
        ("commit", &a.commit),
        ("host", &a.host),
        ("cause", &a.cause),
        ("fix", &a.fix),
        ("at", &a.at),
        ("question", &a.question),
        ("asked_by", &a.asked_by),
    ] {
        if let Some(s) = val.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            o.insert(k.into(), json!(s));
        }
    }
    if let Some(s) = a.severity {
        o.insert("severity".into(), json!(s));
    }
    if let Some(d) = a.decided_by {
        o.insert("decided_by".into(), json!(d));
    }
    Ok(v)
}

#[async_trait::async_trait]
impl Tool for OpsTool {
    fn name(&self) -> &'static str {
        "ops"
    }
    fn description(&self) -> &'static str {
        "Lucina's record of Giuseppe's projects (deploys, incidents, decisions, status, findings), kept in \
         his Obsidian folder Lucina/ (daily log, one page per project, incident pages) and in her memory. \
         record: when Giuseppe decides something about one of his projects in chat (kind decision; title = \
         the decision, body = why and the alternatives, decided_by owner), when you decide something for a \
         session or agent that asked you (decided_by lucina, question = what they asked, asked_by = who), when he reports a deploy or an incident, or when \
         you hit one yourself; repeats are de-duplicated and secrets are redacted. search: to answer \"what \
         happened with X\", \"what changed on X\", \"why did we decide Y\", \"when did Z go live\": answer \
         only from the returned records, citing their dates and pages (Lucina/Projects/<project>.md); never \
         invent a record. get: one record by id."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "ops {} {} {}",
            args["action"].as_str().unwrap_or(""),
            args["project"].as_str().unwrap_or(""),
            args["title"]
                .as_str()
                .or(args["query"].as_str())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect::<String>()
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let ops = ctx
            .env
            .ops
            .clone()
            .ok_or_else(|| ToolError::new("the ops record is off on this server"))?;
        match a.action {
            Action::Record => {
                let new: lucina_ops::NewRecord = serde_json::from_value(record_json(&a)?)
                    .map_err(|e| ToolError::new(format!("invalid record: {e}")))?;
                let r = ops
                    .record(new, "lucina")
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                Ok(json!({
                    "id": r.id,
                    "duplicate": r.duplicate,
                    "redacted": r.redacted,
                    "pages": {
                        "log": format!("Lucina/{}", r.paths.log),
                        "project": format!("Lucina/{}", r.paths.project),
                        "incident": r.paths.incident.map(|p| format!("Lucina/{p}")),
                    },
                }))
            }
            Action::Search => {
                let kind = a
                    .kind
                    .map(|k| serde_json::from_value(json!(k)).expect("same kinds"));
                let q = lucina_ops::SearchQuery {
                    q: a.query.clone().unwrap_or_default(),
                    project: a.project.clone(),
                    kind,
                    since: date(&a.since, "since")?,
                    until: date(&a.until, "until")?,
                    limit: Some(a.limit.unwrap_or(8).clamp(1, 20)),
                };
                let r = ops
                    .search(q)
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                let items: Vec<Value> = r
                    .items
                    .iter()
                    .map(|i| {
                        json!({
                            "id": i.id,
                            "at": i.at,
                            "kind": i.kind,
                            "project": i.project,
                            "title": i.title,
                            "body": i.body.chars().take(800).collect::<String>(),
                            "cause": i.cause,
                            "fix": i.fix,
                            "follow_ups": i.follow_ups,
                            "commit": i.commit,
                            "host": i.host,
                            "severity": i.severity,
                            "source": i.source,
                            "decided_by": i.decided_by,
                            "question": i.question,
                            "asked_by": i.asked_by,
                            "page": format!("Lucina/{}", i.paths.project),
                        })
                    })
                    .collect();
                Ok(json!({"count": items.len(), "records": items}))
            }
            Action::Get => {
                let id =
                    a.id.as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ToolError::new("get needs id"))?;
                match ops.get(id).map_err(|e| ToolError::new(e.to_string()))? {
                    Some(r) => Ok(json!(r)),
                    None => Err(ToolError::new("no such record")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{Ctx, ToolEnv};

    #[tokio::test]
    async fn record_search_and_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let ops = lucina_ops::Ops::open(
            lucina_ops::OpsConfig::new(&vault, dir.path().join("state"), chrono_tz::Europe::Rome),
            None,
        )
        .unwrap();
        let mut env = ToolEnv::new(dir.path(), chrono_tz::Europe::Rome);
        env.ops = Some(Arc::new(ops));
        let ctx = Ctx::new(Arc::new(env), "ios", "s");
        let r = OpsTool
            .call(
                json!({"action": "record", "kind": "decision", "project": "pokoin",
                       "title": "Pokoin sync moves to events", "body": "the ping-pong rewrote 12k rows a cycle",
                       "follow_ups": ["measure the new sync"]}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(r["duplicate"], false);
        assert_eq!(r["pages"]["project"], "Lucina/Projects/pokoin.md");
        let id = r["id"].as_str().unwrap().to_owned();
        let s = OpsTool
            .call(json!({"action": "search", "query": "sync ping-pong"}), &ctx)
            .await
            .unwrap();
        assert_eq!(s["count"], 1);
        assert_eq!(s["records"][0]["id"], id.as_str());
        assert_eq!(s["records"][0]["source"], "lucina");
        let g = OpsTool
            .call(json!({"action": "get", "id": id}), &ctx)
            .await
            .unwrap();
        assert_eq!(g["follow_ups"][0], "measure the new sync");
        // A record needs its kind; unknown arguments are refused.
        assert!(OpsTool
            .call(
                json!({"action": "record", "project": "pokoin", "title": "x"}),
                &ctx
            )
            .await
            .is_err());
        assert!(OpsTool
            .call(json!({"action": "search", "nope": 1}), &ctx)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn off_without_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(
            Arc::new(ToolEnv::new(dir.path(), chrono_tz::Europe::Rome)),
            "ios",
            "s",
        );
        let e = OpsTool
            .call(json!({"action": "search"}), &ctx)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("off"), "{e}");
    }
}

//! `lagent id …` and `lagent trace`: the identifiers of runs, changes, versions and findings
//! ([`crate::manager::ids`]). Issuing needs the owner's socket; listing, showing and tracing
//! change nothing.

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::{pipe, s, table, CmdError, Output};
use crate::client::{Client, ClientError};

#[derive(Subcommand, Debug)]
pub enum IdCommand {
    /// Issue the next id of a kind; prints the id alone (or the row with --json)
    New(NewArgs),
    /// Add a row with a new state to an id (earlier rows stay)
    State(StateArgs),
    /// List the ids with their latest state
    Ls(LsArgs),
    /// One id: its view, then every row, oldest first
    Show {
        /// The id (`LA-R023`, `GC-03`, ...)
        id: String,
    },
    /// Register or list the leads (two capital letters; one chat each)
    #[command(subcommand)]
    Lead(LeadCommand),
    /// Register or list the areas findings are filed under
    #[command(subcommand)]
    Area(AreaCommand),
    /// Read a hand-kept tab-separated ledger into the store
    Import(ImportArgs),
}

#[derive(Args, Debug)]
pub struct NewArgs {
    /// What to issue: run, change, version or finding
    #[arg(value_parser = ["run", "change", "version", "finding"])]
    pub kind: String,
    /// The lead code of a run, change or version
    #[arg(long)]
    pub lead: Option<String>,
    /// The area code of a finding
    #[arg(long)]
    pub area: Option<String>,
    #[arg(long)]
    pub title: String,
    /// Who does it (a run declared by hand must say)
    #[arg(long, required_if_eq("kind", "run"))]
    pub by: Option<String>,
    /// The agent id or transcript path a hand-declared run stands for
    #[arg(long, required_if_eq("kind", "run"))]
    pub agent: Option<String>,
    /// The lane that does it
    #[arg(long)]
    pub lane: Option<String>,
    /// The model, as the lane config spells it
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub spec: Option<String>,
    #[arg(long = "where")]
    pub where_: Option<String>,
    #[arg(long)]
    pub base: Option<String>,
    #[arg(long)]
    pub result: Option<String>,
    /// Other ids this one is linked to (they must exist)
    #[arg(long, num_args = 1..)]
    pub refs: Vec<String>,
    /// The first state (default: started, open, built or reported)
    #[arg(long)]
    pub state: Option<String>,
}

#[derive(Args, Debug)]
pub struct StateArgs {
    pub id: String,
    pub state: String,
    #[arg(long)]
    pub result: Option<String>,
    #[arg(long, num_args = 1..)]
    pub refs: Vec<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub by: Option<String>,
    #[arg(long)]
    pub spec: Option<String>,
    #[arg(long = "where")]
    pub where_: Option<String>,
    #[arg(long)]
    pub base: Option<String>,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// Only this lead's ids
    #[arg(long)]
    pub lead: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub state: Option<String>,
    /// A text in the id or the title (case does not matter)
    #[arg(short = 'q')]
    pub query: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum LeadCommand {
    /// Register a lead: two capital letters and the chat's name
    Add {
        code: String,
        title: String,
    },
    Ls,
}

#[derive(Subcommand, Debug)]
pub enum AreaCommand {
    /// Register an area (two to six capital letters) that a lead owns
    Add {
        code: String,
        #[arg(long)]
        lead: String,
        /// What the area covers
        covers: String,
    },
    Ls,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// The tab-separated file
    pub file: String,
    /// The file lists areas (`code`, `lead`, `covers`) instead of ids
    #[arg(long)]
    pub areas: bool,
}

#[derive(Args, Debug)]
pub struct TraceArgs {
    /// The id to follow
    pub id: Option<String>,
    /// Follow a commit instead: what its trailers and the records say
    #[arg(long, conflicts_with = "id")]
    pub commit: Option<String>,
    /// The repository of --commit (default: the current directory)
    #[arg(long, requires = "commit")]
    pub repo: Option<String>,
}

fn invalid(message: impl Into<String>) -> CmdError {
    CmdError {
        code: "INVALID_OPTIONS".into(),
        message: message.into(),
        details: None,
    }
}

fn api(e: ClientError) -> CmdError {
    e.into()
}

fn query_value(v: &str) -> String {
    v.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn cut(v: &str, n: usize) -> String {
    v.chars().take(n).collect()
}

/// A list of ids as text: `ID KIND STATE UTC BY TITLE REFS`.
fn list_text(ids: &[Value]) -> String {
    if ids.is_empty() {
        return "No ids.".into();
    }
    let rows: Vec<Vec<String>> = ids
        .iter()
        .map(|v| {
            vec![
                text(&v["id"]),
                text(&v["kind"]),
                text(&v["state"]),
                text(&v["utc"]),
                text(&v["by"]),
                cut(&text(&v["title"]), 60),
                v["refs"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .map(|x| x.as_str().unwrap_or_default())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            ]
        })
        .collect();
    table(
        &["ID", "KIND", "STATE", "UTC", "BY", "TITLE", "REFS"],
        &rows,
    )
}

/// `lagent id show`: the view's fields, then every row.
fn show_text(v: &Value) -> String {
    let view = &v["view"];
    let mut lines = Vec::new();
    let keys = [
        "id",
        "kind",
        "state",
        "title",
        "by",
        "firstUtc",
        "utc",
        "declaredBy",
        "lane",
        "model",
        "modelRaw",
        "chain",
        "agent",
        "spec",
        "where",
        "base",
        "repo",
        "workspace",
        "branch",
        "result",
        "refs",
        "rows",
        "afterBreak",
    ];
    for k in keys {
        let value = &view[k];
        let shown = match value {
            Value::Null => continue,
            Value::String(x) if x.is_empty() => continue,
            Value::Array(a) if a.is_empty() => continue,
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" "),
            Value::Bool(false) => continue,
            other => s(other),
        };
        lines.push(format!("{k:<10}  {shown}"));
    }
    let rows: Vec<Vec<String>> = v["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|r| {
            vec![
                text(&r["utc"]),
                text(&r["state"]),
                text(&r["declaredBy"]),
                text(&r["by"]),
                text(&r["result"]),
            ]
        })
        .collect();
    format!(
        "{}\n\nROWS\n{}",
        lines.join("\n"),
        table(&["UTC", "STATE", "DECLARED", "BY", "RESULT"], &rows)
    )
}

fn ids_of(v: &Value) -> Vec<Value> {
    v["ids"].as_array().cloned().unwrap_or_default()
}

/// `<id>  <state>  <by>  <title>`.
fn node_line(indent: &str, n: &Value) -> String {
    format!(
        "{indent}{}  {}  {}  {}",
        text(&n["id"]),
        text(&n["state"]),
        text(&n["by"]),
        text(&n["title"])
    )
}

fn short(sha: &str) -> String {
    cut(sha, 10)
}

/// The line of a node and, under a run, its lane, model, brief and commits; under a change its
/// merge commit.
fn node_lines(indent: &str, n: &Value) -> Vec<String> {
    let mut out = vec![node_line(indent, n)];
    let under = format!("{indent}    ");
    match n["kind"].as_str() {
        Some("run") => {
            let job = &n["job"];
            let pick = |keys: &[&Value]| {
                keys.iter()
                    .find_map(|v| v.as_str().filter(|x| !x.is_empty()))
                    .unwrap_or("-")
                    .to_string()
            };
            let lane = pick(&[&n["lane"], &job["finalLane"], &job["lane"], &job["chain"]]);
            let model = pick(&[&n["model"], &job["model"]]);
            let brief = pick(&[&n["spec"], &job["specPath"]]);
            out.push(format!("{under}lane {lane}  model {model}  brief {brief}"));
            for c in n["commits"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                out.push(format!(
                    "{under}commit {}  {}",
                    short(&text(&c["sha"])),
                    text(&c["subject"])
                ));
            }
        }
        Some("change") => {
            for m in n["merges"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let landed = m["landed"]
                    .as_str()
                    .map(short)
                    .unwrap_or_else(|| "-".into());
                out.push(format!(
                    "{under}merge job {}  {}  commit {landed}",
                    text(&m["jobId"]),
                    text(&m["state"])
                ));
            }
        }
        _ => {}
    }
    out
}

/// `lagent trace <id>` as text: the subject, then one block per relation.
fn trace_text(v: &Value) -> String {
    let mut lines = node_lines("", &v["subject"]);
    let mut current = String::new();
    for l in v["links"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let relation = text(&l["relation"]);
        if relation != current {
            lines.push(String::new());
            lines.push(relation.clone());
            current = relation;
        }
        lines.extend(node_lines("  ", &l["view"]));
    }
    if let Some(n) = v["chainBrokenAt"].as_u64() {
        lines.push(String::new());
        lines.push(format!(
            "warning: the id store's hash chain is broken at line {n}"
        ));
    }
    lines.join("\n")
}

/// `lagent trace --commit` as text.
fn commit_text(v: &Value) -> String {
    let mut lines = vec![format!(
        "commit {}  {}",
        text(&v["sha"]),
        text(&v["subject"])
    )];
    let t = &v["trailers"];
    let or_dash = |x: &Value| {
        x.as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
            .to_string()
    };
    lines.push(format!("committed  {}", text(&v["committerTime"])));
    lines.push(format!("lead       {}", or_dash(&t["lead"])));
    lines.push(format!("run        {}", or_dash(&t["run"])));
    lines.push(format!("change     {}", or_dash(&t["change"])));
    for key in ["fixes", "refs"] {
        let list: Vec<&str> = t[key]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if !list.is_empty() {
            lines.push(format!("{key:<10} {}", list.join(" ")));
        }
    }
    let run = &v["runView"];
    if run.is_object() {
        lines.push(format!(
            "lane       {}  model {}",
            or_dash(&run["lane"]),
            or_dash(&run["model"])
        ));
        let flag = |x: &Value| match x {
            Value::Bool(b) => b.to_string(),
            _ => "unknown".to_string(),
        };
        lines.push(format!(
            "descends from the run's base: {}   after the run's first row: {}",
            flag(&v["descendsFromBase"]),
            flag(&v["afterFirstRow"])
        ));
    }
    let ledger = &v["ledger"];
    if ledger.is_object() {
        lines.push(format!(
            "ledger     model {}  kind {}  lane {}  session {}  workspace {}",
            or_dash(&ledger["model"]),
            or_dash(&ledger["kind"]),
            or_dash(&ledger["lane"]),
            or_dash(&ledger["session"]),
            or_dash(&ledger["workspace"])
        ));
    }
    let missing: Vec<&str> = v["missing"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !missing.is_empty() {
        lines.push(format!("missing    {}", missing.join(", ")));
    }
    for n in v["notes"].as_array().map(Vec::as_slice).unwrap_or_default() {
        lines.push(format!("note       {}", text(n)));
    }
    for key in ["run", "change"] {
        if v[key].is_object() {
            lines.push(String::new());
            lines.push(trace_text(&v[key]));
        }
    }
    lines.join("\n")
}

fn read_file(path: &str) -> Result<String, CmdError> {
    std::fs::read_to_string(path).map_err(|e| invalid(format!("cannot read {path}: {e}")))
}

pub async fn ids(client: &Client, cmd: &IdCommand) -> Result<Output, CmdError> {
    match cmd {
        IdCommand::New(a) => {
            let kind = a.kind.as_str();
            let mut body = json!({
                "kind": kind, "title": a.title, "by": a.by.clone().unwrap_or_default(),
                "spec": a.spec.clone().unwrap_or_default(),
                "where": a.where_.clone().unwrap_or_default(),
                "base": a.base.clone().unwrap_or_default(),
                "result": a.result.clone().unwrap_or_default(),
                "refs": a.refs, "state": a.state, "agent": a.agent, "lane": a.lane, "model": a.model,
            });
            if kind == "finding" {
                if a.area.is_none() {
                    return Err(invalid("a finding needs --area <code>"));
                }
                body["area"] = json!(a.area);
            } else {
                if a.lead.is_none() {
                    return Err(invalid("a run, change or version needs --lead <code>"));
                }
                body["lead"] = json!(a.lead);
            }
            let row = client.post("/ids", &body).await.map_err(api)?;
            Ok(Output {
                text: text(&row["id"]),
                json: Some(row),
            })
        }
        IdCommand::State(a) => {
            let body = json!({
                "state": a.state,
                "result": a.result.clone().unwrap_or_default(),
                "refs": a.refs,
                "title": a.title.clone().unwrap_or_default(),
                "by": a.by.clone().unwrap_or_default(),
                "spec": a.spec.clone().unwrap_or_default(),
                "where": a.where_.clone().unwrap_or_default(),
                "base": a.base.clone().unwrap_or_default(),
            });
            let row = client
                .post(&format!("/ids/{}/state", query_value(&a.id)), &body)
                .await
                .map_err(api)?;
            Ok(Output {
                text: format!("{} {}", text(&row["id"]), text(&row["state"])),
                json: Some(row),
            })
        }
        IdCommand::Ls(a) => {
            let mut q = Vec::new();
            for (k, v) in [
                ("lead", &a.lead),
                ("kind", &a.kind),
                ("state", &a.state),
                ("q", &a.query),
            ] {
                if let Some(v) = v {
                    q.push(format!("{k}={}", query_value(v)));
                }
            }
            let v = client
                .get(&format!("/ids?{}", q.join("&")))
                .await
                .map_err(api)?;
            let mut out = list_text(&ids_of(&v));
            if let Some(n) = v["chainBrokenAt"].as_u64() {
                out.push_str(&format!(
                    "\nwarning: the id store's hash chain is broken at line {n}"
                ));
            }
            Ok(Output {
                text: out,
                json: Some(v),
            })
        }
        IdCommand::Show { id } => {
            let v = client
                .get(&format!("/ids/{}", query_value(id)))
                .await
                .map_err(api)?;
            Ok(Output {
                text: show_text(&v),
                json: Some(v),
            })
        }
        IdCommand::Lead(LeadCommand::Add { code, title }) => {
            let row = client
                .post(
                    "/ids",
                    &json!({"kind": "lead", "code": code, "title": title}),
                )
                .await
                .map_err(api)?;
            Ok(Output {
                text: format!("registered lead {}", text(&row["id"])),
                json: Some(row),
            })
        }
        IdCommand::Lead(LeadCommand::Ls) => {
            let v = client.get("/ids?kind=lead").await.map_err(api)?;
            let rows: Vec<Vec<String>> = ids_of(&v)
                .iter()
                .map(|r| vec![text(&r["id"]), text(&r["title"])])
                .collect();
            Ok(Output {
                text: table(&["LEAD", "NAME"], &rows),
                json: Some(v),
            })
        }
        IdCommand::Area(AreaCommand::Add { code, lead, covers }) => {
            let row = client
                .post(
                    "/ids",
                    &json!({"kind": "area", "code": code, "lead": lead, "title": covers}),
                )
                .await
                .map_err(api)?;
            Ok(Output {
                text: format!("registered area {} for lead {lead}", text(&row["id"])),
                json: Some(row),
            })
        }
        IdCommand::Area(AreaCommand::Ls) => {
            let v = client.get("/ids?kind=area").await.map_err(api)?;
            let rows: Vec<Vec<String>> = ids_of(&v)
                .iter()
                .map(|r| vec![text(&r["id"]), text(&r["by"]), text(&r["title"])])
                .collect();
            Ok(Output {
                text: table(&["AREA", "LEAD", "COVERS"], &rows),
                json: Some(v),
            })
        }
        IdCommand::Import(a) => {
            let tsv = read_file(&a.file)?;
            let v = client
                .post("/ids/import", &json!({"tsv": tsv, "areas": a.areas}))
                .await
                .map_err(api)?;
            Ok(Output {
                text: format!(
                    "imported {}, skipped {}",
                    v["imported"].as_u64().unwrap_or(0),
                    v["skipped"].as_u64().unwrap_or(0)
                ),
                json: Some(v),
            })
        }
    }
}

pub async fn trace(client: &Client, a: &TraceArgs) -> Result<Output, CmdError> {
    match (&a.id, &a.commit) {
        (Some(id), None) => {
            let v = client
                .get(&format!("/ids/{}/trace", query_value(id)))
                .await
                .map_err(api)?;
            Ok(Output {
                text: trace_text(&v),
                json: Some(v),
            })
        }
        (None, Some(sha)) => {
            let repo = match &a.repo {
                Some(r) => r.clone(),
                None => std::env::current_dir()
                    .map_err(|e| invalid(format!("cannot read the current directory: {e}")))?
                    .display()
                    .to_string(),
            };
            let repo = std::path::absolute(&repo)
                .map_err(|e| invalid(format!("bad --repo {repo}: {e}")))?
                .display()
                .to_string();
            let v = client
                .get(&format!(
                    "/ids/trace/commit?repo={}&sha={}",
                    query_value(&pipe::host_path(&repo)),
                    query_value(sha)
                ))
                .await
                .map_err(api)?;
            Ok(Output {
                text: commit_text(&v),
                json: Some(v),
            })
        }
        _ => Err(invalid("give an id, or --commit <sha>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_prints_blocks_per_relation_and_details_under_runs() {
        let v = json!({
            "subject": {"id": "GC-01", "kind": "finding", "state": "fixed", "by": "JS-R004", "title": "uaf"},
            "links": [
                {"relation": "found by", "view": {"id": "LA-R001", "kind": "run", "state": "verified",
                    "by": "qoder auto-free", "title": "find", "lane": "qoder", "model": "auto-free",
                    "spec": "/s/a.md", "commits": [{"sha": "0123456789abcdef", "subject": "fix it"}]}},
                {"relation": "fixed by", "view": {"id": "LA-C001", "kind": "change", "state": "merged",
                    "by": "LA", "title": "fix", "merges": [{"jobId": "j1", "state": "succeeded", "landed": "fedcba9876543210"}]}}
            ],
            "chainBrokenAt": null
        });
        let t = trace_text(&v);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "GC-01  fixed  JS-R004  uaf");
        assert!(lines.contains(&"found by"));
        assert!(lines.contains(&"  LA-R001  verified  qoder auto-free  find"));
        assert!(lines.contains(&"      lane qoder  model auto-free  brief /s/a.md"));
        assert!(lines.contains(&"      commit 0123456789  fix it"));
        assert!(lines.contains(&"fixed by"));
        assert!(lines
            .iter()
            .any(|l| l.contains("merge job j1  succeeded  commit fedcba9876")));
        assert!(!t.contains("warning"));
    }

    #[test]
    fn a_commit_without_trailers_names_what_is_missing() {
        let v = json!({
            "sha": "abc", "subject": "plain", "committerTime": "2026-11-17T10:00:00Z",
            "trailers": {"lead": null, "run": null, "change": null, "fixes": [], "refs": []},
            "missing": ["Lead", "Run"], "ledger": null, "notes": []
        });
        let t = commit_text(&v);
        assert!(t.contains("missing    Lead, Run"), "{t}");
        assert!(t.contains("lead       -"), "{t}");
    }

    #[test]
    fn lists_cut_the_title_and_join_the_refs() {
        let ids = vec![
            json!({"id": "LA-R001", "kind": "run", "state": "started", "utc": "u",
            "by": "b", "title": "x".repeat(100), "refs": ["A-1", "B-2"]}),
        ];
        let t = list_text(&ids);
        assert!(t.starts_with("ID"), "{t}");
        assert!(t.contains(&"x".repeat(60)) && !t.contains(&"x".repeat(61)));
        assert!(t.contains("A-1 B-2"));
        assert_eq!(list_text(&[]), "No ids.");
    }
}

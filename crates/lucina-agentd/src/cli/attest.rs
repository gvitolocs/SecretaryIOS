//! `lagent attest` and `lagent ledger show|verify`: the commit attribution ledger
//! ([`crate::ledger`]). These work on the ledger file and the repo directly, not through the
//! daemon: the file is flock-ed and hash-chained, and a test (or a throwaway state directory)
//! needs no running daemon.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::{table, CmdError, Output};
use crate::ledger::{self, Appended, AttestArgs, Ledger, LedgerError, Selection};

#[derive(Args, Debug)]
pub struct AttestCli {
    /// The model id that wrote the commits (default: $LAGENT_ACTOR_MODEL), e.g. claude-sonnet-5-5
    #[arg(long)]
    pub model: Option<String>,
    /// lane, subagent or lead (default: $LAGENT_ACTOR_KIND, else lead)
    #[arg(long)]
    pub kind: Option<String>,
    /// The lane that ran it (default: $LAGENT_ACTOR_LANE)
    #[arg(long)]
    pub lane: Option<String>,
    /// One commit (any revision)
    #[arg(long, conflicts_with = "range")]
    pub commit: Option<String>,
    /// A range A..B (non-merge commits)
    #[arg(long)]
    pub range: Option<String>,
    /// Any path in the repo (default: the current directory); with neither --commit nor --range,
    /// the commits of its current branch that are not on the repo's trunk
    #[arg(long)]
    pub repo: Option<PathBuf>,
    /// The workspace id, for the record (default: $LAGENT_WORKSPACE)
    #[arg(long)]
    pub workspace: Option<String>,
    /// The daemon's state directory (default: $LUCINA_AGENTD_STATE_DIR, else ~/.local/share/lucina-agentd)
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum LedgerCommand {
    /// Print ledger entries
    Show {
        /// Only this commit (any revision, or a hex prefix)
        #[arg(long)]
        commit: Option<String>,
        /// Only this repo (any path in it)
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Walk the hash chain; non-zero exit and the first bad line if it is broken
    Verify {
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

fn ledger_for(state_dir: &Option<PathBuf>) -> Ledger {
    let dir = state_dir
        .clone()
        .or_else(|| std::env::var_os("LUCINA_AGENTD_STATE_DIR").map(PathBuf::from))
        .unwrap_or_else(crate::config::default_state_dir);
    Ledger::in_state_dir(&dir)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn session() -> String {
    for k in [
        "LAGENT_SESSION",
        "CLAUDE_CODE_HOST_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        if let Some(v) = env(k) {
            return v;
        }
    }
    let user = env("USER").unwrap_or_else(|| "unknown".into());
    let host = env("HOSTNAME")
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into());
    format!("{user}@{host}")
}

fn err(e: LedgerError) -> CmdError {
    match e {
        LedgerError::Refused(m) => CmdError::new(
            "ATTEST_REFUSED",
            format!("not attested, nothing was written:\n  {}", m.join("\n  ")),
        ),
        LedgerError::Other(m) => CmdError::new("LEDGER_ERROR", m),
    }
}

fn entry_json(e: &ledger::Entry) -> Value {
    serde_json::to_value(e).unwrap_or(Value::Null)
}

pub fn attest(a: &AttestCli) -> Result<Output, CmdError> {
    let (model, mut declared_by) = match (&a.model, env("LAGENT_ACTOR_MODEL")) {
        (Some(m), _) => (m.clone(), "flag"),
        (None, Some(m)) => (m, "env"),
        (None, None) => {
            return Err(CmdError::new(
                "MISSING_MODEL",
                "say which model wrote the commits: --model <id> (or $LAGENT_ACTOR_MODEL)",
            ))
        }
    };
    let lane = a.lane.clone().or_else(|| env("LAGENT_ACTOR_LANE"));
    // A lane that exported its own environment is the strongest declaration.
    if declared_by == "env" && env("LAGENT_ACTOR_LANE").is_some() {
        declared_by = "lane";
    }
    let selection = match (&a.commit, &a.range) {
        (Some(c), _) => Selection::Commit(c.clone()),
        (_, Some(r)) => Selection::Range(r.clone()),
        _ => Selection::Default,
    };
    let args = AttestArgs {
        model,
        kind: a
            .kind
            .clone()
            .or_else(|| env("LAGENT_ACTOR_KIND"))
            .unwrap_or_else(|| "lead".into()),
        lane: lane.unwrap_or_default(),
        session: session(),
        declared_by: declared_by.into(),
        workspace: a
            .workspace
            .clone()
            .or_else(|| env("LAGENT_WORKSPACE"))
            .unwrap_or_default(),
        selection,
    };
    let dir = match &a.repo {
        Some(p) => p.clone(),
        None => std::env::current_dir().map_err(|e| CmdError::new("NO_CWD", e.to_string()))?,
    };
    let lines = ledger::attest(&ledger_for(&a.state_dir), &dir, &args).map_err(err)?;
    let mut text = Vec::new();
    let mut js = Vec::new();
    for l in &lines {
        match &l.appended {
            Appended::Written(e) => {
                text.push(format!(
                    "attested {} {} ({})",
                    &e.commit[..12],
                    e.model,
                    l.subject
                ));
                js.push(json!({"status": "attested", "entry": entry_json(e)}));
            }
            Appended::Already(e) => {
                text.push(format!(
                    "already attested {} {} ({})",
                    &e.commit[..12],
                    e.model,
                    l.subject
                ));
                js.push(json!({"status": "already", "entry": entry_json(e)}));
            }
        }
    }
    if lines.is_empty() {
        text.push("no commits to attest".into());
    }
    Ok(Output {
        json: Some(Value::Array(js)),
        text: text.join("\n"),
    })
}

pub fn ledger_cmd(cmd: &LedgerCommand) -> Result<Output, CmdError> {
    match cmd {
        LedgerCommand::Verify { state_dir } => {
            let l = ledger_for(state_dir);
            match l.read() {
                Ok(es) => Ok(Output {
                    json: Some(
                        json!({"ok": true, "entries": es.len(), "path": l.path.display().to_string()}),
                    ),
                    text: format!(
                        "ok: {} entr{} in {}",
                        es.len(),
                        if es.len() == 1 { "y" } else { "ies" },
                        l.path.display()
                    ),
                }),
                Err(b) => Err(CmdError::new(
                    "LEDGER_BROKEN",
                    format!(
                        "{}: broken at line {}: {}",
                        l.path.display(),
                        b.line,
                        b.reason
                    ),
                )),
            }
        }
        LedgerCommand::Show {
            commit,
            repo,
            state_dir,
        } => {
            let l = ledger_for(state_dir);
            let mut es = l.read().map_err(|b| {
                CmdError::new(
                    "LEDGER_BROKEN",
                    format!(
                        "{}: broken at line {}: {}",
                        l.path.display(),
                        b.line,
                        b.reason
                    ),
                )
            })?;
            if let Some(r) = repo {
                let ctx = ledger::repo_ctx(r).map_err(|e| CmdError::new("NOT_A_REPO", e))?;
                es.retain(|e| e.repo == ctx.name);
            }
            if let Some(c) = commit {
                let dir = repo
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                let full = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(["rev-parse", "--verify", "-q", &format!("{c}^{{commit}}")])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                let want = full.unwrap_or_else(|| c.to_lowercase());
                es.retain(|e| e.commit.starts_with(&want));
            }
            let rows: Vec<Vec<String>> = es
                .iter()
                .map(|e| {
                    vec![
                        e.commit[..12].to_string(),
                        e.repo.clone(),
                        e.model.clone(),
                        e.kind.clone(),
                        e.lane.clone(),
                        e.declared_by.clone(),
                        e.time.clone(),
                    ]
                })
                .collect();
            Ok(Output {
                json: Some(Value::Array(es.iter().map(entry_json).collect())),
                text: table(
                    &["COMMIT", "REPO", "MODEL", "KIND", "LANE", "BY", "TIME"],
                    &rows,
                ),
            })
        }
    }
}

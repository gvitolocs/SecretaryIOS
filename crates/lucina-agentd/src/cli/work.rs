//! `lagent work …`: delegated worker runs that go through `lane` ([`crate::manager::work`]).
//! Starting one needs the owner's socket; listing and waiting do not change anything.

use std::io::Read;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use lucina_agents::work::{CostClass, Priority, WorkState};
use serde_json::{json, Value};

use super::{pipe, s, table, CmdError, Output};
use crate::client::{Client, ClientError};

/// How long one wait request holds on before `lagent work wait` asks again.
const WAIT_CHUNK_SECS: u64 = 300;

#[derive(Subcommand, Debug)]
pub enum WorkCommand {
    /// Start a worker run through `lane`; it belongs to a lead
    Start(Box<StartArgs>),
    /// Every lead's runs, newest work first
    Ls(LsArgs),
    /// One run with every field
    Show {
        /// The work id (`wk-…`)
        id: String,
    },
    /// Wait for a run to end; the exit code says how it ended: 0 done or verified, 1 failed
    /// or lost, 3 all lanes exhausted, 4 dirty after quota, 5 still going at the timeout,
    /// 6 ended with jobs queued, 7 ended dirty, 8 ended empty
    Wait(WaitArgs),
}

#[derive(Args, Debug)]
pub struct StartArgs {
    /// The chat that owns the run (default: $LAGENT_SESSION, $CLAUDE_CODE_HOST_SESSION_ID, $CLAUDE_CODE_SESSION_ID)
    #[arg(long)]
    pub lead: Option<String>,
    /// Who runs first when the host is busy: owner, high, normal or backlog
    #[arg(long, default_value = "normal")]
    pub priority: String,
    /// light: the worker reads, reviews or measures and never builds (its cargo calls are refused by lane and it needs --expect); build: it compiles or tests
    #[arg(long, default_value = "build")]
    pub cost: String,
    /// The lane to run (one of --lane and --chain)
    #[arg(long, conflicts_with = "chain")]
    pub lane: Option<String>,
    /// The lane category to run through (one of --lane and --chain)
    #[arg(long)]
    pub chain: Option<String>,
    /// The directory the worker runs in
    #[arg(long)]
    pub workspace: String,
    /// A command that checks the result (run by lane after the worker ends)
    #[arg(long)]
    pub verify: Option<String>,
    /// A file, relative to the workspace, that the run must produce
    #[arg(long)]
    pub expect: Option<String>,
    /// A short name for the run
    #[arg(long)]
    pub title: Option<String>,
    /// The spec file (default: read the spec from stdin)
    #[arg(long)]
    pub spec: Option<String>,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// Only this lead's runs
    #[arg(long)]
    pub lead: Option<String>,
    /// Leave out the runs that were started with `lane` directly
    #[arg(long)]
    pub no_direct: bool,
    /// Include runs that ended more than a day ago
    #[arg(long)]
    pub old: bool,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    /// The work id (`wk-…`)
    pub id: String,
    /// Give up after this many seconds (exit code 5 while the run is still going)
    #[arg(long, default_value_t = 10800)]
    pub timeout: u64,
}

/// The lead: the first of the session variables that is set.
fn session_lead() -> Option<String> {
    [
        "LAGENT_SESSION",
        "CLAUDE_CODE_HOST_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
    ]
    .into_iter()
    .filter_map(|k| std::env::var(k).ok())
    .map(|v| v.trim().to_string())
    .find(|v| !v.is_empty())
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

/// `<lead>: N running, N queued, N ended, N empty`, then `, N lost` when N > 0.
pub(super) fn lead_header(l: &Value) -> String {
    let n = |k: &str| l[k].as_u64().unwrap_or(0);
    let lost = match n("lost") {
        0 => String::new(),
        c => format!(", {c} lost"),
    };
    format!(
        "{}: {} running, {} queued, {} ended, {} empty{lost}",
        s(&l["lead"]),
        n("running"),
        n("queued"),
        n("ended"),
        n("empty")
    )
}

/// An unfinished job whose runner is gone.
fn runner_lost(j: &Value) -> bool {
    j["runnerLost"] == true
        && j["state"]
            .as_str()
            .is_some_and(|s| matches!(s, "starting" | "queued" | "running"))
}

/// A short age, `5m`, `3h`, `2d`, for an RFC 3339 time.
fn age(iso: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let secs = (chrono::Utc::now() - then.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// A finished job that agentd started, whose `empty` could not be decided.
fn emptiness_unknown(j: &Value) -> bool {
    let finished = j["state"]
        .as_str()
        .is_some_and(|s| !matches!(s, "starting" | "queued" | "running"));
    finished && j["empty"].is_null() && j["direct"] != true
}

fn job_row(j: &Value) -> Vec<String> {
    let lane = [&j["finalLane"], &j["lane"], &j["chain"]]
        .into_iter()
        .find_map(|v| v.as_str().filter(|x| !x.is_empty()))
        .unwrap_or_default();
    let workspace = text(&j["workspace"])
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    vec![
        s(&j["id"]),
        if runner_lost(j) {
            format!("{}(lost)", s(&j["state"]))
        } else {
            s(&j["state"])
        },
        s(&j["priority"]),
        s(&j["cost"]),
        lane.to_string(),
        j["queuePosition"]
            .as_u64()
            .map(|q| q.to_string())
            .unwrap_or_default(),
        age(text(&j["createdAt"])),
        if j["empty"] == true {
            "empty".to_string()
        } else if emptiness_unknown(j) {
            "?".to_string()
        } else {
            String::new()
        },
        workspace,
        text(&j["note"]).chars().take(60).collect(),
    ]
}

fn list_text(leads: &[Value]) -> String {
    if leads.is_empty() {
        return "No work jobs.".into();
    }
    leads
        .iter()
        .map(|l| {
            let rows: Vec<Vec<String>> = l["jobs"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(job_row)
                .collect();
            let t = table(
                &[
                    "ID",
                    "STATE",
                    "PRI",
                    "COST",
                    "LANE",
                    "QUEUE",
                    "AGE",
                    "EMPTY",
                    "WORKSPACE",
                    "NOTE",
                ],
                &rows,
            );
            format!("{}\n{t}", lead_header(l))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Every field of a job, one per line.
fn show_text(job: &Value) -> String {
    let Some(map) = job.as_object() else {
        return s(job);
    };
    let width = map.keys().map(|k| k.len()).max().unwrap_or(0);
    let lost =
        runner_lost(job).then(|| "runner lost: yes (resume or dismiss it with lane)".to_string());
    map.iter()
        .map(|(k, v)| {
            let shown = if k == "empty" && emptiness_unknown(job) {
                "unknown (no git heads and no expected file)".to_string()
            } else if v.is_null() {
                "-".to_string()
            } else {
                s(v)
            };
            format!("{k:<width$}  {shown}")
        })
        .chain(lost)
        .collect::<Vec<_>>()
        .join("\n")
}

fn show_output(job: Value) -> Output {
    Output {
        text: show_text(&job),
        json: Some(job),
    }
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

fn read_spec(path: &Option<String>) -> Result<String, CmdError> {
    let spec = match path {
        Some(p) => {
            std::fs::read_to_string(p).map_err(|e| invalid(format!("cannot read {p}: {e}")))?
        }
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| invalid(format!("cannot read the spec from stdin: {e}")))?;
            buf
        }
    };
    if spec.trim().is_empty() {
        return Err(invalid("the spec is empty"));
    }
    Ok(spec)
}

/// The longest `lagent work wait --timeout`: 7 days.
const WAIT_MAX_SECS: u64 = 604_800;

fn capped_timeout(secs: u64) -> u64 {
    secs.min(WAIT_MAX_SECS)
}

/// The exit code of `lagent work start`: 1 when `lane run` failed and the job is `failed`.
fn start_exit_code(state: &str) -> i32 {
    i32::from(state == "failed")
}

/// Prints a final output ourselves when the exit code is not 0 (`render` only knows 0 and 1).
fn print_and_exit(out: &Output, json: bool, code: i32) -> ! {
    match (&out.json, json) {
        (Some(v), true) => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
        _ => println!("{}", out.text),
    }
    std::process::exit(code)
}

pub async fn work(client: &Client, cmd: &WorkCommand, json: bool) -> Result<Output, CmdError> {
    match cmd {
        WorkCommand::Start(a) => {
            let lead = a.lead.clone().filter(|l| !l.trim().is_empty()).or_else(session_lead).ok_or_else(|| {
                invalid("no lead: pass --lead <chat> (or set LAGENT_SESSION, CLAUDE_CODE_HOST_SESSION_ID or CLAUDE_CODE_SESSION_ID)")
            })?;
            if a.lane.is_none() && a.chain.is_none() {
                return Err(invalid("give --lane <name> or --chain <name>"));
            }
            let priority: Priority = a.priority.parse().map_err(invalid)?;
            let cost: CostClass = a.cost.parse().map_err(invalid)?;
            let spec = read_spec(&a.spec)?;
            let workspace = std::path::absolute(&a.workspace)
                .map_err(|e| invalid(format!("bad --workspace {}: {e}", a.workspace)))?
                .display()
                .to_string();
            let body = json!({
                "lead": lead, "priority": priority, "cost": cost, "lane": a.lane, "chain": a.chain,
                "workspace": pipe::host_path(&workspace), "verify": a.verify, "expect": a.expect,
                "title": a.title, "spec": spec,
            });
            let job = client.post("/work", &body).await.map_err(api)?;
            let code = start_exit_code(job["state"].as_str().unwrap_or_default());
            let text = format!("{} {}", s(&job["id"]), s(&job["state"]));
            let out = Output {
                text,
                json: Some(job),
            };
            match code {
                0 => Ok(out),
                code => print_and_exit(&out, json, code),
            }
        }
        WorkCommand::Ls(a) => {
            let mut q = vec![format!("direct={}", !a.no_direct), format!("old={}", a.old)];
            if let Some(l) = &a.lead {
                q.push(format!("lead={}", query_value(l)));
            }
            let v = client
                .get(&format!("/work?{}", q.join("&")))
                .await
                .map_err(api)?;
            let (problems, code) = ls_problems(&v);
            for p in &problems {
                eprintln!("{p}");
            }
            let leads = v["leads"].as_array().cloned().unwrap_or_default();
            let out = Output {
                text: list_text(&leads),
                json: Some(v),
            };
            match code {
                0 => Ok(out),
                code => print_and_exit(&out, json, code),
            }
        }
        WorkCommand::Show { id } => Ok(show_output(
            client.get(&format!("/work/{id}")).await.map_err(api)?,
        )),
        WorkCommand::Wait(a) => {
            let deadline = Instant::now() + Duration::from_secs(capped_timeout(a.timeout));
            let job = loop {
                let left = deadline.saturating_duration_since(Instant::now()).as_secs();
                let job = client
                    .post(
                        &format!("/work/{}/wait", a.id),
                        &json!({"timeoutSecs": left.min(WAIT_CHUNK_SECS)}),
                    )
                    .await
                    .map_err(api)?;
                let state: WorkState =
                    serde_json::from_value(job["state"].clone()).unwrap_or(WorkState::Running);
                if state.is_terminal() || left == 0 {
                    break job;
                }
            };
            let state: WorkState =
                serde_json::from_value(job["state"].clone()).unwrap_or(WorkState::Running);
            let out = show_output(job);
            match state.wait_exit_code() {
                0 => Ok(out),
                code => print_and_exit(&out, json, code),
            }
        }
    }
}

/// What is wrong with a listing, as stderr lines, and the exit code of `lagent work ls`:
/// 2 when the daemon sent a warning or a listed job's `lane status` could not be read.
fn ls_problems(v: &Value) -> (Vec<String>, i32) {
    let mut lines = Vec::new();
    if let Some(w) = v["warning"].as_str() {
        lines.push(format!("warning: {w}"));
    }
    let unreadable = v["jobs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|j| j["statusErrors"].as_u64().unwrap_or(0) > 0)
        .count();
    if unreadable > 0 {
        lines.push(format!(
            "{unreadable} job(s) whose lane status could not be read"
        ));
    }
    let code = if lines.is_empty() { 0 } else { 2 };
    (lines, code)
}

/// The `WORK` part of `lagent board`: one header line per lead, and the daemon's warning when
/// it has one. Empty when the daemon has no work route.
pub async fn board_section(client: &Client) -> Option<String> {
    let v = client.get("/work").await.ok()?;
    let leads = v["leads"].as_array()?;
    let mut lines: Vec<String> = leads.iter().map(lead_header).collect();
    if let Some(w) = v["warning"].as_str() {
        lines.push(format!("warning: {w}"));
    }
    Some(format!(
        "WORK\n{}",
        if lines.is_empty() {
            "none".to_string()
        } else {
            lines.join("\n")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_huge_timeout_is_capped() {
        assert_eq!(capped_timeout(10), 10);
        assert_eq!(capped_timeout(604_800), 604_800);
        assert_eq!(capped_timeout(u64::MAX), 604_800);
        // Adding the capped value to the clock does not overflow.
        let _ = Instant::now() + Duration::from_secs(capped_timeout(u64::MAX));
    }

    #[test]
    fn a_failed_start_exits_1() {
        assert_eq!(start_exit_code("failed"), 1);
        assert_eq!(start_exit_code("starting"), 0);
        assert_eq!(start_exit_code(""), 0);
    }

    #[test]
    fn header_line() {
        let l = json!({"lead": "chat-1", "running": 2, "queued": 1, "ended": 5, "empty": 0});
        assert_eq!(
            lead_header(&l),
            "chat-1: 2 running, 1 queued, 5 ended, 0 empty"
        );
    }

    #[test]
    fn row_picks_the_lane_and_cuts_the_note() {
        let j = json!({
            "id": "wk-1", "state": "running", "priority": "normal", "cost": "build", "lane": "qoder", "chain": null,
            "finalLane": null, "queuePosition": 2, "createdAt": "2020-01-01T00:00:00Z", "empty": null,
            "workspace": "/home/nez/x/repo/", "note": "n".repeat(100)
        });
        let r = job_row(&j);
        assert_eq!(r[4], "qoder");
        assert_eq!(r[5], "2");
        assert_eq!(r[7], "");
        assert_eq!(job_row(&json!({"empty": true}))[7], "empty");
        // A finished job whose emptiness is unknown shows `?`; a running or direct one stays blank.
        assert_eq!(
            job_row(&json!({"state": "verified", "empty": null}))[7],
            "?"
        );
        assert_eq!(
            job_row(&json!({"state": "verified", "empty": false}))[7],
            ""
        );
        assert_eq!(
            job_row(&json!({"state": "verified", "empty": null, "direct": true}))[7],
            ""
        );
        let shown = show_text(&json!({"state": "done", "empty": null}));
        assert!(
            shown.contains("unknown (no git heads and no expected file)"),
            "{shown}"
        );
        assert_eq!(r[8], "repo");
        assert_eq!(r[9].len(), 60);
        assert!(r[6].ends_with('d'));
        let c = json!({"chain": "deep", "finalLane": "zen"});
        assert_eq!(job_row(&c)[4], "zen");
        assert_eq!(job_row(&json!({"chain": "deep"}))[4], "deep");
    }

    #[test]
    fn a_warning_or_an_unreadable_status_exits_2() {
        let clean = json!({"leads": [], "jobs": [{"statusErrors": 0}], "warning": null});
        assert_eq!(ls_problems(&clean), (vec![], 0));
        let warned =
            json!({"jobs": [], "warning": "1 of 3 lane rows could not be read: line 2: x"});
        let (lines, code) = ls_problems(&warned);
        assert_eq!(code, 2);
        assert_eq!(
            lines,
            ["warning: 1 of 3 lane rows could not be read: line 2: x"]
        );
        let stuck =
            json!({"jobs": [{"statusErrors": 3}, {"statusErrors": 1}, {}], "warning": null});
        let (lines, code) = ls_problems(&stuck);
        assert_eq!(code, 2);
        assert_eq!(lines, ["2 job(s) whose lane status could not be read"]);
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(query_value("a b&c"), "a%20b%26c");
        assert_eq!(query_value("direct:x-1"), "direct%3Ax-1");
    }

    #[test]
    fn show_lists_every_field() {
        let t = show_text(&json!({"id": "wk-1", "note": null, "dirtyFiles": 3}));
        assert!(t.contains("id") && t.contains("wk-1"));
        assert!(t.lines().any(|l| l.starts_with("note") && l.ends_with('-')));
        assert_eq!(t.lines().count(), 3);
    }

    #[test]
    fn a_lost_runner_shows_in_the_state_the_header_and_show() {
        let lost = json!({"id": "wk-1", "state": "queued", "runnerLost": true});
        assert_eq!(job_row(&lost)[1], "queued(lost)");
        assert_eq!(
            job_row(&json!({"state": "running", "runnerLost": true}))[1],
            "running(lost)"
        );
        assert_eq!(
            job_row(&json!({"state": "starting", "runnerLost": true}))[1],
            "starting(lost)"
        );
        assert_eq!(
            job_row(&json!({"state": "queued", "runnerLost": false}))[1],
            "queued"
        );
        // A finished job is not marked.
        assert_eq!(
            job_row(&json!({"state": "failed", "runnerLost": true}))[1],
            "failed"
        );
        assert!(show_text(&lost)
            .lines()
            .any(|l| l == "runner lost: yes (resume or dismiss it with lane)"));
        assert!(
            !show_text(&json!({"id": "wk-1", "state": "queued", "runnerLost": false}))
                .contains("runner lost")
        );
        let head = |lost: u64| {
            lead_header(
                &json!({"lead": "a", "running": 1, "queued": 2, "ended": 3, "empty": 4, "lost": lost}),
            )
        };
        assert_eq!(head(0), "a: 1 running, 2 queued, 3 ended, 4 empty");
        assert_eq!(head(2), "a: 1 running, 2 queued, 3 ended, 4 empty, 2 lost");
    }

    #[test]
    fn show_has_a_scope_line() {
        let t = show_text(&json!({"id": "wk-1", "scope": "lucina-work-wk-1"}));
        assert!(t
            .lines()
            .any(|l| l.starts_with("scope") && l.ends_with("lucina-work-wk-1")));
        let t = show_text(&json!({"id": "wk-1", "scope": null}));
        assert!(t
            .lines()
            .any(|l| l.starts_with("scope") && l.ends_with('-')));
    }
}

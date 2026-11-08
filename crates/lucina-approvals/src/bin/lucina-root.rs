//! `lucina-root --reason "…" --plan plan.json`: run root commands on this host
//! after the owner approves exactly those commands on his phone.
//!
//! The plan is a JSON list of argv arrays (no shell), e.g.
//! `[["systemctl","daemon-reload"],["install","-m","644","/a b","/etc/x"]]`.
//! Settings come from the process environment, then lucina-rs's
//! `~/.config/lucina-rs/lucina.env` and `mode.env` (server URL, service
//! token file, pinned key, `LUCINA_ROOT_*`); nothing secret is printed.
//!
//! Exit codes: 0 all commands ran and succeeded; 1 a command failed;
//! 2 usage or configuration error; 3 denied; 4 expired; 5 no answer in time;
//! 6 the request was refused (server unreachable, grant not acceptable).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use lucina_approvals::root::{plan_details, validate_plan, Who};
use lucina_approvals::{envfile, plan_hash, Approvals, ApprovalsConfig, RootOutcome};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Run root commands on nezopt after the owner approves them"
)]
struct Args {
    /// Why (shown to the owner).
    #[arg(long)]
    reason: String,
    /// JSON file: a list of argv arrays.
    #[arg(long)]
    plan: PathBuf,
    /// Wait for the owner this many seconds (default LUCINA_ROOT_WAIT_SECS, 300).
    #[arg(long)]
    wait_secs: Option<u64>,
    /// Read these env files instead of lucina.env and mode.env (repeatable).
    #[arg(long = "env-file")]
    env_files: Vec<PathBuf>,
    /// Only validate the plan and print what the owner would see, then exit.
    #[arg(long)]
    dry_run: bool,
}

fn read_plan(path: &PathBuf) -> Result<Vec<Vec<String>>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e.kind()))?;
    let plan: Vec<Vec<String>> = serde_json::from_str(&text)
        .map_err(|e| format!("the plan must be a JSON list of argv arrays of strings: {e}"))?;
    validate_plan(&plan).map_err(|e| e.to_string())?;
    Ok(plan)
}

fn indent(s: &str) -> String {
    s.trim_end()
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let plan = match read_plan(&args.plan) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lucina-root: {e}");
            return ExitCode::from(2);
        }
    };
    if args.dry_run {
        println!("{}\n{}", plan_details(&plan), plan_hash(&plan));
        return ExitCode::SUCCESS;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let files: Vec<PathBuf> = if args.env_files.is_empty() {
        vec![
            PathBuf::from(&home).join(".config/lucina-rs/lucina.env"),
            PathBuf::from(&home).join(".config/lucina-rs/mode.env"),
        ]
    } else {
        args.env_files.clone()
    };
    let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
    let from_files: HashMap<String, String> = envfile::load(&refs);
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| from_files.get(k).cloned().filter(|v| !v.trim().is_empty()))
    };
    let cfg = ApprovalsConfig::from_lookup(&get);
    let approvals = match Approvals::new(cfg) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lucina-root: {e}");
            return ExitCode::from(2);
        }
    };
    let who = Who::lucina(format!(
        "lucina-root CLI as {} on {}",
        std::env::var("USER").unwrap_or_else(|_| "?".into()),
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned()))
            .unwrap_or_else(|| "host".into())
    ));
    eprintln!("Asking the owner to approve {} command(s):", plan.len());
    eprintln!("{}", indent(&plan_details(&plan)));
    let wait = args
        .wait_secs
        .map(Duration::from_secs)
        .unwrap_or(approvals.config().root_wait);
    let report = approvals
        .root_exec_wait(&plan, &args.reason, &who, wait)
        .await;
    if let Some(id) = &report.approval_id {
        eprintln!("approval {id}");
    }
    let n = plan.len();
    for (i, s) in report.steps.iter().enumerate() {
        let state = if s.ok {
            "ok".to_owned()
        } else if s.timed_out {
            "TIMED OUT".to_owned()
        } else if let Some(c) = s.exit_code {
            format!("FAILED (exit {c})")
        } else {
            format!("FAILED ({})", s.error.clone().unwrap_or_default())
        };
        println!(
            "[{}/{n}] {}  ->  {state}  {} ms",
            i + 1,
            lucina_approvals::shell_line(&s.argv),
            s.ms
        );
        if !s.stdout.trim().is_empty() {
            println!("{}", indent(&s.stdout));
        }
        if !s.stderr.trim().is_empty() {
            println!("{}", indent(&s.stderr));
        }
        if s.truncated {
            println!("    (output cut)");
        }
    }
    eprintln!("{}", report.summary());
    match report.outcome {
        RootOutcome::Done => ExitCode::SUCCESS,
        RootOutcome::Failed { .. } => ExitCode::from(1),
        RootOutcome::Denied { .. } => ExitCode::from(3),
        RootOutcome::Expired => ExitCode::from(4),
        RootOutcome::Pending { .. } => ExitCode::from(5),
        RootOutcome::Refused(_) => ExitCode::from(6),
    }
}

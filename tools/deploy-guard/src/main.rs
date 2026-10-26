//! `deploy-guard`: refuses a deploy that would roll production back.
//!
//! The rule comes from the deploy model (verification/models/src/deploy.rs,
//! formal/deploy/Deploy.tla; kernel proved in verification/verus/deploy_guard.rs):
//! a build may go live only if
//!
//! 1. the build tree is clean (no uncommitted or untracked source),
//! 2. its commit is on the runtime's target branch (main, or lucina-rs for lucina-rs),
//! 3. the commit live now is an ancestor of it (nothing deployed before is missing),
//! 4. the merge lock is held (check-then-push is only safe inside it).
//!
//! ```sh
//! deploy-guard check  --runtime secretary-server [--repo DIR]   # before the deploy
//! deploy-guard record --runtime secretary-server [--repo DIR]   # right after it
//! deploy-guard status
//! ```
//!
//! The ledger of what is live is one file per runtime in
//! `$DEPLOY_GUARD_LEDGER` (default `~/.local/state/deploy-guard`). Exit codes:
//! 0 allowed / recorded, 1 refused, 2 usage or git error, 3 no ledger entry yet.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const LOCK: &str = "/tmp/lucina-merge.lock";

/// Target branch per known runtime.
fn default_target(runtime: &str) -> Option<&'static str> {
    match runtime {
        "secretary-server" | "secretary-mcp" | "web" => Some("main"),
        "lucina-rs" => Some("lucina-rs"),
        _ => None,
    }
}

/// What `check` found, as plain facts (the decision is a pure function of them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub dirty_paths: Vec<String>,
    pub commit: String,
    pub on_target: bool,
    /// The live commit and whether it is an ancestor of `commit`; `None`: no ledger entry.
    pub live: Option<(String, bool)>,
    pub lock_held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Refused(Vec<String>),
    NoLedger,
}

/// The guard's rule (Verus kernel D-K1, `guard_allows`).
pub fn decide(f: &Facts, target: &str) -> Verdict {
    let mut why = Vec::new();
    if !f.dirty_paths.is_empty() {
        why.push(format!(
            "the build tree has uncommitted or untracked files ({}{}); commit them (or move them to a wip branch) first",
            f.dirty_paths.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if f.dirty_paths.len() > 5 { ", ..." } else { "" }
        ));
    }
    if !f.on_target {
        why.push(format!(
            "{} is not on {target}: merge it into {target} first and build from the merged tip",
            short(&f.commit)
        ));
    }
    if let Some((live, false)) = &f.live {
        why.push(format!(
            "the live build {} is not an ancestor of {}: deploying would drop commits that are live now; merge {target}'s tip (or the live commit) first",
            short(live),
            short(&f.commit)
        ));
    }
    if !f.lock_held {
        why.push(format!("{LOCK} is not held: take it before checking (check-then-push is only safe inside the lock)"));
    }
    if !why.is_empty() {
        Verdict::Refused(why)
    } else if f.live.is_none() {
        Verdict::NoLedger
    } else {
        Verdict::Allowed
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

struct Args {
    cmd: String,
    runtime: Option<String>,
    repo: PathBuf,
    commit: Option<String>,
    target: Option<String>,
    note: Option<String>,
    lock: PathBuf,
    no_lock_check: bool,
    allow_untracked: bool,
    bootstrap: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or("usage: deploy-guard check|record|status --runtime NAME [--repo DIR] [--commit REV] [--target BRANCH] [--note TEXT] [--bootstrap] [--allow-untracked] [--no-lock-check]")?;
    let mut a = Args {
        cmd,
        runtime: None,
        repo: PathBuf::from("."),
        commit: None,
        target: None,
        note: None,
        lock: PathBuf::from(LOCK),
        no_lock_check: false,
        allow_untracked: false,
        bootstrap: false,
    };
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--runtime" => a.runtime = Some(val()?),
            "--repo" => a.repo = PathBuf::from(val()?),
            "--commit" => a.commit = Some(val()?),
            "--target" => a.target = Some(val()?),
            "--note" => a.note = Some(val()?),
            "--lock" => a.lock = PathBuf::from(val()?),
            "--no-lock-check" => a.no_lock_check = true,
            "--allow-untracked" => a.allow_untracked = true,
            "--bootstrap" => a.bootstrap = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(a)
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))
}

fn git_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_ancestor(repo: &Path, a: &str, b: &str) -> Result<bool, String> {
    let out = git(repo, &["merge-base", "--is-ancestor", a, b])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git merge-base --is-ancestor {a} {b}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// `origin/<branch>` when the repo has it (a clone), else the local branch.
fn target_ref(repo: &Path, branch: &str) -> String {
    let remote = format!("refs/remotes/origin/{branch}");
    if git(repo, &["rev-parse", "--verify", "--quiet", &remote]).is_ok_and(|o| o.status.success()) {
        remote
    } else {
        format!("refs/heads/{branch}")
    }
}

fn ledger_dir() -> PathBuf {
    std::env::var_os("DEPLOY_GUARD_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".local/state/deploy-guard")
        })
}

fn read_ledger(runtime: &str) -> Option<String> {
    let text = std::fs::read_to_string(ledger_dir().join(format!("{runtime}.commit"))).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("commit ").map(|s| s.trim().to_string()))
}

fn facts(a: &Args, runtime: &str, target: &str) -> Result<Facts, String> {
    let repo = &a.repo;
    let commit = git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            a.commit.as_deref().unwrap_or("HEAD"),
        ],
    )?;
    let head = git_ok(repo, &["rev-parse", "HEAD"])?;
    let mut dirty_paths: Vec<String> = git_ok(
        repo,
        &[
            "status",
            "--porcelain",
            if a.allow_untracked {
                "--untracked-files=no"
            } else {
                "--untracked-files=normal"
            },
        ],
    )?
    .lines()
    .map(|l| l.get(3..).unwrap_or(l).to_string())
    .collect();
    if commit != head {
        dirty_paths.insert(
            0,
            format!(
                "HEAD is {} but the deploy is {}",
                short(&head),
                short(&commit)
            ),
        );
    }
    let on_target = is_ancestor(repo, &commit, &target_ref(repo, target))?;
    let live = match read_ledger(runtime) {
        None => None,
        Some(live) => {
            if git(repo, &["cat-file", "-e", &format!("{live}^{{commit}}")])
                .is_ok_and(|o| o.status.success())
            {
                let anc = is_ancestor(repo, &live, &commit)?;
                Some((live, anc))
            } else {
                return Err(format!(
                    "the live build {} of {runtime} is not in {}: fetch first",
                    short(&live),
                    repo.display()
                ));
            }
        }
    };
    let lock_held = a.no_lock_check || a.lock.join("owner").exists() || a.lock.is_dir();
    Ok(Facts {
        dirty_paths,
        commit,
        on_target,
        live,
        lock_held,
    })
}

fn run() -> Result<ExitCode, String> {
    let a = parse()?;
    if a.cmd == "status" {
        let dir = ledger_dir();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().collect())
            .unwrap_or_default();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            println!(
                "{}: {}",
                e.file_name().to_string_lossy().trim_end_matches(".commit"),
                text.lines().collect::<Vec<_>>().join(" | ")
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    let runtime = a.runtime.clone().ok_or("--runtime is required")?;
    let target = match (&a.target, default_target(&runtime)) {
        (Some(t), _) => t.clone(),
        (None, Some(t)) => t.to_string(),
        (None, None) => return Err(format!("unknown runtime {runtime}: pass --target BRANCH")),
    };
    let f = facts(&a, &runtime, &target)?;
    match a.cmd.as_str() {
        "check" => match decide(&f, &target) {
            Verdict::Allowed => {
                println!("deploy-guard: OK {runtime} {} (live {} is an ancestor, on {target}, clean, lock held)", short(&f.commit), short(f.live.as_ref().map(|l| l.0.as_str()).unwrap_or("")));
                Ok(ExitCode::SUCCESS)
            }
            Verdict::NoLedger => {
                eprintln!("deploy-guard: no ledger entry for {runtime}. Find the commit that is live now, then: deploy-guard record --runtime {runtime} --commit <live sha> --bootstrap");
                Ok(ExitCode::from(3))
            }
            Verdict::Refused(why) => {
                eprintln!("deploy-guard: REFUSED {runtime} {}:", short(&f.commit));
                for w in why {
                    eprintln!("  - {w}");
                }
                Ok(ExitCode::from(1))
            }
        },
        "record" => {
            // The recorded commit must itself pass the rule (except the lock and,
            // for the first entry, the ancestor check).
            let mut g = f.clone();
            g.lock_held = true;
            if a.commit.is_some() {
                g.dirty_paths.retain(|p| !p.starts_with("HEAD is "));
                if a.bootstrap {
                    g.dirty_paths.clear();
                }
            }
            match decide(&g, &target) {
                Verdict::Refused(why) => {
                    eprintln!(
                        "deploy-guard: not recording {runtime} {}:",
                        short(&f.commit)
                    );
                    for w in why {
                        eprintln!("  - {w}");
                    }
                    return Ok(ExitCode::from(1));
                }
                Verdict::NoLedger if !a.bootstrap => {
                    eprintln!("deploy-guard: no ledger entry for {runtime} yet; pass --bootstrap for the first record");
                    return Ok(ExitCode::from(3));
                }
                _ => {}
            }
            let dir = ledger_dir();
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let who = format!(
                "{}@{}",
                std::env::var("USER").unwrap_or_default(),
                std::fs::read_to_string("/etc/hostname")
                    .unwrap_or_default()
                    .trim()
            );
            let at = git_ok(&a.repo, &["log", "-1", "--format=%cI", &f.commit]).unwrap_or_default();
            let now = Command::new("date")
                .args(["-u", "+%FT%TZ"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let body = format!(
                "commit {}\nrepo {}\ntarget {target}\nrecorded {now}\ncommitted {at}\nby {who}\nnote {}\n",
                f.commit,
                a.repo.canonicalize().unwrap_or(a.repo.clone()).display(),
                a.note.as_deref().unwrap_or("")
            );
            let path = dir.join(format!("{runtime}.commit"));
            let tmp = dir.join(format!(".{runtime}.commit.tmp"));
            std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("deploy-guard: recorded {runtime} = {}", short(&f.commit));
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("deploy-guard: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(dirty: bool, on_target: bool, live: Option<bool>, lock: bool) -> Facts {
        Facts {
            dirty_paths: if dirty {
                vec!["web/src/views/Shell.tsx".into()]
            } else {
                vec![]
            },
            commit: "c".repeat(40),
            on_target,
            live: live.map(|anc| ("a".repeat(40), anc)),
            lock_held: lock,
        }
    }

    /// The decision equals the Verus kernel's rule for every combination.
    #[test]
    fn decide_matches_the_kernel_rule() {
        for dirty in [false, true] {
            for on_target in [false, true] {
                for live in [None, Some(false), Some(true)] {
                    for lock in [false, true] {
                        let v = decide(&facts(dirty, on_target, live, lock), "main");
                        let rule = !dirty && on_target && live != Some(false) && lock;
                        match v {
                            Verdict::Allowed => assert!(rule && live == Some(true)),
                            Verdict::NoLedger => assert!(rule && live.is_none()),
                            Verdict::Refused(why) => {
                                assert!(!rule);
                                assert!(!why.is_empty());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn todays_bug_is_refused() {
        // 25 Oct: the live server had calls (not an ancestor of the clean main build).
        let v = decide(&facts(false, true, Some(false), true), "main");
        let Verdict::Refused(why) = v else {
            panic!("must refuse")
        };
        assert!(why[0].contains("not an ancestor"));
    }
}

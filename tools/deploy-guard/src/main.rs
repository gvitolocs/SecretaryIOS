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

/// Whether someone holds the merge lock. Two styles are in use: the older
/// directory lock (`mkdir /tmp/lucina-merge.lock`, optionally with an `owner`
/// file inside) and a `flock(1)` on the file itself (`exec 9>/tmp/lucina-merge.lock;
/// flock 9`). A flock is held when a non-blocking `flock -n` on the same path
/// fails; the caller's own held descriptor counts, since it is a separate open.
fn lock_is_held(lock: &Path) -> bool {
    if lock.is_dir() || lock.join("owner").exists() {
        return true;
    }
    if !lock.is_file() {
        return false;
    }
    match Command::new("flock").arg("-n").arg(lock).arg("true").status() {
        Ok(st) => !st.success(),
        Err(_) => false,
    }
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
    let lock_held = a.no_lock_check || lock_is_held(&a.lock);
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
            let d = Deploy {
                runtime: runtime.clone(),
                commit: f.commit.clone(),
                subject: git_ok(&a.repo, &["log", "-1", "--format=%s", &f.commit])
                    .unwrap_or_default(),
                repo: a
                    .repo
                    .canonicalize()
                    .unwrap_or(a.repo.clone())
                    .display()
                    .to_string(),
                target: target.clone(),
                recorded: now.clone(),
                committed: at.clone(),
                by: who.clone(),
                note: a.note.clone().unwrap_or_default(),
            };
            append_history(&dir, &d);
            report_to_ops(&d);
            println!("deploy-guard: recorded {runtime} = {}", short(&f.commit));
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// One recorded deploy (the ledger entry, for the history and the ops record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deploy {
    pub runtime: String,
    pub commit: String,
    pub subject: String,
    pub repo: String,
    pub target: String,
    pub recorded: String,
    pub committed: String,
    pub by: String,
    pub note: String,
}

/// A JSON string literal.
fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The ops record's project and host of a runtime (`DEPLOY_GUARD_OPS_PROJECT` overrides the project).
pub fn ops_place(runtime: &str, project_override: Option<&str>) -> (String, &'static str) {
    let project = match project_override.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None if runtime == "lucina-agentd" => "lucina-agents".to_owned(),
        None if matches!(runtime, "secretary-server" | "secretary-mcp" | "web") => "secretary".to_owned(),
        None => "lucina".to_owned(),
    };
    let host = match runtime {
        "secretary-server" | "secretary-mcp" | "web" => "p1",
        _ => "nezopt",
    };
    (project, host)
}

/// One line of `history.jsonl`.
pub fn history_line(d: &Deploy) -> String {
    format!(
        "{{\"runtime\":{},\"commit\":{},\"repo\":{},\"target\":{},\"recorded\":{},\"committed\":{},\"by\":{},\"note\":{}}}",
        js(&d.runtime),
        js(&d.commit),
        js(&d.repo),
        js(&d.target),
        js(&d.recorded),
        js(&d.committed),
        js(&d.by),
        js(&d.note)
    )
}

/// The NewRecord `lucina-rs ops record` gets for a deploy.
pub fn ops_record_json(d: &Deploy, project_override: Option<&str>) -> String {
    let (project, host) = ops_place(&d.runtime, project_override);
    let subject: String = d.subject.chars().take(150).collect();
    let title = if subject.is_empty() {
        format!("{} deployed {}", d.runtime, short(&d.commit))
    } else {
        format!("{} deployed {}: {subject}", d.runtime, short(&d.commit))
    };
    let mut body = format!("{} on {}", d.repo, d.target);
    if !d.note.is_empty() {
        body.push('\n');
        body.push_str(&d.note);
    }
    body.push_str(&format!("\nby {}", d.by));
    let mut out = format!(
        "{{\"kind\":\"deploy\",\"project\":{},\"title\":{},\"body\":{},\"commit\":{},\"host\":{},\"dedup_key\":{}",
        js(&project),
        js(&title),
        js(&body),
        js(&d.commit),
        js(host),
        js(&format!("deploy:{}:{}", d.runtime, d.commit))
    );
    if !d.recorded.is_empty() {
        out.push_str(&format!(",\"at\":{}", js(&d.recorded)));
    }
    out.push('}');
    out
}

/// Append-only history next to the per-runtime files (best effort).
fn append_history(dir: &Path, d: &Deploy) {
    use std::io::Write as _;
    let path = dir.join("history.jsonl");
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{}", history_line(d)));
    if let Err(e) = r {
        eprintln!(
            "deploy-guard: history not written ({}): {e}",
            path.display()
        );
    }
}

/// `lucina-rs ops record --source deploy-guard` (best effort, at most 10 s; never changes the exit code).
fn report_to_ops(d: &Deploy) {
    use std::io::Write as _;
    if std::env::var("DEPLOY_GUARD_OPS").is_ok_and(|v| v.trim() == "off") {
        return;
    }
    let bin = match std::env::var_os("DEPLOY_GUARD_OPS_BIN") {
        Some(b) => PathBuf::from(b),
        None => {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".local/bin/lucina-rs");
            if home.is_file() {
                home
            } else {
                PathBuf::from("lucina-rs")
            }
        }
    };
    let json = ops_record_json(d, std::env::var("DEPLOY_GUARD_OPS_PROJECT").ok().as_deref());
    let child = Command::new(&bin)
        .args(["ops", "record", "--source", "deploy-guard"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("deploy-guard: ops record skipped: {}: {e}", bin.display());
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(json.as_bytes());
    }
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(10) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("deploy-guard: ops record skipped: no answer in 10 s");
                return;
            }
        }
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let id = text
                .split("\"id\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .unwrap_or("?");
            println!("deploy-guard: ops record {id}");
        }
        Ok(out) => eprintln!(
            "deploy-guard: ops record skipped: {}",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(300)
                .collect::<String>()
        ),
        Err(e) => eprintln!("deploy-guard: ops record skipped: {e}"),
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

    fn deploy(runtime: &str) -> Deploy {
        Deploy {
            runtime: runtime.into(),
            commit: "90e94263abcdef0123456789abcdef0123456789".into(),
            subject: "Merge branch \"agents-p5\" into int5-rs".into(),
            repo: "/home/nez/Projects/.worktrees/SecretaryIOS/lucina".into(),
            target: "lucina-rs".into(),
            recorded: "2026-10-28T15:53:08Z".into(),
            committed: "2026-10-28T15:23:30+00:00".into(),
            by: "nez@nez_opt".into(),
            note: "phase 5\tlive".into(),
        }
    }

    #[test]
    fn ops_places() {
        assert_eq!(
            ops_place("lucina-agentd", None),
            ("lucina-agents".into(), "nezopt")
        );
        assert_eq!(ops_place("lucina-rs", None), ("lucina".into(), "nezopt"));
        assert_eq!(ops_place("secretary-server", None), ("secretary".into(), "p1"));
        assert_eq!(ops_place("web", None), ("secretary".into(), "p1"));
        assert_eq!(
            ops_place("lucina-rs", Some("pokoin")),
            ("pokoin".into(), "nezopt")
        );
        assert_eq!(
            ops_place("lucina-rs", Some(" ")),
            ("lucina".into(), "nezopt")
        );
    }

    #[test]
    fn ops_record_and_history_json() {
        let j = ops_record_json(&deploy("secretary-mcp"), None);
        assert_eq!(
            j,
            "{\"kind\":\"deploy\",\"project\":\"secretary\",\"title\":\"secretary-mcp deployed 90e94263ab: Merge branch \\\"agents-p5\\\" into int5-rs\",\"body\":\"/home/nez/Projects/.worktrees/SecretaryIOS/lucina on lucina-rs\\nphase 5\\tlive\\nby nez@nez_opt\",\"commit\":\"90e94263abcdef0123456789abcdef0123456789\",\"host\":\"p1\",\"dedup_key\":\"deploy:secretary-mcp:90e94263abcdef0123456789abcdef0123456789\",\"at\":\"2026-10-28T15:53:08Z\"}"
        );
        let h = history_line(&deploy("lucina-rs"));
        assert!(
            h.starts_with("{\"runtime\":\"lucina-rs\",\"commit\":\"90e94263"),
            "{h}"
        );
        assert!(h.ends_with("\"note\":\"phase 5\\tlive\"}"), "{h}");
        assert_eq!(js("a\u{1}b"), "\"a\\u0001b\"");
    }

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
    #[test]
    fn flock_held_lock_file_counts_as_held() {
        let dir = std::env::temp_dir().join(format!("dg-flock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("lucina-merge.lock");
        std::fs::write(&lock, "").unwrap();
        assert!(!super::lock_is_held(&lock), "a free lock file is not held");
        let mut holder = Command::new("flock")
            .arg("-o")
            .arg(&lock)
            .args(["sleep", "5"])
            .spawn()
            .expect("flock(1) available");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(super::lock_is_held(&lock), "a flock-held file is held");
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(!super::lock_is_held(&lock), "released again");
        let d = dir.join("dir-style.lock");
        std::fs::create_dir_all(&d).unwrap();
        assert!(super::lock_is_held(&d), "the directory style still counts");
        assert!(!super::lock_is_held(&dir.join("missing.lock")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

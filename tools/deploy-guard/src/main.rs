//! `deploy-guard`: refuses a deploy that would roll production back.
//!
//! The rule comes from the deploy model (verification/models/src/deploy.rs,
//! formal/deploy/Deploy.tla; kernel proved in verification/verus/deploy_guard.rs):
//! a build may go live only if
//!
//! 1. the merge lock is held (checked first: check-then-push is only safe inside it),
//! 2. the build tree is clean (no uncommitted or untracked source),
//! 3. its commit is `main`'s tip (`origin/main` fetched fresh), the one branch every runtime deploys from
//!    (docs/repo-workflow.md),
//! 4. every live build is an ancestor of it (nothing deployed before is missing). The live build is the
//!    newest `deploy/<runtime>/*` tag (fetched fresh) and the ledger entry.
//!
//! Two overrides, both logged to the ops record: `--allow-not-main=<reason>` waives rule 3, and
//! `--rollback-to <deploy tag> --reason <text>` (a deliberate rollback to a build that was live before)
//! waives rules 3 and 4.
//!
//! ```sh
//! deploy-guard check  --runtime secretary-server [--repo DIR]   # before the deploy
//! deploy-guard record --runtime secretary-server [--repo DIR]   # right after it: ledger, tag, ops record
//! deploy-guard plan   [--runtime NAME] [--repo DIR]             # which runtimes main changed since their live build
//! deploy-guard status
//! ```
//!
//! The ledger of what is live is one file per runtime in
//! `$DEPLOY_GUARD_LEDGER` (default `~/.local/state/deploy-guard`). `record` also
//! pushes the tag `deploy/<runtime>/<UTC stamp>-<sha8>` (`DEPLOY_GUARD_PUSH=off`
//! skips the pushes, not the fetch). Exit codes: 0 allowed / recorded, 1 refused, 2 usage or
//! git error, 3 no ledger entry yet.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const LOCK: &str = "/tmp/lucina-merge.lock";

/// The one branch every runtime deploys from. The per-target deploy lines
/// (lucina-rs, server-live, web-live, ios-app, main-next) were retired on 2026-10-30.
const TRUNK: &str = "main";

/// While this file is absent from the ledger directory, `record` also fast-forwards
/// the runtime's retired deploy line to the deployed commit, so the old names keep
/// pointing at what is live for sessions that still use them. It is created once
/// every session has acknowledged the trunk workflow and the aliases are deleted.
const ALIASES_RETIRED: &str = "aliases-retired";

/// The retired deploy line a runtime used to deploy from (kept as a read-only alias until every session has moved to main).
fn legacy_line(runtime: &str) -> &'static str {
    match runtime {
        "secretary-server" | "secretary-mcp" => "server-live",
        "web" => "web-live",
        "ios" => "ios-app",
        "lucina-media" => "main-next",
        // Every other runtime runs on nezopt and deployed from lucina-rs.
        _ => "lucina-rs",
    }
}

/// The deploy tag of a runtime's build, e.g. `deploy/web/20261030T0912Z-f401dc24`.
pub fn deploy_tag(runtime: &str, stamp: &str, commit: &str) -> String {
    format!("deploy/{runtime}/{stamp}-{}", &commit[..commit.len().min(8)])
}

/// A live build of the runtime: the newest deploy tag or the ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    /// `the live build deploy/<runtime>/<stamp>-<sha8>` or `the ledger entry`.
    pub name: String,
    pub commit: String,
    /// Whether it is an ancestor of the deploy commit.
    pub ancestor: bool,
    /// `git log --oneline --reverse <deploy>..<live>`: the live commits the deploy lacks.
    pub missing: Vec<String>,
}

/// What `check` found, as plain facts (the decision is a pure function of them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub dirty_paths: Vec<String>,
    pub commit: String,
    /// The main ref as shown in messages (`origin/main`, or `main` without an origin).
    pub main_ref: String,
    /// The main ref's commit.
    pub main: String,
    /// `commit` is the main ref's commit.
    pub main_tip: bool,
    /// `commit` is an ancestor of the main ref.
    pub on_main: bool,
    /// `git log --oneline --reverse <commit>..<main>`: main's commits the deploy lacks.
    pub newer: Vec<String>,
    /// The live builds; empty: no deploy tag and no ledger entry.
    pub live: Vec<Live>,
    pub lock_held: bool,
}

/// The overrides given on the command line (each with its reason).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub allow_not_main: Option<String>,
    /// The deploy tag rolled back to and the reason.
    pub rollback: Option<(String, String)>,
}

impl Overrides {
    /// The override as one line for the ops record and the history.
    fn note(&self) -> Option<String> {
        match (&self.rollback, &self.allow_not_main) {
            (Some((tag, why)), _) => Some(format!("rollback-to {tag}: {why}")),
            (None, Some(why)) => Some(format!("allow-not-main: {why}")),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Refused(Vec<String>),
    NoLedger,
}

/// At most `max` lines, indented, then `... and K more`.
fn listing(lines: &[String], max: usize, indent: &str) -> String {
    let mut out: String = lines.iter().take(max).map(|l| format!("\n{indent}{l}")).collect();
    if lines.len() > max {
        out.push_str(&format!("\n{indent}... and {} more", lines.len() - max));
    }
    out
}

/// The guard's rule (Verus kernel D-K1, `guard_allows`), with the two overrides.
pub fn decide(f: &Facts, o: &Overrides, target: &str) -> Verdict {
    let mut why = Vec::new();
    if !f.dirty_paths.is_empty() {
        why.push(format!(
            "the build tree has uncommitted or untracked files ({}{}); commit them (or move them to a wip branch) first",
            f.dirty_paths.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if f.dirty_paths.len() > 5 { ", ..." } else { "" }
        ));
    }
    let waived = o.allow_not_main.is_some() || o.rollback.is_some();
    if !f.main_tip && !waived {
        if f.on_main {
            why.push(format!(
                "{} is not {target}'s tip {}: {target} has {} newer commit(s) (git log --oneline {}..{}). \
                 Rebase or merge onto {}, push, and deploy {target}'s tip.{}",
                short(&f.commit),
                short(&f.main),
                f.newer.len(),
                short(&f.commit),
                f.main_ref,
                f.main_ref,
                listing(&f.newer, 10, "      ")
            ));
        } else {
            why.push(format!(
                "{} is not on {target}: merge it into {target} first and build from the merged tip (or --allow-not-main=<reason>)",
                short(&f.commit)
            ));
        }
    }
    if o.rollback.is_none() {
        for l in f.live.iter().filter(|l| !l.ancestor) {
            why.push(format!(
                "{} ({}) is not an ancestor of {}: deploying would drop {} live commit(s):{}\n    \
                 Merge {} (which contains the live build) or the live tag into your branch, push {target}, and deploy {target}'s tip. \
                 A deliberate rollback: --rollback-to <deploy tag> --reason <text>.",
                l.name,
                short(&l.commit),
                short(&f.commit),
                l.missing.len(),
                listing(&l.missing, 20, "      "),
                f.main_ref
            ));
        }
    }
    if !f.lock_held {
        why.push(format!("{LOCK} is not held: take it before checking (check-then-push is only safe inside the lock)"));
    }
    if !why.is_empty() {
        Verdict::Refused(why)
    } else if f.live.is_empty() && o.rollback.is_none() {
        Verdict::NoLedger
    } else {
        Verdict::Allowed
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

const USAGE: &str = "usage: deploy-guard check|record|plan|status --runtime NAME [--repo DIR] [--commit REV] [--target BRANCH] \
[--note TEXT] [--bootstrap] [--allow-untracked] [--no-lock-check] [--allow-not-main=REASON] [--rollback-to DEPLOY_TAG --reason TEXT]";

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
    overrides: Overrides,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or(USAGE)?;
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
        overrides: Overrides::default(),
    };
    let mut rollback_to = None;
    let mut reason = None;
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
            "--allow-not-main" => a.overrides.allow_not_main = Some(val()?),
            "--rollback-to" => rollback_to = Some(val()?),
            "--reason" => reason = Some(val()?),
            other => match other.strip_prefix("--allow-not-main=") {
                Some(r) => a.overrides.allow_not_main = Some(r.to_string()),
                None => return Err(format!("unknown flag {other}\n{USAGE}")),
            },
        }
    }
    if a.overrides.allow_not_main.as_deref().is_some_and(|r| r.trim().is_empty()) {
        return Err("--allow-not-main needs a reason: --allow-not-main=<reason>".into());
    }
    a.overrides.rollback = match (rollback_to, reason) {
        (Some(tag), Some(why)) if !tag.trim().is_empty() && !why.trim().is_empty() => Some((tag, why)),
        (None, None) => None,
        _ => return Err("--rollback-to <deploy tag> and --reason <text> go together, both non-empty".into()),
    };
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

fn has_origin(repo: &Path) -> bool {
    git(repo, &["remote", "get-url", "origin"]).is_ok_and(|o| o.status.success())
}

/// `origin/main` and the runtime's deploy tags, fresh (`DEPLOY_GUARD_PUSH=off` does not skip this).
/// Without an origin (local test repos) the guard compares with the local main.
fn fetch(repo: &Path, runtime: &str) -> Result<(), String> {
    if !has_origin(repo) {
        eprintln!("deploy-guard: warning: no origin remote; comparing with local {TRUNK}");
        return Ok(());
    }
    let heads = format!("+refs/heads/{TRUNK}:refs/remotes/origin/{TRUNK}");
    let tags = format!("+refs/tags/deploy/{runtime}/*:refs/tags/deploy/{runtime}/*");
    let out = git(repo, &["fetch", "-q", "origin", &heads, &tags])?;
    if !out.status.success() {
        return Err(format!(
            "fetch origin failed: {}; the guard needs origin/{TRUNK} and the deploy tags fresh",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The newest `deploy/<runtime>/*` tag (names are `<YYYYMMDDTHHMMZ>-<sha8>`, so lexical order is time order).
fn newest_tag(repo: &Path, runtime: &str) -> Result<Option<String>, String> {
    let prefix = format!("refs/tags/deploy/{runtime}/");
    let name = git_ok(
        repo,
        &["for-each-ref", "--sort=-refname", "--count=1", "--format=%(refname)", &prefix],
    )?;
    Ok(name.strip_prefix("refs/tags/").map(String::from))
}

/// `git log --oneline --reverse <range>`.
fn log_lines(repo: &Path, range: &str) -> Result<Vec<String>, String> {
    Ok(git_ok(repo, &["log", "--oneline", "--abbrev=8", "--reverse", range])?
        .lines()
        .map(String::from)
        .collect())
}

/// A rollback's deploy commit: the tag's, which must be a `deploy/<runtime>/` tag. `Ok(None)`: refused.
fn rollback_commit(repo: &Path, runtime: &str, tag: &str) -> Result<Option<String>, String> {
    let tag = tag.strip_prefix("refs/tags/").unwrap_or(tag);
    if !tag.starts_with(&format!("deploy/{runtime}/")) {
        return Ok(None);
    }
    let out = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/tags/{tag}^{{commit}}")])?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

fn facts(a: &Args, runtime: &str, target: &str, rev: &str, lock_held: bool) -> Result<Facts, String> {
    let repo = &a.repo;
    let commit = git_ok(repo, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?;
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
    // `git_ok` trims, so the first line may have lost its leading status space: split at the first space instead.
    .map(|l| l.trim_start().split_once(' ').map_or(l, |(_, p)| p.trim_start()).to_string())
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
    let main_ref = target_ref(repo, target);
    let main = git_ok(repo, &["rev-parse", "--verify", &format!("{main_ref}^{{commit}}")])?;
    let on_main = is_ancestor(repo, &commit, &main)?;
    let newer = if on_main {
        log_lines(repo, &format!("{commit}..{main}"))?
    } else {
        Vec::new()
    };
    let mut live = Vec::new();
    if let Some(tag) = newest_tag(repo, runtime)? {
        let c = git_ok(repo, &["rev-parse", "--verify", &format!("refs/tags/{tag}^{{commit}}")])?;
        live.push((format!("the live build {tag}"), c));
    }
    if let Some(l) = read_ledger(runtime) {
        if !git(repo, &["cat-file", "-e", &format!("{l}^{{commit}}")]).is_ok_and(|o| o.status.success()) {
            return Err(format!(
                "the live build {} of {runtime} is not in {}: fetch first",
                short(&l),
                repo.display()
            ));
        }
        let l = git_ok(repo, &["rev-parse", "--verify", &format!("{l}^{{commit}}")])?;
        if live.iter().all(|(_, c)| *c != l) {
            live.push(("the ledger entry".to_string(), l));
        }
    }
    let live = live
        .into_iter()
        .map(|(name, c)| {
            let ancestor = is_ancestor(repo, &c, &commit)?;
            let missing = if ancestor {
                Vec::new()
            } else {
                log_lines(repo, &format!("{commit}..{c}"))?
            };
            Ok(Live { name, commit: c, ancestor, missing })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Facts {
        dirty_paths,
        main_tip: commit == main,
        commit,
        main_ref: main_ref
            .strip_prefix("refs/remotes/")
            .or_else(|| main_ref.strip_prefix("refs/heads/"))
            .unwrap_or(&main_ref)
            .to_string(),
        main,
        on_main,
        newer,
        live,
        lock_held,
    })
}

fn who() -> String {
    format!(
        "{}@{}",
        std::env::var("USER").unwrap_or_default(),
        std::fs::read_to_string("/etc/hostname")
            .unwrap_or_default()
            .trim()
    )
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
    if a.cmd == "plan" {
        return plan(&a);
    }
    if a.cmd != "check" && a.cmd != "record" {
        return Err(format!("unknown command {}", a.cmd));
    }
    let runtime = a.runtime.clone().ok_or("--runtime is required")?;
    // Only a bootstrap record may name another branch: it records what is live, it deploys nothing.
    let target = match &a.target {
        Some(t) if t != TRUNK && !(a.cmd == "record" && a.bootstrap) => {
            eprintln!(
                "deploy-guard: REFUSED {runtime}: --target {t}: every runtime deploys from {TRUNK} since 2026-10-30 \
                 (docs/repo-workflow.md). Merge your branch into {TRUNK}, push it and deploy from {TRUNK}'s tree."
            );
            return Ok(ExitCode::from(1));
        }
        Some(t) => t.clone(),
        None => TRUNK.to_string(),
    };
    // The lock first: every check below must run inside it.
    let lock_held = a.no_lock_check || lock_is_held(&a.lock);
    if a.cmd == "check" && !lock_held {
        let rev = a.commit.as_deref().unwrap_or("HEAD");
        let sha = git_ok(&a.repo, &["rev-parse", "--verify", rev]).unwrap_or_default();
        eprintln!("deploy-guard: REFUSED {runtime} {}:", short(&sha));
        eprintln!("  - {LOCK} is not held: take it before checking (check-then-push is only safe inside the lock)");
        return Ok(ExitCode::from(1));
    }
    fetch(&a.repo, &runtime)?;
    let o = &a.overrides;
    let rev = match &o.rollback {
        None => a.commit.clone().unwrap_or_else(|| "HEAD".into()),
        Some((tag, _)) => {
            let Some(c) = rollback_commit(&a.repo, &runtime, tag)? else {
                eprintln!(
                    "deploy-guard: REFUSED {runtime}: --rollback-to {tag}: not a deploy/{runtime}/ tag; \
                     a rollback goes to a build that was live before"
                );
                return Ok(ExitCode::from(1));
            };
            if let Some(rev) = &a.commit {
                let given = git_ok(&a.repo, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?;
                if given != c {
                    return Err(format!(
                        "--commit {rev} is {} but --rollback-to {tag} is {}: give one of them",
                        short(&given),
                        short(&c)
                    ));
                }
            }
            c
        }
    };
    let f = facts(&a, &runtime, &target, &rev, lock_held)?;
    if a.cmd == "check" {
        warn_alias(&a.repo, legacy_line(&runtime));
    }
    // An override is logged only when it is what let the deploy through.
    let overridden = o.note().filter(|_| matches!(decide(&f, &Overrides::default(), &target), Verdict::Refused(_)));
    if a.cmd == "check" {
        return Ok(match decide(&f, o, &target) {
            Verdict::Allowed => {
                let live = f.live.first().map(|l| short(&l.commit)).unwrap_or("-");
                if let Some((tag, why)) = &o.rollback {
                    println!("deploy-guard: OK {runtime} {} ROLLBACK to {tag} (reason: {why})", short(&f.commit));
                } else if let (Some(why), true) = (&o.allow_not_main, overridden.is_some()) {
                    println!(
                        "deploy-guard: OK {runtime} {} OVERRIDE allow-not-main (reason: {why}; live {live} is an ancestor, clean, lock held)",
                        short(&f.commit)
                    );
                } else {
                    println!(
                        "deploy-guard: OK {runtime} {} ({target}'s tip, live {live} is an ancestor, clean, lock held)",
                        short(&f.commit)
                    );
                }
                if overridden.is_some() {
                    report_override(&a, &runtime, &f.commit, "check");
                }
                ExitCode::SUCCESS
            }
            Verdict::NoLedger => {
                eprintln!("deploy-guard: no ledger entry for {runtime}. Find the commit that is live now, then: deploy-guard record --runtime {runtime} --commit <live sha> --bootstrap");
                ExitCode::from(3)
            }
            Verdict::Refused(why) => {
                eprintln!("deploy-guard: REFUSED {runtime} {}:", short(&f.commit));
                for w in why {
                    eprintln!("  - {w}");
                }
                ExitCode::from(1)
            }
        });
    }
    // record: the recorded commit must itself pass the rule, except the lock, for the first
    // entry the ancestor check, and main's tip: main may move between check and record, so
    // being on main is enough.
    let mut g = f.clone();
    g.lock_held = true;
    g.main_tip = g.on_main;
    if a.commit.is_some() || o.rollback.is_some() {
        g.dirty_paths.retain(|p| !p.starts_with("HEAD is "));
        if a.bootstrap {
            g.dirty_paths.clear();
        }
    }
    match decide(&g, o, &target) {
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
    let overridden = o.note().filter(|_| matches!(decide(&g, &Overrides::default(), &target), Verdict::Refused(_)));
    let dir = ledger_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let who = who();
    let at = git_ok(&a.repo, &["log", "-1", "--format=%cI", &f.commit]).unwrap_or_default();
    let now = utc("+%FT%TZ");
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
        override_note: overridden.clone(),
    };
    append_history(&dir, &d);
    report_to_ops(&ops_record_json(&d, std::env::var("DEPLOY_GUARD_OPS_PROJECT").ok().as_deref()));
    if overridden.is_some() {
        report_override(&a, &runtime, &f.commit, "record");
    }
    println!("deploy-guard: recorded {runtime} = {}", short(&f.commit));
    // Every deploy is tagged (rollbacks too), so the newest tag is always what is live.
    let on_main = target == TRUNK && f.on_main;
    tag_and_alias(&a.repo, &runtime, &f.commit, &a.note.clone().unwrap_or_default(), on_main);
    Ok(ExitCode::SUCCESS)
}

/// The ops record of a deploy an override let through.
fn report_override(a: &Args, runtime: &str, commit: &str, step: &str) {
    let repo = a.repo.canonicalize().unwrap_or(a.repo.clone()).display().to_string();
    report_to_ops(&override_record_json(
        runtime,
        commit,
        &a.overrides,
        step,
        &format!("{repo}\nby {}", who()),
        std::env::var("DEPLOY_GUARD_OPS_PROJECT").ok().as_deref(),
    ));
}

fn utc(fmt: &str) -> String {
    Command::new("date")
        .args(["-u", fmt])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn pushes_enabled(repo: &Path) -> bool {
    !std::env::var("DEPLOY_GUARD_PUSH").is_ok_and(|v| v.trim() == "off") && has_origin(repo)
}

/// After a record: the deploy tag (pushed), and in the transition week, for a commit on main, a
/// fast-forward of the runtime's retired line. Failures only warn: the deploy is done.
fn tag_and_alias(repo: &Path, runtime: &str, commit: &str, note: &str, on_main: bool) {
    let tag = deploy_tag(runtime, &utc("+%Y%m%dT%H%MZ"), commit);
    let msg = format!("{runtime} deployed {}\n\n{note}", short(commit));
    match git(repo, &["tag", "-a", &tag, commit, "-m", &msg]) {
        Ok(o) if o.status.success() => println!("deploy-guard: tagged {tag}"),
        Ok(o) => {
            eprintln!("deploy-guard: warning: tag {tag}: {}", String::from_utf8_lossy(&o.stderr).trim());
            return;
        }
        Err(e) => {
            eprintln!("deploy-guard: warning: tag {tag}: {e}");
            return;
        }
    }
    if !pushes_enabled(repo) {
        return;
    }
    let tag_ref = format!("refs/tags/{tag}");
    match git(repo, &["push", "-q", "origin", &tag_ref]) {
        Ok(o) if o.status.success() => println!("deploy-guard: pushed {tag}"),
        Ok(o) => eprintln!("deploy-guard: warning: push {tag}: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => eprintln!("deploy-guard: warning: push {tag}: {e}"),
    }
    if !on_main || ledger_dir().join(ALIASES_RETIRED).exists() {
        return;
    }
    let line = legacy_line(runtime);
    // Only lines that exist on origin get an alias (main-next was local-only).
    if git(repo, &["rev-parse", "-q", "--verify", &format!("refs/remotes/origin/{line}")]).map_or(true, |o| !o.status.success()) {
        return;
    }
    let spec = format!("{commit}:refs/heads/{line}");
    match git(repo, &["push", "-q", "origin", &spec]) {
        Ok(o) if o.status.success() => println!("deploy-guard: alias {line} -> {}", short(commit)),
        _ => eprintln!(
            "deploy-guard: warning: alias {line} was not fast-forwarded to {}: it has commits that are not on {TRUNK}. \
             Merge them into {TRUNK} (git log origin/{TRUNK}..origin/{line}).",
            short(commit)
        ),
    }
}

/// `check`: a retired line that gained commits main lacks means someone still deploys the old way.
fn warn_alias(repo: &Path, line: &str) {
    let alias = format!("refs/remotes/origin/{line}");
    let main = format!("refs/remotes/origin/{TRUNK}");
    if !git(repo, &["rev-parse", "--verify", "--quiet", &alias]).is_ok_and(|o| o.status.success()) {
        return;
    }
    let range = format!("{main}..{alias}");
    if let Ok(n) = git_ok(repo, &["rev-list", "--count", &range]) {
        if n != "0" {
            eprintln!(
                "deploy-guard: warning: the retired line {line} has {n} commit(s) not on {TRUNK} \
                 (git log origin/{TRUNK}..origin/{line}); merge them into {TRUNK} so they are not lost."
            );
        }
    }
}

/// One line of `deploy/targets.tsv`: runtime, host, the paths its build reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub runtime: String,
    pub host: String,
    pub paths: Vec<String>,
}

/// `runtime<TAB>host<TAB>path path ...`; blank lines and `#` comments are skipped.
pub fn parse_targets(text: &str) -> Result<Vec<Target>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').filter(|c| !c.is_empty()).collect();
        let [runtime, host, paths] = cols[..] else {
            return Err(format!("deploy/targets.tsv:{}: want runtime<TAB>host<TAB>paths", i + 1));
        };
        out.push(Target {
            runtime: runtime.trim().into(),
            host: host.trim().into(),
            paths: paths.split_whitespace().map(String::from).collect(),
        });
    }
    Ok(out)
}

/// For each runtime: its live commit and how many commits on main touched its paths since.
fn plan(a: &Args) -> Result<ExitCode, String> {
    let file = a.repo.join("deploy/targets.tsv");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let main = target_ref(&a.repo, TRUNK);
    for t in parse_targets(&text)? {
        if a.runtime.as_ref().is_some_and(|r| *r != t.runtime) {
            continue;
        }
        let state = match read_ledger(&t.runtime) {
            None => "no ledger entry".to_string(),
            Some(live) if !is_ancestor(&a.repo, &live, &main).unwrap_or(false) => {
                format!("live {} is not on {TRUNK}", short(&live))
            }
            Some(live) => {
                let range = format!("{live}..{main}");
                let mut args = vec!["rev-list", "--count", range.as_str(), "--"];
                args.extend(t.paths.iter().map(String::as_str));
                match git_ok(&a.repo, &args)?.as_str() {
                    "0" => format!("up to date (live {})", short(&live)),
                    n => format!("{n} commit(s) to deploy (live {})", short(&live)),
                }
            }
        };
        println!("{:<22} {:<7} {state}", t.runtime, t.host);
    }
    Ok(ExitCode::SUCCESS)
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
    /// `allow-not-main: <reason>` or `rollback-to <tag>: <reason>` when an override let it through.
    pub override_note: Option<String>,
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
        None if matches!(runtime, "secretary-server" | "secretary-mcp" | "web" | "ios") => "secretary".to_owned(),
        None => "lucina".to_owned(),
    };
    let host = match runtime {
        "secretary-server" | "secretary-mcp" | "web" => "p1",
        "ios" => "iphone",
        _ => "nezopt",
    };
    (project, host)
}

/// One line of `history.jsonl`.
pub fn history_line(d: &Deploy) -> String {
    let overrode = d
        .override_note
        .as_deref()
        .map(|o| format!(",\"override\":{}", js(o)))
        .unwrap_or_default();
    format!(
        "{{\"runtime\":{},\"commit\":{},\"repo\":{},\"target\":{},\"recorded\":{},\"committed\":{},\"by\":{},\"note\":{}{overrode}}}",
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
    if let Some(o) = &d.override_note {
        body.push_str(&format!("\noverride: {o}"));
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

/// The ops record of a check or record that an override let through.
pub fn override_record_json(
    runtime: &str,
    commit: &str,
    o: &Overrides,
    step: &str,
    body: &str,
    project_override: Option<&str>,
) -> String {
    let (project, host) = ops_place(runtime, project_override);
    let title = match (&o.rollback, &o.allow_not_main) {
        (Some((tag, why)), _) => format!("{runtime} rollback allowed to {tag}: {why}"),
        (None, why) => format!(
            "{runtime} override allowed {}: allow-not-main: {}",
            short(commit),
            why.as_deref().unwrap_or("")
        ),
    };
    format!(
        "{{\"kind\":\"deploy\",\"project\":{},\"title\":{},\"body\":{},\"commit\":{},\"host\":{},\"dedup_key\":{}}}",
        js(&project),
        js(&title),
        js(body),
        js(commit),
        js(host),
        js(&format!("deploy-override:{runtime}:{commit}:{step}"))
    )
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
fn report_to_ops(json: &str) {
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
            override_note: None,
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

    fn facts(dirty: bool, main_tip: bool, live: &[bool], lock: bool) -> Facts {
        Facts {
            dirty_paths: if dirty {
                vec!["web/src/views/Shell.tsx".into()]
            } else {
                vec![]
            },
            commit: "c".repeat(40),
            main_ref: "origin/main".into(),
            main: if main_tip { "c".repeat(40) } else { "b".repeat(40) },
            main_tip,
            on_main: main_tip,
            newer: if main_tip { vec![] } else { vec!["bbbbbbbb other chat".into()] },
            live: live
                .iter()
                .map(|&ancestor| Live {
                    name: "the live build deploy/web/20261101T0027Z-aaaaaaaa".into(),
                    commit: "a".repeat(40),
                    ancestor,
                    missing: if ancestor { vec![] } else { vec!["aaaaaaaa live fix".into()] },
                })
                .collect(),
            lock_held: lock,
        }
    }

    /// The decision equals the rule for every combination: allowed iff the tree is clean, the
    /// lock is held, the commit is main's tip (or an override waives it) and every live build is
    /// an ancestor (or it is a deliberate rollback).
    #[test]
    fn decide_matches_the_kernel_rule() {
        let lives: [&[bool]; 5] = [&[], &[false], &[true], &[true, false], &[true, true]];
        for dirty in [false, true] {
            for main_tip in [false, true] {
                for live in lives {
                    for lock in [false, true] {
                        for allow in [false, true] {
                            for rollback in [false, true] {
                                let o = Overrides {
                                    allow_not_main: allow.then(|| "hotfix".into()),
                                    rollback: rollback.then(|| ("deploy/web/20200101T0000Z-aaaaaaaa".into(), "broke".into())),
                                };
                                let v = decide(&facts(dirty, main_tip, live, lock), &o, "main");
                                let rule = !dirty
                                    && lock
                                    && (main_tip || allow || rollback)
                                    && (rollback || live.iter().all(|&a| a));
                                match v {
                                    Verdict::Allowed => assert!(rule && (!live.is_empty() || rollback)),
                                    Verdict::NoLedger => assert!(rule && live.is_empty() && !rollback),
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
        }
    }

    #[test]
    fn refusals_say_what_to_do() {
        let none = Overrides::default();
        let mut behind = facts(false, false, &[true], true);
        behind.on_main = true;
        let Verdict::Refused(why) = decide(&behind, &none, "main") else {
            panic!("must refuse")
        };
        assert!(why[0].contains("is not main's tip bbbbbbbbbb: main has 1 newer commit(s)"), "{}", why[0]);
        assert!(why[0].ends_with("\n      bbbbbbbb other chat"), "{}", why[0]);
        let Verdict::Refused(why) = decide(&facts(false, true, &[false], true), &none, "main") else {
            panic!("must refuse")
        };
        assert!(why[0].contains("deploy/web/20261101T0027Z-aaaaaaaa (aaaaaaaaaa) is not an ancestor"), "{}", why[0]);
        assert!(why[0].contains("would drop 1 live commit(s):\n      aaaaaaaa live fix\n    Merge origin/main"), "{}", why[0]);
        assert!(why[0].contains("--rollback-to <deploy tag> --reason <text>"), "{}", why[0]);
        let lines: Vec<String> = (0..25).map(|i| format!("c{i}")).collect();
        assert!(listing(&lines, 20, "  ").ends_with("\n  c19\n  ... and 5 more"));
    }

    #[test]
    fn override_records() {
        let o = Overrides {
            allow_not_main: Some("hotfix".into()),
            rollback: None,
        };
        let j = override_record_json("web", &"c".repeat(40), &o, "check", "repo\nby t", None);
        assert!(j.contains("\"title\":\"web override allowed cccccccccc: allow-not-main: hotfix\""), "{j}");
        assert!(j.contains(&format!("\"dedup_key\":\"deploy-override:web:{}:check\"", "c".repeat(40))), "{j}");
        let o = Overrides {
            allow_not_main: None,
            rollback: Some(("deploy/web/20200101T0000Z-aaaaaaaa".into(), "B broke login".into())),
        };
        let j = override_record_json("web", &"a".repeat(40), &o, "record", "", None);
        assert!(j.contains("\"title\":\"web rollback allowed to deploy/web/20200101T0000Z-aaaaaaaa: B broke login\""), "{j}");
        let mut d = deploy("web");
        d.override_note = o.note();
        assert!(ops_record_json(&d, None).contains("live\\noverride: rollback-to deploy/web/20200101T0000Z-aaaaaaaa: B broke login\\nby "));
        assert!(history_line(&d).ends_with("\"note\":\"phase 5\\tlive\",\"override\":\"rollback-to deploy/web/20200101T0000Z-aaaaaaaa: B broke login\"}"));
    }

    #[test]
    fn deploy_tags_and_retired_lines() {
        assert_eq!(
            deploy_tag("web", "20261030T0912Z", "f401dc2435aa"),
            "deploy/web/20261030T0912Z-f401dc24"
        );
        assert_eq!(legacy_line("secretary-mcp"), "server-live");
        assert_eq!(legacy_line("web"), "web-live");
        assert_eq!(legacy_line("ios"), "ios-app");
        assert_eq!(legacy_line("lucina-media"), "main-next");
        assert_eq!(legacy_line("secretary-instagram"), "lucina-rs");
        assert_eq!(ops_place("ios", None), ("secretary".into(), "iphone"));
    }

    #[test]
    fn targets_file_parses_and_rejects_bad_lines() {
        let t = parse_targets("# runtime\thost\tpaths\n\nweb\tp1\tweb packages/lucina-stage\n").unwrap();
        assert_eq!(
            t,
            vec![Target {
                runtime: "web".into(),
                host: "p1".into(),
                paths: vec!["web".into(), "packages/lucina-stage".into()],
            }]
        );
        assert!(parse_targets("web p1 web\n").is_err());
    }

    #[test]
    fn todays_bug_is_refused() {
        // 25 Oct: the live server had calls (not an ancestor of the clean main build).
        let v = decide(&facts(false, true, &[false], true), &Overrides::default(), "main");
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

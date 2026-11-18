//! The job executor: `lagent pipeline-exec <job dir>`, started by agentd under the repo's
//! merge lock. It reads `job.json`, appends what it does to `log` and writes `result.json`.
//! Plain blocking code: it is one process doing one job.
//!
//! **Merge**: fetch the trunk; in the repo's landing worktree (`<work root>/.landing`,
//! detached) merge the queued commit into the trunk with a merge commit (`--no-ff`, never a
//! rebase: the owner's rule, for every session since 2026-11-07); run the gates ([`super::repo::plan`]); push the result to the trunk (a
//! fast-forward; when the trunk moved meanwhile, start over, at most three times).
//! The session's own worktree is never touched here.
//!
//! **Deploy**: fetch; put the deploy worktree (`<work root>/.deploy`) at the trunk's tip;
//! `deploy-guard plan` (nothing new: stop unless forced), the recipe's build,
//! `deploy-guard check` (refused: stop), install, health, `deploy-guard record` (ledger,
//! tag, ops record).

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::repo::{self, Crate, Gate, Rules};
use super::{GateResult, JobKind, Outcome, Spec};

/// The exit code: 0 when the job did what it was for.
pub fn run(dir: &Path) -> i32 {
    let spec: Spec = match std::fs::read_to_string(dir.join("job.json"))
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pipeline-exec: {}: {e}", dir.join("job.json").display());
            return 2;
        }
    };
    let mut x = match Exec::open(dir, spec) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("pipeline-exec: {e}");
            return 2;
        }
    };
    let outcome = x.job();
    x.say(&format!("{}: {}", outcome.code, outcome.message));
    let ok = outcome.ok;
    let json = serde_json::to_string_pretty(&outcome).unwrap_or_default();
    let tmp = dir.join("result.json.tmp");
    if std::fs::write(&tmp, json)
        .and_then(|_| std::fs::rename(&tmp, dir.join("result.json")))
        .is_err()
    {
        return 2;
    }
    if ok {
        0
    } else {
        1
    }
}

fn fail(code: &str, message: impl Into<String>) -> Outcome {
    Outcome {
        ok: false,
        code: code.into(),
        message: message.into(),
        ..Outcome::default()
    }
}

struct Exec {
    spec: Spec,
    log: File,
    path: String,
    members: Option<Members>,
    /// The job's allows as gate-command environment (empty while diffgate or a non-merge command runs).
    gate_env: Vec<(String, String)>,
    /// The job directory, and the waivers file written into it (decided once, before the first gate).
    dir: PathBuf,
    waivers: Option<Option<String>>,
    /// The executor's own environment: the source the built child environments come from
    /// ([`crate::env`]). A field, not std::env at spawn time, so tests pass a stand-in in.
    source_env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// The names the deploy recipe declares (`env = [...]`); only the deploy job of that
    /// recipe passes them, and only to its own commands.
    deploy_env: Vec<String>,
    /// The merge warm-up's cap in seconds. 20 minutes by default, well outside the timed gates;
    /// a test may set it to a few seconds to watch the cap fire. It is given to `timeout` as
    /// seconds and, without `--foreground`, the cap ends the warm-up and every process it forked.
    warm_up_secs: u64,
}

struct Members {
    directories: Vec<String>,
    names: HashSet<String>,
}

/// `git` output, or its error text.
type Git = Result<String, String>;

impl Exec {
    fn open(dir: &Path, spec: Spec) -> Result<Self, String> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("log"))
            .map_err(|e| format!("log: {e}"))?;
        let home = std::env::var("HOME").unwrap_or_default();
        // The same tools as a login shell: tests call mkfs.fat and friends, which live in sbin.
        let path = format!(
            "{home}/.cargo/bin:{home}/.local/bin:{home}/bin:{}:/usr/sbin:/sbin",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into())
        );
        Ok(Exec {
            gate_env: Vec::new(),
            dir: dir.to_path_buf(),
            waivers: None,
            source_env: std::env::vars_os().collect(),
            deploy_env: Vec::new(),
            warm_up_secs: 20 * 60,
            spec,
            log,
            path,
            members: None,
        })
    }

    /// The job's waivers file for the gate commands: written once (temp file and rename, 0644)
    /// the first time it is asked for, `None` when the job has no waiver or the write failed.
    fn waivers_file(&mut self) -> Option<String> {
        if let Some(done) = &self.waivers {
            return done.clone();
        }
        let lines = repo::waiver_lines(
            &self.spec.allow,
            self.spec.requested_by.as_deref().unwrap_or(""),
            &self.spec.job_id,
        );
        let mut path = None;
        if !lines.is_empty() {
            let dir = std::fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
            let file = dir.join(repo::WAIVERS_FILE);
            let tmp = dir.join(format!("{}.tmp", repo::WAIVERS_FILE));
            let text = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
            let written = std::fs::write(&tmp, text)
                .and_then(|_| {
                    std::fs::set_permissions(
                        &tmp,
                        std::os::unix::fs::PermissionsExt::from_mode(0o644),
                    )
                })
                .and_then(|_| std::fs::rename(&tmp, &file));
            match written {
                Ok(()) => {
                    self.say(&format!(
                        "waivers passed to gate commands: {} ({})",
                        lines.len(),
                        file.display()
                    ));
                    path = Some(file.display().to_string());
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    self.say(&format!("cannot write the waivers file: {e}"));
                }
            }
        }
        self.waivers = Some(path.clone());
        path
    }

    /// Finish cleanup while the executor still holds the job's flock, regardless of outcome.
    fn job(&mut self) -> Outcome {
        let outcome = match self.spec.kind {
            JobKind::Merge => self.merge(),
            JobKind::Deploy => self.deploy(),
        };
        let root = self
            .spec
            .target_root
            .clone()
            .unwrap_or_else(|| super::Config::from_env(Path::new(".")).target_root);
        self.say(&super::sweep::run(
            Path::new(&self.spec.target_dir),
            &root,
            self.members.as_ref().map(|m| &m.names),
        ));
        outcome
    }

    fn say(&mut self, msg: &str) {
        let _ = writeln!(
            self.log,
            "[{}] {msg}",
            chrono::Utc::now().format("%H:%M:%S")
        );
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Git {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "color.ui=false", "-c", "core.quotePath=false"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                err
            })
        }
    }

    /// `git`, with the command and any error in the log.
    fn git_logged(&mut self, dir: &Path, args: &[&str]) -> Git {
        self.say(&format!("$ git {}", args.join(" ")));
        let r = self.git(dir, args);
        if let Err(e) = &r {
            let _ = writeln!(self.log, "{e}");
        }
        r
    }

    /// `bash -c cmd` in `cwd` with output into the log, under `timeout`. Returns its success.
    fn sh(&mut self, cwd: &Path, cmd: &str, timeout_min: u64) -> bool {
        self.sh_run(
            cwd,
            cmd,
            &format!("{}m", timeout_min.max(1)),
            &format!("timed out after {timeout_min} min"),
        )
        .is_none()
    }

    /// [`Exec::sh`]'s worker, with the limit given to `timeout` verbatim (`limit`, e.g. `45m` or
    /// `20s`) and the phrase to report on a timeout (`timed_out`). Returns `None` when the command
    /// succeeded, or the reason it did not: `timed_out` on a timeout, otherwise the exit code. The
    /// gates run through [`Exec::sh`] and ignore the reason; the warm-up puts it in its own log
    /// line, so a cap that fires can say so. The command, its clean environment and the log lines
    /// are exactly as [`Exec::sh`] always had them.
    fn sh_run(&mut self, cwd: &Path, cmd: &str, limit: &str, timed_out: &str) -> Option<String> {
        self.say(&format!("$ {cmd}"));
        // The command gets the built environment (the allowlist from the executor's own
        // environment, plus the deploy recipe's declared names), not what the executor
        // inherited. How many names were kept back is said by count only.
        let (built, not_passed) = {
            let extra: Vec<&str> = self.deploy_env.iter().map(String::as_str).collect();
            crate::env::scrubbed_env_from(self.source_env.iter().cloned(), &extra)
        };
        if not_passed > 0 {
            self.say(&format!("env: {not_passed} variable(s) not passed"));
        }
        let (Ok(out), Ok(err)) = (self.log.try_clone(), self.log.try_clone()) else {
            // No duplicated fd means the command never starts. Without a word here the job ends
            // as GATE_FAILED with a log that says nothing about it: a gate that could not be run
            // reads exactly like a gate that ran and was refused.
            self.say("cannot run: the job log could not be duplicated for the command's output");
            return Some("cannot run: the job log could not be duplicated".to_string());
        };
        // `timeout` without `--foreground`: it runs `bash` in its own process group and, when the
        // limit is reached, signals the whole group, so a warm-up that hangs takes down every
        // process it forked, not only the one `timeout` waits on.
        let status = Command::new("timeout")
            .args(["--kill-after=30s", limit, "bash", "-c", cmd])
            .current_dir(cwd)
            .env_clear()
            .envs(built)
            .env("PATH", &self.path)
            // The gate runs with exactly the PATH above. A shell init hook left in the
            // environment (BASH_ENV, or ENV for a POSIX sh) is read before the command and can
            // rewrite PATH, which would put a machine-wide wrapper ahead of what the job asked
            // for; the machine's own PATH is already folded into `path`, so its tools stay found.
            // An exported shell function — BASH_FUNC_<name>%% — beats PATH the same way, and the
            // allowlist above is what keeps it back: it admits exact names and the LC_ and BQ_
            // prefixes only. `an_exported_cargo_function_does_not_beat_the_gate_s_path` is the proof.
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .env("CARGO_TARGET_DIR", &self.spec.target_dir)
            .env("LAGENT_JOB", &self.spec.job_id)
            .env("LAGENT_FORCE", if self.spec.force { "1" } else { "" })
            .env("CARGO_TERM_COLOR", "never")
            .env_remove("LAGENT_ALLOW")
            .env_remove("LAGENT_ALLOW_GATE_CHANGE")
            .env_remove("LAGENT_ALLOW_SPEC_CHANGE")
            .env_remove(repo::WAIVERS_ENV)
            .env_remove(repo::LEDGER_ENV)
            .envs(self.gate_env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .status();
        match status {
            Ok(s) if s.success() => None,
            Ok(s) => {
                let why = if s.code() == Some(124) {
                    timed_out.to_string()
                } else {
                    format!(
                        "exit {}",
                        s.code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into())
                    )
                };
                self.say(&format!("failed: {why}"));
                Some(why)
            }
            Err(e) => {
                self.say(&format!("cannot run: {e}"));
                Some(format!("cannot run: {e}"))
            }
        }
    }

    fn repo(&self) -> PathBuf {
        PathBuf::from(&self.spec.repo)
    }

    /// The commit ledger the gates are pointed at (`LAGENT_COMMIT_LEDGER`).
    fn ledger_path(&self) -> PathBuf {
        match &self.spec.ledger {
            Some(p) => PathBuf::from(p),
            None => crate::ledger::Ledger::in_state_dir(&crate::config::default_state_dir()).path,
        }
    }

    /// Fetch, then the trunk's commit. A remote repo's trunk is the remote's: once the fetch is
    /// done and its ref is still missing, there is nothing to land on and nothing is fetched
    /// from a local branch of the same name.
    fn trunk_tip(&mut self) -> Git {
        let repo = self.repo();
        let trunk = self.spec.trunk.clone();
        match self.spec.remote.clone() {
            Some(remote) => {
                self.git_logged(&repo, &["fetch", "-q", &remote])?;
                self.git(
                    &repo,
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("refs/remotes/{remote}/{trunk}^{{commit}}"),
                    ],
                )
                .map_err(|_| {
                    format!(
                        "{remote}/{trunk} does not exist in {}: the remote does not carry the trunk (git fetch {remote}, or check the branch name)",
                        repo.display()
                    )
                })
            }
            None => self.git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/heads/{trunk}^{{commit}}"),
                ],
            ),
        }
    }

    /// A clean detached worktree at `sha` (made on first use, reset after).
    fn fresh_worktree(&mut self, name: &str, sha: &str) -> Result<PathBuf, String> {
        let dir = PathBuf::from(&self.spec.work_root).join(name);
        let repo = self.repo();
        if !dir.join(".git").exists() {
            let _ = self.git(&repo, &["worktree", "prune"]);
            std::fs::create_dir_all(&self.spec.work_root).map_err(|e| e.to_string())?;
            self.git_logged(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &dir.display().to_string(),
                    sha,
                ],
            )?;
            return Ok(dir);
        }
        let _ = self.git(&dir, &["rebase", "--abort"]);
        let _ = self.git(&dir, &["merge", "--abort"]);
        self.git(&dir, &["checkout", "-q", "-f", "--detach", sha])?;
        self.git(&dir, &["reset", "-q", "--hard", sha])?;
        self.git(&dir, &["clean", "-q", "-fd"])?;
        Ok(dir)
    }

    fn rules(&mut self, base: &str) -> Result<Rules, String> {
        match self.git(&self.repo(), &["show", &format!("{base}:{}", repo::FILE)]) {
            Ok(text) => Rules::parse(&text),
            Err(_) => Ok(Rules::default()),
        }
    }

    /// The repo's crates. Workspace members come from `cargo metadata`; any other crate is
    /// standalone (its own lock, `--manifest-path`).
    fn crates(&self, dir: &Path) -> (Vec<Crate>, bool) {
        let files = self
            .git(dir, &["ls-files", "--", "Cargo.toml", "*/Cargo.toml"])
            .unwrap_or_default();
        let mut crates = Vec::new();
        let mut has_workspace = false;
        let members = &self.members;
        for f in files.lines() {
            let Ok(text) = std::fs::read_to_string(dir.join(f)) else {
                continue;
            };
            let d = f
                .strip_suffix("Cargo.toml")
                .unwrap_or_default()
                .trim_end_matches('/');
            if d.is_empty() && text.contains("[workspace]") {
                has_workspace = true;
            }
            if let Some(mut c) = repo::crate_of(d, &text) {
                if let Some(m) = &members {
                    c.standalone = !m.directories.iter().any(|x| x == d);
                }
                crates.push(c);
            }
        }
        (crates, has_workspace)
    }

    /// The job checkout's workspace member directories and normalized package names.
    fn members(&self, dir: &Path) -> Option<Members> {
        if !dir.join("Cargo.toml").is_file() {
            return None;
        }
        let out = Command::new("cargo")
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--offline",
            ])
            .current_dir(dir)
            .env("PATH", &self.path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let root = std::fs::canonicalize(dir).ok()?;
        let ids = v["workspace_members"].as_array()?;
        let mut members = Members {
            directories: Vec::new(),
            names: HashSet::new(),
        };
        for p in v["packages"].as_array()? {
            if !ids.contains(&p["id"]) {
                continue;
            }
            let manifest = PathBuf::from(p["manifest_path"].as_str()?);
            let d = manifest.parent()?.strip_prefix(&root).ok()?;
            members.directories.push(d.display().to_string());
            members.names.insert(p["name"].as_str()?.replace('-', "_"));
        }
        Some(members)
    }

    fn run_gates(&mut self, dir: &Path, gates: &[Gate]) -> (Vec<GateResult>, Option<String>) {
        let mut results = Vec::new();
        let waivers = self.waivers_file();
        for g in gates {
            self.say(&format!("gate: {}", g.name));
            self.gate_env = if g.name == "diffgate" {
                Vec::new()
            } else {
                let mut env = repo::allow_env(&self.spec.allow);
                if let Some(w) = &waivers {
                    env.push((repo::WAIVERS_ENV.to_string(), w.clone()));
                }
                env.push((
                    repo::LEDGER_ENV.to_string(),
                    self.ledger_path().display().to_string(),
                ));
                env
            };
            let t = Instant::now();
            let mut ok = self.sh(dir, &g.run, g.timeout_min);
            let mut flaky = false;
            // Some tests time things or bind ports and fail when nezopt is loaded: a failed cargo
            // test runs once more, and a pass then is reported as flaky, not hidden.
            if !ok && g.run.starts_with("cargo test") {
                self.say(&format!(
                    "gate failed: {}; running it once more (flaky tests under load)",
                    g.name
                ));
                ok = self.sh(dir, &g.run, g.timeout_min);
                flaky = ok;
            }
            results.push(GateResult {
                name: g.name.clone(),
                command: g.run.clone(),
                ok,
                seconds: t.elapsed().as_secs(),
                flaky,
            });
            if !ok {
                return (results, Some(g.name.clone()));
            }
        }
        self.gate_env = Vec::new();
        (results, None)
    }

    /// Warm the shared `CARGO_TARGET_DIR` with one `cargo check` of the merge base, run in the
    /// landing worktree while it is still at `base` and before the merge's timed gates. A cold
    /// target otherwise makes the first timed step (a workspace check) pay a full build, which
    /// under load can cross its per-step limit and kill the gate (finding GATE-04). This is not a
    /// gate: it is outside every timed step, its result is not recorded, and a failure or timeout
    /// is only logged before the merge goes on to `git merge` and the gates exactly as before. It
    /// is skipped when the repo runs no cargo gate (`cargo` is `off`/`none`) or the base has no
    /// `Cargo.toml` to check. It runs through [`Exec::sh_run`] for the same clean environment, PATH
    /// and `CARGO_TARGET_DIR`, under its own cap ([`Exec::warm_up_secs`], given to `timeout` as
    /// seconds) and with `gate_env` kept empty, so it gets none of the gate allow/waivers. Without
    /// `--foreground` the cap ends the check and every process it forked; the reason it returns
    /// (a timeout in particular) goes into the `warm-up: failed` line.
    fn warm_up(&mut self, dir: &Path, base: &str, rules: &Rules) {
        if repo::cargo_mode(&rules.merge.cargo) == Some(repo::CargoMode::None) {
            return;
        }
        if !dir.join("Cargo.toml").is_file() {
            return;
        }
        self.gate_env = Vec::new();
        let short = &base[..base.len().min(7)];
        self.say(&format!(
            "warm-up: cargo check of the merge base {short} (not a gate)"
        ));
        let t = Instant::now();
        let cap = self.warm_up_secs;
        let why = self.sh_run(
            dir,
            "cargo check --workspace --quiet",
            &format!("{cap}s"),
            &format!("timed out after {cap}s"),
        );
        match why {
            None => self.say(&format!("warm-up: done in {}s", t.elapsed().as_secs())),
            Some(reason) => self.say(&format!(
                "warm-up: failed after {}s (not a gate failure): {reason}",
                t.elapsed().as_secs()
            )),
        }
    }

    pub fn merge(&mut self) -> Outcome {
        let (Some(branch), Some(head)) = (self.spec.branch.clone(), self.spec.head.clone()) else {
            return fail("INVALID", "a merge job needs a branch and a commit");
        };
        let title = self.spec.title.clone().unwrap_or_else(|| branch.clone());
        for attempt in 1..=3 {
            self.say(&format!(
                "merge {branch} at {} into {} (attempt {attempt})",
                &head[..head.len().min(10)],
                self.spec.trunk
            ));
            let base = match self.trunk_tip() {
                Ok(b) => b,
                Err(e) => return fail("FETCH_FAILED", e),
            };
            // The trunk's rules first: a bad `.lucina/pipeline.toml` stops the job before any gate runs.
            let rules = match self.rules(&base) {
                Ok(r) => r,
                Err(e) => return fail("RULES_INVALID", e),
            };
            let repo = self.repo();
            if self
                .git(&repo, &["merge-base", "--is-ancestor", &head, &base])
                .is_ok()
            {
                return Outcome {
                    ok: true,
                    code: "NOTHING_TO_MERGE".into(),
                    message: format!("{branch} is already on {}", self.spec.trunk),
                    landed: Some(base),
                    method: Some("already".into()),
                    ..Outcome::default()
                };
            }
            let dir = match self.fresh_worktree(".landing", &base) {
                Ok(d) => d,
                Err(e) => return fail("LANDING_FAILED", e),
            };
            // Warm the shared target at the base, outside every timed gate step, so the first
            // timed check does not pay a cold build (finding GATE-04). Its outcome changes nothing.
            self.warm_up(&dir, &base, &rules);
            // Always a merge commit (the owner, 2026-11-07: "merge for everyone"; merge, don't
            // rebase): the branch's commits land as they were made, the merge names the branch.
            let method = "merge";
            {
                let msg = format!("Merge {branch}: {title}");
                let _ = self.git(&dir, &["checkout", "-q", "-f", "--detach", &base]);
                if self
                    .git_logged(&dir, &["merge", "-q", "--no-ff", "-m", &msg, &head])
                    .is_err()
                {
                    let conflicts: Vec<String> = self
                        .git(&dir, &["diff", "--name-only", "--diff-filter=U"])
                        .unwrap_or_default()
                        .lines()
                        .map(str::to_string)
                        .collect();
                    let _ = self.git(&dir, &["merge", "--abort"]);
                    return Outcome {
                        ok: false,
                        code: "CONFLICT".into(),
                        message: format!("{branch} conflicts with {}: merge {} into the branch in your workspace, commit, and run lagent merge again", self.spec.trunk, self.spec.remote.as_ref().map(|r| format!("{r}/{}", self.spec.trunk)).unwrap_or(self.spec.trunk.clone())),
                        conflicts,
                        ..Outcome::default()
                    };
                }
            }
            let candidate = match self.git(&dir, &["rev-parse", "HEAD"]) {
                Ok(c) => c,
                Err(e) => return fail("FAILED", e),
            };
            self.members = self.members(&dir);
            if candidate == base {
                return Outcome {
                    ok: true,
                    code: "NOTHING_TO_MERGE".into(),
                    message: format!("every commit of {branch} is already on {}", self.spec.trunk),
                    landed: Some(base),
                    method: Some("already".into()),
                    ..Outcome::default()
                };
            }
            let changed: Vec<String> = self
                .git(&dir, &["diff", "--name-only", &base, &candidate])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            self.say(&format!("{method}: {} file(s) changed", changed.len()));
            // Who wrote the commits: how many of the branch's have a commit-ledger entry. Only said;
            // the gates (LAGENT_COMMIT_LEDGER) decide what to do with it.
            {
                let name = repo
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ledger = self.ledger_path();
                let line = match crate::ledger::count_attested(&ledger, &repo, &name, &base, &head) {
                    Some((total, have)) => format!(
                        "commit ledger: {have} of {total} non-merge commit(s) of the branch have a ledger entry ({})",
                        ledger.display()
                    ),
                    None => format!("commit ledger: could not list the branch's commits ({})", ledger.display()),
                };
                self.say(&line);
            }
            // A branch's gate scripts run from the candidate tree, so a branch that edits one is
            // judged by its own edit: that needs an explicit, logged allow. Renames count as a
            // delete plus an add here, so a moved gate script is seen at both paths.
            let all_changed: Vec<String> = self
                .git(
                    &dir,
                    &["diff", "--name-only", "--no-renames", &base, &candidate],
                )
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            let gate_changes = {
                let exists = |p: &str| {
                    dir.join(p).is_file()
                        || self
                            .git(&repo, &["cat-file", "-e", &format!("{base}:{p}")])
                            .is_ok()
                };
                repo::gate_files_changed(&rules, &all_changed, &exists)
            };
            let trunk_has_rules = self
                .git(
                    &repo,
                    &["cat-file", "-e", &format!("{base}:{}", repo::FILE)],
                )
                .is_ok();
            let adds_rules = !trunk_has_rules && dir.join(repo::FILE).is_file();
            let mut rules_from_merge = false;
            let mut rules = rules;
            if !gate_changes.is_empty() {
                match repo::gate_change_reason(&self.spec.allow) {
                    None => {
                        return Outcome {
                            ok: false,
                            code: "GATE_CHANGE_NEEDS_ALLOW".into(),
                            message: format!(
                                "{branch} changes the files its own gates run from: {}. Take a second look at the diff of those files, then rerun with --allow gate-change=\"<why>\"{}",
                                gate_changes.join(", "),
                                if adds_rules {
                                    format!(". The trunk has no {}: with the allow, the rules of the file this merge adds apply to this merge", repo::FILE)
                                } else {
                                    String::new()
                                }
                            ),
                            method: Some(method.into()),
                            gate_changes,
                            ..Outcome::default()
                        };
                    }
                    Some(reason) => {
                        if repo::rules_source(trunk_has_rules, adds_rules, &self.spec.allow)
                            == repo::RulesSource::Candidate
                        {
                            let text =
                                std::fs::read_to_string(dir.join(repo::FILE)).unwrap_or_default();
                            rules = match Rules::parse(&text) {
                                Ok(r) => r,
                                Err(e) => return fail("RULES_INVALID", e),
                            };
                            rules_from_merge = true;
                            self.say(&format!(
                                "RULES TAKEN FROM THIS MERGE: the trunk has no {} (allowed: {reason})",
                                repo::FILE
                            ));
                            if !rules.merge.diffgate {
                                self.say("diffgate is OFF in the rules this merge adds");
                            }
                        }
                        for f in &gate_changes {
                            self.say(&format!(
                                "GATE CHANGED by this merge: {f} (allowed: {reason})"
                            ));
                        }
                    }
                }
            }
            let (crates, has_workspace) = self.crates(&dir);
            let range = format!("{base}..{candidate}");
            let (gates, notes) = repo::plan_with(
                &rules,
                &changed,
                &crates,
                has_workspace,
                &range,
                &self.spec.allow,
                self.spec.full,
            );
            for n in &notes {
                self.say(n);
            }
            let spec_change = repo::spec_change_reason(&self.spec.allow);
            if let Some(r) = &spec_change {
                self.say(&format!("SPEC CHANGE allowed for this merge: {r}"));
            }
            let (results, failed) = self.run_gates(&dir, &gates);
            if let Some(g) = failed {
                return Outcome {
                    ok: false,
                    code: "GATE_FAILED".into(),
                    message: format!("gate failed: {g} (lagent job logs {})", self.spec.job_id),
                    method: Some(method.into()),
                    gates: results,
                    gate_changes,
                    rules_from_merge,
                    spec_change: spec_change.clone(),
                    ..Outcome::default()
                };
            }
            match self.advance(&dir, &base, &candidate) {
                Ok(()) => {
                    return Outcome {
                        ok: true,
                        code: "MERGED".into(),
                        message: format!(
                            "{branch} is on {} at {} ({method}, {} gate(s) passed{})",
                            self.spec.trunk,
                            &candidate[..10],
                            results.len(),
                            match results
                                .iter()
                                .filter(|r| r.flaky)
                                .map(|r| r.name.as_str())
                                .collect::<Vec<_>>()
                            {
                                f if f.is_empty() => String::new(),
                                f => format!("; flaky, passed on retry: {}", f.join(", ")),
                            }
                        ),
                        landed: Some(candidate),
                        method: Some(method.into()),
                        gates: results,
                        gate_changes,
                        rules_from_merge,
                        spec_change: spec_change.clone(),
                        ..Outcome::default()
                    }
                }
                Err(Stop::Moved) => {
                    self.say(&format!(
                        "{} moved while the gates ran; starting over",
                        self.spec.trunk
                    ));
                    continue;
                }
                Err(Stop::Blocked(why, conflicts)) => {
                    return Outcome {
                        ok: false,
                        code: "TRUNK_CHECKOUT_BLOCKED".into(),
                        message: why,
                        method: Some(method.into()),
                        gates: results,
                        gate_changes,
                        rules_from_merge,
                        spec_change: spec_change.clone(),
                        conflicts,
                        ..Outcome::default()
                    };
                }
            }
        }
        fail(
            "PUSH_FAILED",
            format!("{} kept moving; three attempts failed", self.spec.trunk),
        )
    }

    /// Fast-forward the trunk from `base` to `candidate`.
    fn advance(&mut self, dir: &Path, base: &str, candidate: &str) -> Result<(), Stop> {
        let trunk = self.spec.trunk.clone();
        match self.spec.remote.clone() {
            Some(remote) => match self.git_logged(
                dir,
                &[
                    "push",
                    "-q",
                    &remote,
                    &format!("{candidate}:refs/heads/{trunk}"),
                ],
            ) {
                Ok(_) => Ok(()),
                Err(_) => Err(Stop::Moved),
            },
            None => {
                // A local trunk: where it is checked out, a fast-forward merge there (git keeps
                // uncommitted changes that don't touch the merged files, and refuses otherwise);
                // else the ref, if it is still at `base`.
                let repo = self.repo();
                let list = self
                    .git(&repo, &["worktree", "list", "--porcelain"])
                    .unwrap_or_default();
                let mut path = None;
                let mut current = None;
                for line in list.lines() {
                    if let Some(p) = line.strip_prefix("worktree ") {
                        current = Some(p.to_string());
                    } else if line == format!("branch refs/heads/{trunk}") {
                        path = current.clone();
                    }
                }
                match path {
                    Some(p) => {
                        let p = PathBuf::from(p);
                        if self.git(&p, &["rev-parse", "HEAD"]).ok().as_deref() != Some(base) {
                            return Err(Stop::Moved);
                        }
                        self.git_logged(&p, &["merge", "-q", "--ff-only", candidate]).map(|_| ()).map_err(|e| {
                            let files = blocking_files(&e);
                            Stop::Blocked(
                                format!(
                                    "{trunk} is checked out in {} and {} there would be overwritten by this merge; whoever wrote them should commit them or move them to a workspace (a main checkout is not for working in), then lagent merge again",
                                    p.display(),
                                    if files.is_empty() { "uncommitted files".to_string() } else { format!("{} uncommitted file(s)", files.len()) },
                                ),
                                files,
                            )
                        })
                    }
                    None => self
                        .git_logged(
                            &repo,
                            &[
                                "update-ref",
                                &format!("refs/heads/{trunk}"),
                                candidate,
                                base,
                            ],
                        )
                        .map(|_| ())
                        .map_err(|_| Stop::Moved),
                }
            }
        }
    }

    pub fn deploy(&mut self) -> Outcome {
        let Some(runtime) = self.spec.runtime.clone() else {
            return fail("INVALID", "a deploy job needs a runtime");
        };
        let tip = match self.trunk_tip() {
            Ok(t) => t,
            Err(e) => return fail("FETCH_FAILED", e),
        };
        let rules = match self.rules(&tip) {
            Ok(r) => r,
            Err(e) => return fail("RULES_INVALID", e),
        };
        let Some(recipe) = rules.deploy.get(&runtime).cloned() else {
            let known: Vec<&str> = rules.deploy.keys().map(String::as_str).collect();
            return fail("NO_RECIPE", format!("{} on {} has no [deploy.{runtime}] (declared: {}); deploy it by hand with deploy-guard, or add the recipe", repo::FILE, self.spec.trunk, if known.is_empty() { "none".into() } else { known.join(", ") }));
        };
        // This recipe's declared names (from the executor's own environment) are the extra
        // its commands get; the denylist does not take them back — that is the point of
        // declaring them. Only this deploy job passes them, never a gate or another recipe.
        self.deploy_env = recipe.env.clone();
        let dir = match self.fresh_worktree(".deploy", &tip) {
            Ok(d) => d,
            Err(e) => return fail("DEPLOY_TREE_FAILED", e),
        };
        self.members = self.members(&dir);
        self.say(&format!(
            "deploy {runtime} from {} at {}",
            self.spec.trunk,
            &tip[..10]
        ));
        let built = {
            let extra: Vec<&str> = self.deploy_env.iter().map(String::as_str).collect();
            crate::env::scrubbed_env(&extra)
        };
        let plan = Command::new("deploy-guard")
            .args(["plan", "--runtime", &runtime, "--repo"])
            .arg(&dir)
            .env_clear()
            .envs(built)
            .env("PATH", &self.path)
            .output();
        let plan = plan
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        self.say(&format!("plan: {plan}"));
        if plan.contains("up to date") && !self.spec.force {
            return Outcome {
                ok: true,
                code: "NOTHING_TO_DEPLOY".into(),
                message: format!(
                    "{runtime} already runs {}'s changes for it ({plan}); --force deploys anyway",
                    self.spec.trunk
                ),
                landed: Some(tip),
                ..Outcome::default()
            };
        }
        let t = recipe.timeout_min.unwrap_or(45);
        if let Some(build) = &recipe.build {
            if !self.sh(&dir, build, t) {
                return Outcome {
                    ok: false,
                    code: "BUILD_FAILED".into(),
                    message: format!("the build failed (lagent job logs {})", self.spec.job_id),
                    landed: Some(tip),
                    ..Outcome::default()
                };
            }
        }
        let rq = repo::shell_quote(&runtime);
        let dq = repo::shell_quote(&dir.display().to_string());
        if !self.sh(
            &dir,
            &format!("deploy-guard check --runtime {rq} --repo {dq}"),
            5,
        ) {
            return Outcome {
                ok: false,
                code: "REFUSED".into(),
                message: "deploy-guard refused the deploy (see the log)".into(),
                landed: Some(tip),
                ..Outcome::default()
            };
        }
        if !self.sh(&dir, &recipe.install, t) {
            return Outcome {
                ok: false,
                code: "INSTALL_FAILED".into(),
                message: format!(
                    "the install failed; check {runtime} now (lagent job logs {})",
                    self.spec.job_id
                ),
                landed: Some(tip),
                ..Outcome::default()
            };
        }
        if let Some(health) = &recipe.health {
            if !self.sh(&dir, health, 5) {
                return Outcome {
                    ok: false,
                    code: "UNHEALTHY".into(),
                    message: format!(
                        "{runtime} is installed but its health check failed; not recorded"
                    ),
                    landed: Some(tip),
                    ..Outcome::default()
                };
            }
        }
        let note = format!(
            "{} (lagent deploy {})",
            self.spec.note.clone().unwrap_or_else(|| format!(
                "{} at {}",
                self.spec.trunk,
                &tip[..8]
            )),
            self.spec.job_id
        );
        if !self.sh(
            &dir,
            &format!(
                "deploy-guard record --runtime {rq} --repo {dq} --note {}",
                repo::shell_quote(&note)
            ),
            5,
        ) {
            return Outcome {
                ok: false,
                code: "RECORD_FAILED".into(),
                message: "installed, but deploy-guard record failed: record it by hand".into(),
                landed: Some(tip),
                ..Outcome::default()
            };
        }
        Outcome {
            ok: true,
            code: "DEPLOYED".into(),
            message: format!("{runtime} runs {} at {}", self.spec.trunk, &tip[..10]),
            landed: Some(tip),
            ..Outcome::default()
        }
    }
}

/// Why the trunk didn't move: it moved meanwhile (start over), or the checkout it is in
/// can't take the fast-forward.
enum Stop {
    Moved,
    /// Why, and the files in the way.
    Blocked(String, Vec<String>),
}

/// The files git names when it refuses a merge (indented lines of its error).
fn blocking_files(err: &str) -> Vec<String> {
    err.lines()
        .filter(|l| l.starts_with([' ', '\t']))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{JobKind, Spec};
    use super::*;
    use crate::testutil::write_exec;
    use std::ffi::OsString;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A bare origin, a clone with main, and a topic branch.
    fn setup(root: &Path) -> (PathBuf, PathBuf) {
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&origin)
            .status()
            .unwrap();
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&origin)
            .arg(&repo)
            .status()
            .unwrap();
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["checkout", "-q", "-b", "main"]);
        commit(&repo, "a.txt", "1\n", "a");
        git(&repo, &["push", "-q", "origin", "main"]);
        (origin, repo)
    }

    fn spec(root: &Path, repo: &Path, branch: &str, head: &str) -> Spec {
        Spec {
            job_id: "job_t".into(),
            kind: JobKind::Merge,
            repo: repo.display().to_string(),
            trunk: "main".into(),
            remote: Some("origin".into()),
            work_root: root.join("wt").display().to_string(),
            target_dir: root.join("target").display().to_string(),
            target_root: Some(root.to_path_buf()),
            branch: Some(branch.into()),
            head: Some(head.into()),
            title: Some("t".into()),
            runtime: None,
            note: None,
            allow: vec![],
            force: false,
            full: false,
            ledger: None,
            requested_by: None,
        }
    }

    fn exec(root: &Path, spec: Spec) -> Outcome {
        let dir = root.join(format!("job-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        // No cargo or diffgate gates in a repo without crates and with diffgate off.
        let mut x = Exec::open(&dir, spec).unwrap();
        x.merge()
    }

    #[test]
    fn merges_with_a_merge_commit_then_reports_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\n[[gate]]\nname = \"has b\"\npaths = [\"b.txt\"]\nrun = \"test -f b.txt\"\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        // Topic branch, then main moves on (another file): a merge commit, never a rebase.
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        git(&repo, &["checkout", "-q", "main"]);
        let main_tip = commit(&repo, "c.txt", "c\n", "add c");
        git(&repo, &["push", "-q", "origin", "main"]);
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert!(out.ok, "{out:?}");
        assert_eq!(
            (out.code.as_str(), out.method.as_deref()),
            ("MERGED", Some("merge"))
        );
        assert_eq!(out.gates.len(), 1);
        let landed = out.landed.unwrap();
        assert_eq!(git(&origin, &["rev-parse", "main"]), landed);
        // The branch's own commit landed unchanged as the merge's second parent, main's tip as its first.
        assert_eq!(
            git(&origin, &["rev-parse", &format!("{landed}^1")]),
            main_tip
        );
        assert_eq!(git(&origin, &["rev-parse", &format!("{landed}^2")]), head);
        assert!(git(&origin, &["log", "--format=%s", "-1", "main"]).starts_with("Merge topic"));
        assert_eq!(
            git(
                &origin,
                &["log", "--first-parent", "--format=%s", "-3", "main"]
            )
            .lines()
            .skip(1)
            .collect::<Vec<_>>(),
            vec!["add c", "rules"]
        );
        // Merged again: nothing to do.
        let again = exec(root, spec(root, &repo, "topic", &head));
        assert_eq!(again.code, "NOTHING_TO_MERGE");

        // A conflict on a.txt: CONFLICT with the file named, main unchanged.
        git(&repo, &["fetch", "-q", "origin"]);
        git(&repo, &["checkout", "-q", "-b", "clash", "origin/main"]);
        let head = commit(&repo, "a.txt", "topic\n", "clash");
        git(&repo, &["checkout", "-q", "-B", "main", "origin/main"]);
        commit(&repo, "a.txt", "main\n", "main edit");
        git(&repo, &["push", "-q", "origin", "main"]);
        let before = git(&origin, &["rev-parse", "main"]);
        let out = exec(root, spec(root, &repo, "clash", &head));
        assert_eq!(out.code, "CONFLICT");
        assert_eq!(out.conflicts, vec!["a.txt".to_string()]);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    #[test]
    fn a_cargo_test_gate_that_passes_on_retry_is_flaky() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        // `cargo test` is shadowed by a script in `bin/` that fails the first time and passes
        // after. It needs no tool of its own: the state file is written by the shell itself, so
        // the shadow does its work whatever PATH the gate command ends up with.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ran = root.join("cargo-ran");
        // Mode 0o755 is part of this: a shadow the shell cannot exec is not a shadow that fails,
        // it is a shadow that is *skipped* — the PATH search moves on and the machine's own cargo
        // answers both gate runs in no time, which is the outcome that looked unexplainable.
        // Un-executable covers a missing execute bit (`chmod +x` lets the umask eat it) and an
        // inode another process still holds open for writing (ETXTBSY); the helper sets the mode
        // absolutely and returns only when the script can actually be exec'd, whatever the other
        // test threads are forking.
        write_exec(
            &bin.join("cargo"),
            &format!(
                "#!/bin/sh\nif [ -s {r} ]; then exit 0; fi\necho once >>{r}\nexit 1\n",
                r = ran.display()
            ),
        );
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"t\"\nrun = \"cargo test --quiet\"\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let dir = root.join("job-flaky");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        let out = x.merge();
        // The job log travels with the messages below: when this fails on a machine nobody is
        // looking, the gate's own line ("cannot run", "failed: exit 126", "timed out") is what
        // says which of the two runs broke and how.
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert!(out.gates[0].flaky, "{out:?}\nLOG:\n{log}");
        // Both gate runs went through the shadow, not through a cargo of the machine: it failed
        // once (one line in its state file) and passed on the retry.
        assert_eq!(
            std::fs::read_to_string(&ran).ok().as_deref(),
            Some("once\n"),
            "{out:?}\nLOG:\n{log}"
        );
        assert!(
            out.message.contains("flaky, passed on retry: t"),
            "{}\nLOG:\n{log}",
            out.message
        );
        assert_eq!(git(&origin, &["rev-parse", "main"]).len(), 40);
    }

    #[test]
    fn an_exported_cargo_function_does_not_beat_the_gate_s_path() {
        // The 2026-11-17 failure: the caller had exported a `cargo` shell function, and the
        // gate's bash resolved it before PATH, so the machine's cargo answered both gate runs
        // ("could not find Cargo.toml") and the flaky test failed everywhere. What closes it is
        // `crate::env`'s allowlist: a BASH_FUNC_* name is neither an exact entry nor an LC_ or
        // BQ_ prefix, so it never reaches the built child environment. This is the test that
        // would have caught the failure and is the proof that it still cannot come back —
        // against a gate command that would say so: a gate that sees the function runs
        // `echo HIJACKED` instead of the shadow, which shows up as a non-flaky pass and an
        // empty state file. The function is injected as the source environment's stand-in
        // (`source_env`, never as a process-wide variable).
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ran = root.join("cargo-ran-fn");
        write_exec(
            &bin.join("cargo"),
            &format!(
                "#!/bin/sh\nif [ -s {r} ]; then exit 0; fi\necho once >>{r}\nexit 1\n",
                r = ran.display()
            ),
        );
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"t\"\nrun = \"cargo test --quiet\"\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let dir = root.join("job-fn");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        x.source_env =
            crate::testutil::env_source(&[("BASH_FUNC_cargo%%", "() { echo HIJACKED; }")]);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert!(out.gates[0].flaky, "{out:?}\nLOG:\n{log}");
        assert_eq!(
            std::fs::read_to_string(&ran).ok().as_deref(),
            Some("once\n"),
            "{out:?}\nLOG:\n{log}"
        );
        assert!(
            out.message.contains("flaky, passed on retry: t"),
            "{}\nLOG:\n{log}",
            out.message
        );
        assert_eq!(git(&origin, &["rev-parse", "main"]).len(), 40);
    }

    /// The shape of every stand-in in this crate — write an executable, then exec it — in a loop
    /// and under load, with three threads in this same process forking children non-stop.
    /// `write_exec`'s immunity is what the counts in its module docs measure; this is the guard,
    /// so that a stand-in or the helper going back to a bare `fs::write` shows up here.
    #[test]
    fn a_script_written_by_the_helper_runs_under_concurrent_forks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let hammers: Vec<_> = (0..3)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = Command::new("/bin/true").status();
                    }
                })
            })
            .collect();
        let script = dir.path().join("stand-in");
        for round in 0..120 {
            write_exec(&script, "#!/bin/sh\necho ran\n");
            let out = Command::new(&script)
                .output()
                .unwrap_or_else(|e| panic!("round {round}: the script would not start: {e}"));
            assert!(
                out.status.success(),
                "round {round}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "ran\n",
                "round {round}"
            );
        }
        stop.store(true, Ordering::Relaxed);
        for h in hammers {
            let _ = h.join();
        }
    }

    #[test]
    fn sweep_runs_after_jobs_and_failure_preserves_outcome_and_exit_code() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_, repo) = setup(root);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        for (kind, valid, code, exit) in [
            (JobKind::Merge, true, "NOTHING_TO_MERGE", 0),
            (JobKind::Merge, false, "INVALID", 1),
            (JobKind::Deploy, false, "INVALID", 1),
        ] {
            for sweep_fails in [false, true] {
                let dir = root.join(format!("job-{kind:?}-{valid}-{sweep_fails}"));
                std::fs::create_dir(&dir).unwrap();
                let mut spec = spec(root, &repo, "main", &head);
                spec.kind = kind;
                if !valid {
                    spec.branch = None;
                }
                let artifact = Path::new(&spec.target_dir).join("debug/incremental/old");
                std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
                std::fs::write(&artifact, "old output").unwrap();
                let old =
                    std::time::SystemTime::now() - std::time::Duration::from_secs(4 * 60 * 60);
                File::open(&artifact)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
                if sweep_fails {
                    // An existing but wrong boundary fails deterministically, even as root.
                    spec.target_root = Some(repo.clone());
                }
                std::fs::write(dir.join("job.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
                assert_eq!(run(&dir), exit);
                let outcome: Outcome =
                    serde_json::from_slice(&std::fs::read(dir.join("result.json")).unwrap())
                        .unwrap();
                assert_eq!(outcome.code, code);
                assert_eq!(outcome.ok, exit == 0);
                let log = std::fs::read_to_string(dir.join("log")).unwrap();
                assert_eq!(log.matches("target sweep:").count(), 1, "{log}");
                assert_eq!(
                    log.contains("refusing target outside root"),
                    sweep_fails,
                    "{log}"
                );
                assert_eq!(artifact.exists(), sweep_fails);
            }
        }
    }

    #[test]
    fn member_names_are_normalized_and_exclude_external_path_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let dependency = root.path().join("dependency");
        for dir in [&project, &dependency] {
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/lib.rs"), "").unwrap();
        }
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"stable-dependency\"\nversion = \"0.1.0\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"workspace-crate\"\nversion = \"0.1.0\"\n[workspace]\n[dependencies]\nstable-dependency = { path = \"../dependency\" }\n").unwrap();
        let x = Exec::open(root.path(), spec(root.path(), &project, "b", "abc")).unwrap();
        let members = x.members(&project).unwrap();
        assert_eq!(members.names, HashSet::from(["workspace_crate".into()]));
        assert_eq!(members.directories, vec![""]);
        assert!(x.members(&dependency.join("missing")).is_none());
    }

    #[test]
    fn gates_find_sbin_tools() {
        let root = tempfile::tempdir().unwrap();
        let x = Exec::open(root.path(), spec(root.path(), root.path(), "b", "abc")).unwrap();
        assert!(x.path.split(':').any(|d| d == "/usr/sbin"), "{}", x.path);
        assert!(x.path.split(':').any(|d| d == "/sbin"), "{}", x.path);
    }

    /// A repo whose rules have one passing gate that runs `scripts/gate.sh`; returns (origin, repo).
    fn gate_repo(root: &Path) -> (PathBuf, PathBuf) {
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::write(repo.join("scripts/gate.sh"), "exit 0\n").unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n[[gate]]\nname = \"g\"\nrun = \"bash scripts/gate.sh\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        (origin, repo)
    }

    #[test]
    fn a_merge_that_edits_a_gate_script_needs_an_allow() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = gate_repo(root);
        let before = git(&origin, &["rev-parse", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "scripts/gate.sh", "exit 0 # edited\n", "edit gate");
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert_eq!(out.code, "GATE_CHANGE_NEEDS_ALLOW");
        assert!(!out.ok);
        assert_eq!(out.gate_changes, vec!["scripts/gate.sh".to_string()]);
        assert!(out.gates.is_empty(), "no gate ran");
        assert!(
            out.message.contains("scripts/gate.sh") && out.message.contains("--allow gate-change")
        );
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
        // An empty reason is still a refusal.
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["gate-change=".into()];
        assert_eq!(exec(root, s).code, "GATE_CHANGE_NEEDS_ALLOW");
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    #[test]
    fn an_allowed_gate_change_merges_and_is_logged() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = gate_repo(root);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "scripts/gate.sh", "exit 0 # edited\n", "edit gate");
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["gate-change=reviewed the diff".into()];
        let dir = root.join("job-allowed");
        std::fs::create_dir_all(&dir).unwrap();
        let out = Exec::open(&dir, s).unwrap().merge();
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert_eq!(out.gate_changes, vec!["scripts/gate.sh".to_string()]);
        assert_eq!(out.gates.len(), 1);
        assert_ne!(git(&origin, &["rev-parse", "main"]), head);
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(
            log.contains(
                "GATE CHANGED by this merge: scripts/gate.sh (allowed: reviewed the diff)"
            ),
            "{log}"
        );
    }

    /// A topic branch off a trunk with no pipeline file: a crate that does not compile (the default
    /// `cargo = "touched"` gate would fail) and a `.lucina/pipeline.toml` with `rules`.
    fn bootstrap_branch(root: &Path, rules: &str) -> (PathBuf, PathBuf, String) {
        let (origin, repo) = setup(root);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"bt\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.rs"), "this does not compile\n").unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), rules).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "bootstrap"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        (origin, repo, head)
    }

    #[test]
    fn bootstrap_is_refused_without_the_allow() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo, head) =
            bootstrap_branch(root, "[merge]\ncargo = \"none\"\ndiffgate = false\n");
        let before = git(&origin, &["rev-parse", "main"]);
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert_eq!(out.code, "GATE_CHANGE_NEEDS_ALLOW");
        assert!(
            out.message
                .contains("the rules of the file this merge adds apply"),
            "{}",
            out.message
        );
        assert!(out
            .gate_changes
            .contains(&".lucina/pipeline.toml".to_string()));
        assert!(!out.rules_from_merge && out.gates.is_empty());
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    #[test]
    fn bootstrap_with_the_allow_uses_the_added_rules() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo, head) =
            bootstrap_branch(root, "[merge]\ncargo = \"none\"\ndiffgate = false\n");
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["gate-change=first pipeline file".into()];
        let dir = root.join("job-bootstrap");
        std::fs::create_dir_all(&dir).unwrap();
        let out = Exec::open(&dir, s).unwrap().merge();
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert!(out.rules_from_merge);
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(log.contains("RULES TAKEN FROM THIS MERGE: the trunk has no .lucina/pipeline.toml (allowed: first pipeline file)"), "{log}");
        assert!(
            log.contains("diffgate is OFF in the rules this merge adds"),
            "{log}"
        );
        assert!(
            log.contains("GATE CHANGED by this merge: .lucina/pipeline.toml"),
            "{log}"
        );
    }

    #[test]
    fn bootstrap_with_the_trunk_default_would_fail_and_a_bad_added_file_is_invalid() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo, head) = bootstrap_branch(root, "[merge]\ncargo = \"sometimes\"\n");
        let before = git(&origin, &["rev-parse", "main"]);
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["gate-change=first pipeline file".into()];
        let out = exec(root, s);
        assert_eq!(out.code, "RULES_INVALID", "{}", out.message);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
        // Without the allow the same compile error is judged by the trunk default: nothing lands either.
        let root2 = tempfile::tempdir().unwrap();
        let (origin2, repo2, head2) = bootstrap_branch(root2.path(), "[merge]\ncargo = \"none\"\n");
        let before2 = git(&origin2, &["rev-parse", "main"]);
        assert_ne!(
            exec(root2.path(), spec(root2.path(), &repo2, "topic", &head2)).code,
            "MERGED"
        );
        assert_eq!(git(&origin2, &["rev-parse", "main"]), before2);
    }

    #[test]
    fn a_trunk_that_has_rules_keeps_them_for_the_merge_that_changes_them() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = gate_repo(root);
        // The trunk's gate always fails; the branch's file would drop it.
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n[[gate]]\nname = \"never\"\nrun = \"exit 3\"\n",
        )
        .unwrap();
        git(&repo, &["commit", "-qam", "failing gate"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        let before = git(&origin, &["rev-parse", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(
            &repo,
            ".lucina/pipeline.toml",
            "[merge]\ndiffgate = false\ncargo = \"none\"\n",
            "drop gate",
        );
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["gate-change=drop the gate".into()];
        let out = exec(root, s);
        assert_eq!(out.code, "GATE_FAILED", "{}", out.message);
        assert!(!out.rules_from_merge);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// A repo whose one gate records what it sees of `LAGENT_WAIVERS_FILE` in `<root>/seen.jsonl`
    /// (a copy of the file, or the word `unset`) and always passes.
    fn waiver_probe_repo(root: &Path) -> PathBuf {
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        let seen = root.join("seen.jsonl");
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            format!(
                "[merge]\ndiffgate = false\n[[gate]]\nname = \"probe\"\nrun = \"if [ -n \\\"${{LAGENT_WAIVERS_FILE+x}}\\\" ]; then cp \\\"$LAGENT_WAIVERS_FILE\\\" {0}; else echo unset > {0}; fi\"\n",
                seen.display()
            ),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        repo
    }

    /// Merge `file` on topic with `allow`; the job directory, the outcome and what the gate saw.
    fn merge_with_waivers(
        root: &Path,
        repo: &Path,
        n: usize,
        allow: &[&str],
    ) -> (PathBuf, Outcome, String) {
        let head = commit(repo, &format!("w{n}.txt"), "w\n", "add w");
        let mut s = spec(root, repo, "topic", &head);
        s.allow = allow.iter().map(|a| a.to_string()).collect();
        s.requested_by = Some("local_req".into());
        let dir = root.join(format!("job-waivers-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(root.join("seen.jsonl"));
        let out = Exec::open(&dir, s).unwrap().merge();
        let seen = std::fs::read_to_string(root.join("seen.jsonl")).unwrap_or_default();
        (dir, out, seen)
    }

    #[test]
    fn gate_commands_get_the_waivers_file_only_when_the_job_has_waivers() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let repo = waiver_probe_repo(root);

        // Two allows with reasons: the gate sees the file, with both lines.
        let (dir, out, seen) = merge_with_waivers(
            root,
            &repo,
            1,
            &[
                "skipped-test=ignored: needs a server",
                "spec-change=proof reworded",
            ],
        );
        assert_eq!(out.code, "MERGED", "{}", out.message);
        let file = dir.join("waivers.jsonl");
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(seen.as_bytes(), bytes.as_slice());
        let rows: Vec<serde_json::Value> = seen
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        let want = [
            ("skipped-test", "ignored: needs a server"),
            ("spec-change", "proof reworded"),
        ];
        for (row, (rule, reason)) in rows.iter().zip(want) {
            let o = row.as_object().unwrap();
            assert_eq!(o.len(), 4);
            assert_eq!(o["rule"], rule);
            assert_eq!(o["reason"], reason);
            assert_eq!(o["by"], "local_req");
            assert_eq!(o["job"], "job_t");
        }
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(!dir.join("waivers.jsonl.tmp").exists());
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(
            log.contains("waivers passed to gate commands: 2 ("),
            "{log}"
        );
        // Still the same bytes after the gates ran.
        assert_eq!(std::fs::read(&file).unwrap(), bytes);

        // No allow: the gate sees the variable unset and no file exists.
        let (dir, out, seen) = merge_with_waivers(root, &repo, 2, &[]);
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert_eq!(seen.trim(), "unset");
        assert!(!dir.join("waivers.jsonl").exists());
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(!log.contains("waivers passed"), "{log}");

        // Only a blank reason: the same.
        let (dir, out, seen) = merge_with_waivers(root, &repo, 3, &["skipped-test=  "]);
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert_eq!(seen.trim(), "unset");
        assert!(!dir.join("waivers.jsonl").exists());
    }

    #[test]
    fn a_deploy_job_never_has_the_waivers_file_or_variable() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let mut s = spec(root, root, "b", "abc");
        s.kind = JobKind::Deploy;
        s.allow = vec!["skipped-test=why".into()];
        s.requested_by = Some("local_req".into());
        let mut x = Exec::open(root, s).unwrap();
        let seen = root.join("seen");
        let cmd = format!(
            "if [ -n \"${{LAGENT_WAIVERS_FILE+x}}\" ]; then echo set; else echo unset; fi > {}",
            seen.display()
        );
        assert!(x.sh(root, &cmd, 1));
        assert_eq!(std::fs::read_to_string(&seen).unwrap().trim(), "unset");
        assert!(!root.join("waivers.jsonl").exists());
    }

    #[test]
    fn gate_commands_see_the_spec_change_allow() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n[[gate]]\nname = \"needs spec\"\nrun = \"test -n \\\"$LAGENT_ALLOW_SPEC_CHANGE\\\" && test -z \\\"$LAGENT_ALLOW_GATE_CHANGE\\\" && printf %s \\\"$LAGENT_ALLOW\\\" | grep -qx 'spec-change=proof reworded'\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        // Without the allow the gate fails; an empty reason is the same as none.
        assert_eq!(
            exec(root, spec(root, &repo, "topic", &head)).code,
            "GATE_FAILED"
        );
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["spec-change=".into()];
        assert_eq!(exec(root, s).code, "GATE_FAILED");
        // With it the gate sees the variables and the log and outcome say so.
        let mut s = spec(root, &repo, "topic", &head);
        s.allow = vec!["spec-change=proof reworded".into()];
        let dir = root.join("job-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let out = Exec::open(&dir, s).unwrap().merge();
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert_eq!(out.spec_change.as_deref(), Some("proof reworded"));
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(
            log.contains("SPEC CHANGE allowed for this merge: proof reworded"),
            "{log}"
        );
    }

    #[test]
    fn gate_commands_see_the_commit_ledger_and_the_log_counts_attested_commits() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = setup(root);
        let ledger = root.join("state/commit-ledger.jsonl");
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            format!(
                "[merge]\ndiffgate = false\n[[gate]]\nname = \"sees ledger\"\nrun = \"test \\\"$LAGENT_COMMIT_LEDGER\\\" = {}\"\n",
                ledger.display()
            ),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let c1 = commit(&repo, "b.txt", "b\n", "add b");
        let head = commit(&repo, "c.txt", "c\n", "add c");
        // Without a ledger path in the spec the gate sees the default one and fails.
        assert_eq!(
            exec(root, spec(root, &repo, "topic", &head)).code,
            "GATE_FAILED"
        );
        // One of the two commits is attested.
        crate::ledger::Ledger::new(&ledger)
            .append(vec![crate::ledger::Entry {
                v: 1,
                commit: c1,
                repo: "repo".into(),
                workspace: String::new(),
                model: "claude-sonnet-5-5".into(),
                kind: "lane".into(),
                lane: String::new(),
                session: "t".into(),
                declared_by: "flag".into(),
                time: String::new(),
                prev: String::new(),
            }])
            .unwrap();
        let mut s = spec(root, &repo, "topic", &head);
        s.ledger = Some(ledger.display().to_string());
        let dir = root.join("job-ledger");
        std::fs::create_dir_all(&dir).unwrap();
        let out = Exec::open(&dir, s).unwrap().merge();
        assert_eq!(out.code, "MERGED", "{}", out.message);
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(
            log.contains(
                "commit ledger: 1 of 2 non-merge commit(s) of the branch have a ledger entry"
            ),
            "{log}"
        );
    }

    #[test]
    fn a_merge_without_gate_files_is_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = gate_repo(root);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert_eq!(out.code, "MERGED", "{}", out.message);
        assert!(out.gate_changes.is_empty());
    }

    #[test]
    fn a_failing_gate_stops_the_merge() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n[[gate]]\nname = \"never\"\nrun = \"exit 3\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        let before = git(&origin, &["rev-parse", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert_eq!(out.code, "GATE_FAILED");
        assert!(!out.gates[0].ok);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    #[test]
    fn a_local_trunk_moves_where_it_is_checked_out() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let repo = root.join("repo");
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&repo)
            .status()
            .unwrap();
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n",
        )
        .unwrap();
        commit(&repo, "a.txt", "1\n", "a");
        git(&repo, &["branch", "topic"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                &root.join("topic").display().to_string(),
                "topic",
            ],
        );
        let head = commit(&root.join("topic"), "b.txt", "b\n", "add b");
        let mut s = spec(root, &repo, "topic", &head);
        s.remote = None;
        // Uncommitted work in the main checkout that the merge doesn't touch stays.
        std::fs::write(repo.join("notes.txt"), "mine\n").unwrap();
        let out = exec(root, s.clone());
        assert!(out.ok, "{out:?}");
        // A merge commit, the branch's commit as its second parent.
        let landed = git(&repo, &["rev-parse", "main"]);
        assert_eq!(out.landed.as_deref(), Some(landed.as_str()));
        assert_eq!(git(&repo, &["rev-parse", "main^2"]), head);
        assert!(
            repo.join("b.txt").exists(),
            "the main checkout fast-forwarded"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
            "mine\n"
        );
        // Uncommitted changes to a file the merge changes: blocked, said why, main unchanged.
        let head2 = commit(&root.join("topic"), "c.txt", "topic\n", "add c");
        std::fs::write(repo.join("c.txt"), "local edit\n").unwrap();
        let out = exec(
            root,
            Spec {
                head: Some(head2),
                ..s
            },
        );
        assert_eq!(out.code, "TRUNK_CHECKOUT_BLOCKED", "{out:?}");
        assert_eq!(out.conflicts, vec!["c.txt".to_string()]);
        assert_eq!(git(&repo, &["rev-parse", "main"]), landed);
    }

    /// `deploy-guard plan` is a direct `Command::new(name)`: the program is looked up in this
    /// process's PATH, not the job's built one. The stand-in must be findable there; prepended,
    /// held under a lock, and restored on drop — the one process-wide mutation in these tests,
    /// and nothing else runs under it (the prepended dir holds only `deploy-guard`).
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PathStandIn {
        _lock: std::sync::MutexGuard<'static, ()>,
        old: Option<OsString>,
    }

    fn path_stand_in(dir: &Path) -> PathStandIn {
        let lock = PATH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let old = std::env::var_os("PATH");
        let mut parts = vec![dir.display().to_string()];
        if let Some(o) = &old {
            parts.push(o.to_string_lossy().into_owned());
        }
        std::env::set_var("PATH", parts.join(":"));
        PathStandIn { _lock: lock, old }
    }

    impl Drop for PathStandIn {
        fn drop(&mut self) {
            match &self.old {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn a_gate_command_gets_the_allowlisted_environment() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\n[[gate]]\nname = \"env\"\nrun = \"env | cut -d= -f1 > names.txt && printf %s \\\"$PATH\\\" > path.txt && printf %s \\\"$TMPDIR\\\" > tmpdir.txt\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let dir = root.join("job-env");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.source_env = crate::testutil::env_source(&[
            ("PATH", "/gone"),
            ("HOME", "/home/fake"),
            ("TMPDIR", "/fake/tmp"),
            ("CARGO_TARGET_DIR", "/fake/target"),
            ("SSH_AUTH_SOCK", "/run/user/1000/ssh.sock"),
            ("LAGENT_WAIVERS_FILE", "/run/fake/waivers.jsonl"),
            ("FAKE_API_TOKEN", "fake-token"),
            ("CLOUDFLARE_FAKE_ID", "fake-id"),
        ]);
        let out = x.merge();
        assert!(out.ok, "{out:?}");
        let landing = root.join("wt/.landing");
        let names = std::fs::read_to_string(landing.join("names.txt")).unwrap();
        // LAGENT_WAIVERS_FILE belongs to a gate command alone, set there explicitly by the
        // merge job; whatever agentd's own environment holds for it is not a route in.
        for n in [
            "FAKE_API_TOKEN",
            "CLOUDFLARE_FAKE_ID",
            "SSH_AUTH_SOCK",
            "LAGENT_WAIVERS_FILE",
        ] {
            assert!(!names.lines().any(|l| l == n), "{names}");
        }
        for n in ["PATH", "HOME", "TMPDIR", "CARGO_TARGET_DIR"] {
            assert!(names.lines().any(|l| l == n), "{names}");
        }
        // The job's built PATH is the one the gate sees, not the source's.
        let gate_path = std::fs::read_to_string(landing.join("path.txt")).unwrap();
        assert!(
            gate_path == x.path,
            "the gate saw a PATH the job did not build"
        );
        // An allowlisted value travels with its name.
        assert_eq!(
            std::fs::read_to_string(landing.join("tmpdir.txt")).unwrap(),
            "/fake/tmp"
        );
        // The count only: no names, no values.
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.contains("env: 4 variable(s) not passed"), "{log}");
        assert!(
            !log.contains("FAKE_API_TOKEN")
                && !log.contains("CLOUDFLARE_FAKE_ID")
                && !log.contains("SSH_AUTH_SOCK")
                && !log.contains("LAGENT_WAIVERS_FILE"),
            "{log}"
        );
    }

    #[test]
    fn a_recipe_declared_variable_reaches_its_deploy_and_not_a_gate_or_another_recipe() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            concat!(
                "[merge]\ndiffgate = false\n",
                "[[gate]]\nname = \"env\"\nrun = \"env | cut -d= -f1 > gate-names.txt\"\n",
                "[deploy.alpha]\nbuild = \"env | cut -d= -f1 > build-names.txt\"\ninstall = \"exit 0\"\nenv = [\"FAKE_RECIPE_ALPHA\"]\n",
                "[deploy.beta]\nbuild = \"env | cut -d= -f1 > build-names.txt\"\ninstall = \"exit 0\"\nenv = [\"FAKE_RECIPE_BETA\"]\n",
            ),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        // deploy-guard: plan sees something new, check and record pass.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("deploy-guard"),
            "#!/bin/sh\ncase \"$1\" in\nplan) echo \"1 change(s) to deploy\";;\nesac\nexit 0\n",
        );
        let source = crate::testutil::env_source(&[
            ("PATH", "/gone"),
            ("HOME", "/home/fake"),
            ("FAKE_RECIPE_ALPHA", "fake-alpha"),
            ("FAKE_RECIPE_BETA", "fake-beta"),
        ]);

        // The gate never sees a recipe's names.
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let dir = root.join("job-merge");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.source_env = source.clone();
        let out = x.merge();
        assert!(out.ok, "{out:?}");
        let gate_names = std::fs::read_to_string(root.join("wt/.landing/gate-names.txt")).unwrap();
        for n in ["FAKE_RECIPE_ALPHA", "FAKE_RECIPE_BETA"] {
            assert!(!gate_names.lines().any(|l| l == n), "{gate_names}");
        }
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.contains("env: 2 variable(s) not passed"), "{log}");

        for (runtime, declared, other) in [
            ("alpha", "FAKE_RECIPE_ALPHA", "FAKE_RECIPE_BETA"),
            ("beta", "FAKE_RECIPE_BETA", "FAKE_RECIPE_ALPHA"),
        ] {
            let mut s = spec(root, &repo, "topic", &head);
            s.kind = JobKind::Deploy;
            s.runtime = Some(runtime.into());
            let dir = root.join(format!("job-deploy-{runtime}"));
            std::fs::create_dir_all(&dir).unwrap();
            let mut x = Exec::open(&dir, s).unwrap();
            x.source_env = source.clone();
            x.path = format!("{}:{}", bin.display(), x.path);
            let _on_path = path_stand_in(&bin);
            let out = x.deploy();
            assert_eq!(out.code, "DEPLOYED", "{}: {}", runtime, out.message);
            let build_names =
                std::fs::read_to_string(root.join("wt/.deploy/build-names.txt")).unwrap();
            assert!(
                build_names.lines().any(|l| l == declared),
                "{runtime}: {build_names}"
            );
            assert!(
                !build_names.lines().any(|l| l == other),
                "{runtime}: {build_names}"
            );
            // The other recipe's name did not pass — said by count only.
            let log = std::fs::read_to_string(dir.join("log")).unwrap();
            assert!(log.contains("env: 1 variable(s) not passed"), "{log}");
            assert!(!log.contains(other), "{log}");
        }
    }

    /// The SSH agent socket: not allowlisted, so no gate command, agent, terminal or setup
    /// script is built with it. A recipe that needs it for its own deploy steps asks for it by
    /// name, exactly like any other declared variable.
    #[test]
    fn ssh_auth_sock_reaches_the_deploy_steps_of_the_recipe_that_declares_it() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            concat!(
                "[merge]\ndiffgate = false\n",
                "[deploy.asked]\nbuild = \"env | cut -d= -f1 > build-names.txt\"\ninstall = \"env | cut -d= -f1 > install-names.txt\"\nenv = [\"SSH_AUTH_SOCK\"]\n",
                "[deploy.notasked]\nbuild = \"env | cut -d= -f1 > build-names.txt\"\ninstall = \"env | cut -d= -f1 > install-names.txt\"\n",
            ),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        // deploy-guard: plan sees something new, check and record pass.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("deploy-guard"),
            "#!/bin/sh\ncase \"$1\" in\nplan) echo \"1 change(s) to deploy\";;\nesac\nexit 0\n",
        );
        let _on_path = path_stand_in(&bin);
        let tip = git(&repo, &["rev-parse", "main"]);
        for (runtime, declared) in [("asked", true), ("notasked", false)] {
            let dir = root.join(format!("job-{runtime}"));
            std::fs::create_dir_all(&dir).unwrap();
            let mut s = spec(root, &repo, "main", &tip);
            s.kind = JobKind::Deploy;
            s.runtime = Some(runtime.into());
            let mut x = Exec::open(&dir, s).unwrap();
            x.source_env = crate::testutil::env_source(&[
                ("PATH", "/gone"),
                ("HOME", "/home/fake"),
                ("SSH_AUTH_SOCK", "/run/user/1000/ssh.sock"),
            ]);
            x.path = format!("{}:{}", bin.display(), x.path);
            let out = x.deploy();
            assert_eq!(out.code, "DEPLOYED", "{runtime}: {}", out.message);
            for step in ["build", "install"] {
                let names =
                    std::fs::read_to_string(root.join(format!("wt/.deploy/{step}-names.txt")))
                        .unwrap();
                assert_eq!(
                    names.lines().any(|l| l == "SSH_AUTH_SOCK"),
                    declared,
                    "{runtime} {step}: {names}"
                );
            }
            // The recipe that did not ask kept it back, and the log says so by count only.
            let log = std::fs::read_to_string(dir.join("log")).unwrap();
            assert_eq!(log.contains("variable(s) not passed"), !declared, "{log}");
            assert!(
                !log.contains("/run/user/1000/ssh.sock"),
                "the log does not repeat the socket path"
            );
        }
    }

    /// A trunk whose tip has a root `Cargo.toml`, so the merge base is warmable. The manifest
    /// declares no `[package]`: `crate_of` gives no crate, so a change that touches no crate adds
    /// no cargo gate and the merge's only gate is the one declared in `rules`. The rules are
    /// committed to `main`, so they are the trunk rules the warm-up reads at the base.
    fn main_with_cargo(root: &Path, rules: &str) -> (PathBuf, PathBuf) {
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), rules).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "cargo on main"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        (origin, repo)
    }

    /// The rules for a warmable repo: one always-passing declared gate, cargo on.
    fn warm_rules(cargo: &str) -> String {
        format!("[merge]\ndiffgate = false\ncargo = \"{cargo}\"\n[[gate]]\nname = \"trivial\"\nrun = \"exit 0\"\n")
    }

    /// A trunk with a base `Cargo.toml`, a topic that changes one non-crate file, `shadow` dropped
    /// on the job's PATH as `cargo`, and the merge run over it. Returns the landing origin, the
    /// outcome, the job log, and the base commit.
    fn warm_shadow_merge(
        root: &Path,
        rules: &str,
        shadow: &str,
    ) -> (PathBuf, Outcome, String, String) {
        let (origin, repo) = main_with_cargo(root, rules);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("cargo"), shadow);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let base = git(&origin, &["rev-parse", "main"]);
        let dir = root.join("job-warm");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        (origin, out, log, base)
    }

    #[test]
    fn a_warm_up_that_fails_does_not_fail_the_merge() {
        // The shadow cargo fails any `cargo check`, so the warm-up at the base fails; the declared
        // gate runs `exit 0` and never reaches cargo. The merge still lands, and the warm-up is not
        // in the report's gates.
        let shadow = "#!/bin/sh\nif [ \"$1\" = check ]; then exit 1; fi\nexit 0\n";
        let root_a = tempfile::tempdir().unwrap();
        let (origin, out, log, _base) =
            warm_shadow_merge(root_a.path(), &warm_rules("check"), shadow);
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
        assert!(
            log.contains("warm-up: cargo check of the merge base"),
            "{log}"
        );
        assert!(log.contains("warm-up: failed after"), "{log}");
        assert!(log.contains("(not a gate failure)"), "{log}");
        let names: Vec<&str> = out.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["trivial"], "{log}");
        assert!(out.gates.iter().all(|g| g.ok), "{log}");

        // The same merge with cargo = "off" skips the warm-up and reports the same gate results.
        let root_b = tempfile::tempdir().unwrap();
        let (_o, out_b, log_b, _b) = warm_shadow_merge(root_b.path(), &warm_rules("off"), shadow);
        assert!(!log_b.contains("warm-up"), "{log_b}");
        let names_b: Vec<&str> = out_b.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, names_b);
        assert_eq!(
            out.gates.iter().map(|g| g.ok).collect::<Vec<_>>(),
            out_b.gates.iter().map(|g| g.ok).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_warm_up_checks_the_merge_base_before_the_first_gate() {
        let root = tempfile::tempdir().unwrap();
        let head_file = root.path().join("cargo-check-head");
        // A warm-up that succeeds, recording the HEAD it ran under the way the env tests write names.txt.
        let shadow = format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then git -C . rev-parse HEAD >> {}; fi\nexit 0\n",
            head_file.display()
        );
        let (_origin, out, log, base) =
            warm_shadow_merge(root.path(), &warm_rules("check"), &shadow);
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        // It ran at the base commit, not the merge candidate.
        let recorded = std::fs::read_to_string(&head_file).unwrap();
        assert_eq!(recorded.trim(), base, "{recorded}\n{log}");
        assert_ne!(recorded.trim(), out.landed.unwrap());
        // Logged before the first gate, and it finished.
        let w = log
            .find("warm-up: cargo check of the merge base")
            .expect("warm-up line");
        let g = log.find("gate: trivial").expect("gate line");
        assert!(
            w < g,
            "the warm-up must be logged before the first gate:\n{log}"
        );
        assert!(log.contains("warm-up: done in"), "{log}");
    }

    #[test]
    fn a_repo_that_runs_no_cargo_skips_the_warm_up() {
        for cargo in ["off", "none"] {
            let root = tempfile::tempdir().unwrap();
            let check_file = root.path().join("saw-check");
            // The shadow records every `cargo check`; with cargo = off/none the warm-up must not run it.
            let shadow = format!(
                "#!/bin/sh\nif [ \"$1\" = check ]; then echo check >> {}; fi\nexit 0\n",
                check_file.display()
            );
            let (_origin, out, log, _base) =
                warm_shadow_merge(root.path(), &warm_rules(cargo), &shadow);
            assert_eq!(out.code, "MERGED", "{out:?}\nLOG:\n{log}");
            assert!(!log.contains("warm-up"), "{log}");
            assert!(!check_file.exists(), "cargo check ran for cargo = {cargo}");
        }
    }

    /// The pids of every process whose command line carries `marker` (the argv, never the
    /// environment). This is how the cap test proves nothing it started survived — by reading
    /// `/proc/*/cmdline`, not by `pgrep`/`pkill`.
    fn procs_with_marker(marker: &str) -> Vec<u32> {
        let mut hits = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return hits;
        };
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            // A pid that vanished between readdir and read is simply skipped.
            let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                continue;
            };
            if String::from_utf8_lossy(&cmdline)
                .split('\0')
                .any(|arg| arg.contains(marker))
            {
                hits.push(pid);
            }
        }
        hits
    }

    /// A cleanup guard for one warm-up test: on drop, and so even when an assertion below it
    /// fails, it signals SIGKILL to the processes carrying this run's own unique marker — and to
    /// nothing else. The warm-up's cap should already have ended them (its own process group, no
    /// `--foreground`), so on the passing path there is nothing to reap; this only stops a bug
    /// from leaving a `sleep 300` alive after the test. It is the one kill these tests are
    /// allowed, aimed only at this test's own marker pids (`#![forbid(unsafe_code)]` bars the
    /// `kill` syscall here, so it goes through `/bin/kill` — never `pkill`, never a pattern).
    struct MarkerReaper {
        marker: String,
    }

    impl Drop for MarkerReaper {
        fn drop(&mut self) {
            for pid in procs_with_marker(&self.marker) {
                let _ = Command::new("kill")
                    .args(["-9", "--", &pid.to_string()])
                    .status();
            }
        }
    }

    #[test]
    fn a_warm_up_that_outlives_its_cap_leaves_no_process() {
        let root = tempfile::tempdir().unwrap();
        let marker = format!("lucina-warmup-marker-{}", uuid::Uuid::new_v4().simple());
        // Reap this run's marker processes on drop, whatever the assertions do next.
        let _reaper = MarkerReaper {
            marker: marker.clone(),
        };

        // A fake cargo that, on `check`, forks a background `sleep` named with the marker and then
        // blocks in a foreground `sleep` — a warm-up that never ends on its own, so only the cap
        // stops it. Every other subcommand (the workspace's `cargo metadata`) is a plain success.
        let shadow = format!(
            "#!/bin/bash\nif [ \"$1\" = check ]; then\n  exec -a {marker} sleep 300 &\n  sleep 300\nfi\nexit 0\n"
        );
        let (origin, repo) = main_with_cargo(root.path(), &warm_rules("check"));
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("cargo"), &shadow);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let dir = root.path().join("job-warmup-cap");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root.path(), &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        // A few seconds, not the default 20 minutes.
        x.warm_up_secs = 2;

        let t = Instant::now();
        let out = x.merge();
        let elapsed = t.elapsed();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();

        // The merge still lands.
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        assert_eq!(
            git(&origin, &["rev-parse", "main"]),
            out.landed.unwrap(),
            "{log}"
        );
        // The cap fired, and said so as a timeout that is not a gate failure.
        assert!(log.contains("warm-up: failed after"), "{log}");
        assert!(
            log.contains("(not a gate failure): timed out after 2s"),
            "{log}"
        );
        // Its gate results equal the no-warm-up case: only the declared gate, passing.
        let names: Vec<&str> = out.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["trivial"], "{log}");
        assert!(out.gates.iter().all(|g| g.ok), "{log}");
        let root_off = tempfile::tempdir().unwrap();
        let (_o, out_off, _l, _b) = warm_shadow_merge(root_off.path(), &warm_rules("off"), &shadow);
        assert_eq!(
            names,
            out_off
                .gates
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            out.gates.iter().map(|g| g.ok).collect::<Vec<_>>(),
            out_off.gates.iter().map(|g| g.ok).collect::<Vec<_>>()
        );
        // The cap stopped it far short of the 300 s it would otherwise have slept.
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "the merge took {elapsed:?}\n{log}"
        );
        // And it left nothing behind: no process carries this run's marker.
        let leftover = procs_with_marker(&marker);
        assert!(
            leftover.is_empty(),
            "the warm-up left marker processes {leftover:?}\nLOG:\n{log}"
        );
    }
}

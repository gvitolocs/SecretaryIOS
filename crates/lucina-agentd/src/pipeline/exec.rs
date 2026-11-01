//! The job executor: `lagent pipeline-exec <job dir>`, started by agentd under the repo's
//! merge lock. It reads `job.json`, appends what it does to `log` and writes `result.json`.
//! Plain blocking code: it is one process doing one job.
//!
//! **Merge**: fetch the trunk; in the repo's landing worktree (`<work root>/.landing`,
//! detached) rebase the queued commit onto the trunk, or merge it when the rebase
//! conflicts; run the gates ([`super::repo::plan`]); push the result to the trunk (a
//! fast-forward; when the trunk moved meanwhile, start over, at most three times).
//! The session's own worktree is never touched here.
//!
//! **Deploy**: fetch; put the deploy worktree (`<work root>/.deploy`) at the trunk's tip;
//! `deploy-guard plan` (nothing new: stop unless forced), the recipe's build,
//! `deploy-guard check` (refused: stop), install, health, `deploy-guard record` (ledger,
//! tag, ops record).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::repo::{self, Crate, Gate, Rules};
use super::{GateResult, JobKind, Outcome, Spec};

/// The exit code: 0 when the job did what it was for.
pub fn run(dir: &Path) -> i32 {
    let spec: Spec = match std::fs::read_to_string(dir.join("job.json")).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
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
    let outcome = match x.spec.kind {
        JobKind::Merge => x.merge(),
        JobKind::Deploy => x.deploy(),
    };
    x.say(&format!("{}: {}", outcome.code, outcome.message));
    let ok = outcome.ok;
    let json = serde_json::to_string_pretty(&outcome).unwrap_or_default();
    let tmp = dir.join("result.json.tmp");
    if std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, dir.join("result.json"))).is_err() {
        return 2;
    }
    if ok {
        0
    } else {
        1
    }
}

fn fail(code: &str, message: impl Into<String>) -> Outcome {
    Outcome { ok: false, code: code.into(), message: message.into(), ..Outcome::default() }
}

struct Exec {
    spec: Spec,
    log: File,
    path: String,
}

/// `git` output, or its error text.
type Git = Result<String, String>;

impl Exec {
    fn open(dir: &Path, spec: Spec) -> Result<Self, String> {
        let log = OpenOptions::new().create(true).append(true).open(dir.join("log")).map_err(|e| format!("log: {e}"))?;
        let home = std::env::var("HOME").unwrap_or_default();
        // The same tools as a login shell: tests call mkfs.fat and friends, which live in sbin.
        let path = format!("{home}/.cargo/bin:{home}/.local/bin:{home}/bin:{}:/usr/sbin:/sbin", std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()));
        Ok(Exec { spec, log, path })
    }

    fn say(&mut self, msg: &str) {
        let _ = writeln!(self.log, "[{}] {msg}", chrono::Utc::now().format("%H:%M:%S"));
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
            Err(if err.is_empty() { format!("git {} failed", args.join(" ")) } else { err })
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
        self.say(&format!("$ {cmd}"));
        let (Ok(out), Ok(err)) = (self.log.try_clone(), self.log.try_clone()) else { return false };
        let status = Command::new("timeout")
            .args(["--kill-after=30s", &format!("{}m", timeout_min.max(1)), "bash", "-c", cmd])
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("CARGO_TARGET_DIR", &self.spec.target_dir)
            .env("LAGENT_JOB", &self.spec.job_id)
            .env("LAGENT_FORCE", if self.spec.force { "1" } else { "" })
            .env("CARGO_TERM_COLOR", "never")
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .status();
        match status {
            Ok(s) if s.success() => true,
            Ok(s) => {
                let why = if s.code() == Some(124) { format!("timed out after {timeout_min} min") } else { format!("exit {}", s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())) };
                self.say(&format!("failed: {why}"));
                false
            }
            Err(e) => {
                self.say(&format!("cannot run: {e}"));
                false
            }
        }
    }

    fn repo(&self) -> PathBuf {
        PathBuf::from(&self.spec.repo)
    }

    /// Fetch, then the trunk's commit.
    fn trunk_tip(&mut self) -> Git {
        let repo = self.repo();
        let trunk = self.spec.trunk.clone();
        match self.spec.remote.clone() {
            Some(remote) => {
                self.git_logged(&repo, &["fetch", "-q", &remote])?;
                self.git(&repo, &["rev-parse", "--verify", &format!("refs/remotes/{remote}/{trunk}^{{commit}}")])
            }
            None => self.git(&repo, &["rev-parse", "--verify", &format!("refs/heads/{trunk}^{{commit}}")]),
        }
    }

    /// A clean detached worktree at `sha` (made on first use, reset after).
    fn fresh_worktree(&mut self, name: &str, sha: &str) -> Result<PathBuf, String> {
        let dir = PathBuf::from(&self.spec.work_root).join(name);
        let repo = self.repo();
        if !dir.join(".git").exists() {
            let _ = self.git(&repo, &["worktree", "prune"]);
            std::fs::create_dir_all(&self.spec.work_root).map_err(|e| e.to_string())?;
            self.git_logged(&repo, &["worktree", "add", "--detach", &dir.display().to_string(), sha])?;
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
        let files = self.git(dir, &["ls-files", "--", "Cargo.toml", "*/Cargo.toml"]).unwrap_or_default();
        let mut crates = Vec::new();
        let mut has_workspace = false;
        let members = self.members(dir);
        for f in files.lines() {
            let Ok(text) = std::fs::read_to_string(dir.join(f)) else { continue };
            let d = f.strip_suffix("Cargo.toml").unwrap_or_default().trim_end_matches('/');
            if d.is_empty() && text.contains("[workspace]") {
                has_workspace = true;
            }
            if let Some(mut c) = repo::crate_of(d, &text) {
                if let Some(m) = &members {
                    c.standalone = !m.iter().any(|x| x == d);
                }
                crates.push(c);
            }
        }
        (crates, has_workspace)
    }

    /// The root workspace's member directories (relative), if cargo can say.
    fn members(&self, dir: &Path) -> Option<Vec<String>> {
        if !dir.join("Cargo.toml").is_file() {
            return None;
        }
        let out = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1", "--offline"])
            .current_dir(dir)
            .env("PATH", &self.path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let root = std::fs::canonicalize(dir).ok()?;
        let mut list = Vec::new();
        for p in v["packages"].as_array()? {
            let manifest = PathBuf::from(p["manifest_path"].as_str()?);
            let d = manifest.parent()?.strip_prefix(&root).ok()?;
            list.push(d.display().to_string());
        }
        Some(list)
    }

    fn run_gates(&mut self, dir: &Path, gates: &[Gate]) -> (Vec<GateResult>, Option<String>) {
        let mut results = Vec::new();
        for g in gates {
            self.say(&format!("gate: {}", g.name));
            let t = Instant::now();
            let mut ok = self.sh(dir, &g.run, g.timeout_min);
            let mut flaky = false;
            // Some tests time things or bind ports and fail when nezopt is loaded: a failed cargo
            // test runs once more, and a pass then is reported as flaky, not hidden.
            if !ok && g.run.starts_with("cargo test") {
                self.say(&format!("gate failed: {}; running it once more (flaky tests under load)", g.name));
                ok = self.sh(dir, &g.run, g.timeout_min);
                flaky = ok;
            }
            results.push(GateResult { name: g.name.clone(), command: g.run.clone(), ok, seconds: t.elapsed().as_secs(), flaky });
            if !ok {
                return (results, Some(g.name.clone()));
            }
        }
        (results, None)
    }

    pub fn merge(&mut self) -> Outcome {
        let (Some(branch), Some(head)) = (self.spec.branch.clone(), self.spec.head.clone()) else {
            return fail("INVALID", "a merge job needs a branch and a commit");
        };
        let title = self.spec.title.clone().unwrap_or_else(|| branch.clone());
        for attempt in 1..=3 {
            self.say(&format!("merge {branch} at {} into {} (attempt {attempt})", &head[..head.len().min(10)], self.spec.trunk));
            let base = match self.trunk_tip() {
                Ok(b) => b,
                Err(e) => return fail("FETCH_FAILED", e),
            };
            let repo = self.repo();
            if self.git(&repo, &["merge-base", "--is-ancestor", &head, &base]).is_ok() {
                return Outcome { ok: true, code: "NOTHING_TO_MERGE".into(), message: format!("{branch} is already on {}", self.spec.trunk), landed: Some(base), method: Some("already".into()), ..Outcome::default() };
            }
            let dir = match self.fresh_worktree(".landing", &base) {
                Ok(d) => d,
                Err(e) => return fail("LANDING_FAILED", e),
            };
            // Rebase first (a straight line on main); a branch whose own merges resolved
            // conflicts often only goes in as a merge.
            let mut method = "rebase";
            let rebased = self.git(&dir, &["checkout", "-q", "--detach", &head]).is_ok() && self.git_logged(&dir, &["rebase", "-q", &base]).is_ok();
            if !rebased {
                let _ = self.git(&dir, &["rebase", "--abort"]);
                method = "merge";
                self.say("the rebase conflicts; trying a merge");
                let msg = format!("Merge {branch}: {title}");
                let _ = self.git(&dir, &["checkout", "-q", "-f", "--detach", &base]);
                if self.git_logged(&dir, &["merge", "-q", "--no-ff", "-m", &msg, &head]).is_err() {
                    let conflicts: Vec<String> = self.git(&dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default().lines().map(str::to_string).collect();
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
            if candidate == base {
                return Outcome { ok: true, code: "NOTHING_TO_MERGE".into(), message: format!("every commit of {branch} is already on {}", self.spec.trunk), landed: Some(base), method: Some("already".into()), ..Outcome::default() };
            }
            let changed: Vec<String> = self.git(&dir, &["diff", "--name-only", &base, &candidate]).unwrap_or_default().lines().map(str::to_string).collect();
            self.say(&format!("{method}: {} file(s) changed", changed.len()));
            let mut rules = match self.rules(&base) {
                Ok(r) => r,
                Err(e) => return fail("RULES_INVALID", e),
            };
            if self.spec.full && rules.merge.cargo == "check" {
                rules.merge.cargo = "touched".into();
            }
            let (crates, has_workspace) = self.crates(&dir);
            let range = format!("{base}..{candidate}");
            let gates = repo::plan(&rules, &changed, &crates, has_workspace, &range, &self.spec.allow);
            let (results, failed) = self.run_gates(&dir, &gates);
            if let Some(g) = failed {
                return Outcome { ok: false, code: "GATE_FAILED".into(), message: format!("gate failed: {g} (lagent job logs {})", self.spec.job_id), method: Some(method.into()), gates: results, ..Outcome::default() };
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
                            match results.iter().filter(|r| r.flaky).map(|r| r.name.as_str()).collect::<Vec<_>>() {
                                f if f.is_empty() => String::new(),
                                f => format!("; flaky, passed on retry: {}", f.join(", ")),
                            }
                        ),
                        landed: Some(candidate),
                        method: Some(method.into()),
                        gates: results,
                        ..Outcome::default()
                    }
                }
                Err(Stop::Moved) => {
                    self.say(&format!("{} moved while the gates ran; starting over", self.spec.trunk));
                    continue;
                }
                Err(Stop::Blocked(why, conflicts)) => {
                    return Outcome { ok: false, code: "TRUNK_CHECKOUT_BLOCKED".into(), message: why, method: Some(method.into()), gates: results, conflicts, ..Outcome::default() };
                }
            }
        }
        fail("PUSH_FAILED", format!("{} kept moving; three attempts failed", self.spec.trunk))
    }

    /// Fast-forward the trunk from `base` to `candidate`.
    fn advance(&mut self, dir: &Path, base: &str, candidate: &str) -> Result<(), Stop> {
        let trunk = self.spec.trunk.clone();
        match self.spec.remote.clone() {
            Some(remote) => match self.git_logged(dir, &["push", "-q", &remote, &format!("{candidate}:refs/heads/{trunk}")]) {
                Ok(_) => Ok(()),
                Err(_) => Err(Stop::Moved),
            },
            None => {
                // A local trunk: where it is checked out, a fast-forward merge there (git keeps
                // uncommitted changes that don't touch the merged files, and refuses otherwise);
                // else the ref, if it is still at `base`.
                let repo = self.repo();
                let list = self.git(&repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
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
                    None => self.git_logged(&repo, &["update-ref", &format!("refs/heads/{trunk}"), candidate, base]).map(|_| ()).map_err(|_| Stop::Moved),
                }
            }
        }
    }

    pub fn deploy(&mut self) -> Outcome {
        let Some(runtime) = self.spec.runtime.clone() else { return fail("INVALID", "a deploy job needs a runtime") };
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
        let dir = match self.fresh_worktree(".deploy", &tip) {
            Ok(d) => d,
            Err(e) => return fail("DEPLOY_TREE_FAILED", e),
        };
        self.say(&format!("deploy {runtime} from {} at {}", self.spec.trunk, &tip[..10]));
        let plan = Command::new("deploy-guard").args(["plan", "--runtime", &runtime, "--repo"]).arg(&dir).env("PATH", &self.path).output();
        let plan = plan.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        self.say(&format!("plan: {plan}"));
        if plan.contains("up to date") && !self.spec.force {
            return Outcome { ok: true, code: "NOTHING_TO_DEPLOY".into(), message: format!("{runtime} already runs {}'s changes for it ({plan}); --force deploys anyway", self.spec.trunk), landed: Some(tip), ..Outcome::default() };
        }
        let t = recipe.timeout_min.unwrap_or(45);
        if let Some(build) = &recipe.build {
            if !self.sh(&dir, build, t) {
                return Outcome { ok: false, code: "BUILD_FAILED".into(), message: format!("the build failed (lagent job logs {})", self.spec.job_id), landed: Some(tip), ..Outcome::default() };
            }
        }
        let rq = repo::shell_quote(&runtime);
        let dq = repo::shell_quote(&dir.display().to_string());
        if !self.sh(&dir, &format!("deploy-guard check --runtime {rq} --repo {dq}"), 5) {
            return Outcome { ok: false, code: "REFUSED".into(), message: "deploy-guard refused the deploy (see the log)".into(), landed: Some(tip), ..Outcome::default() };
        }
        if !self.sh(&dir, &recipe.install, t) {
            return Outcome { ok: false, code: "INSTALL_FAILED".into(), message: format!("the install failed; check {runtime} now (lagent job logs {})", self.spec.job_id), landed: Some(tip), ..Outcome::default() };
        }
        if let Some(health) = &recipe.health {
            if !self.sh(&dir, health, 5) {
                return Outcome { ok: false, code: "UNHEALTHY".into(), message: format!("{runtime} is installed but its health check failed; not recorded"), landed: Some(tip), ..Outcome::default() };
            }
        }
        let note = format!("{} (lagent deploy {})", self.spec.note.clone().unwrap_or_else(|| format!("{} at {}", self.spec.trunk, &tip[..8])), self.spec.job_id);
        if !self.sh(&dir, &format!("deploy-guard record --runtime {rq} --repo {dq} --note {}", repo::shell_quote(&note)), 5) {
            return Outcome { ok: false, code: "RECORD_FAILED".into(), message: "installed, but deploy-guard record failed: record it by hand".into(), landed: Some(tip), ..Outcome::default() };
        }
        Outcome { ok: true, code: "DEPLOYED".into(), message: format!("{runtime} runs {} at {}", self.spec.trunk, &tip[..10]), landed: Some(tip), ..Outcome::default() }
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
    err.lines().filter(|l| l.starts_with([' ', '\t'])).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::super::{JobKind, Spec};
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
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
        Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&origin).status().unwrap();
        Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&repo).status().unwrap();
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
            branch: Some(branch.into()),
            head: Some(head.into()),
            title: Some("t".into()),
            runtime: None,
            note: None,
            allow: vec![],
            force: false,
            full: false,
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
    fn merges_rebase_first_then_reports_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\n[[gate]]\nname = \"has b\"\npaths = [\"b.txt\"]\nrun = \"test -f b.txt\"\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        // Topic branch, then main moves on (another file): a clean rebase.
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        git(&repo, &["checkout", "-q", "main"]);
        commit(&repo, "c.txt", "c\n", "add c");
        git(&repo, &["push", "-q", "origin", "main"]);
        let out = exec(root, spec(root, &repo, "topic", &head));
        assert!(out.ok, "{out:?}");
        assert_eq!((out.code.as_str(), out.method.as_deref()), ("MERGED", Some("rebase")));
        assert_eq!(out.gates.len(), 1);
        let landed = out.landed.unwrap();
        assert_eq!(git(&origin, &["rev-parse", "main"]), landed);
        assert_eq!(git(&origin, &["log", "--format=%s", "-3", "main"]), "add b\nadd c\nrules");
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
        // `cargo test` is shadowed by a script that fails the first time.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let marker = root.join("ran-once");
        std::fs::write(bin.join("cargo"), format!("#!/bin/sh\nif [ -f {m} ]; then exit 0; fi\ntouch {m}; exit 1\n", m = marker.display())).unwrap();
        Command::new("chmod").arg("+x").arg(bin.join("cargo")).status().unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), format!("[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"t\"\nrun = \"cargo test --quiet\"\n")).unwrap();
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
        assert!(out.ok, "{out:?}");
        assert!(out.gates[0].flaky);
        assert!(out.message.contains("flaky, passed on retry: t"), "{}", out.message);
        assert_eq!(git(&origin, &["rev-parse", "main"]).len(), 40);
    }

    #[test]
    fn gates_find_sbin_tools() {
        let root = tempfile::tempdir().unwrap();
        let x = Exec::open(root.path(), spec(root.path(), root.path(), "b", "abc")).unwrap();
        assert!(x.path.split(':').any(|d| d == "/usr/sbin"), "{}", x.path);
        assert!(x.path.split(':').any(|d| d == "/sbin"), "{}", x.path);
    }

    #[test]
    fn a_failing_gate_stops_the_merge() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\n[[gate]]\nname = \"never\"\nrun = \"exit 3\"\n").unwrap();
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
        Command::new("git").args(["init", "-q", "-b", "main"]).arg(&repo).status().unwrap();
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\n").unwrap();
        commit(&repo, "a.txt", "1\n", "a");
        git(&repo, &["branch", "topic"]);
        git(&repo, &["worktree", "add", "-q", &root.join("topic").display().to_string(), "topic"]);
        let head = commit(&root.join("topic"), "b.txt", "b\n", "add b");
        let mut s = spec(root, &repo, "topic", &head);
        s.remote = None;
        // Uncommitted work in the main checkout that the merge doesn't touch stays.
        std::fs::write(repo.join("notes.txt"), "mine\n").unwrap();
        let out = exec(root, s.clone());
        assert!(out.ok, "{out:?}");
        assert_eq!(git(&repo, &["rev-parse", "main"]), head);
        assert!(repo.join("b.txt").exists(), "the main checkout fast-forwarded");
        assert_eq!(std::fs::read_to_string(repo.join("notes.txt")).unwrap(), "mine\n");
        // Uncommitted changes to a file the merge changes: blocked, said why, main unchanged.
        let head2 = commit(&root.join("topic"), "c.txt", "topic\n", "add c");
        std::fs::write(repo.join("c.txt"), "local edit\n").unwrap();
        let out = exec(root, Spec { head: Some(head2), ..s });
        assert_eq!(out.code, "TRUNK_CHECKOUT_BLOCKED", "{out:?}");
        assert_eq!(out.conflicts, vec!["c.txt".to_string()]);
        assert_eq!(git(&repo, &["rev-parse", "main"]), head);
    }
}

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
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::kill::{self, Grant, Step};
use super::repo::{self, Crate, Gate, Rules};
use super::warm;
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
    x.announce_agentd();
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

/// The grace a step gets between the deadline's SIGTERM and the SIGKILL rounds: what
/// `timeout --kill-after=30s` gave a gate step until now, kept as it was. GATE-05 asked for the
/// killing to reach every process of the step, not for it to start later.
const KILL_GRACE: Duration = Duration::from_secs(30);

/// How long the question "does a step of this job get its own systemd scope?" may take to answer.
/// `/bin/true` in a scope of its own: a user manager that is there answers in tens of milliseconds,
/// one that is not fails at once.
const SCOPE_PROBE_CAP: Duration = Duration::from_secs(10);

/// The systemd slice every gate step's scope and the scope probe are started in (HOST-11): build work
/// belongs in `build.slice`, where the host puts its limits on builds, and not in the slice of whatever
/// started the job. Only the scope route carries it; the process-group route has no unit to place.
const SCOPE_SLICE: &str = "build.slice";

/// What the host answers to the one question every step asks: can it have a transient systemd
/// scope of its own, or is the process group all there is. Asked once per job, by asking and not by
/// guessing from the environment (finding GATE-05: the group alone is the route a nested scope
/// keeps, and it is not the route a `setsid` child leaves by).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    /// `systemd-run --user --scope` takes the unit it is asked for, so every step stands in a
    /// control group of its own as well as leading a process group.
    Takes,
    /// There is no user manager to ask, or it refused: the steps get their process group alone, and
    /// say so in the log. Never a reason to fail a step.
    NoUserManager(String),
    /// The probe did not answer within [`SCOPE_PROBE_CAP`]. Not the same as [`Scope::NoUserManager`]:
    /// a step must not lose its scope because of a probe that hung, and a test must not read the
    /// two answers as one.
    DidNotAnswer(String),
}

impl Scope {
    /// Why this answer says a step has no scope, for the one line each step says in its place.
    fn reason(&self) -> &str {
        match self {
            Scope::Takes => "",
            Scope::NoUserManager(r) | Scope::DidNotAnswer(r) => r,
        }
    }
}

/// The job's own scratch directory, held for as long as the job runs: what a step's `TMPDIR` points
/// at when the environment it was given would put temporary files under `/tmp` or `/var/tmp`
/// (finding FENCE-08), and gone with this guard whatever the job's outcome.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How long a gate that takes a bq slot waits for the grant, as bq's own `--max-wait` (LA-C038,
/// WORK-26). The merge job holds the repo's merge lock while it waits, so every other merge of that
/// repo queues behind it: a half-hour wait for the one gate slot is a host problem to surface, not
/// to sit in.
const BQ_QUEUE_CAP_SECS: u64 = 1800;

/// How long past [`BQ_QUEUE_CAP_SECS`] a step may wait for its grant: bq ends its own wait at the cap
/// (`--max-wait`), so a step still ungranted this much later has a `bq` that neither grants nor exits,
/// and the executor ends it like a step past its deadline (the job holds the repo's merge lock).
const QUEUE_SLACK: Duration = Duration::from_secs(30);

/// How long `bq` gets to print its usage when the executor asks whether the file at the home
/// directory is ours (as `scripts/check-fast` asks it).
const BQ_USAGE_CAP: Duration = Duration::from_secs(5);

/// The two names the executor hands the wrapped job: the gate command, and the marker file the job
/// writes first. Both are read and unset by the wrapper before the gate command starts, so the gate
/// sees neither. The command travels in a name and not in bq's argv because `systemd-run` expands
/// the braced `${…}` of an argv it is given, and a gate's text is not ours to rewrite.
const SLOT_COMMAND_VAR: &str = "LAGENT_GATE_COMMAND";
const SLOT_GRANT_VAR: &str = "LAGENT_GATE_GRANT";

/// Where the executor finds `bq`: `<passwd home>/.local/bin/bq`, the way `scripts/check-fast` finds
/// it, and never `PATH`, `$HOME` or a variable a step or a repo could set. A field of the executor so
/// that a test stands a fake in.
#[derive(Clone, Debug)]
enum BqSource {
    Home,
    /// The file to use (`None`: there is none).
    Fixed(Option<PathBuf>),
}

/// What looking for `bq` found.
#[derive(Debug, PartialEq, Eq)]
enum BqFind {
    Found(PathBuf),
    /// No `bq` here (GitHub's runners, a scratch repo on a host without it): the gate runs as it always did.
    Missing(String),
    /// A file is there and does not print bq's usage: the gate is refused.
    Refused(String),
}

/// A gate that takes a bq slot: the command line that starts it as the job of a `bq run`, and the
/// marker that job writes first.
struct SlotRun {
    name: String,
    wrapper: String,
    marker: PathBuf,
    queue_cap_secs: u64,
}

/// Does `usage` read as the usage of our `bq`: the class list and the options a gate's call passes?
fn bq_usage_is_ours(usage: &str) -> bool {
    ["--class short|long|proof", "--max-wait", "--priority"]
        .iter()
        .all(|w| usage.contains(w))
}

/// The wrapper text a gate step runs (`bash -c <this>`): `bq run --class long --priority gate
/// --max-wait <cap> --who <who> -- bash -c <script> bash`, replacing the step's shell by `bq`. bq
/// starts the script as its job once it holds the slot, with `BQ_SLOT` and `BQ_SLOT_PID` set; the
/// script writes the marker (a temporary file renamed into place, so the executor never reads half
/// of it), unsets the two names it was handed and `exec`s the gate command as `bash -c <command>
/// bash`, the shape every gate step has. A marker that cannot be written ends the job with 97
/// instead of running a gate whose clock never starts.
fn slot_wrapper(bq: &Path, who: &str, cap_secs: u64) -> String {
    let script = format!(
        "m=${SLOT_GRANT_VAR}; c=${SLOT_COMMAND_VAR}; unset {SLOT_COMMAND_VAR} {SLOT_GRANT_VAR}; \
         {{ printf %s \"$BQ_SLOT\" > \"$m.tmp\" && mv \"$m.tmp\" \"$m\"; }} || exit 97; \
         exec bash -c \"$c\" bash"
    );
    format!(
        "exec {} run --class long --priority gate --max-wait {cap_secs} --who {} -- bash -c {} bash",
        repo::shell_quote(&bq.display().to_string()),
        repo::shell_quote(who),
        repo::shell_quote(&script)
    )
}

/// The home directory of this uid from the passwd entry, as `scripts/check-fast` reads it: never
/// `$HOME`.
fn passwd_home() -> Option<PathBuf> {
    let uid = rustix::process::getuid().as_raw();
    let from_line = |l: &str| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 6 && f[2].parse::<u32>().ok() == Some(uid) && !f[5].is_empty())
            .then(|| PathBuf::from(f[5]))
    };
    // `getent` may ask sssd or systemd-userdbd over a socket, and on a badly configured host that is
    // a hang: it gets BQ_USAGE_CAP like the usage ask, and the passwd file is read when it does not
    // answer (R090 F3: it runs before any clock exists).
    let asked = Command::new("/usr/bin/getent")
        .args(["passwd", &uid.to_string()])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            let t = Instant::now();
            let answered = loop {
                match child.try_wait() {
                    Ok(Some(s)) => break s.success(),
                    Ok(None) if t.elapsed() < BQ_USAGE_CAP => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break false;
                    }
                }
            };
            if !answered {
                return None;
            }
            use std::io::Read;
            let mut text = String::new();
            child.stdout.take()?.read_to_string(&mut text).ok()?;
            text.lines().find_map(from_line)
        });
    asked.or_else(|| {
        std::fs::read_to_string("/etc/passwd")
            .ok()?
            .lines()
            .find_map(from_line)
    })
}

/// Does the program at `bq` print bq's usage when run without arguments, within [`BQ_USAGE_CAP`]?
/// `Err` is the real reason it does not: it cannot be run, it did not answer in time, or it ran and
/// printed something else (R090 F5).
fn bq_usage_check(bq: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut child = Command::new(bq)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot be run: {e}"))?;
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t.elapsed() < BQ_USAGE_CAP => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "did not print anything within {} s",
                    BQ_USAGE_CAP.as_secs()
                ));
            }
        }
    }
    let mut text = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut text);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut text);
    }
    if bq_usage_is_ours(&text) {
        Ok(())
    } else {
        Err("ran, and does not print bq's usage (--class short|long|proof, --max-wait, --priority), so it is not the queue this gate can call".into())
    }
}

/// [`bq_usage_check`] as a yes or no.
#[cfg(test)]
fn bq_prints_usage(bq: &Path) -> bool {
    bq_usage_check(bq).is_ok()
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
    /// a test may set it to a few seconds to watch the cap fire. It is the step's own deadline
    /// ([`Exec::sh_run`]), so it is the warm-up's length and not `timeout`'s argument.
    warm_up_secs: u64,
    /// How long a step gets to end itself after its deadline before the SIGKILL rounds start
    /// ([`KILL_GRACE`]; a test may shorten it to watch either half of the sequence).
    kill_grace: Duration,
    /// Whether this host takes a scope for a step, asked of the host once ([`Exec::probe_scope`]).
    user_scope: Option<Scope>,
    /// The job's own scratch, and so a step's `TMPDIR` where the inherited one is not usable
    /// ([`Exec::job_scratch`]).
    step_tmp: Option<PathBuf>,
    /// Where bq is looked for (BqSource), how long a gate waits for its slot, and how many
    /// grant markers this job has made (each gate run has its own).
    bq: BqSource,
    bq_queue_cap_secs: u64,
    /// How long past the queue cap a step may wait for its grant before it is ended ([`QUEUE_SLACK`]).
    queue_slack: Duration,
    grants: u32,
    /// The systemd unit this executor itself runs in (`lagent-job-<id>.service`), read once from its
    /// own control group at start: what every step's scope is bound to, so that the scope stops when
    /// this unit does. A field of the executor, not a name a step could set. `None`: not run as that
    /// unit (a test, a child-process fallback), and every step says so.
    executor_unit: Option<String>,
}

/// The unit this process runs in, when it is the runner unit of `job_id`: the last component of its
/// own control group names it.
fn own_unit(job_id: &str) -> Option<String> {
    let want = format!("{}.service", super::runner_unit(job_id));
    let t = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    t.lines()
        .filter_map(|l| l.strip_prefix("0::"))
        .any(|p| p.rsplit('/').next() == Some(want.as_str()))
        .then_some(want)
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
        let executor_unit = own_unit(&spec.job_id);
        Ok(Exec {
            executor_unit,
            gate_env: Vec::new(),
            dir: dir.to_path_buf(),
            waivers: None,
            source_env: std::env::vars_os().collect(),
            deploy_env: Vec::new(),
            warm_up_secs: 20 * 60,
            kill_grace: KILL_GRACE,
            user_scope: None,
            step_tmp: None,
            // A test run never asks the real queue: an executor built for a test has no bq unless the
            // test gives it one.
            bq: if cfg!(test) {
                BqSource::Fixed(None)
            } else {
                BqSource::Home
            },
            bq_queue_cap_secs: BQ_QUEUE_CAP_SECS,
            queue_slack: QUEUE_SLACK,
            grants: 0,
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

    /// PIPE-05: agentd identity, read once for the whole job, and the one line the job log gets of it.
    fn announce_agentd(&mut self) {
        let (mut env, _) = self.built_env();
        env.push((OsString::from("PATH"), self.path.clone().into()));
        let ctl = self.resolve("systemctl");
        for line in kill::announce_agentd(ctl.as_deref(), &env) {
            self.say(&line);
        }
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

    /// `bash -c cmd` in `cwd` with output into the log, under `timeout_min` minutes. Returns its
    /// success.
    fn sh(&mut self, cwd: &Path, cmd: &str, timeout_min: u64) -> bool {
        let mins = timeout_min.max(1);
        self.sh_run(
            cwd,
            cmd,
            Duration::from_secs(mins * 60),
            &format!("timed out after {timeout_min} min"),
        )
        .is_none()
    }

    /// Where `bq` is, and whether it is ours ([`BqFind`]).
    fn find_bq(&self) -> BqFind {
        let path = match &self.bq {
            BqSource::Fixed(None) => return BqFind::Missing("there is no bq here".into()),
            BqSource::Fixed(Some(p)) => p.clone(),
            BqSource::Home => {
                let Some(home) = passwd_home() else {
                    return BqFind::Missing(
                        "the home directory of this uid cannot be read from the passwd entry"
                            .into(),
                    );
                };
                let p = home.join(".local/bin/bq");
                if std::fs::symlink_metadata(&p).is_err() {
                    return BqFind::Missing(format!("{} does not exist", p.display()));
                }
                p
            }
        };
        match bq_usage_check(&path) {
            Ok(()) => BqFind::Found(path),
            Err(why) => BqFind::Refused(format!("{} {why}", path.display())),
        }
    }

    /// A gate (LA-C038, WORK-26): `run` in `cwd` under the limit `cap`, said as `limit` in the
    /// failure. With `slot`, the command is the job of a `bq run` that takes a `long` slot first, as a
    /// gate (`--priority gate`, `--who lagent-merge-<job id>`, at most [`BQ_QUEUE_CAP_SECS`] of waiting),
    /// and `cap` counts from the grant: the slot is taken before the clock starts. No `bq` here: the
    /// gate runs as it always did, with a line saying why. A `bq` that is not ours: the gate fails
    /// and nothing runs.
    fn gate_run(
        &mut self,
        cwd: &Path,
        name: &str,
        run: &str,
        slot: bool,
        cap: Duration,
        limit: &str,
    ) -> bool {
        let mut timed_out = format!("timed out after {limit}");
        let mut slot_run = None;
        if slot {
            match self.find_bq() {
                BqFind::Found(bq) => {
                    self.grants += 1;
                    let dir = std::fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
                    let who = format!("lagent-merge-{}", self.spec.job_id);
                    slot_run = Some(SlotRun {
                        name: name.to_string(),
                        wrapper: slot_wrapper(&bq, &who, self.bq_queue_cap_secs),
                        marker: dir.join(format!("grant-{}", self.grants)),
                        queue_cap_secs: self.bq_queue_cap_secs,
                    });
                    timed_out.push_str(" (clock started at the bq grant)");
                }
                BqFind::Missing(why) => self.say(&format!(
                    "gate: {name} takes no bq slot ({why}); it runs without one"
                )),
                BqFind::Refused(why) => {
                    // A queue that is there and does not work fails the gate: a decision, so that a broken
                    // bq is seen and not quietly run around (R090 F5).
                    self.say(&format!(
                        "gate: {name} refused: {why}; a bq that is there and does not work fails the gate, it does not run unqueued"
                    ));
                    return false;
                }
            }
        }
        self.sh_run_slot(cwd, run, cap, &timed_out, slot_run.as_ref())
            .is_none()
    }

    /// [`Exec::gate_run`] for a planned gate, under its `timeout_min`.
    fn gate_sh(&mut self, cwd: &Path, g: &Gate) -> bool {
        let mins = g.timeout_min.max(1);
        self.gate_run(
            cwd,
            &g.name,
            &g.run,
            g.slot,
            Duration::from_secs(mins * 60),
            &format!("{} min", g.timeout_min),
        )
    }

    /// The step's environment as the job builds it: the allowlist from the source environment plus
    /// the deploy recipe's declared names, and how many names were kept back — a count only, never
    /// a name or a value.
    fn built_env(&self) -> (Vec<(OsString, OsString)>, usize) {
        let extra: Vec<&str> = self.deploy_env.iter().map(String::as_str).collect();
        crate::env::scrubbed_env_from(self.source_env.iter().cloned(), &extra)
    }

    /// `name` as a step runs it: found in the job's own PATH, the one the step is given. std looks a
    /// bare program name up in *this process's* PATH instead, and the job's PATH is the promise the
    /// gate runs under — the same door `an_exported_cargo_function_does_not_beat_the_gate_s_path`
    /// closed for the command itself.
    fn resolve(&self, name: &str) -> Option<PathBuf> {
        std::env::split_paths(&self.path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    }

    /// Whether this job's steps get a transient systemd scope of their own, asked of the host once
    /// ([`Exec::probe_scope`]) and kept for the rest of the job. With no `systemd-run` on the job's
    /// own PATH there is nothing to ask, and no reason to fork for the answer.
    fn user_scope(&mut self, built: &[(OsString, OsString)], systemd_run: Option<&Path>) -> Scope {
        if let Some(s) = &self.user_scope {
            return s.clone();
        }
        let scope = match systemd_run {
            None => Scope::NoUserManager("systemd-run is not on the job's PATH".to_string()),
            Some(program) => self.probe_scope(program, built),
        };
        self.user_scope = Some(scope.clone());
        scope
    }

    /// Ask the host: run `/bin/true` in a transient scope under a probe's own unit name, bounded, with
    /// its noise kept out of the job log (a refusal says nothing a job needs to read twice — the reason
    /// is the exit). It goes through [`Exec::step_argv_bound`], the very argv a step gets (the binding to the executor's
    /// unit included), and the environment
    /// a step gets, because those are what the question is about: whether this user has a manager that
    /// will make *this* unit from *this* command line. A host or a wrapper that takes the scope but not
    /// one of its options answers the probe yes and every step no, and that is the failure this asks its
    /// way out of (finding LA-R059, PROBE-01: the lane's shim for `systemd-run` allows
    /// `--user --scope --quiet --collect --unit` and refuses any option it does not list, so
    /// `--expand-environment=no` — which a step carries and the probe did not — made the probe answer
    /// yes and 23 gate steps exit 79).
    fn probe_scope(&self, program: &Path, built: &[(OsString, OsString)]) -> Scope {
        // A user manager is the bus, and the bus is named by this one variable. Without it there
        // is nobody to ask and `systemd-run` says so by failing at once.
        if !built.iter().any(|(k, _)| k == "XDG_RUNTIME_DIR") {
            return Scope::NoUserManager(
                "XDG_RUNTIME_DIR is not set, so there is no systemd user manager to ask"
                    .to_string(),
            );
        }
        let Some(bash) = self.resolve("bash") else {
            return Scope::NoUserManager("bash is not on the job's PATH".to_string());
        };
        let unit = kill::probe_name();
        let (program, argv) = Self::step_argv_bound(
            &bash,
            Some(program),
            Some(&unit),
            "/bin/true",
            self.executor_unit.as_deref(),
        );
        let mut child = match Command::new(program)
            .args(argv)
            .current_dir(&self.dir)
            .env_clear()
            .envs(built.iter().cloned())
            .env("PATH", &self.path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return Scope::NoUserManager(format!("systemd-run could not be started: {e}"))
            }
        };
        // The probe asks the scope question, so it must not also be what the question is about: it
        // gets the process group alone, which is enough to end a `/bin/true`.
        let ended = Step::new(child.id(), None, None).run_to_end(
            &mut child,
            SCOPE_PROBE_CAP,
            Duration::from_millis(200),
        );
        if ended.timed_out || ended.status.is_none() {
            return Scope::DidNotAnswer(format!(
                "the {unit} probe did not answer within {}s",
                SCOPE_PROBE_CAP.as_secs()
            ));
        }
        let code = ended.status.and_then(|s| s.code());
        match code {
            Some(0) => Scope::Takes,
            Some(c) => Scope::NoUserManager(format!("the {unit} probe exited {c}")),
            None => Scope::NoUserManager(format!("the {unit} probe died on a signal")),
        }
    }

    /// The job's own scratch directory, made before its first step and taken away with the returned
    /// guard: what a step's `TMPDIR` is when the environment it was given would put temporary files
    /// under `/tmp` or `/var/tmp` (finding FENCE-08: everything here runs with `TMPDIR=/tmp`, and
    /// the host sandbox `lucina-agents` checks for itself refuses a test file under it, so every
    /// `lagent merge --full` that touched that crate failed on a step that had done nothing wrong).
    /// `None`, with a line in the log, when it cannot be made — which is not a reason to fail a
    /// step, only to give it the `TMPDIR` it came with.
    fn job_scratch(&mut self) -> Option<Scratch> {
        let dir = std::fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
        let scratch = dir.join("tmp");
        match std::fs::create_dir_all(&scratch) {
            Ok(()) => {
                self.step_tmp = Some(scratch.clone());
                Some(Scratch(scratch))
            }
            Err(e) => {
                self.say(&format!(
                    "cannot make the job's scratch {}: {e}; a step gets the TMPDIR it was given",
                    scratch.display()
                ));
                None
            }
        }
    }

    /// Whether a step's `TMPDIR` sends its temporary files where the host will refuse them: absent,
    /// empty, relative, or under `/tmp` or `/var/tmp`. A caller who set somewhere sane keeps it —
    /// the built environment's value travels with its name (`a_gate_command_gets_the_allowlisted_environment`).
    fn tmpdir_is_unusable(v: Option<&std::ffi::OsStr>) -> bool {
        let Some(t) = v.filter(|t| !t.is_empty()) else {
            return true;
        };
        let p = Path::new(t);
        let mut c = p.components();
        let (Some(std::path::Component::RootDir), Some(std::path::Component::Normal(first))) =
            (c.next(), c.next())
        else {
            // Relative, empty, or the root itself: nothing a step's temporary files belong in.
            return true;
        };
        first == "tmp"
            || (first == "var"
                && matches!(c.next(), Some(std::path::Component::Normal(second)) if second == "tmp"))
    }

    /// The step's command line as [`Exec::sh_run`] starts it, both routes in one place so that what a step is
    /// *handed* is testable without a host to ask: the program to start and the arguments that follow it.
    /// `bash -c <cmd> bash` is the step whichever way the scope went — the word after the command is the name
    /// bash reads into `$0` — so a gate that asks its own shell what it was called hears `bash`, what
    /// `timeout … bash -c` gave and a script file of the step's own took away (finding LA-R059, item 2).
    /// The braced `${…}` of the command text is what `systemd-run` would expand in an argv it is given — the one
    /// reason a script file looked necessary — and `--expand-environment=no` (systemd 254; this host runs 259)
    /// turns that off, so the command reaches `bash` as the repo wrote it and no file is left in the job
    /// directory at all.
    ///
    /// This is the unbound form, which only the tests of the argv's shape call; a step and the scope probe go
    /// through [`Exec::step_argv_bound`], which adds the binding to the executor's unit when it is known.
    #[cfg(test)]
    fn step_argv(
        bash: &Path,
        systemd_run: Option<&Path>,
        unit: Option<&str>,
        cmd: &str,
    ) -> (PathBuf, Vec<std::ffi::OsString>) {
        Self::step_argv_bound(bash, systemd_run, unit, cmd, None)
    }

    /// [`Exec::step_argv`], and with `bind` (`Some(<executor unit>)`) a scope that `BindsTo` and
    /// starts `After` that unit: when the executor's unit stops, or its main process dies, systemd
    /// stops the scope, which takes the step (a `bq run` holding a slot included) with it (LA-C038).
    fn step_argv_bound(
        bash: &Path,
        systemd_run: Option<&Path>,
        unit: Option<&str>,
        cmd: &str,
        bind: Option<&str>,
    ) -> (PathBuf, Vec<std::ffi::OsString>) {
        // `-c <cmd>` and then the name bash reads into `$0` for it — the argument after the command string,
        // which bash calls its own `argv[0]`. That name is what item 2 is about: `timeout … bash -c` gave a
        // gate a `$0` of `bash`, a script file of the step's own gave it the file's path, and the program
        // here stays the absolute path this job resolved, so the step depends on no lookup but the one
        // above.
        let step = vec![
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from(cmd),
            std::ffi::OsString::from("bash"),
        ];
        match (unit, systemd_run) {
            (Some(u), Some(program)) => {
                let mut argv = vec![
                    std::ffi::OsString::from("--user"),
                    std::ffi::OsString::from("--scope"),
                    std::ffi::OsString::from("--quiet"),
                    std::ffi::OsString::from("--collect"),
                    std::ffi::OsString::from("--unit"),
                    std::ffi::OsString::from(u),
                    std::ffi::OsString::from("--expand-environment=no"),
                    std::ffi::OsString::from(format!("--slice={SCOPE_SLICE}")),
                ];
                if let Some(b) = bind {
                    for p in [format!("BindsTo={b}"), format!("After={b}")] {
                        argv.push(std::ffi::OsString::from("-p"));
                        argv.push(std::ffi::OsString::from(p));
                    }
                }
                argv.push(std::ffi::OsString::from("--"));
                argv.push(bash.to_path_buf().into());
                argv.extend(step);
                (program.to_path_buf(), argv)
            }
            _ => (bash.to_path_buf(), step),
        }
    }

    /// [`Exec::sh`]'s worker, and the warm-up's: `bash` over `cmd` in `cwd`, under the step's own
    /// deadline (`cap`, reported with the phrase `timed_out`), with output into the job log, leading
    /// a process group of its own and — where the host takes one ([`Exec::user_scope`]) — standing in
    /// a transient systemd scope of its own. Returns `None` when the command succeeded, or the reason
    /// it did not: `timed_out` on a timeout, otherwise the exit code.
    ///
    /// `timeout(1)` is not in this. On this host (uutils coreutils) its final SIGKILL reaches only
    /// the process it waits on, so a grandchild that ignores SIGTERM outlived the limit and, still
    /// holding the shared `CARGO_TARGET_DIR`'s cargo lock, kept the next gate busy for longer than
    /// its own cap allowed (finding GATE-05). The deadline and every signal after it are the job's
    /// own instead, aimed at both routes to the step's processes ([`super::kill`]: the group, for
    /// what a step hands to a nested scope, and the scope's control group, for a child that leaves
    /// the group), and they come again after a step that ended by itself: no step of a job leaves a
    /// process behind. The command's environment, PATH, `CARGO_TARGET_DIR`, log lines and outcomes
    /// are the ones `sh_run` always built.
    fn sh_run(&mut self, cwd: &Path, cmd: &str, cap: Duration, timed_out: &str) -> Option<String> {
        self.sh_run_slot(cwd, cmd, cap, timed_out, None)
    }

    /// [`Exec::sh_run`], and with `slot` (`Some`) the command runs as the job of a `bq run` that takes
    /// a slot first: the executor waits for the grant, and `cap` counts from the grant, not from the
    /// start (LA-C038, WORK-26). The slot is bq's: this starts the `bq run` as the step's own process
    /// and promises nothing about releasing it; the deadline, the kill rounds and the sweep reach the
    /// `bq` process with everything else the step started, and bq's holder check frees the slot when
    /// that process is gone.
    fn sh_run_slot(
        &mut self,
        cwd: &Path,
        cmd: &str,
        cap: Duration,
        timed_out: &str,
        slot: Option<&SlotRun>,
    ) -> Option<String> {
        // A step with a slot says its command line when it is granted; "queued" is said once the
        // `bq run` has started (R090 F11), not before bash, the scope and bq are known to exist.
        if slot.is_none() {
            self.say(&format!("$ {cmd}"));
        }
        // The command gets the built environment (the allowlist from the executor's own
        // environment, plus the deploy recipe's declared names), not what the executor
        // inherited. How many names were kept back is said by count only.
        let (mut built, not_passed) = self.built_env();
        // A slot the executor was itself started under is somebody else's: no step, no scope probe and no
        // manager ask is handed the pair, so none can take it for its own (R090 F7). bq sets its own pair
        // for the job it starts.
        built.retain(|(k, _)| k != "BQ_SLOT" && k != "BQ_SLOT_PID");
        if not_passed > 0 {
            self.say(&format!("env: {not_passed} variable(s) not passed"));
        }
        // FENCE-08: a step whose `TMPDIR` would put its temporary files under `/tmp` or `/var/tmp`
        // gets the job's own scratch instead. A name the built environment carries is only sent
        // elsewhere when it is unusable, so an allowlisted value that is fine keeps its meaning.
        if let Some(scratch) = &self.step_tmp {
            if Self::tmpdir_is_unusable(
                built
                    .iter()
                    .find(|(k, _)| k == "TMPDIR")
                    .map(|(_, v)| v.as_os_str()),
            ) {
                built.retain(|(k, _)| k != "TMPDIR");
                built.push((OsString::from("TMPDIR"), scratch.clone().into()));
            }
        }
        let (Ok(out), Ok(err)) = (self.log.try_clone(), self.log.try_clone()) else {
            // No duplicated fd means the command never starts. Without a word here the job ends
            // as GATE_FAILED with a log that says nothing about it: a gate that could not be run
            // reads exactly like a gate that ran and was refused.
            self.say("cannot run: the job log could not be duplicated for the command's output");
            return Some("cannot run: the job log could not be duplicated".to_string());
        };
        let Some(bash) = self.resolve("bash") else {
            self.say("cannot run: bash is not on the job's PATH");
            return Some("cannot run: bash is not on the job's PATH".to_string());
        };
        let systemd_run = self.resolve("systemd-run");
        let scope = self.user_scope(&built, systemd_run.as_deref());
        let unit = (scope == Scope::Takes).then(|| kill::unit_name(&self.spec.job_id));
        // The command goes to the step as `bash -c <cmd>`, exactly as it did under `timeout(1)`, so that `$0`
        // inside a gate is still `bash`: a script file of its own would make it the path of that file, and a
        // gate that reads `dirname "$0"` or `BASH_SOURCE` would then act on the job directory (finding
        // LA-R059, item 2). What made the file tempting is that `systemd-run` expands the braced form `${…}`
        // in the payload argv it is handed, so a gate whose own text tests
        // `[ -n "${LAGENT_WAIVERS_FILE+x}" ]` would arrive at its shell with the test already answered; that is
        // what `--expand-environment=no` turns off (systemd 254, and this host runs 259), and nothing of the
        // command text is rewritten on the way in.
        // The step's own transient scope, the way `manager/work.rs` starts a lane runner:
        // `systemd-run --scope` execs its payload into the unit it made, so the step's pid leads
        // the process group *and* is the only member of the scope. Where there is no user manager,
        // the group is what there is, and the job says so once per step rather than failing it. A scope that
        // was asked for is only said of once it has been *seen* (`kill::Sighting`).
        let systemctl = self.resolve("systemctl");
        // A gate that takes a slot runs as `bq run … -- <the gate command>` (the step is that `bq`, by
        // `exec`); the gate command and the marker travel in two names that the wrapper reads and unsets.
        let started = match slot {
            Some(s) => s.wrapper.clone(),
            None => cmd.to_string(),
        };
        let bind = unit.as_ref().and(self.executor_unit.clone());
        let (program, argv) = Self::step_argv_bound(
            &bash,
            systemd_run.as_deref(),
            unit.as_deref(),
            &started,
            bind.as_deref(),
        );
        if unit.is_none() {
            self.say(&format!(
                "scope: none ({}): the step has its process group alone",
                scope.reason()
            ));
        }
        // One line when the step is not bound to the executor's unit, and the reason: no silent difference.
        if bind.is_none() {
            self.say(&format!(
                "bound: no ({}): this step does not stop with the executor",
                if unit.is_none() {
                    "the step has no scope of its own"
                } else {
                    "the executor does not know its own systemd unit"
                }
            ));
        }
        let mut command = Command::new(program);
        command
            .args(argv)
            .current_dir(cwd)
            // The group the deadline and every signal after it works on (`super::kill`), and the
            // group a step's own `fork` work stays in unless it leaves on purpose.
            .process_group(0)
            .env_clear()
            .envs(built.iter().cloned())
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
            .stderr(Stdio::from(err));
        if let Some(s) = slot {
            command
                .env(SLOT_COMMAND_VAR, cmd)
                .env(SLOT_GRANT_VAR, &s.marker);
            let _ = std::fs::remove_file(&s.marker);
        }
        let started_at = Instant::now();
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.say(&format!("cannot run: {e}"));
                return Some(format!("cannot run: {e}"));
            }
        };
        if let Some(s) = slot {
            self.say(&format!(
                "gate: {} queued for a bq slot (class long, priority gate, waits at most {} s)",
                s.name, s.queue_cap_secs
            ));
        }
        // The two routes to every process this step started, the deadline, and the sweep after it.
        let grace = self.kill_grace;
        // The manager ask is a child of this process too, so it runs under the same cleared
        // environment the probe and the step do: what a shell or an EnvironmentFile would hand a
        // restart, it does not see.
        let step_env = built
            .iter()
            .cloned()
            .chain(std::iter::once((
                OsString::from("PATH"),
                self.path.clone().into(),
            )))
            .collect();
        let mut step = Step::new(child.id(), unit, systemctl).with_env(step_env);
        if let Some(s) = slot {
            // The clock starts at the grant. The grant is announced in the log the moment it is seen,
            // with the command line after it.
            // The grant is armed whether or not the log can be duplicated for its line: the clock and the
            // queue bound depend on it, the line does not (R090 F9).
            let mut log = self.log.try_clone().ok();
            let (name, shown) = (s.name.clone(), cmd.to_string());
            step = step.with_grant(Grant {
                marker: s.marker.clone(),
                queue_bound: Duration::from_secs(s.queue_cap_secs) + self.queue_slack,
                seen: Box::new(move |slot_name, after| {
                    let Some(log) = log.as_mut() else { return };
                    let now = chrono::Utc::now().format("%H:%M:%S");
                    let _ = writeln!(
                        log,
                        "[{now}] gate: {name} got {slot_name} after {}s",
                        after.as_secs()
                    );
                    let _ = writeln!(log, "[{now}] $ {shown}");
                }),
            });
        }
        let ended = step.run_to_end(&mut child, cap, grace);
        if let Some(s) = slot {
            let _ = std::fs::remove_file(&s.marker);
        }
        // Only now is the scope a fact: the unit was seen as the step's control group, or it never was and
        // the step is worked by its process group alone whatever the name at its start said.
        if let Some(line) = ended.scope.line() {
            self.say(&line);
        }
        if let Some(note) = &ended.manager_note {
            self.say(note);
        }
        if !ended.miss.says_nothing() {
            self.say(&ended.miss.line());
        }
        if ended.left > 0 {
            self.say(&format!(
                "{} of the step's processes were still there when the killing gave up",
                ended.left
            ));
        }
        // No grant within the queue cap plus its slack: bq neither granted nor ended, and the step was ended.
        if ended.queue_expired {
            let waited = format!(
                "waited for a bq slot for {} s: bq neither granted the slot nor ended within the queue cap plus {} s, so the step was ended",
                started_at.elapsed().as_secs(),
                self.queue_slack.as_secs()
            );
            self.say(&format!("failed: {waited}"));
            return Some(waited);
        }
        // Never granted and not ended by its limit: the wait itself is what failed (bq's own
        // `--max-wait`, or bq refusing the job), and the limit of the gate was never in play.
        if slot.is_some() && ended.granted_after.is_none() && !ended.timed_out {
            if let Some(code) = ended.status.filter(|s| !s.success()) {
                // 97 is the wrapper's own: the job started and could not write the grant marker.
                let waited = if code.code() == Some(97) {
                    "the bq job started but could not write its grant marker (wrapper exit 97), so the gate did not run".to_string()
                } else {
                    format!(
                        "waited for a bq slot for {} s without a grant (bq exit {})",
                        started_at.elapsed().as_secs(),
                        code.code().map_or("signal".to_string(), |c| c.to_string())
                    )
                };
                self.say(&format!("failed: {waited}"));
                return Some(waited);
            }
        }
        if ended.timed_out {
            self.say(&format!("failed: {timed_out}"));
            return Some(timed_out.to_string());
        }
        match ended.status {
            Some(s) if s.success() => None,
            Some(s) => {
                let why = format!(
                    "exit {}",
                    s.code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into())
                );
                self.say(&format!("failed: {why}"));
                Some(why)
            }
            None => {
                // No status and no timeout: the wait itself could not be had. Report it as its own
                // failure rather than as a step that ended.
                self.say("failed: the step did not answer within its bound");
                Some("did not answer within its bound".to_string())
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
            let mut ok = self.gate_sh(dir, g);
            let mut flaky = false;
            // Some tests time things or bind ports and fail when nezopt is loaded: a failed cargo
            // test runs once more, and a pass then is reported as flaky, not hidden.
            if !ok && g.run.starts_with("cargo test") {
                self.say(&format!(
                    "gate failed: {}; running it once more (flaky tests under load)",
                    g.name
                ));
                ok = self.gate_sh(dir, g);
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
    /// `Cargo.toml` to check. It runs through [`Exec::sh_run`] like every other step — the same
    /// clean environment, PATH and `CARGO_TARGET_DIR`, and the same deadline, signals and sweep, so
    /// a warm-up that is ended by its cap ends *whole* — under its own cap
    /// ([`Exec::warm_up_secs`]) and with `gate_env` kept empty, so it gets none of the gate
    /// allow/waivers. The reason it returns (a timeout in particular) goes into the
    /// `warm-up: failed` line.
    ///
    /// Then (LA-C026, GATE-13) the gate's own clippy for the branch's touched crates, so the gate's
    /// clippy — whose artefacts are not `cargo check`'s — finds a warm target too: the commands are
    /// [`super::warm::clippy_commands`]. They run through the same [`Exec::sh_run`], one line before
    /// and one after each, and share the warm-up's one cap with the check: a command gets what is
    /// left of [`Exec::warm_up_secs`], and none starts once it is spent. Whatever they do — fail,
    /// time out, or never start because the member names could not be had — is only logged.
    fn warm_up(&mut self, dir: &Path, base: &str, head: &str, rules: &Rules) {
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
        let started = Instant::now();
        let cap = self.warm_up_secs;
        self.warm_step(dir, "cargo check --workspace --quiet", started, cap);
        self.warm_clippy(dir, base, head, started, cap);
    }

    /// One warm-up command, under what is left of the warm-up's `cap` seconds counted from
    /// `started`. Its result is logged and goes nowhere else.
    fn warm_step(&mut self, dir: &Path, cmd: &str, started: Instant, cap: u64) {
        let left = Duration::from_secs(cap).saturating_sub(started.elapsed());
        if left.is_zero() {
            self.say(&format!(
                "warm-up: {cmd} not run (not a gate): the warm-up's {cap}s are spent"
            ));
            return;
        }
        let t = Instant::now();
        let why = self.sh_run(dir, cmd, left, &format!("timed out after {cap}s"));
        match why {
            None => self.say(&format!("warm-up: done in {}s", t.elapsed().as_secs())),
            Some(reason) => self.say(&format!(
                "warm-up: failed after {}s (not a gate failure): {reason}",
                t.elapsed().as_secs()
            )),
        }
    }

    /// The clippy half of the warm-up: what the branch touches, mapped to the gate's clippy
    /// commands, each run as a [`Exec::warm_step`] under the shared cap. A branch whose files give
    /// the gate no clippy step runs nothing and says nothing.
    fn warm_clippy(&mut self, dir: &Path, base: &str, head: &str, started: Instant, cap: u64) {
        let files: Vec<String> = match self
            .git(dir, &["diff", "--name-only", &format!("{base}...{head}")])
        {
            Ok(out) => out.lines().map(str::to_string).collect(),
            Err(e) => {
                self.say(&format!(
                    "warm-up: clippy skipped (not a gate): the branch's files could not be listed: {e}"
                ));
                return;
            }
        };
        if !files
            .iter()
            .any(|f| f.ends_with(".rs") || f.ends_with("Cargo.toml"))
        {
            return;
        }
        let Some(scratch) = self.step_tmp.clone() else {
            self.say(
                "warm-up: clippy skipped (not a gate): the job has no scratch for cargo metadata",
            );
            return;
        };
        let out_file = scratch.join("warm-up-metadata.json");
        if out_file.display().to_string().contains('\'') {
            self.say("warm-up: clippy skipped (not a gate): the scratch path cannot be quoted");
            return;
        }
        let left = Duration::from_secs(cap).saturating_sub(started.elapsed());
        if left.is_zero() {
            self.say("warm-up: clippy skipped (not a gate): the warm-up's time is spent");
            return;
        }
        let cmd = format!(
            "cargo metadata --no-deps --format-version 1 --offline > '{}'",
            out_file.display()
        );
        let meta = self.sh_run(dir, &cmd, left, &format!("timed out after {cap}s"));
        let members = match (meta, std::fs::canonicalize(dir)) {
            (None, Ok(root)) => std::fs::read(&out_file)
                .ok()
                .and_then(|j| warm::member_map(&j, &root)),
            _ => None,
        };
        let _ = std::fs::remove_file(&out_file);
        let Some(members) = members else {
            self.say("warm-up: clippy skipped (not a gate): the workspace members could not be read from cargo metadata");
            return;
        };
        let root = dir.to_path_buf();
        let manifest = |d: &str| std::fs::read_to_string(root.join(d).join("Cargo.toml")).ok();
        for cmd in warm::clippy_commands(&files, &members, &manifest) {
            self.say(&format!("warm-up: {cmd} (not a gate)"));
            self.warm_step(dir, &cmd, started, cap);
        }
    }

    pub fn merge(&mut self) -> Outcome {
        // The job's own scratch, for the `TMPDIR` of every step of it, gone with the job whatever
        // its outcome (finding FENCE-08).
        let _scratch = self.job_scratch();
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
            self.warm_up(&dir, &base, &head, &rules);
            // The warm-up ran `cargo check`, which can leave files in the worktree that are not in
            // the base — an untracked `Cargo.lock`, when the base had none and cargo wrote one. The
            // `checkout -f` below puts the *tracked* files back but leaves that untracked one, and a
            // branch that adds the same path then makes `git merge` refuse it ("untracked working
            // tree files would be overwritten"): the job returns CONFLICT with no gates run at all,
            // a warm-up skipping the merge it was never meant to gate (finding LA-R056, point 2).
            // Put the worktree back exactly at the base the same way a fresh one is made, and never
            // fall through into the merge if that cannot be done.
            if let Err(e) = self.git(&dir, &["reset", "-q", "--hard", &base]) {
                return fail("LANDING_FAILED", e);
            }
            if let Err(e) = self.git(&dir, &["clean", "-q", "-fd"]) {
                return fail("LANDING_FAILED", e);
            }
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
            // PIPE-09: the plan log always says whether `--full` was set, not only when it changes a gate.
            self.say(if self.spec.full {
                "merge: --full set"
            } else {
                "merge: --full not set"
            });
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
        // The job's own scratch, for the `TMPDIR` of every step of it, gone with the job whatever
        // its outcome (finding FENCE-08).
        let _scratch = self.job_scratch();
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

    /// PIPE-09: the plan log says whether `--full` was set, in both cases.
    #[test]
    fn the_plan_log_says_whether_full_was_set() {
        for full in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            let (_origin, repo) = gate_repo(root);
            git(&repo, &["checkout", "-q", "-b", "topic"]);
            let head = commit(&repo, "b.txt", "b\n", "add b");
            let mut s = spec(root, &repo, "topic", &head);
            s.full = full;
            let dir = root.join("job-full");
            std::fs::create_dir_all(&dir).unwrap();
            let out = Exec::open(&dir, s).unwrap().merge();
            assert_eq!(out.code, "MERGED", "{}", out.message);
            let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
            let (says, not) = if full {
                ("merge: --full set", "merge: --full not set")
            } else {
                ("merge: --full not set", "merge: --full set\n")
            };
            assert!(log.contains(says), "{log}");
            assert!(!log.contains(not), "{log}");
        }
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

    /// The `comm` field of a `/proc/<pid>/stat`: the program name the kernel recorded, between the
    /// first `(` and the last `)`. A killed-but-not-yet-reaped process (a zombie) has an empty
    /// `cmdline`, so this — a run-unique name the survivor is actually `exec`'d from — is the only
    /// way to still see it.
    fn comm_of(stat: &[u8]) -> Option<String> {
        let s = String::from_utf8_lossy(stat);
        let open = s.find('(')?;
        let close = s.rfind(')')?;
        if close <= open {
            return None;
        }
        Some(s[open + 1..close].to_string())
    }

    /// The pids of every process the cap test started: those whose command line carries `marker`
    /// (the argv, never the environment) and those whose `/proc` `comm` equals `comm_tag`. The
    /// second catches a zombie whose `cmdline` has emptied, so this cannot be fooled into "nothing
    /// left" by reading `/proc/*/cmdline` alone. It is `pgrep`/`pkill` never used: the list comes
    /// straight from `/proc`. A pid that vanishes between the listing and the read is skipped. If
    /// `/proc` itself cannot be read this is an error, not an empty list — a scan that silently saw
    /// nothing would make the cap test pass for the wrong reason.
    fn find_marker(marker: &str, comm_tag: &str) -> Result<Vec<u32>, std::io::Error> {
        let mut hits = Vec::new();
        let entries = std::fs::read_dir("/proc")?;
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let mut hit = false;
            if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) {
                hit = String::from_utf8_lossy(&cmdline)
                    .split('\0')
                    .any(|arg| arg.contains(marker));
            }
            if !hit {
                if let Ok(stat) = std::fs::read(format!("/proc/{pid}/stat")) {
                    hit = matches!(comm_of(&stat), Some(c) if c == comm_tag);
                }
            }
            if hit {
                hits.push(pid);
            }
        }
        hits.sort_unstable();
        hits.dedup();
        Ok(hits)
    }

    /// The process state letter from `/proc/<pid>/stat` — the single character right after the
    /// `comm` field — or `None` when it cannot be read (the pid has gone, or raced away between the
    /// listing and the read). `Z` marks a zombie: killed, its exit not yet reaped by a parent.
    fn proc_state(pid: u32) -> Option<char> {
        let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        let s = String::from_utf8_lossy(&stat);
        let close = s.rfind(')')?;
        s[close + 1..].trim_start().chars().next()
    }

    /// The pid a proof file holds. Each warm-up child writes its own pid there the instant it starts,
    /// so a missing file or an unparsable one means the child never ran — and a cap test that never
    /// saw its own child alive would prove nothing. Panics with the reason if the proof is absent.
    fn read_recorded_pid(path: &Path) -> u32 {
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "proof file {} was never written: the warm-up child did not start ({e})",
                path.display()
            )
        });
        raw.trim().parse::<u32>().unwrap_or_else(|e| {
            panic!(
                "proof file {} holds no pid (read {raw:?}): {e}",
                path.display()
            )
        })
    }

    /// The same proof as [`read_recorded_pid`], for the `setsid` straggler, which records its process
    /// group beside its pid: a test of the control-group route means nothing unless it can say that the
    /// child it killed had really left the group the group signal reaches.
    fn read_recorded_pids(path: &Path) -> (u32, u32) {
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "proof file {} was never written: the straggler did not start ({e})",
                path.display()
            )
        });
        let mut f = raw.split_whitespace();
        let (Some(pid), Some(pgid)) = (f.next(), f.next()) else {
            panic!(
                "proof file {} holds no pid and pgid (read {raw:?})",
                path.display()
            );
        };
        let parse = |w: &str, what: &str| {
            w.parse::<u32>().unwrap_or_else(|e| {
                panic!(
                    "proof file {} holds no {what} (read {raw:?}): {e}",
                    path.display()
                )
            })
        };
        (parse(pid, "pid"), parse(pgid, "pgid"))
    }

    /// Whether the cap has ended this run's recorded `pid`: gone from `/proc`, or present only as a
    /// zombie still carrying this run's marker (by `comm`, or by argv before a zombie's command line
    /// empties). A process still running and carrying the marker is the survivor the cap failed to
    /// reap; one running that carries neither is a recycled pid, so our own child has already ended.
    fn pid_ended_by_cap(pid: u32, marker: &str, comm_tag: &str) -> bool {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        let by_argv = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(c) => String::from_utf8_lossy(&c)
                .split('\0')
                .any(|a| a.contains(marker)),
            Err(_) => false,
        };
        let by_comm = matches!(
            std::fs::read(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|st| comm_of(&st)),
            Some(c) if c == comm_tag
        );
        let carries = by_argv || by_comm;
        if proc_state(pid) == Some('Z') {
            return carries;
        }
        !carries
    }

    /// A cleanup guard for one warm-up test: on drop, and so even when an assertion below it fails,
    /// it signals SIGKILL to the processes carrying this run's own unique marker or comm tag — and to
    /// nothing else. The warm-up's cap and its group reap should already have ended them, so on the
    /// passing path there is nothing to reap; this only stops a bug from leaving a survivor alive
    /// after the test. It is the one kill these tests are allowed, aimed only at this test's own
    /// marker/comm pids (`#![forbid(unsafe_code)]` bars the `kill` syscall here, so it goes through
    /// the `kill` program — never `pkill`, never a pattern, never a group).
    struct MarkerReaper {
        marker: String,
        comm_tag: String,
    }

    impl Drop for MarkerReaper {
        fn drop(&mut self) {
            if let Ok(pids) = find_marker(&self.marker, &self.comm_tag) {
                for pid in pids {
                    let _ = Command::new("kill")
                        .args(["-9", "--", &pid.to_string()])
                        .status();
                }
            }
        }
    }

    #[test]
    fn a_warm_up_that_outlives_its_cap_leaves_no_process() {
        let root = tempfile::tempdir().unwrap();
        // `tag` is short enough to survive the kernel's 15-character `comm` limit, so it names the
        // process even as a zombie; `marker` is the longer string put in its command line.
        let uid = uuid::Uuid::new_v4().simple().to_string();
        let tag = format!("lw{}", &uid[..8]);
        let marker = format!("lucina-warmup-marker-{tag}");
        // Reap this run's processes on drop, whatever the assertions do next.
        let _reaper = MarkerReaper {
            marker: marker.clone(),
            comm_tag: tag.clone(),
        };

        // A second copy of bash under a run-unique name: `exec`'d, its `comm` is that name, and it
        // accepts any argv[0] (`exec -a`) — unlike a renamed uutils `sleep`, which dispatches on its
        // own file and refuses to run. It lives outside the worktree, so it is not an untracked file.
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bash = ["/bin/bash", "/usr/bin/bash"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .expect("a bash to copy");
        let survivor = bin.join(&tag);
        std::fs::copy(&bash, &survivor).unwrap();
        // Proof files outside the worktree, one per child: each writes its own pid the instant it
        // starts. So the test first shows the SIGTERM-ignoring child really existed — a cap test that
        // never saw its own child alive would prove nothing — then that the cap ended it.
        let surv_proof = root.path().join("cap-survivor-pid");
        let fg_proof = root.path().join("cap-foreground-pid");

        // A fake cargo that, on `check`, forks the survivor in the background — it IGNORES SIGTERM
        // (`trap '' TERM`), which is exactly the process uutils `timeout`'s group SIGTERM cannot end
        // and its `--kill-after` SIGKILL does not reach (it only signals the direct child) — and then
        // blocks in a foreground `sleep` that also carries the marker. Each records its own pid in its
        // proof file before it blocks, so the test can prove the child started before demanding the
        // cap ended it. So only a warm-up that ends its whole process group stops the survivor. Every
        // other subcommand (the workspace's `cargo metadata`) is a plain success.
        let shadow = format!(
            "#!/bin/bash\nif [ \"$1\" = check ]; then\n  ( exec -a {marker} '{surv}' -c 'trap \"\" TERM; echo $$ > {survp}; while :; do sleep 300; done' ) &\n  echo $$ > {fgp}\n  exec -a {marker} sleep 300\nfi\nexit 0\n",
            surv = survivor.display(),
            survp = surv_proof.display(),
            fgp = fg_proof.display()
        );
        let (origin, repo) = main_with_cargo(root.path(), &warm_rules("check"));
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
        // Each child proved itself alive by writing its own pid before the cap fired; the cap must
        // then have ended both — the foreground sleep and, crucially, the SIGTERM-ignoring survivor
        // that a group SIGTERM alone cannot stop. "Ended" means the pid is gone, or a zombie still
        // carrying this run's marker; a killed child lingers a moment before it is reaped, so wait.
        // Without the group reap the survivor is still running here, and this is the assertion that
        // fails — the point the previous form of this test could not make.
        for (what, proof) in [("survivor", &surv_proof), ("foreground", &fg_proof)] {
            let pid = read_recorded_pid(proof);
            let until = Instant::now() + std::time::Duration::from_secs(5);
            while Instant::now() < until && !pid_ended_by_cap(pid, &marker, &tag) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(
                pid_ended_by_cap(pid, &marker, &tag),
                "the warm-up cap left its {what} child (pid {pid}) alive\nLOG:\n{log}"
            );
        }
        // And it left nothing behind: no process — running, or killed-but-not-yet-reaped (a zombie,
        // whose empty cmdline a `/proc/*/cmdline` scan would miss) — carries this run's marker or
        // comm tag. A SIGTERM-ignoring grandchild is exactly what the group reap must clear; if the
        // cap only reached the direct child, it is still running here and this fails. A correctly
        // killed one lingers only as a brief zombie, so wait a moment for it to be reaped first.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let leftover = loop {
            let hits = find_marker(&marker, &tag)
                .expect("/proc could not be read: the cap test cannot prove anything");
            if hits.is_empty() || Instant::now() >= deadline {
                break hits;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(
            leftover.is_empty(),
            "the warm-up left marker/tag processes {leftover:?}\nLOG:\n{log}"
        );
    }

    #[test]
    fn a_file_the_warm_up_leaves_behind_does_not_skip_the_gates() {
        let root = tempfile::tempdir().unwrap();
        // A fake cargo that, on `check`, writes an untracked `Cargo.lock` into its working directory
        // (the landing worktree at the base) — the thing `cargo check` really does when the base has
        // no lock — and then records, in a proof file outside the worktree, the exact path it wrote.
        // So the test can show the leftover was really produced: without it the merge would have
        // nothing to trip over and would land with or without the reset/clean. Every other subcommand
        // is a plain success.
        let proof = root.path().join("warmup-wrote-lock");
        let shadow = format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then\n  printf 'left by the warm-up\\n' > Cargo.lock\n  printf '%s\\n' \"$(pwd)/Cargo.lock\" > {proof_path}\nfi\nexit 0\n",
            proof_path = proof.display()
        );
        // The base (main) has a root Cargo.toml and NO Cargo.lock; the branch ADDS a tracked one.
        let (origin, repo) = main_with_cargo(root.path(), &warm_rules("check"));
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("cargo"), &shadow);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "Cargo.lock", "the branch's lock\n", "add lock");
        // The base the merge runs on has no Cargo.lock (main_with_cargo writes only a Cargo.toml);
        // it is the branch that adds one, so the warm-up's untracked lock is the one that must go.
        let base = git(&origin, &["rev-parse", "main"]);
        let base_has_lock = Command::new("git")
            .arg("-C")
            .arg(&origin)
            .args(["cat-file", "-e", &format!("{base}:Cargo.lock")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(!base_has_lock, "the base must have no Cargo.lock");
        let dir = root.path().join("job-warmup-leftover");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root.path(), &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();

        // First the setup the whole test depends on: the warm-up really did write the untracked lock,
        // and wrote it inside the landing worktree. Had it not, `git merge` would have had no leftover
        // to refuse and the test would pass whether or not the reset/clean fix is present.
        let written = std::fs::read_to_string(&proof).unwrap_or_else(|e| {
            panic!(
                "the warm-up never wrote the proof file {}: {e}",
                proof.display()
            )
        });
        let written = written.trim();
        assert!(
            written.ends_with("/wt/.landing/Cargo.lock"),
            "the warm-up wrote {written:?}, not a Cargo.lock inside the landing worktree"
        );
        // The warm-up ran, and its leftover did not stop the merge or skip the gates: the branch's
        // lock landed and the declared gate ran. (Without the reset/clean before `git merge`, the
        // untracked Cargo.lock makes the merge abort with CONFLICT and no gates.)
        assert!(
            log.contains("warm-up: cargo check of the merge base"),
            "{log}"
        );
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{out:?}\nLOG:\n{log}");
        // The gates ran. The branch touches `Cargo.lock`, so `plan_with` adds a workspace check next
        // to the declared `trivial` gate; what matters is that the list is not empty (before the fix
        // the merge aborts with CONFLICT and no gates at all) and every gate passed.
        assert!(!out.gates.is_empty(), "the gates were skipped: {log}");
        assert!(
            out.gates.iter().any(|g| g.name == "trivial" && g.ok),
            "{log}"
        );
        assert!(out.gates.iter().all(|g| g.ok), "{log}");
        let landed = out.landed.unwrap();
        assert_eq!(git(&origin, &["rev-parse", "main"]), landed, "{log}");
        // What landed is the branch's Cargo.lock, not the warm-up's leftover.
        assert_eq!(
            git(&origin, &["show", &format!("{landed}:Cargo.lock")]),
            "the branch's lock",
            "{log}"
        );
    }

    // ── LA-C024: a step is ended whole, at its deadline and after it ─────────────────────
    //
    // Three shapes of the one thing that went wrong (finding GATE-05): a step whose grandchild
    // ignores SIGTERM, which the limit's final blow never reached because it only ever went to the
    // process the limit waited on. By the process group (`group`), out of the group with `setsid`
    // (`setsid`), and holding the shared `CARGO_TARGET_DIR`'s lock while it does it (`lock`). They
    // are red against a runner that signals the direct child only, and green against the two routes
    // of [`super::kill`].

    /// A copy of bash under a name of this run alone, and the `argv[0]` marker that goes with it:
    /// how a test below finds its own survivor again ([`find_marker`] matches the `comm` name or the
    /// argv) without `pgrep`, without a pattern, and without touching a process that is not its own.
    fn survivor_names(root: &Path) -> (String, String, PathBuf) {
        let uid = uuid::Uuid::new_v4().simple().to_string();
        // 14 characters: `comm` is cut at 15, and the name is what survives an empty `cmdline`.
        let tag = format!("lg{}", &uid[..12]);
        let marker = format!("lucina-gate-marker-{tag}");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bash = ["/bin/bash", "/usr/bin/bash"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .expect("a bash to copy");
        let copy = bin.join(&tag);
        std::fs::copy(&bash, &copy).unwrap();
        (tag, marker, copy)
    }

    /// The gate command of a straggler test: start a process that writes its own pid the instant it
    /// is alive — atomically, so the reader never sees half a pid — and then ignores SIGTERM for
    /// five minutes, and end the step as soon as that proof is there. So the step that *passed* its
    /// gate is a step that left a process behind, which is the step that must not leave one.
    /// `kind` is `group` (a plain background job), `setsid` (out of the step's process group, which
    /// is what leaves only the control group as a route to it) or `lock` (an exclusive `flock` on a
    /// file in the step's `CARGO_TARGET_DIR`, held for the same five minutes).
    ///
    /// `setsid` records two more things, because a test that only knows the child is gone cannot tell
    /// whether it ever watched the right process: the child writes its own process group beside its
    /// pid, and the step writes its group to `<proof>-step`. Those two numbers differing is the proof
    /// that the child really left the group, which is the whole premise of the control-group route.
    fn straggler_gate_script(kind: &str, copy: &Path, marker: &str, proof: &Path) -> String {
        let proof = proof.display();
        let copy = copy.display();
        let take = if kind == "lock" {
            "flock -x 9 || exit 1; "
        } else {
            ""
        };
        // `pid (comm) state ppid pgid …`, and `comm` may hold `)` of its own: cut from the last one.
        // Double quotes across all of the inner text, which is handed over inside one pair of single ones:
        // a nested `'` does not quote what it looks like it quotes — it closes the outer pair, and the
        // `)` of the `sed` script lands where the shell reads it as a token (the step then dies at exit 2
        // before any process is left behind, which is what a lane never sees because it skips this test).
        let own_pgid = "sed \"s/^.*) //\" /proc/self/stat | cut -d\" \" -f3";
        let left_the_group = if kind == "setsid" {
            format!(
                "pgid=$({own_pgid}); [ -n \"$pgid\" ] || {{ echo \"no pgid for the straggler\" >&2; exit 9; }}; \
                 printf \"%s %s\\n\" \"$$\" \"$pgid\" > \"{proof}.tmp\" && mv -f \"{proof}.tmp\" \"{proof}\""
            )
        } else {
            format!("printf \"%s\\n\" $$ > \"{proof}.tmp\" && mv -f \"{proof}.tmp\" \"{proof}\"")
        };
        let inner = format!("trap \"\" TERM; {take}{left_the_group}; while :; do sleep 300; done");
        let spawn = match kind {
            // `setsid`: the child takes a new session and a new process group, so the group signal
            // is not a route to it at all. The marker is its `argv[0]`. Backgrounded, like the
            // others: a step that *waits* for the thing it is only supposed to leave behind never
            // ends, and the deadline is not what this is about.
            "setsid" => format!("setsid '{copy}' -c '{inner}' {marker} &"),
            "lock" => format!("( exec 9>\"$LOCK\"; exec -a {marker} '{copy}' -c '{inner}' ) &"),
            _ => format!("( exec -a {marker} '{copy}' -c '{inner}' ) &"),
        };
        let head = if kind == "lock" {
            "test -n \"$CARGO_TARGET_DIR\" || { echo 'the step was given no CARGO_TARGET_DIR' >&2; exit 9; }\nmkdir -p \"$CARGO_TARGET_DIR\" || exit 9\nLOCK=\"$CARGO_TARGET_DIR/lucina-c024-gate.lock\"\n"
        } else if kind == "setsid" {
            // The step's own group, before anything is left behind: the leader of the group the
            // signal is aimed at is this shell, and the number is what the test compares.
            "pgid=$(sed 's/^.*) //' /proc/self/stat | cut -d' ' -f3)\n[ -n \"$pgid\" ] || { echo 'no pgid for the step' >&2; exit 9; }\nprintf '%s\\n' \"$pgid\" > 'STEP-PGID'\n"
        } else {
            ""
        };
        format!(
            "#!/bin/bash\n{head}{spawn}\nfor _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do [ -e '{proof}' ] && exit 0; sleep 0.1; done\necho 'the straggler never recorded its own pid' >&2\nexit 9\n"
        )
            .replace("STEP-PGID", &format!("{proof}-step"))
    }

    /// A trunk whose only gate is `run`, and a topic branch to merge onto it.
    fn one_gate_repo(root: &Path, run: &str) -> (PathBuf, PathBuf, String) {
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            format!("[merge]\ndiffgate = false\n[[gate]]\nname = \"leave\"\nrun = \"{run}\"\n"),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        (_origin, repo, head)
    }

    /// That no process of this run is left: gone from `/proc`, or a zombie on its way out (a killed
    /// process is not reaped by the signal that killed it, so this waits for that a moment). An
    /// unreadable `/proc` is the error it is: a scan that saw nothing would make the test pass for
    /// the wrong reason.
    fn assert_leaves_nothing(marker: &str, tag: &str, log: &str) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let leftover = loop {
            let hits =
                find_marker(marker, tag).expect("/proc cannot be read: the test proves nothing");
            if hits.is_empty() || Instant::now() >= deadline {
                break hits;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(
            leftover.is_empty(),
            "the step left marker/tag processes {leftover:?}\nLOG:\n{log}"
        );
    }

    /// The line a step says when it gets no scope, or `None` when it got one: how the `setsid` test
    /// below knows whether it just watched the control group do its work or a host with no user
    /// manager at all.
    fn scope_fallback_line(log: &str) -> Option<String> {
        log.lines().find_map(|l| {
            l.contains("scope: none (").then(|| {
                l.split_once("] ")
                    .map(|(_, r)| r.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
        })
    }

    /// Whether the job's scope probe actually ran and was refused by the lane's shim: the probe's
    /// exit status is 79, which is the code every refusing wrapper in `/scratch/offload/lane-shim`
    /// uses (HOST-08). A probe that never ran — no `systemd-run` on the job's PATH, no
    /// `XDG_RUNTIME_DIR`, no bash — has no exit status and this returns false. A probe that exited
    /// for any other reason also returns false: it was not a lane refusal, and the test must fail
    /// saying what happened, not skip silently. This is the condition under which the setsid test
    /// below may skip: only when the shim actually refused the very command the step would get.
    fn lane_refused_the_scope_probe(log: &str) -> bool {
        log.lines().any(|l| {
            l.contains("scope: none (the lucina-gate-probe-") && l.contains(" probe exited 79)")
        })
    }

    #[test]
    fn a_gate_step_leaves_no_grandchild_that_ignores_sigterm() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (tag, marker, copy) = survivor_names(root);
        let _reaper = MarkerReaper {
            marker: marker.clone(),
            comm_tag: tag.clone(),
        };
        let proof = root.join("straggler-pid");
        let script = root.join("bin").join("leave-a-straggler");
        write_exec(
            &script,
            &straggler_gate_script("group", &copy, &marker, &proof),
        );
        let (origin, repo, head) = one_gate_repo(root, &script.display().to_string());
        let dir = root.join("job-gate-straggler");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        // The grace between the first blow and the killing is 30 s on a real gate, as
        // `--kill-after=30s` was; here it is the thing under test, not something to wait for.
        x.kill_grace = std::time::Duration::from_millis(250);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();

        // The merge landed and its gate passed — and the gate passed *because* the survivor had
        // started and recorded itself, which the script demands before it exits 0.
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        assert!(out.gates.iter().all(|g| g.ok), "{log}");
        // Then: the step is over, so the process it left behind is not running. It is the direct
        // child's own SIGTERM-ignoring sibling of the warm-up's, in the step's process group, and a
        // limit that only ever reached the direct child leaves exactly this one alive.
        let pid = read_recorded_pid(&proof);
        assert!(
            pid_ended_by_cap(pid, &marker, &tag),
            "the step left its SIGTERM-ignoring grandchild (pid {pid}) alive\nLOG:\n{log}"
        );
        assert_leaves_nothing(&marker, &tag, &log);
        // Nothing was unreachable: a kill that was refused says so in the log.
        assert!(!log.contains("refused the signal"), "{log}");
        assert!(!log.contains("when the killing gave up"), "{log}");
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
    }

    /// The control-group route: a step that stands in a transient systemd scope of its own reaches a child
    /// that `setsid` away from its process group, and a step without that scope reaches nothing of it. This
    /// is therefore a test of the host's `systemd-run`, and **it cannot run inside a lane run**: the lane's
    /// shim for that program refuses the request by design (HOST-08), and getting at the real one from here
    /// would mean editing PATH, which a worker must never do (FENCE-09). The test skips only when the job's
    /// scope probe actually ran and was refused by that shim — the probe's exit status is 79, the code every
    /// refusing wrapper uses. Any other reason the step got no scope makes the test fail, not skip: a host
    /// with no user manager at all, or a probe that died for an unrelated reason, is not a lane refusal and
    /// must not pass silently. The merge gate — a `lagent-job` unit, with the host's own `systemd-run` on its
    /// PATH — is where this proves anything at all.
    #[test]
    fn a_gate_step_leaves_no_grandchild_that_left_its_process_group() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (tag, marker, copy) = survivor_names(root);
        let _reaper = MarkerReaper {
            marker: marker.clone(),
            comm_tag: tag.clone(),
        };
        let proof = root.join("setsid-pid");
        let script = root.join("bin").join("leave-a-setsid-straggler");
        write_exec(
            &script,
            &straggler_gate_script("setsid", &copy, &marker, &proof),
        );
        let (origin, repo, head) = one_gate_repo(root, &script.display().to_string());
        let dir = root.join("job-gate-setsid");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.kill_grace = std::time::Duration::from_millis(250);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        // The only reason this test may skip is a real refusal: the probe ran, hit the lane's shim, and
        // exited 79. Any other no-scope answer — no manager at all, a probe that died, a PATH without
        // systemd-run — makes the test fail below with the probe's own line.
        if lane_refused_the_scope_probe(&log) {
            println!("SKIPPED: needs the real systemd-run; lane workers are refused it by design (HOST-08); this test runs at the merge gate");
            return;
        }
        // A scope that was not had is not a host to blame here: outside the lane's shim this process has a
        // user manager, and a step that got no unit is the code failing to ask for one. Say what the probe
        // answered, and fail — an early return on this path was the hole the review found.
        if let Some(line) = scope_fallback_line(&log) {
            panic!("the step got no scope, so the control-group route was never taken: {line}\nLOG:\n{log}");
        }
        // The step stands in a unit of its own, and it is the seeing of that unit that makes the route real:
        // a scope only named, whose control group was never read from anything, sweeps the group alone and
        // leaves exactly the child this test started.
        let sight = log
            .lines()
            .find(|l| l.contains("scope: lucina-gate-"))
            .unwrap_or_else(|| panic!("the step neither got a scope nor said why not:\n{log}"));
        assert!(
            !sight.contains("was never seen"),
            "the scope was asked for and never seen, so a child outside the process group was out of reach: {sight}\nLOG:\n{log}"
        );
        // And that child was the one the control group reached: its recorded process group is not the step's
        // own, which is what puts it outside every group signal this job can send. Read from the proof the
        // two wrote while they were alive — a test that only knows the pid is gone proves nothing about the
        // route that removed it.
        let (pid, pgid) = read_recorded_pids(&proof);
        let step_group = read_recorded_pid(&root.join("setsid-pid-step"));
        assert_ne!(
            pgid, step_group,
            "the straggler never left the step's process group (child pgid {pgid}, step pgid {step_group}), \
             so it is the group signal and not the control group that this watched\nLOG:\n{log}"
        );
        assert!(
            pid_ended_by_cap(pid, &marker, &tag),
            "the step left its `setsid` grandchild (pid {pid}) alive: outside the process group, \
             only the step's own scope control group reaches it\nLOG:\n{log}"
        );
        assert_leaves_nothing(&marker, &tag, &log);
        assert!(!log.contains("refused the signal"), "{log}");
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
    }

    /// The log of a job whose `systemd-run` is a stand-in that exits `code` at once: the scope probe runs
    /// against it, and what it answered is what the job says about the scope.
    fn log_of_a_job_whose_systemd_run_exits(code: i32) -> String {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("systemd-run"),
            &format!("#!/bin/sh\nexit {code}\n"),
        );
        let gate = bin.join("says-hello");
        write_exec(&gate, "#!/bin/sh\necho 'hello from the step'\n");
        let (_origin, repo, head) = one_gate_repo(root, &gate.display().to_string());
        let dir = root.join("job-skip-condition");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        let path = x.path.clone();
        let runtime = root.display().to_string();
        x.source_env = crate::testutil::env_source(&[
            ("PATH", &path),
            ("HOME", "/home/fake"),
            ("XDG_RUNTIME_DIR", runtime.as_str()),
        ]);
        let _ = x.merge();
        std::fs::read_to_string(dir.join("log")).unwrap_or_default()
    }

    /// The setsid test's skip condition: only a probe that ran and was refused by the lane's shim
    /// (exit 79) may skip. A probe that exited for any other reason must not, because the test would
    /// then pass without watching the control-group route it claims to prove.
    #[test]
    fn lane_refused_the_scope_probe_keys_on_exit_79_and_nothing_else() {
        let refused = log_of_a_job_whose_systemd_run_exits(79);
        assert!(
            lane_refused_the_scope_probe(&refused),
            "a probe that exited 79 is the shim's refusal and must skip:\n{refused}"
        );
        let other = log_of_a_job_whose_systemd_run_exits(1);
        assert!(
            !lane_refused_the_scope_probe(&other),
            "a probe that exited 1 is not a lane refusal and must not skip:\n{other}"
        );
        assert!(
            scope_fallback_line(&other).is_some(),
            "the setsid test would fail on this log, as it must:\n{other}"
        );
        assert!(!lane_refused_the_scope_probe("no probe line at all"));
    }

    #[test]
    fn a_gate_step_leaves_the_lock_of_its_cargo_target_dir_free() {
        // GATE-05 as the owner saw it: not a process alive for its own sake, but a step that had
        // been reported and was still holding the shared target dir, so the *next* gate sat behind
        // a lock nobody could see. The lock here is an explicit `flock` on a file in
        // `CARGO_TARGET_DIR`, which is what cargo's own lock is — the same one lock on the same
        // directory, taken by a grandchild that ignores SIGTERM.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (tag, marker, copy) = survivor_names(root);
        let _reaper = MarkerReaper {
            marker: marker.clone(),
            comm_tag: tag.clone(),
        };
        let proof = root.join("lock-holder-pid");
        let script = root.join("bin").join("hold-the-target-lock");
        write_exec(
            &script,
            &straggler_gate_script("lock", &copy, &marker, &proof),
        );
        let (origin, repo, head) = one_gate_repo(root, &script.display().to_string());
        let dir = root.join("job-gate-lock");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.kill_grace = std::time::Duration::from_millis(250);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        let lock = root.join("target").join("lucina-c024-gate.lock");
        // The proof file is written *after* `flock -x 9` is granted, so the survivor really did
        // hold the lock: without that, the free lock below would mean nothing.
        let pid = read_recorded_pid(&proof);
        // The step is over. A non-blocking `flock` of the very same file must be granted now — that
        // is the next gate getting its turn. While the survivor lives, this is refused, and this is
        // the assertion a runner that only signals the direct child fails.
        let got = Command::new("flock")
            .args(["-n", &lock.display().to_string(), "/bin/true"])
            .status()
            .expect("flock runs");
        assert!(
            got.success(),
            "the lock {} was still held after the step ended (its holder was pid {pid})\nLOG:\n{log}",
            lock.display()
        );
        assert!(
            pid_ended_by_cap(pid, &marker, &tag),
            "the lock holder (pid {pid}) is still there\nLOG:\n{log}"
        );
        assert_leaves_nothing(&marker, &tag, &log);
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
    }

    #[test]
    fn a_step_that_ends_inside_its_limit_is_the_step_it_always_was() {
        // The other half of the change: what a step that ends by itself always gave the job — the
        // `$ <command>` line, its stdout and its stderr in the job log, `failed: exit <n>`, the gate
        // outcome — is still exactly what it gives. This is the test that keeps the three above from
        // being about the killing only.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let pass = bin.join("says-hello");
        write_exec(&pass, "#!/bin/sh\necho 'hello from the step'\nexit 0\n");
        let (_origin, repo, head) = one_gate_repo(root, &pass.display().to_string());
        let dir = root.join("job-step-passes");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        assert_eq!(out.gates.len(), 1, "{log}");
        assert!(out.gates[0].ok, "{log}");
        assert!(log.contains("gate: leave"), "{log}");
        assert!(log.contains(&format!("$ {}", pass.display())), "{log}");
        assert!(log.contains("hello from the step"), "{log}");
        assert!(!log.contains("failed:"), "{log}");
        assert!(!log.contains("cannot run"), "{log}");
        assert!(!log.contains("timed out"), "{log}");

        // A step that fails inside its limit: the reason is its exit code, the gate is the failure,
        // and the job says both.
        let root2 = tempfile::tempdir().unwrap();
        let root2 = root2.path();
        let bin2 = root2.join("bin");
        std::fs::create_dir_all(&bin2).unwrap();
        let fail = bin2.join("says-no");
        write_exec(&fail, "#!/bin/sh\necho 'bad from the step' >&2\nexit 3\n");
        let (_origin2, repo2, head2) = one_gate_repo(root2, &fail.display().to_string());
        let dir2 = root2.join("job-step-fails");
        std::fs::create_dir_all(&dir2).unwrap();
        let mut x = Exec::open(&dir2, spec(root2, &repo2, "topic", &head2)).unwrap();
        let out = x.merge();
        let log = std::fs::read_to_string(dir2.join("log")).unwrap_or_default();
        assert!(!out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "GATE_FAILED", "{log}");
        assert_eq!(out.gates.len(), 1, "{log}");
        assert!(!out.gates[0].ok, "{log}");
        assert!(out.message.contains("leave"), "{}", out.message);
        assert!(log.contains("bad from the step"), "{log}");
        assert!(log.contains("failed: exit 3"), "{log}");
        assert!(!log.contains("cannot run"), "{log}");
        assert!(!log.contains("timed out"), "{log}");
        assert!(!log.contains("refused the signal"), "{log}");
        assert!(!log.contains("when the killing gave up"), "{log}");
    }

    /// LA-R059, item 6, as a decision and not an accident: `timeout(1)` returned 124 for its own deadline, so a
    /// command that exits 124 by itself used to read as a timeout. This runner's deadline says `timed out after
    /// <N> min` and an exit says its number, whichever number that is.
    #[test]
    fn a_gate_step_that_exits_124_reports_an_exit_not_a_timeout() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exit124 = bin.join("exits-124");
        write_exec(
            &exit124,
            "#!/bin/sh\necho 'the step leaves of itself' >&2\nexit 124\n",
        );
        let (_origin, repo, head) = one_gate_repo(root, &exit124.display().to_string());
        let dir = root.join("job-step-124");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(!out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "GATE_FAILED", "{log}");
        assert!(log.contains("failed: exit 124"), "{log}");
        assert!(!log.contains("timed out"), "{log}");
    }

    /// LA-R059, item 2, from the inside: the step's command line, both routes, as [`Exec::step_argv`] builds
    /// it. `bash` `-c` `<cmd>` is what either route ends up starting — the program and its arguments on the plain
    /// one, the last three arguments of `systemd-run` on the scope one — so `$0` in the gate's own text is `bash`,
    /// what `timeout … bash -c` gave and a script file of the step's own took away. The scope route carries the
    /// one flag that makes such a file unnecessary in front of it.
    #[test]
    fn the_step_is_handed_bash_dash_c_on_both_routes() {
        let bash = Path::new("/bin/bash");
        let run = Path::new("/usr/bin/systemd-run");
        let cmd = "test -n \"${LAGENT_WAIVERS_FILE+x}\" && printf '%s\\n' \"$0\"";
        // The step: bash at the path it was resolved to, the command, and the name for `$0`.
        let want_step = vec![
            std::ffi::OsString::from("/bin/bash"),
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from(cmd),
            std::ffi::OsString::from("bash"),
        ];
        // The command line as the process sees it: the program `Exec::sh_run` starts, then what follows it.
        let whole = |program: &Path, argv: &[std::ffi::OsString]| {
            let mut line = vec![std::ffi::OsString::from(program)];
            line.extend_from_slice(argv);
            line
        };

        let (program, argv) = Exec::step_argv(bash, None, None, cmd);
        assert_eq!(
            program, bash,
            "the step without a scope runs the bash this job resolved"
        );
        assert_eq!(
            whole(&program, &argv),
            want_step,
            "the step without a scope is `bash -c <cmd> bash`, so its `$0` is bash"
        );

        let (program, argv) = Exec::step_argv(bash, Some(run), None, cmd);
        assert_eq!(
            program, bash,
            "a host with the program but no unit asked for is the plain route still"
        );
        assert_eq!(
            whole(&program, &argv),
            want_step,
            "and the plain route is unchanged"
        );

        let (program, argv) = Exec::step_argv(bash, Some(run), Some("lucina-gate-job-1"), cmd);
        assert_eq!(program, run, "the scope route starts at systemd-run");
        assert_eq!(
            argv[argv.len() - 4..],
            want_step[..],
            "the scope route hands the same `bash -c <cmd> bash` to systemd-run, so `$0` is bash there too: {argv:?}"
        );
        let flags: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            flags.contains(&"--expand-environment=no".to_string()),
            "systemd-run expands the braced dollar-brace form of a payload argv it is handed unless this says otherwise, \
             which is the only reason the step had a file of its own: {flags:?}"
        );
        let slice_at = flags
            .iter()
            .position(|f| f == "--slice=build.slice")
            .expect("the scope route places its unit in build.slice (HOST-11)");
        let dashes_at = flags
            .iter()
            .position(|f| f == "--")
            .expect("the scope route separates its options from the payload");
        assert!(
            slice_at < dashes_at,
            "the slice is an option of systemd-run, so it comes before the payload: {flags:?}"
        );
        let (_, plain) = Exec::step_argv(bash, None, None, cmd);
        assert!(
            !plain
                .iter()
                .any(|a| a.to_string_lossy().starts_with("--slice")),
            "the process-group route has no scope and so no slice: {plain:?}"
        );
        let unit_at = flags
            .iter()
            .position(|f| f == "--unit")
            .expect("the scope route names its unit");
        assert_eq!(
            flags[unit_at + 1],
            "lucina-gate-job-1",
            "the unit is the one this step made"
        );
    }

    /// LA-R059, item 2, from the outside: a gate that asks its own shell what it was called, and gets `bash`.
    /// This is the route the job takes on a host without a user manager; [`the_step_is_handed_bash_dash_c_on_both_routes`]
    /// pins the other one, where `systemd-run` stands in front, because the lane cannot make a scope
    /// (HOST-08) and a worker must not reach around the shim that refuses it (FENCE-09).
    #[test]
    fn a_gate_command_sees_bash_as_its_argv_zero() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        // The gate is shell text, not the path of a script: `bash -c /some/thing` is a single simple command,
        // and bash answers that by replacing itself with the thing, whose `$0` is its own path and says
        // nothing about how it was reached. The `;` is what keeps bash running the printf itself, and the
        // `\"` is for the TOML string the rule is written in.
        let gate = r#"printf %s \"$0\" > argv0.txt; : the second statement keeps bash doing the printf itself"#;
        let (_origin, repo, head) = one_gate_repo(root, gate);
        let dir = root.join("job-argv-zero");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        let seen = std::fs::read_to_string(root.join("wt/.landing/argv0.txt"))
            .unwrap_or_else(|e| panic!("the step wrote no argv0.txt: {e}\nLOG:\n{log}"));
        assert_eq!(
            seen.trim(),
            "bash",
            "the step's `$0` is what it was handed as, and a gate that reads `dirname \"$0\"` acts on that"
        );
        // And no step script is left in the job directory, because there is no such file to leave.
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("step-") && n.ends_with(".sh"))
            .collect();
        assert!(left.is_empty(), "the job kept a step script {left:?}");
    }

    /// The scope question has to be asked with the command line the answer is about. A probe that is
    /// narrower than a step answers yes on a host that will refuse every step of the job: the lane's
    /// shim for `systemd-run` allows `--user --scope --quiet --collect --unit` and refuses any option it
    /// does not list, so `--expand-environment=no` — which a step carries since item 2 and the probe did
    /// not — left this crate's own tests with 23 gate steps exiting 79 (finding PROBE-01). The stand-in
    /// here is the shim's rule: an option it does not know is a refusal.
    #[test]
    fn the_scope_probe_is_asked_with_the_argv_a_step_gets() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let calls = root.join("systemd-run-calls");
        let run = bin.join("systemd-run");
        write_exec(
            &run,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nfor a in \"$@\"; do case \"$a\" in --expand-environment*) exit 79;; esac; done\nexit 0\n",
                calls.display()
            ),
        );
        let gate = bin.join("says-hello");
        write_exec(&gate, "#!/bin/sh\necho 'hello from the step'\n");
        let (_origin, repo, head) = one_gate_repo(root, &gate.display().to_string());
        let dir = root.join("job-probe-argv");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        let path = x.path.clone();
        let runtime = root.display().to_string();
        // A manager to ask is the one thing the probe looks for before it forks, and the stand-in needs
        // no real bus behind the name.
        x.source_env = crate::testutil::env_source(&[
            ("PATH", &path),
            ("HOME", "/home/fake"),
            ("XDG_RUNTIME_DIR", runtime.as_str()),
        ]);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        let asked = std::fs::read_to_string(&calls).unwrap_or_else(|e| {
            panic!("the job never asked the host about a scope: {e}\nLOG:\n{log}")
        });
        assert!(
            asked.contains("--expand-environment=no"),
            "the probe was asked without the option a step is handed, so a host that refuses it says \
             yes to the job and no to every gate: {asked}"
        );
        assert!(
            asked.contains("--slice=build.slice"),
            "the probe is asked with the argv a step gets, slice included: {asked}"
        );
        assert!(
            asked.contains("bash -c /bin/true"),
            "and it is asked through the same payload form, so `$0` and the expansion of the text mean \
             the same thing in the answer and in the step: {asked}"
        );
        assert_eq!(
            asked.lines().count(),
            1,
            "one question per job, whatever the answer: {asked}"
        );
        // The refusal is the fallback, not the failure: the gate runs as a plain step and the merge lands.
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        assert!(log.contains("hello from the step"), "{log}");
        let line = log
            .lines()
            .find(|l| l.contains("scope: none (the lucina-gate-probe-"))
            .unwrap_or_else(|| panic!("the probe's refusal never reached the log:\n{log}"));
        assert!(line.contains("probe exited 79)"), "{line}");
        assert!(!log.contains("was never seen"), "{log}");
    }

    #[test]
    fn a_gate_step_writes_its_temporary_files_outside_tmp() {
        // FENCE-08: everything here runs with `TMPDIR=/tmp`, and the host sandbox that
        // `lucina-agents`' own test checks for itself refuses a file under it — so a merge of
        // anything touching that crate failed on a gate that had done nothing wrong. A step whose
        // `TMPDIR` is not usable gets the job's own scratch, which is gone when the job is.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("where-is-tmpdir");
        write_exec(
            &script,
            "#!/bin/bash\nprintf %s \"$TMPDIR\" > tmpdir-seen.txt\nmkdir -p \"$TMPDIR/sub\" || { echo \"cannot make a directory in $TMPDIR\" >&2; exit 9; }\nprintf 'written\\n' > \"$TMPDIR/sub/written.txt\" || exit 9\ntest -s \"$TMPDIR/sub/written.txt\"\n",
        );
        let (origin, repo, head) = one_gate_repo(root, &script.display().to_string());
        // LA-R059, item 5: the job directory of a real run is `<state>/pipeline/jobs/<id>`, which is on
        // the owner's home and in neither of the machine-wide temporary directories. The scratch hangs
        // from it, so a job directory built from `tempfile::tempdir()` — under `/tmp` on the machine's
        // own default, as `TMPDIR` says — proves the equality below and nothing about where the files
        // land: the scratch of such a job is itself somewhere under `/tmp/<random>/job-tmpdir/tmp`.
        // Built out of the scratch tree the owner's rules name, by name, and removed with the test.
        let home = PathBuf::from(format!(
            "/home/nez/scratch/tmp/lucina-agentd-tests/a-gate-step-tmpdir-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let home = std::fs::canonicalize(&home).unwrap();
        let dir = home.join("job");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        // The failure condition, as this host gives it: an inherited `TMPDIR` under `/tmp`.
        x.source_env = crate::testutil::env_source(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/fake"),
            ("TMPDIR", "/tmp"),
        ]);
        let out = x.merge();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        // The step passed, which it can only do if it could make a directory and write a file in
        // the `TMPDIR` it was given.
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        let seen = std::fs::read_to_string(root.join("wt/.landing/tmpdir-seen.txt")).unwrap();
        let seen = Path::new(seen.trim());
        // Pinned exactly, which is the contract: the scratch is `<job dir>/tmp`, and the step wrote
        // its file inside it (the script exits 9 when it cannot).
        let want = std::fs::canonicalize(&dir).unwrap().join("tmp");
        assert_eq!(seen, want.as_path(), "{log}");
        assert!(
            !seen.exists(),
            "the job left its scratch {} behind\nLOG:\n{log}",
            seen.display()
        );
        // And where that puts it, which is the half the equality left unshown: the scratch is inside
        // the job directory, that directory is under the owner's home and so in neither of the
        // machine-wide temporary directories, and it is not under the directory this very process
        // uses for its own temporaries — which on this host is the first of them, and is what let the
        // old version of this test pass with a job directory built out of it.
        assert!(
            seen.starts_with(home.join("job")),
            "the scratch {seen:?} is not inside the job directory {}",
            home.join("job").display()
        );
        assert!(
            seen.starts_with("/home/"),
            "the scratch {seen:?} is not under the owner's home, so the job directory it hangs from \
             is not the one a real run gets"
        );
        // Neither of the machine-wide temporary directories, in the guard's own words: asked of
        // [`Exec::tmpdir_is_unusable`], which is the decision this whole finding turned on, rather than
        // paraphrased here a second time — a copy of a rule in the test of that rule is the half that
        // drifts from it. Not `std::env::temp_dir()` either: on this host that is the scratch tree the
        // owner's rules send this test's own job directory to, so it would refuse the very layout asked
        // for and let the old vacuous one through. `tempfile::tempdir()` follows whatever `TMPDIR` says,
        // and where this runs on the machine's default it put the job directory, and so the scratch,
        // under it.
        assert!(
            !Exec::tmpdir_is_unusable(Some(seen.as_os_str())),
            "the scratch {seen:?} is where the runner's own guard would refuse to leave a step's \
             temporary files, which is the one thing FENCE-08 was about\nLOG:\n{log}"
        );
        assert_eq!(git(&origin, &["rev-parse", "main"]), out.landed.unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- the warm-up's clippy (LA-C026, GATE-13) ---------------------------------------------

    /// A trunk with one workspace member, `crates/foo`, a topic that changes `touched` (path, text),
    /// and a fake `cargo` on the job's PATH that records its argv, one line per call, in a file,
    /// answers `cargo metadata` with that one member and otherwise runs `extra` (shell) before
    /// succeeding. `cap` sets the warm-up's cap in seconds. Returns the outcome, the job log, the
    /// recorded calls and the merge's wall time.
    fn warm_clippy_merge(
        root: &Path,
        extra: &str,
        touched: &[(&str, &str)],
        cap: Option<u64>,
    ) -> (Outcome, String, Vec<String>, Duration) {
        let (_origin, repo) = main_with_cargo(root, &warm_rules("check"));
        std::fs::create_dir_all(repo.join("crates/foo/src")).unwrap();
        std::fs::write(
            repo.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        commit(&repo, "crates/foo/src/lib.rs", "\n", "a member");
        git(&repo, &["push", "-q", "origin", "main"]);
        let rec = root.join("cargo-calls");
        let shadow = format!(
            "#!/bin/sh\necho \"$*\" >> {rec}\nif [ \"$1\" = metadata ]; then printf '{{\"workspace_members\":[\"foo 0.1\"],\"packages\":[{{\"id\":\"foo 0.1\",\"name\":\"foo\",\"manifest_path\":\"%s/crates/foo/Cargo.toml\"}}]}}' \"$(pwd -P)\"; exit 0; fi\n{extra}\nexit 0\n",
            rec = rec.display()
        );
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("cargo"), &shadow);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let mut head = String::new();
        for (path, text) in touched {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(repo.join(parent)).unwrap();
            }
            head = commit(&repo, path, text, "topic");
        }
        let dir = root.join("job-warm-clippy");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, &repo, "topic", &head)).unwrap();
        x.path = format!("{}:{}", bin.display(), x.path);
        if let Some(c) = cap {
            x.warm_up_secs = c;
        }
        let t = Instant::now();
        let out = x.merge();
        let took = t.elapsed();
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        let calls = std::fs::read_to_string(&rec)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        (out, log, calls, took)
    }

    const GATE_CLIPPY_FOO: &str =
        "clippy --quiet --locked -p foo --all-targets --no-deps --message-format=short";

    #[test]
    fn the_warm_up_runs_the_gate_s_clippy_for_the_touched_member() {
        let root = tempfile::tempdir().unwrap();
        let (out, log, calls, _) = warm_clippy_merge(
            root.path(),
            "",
            &[("crates/foo/src/lib.rs", "pub fn x() {}\n")],
            None,
        );
        assert_eq!(out.code, "MERGED", "{out:?}\nLOG:\n{log}");
        let clippy: Vec<usize> = (0..calls.len())
            .filter(|&i| calls[i].starts_with("clippy"))
            .collect();
        assert_eq!(clippy.len(), 1, "{calls:?}\n{log}");
        assert_eq!(calls[clippy[0]], GATE_CLIPPY_FOO, "{calls:?}");
        let check = calls
            .iter()
            .position(|c| c == "check --workspace --quiet")
            .expect("the cargo check");
        assert!(check < clippy[0], "clippy after check: {calls:?}");
        let line = format!("warm-up: cargo {GATE_CLIPPY_FOO} (not a gate)");
        let w = log
            .find(&line)
            .unwrap_or_else(|| panic!("no `{line}`:\n{log}"));
        let g = log.find("gate: ").expect("gate line");
        assert!(
            w < g,
            "the clippy warm-up is logged before the first gate:\n{log}"
        );
        assert!(log[w..g].contains("warm-up: done in"), "{log}");
        // Not a gate: the report carries only the declared gate.
        let names: Vec<&str> = out.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["cargo check (workspace)", "trivial"], "{log}");
    }

    #[test]
    fn a_docs_only_branch_makes_no_clippy_warm_up() {
        let root = tempfile::tempdir().unwrap();
        let (out, log, calls, _) =
            warm_clippy_merge(root.path(), "", &[("docs/x.md", "text\n")], None);
        assert_eq!(out.code, "MERGED", "{out:?}\nLOG:\n{log}");
        assert!(!calls.iter().any(|c| c.starts_with("clippy")), "{calls:?}");
        assert!(!log.contains("cargo clippy"), "{log}");
        assert!(
            calls.iter().any(|c| c == "check --workspace --quiet"),
            "{calls:?}"
        );
    }

    #[test]
    fn a_failing_clippy_warm_up_does_not_fail_the_merge() {
        let root = tempfile::tempdir().unwrap();
        let (out, log, calls, _) = warm_clippy_merge(
            root.path(),
            "if [ \"$1\" = clippy ]; then exit 1; fi",
            &[("crates/foo/src/lib.rs", "pub fn x() {}\n")],
            None,
        );
        assert!(calls.iter().any(|c| c == GATE_CLIPPY_FOO), "{calls:?}");
        assert!(out.ok, "{out:?}\nLOG:\n{log}");
        assert_eq!(out.code, "MERGED", "{log}");
        let w = log
            .find("warm-up: cargo clippy")
            .expect("clippy warm-up line");
        assert!(
            log[w..].contains("warm-up: failed after")
                && log[w..].contains("(not a gate failure): exit 1"),
            "{log}"
        );
        let names: Vec<&str> = out.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["cargo check (workspace)", "trivial"], "{log}");
        assert!(out.gates.iter().all(|g| g.ok), "{log}");
    }

    #[test]
    fn the_check_and_the_clippy_share_one_warm_up_cap() {
        let root = tempfile::tempdir().unwrap();
        // The check takes 1 s of the 2 s; the clippy would sleep 300 s and gets only what is left.
        let (out, log, calls, took) = warm_clippy_merge(
            root.path(),
            "if [ \"$1\" = check ]; then exec sleep 1; fi\nif [ \"$1\" = clippy ]; then exec sleep 300; fi",
            &[("crates/foo/src/lib.rs", "pub fn x() {}\n")],
            Some(2),
        );
        assert!(calls.iter().any(|c| c == GATE_CLIPPY_FOO), "{calls:?}");
        assert_eq!(out.code, "MERGED", "{out:?}\nLOG:\n{log}");
        assert!(
            took < Duration::from_secs(30),
            "the merge took {took:?}\n{log}"
        );
        let w = log
            .find("warm-up: cargo clippy")
            .expect("clippy warm-up line");
        assert!(
            log[w..].contains("(not a gate failure): timed out after 2s"),
            "{log}"
        );
        // One budget, not two: the clippy was cut after what the check left, not after a cap of its own.
        assert!(!log[w..].contains("failed after 2s"), "{log}");
    }

    // ---- LA-C038 (WORK-26): a gate takes its bq slot before its clock starts ----------------------

    /// Ends a child and reaps it on every path out of a test, a panic included.
    struct Reaped(std::process::Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The usage line a real bq prints, as far as the executor reads it.
    const FAKE_USAGE: &str = "usage: bq run --class short|long|proof|gate|node [--who NAME] [--priority gate|normal] [--max-wait SECONDS] -- cmd args...";

    /// A fake `bq` in `dir`, standing in for `<passwd home>/.local/bin/bq`. It prints its usage when
    /// run bare; `bq run … -- cmd` appends its own arguments and the `BQ_SLOT` it was started with to
    /// `calls`, holds the grant back for the seconds in `delay` (when that file is there), then runs
    /// `cmd` as bq does: as its child, with `BQ_SLOT=long-0` and `BQ_SLOT_PID` its own pid. With a
    /// `never` file it never grants: it waits out `--max-wait` and fails as bq does.
    fn fake_bq(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let bq = dir.join("bq");
        write_exec(
            &bq,
            &format!(
                "#!/bin/sh\nd=$(dirname \"$0\")\n\
                 if [ \"$1\" != run ]; then echo '{FAKE_USAGE}' >&2; exit 2; fi\n\
                 echo \"$* in=$BQ_SLOT\" >> \"$d/calls\"\n\
                 shift\nmaxwait=0\n\
                 while [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do\n\
                 case \"$1\" in --max-wait) maxwait=$2; shift;; esac; shift\ndone\nshift\n\
                 if [ -f \"$d/hang\" ]; then sleep 20; exit 1; fi\n\
                 if [ -f \"$d/never\" ]; then sleep \"$maxwait\"; echo \"no build slot in $maxwait s: the long queue is full\" >&2; exit 1; fi\n\
                 if [ -f \"$d/delay\" ]; then sleep \"$(cat \"$d/delay\")\"; fi\n\
                 echo \"bq: got long-0 after the wait\" >&2\n\
                 BQ_SLOT=long-0 BQ_SLOT_PID=$$ \"$@\"\nexit $?\n"
            ),
        );
        bq
    }

    /// An executor on a fresh job directory under `root`, with `bq` (or none) as its queue.
    fn slot_exec(root: &Path, bq: Option<PathBuf>) -> (Exec, PathBuf) {
        let dir = root.join(format!("job-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(root, root, "topic", "0")).unwrap();
        x.bq = BqSource::Fixed(bq);
        x.kill_grace = Duration::from_millis(500);
        x.source_env = Vec::new();
        (x, dir)
    }

    fn slot_log(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("log")).unwrap_or_default()
    }

    /// The queue wait is longer than the gate's whole limit, and the gate passes: its clock starts at
    /// the grant. Before this change the limit counted from the start and the gate was killed in the queue.
    #[test]
    fn slot_a_queue_wait_longer_than_the_limit_does_not_time_the_gate_out() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = fake_bq(&root.join("fake"));
        std::fs::write(root.join("fake/delay"), "3").unwrap();
        let (mut x, dir) = slot_exec(root, Some(bq));
        // Limit 2 s; the wait is 3 s; the command takes 1 s, about half the limit.
        let ok = x.gate_run(
            root,
            "fast local check",
            "sleep 1",
            true,
            Duration::from_secs(2),
            "2 s",
        );
        let log = slot_log(&dir);
        assert!(ok, "{log}");
        assert!(
            log.contains("gate: fast local check queued for a bq slot"),
            "{log}"
        );
        assert!(
            log.contains("gate: fast local check got long-0 after 3s")
                || log.contains("gate: fast local check got long-0 after 4s"),
            "{log}"
        );
        assert!(!log.contains("timed out"), "{log}");
        // The queued line, then the grant, then the command line.
        let (q, g, c) = (
            log.find("queued for a bq slot").unwrap(),
            log.find("got long-0").unwrap(),
            log.find("$ sleep 1").unwrap(),
        );
        assert!(q < g && g < c, "{log}");
        // The grant marker is gone again.
        assert!(!dir.join("grant-1").exists());
    }

    /// A gate that really overruns after the grant is killed at its limit plus the grace, the child
    /// that ignores TERM included, and the log says the clock started at the grant.
    #[test]
    fn slot_a_gate_that_overruns_after_the_grant_is_killed_at_its_limit() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = fake_bq(&root.join("fake"));
        std::fs::write(root.join("fake/delay"), "1").unwrap();
        let (mut x, dir) = slot_exec(root, Some(bq));
        let pid_file = root.join("gate.pid");
        let cmd = format!(
            "echo $$ > {}; trap '' TERM; while :; do sleep 1; done",
            pid_file.display()
        );
        let t = Instant::now();
        let ok = x.gate_run(root, "g", &cmd, true, Duration::from_secs(2), "2 s");
        let took = t.elapsed();
        let log = slot_log(&dir);
        assert!(!ok, "{log}");
        assert!(
            log.contains("failed: timed out after 2 s (clock started at the bq grant)"),
            "{log}"
        );
        // 1 s of queue + 2 s limit + 0.5 s grace, and nothing like the 30 s of a hung kill.
        assert!(took < Duration::from_secs(15), "{took:?}\n{log}");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "the gate's shell is still there\n{log}"
        );
    }

    /// A gate that is not a build or a check takes no slot: the fake bq is never called. Through a
    /// whole merge, with the gate set the repo's own file makes.
    #[test]
    fn slot_a_non_build_gate_never_calls_bq_and_a_cargo_gate_does() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let fake = root.join("fake");
        let bq = fake_bq(&fake);
        let (_origin, repo) = setup(root);
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("cargo"), "#!/bin/sh\nexit 0\n");
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"poll\"\nrun = \"test -f b.txt\"\n[[gate]]\nname = \"sleeper\"\nrun = \"sleep 0\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let head = commit(&repo, "b.txt", "b\n", "add b");
        let (mut x, dir) = slot_exec(root, Some(bq.clone()));
        x.spec = spec(root, &repo, "topic", &head);
        let out = x.merge();
        assert_eq!(out.code, "MERGED", "{out:?}\n{}", slot_log(&dir));
        assert!(
            !fake.join("calls").exists(),
            "bq was called for a gate that builds nothing"
        );
        assert!(!slot_log(&dir).contains("queued for a bq slot"));

        // The same repo with a gate that runs cargo (shadowed by the script above): one `bq run`.
        git(&repo, &["fetch", "-q", "origin"]);
        git(&repo, &["checkout", "-q", "-B", "main", "origin/main"]);
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"build\"\nrun = \"cargo test --quiet\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "rules two"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "-b", "topic2"]);
        let head = commit(&repo, "c.txt", "c\n", "add c");
        let (mut x, dir) = slot_exec(root, Some(bq));
        x.spec = spec(root, &repo, "topic2", &head);
        x.path = format!("{}:{}", bin.display(), x.path);
        let out = x.merge();
        assert_eq!(out.code, "MERGED", "{out:?}\n{}", slot_log(&dir));
        let calls = std::fs::read_to_string(fake.join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
        assert!(
            calls.starts_with(
                "run --class long --priority gate --max-wait 1800 --who lagent-merge-job_t -- bash -c "
            ),
            "{calls}"
        );
    }

    /// No bq on the host: the gate runs as it always did, and the log says so once.
    #[test]
    fn slot_a_host_without_bq_runs_the_gate_as_it_did() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let (mut x, dir) = slot_exec(root, None);
        let out = root.join("ran");
        let ok = x.gate_run(
            root,
            "build",
            &format!("echo ran > {}", out.display()),
            true,
            Duration::from_secs(5),
            "5 s",
        );
        let log = slot_log(&dir);
        assert!(ok && out.exists(), "{log}");
        assert_eq!(log.matches("takes no bq slot").count(), 1, "{log}");
        assert!(log.contains("there is no bq here"), "{log}");
        assert!(!log.contains("queued for a bq slot"), "{log}");
    }

    /// A file at bq's place that does not print bq's usage: the gate fails with a clear line and runs nothing.
    #[test]
    fn slot_a_bq_that_prints_no_usage_refuses_the_gate() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = root.join("notbq");
        write_exec(&bq, "#!/bin/sh\necho 'this is something else'\n");
        let (mut x, dir) = slot_exec(root, Some(bq));
        let out = root.join("ran");
        let ok = x.gate_run(
            root,
            "build",
            &format!("echo ran > {}", out.display()),
            true,
            Duration::from_secs(5),
            "5 s",
        );
        let log = slot_log(&dir);
        assert!(!ok, "{log}");
        assert!(!out.exists(), "the gate ran\n{log}");
        assert!(
            log.contains("gate: build refused:") && log.contains("does not print bq's usage"),
            "{log}"
        );
    }

    /// A bq that never grants: the gate fails after the queue cap with "waited for a bq slot".
    #[test]
    fn slot_the_queue_cap_fails_the_gate_with_the_wait() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = fake_bq(&root.join("fake"));
        std::fs::write(root.join("fake/never"), "").unwrap();
        let (mut x, dir) = slot_exec(root, Some(bq));
        x.bq_queue_cap_secs = 1;
        let out = root.join("ran");
        let ok = x.gate_run(
            root,
            "build",
            &format!("echo ran > {}", out.display()),
            true,
            Duration::from_secs(60),
            "1 min",
        );
        let log = slot_log(&dir);
        assert!(!ok && !out.exists(), "{log}");
        assert!(
            log.contains("failed: waited for a bq slot for ")
                && log.contains(" s without a grant (bq exit 1)"),
            "{log}"
        );
        assert!(!log.contains("timed out"), "{log}");
        assert!(
            log.contains("--max-wait 1") || log.contains("waits at most 1 s"),
            "{log}"
        );
    }

    /// A bq that neither grants nor exits (it would sit 20 s): the step is ended at the queue cap plus
    /// its slack, not left to hold the job, and the failure says why. R090 F1.
    #[test]
    fn slot_a_bq_that_neither_grants_nor_exits_is_ended_at_the_queue_cap_plus_slack() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = fake_bq(&root.join("fake"));
        std::fs::write(root.join("fake/hang"), "").unwrap();
        let (mut x, dir) = slot_exec(root, Some(bq));
        x.bq_queue_cap_secs = 1;
        x.queue_slack = Duration::from_secs(1);
        let out = root.join("ran");
        let t = Instant::now();
        let ok = x.gate_run(
            root,
            "build",
            &format!("echo ran > {}", out.display()),
            true,
            Duration::from_secs(60),
            "1 min",
        );
        let took = t.elapsed();
        let log = slot_log(&dir);
        assert!(!ok && !out.exists(), "{log}");
        assert!(
            took < Duration::from_secs(10),
            "ended after {took:?}, not at 1 s cap + 1 s slack + grace\n{log}"
        );
        assert!(
            log.contains("failed: waited for a bq slot for ")
                && log.contains("neither granted the slot nor ended"),
            "{log}"
        );
    }

    /// The wrapped command sees `BQ_SLOT` and `BQ_SLOT_PID` as bq sets them, and a slot the executor
    /// itself carried in its environment is not passed on to bq; the names the executor uses to hand
    /// the command over are gone; a gate that takes no slot sees none of it.
    #[test]
    fn slot_the_wrapped_command_sees_the_slot_pair_and_nothing_else_of_ours() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let fake = root.join("fake");
        let bq = fake_bq(&fake);
        let (mut x, dir) = slot_exec(root, Some(bq));
        // A stale pair in the executor's own environment, from a slot that is not this gate's.
        x.source_env = vec![
            (OsString::from("BQ_SLOT"), OsString::from("stale-0")),
            (OsString::from("BQ_SLOT_PID"), OsString::from("1")),
        ];
        let seen = root.join("seen");
        let names = root.join("names");
        let cmd = format!(
            "printf '%s %s\\n' \"$BQ_SLOT\" \"$BQ_SLOT_PID\" > {s}; env | cut -d= -f1 | sort > {n}",
            s = seen.display(),
            n = names.display()
        );
        assert!(
            x.gate_run(root, "g", &cmd, true, Duration::from_secs(10), "10 s"),
            "{}",
            slot_log(&dir)
        );
        let seen_text = std::fs::read_to_string(&seen).unwrap();
        assert!(seen_text.starts_with("long-0 "), "{seen_text}");
        let pid: u32 = seen_text
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        assert!(pid > 1);
        let names_text = std::fs::read_to_string(&names).unwrap();
        for n in ["BQ_SLOT", "BQ_SLOT_PID"] {
            assert!(names_text.lines().any(|l| l == n), "{names_text}");
        }
        for n in [SLOT_COMMAND_VAR, SLOT_GRANT_VAR] {
            assert!(!names_text.lines().any(|l| l == n), "{names_text}");
        }
        // The stale pair was not what the fake bq started with.
        let calls = std::fs::read_to_string(fake.join("calls")).unwrap();
        assert!(calls.trim_end().ends_with("in="), "{calls}");

        // A gate without a slot sees neither name, and nothing of ours.
        let (mut x, dir) = slot_exec(root, Some(fake.join("bq")));
        let names2 = root.join("names2");
        let cmd = format!("env | cut -d= -f1 | sort > {}", names2.display());
        assert!(
            x.gate_run(root, "g", &cmd, false, Duration::from_secs(10), "10 s"),
            "{}",
            slot_log(&dir)
        );
        let t = std::fs::read_to_string(&names2).unwrap();
        for n in ["BQ_SLOT", "BQ_SLOT_PID", SLOT_COMMAND_VAR, SLOT_GRANT_VAR] {
            assert!(!t.lines().any(|l| l == n), "{t}");
        }
    }

    /// The wrapper is a plain `exec bq run …` whose quoting keeps a gate command with quotes, `$`
    /// and braces whole: the command is not in bq's argument list at all.
    #[test]
    fn slot_the_wrapper_names_bq_the_class_the_priority_the_cap_and_no_gate_text() {
        let w = slot_wrapper(Path::new("/h/it's/bq"), "lagent-merge-job_1", 1800);
        assert!(
            w.starts_with(
                "exec '/h/it'\\''s/bq' run --class long --priority gate --max-wait 1800 --who 'lagent-merge-job_1' -- bash -c '"
            ),
            "{w}"
        );
        assert!(w.ends_with("' bash"), "{w}");
        assert!(!w.contains("${"), "{w}");
        assert!(bq_usage_is_ours(FAKE_USAGE));
        assert!(!bq_usage_is_ours("usage: bq run --class short|long"));
        assert!(!bq_usage_is_ours("this is something else"));
    }

    /// A gate with quotes, `$` and braces in its text reaches its shell as written, through the wrapper.
    #[test]
    fn slot_a_gate_text_with_quotes_and_braces_arrives_whole() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bq = fake_bq(&root.join("fake"));
        let (mut x, dir) = slot_exec(root, Some(bq));
        let out = root.join("out");
        let cmd = format!(
            "v='a b'; [ -n \"${{v+x}}\" ] && printf '%s|%s' \"$0\" \"${{v}}\" > {}",
            out.display()
        );
        assert!(
            x.gate_run(root, "g", &cmd, true, Duration::from_secs(10), "10 s"),
            "{}",
            slot_log(&dir)
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "bash|a b");
    }

    /// The test binary, re-run as the executor of the real-bq test below: it takes the gate slot with
    /// the real bq and waits inside it. With no `LA_C038_ROOT` it is nothing, and passes.
    #[test]
    fn slot_real_bq_executor_child() {
        let Some(root) = std::env::var_os("LA_C038_ROOT").map(PathBuf::from) else {
            return;
        };
        let bq = std::env::var_os("LA_C038_BQ").map(PathBuf::from).unwrap();
        let dir = root.join("child-job");
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = Exec::open(&dir, spec(&root, &root, "topic", "0")).unwrap();
        x.bq = BqSource::Fixed(Some(bq));
        let cmd = std::fs::read_to_string(root.join("gate-command")).unwrap();
        x.gate_run(&root, "hold", &cmd, true, Duration::from_secs(60), "1 min");
    }

    /// The REAL bq, with its state directory in the test's own directory and one `long` slot. The gate
    /// holds the slot; the meta file names a pid that is an ancestor of the gate command, and the gate
    /// sees `BQ_SLOT` and `BQ_SLOT_PID` for it (so `scripts/check-fast`'s `bq_slot_is_real` holds in
    /// the wrapped command). Then the executor is killed with SIGKILL and what bq does with the
    /// slot is measured, not assumed. Skipped, with the reason printed, where there is no bq.
    #[test]
    fn slot_real_bq_slot_after_the_executor_is_killed() {
        let Some(home) = passwd_home() else {
            eprintln!("skipped: no passwd home");
            return;
        };
        let bq = home.join(".local/bin/bq");
        if !bq_prints_usage(&bq) {
            eprintln!("skipped: {} is missing or is not bq", bq.display());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let state = root.join("bqstate");
        let knobs = [
            ("BQ_DIR", state.display().to_string()),
            ("BQ_NO_SCOPE", "1".into()),
            ("BQ_LONG_SLOTS", "1".into()),
            ("BQ_NO_TOUCH", "1".into()),
            ("BQ_FREE_FLOOR_GIB", "0".into()),
            ("BQ_FREE_DISK_RESERVE_GIB", "0".into()),
            ("BQ_MEM_FLOOR_GIB", "0".into()),
            ("BQ_MEM_RESERVE_GIB", "0".into()),
            ("BQ_LOAD_CEILING", "0".into()),
        ];
        // The gate: write what the wrapped command sees, then hold the slot for 6 s.
        let seen = root.join("seen");
        std::fs::write(
            root.join("gate-command"),
            format!(
                "echo \"$$ $BQ_SLOT $BQ_SLOT_PID\" > {}; sleep 6",
                seen.display()
            ),
        )
        .unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "pipeline::exec::tests::slot_real_bq_executor_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LA_C038_ROOT", root)
        .env("LA_C038_BQ", &bq)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in &knobs {
            cmd.env(k, v);
        }
        let mut executor = Reaped(cmd.spawn().unwrap());
        // The gate has the slot when it has written what it sees.
        let t = Instant::now();
        while !seen.exists()
            || std::fs::read_to_string(&seen)
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            assert!(
                t.elapsed() < Duration::from_secs(60),
                "the gate never got the slot"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let seen_text = std::fs::read_to_string(&seen).unwrap();
        let f: Vec<&str> = seen_text.split_whitespace().collect();
        let (gate_pid, slot, holder) = (f[0], f[1], f[2]);
        assert_eq!(slot, "long-0", "{seen_text}");
        // bq's slot record names that pid, with a class, and the pid is an ancestor of the gate command.
        let meta = std::fs::read_to_string(state.join("long-0.meta")).unwrap();
        assert!(meta.lines().any(|l| l == format!("pid={holder}")), "{meta}");
        assert!(meta.lines().any(|l| l == "class=long"), "{meta}");
        let (mut p, mut found) = (gate_pid.to_string(), false);
        for _ in 0..16 {
            let stat = std::fs::read_to_string(format!("/proc/{p}/stat")).unwrap_or_default();
            let ppid = stat
                .rsplit_once(')')
                .and_then(|(_, r)| r.split_whitespace().nth(1))
                .unwrap_or("0")
                .to_string();
            if ppid == holder {
                found = true;
                break;
            }
            p = ppid;
        }
        assert!(
            found,
            "BQ_SLOT_PID {holder} is not an ancestor of the gate command"
        );

        // The executor dies without a word.
        executor.0.kill().unwrap();
        executor.0.wait().unwrap();
        let second = |wait: &str| {
            let mut c = Command::new(&bq);
            c.args([
                "run",
                "--class",
                "long",
                "--priority",
                "gate",
                "--max-wait",
                wait,
            ])
            .args(["--who", "la-c038-second", "--", "true"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
            for (k, v) in &knobs {
                c.env(k, v);
            }
            let t = Instant::now();
            let o = c.output().unwrap();
            (
                o.status.success(),
                t.elapsed(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            )
        };
        // bq's rule is the live holder: the `bq run` the dead executor left is alive, so for as long
        // as it is, the slot is not free. This line is what was measured, printed for the report.
        let (got, after, err) = second("1");
        eprintln!(
            "second bq run right after the executor's death: got={got} after={after:?} {err}"
        );
        // It is free once the holder is gone: the gate ends by itself, after at most 6 s, and bq's
        // own accounting hands the slot on, with nothing from the executor.
        let (got, after, err) = second("40");
        assert!(got, "the slot never came back: {err}");
        assert!(after < Duration::from_secs(30), "{after:?}");
    }

    // ---- LA-C038 (WORK-26): a step is bound to the executor's unit --------------------------------

    /// What a transient user unit and a real bq need, or why the test is skipped.
    fn bound_env() -> Result<PathBuf, String> {
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            return Err("no systemd user manager (XDG_RUNTIME_DIR is not set)".into());
        }
        let ok = Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--",
                "/bin/true",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("systemd-run --user --scope does not work here".into());
        }
        let home = passwd_home().ok_or("no passwd home")?;
        let bq = home.join(".local/bin/bq");
        if !bq_prints_usage(&bq) {
            return Err(format!("{} is missing or is not bq", bq.display()));
        }
        Ok(bq)
    }

    /// bq's own knobs for a hermetic queue in `state`: one long slot, no admission holds, the reserved
    /// gate slot far off. bq keeps its own scope for the job (as in production).
    fn bound_knobs(state: &Path) -> Vec<(&'static str, String)> {
        vec![
            ("BQ_DIR", state.display().to_string()),
            ("BQ_LONG_SLOTS", "1".into()),
            ("BQ_NO_TOUCH", "1".into()),
            ("BQ_GATE_AFTER_S", "3600".into()),
            ("BQ_FREE_FLOOR_GIB", "0".into()),
            ("BQ_FREE_DISK_RESERVE_GIB", "0".into()),
            ("BQ_MEM_FLOOR_GIB", "0".into()),
            ("BQ_MEM_RESERVE_GIB", "0".into()),
            ("BQ_LOAD_CEILING", "0".into()),
        ]
    }

    /// A real transient user unit, stopped on every path out of the test.
    struct UnitGuard(String);

    impl Drop for UnitGuard {
        fn drop(&mut self) {
            for verb in ["stop", "reset-failed"] {
                let _ = Command::new("systemctl")
                    .args(["--user", verb, &self.0])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }

    fn systemctl_user(args: &[&str]) -> String {
        let o = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    /// This test binary as the executor of job `job_<tag>`, as the unit `lagent-job-<tag>`, the way
    /// `start_runner` starts one: `mode` is what the child does (`hold`: a gate holding the bq slot;
    /// `plain`: a gate that takes none).
    fn start_executor_unit(
        root: &Path,
        tag: &str,
        bq: &Path,
        mode: &str,
        knobs: &[(&'static str, String)],
        props: &[&str],
    ) -> UnitGuard {
        let unit = format!("lagent-job-{tag}");
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--quiet", "--collect", "--unit", &unit]);
        c.arg(format!("--working-directory={}", root.display()));
        for p in props {
            c.arg(format!("--property={p}"));
        }
        let mut envs: Vec<(String, String)> = vec![
            ("LA_C038_ROOT".into(), root.display().to_string()),
            ("LA_C038_TAG".into(), tag.to_string()),
            ("LA_C038_BQ".into(), bq.display().to_string()),
            ("LA_C038_MODE".into(), mode.to_string()),
        ];
        envs.extend(knobs.iter().map(|(k, v)| (k.to_string(), v.clone())));
        for (k, v) in &envs {
            c.arg(format!("--setenv={k}={v}"));
        }
        c.arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pipeline::exec::tests::slot_bound_executor_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdin(Stdio::null());
        let o = c.output().unwrap();
        assert!(
            o.status.success(),
            "systemd-run: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        UnitGuard(format!("{unit}.service"))
    }

    /// The test binary, re-run as a unit's executor (see [`start_executor_unit`]). With no
    /// `LA_C038_TAG` it is nothing, and passes.
    #[test]
    fn slot_bound_executor_child() {
        let Some(tag) = std::env::var("LA_C038_TAG").ok() else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("LA_C038_ROOT").unwrap());
        let dir = root.join(format!("job-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = spec(&root, &root, "topic", "0");
        s.job_id = format!("job_{tag}");
        let mut x = Exec::open(&dir, s).unwrap();
        std::fs::write(root.join("bound"), format!("{:?}", x.executor_unit)).unwrap();
        let cmd = std::fs::read_to_string(root.join("gate-command")).unwrap();
        let hold = std::env::var("LA_C038_MODE").ok().as_deref() == Some("hold");
        if hold {
            x.bq = BqSource::Fixed(Some(PathBuf::from(std::env::var_os("LA_C038_BQ").unwrap())));
        }
        let ok = x.gate_run(&root, "gate", &cmd, hold, Duration::from_secs(120), "2 min");
        std::fs::write(root.join("result"), format!("{ok}")).unwrap();
    }

    fn wait_until(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(
                t.elapsed() < Duration::from_secs(secs),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn bq_run_second(bq: &Path, knobs: &[(&'static str, String)], wait: &str) -> (bool, Duration) {
        let mut c = Command::new(bq);
        c.args([
            "run",
            "--class",
            "long",
            "--priority",
            "gate",
            "--max-wait",
            wait,
        ])
        .args(["--who", "la-c038-second", "--", "true"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(
            ["XDG_RUNTIME_DIR", "HOME"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in knobs {
            c.env(k, v);
        }
        let t = Instant::now();
        let ok = c.status().unwrap().success();
        (ok, t.elapsed())
    }

    /// Condition 1: a gate step under the REAL bq in an executor that runs as a real user unit. When
    /// the executor's unit is killed (`how = "kill"`: SIGKILL to its main process) or stopped
    /// (`how = "stop"`, what `stop_runner` does), systemd stops the step's scope, bq dies with it, and
    /// bq's own holder check hands the slot to a second `bq run` at once, not when the gate ends.
    fn slot_is_freed_when_the_executor_unit_ends(how: &str) {
        let bq = match bound_env() {
            Ok(b) => b,
            Err(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        };
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let tag = format!("c038{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let state = root.join("bqstate");
        let knobs = bound_knobs(&state);
        let seen = root.join("seen");
        // The gate holds the slot for 8 s; a second bq run that gets it sooner got it from bq's
        // holder check, not from the gate ending.
        std::fs::write(
            root.join("gate-command"),
            format!(
                "echo \"$$ $BQ_SLOT $BQ_SLOT_PID\" > {}; sleep 8",
                seen.display()
            ),
        )
        .unwrap();
        let _unit = start_executor_unit(root, &tag, &bq, "hold", &knobs, &["TimeoutStopSec=30"]);
        wait_until(60, "the gate to hold the slot", || {
            std::fs::read_to_string(&seen)
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
        });
        let bound = std::fs::read_to_string(root.join("bound")).unwrap();
        assert_eq!(
            bound,
            format!("Some(\"lagent-job-{tag}.service\")"),
            "the executor did not find its own unit"
        );
        let scopes = || {
            systemctl_user(&[
                "list-units",
                "--all",
                "--no-legend",
                &format!("lucina-gate-job-{tag}-*"),
            ])
        };
        assert!(
            scopes().contains("lucina-gate-job"),
            "the step has no scope of its own"
        );
        let t = Instant::now();
        if how == "kill" {
            systemctl_user(&[
                "kill",
                "--kill-whom=main",
                "--signal=SIGKILL",
                &format!("lagent-job-{tag}.service"),
            ]);
        } else {
            systemctl_user(&["stop", &format!("lagent-job-{tag}.service")]);
        }
        let (got, after) = bq_run_second(&bq, &knobs, "5");
        assert!(
            got,
            "the slot was not handed on within 5 s of the executor's unit ending ({how})"
        );
        eprintln!(
            "{how}: second bq run got the slot {after:?} after the unit ended ({:?} in all)",
            t.elapsed()
        );
        // The proof, as assertions: the slot came from bq's holder check within moments of the unit
        // ending, while the gate (8 s) was still running. Had it come from the gate ending, `after`
        // would be seconds and the gate's process gone.
        assert!(
            after < Duration::from_secs(3),
            "the slot took {after:?} to be handed on ({how})"
        );
        let gate_pid = std::fs::read_to_string(&seen).unwrap();
        let gate_pid = gate_pid.split_whitespace().next().unwrap();
        assert!(
            Path::new(&format!("/proc/{gate_pid}")).exists(),
            "the gate had already ended when the slot was handed on, so it was not the holder check ({how})"
        );
        wait_until(10, "the step's scope to be gone", || {
            scopes().trim().is_empty()
        });
    }

    #[test]
    fn slot_real_bq_a_killed_executor_unit_takes_the_step_and_frees_the_slot() {
        slot_is_freed_when_the_executor_unit_ends("kill");
    }

    #[test]
    fn slot_real_bq_a_stopped_executor_unit_takes_the_step_and_frees_the_slot() {
        slot_is_freed_when_the_executor_unit_ends("stop");
    }

    fn bq_status(bq: &Path, knobs: &[(&'static str, String)]) -> String {
        let mut c = Command::new(bq);
        c.arg("status")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(
                ["XDG_RUNTIME_DIR", "HOME"]
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
            )
            .stdin(Stdio::null());
        for (k, v) in knobs {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    }

    /// Condition 2: a job cancelled while its bq is still queued leaves no waiting entry behind.
    #[test]
    fn slot_real_bq_a_job_cancelled_while_queued_leaves_no_queue_entry() {
        let bq = match bound_env() {
            Ok(b) => b,
            Err(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        };
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let tag = format!("c038{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let state = root.join("bqstate");
        let knobs = bound_knobs(&state);
        // Somebody else holds the only long slot for 15 s.
        let mut h = Command::new(&bq);
        h.args([
            "run",
            "--class",
            "long",
            "--priority",
            "gate",
            "--max-wait",
            "60",
        ])
        .args(["--who", "la-c038-holder", "--", "sleep", "15"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(
            ["XDG_RUNTIME_DIR", "HOME"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in &knobs {
            h.env(k, v);
        }
        let _holder = Reaped(h.spawn().unwrap());
        wait_until(30, "the holder to have the slot", || {
            state.join("long-0.meta").exists()
        });
        std::fs::write(root.join("gate-command"), "true").unwrap();
        let _unit = start_executor_unit(root, &tag, &bq, "hold", &knobs, &["TimeoutStopSec=30"]);
        let who = format!("lagent-merge-job_{tag}");
        wait_until(30, "the gate to be queued", || {
            bq_status(&bq, &knobs).contains(&who)
        });
        systemctl_user(&["stop", &format!("lagent-job-{tag}.service")]);
        wait_until(10, "the queue entry to be gone", || {
            !bq_status(&bq, &knobs).contains(&who)
        });
    }

    /// Condition 3: where the executor does not know its own unit, or the step has no scope at all,
    /// the step still runs and says in the job log, once, that it is not bound.
    #[test]
    fn slot_a_step_that_is_not_bound_runs_and_says_so_once() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        // The scope route, with no executor unit known.
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            let (mut x, dir) = slot_exec(root, None);
            x.source_env = vec![(OsString::from("XDG_RUNTIME_DIR"), xdg)];
            assert!(x.executor_unit.is_none());
            let out = root.join("ran1");
            assert!(x.gate_run(
                root,
                "g",
                &format!("echo ran > {}", out.display()),
                false,
                Duration::from_secs(20),
                "20 s"
            ));
            let log = slot_log(&dir);
            assert!(out.exists(), "{log}");
            assert_eq!(log.matches("bound: no").count(), 1, "{log}");
            assert!(log.contains("does not know its own systemd unit"), "{log}");
        } else {
            eprintln!("scope route skipped: no XDG_RUNTIME_DIR");
        }
        // The fallback route: no user manager, the process group alone.
        let (mut x, dir) = slot_exec(root, None);
        x.source_env = Vec::new();
        let out = root.join("ran2");
        assert!(x.gate_run(
            root,
            "g",
            &format!("echo ran > {}", out.display()),
            false,
            Duration::from_secs(20),
            "20 s"
        ));
        let log = slot_log(&dir);
        assert!(out.exists(), "{log}");
        assert_eq!(log.matches("bound: no").count(), 1, "{log}");
        assert!(log.contains("the step has no scope of its own"), "{log}");
    }

    /// The scope's arguments: bound to the executor's unit when it is known, and as before when not.
    #[test]
    fn slot_the_scope_binds_to_the_executor_unit_and_only_when_it_is_known() {
        let bash = Path::new("/bin/bash");
        let run = Path::new("/usr/bin/systemd-run");
        let strs = |v: Vec<OsString>| {
            v.iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        };
        let (_, bound) = Exec::step_argv_bound(
            bash,
            Some(run),
            Some("u"),
            "x",
            Some("lagent-job-a.service"),
        );
        let bound = strs(bound);
        let at = bound
            .iter()
            .position(|a| a == "BindsTo=lagent-job-a.service")
            .unwrap();
        assert_eq!(bound[at - 1], "-p");
        assert_eq!(bound[at + 1], "-p");
        assert_eq!(bound[at + 2], "After=lagent-job-a.service");
        assert!(at + 2 < bound.iter().position(|a| a == "--").unwrap());
        let (_, plain) = Exec::step_argv_bound(bash, Some(run), Some("u"), "x", None);
        assert_eq!(
            strs(plain.clone()),
            strs(Exec::step_argv(bash, Some(run), Some("u"), "x").1)
        );
        assert!(!strs(plain).iter().any(|a| a.starts_with("BindsTo")));
        assert_eq!(super::super::runner_unit("job_ab12"), "lagent-job-ab12");
    }

    /// Condition 4: the binding does not touch a step while its executor is healthy. The executor
    /// runs as a real unit with `TimeoutStartSec=1` and `TimeoutStopSec=1` (shortened knobs, against
    /// the default 90 s; a 90 s step was not run) and its slot-holding step under the real bq runs
    /// 5 s, longer than both; it completes and passes. While it runs, the host itself is asked: the
    /// step's scope names the executor's unit in `BindsTo`.
    #[test]
    fn slot_a_slow_step_of_a_healthy_executor_unit_completes() {
        let bq = match bound_env() {
            Ok(b) => b,
            Err(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        };
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let tag = format!("c038{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        std::fs::write(
            root.join("gate-command"),
            format!("sleep 5; echo done > {}", root.join("done").display()),
        )
        .unwrap();
        let knobs = bound_knobs(&root.join("bqstate"));
        let _unit = start_executor_unit(
            root,
            &tag,
            &bq,
            "hold",
            &knobs,
            &["TimeoutStartSec=1", "TimeoutStopSec=1"],
        );
        // While the step runs, the host says what its scope is bound to.
        let mut scope = String::new();
        wait_until(30, "the step's scope", || {
            scope = systemctl_user(&[
                "list-units",
                "--all",
                "--no-legend",
                &format!("lucina-gate-job-{tag}-*"),
            ])
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .to_string();
            !scope.is_empty()
        });
        let binds = systemctl_user(&["show", "-p", "BindsTo", "--value", &scope]);
        assert_eq!(
            binds.trim(),
            format!("lagent-job-{tag}.service"),
            "scope {scope}"
        );
        wait_until(60, "the gate to end", || root.join("result").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("result")).unwrap(),
            "true"
        );
        assert!(root.join("done").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("bound")).unwrap(),
            format!("Some(\"lagent-job-{tag}.service\")")
        );
        // The log must be there to say nothing about the step being unbound.
        let log = std::fs::read_to_string(root.join(format!("job-{tag}/log"))).unwrap();
        assert!(log.contains("queued for a bq slot"), "{log}");
        assert!(!log.contains("bound: no"), "{log}");
    }
}

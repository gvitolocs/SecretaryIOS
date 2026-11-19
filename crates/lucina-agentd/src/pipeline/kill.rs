//! Ending a step of a merge or deploy job: the two routes to every process it started, and the
//! rounds of signals that take them.
//!
//! A step is a shell command of somebody else's writing. It forks, it forks again, some of what
//! it forks outlives it — and on this host `timeout(1)` (uutils coreutils) cannot end it: its
//! SIGTERM goes to the process group, its final SIGKILL only to the process it waits on. A
//! grandchild that ignores SIGTERM keeps running past the limit, sometimes still holding the
//! shared `CARGO_TARGET_DIR`'s cargo lock, and the next gate blocks on a step that has already
//! been reported (finding GATE-05). So this module holds the deadline's two blows itself:
//!
//! * the **process group** the step leads (it is started with `process_group(0)`) — the only
//!   route that reaches work the step handed to a nested `systemd-run --scope`, which keeps the
//!   caller's group and lands in a *sibling* control group, invisible to every pid list;
//! * the step's own transient **scope control group**, whose `cgroup.procs` names everything that
//!   stayed in the group's tree — including a `setsid` child, which left the group and is reached
//!   by nothing else.
//!
//! TERM first (the step may still clean up after itself), then the grace, then KILL in rounds with
//! a hard bound. Every signal goes through `kill(2)` — never a forked `/bin/kill`, because a step
//! that has taken every task slot of its scope leaves no fork for the kill that is supposed to
//! end it — and every refusal is kept in [`Miss`] and reported, never discarded.
//!
//! [`super::exec`] owns the environment, the log lines and the outcome; this owns the targets and
//! the waiting.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{kill_process, kill_process_group, Pid, Signal};

/// How often a step that has passed its deadline is looked at and, past the grace, signalled
/// again.
const TICK: Duration = Duration::from_millis(250);
/// How often a running step is looked at: whether it has ended, whether its scope has shown up,
/// and whether its deadline has passed.
const POLL: Duration = Duration::from_millis(50);
/// The same while the step's control group is still unaccounted for. `systemd-run` moves into the
/// scope before it execs the payload, so the window in which the step's own `/proc` entry names
/// somebody else's group is short — but a step that ends inside it would lose its second route.
const POLL_EARLY: Duration = Duration::from_millis(10);
/// How long the step's own `/proc` entry is watched for the scope it was asked into.
/// `systemd-run` makes the unit and moves into it before it execs the payload, so the name is
/// there tens of milliseconds after the start — and a step that has not landed in it by now never
/// will, and is worked by its process group alone.
const SCOPE_LOOK: Duration = Duration::from_secs(2);
/// How long the KILL rounds keep coming after the grace before the step is reported with whatever
/// was still there. Nothing is waited on past it: an unbounded wait is how a limit that did not
/// bite read as the step's own length (GATE-05).
const SLACK: Duration = Duration::from_secs(2);
/// The cap of the one ask this step makes of the user manager for its unit's control group. The
/// ask happens in the step's own wait, before any signal and again before the sweep, so an
/// unbounded wait on it would hold a job past every bound this module has (GATE-05): `systemctl`
/// that stops answering is ended here, and its silence read as "no answer", not as a hang.
const MANAGER_ASK_CAP: Duration = Duration::from_secs(5);
/// The prefix of a step's transient scope unit: `lucina-gate-<job id>-<pid>-<n>`, the job, this
/// process and a number no other step of this process has had. This crate names a scope the same
/// way twice already (`manager::work` for lane runners, `lagent-job-…` for the job runner itself).
const SCOPE_PREFIX: &str = "lucina-gate-";

/// The sequence behind a unit name: one per scope this process asks for, whatever job or test
/// thread it belongs to. A job's own counter would not do — a test binary runs several jobs of one
/// job id side by side, and two steps of one name are two steps fighting for one control group.
static UNIT_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_unit_no() -> u64 {
    UNIT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The unit name of a step's scope. systemd takes letters, digits and `-` in a unit name of our
/// own making, so anything else in the job id becomes `-` here rather than a name the manager
/// refuses.
pub(crate) fn unit_name(job_id: &str) -> String {
    let safe = job_id
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!(
        "{SCOPE_PREFIX}{}-{}-{n}",
        safe.trim_matches('-'),
        std::process::id(),
        n = next_unit_no()
    )
}

/// The name a scope probe asks for: the same shape, in this process's own name, never a step's.
pub(crate) fn probe_name() -> String {
    format!(
        "{SCOPE_PREFIX}probe-{}-{n}",
        std::process::id(),
        n = next_unit_no()
    )
}

/// The fields of a `/proc/<pid>/stat` line that come after `comm` — state, ppid, pgid, … — which
/// is where they count from, because `comm` holds spaces and parentheses.
fn stat_after_comm(t: &str) -> impl Iterator<Item = &str> + '_ {
    t.rsplit(") ").next().unwrap_or_default().split_whitespace()
}

/// This process's parent and its own process group, from `/proc/self/stat`: forking `ps` to ask
/// is the thing a starved scope cannot do, and these two are what the screen below refuses. They
/// cannot change while this process lives, so a read that worked is kept — and only one that worked.
/// A failure kept here is a screen with nothing drawn on it, and then every pid that cannot be
/// proved to be this process's group or parent passes (finding LA-R059, item 4): ask again.
fn own_ids() -> Option<(u32, u32)> {
    if blind_to_own_ids() {
        return None;
    }
    static IDS: OnceLock<(u32, u32)> = OnceLock::new();
    if let Some(ids) = IDS.get() {
        return Some(*ids);
    }
    let t = fs::read_to_string("/proc/self/stat").ok()?;
    let mut f = stat_after_comm(&t);
    let _state = f.next()?;
    let ppid = f.next()?.parse().ok()?;
    let pgid = f.next()?.parse().ok()?;
    let _ = IDS.set((ppid, pgid));
    Some(*IDS.get()?)
}

// The seam of [`own_ids`]: with it on, this process cannot read its own ids, which is the only way
// a test can stand in the way of a screen instead of reading the real ones.
#[cfg(test)]
thread_local! {
    static NO_OWN_IDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(not(test))]
fn blind_to_own_ids() -> bool {
    false
}

#[cfg(test)]
fn blind_to_own_ids() -> bool {
    NO_OWN_IDS.with(|b| b.get())
}

#[cfg(test)]
pub(crate) fn refuse_own_ids(on: bool) {
    NO_OWN_IDS.with(|b| b.set(on));
}

// The seam of [`Step::look_for_cgroup`]: with it on, a step's own `/proc` entry names the cgroup it is
// in to nobody, which is the only way a test can stand in the gap that opens when `systemd-run` has
// started the unit but not yet exec'd into it.
#[cfg(test)]
thread_local! {
    static NO_CGROUP_SIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(not(test))]
fn blind_to_cgroups() -> bool {
    false
}

#[cfg(test)]
fn blind_to_cgroups() -> bool {
    NO_CGROUP_SIGHT.with(|b| b.get())
}

#[cfg(test)]
pub(crate) fn refuse_cgroup_sight(on: bool) {
    NO_CGROUP_SIGHT.with(|b| b.set(on));
}

/// The screen a pid *list* can apply by itself: 0, 1 and this process are refused whether or not the
/// two inherited ids are known, and the two that need them are applied again, with the refusal they
/// carry, at the moment a signal is actually aimed. A list that held this process's own group is
/// thus never signalled by holding it: it is signalled by nothing.
fn listable(target: u32) -> bool {
    target > 1 && target != std::process::id()
}

/// Whether a signal may be aimed at `target`: never pid 0 (which `kill` reads as *this* process's
/// group), never pid 1, never this process (it has a job log and a `result.json` to finish), never
/// this process's group (the daemon, the test binary, and every other step of the job) and never
/// its parent. Everything a step's group or scope names besides those is the step's own — and when
/// this process does not know its own ids, nothing is its own: the screen is the only thing between
/// a `cgroup.procs` list and the process reading it, so an unscreenable target is a target left
/// alone, and said.
fn signalable(target: u32, miss: &mut Miss) -> bool {
    let Some((ppid, pgid)) = own_ids() else {
        miss.add_process(format!(
            "this process's own ids could not be read; {target} was not signalled"
        ));
        return false;
    };
    if !(listable(target) && Some(target) != Some(pgid) && Some(target) != Some(ppid)) {
        return false;
    }
    // agentd — its pid and its process group — is refused by every route, as the user manager names it.
    miss.guard.is_none_or(|a| a.refuses(target).is_none())
}

/// What the last round of a kill could not deliver. A kill whose failure is thrown away is a kill
/// that looks delivered: that is exactly how GATE-05 read for as long as it did, a deadline that
/// fired and a step that went on running.
#[derive(Default)]
pub(crate) struct Miss {
    processes: u32,
    why: Option<String>,
    cgroups: u32,
    cgroup_why: Option<String>,
    /// agentd's ids for this round, which every signal of it is screened against.
    guard: Option<AgentdIds>,
    /// What the round has to say that is not a refusal: where agentd's pid came from, or that it is unknown.
    notes: Vec<String>,
}

impl Miss {
    fn add_process(&mut self, why: String) {
        self.processes += 1;
        self.why.get_or_insert(why);
    }

    fn add_cgroup(&mut self, why: String) {
        self.cgroups += 1;
        self.cgroup_why.get_or_insert(why);
    }

    fn add_note(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// Nothing was refused. A note is not a refusal.
    pub(crate) fn is_empty(&self) -> bool {
        self.processes == 0 && self.cgroups == 0
    }

    /// Nothing was refused and there is nothing to note either: the one case worth no log line.
    pub(crate) fn says_nothing(&self) -> bool {
        self.is_empty() && self.notes.is_empty()
    }

    /// The line the job log gets when a round did not reach everything: empty when it did, which
    /// is the normal case and the only one worth no line at all.
    pub(crate) fn line(&self) -> String {
        let mut parts = Vec::new();
        if self.processes > 0 {
            parts.push(format!(
                "{} process(es) of the step refused the signal: {}",
                self.processes,
                self.why.as_deref().unwrap_or("no reason given")
            ));
        }
        if self.cgroups > 0 {
            parts.push(format!(
                "{} of the step's `cgroup.kill` writes were refused: {}",
                self.cgroups,
                self.cgroup_why.as_deref().unwrap_or("no reason given")
            ));
        }
        parts.extend(self.notes.iter().cloned());
        parts.join("; ")
    }
}

/// The reason a signal was refused, or `None` when there was nothing to refuse: a target that is
/// already gone (`ESRCH`) is reached, not missed.
fn refused(e: Errno) -> Option<String> {
    if e.raw_os_error() == Errno::SRCH.raw_os_error() {
        None
    } else {
        Some(e.to_string())
    }
}

fn signal_pid(pid: u32, sig: Signal, miss: &mut Miss) {
    if !signalable(pid, miss) {
        return;
    }
    // The screen refused 0, so the conversion cannot fail.
    let Some(p) = Pid::from_raw(pid as i32) else {
        return;
    };
    if let Err(e) = kill_process(p, sig) {
        if let Some(why) = refused(e) {
            miss.add_process(format!("{why} (pid {pid})"));
        }
    }
}

fn signal_group(pgid: u32, sig: Signal, miss: &mut Miss) {
    if !signalable(pgid, miss) {
        return;
    }
    let Some(p) = Pid::from_raw(pgid as i32) else {
        return;
    };
    // `kill_process_group` is `kill(-pgid, sig)`, and only ever called with a group id that
    // passed the screen: the group of 0 is this process's own.
    if let Err(e) = kill_process_group(p, sig) {
        if let Some(why) = refused(e) {
            miss.add_process(format!("{why} (the group of {pgid})"));
        }
    }
}

/// Whether `pid` is a zombie: already dead, waiting only to be reaped by its parent. It names
/// itself in a group and in a `cgroup.procs` until it is, and no signal reaches it, so counting it
/// as a survivor would have a reap wait for a process that is gone.
fn is_zombie(pid: u32) -> bool {
    match fs::read(format!("/proc/{pid}/stat")) {
        Ok(t) => stat_after_comm(&String::from_utf8_lossy(&t)).next() == Some("Z"),
        Err(_) => false,
    }
}

/// The pids a signal to the whole group does not reach, read from the group's own `/proc`
/// entries: no fork, no `pgrep`. Screened, so what comes back is only ever the step's own.
pub(crate) fn group_pids(pgid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let Ok(pid) = e.file_name().to_str().unwrap_or_default().parse::<u32>() else {
            continue;
        };
        if !listable(pid) {
            continue;
        }
        // `/proc/<pid>/stat`: `pid (comm) state ppid pgid …`, and `comm` may hold spaces.
        let Ok(stat) = fs::read(e.path().join("stat")) else {
            continue;
        };
        let text = String::from_utf8_lossy(&stat);
        let mut f = stat_after_comm(&text);
        let state = f.next();
        let _ppid = f.next();
        if state != Some("Z") && f.next().and_then(|p| p.parse::<u32>().ok()) == Some(pgid) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Every pid named by `cgroup.procs` in `cg` and in every control group under it, unscreened: the
/// sweep's refusal has to see this process and its kin, which [`cgroup_pids`] drops.
fn raw_cgroup_pids(cg: &Path) -> Vec<u32> {
    scan_cgroup(cg).1
}

/// Every pid in `cg` and in every control group under it, this process's own excluded by the
/// screen. A step's child can `mkdir` inside its own delegated scope and move a grandchild into
/// it, which drops that grandchild from the scope's list for good, so this descends.
fn cgroup_pids(cg: &Path) -> Vec<u32> {
    let mut out = raw_cgroup_pids(cg);
    out.retain(|p| listable(*p) && !is_zombie(*p));
    out
}

/// The parent and the process group of `pid`, from its `/proc` entry.
fn parent_and_group(pid: u32) -> Option<(u32, u32)> {
    let t = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let t = String::from_utf8_lossy(&t);
    let mut f = stat_after_comm(&t);
    let _state = f.next()?;
    let ppid = f.next()?.parse().ok()?;
    let pgid = f.next()?.parse().ok()?;
    Some((ppid, pgid))
}

/// How many parents up the chain is followed from this process: far past any real depth, so the
/// walk is bounded even when `/proc` answers nonsense.
const ANCESTOR_HOPS: usize = 64;

/// This process's ancestors, nearest first, ending at pid 1 or after [`ANCESTOR_HOPS`].
fn ancestors() -> Vec<u32> {
    let mut out = Vec::new();
    let mut at = std::process::id();
    for _ in 0..ANCESTOR_HOPS {
        let Some((ppid, _)) = parent_and_group(at) else {
            break;
        };
        if ppid == 0 || out.contains(&ppid) {
            break;
        }
        out.push(ppid);
        at = ppid;
    }
    out
}

/// The name of the one variable of the environment this module reads: the main pid of agentd as the
/// daemon gave it when it started the executor (`manager::pipeline::start_runner`). The executor's
/// own environment is out of a step's reach, but the pid goes stale when agentd restarts under a
/// job unit that outlives it, so it is only the fallback to what the user manager says.
const AGENTD_PID_VAR: &str = "LUCINA_AGENTD_PID";
/// The user unit agentd runs as, whose `MainPID` the user manager states and a step cannot change.
const AGENTD_UNIT: &str = "lucina-agentd";
/// How long an answer about agentd's pid is reused by the KILL rounds, which come every few
/// milliseconds: the ask is a fork, and a restart takes longer than this.
const AGENTD_REFRESH: Duration = Duration::from_secs(1);

/// agentd's pid as the daemon gave it at the job's start, read once: `None` when the variable is
/// missing or not a number. Nothing else of the environment is read.
#[cfg(not(test))]
fn agentd_env_pid() -> Option<u32> {
    static PID: OnceLock<Option<u32>> = OnceLock::new();
    *PID.get_or_init(|| {
        std::env::var(AGENTD_PID_VAR)
            .ok()
            .and_then(|v| v.trim().parse().ok())
    })
}

// The seam of [`agentd_env_pid`]: a test sets the pid the daemon "gave" instead of the process-wide variable.
#[cfg(test)]
thread_local! {
    static AGENTD_ENV_PID: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn agentd_env_pid() -> Option<u32> {
    AGENTD_ENV_PID.with(|c| c.get())
}

#[cfg(test)]
fn set_agentd_env_pid(pid: Option<u32>) {
    AGENTD_ENV_PID.with(|c| c.set(pid));
}

/// The name of the variable that carries agentd's own executable path from the daemon, next to
/// [`AGENTD_PID_VAR`] (`manager::pipeline::start_runner`): the fallback of agentd's identity when the
/// user manager gives no usable pid. It is not on the environment allowlist, so a step never sees it.
const AGENTD_EXE_VAR: &str = "LUCINA_AGENTD_EXE";
/// The file name agentd's executable has. The infisical broker is the unit's main process for a moment
/// before it execs agentd, and its executable must never be taken for agentd's.
const AGENTD_BIN: &str = "lucina-agentd";
/// What the kernel appends to the `/proc/<pid>/exe` link of a binary a deploy has replaced.
const DELETED: &str = " (deleted)";

/// `p` without a trailing ` (deleted)`.
fn strip_deleted(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_suffix(DELETED).unwrap_or(&s))
}

/// Whether `p` (with ` (deleted)` removed) is named `lucina-agentd`.
fn is_agentd_name(p: &Path) -> bool {
    strip_deleted(p)
        .file_name()
        .is_some_and(|n| n == AGENTD_BIN)
}

/// agentd's executable as the daemon gave it at the job's start, read once, by name only.
#[cfg(not(test))]
fn agentd_env_exe() -> Option<PathBuf> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| {
        std::env::var_os(AGENTD_EXE_VAR)
            .filter(|v| !v.is_empty())
            .map(|v| strip_deleted(Path::new(&v)))
    })
    .clone()
}

// The seam of [`agentd_env_exe`].
#[cfg(test)]
thread_local! {
    static AGENTD_ENV_EXE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn agentd_env_exe() -> Option<PathBuf> {
    AGENTD_ENV_EXE.with(|c| c.borrow().clone())
}

#[cfg(test)]
fn set_agentd_env_exe(exe: Option<PathBuf>) {
    AGENTD_ENV_EXE.with(|c| *c.borrow_mut() = exe);
}

/// agentd's executable paths (at most the manager's and the daemon's), read once for the job.
#[cfg(not(test))]
static IDENTITY: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

#[cfg(not(test))]
fn identity_get() -> Vec<PathBuf> {
    IDENTITY.lock().map(|g| g.clone()).unwrap_or_default()
}

#[cfg(not(test))]
fn identity_set(v: Vec<PathBuf>) {
    if let Ok(mut g) = IDENTITY.lock() {
        *g = v;
    }
}

// In a test the identity is the test thread's own.
#[cfg(test)]
thread_local! {
    static IDENTITY: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn identity_get() -> Vec<PathBuf> {
    IDENTITY.with(|c| c.borrow().clone())
}

#[cfg(test)]
fn identity_set(v: Vec<PathBuf>) {
    IDENTITY.with(|c| *c.borrow_mut() = v);
}

/// What reading `/proc/<pid>/exe` said.
#[derive(Debug, PartialEq, Eq)]
enum ExeRead {
    /// The executable, ` (deleted)` removed.
    Path(PathBuf),
    /// The process has exited: not protected.
    Gone,
    /// Any other failure (`EACCES` and so on): the pid is refused, not guessed at.
    Unreadable(String),
}

/// `ENOENT` and `ESRCH` mean the process is gone; every other error leaves it unknown.
fn classify_exe_error(raw: Option<i32>, text: &str) -> ExeRead {
    match raw {
        Some(c) if c == Errno::NOENT.raw_os_error() || c == Errno::SRCH.raw_os_error() => {
            ExeRead::Gone
        }
        _ => ExeRead::Unreadable(text.to_string()),
    }
}

fn read_exe(pid: u32) -> ExeRead {
    match fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(p) => ExeRead::Path(strip_deleted(&p)),
        Err(e) => classify_exe_error(e.raw_os_error(), &e.to_string()),
    }
}

/// agentd's executable, from the manager's `MainPID` and from the daemon's variable. A path whose name
/// is not `lucina-agentd` is no identity from either source, and is said (the broker's own executable
/// is the unit's main process in the instant before it execs agentd). The manager's comes first; when
/// the two differ both are kept, and said.
fn resolve_exe(manager_pid: Option<u32>, notes: &mut Vec<String>) -> Vec<(PathBuf, &'static str)> {
    let mut v: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(p) = manager_pid {
        match read_exe(p) {
            ExeRead::Path(e) if is_agentd_name(&e) => v.push((e, "manager")),
            ExeRead::Path(e) => notes.push(format!(
                "the executable of MainPID {p} is {}, not {AGENTD_BIN}: refused as agentd's identity",
                e.display()
            )),
            ExeRead::Gone => notes.push(format!(
                "MainPID {p} had exited before its executable was read"
            )),
            ExeRead::Unreadable(why) => notes.push(format!(
                "the executable of MainPID {p} could not be read ({why})"
            )),
        }
    }
    if let Some(e) = agentd_env_exe() {
        if !is_agentd_name(&e) {
            notes.push(format!(
                "{AGENTD_EXE_VAR} names {}, not {AGENTD_BIN}: refused as agentd's identity",
                e.display()
            ));
        } else if !v.iter().any(|(x, _)| *x == e) {
            if let Some((m, _)) = v.first() {
                notes.push(format!(
                    "agentd's executable differs: {} from the manager, {} from the start; both are refused",
                    m.display(),
                    e.display()
                ));
            }
            v.push((e, "start"));
        }
    }
    v
}

/// Reads agentd's identity once and keeps it for the job: returns what was read, or nothing when the
/// identity is already known or still unknown (then the next sweep that has a manager answer tries again).
fn establish_identity(
    manager_pid: Option<u32>,
    notes: &mut Vec<String>,
) -> Vec<(PathBuf, &'static str)> {
    if !identity_get().is_empty() {
        return Vec::new();
    }
    let v = resolve_exe(manager_pid, notes);
    identity_set(v.iter().map(|(p, _)| p.clone()).collect());
    v
}

/// The one ask of the user manager for agentd's `MainPID`: the answer, or the note why there is none.
fn ask_main_pid(
    ctl: Option<&Path>,
    env: &[(OsString, OsString)],
    notes: &mut Vec<String>,
) -> Option<u32> {
    let Some(ctl) = ctl else {
        notes.push(
            "agentd's pid was not asked of the user manager: there is no systemctl".to_string(),
        );
        return None;
    };
    match bounded_ask(
        ctl,
        &["--user", "show", "-p", "MainPID", "--value", AGENTD_UNIT],
        env,
    ) {
        Err(why) => {
            notes.push(format!("the user manager's MainPID of {AGENTD_UNIT} {why}"));
            None
        }
        Ok(a) => match a.parse::<u32>() {
            Ok(p) if p > 0 => Some(p),
            Ok(_) => {
                notes.push(format!(
                    "the user manager answered MainPID 0 for {AGENTD_UNIT} (no answer)"
                ));
                None
            }
            Err(_) => {
                notes.push(format!(
                    "the user manager's MainPID of {AGENTD_UNIT} was not a number"
                ));
                None
            }
        },
    }
}

/// agentd's pid and where it came from: the manager's, else the daemon's when that process is alive.
fn choose_pid(from_manager: Option<u32>, notes: &mut Vec<String>) -> (Option<u32>, &'static str) {
    match (from_manager, agentd_env_pid()) {
        (Some(p), _) => (Some(p), "manager"),
        (None, Some(p)) if Path::new(&format!("/proc/{p}")).exists() => {
            notes.push(format!(
                "agentd's pid {p} is the fallback the daemon gave at the job's start ({AGENTD_PID_VAR})"
            ));
            (Some(p), "start")
        }
        (None, Some(p)) => {
            notes.push(format!(
                "agentd's pid is unknown: {AGENTD_PID_VAR} names {p}, which is not alive, so agentd is not protected beyond this process's own ancestors"
            ));
            (None, "")
        }
        (None, None) => {
            notes.push(format!(
                "agentd's pid is unknown ({AGENTD_PID_VAR} is missing or not a number), so it is not protected beyond this process's own ancestors"
            ));
            (None, "")
        }
    }
}

/// At the executor's start: asks the manager once, reads agentd's identity once, and returns the lines
/// for the job log — `agentd: pid <p> from <manager|start>, exe <path> from <manager|start>` first, then
/// whatever the reading had to say.
pub(crate) fn announce_agentd(ctl: Option<&Path>, env: &[(OsString, OsString)]) -> Vec<String> {
    let mut notes = Vec::new();
    let from_manager = ask_main_pid(ctl, env, &mut notes);
    let (pid, src) = choose_pid(from_manager, &mut notes);
    let exes = establish_identity(from_manager, &mut notes);
    let pid_s = match pid {
        Some(p) => format!("pid {p} from {src}"),
        None => "pid unknown".to_string(),
    };
    let exe_s = if exes.is_empty() {
        "exe unknown".to_string()
    } else {
        exes.iter()
            .map(|(p, s)| format!("exe {} from {s}", p.display()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![format!("agentd: {pid_s}, {exe_s}")];
    lines.extend(notes);
    lines
}

/// The refusal reason of a pid whose `/proc/<pid>/exe` link could not be read: it is spared (fail
/// closed) but it is not known to be agentd, so it still counts as left over (finding PIPE-06).
const EXE_UNREADABLE: &str = "exe unreadable";

/// Whether a pid with this refusal reason is still one of the step's processes: one that is not
/// refused, or one refused only because its executable could not be read. A pid positively matched
/// as agentd is not.
fn counts_as_left(reason: Option<&str>) -> bool {
    reason.is_none() || reason == Some(EXE_UNREADABLE)
}

/// The note a spared process that is still running gets in the job log.
fn note_unreadable(miss: &mut Miss, pid: u32) {
    miss.add_note(format!(
        "pid {pid}: {EXE_UNREADABLE}, spared and still there"
    ));
}

/// Why `target` is refused for its executable, or `None`. Runs when agentd's executable is known, and
/// when agentd is wholly unidentified (no pid, no executable): then any process named `lucina-agentd` is
/// refused. A process that has exited is not protected; a link that cannot be read is refused.
fn exe_refusal(target: u32, pid_unknown: bool) -> Option<&'static str> {
    let ident = identity_get();
    let unidentified = pid_unknown && ident.is_empty();
    if ident.is_empty() && !pid_unknown {
        return None;
    }
    match read_exe(target) {
        ExeRead::Gone => None,
        ExeRead::Unreadable(_) => Some(EXE_UNREADABLE),
        ExeRead::Path(p) => {
            if ident.contains(&p) {
                Some("agentd's executable")
            } else if unidentified && is_agentd_name(&p) {
                Some("named lucina-agentd while agentd is unidentified")
            } else {
                None
            }
        }
    }
}

// The seam between the screen of a tree and its write, in which a test changes the tree.
#[cfg(test)]
thread_local! {
    static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

fn between_screen_and_write() {
    #[cfg(test)]
    {
        let f = BETWEEN.with(|b| b.borrow_mut().take());
        if let Some(f) = f {
            f();
        }
    }
}

/// agentd's main pid and the process group it stands in, which every signal route refuses.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentdIds {
    pid: u32,
    pgid: Option<u32>,
}

impl AgentdIds {
    /// agentd's pid is not known (no process has pid 0): only its executable, if that is known, screens.
    fn unknown() -> Self {
        AgentdIds { pid: 0, pgid: None }
    }

    /// Neither agentd's pid nor its executable is known: a `cgroup.kill` cannot be screened.
    fn unidentified(&self) -> bool {
        self.pid == 0 && identity_get().is_empty()
    }

    /// Why `target` (a pid, or a group id) is agentd's, or `None`. The executable is screened on every
    /// route, whether or not the pid matches the known one.
    fn refuses(&self, target: u32) -> Option<&'static str> {
        if self.pid != 0 && target == self.pid {
            Some("agentd's main pid")
        } else if self.pgid == Some(target) {
            Some("agentd's process group")
        } else if self.pgid.is_some()
            && parent_and_group(target).map(|(_, g)| g) == self.pgid
            && !is_zombie(target)
        {
            Some("a member of agentd's process group")
        } else {
            exe_refusal(target, self.pid == 0)
        }
    }
}

/// The program `systemctl`'s answer is waited on, bounded by [`MANAGER_ASK_CAP`] and ended with
/// [`end_ask`] when it does not come: the answer, or why there is none.
fn bounded_ask(
    program: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not be asked ({e})"))?;
    let mut pipe = child.stdout.take().ok_or("had no answer pipe")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        let _ = tx.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => {
                let how = end_ask(&mut child);
                return Err(format!("could not be waited on ({e}); {how}"));
            }
        }
        if started.elapsed() >= MANAGER_ASK_CAP {
            let how = end_ask(&mut child);
            return Err(format!(
                "did not answer within {}s; {how}",
                MANAGER_ASK_CAP.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    };
    if !status.success() {
        return Err(format!("was refused ({status})"));
    }
    let left = MANAGER_ASK_CAP.saturating_sub(started.elapsed());
    let out = rx
        .recv_timeout(left)
        .map_err(|_| "was not read in time".to_string())?;
    String::from_utf8(out)
        .map(|s| s.trim().to_string())
        .map_err(|_| "was not text".to_string())
}

/// agentd's ids for `pid`, with a note when its group cannot be read: without the group only the
/// pid is protected, and that is said.
fn agentd_ids(pid: u32, notes: &mut Vec<String>) -> AgentdIds {
    let pgid = parent_and_group(pid).map(|(_, g)| g);
    if pgid.is_none() {
        notes.push(format!(
            "agentd's process group could not be read (pid {pid}); only its pid is protected"
        ));
    }
    AgentdIds { pid, pgid }
}

/// What a `cgroup.kill` may not take: the ids that make this process's kin, and agentd.
struct Protected {
    pgid: u32,
    chain: Vec<u32>,
    agentd: Option<AgentdIds>,
}

/// Why `pid` may not take a `cgroup.kill` with it, or `None` when it may: pid 1, this process, a
/// member of this process's group, this process's parent and every ancestor above it, and agentd —
/// its pid and its process group — as the user manager names it (see [`Step::guard_round`]).
fn protected_reason(pid: u32, p: &Protected) -> Option<&'static str> {
    if pid == 1 {
        return Some("pid 1");
    }
    if pid == std::process::id() {
        return Some("this process, the job's executor");
    }
    if p.chain.contains(&pid) {
        return Some("a parent of this process");
    }
    if let Some(why) = p.agentd.as_ref().and_then(|a| a.refuses(pid)) {
        return Some(why);
    }
    if parent_and_group(pid).map(|(_, g)| g) == Some(p.pgid) && !is_zombie(pid) {
        return Some("a member of this process's group");
    }
    None
}

/// The control groups under `cg` (itself included) and the pids they name, both sorted.
fn scan_cgroup(cg: &Path) -> (Vec<PathBuf>, Vec<u32>) {
    let mut pids = Vec::new();
    let mut all = Vec::new();
    let mut dirs = vec![cg.to_path_buf()];
    while let Some(d) = dirs.pop() {
        pids.extend(
            fs::read_to_string(d.join("cgroup.procs"))
                .unwrap_or_default()
                .lines()
                .filter_map(|p| p.trim().parse::<u32>().ok()),
        );
        if let Ok(rd) = fs::read_dir(&d) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        all.push(d);
    }
    all.sort();
    pids.sort_unstable();
    pids.dedup();
    (all, pids)
}

/// The tree is not written: `why`, and every pid of it that is not protected is signalled one by one
/// through the screened pid route instead.
fn refuse_tree(pids: Vec<u32>, prot: &Protected, why: String, miss: &mut Miss) {
    miss.add_cgroup(why);
    for p in pids {
        if protected_reason(p, prot).is_none() {
            signal_pid(p, Signal::KILL, miss);
        }
    }
}

/// SIGKILL of the step's control group and of every group under it, through `cgroup.kill`: no
/// fork, no pid list, and every thread of every process rather than only the leaders a pid
/// names. Safe to aim at the step's own scope because `systemd-run --scope` execs its payload
/// into it: the scope holds the step and nothing else — the executor of the job stands outside
/// it, unlike a `lucina-ci` launcher, which is why there is nothing here to gather out of it.
///
/// But a step of the daemon's own uid can write any pid it may signal into its scope's
/// `cgroup.procs`, and `cgroup.kill` takes every process of the tree: so before anything is
/// written the whole tree is read, and a tree that holds a pid this process must never kill (see
/// [`protected_reason`]) — or one this process cannot screen at all, its own ids unreadable — gets
/// no `cgroup.kill`. The refusal is a [`Miss`] that names the pid, and every other pid of the tree
/// is signalled one by one through the screened pid route instead (finding PIPE-01). The tree is
/// read again right before the write, and one that has changed since the screen (a group a step
/// made in between) is refused the same way, so the groups written are the groups screened. A pid
/// moved in the instant between that read and the write is the window the pid routes share.
///
/// A refusal is reported, not swallowed: a scope this user was not delegated answers it for
/// every write, and then the signals by group and by pid are all there is.
fn kill_cgroup_tree(cg: &Path, miss: &mut Miss) {
    let Some((_, pgid)) = own_ids() else {
        miss.add_cgroup(format!(
            "this process's own ids could not be read, so the tree {} was not swept through cgroup.kill",
            cg.display()
        ));
        // The pid route screens by the same ids, so it would refuse every one of these as well.
        return;
    };
    let prot = Protected {
        pgid,
        chain: ancestors(),
        agentd: miss.guard,
    };
    if miss.guard.is_some_and(|a| a.unidentified()) {
        // PIPE-05: with neither agentd's pid nor its executable known, no tree can be shown clear of
        // agentd, so none is written: the pids go one by one through the screened route, which also
        // refuses any process named `lucina-agentd`.
        miss.add_note("agentd unidentified: no cgroup.kill".to_string());
        for p in scan_cgroup(cg).1 {
            if protected_reason(p, &prot).is_none() {
                signal_pid(p, Signal::KILL, miss);
            }
        }
        return;
    }
    let refusal = |pids: &[u32]| {
        pids.iter()
            .find_map(|p| protected_reason(*p, &prot).map(|why| (*p, why)))
            .map(|(pid, why)| {
                format!(
                    "pid {pid} ({why}) is in the tree {}, so it was not swept through cgroup.kill; its other pids were signalled one by one",
                    cg.display()
                )
            })
    };
    let (dirs, pids) = scan_cgroup(cg);
    if let Some(why) = refusal(&pids) {
        return refuse_tree(pids, &prot, why, miss);
    }
    between_screen_and_write();
    let (again, pids) = scan_cgroup(cg);
    if again != dirs {
        let why = format!(
            "the tree {} changed between the screen and the write, so it was not swept through cgroup.kill; its pids were signalled one by one",
            cg.display()
        );
        return refuse_tree(pids, &prot, why, miss);
    }
    if let Some(why) = refusal(&pids) {
        return refuse_tree(pids, &prot, why, miss);
    }
    for d in dirs {
        let f = d.join("cgroup.kill");
        if let Err(e) = fs::write(&f, b"1\n") {
            // A kernel without `cgroup.kill`, or a scope not delegated to this user.
            miss.add_cgroup(format!("{e} ({})", f.display()));
        }
    }
}

/// Where one step's processes are: the group it leads and, when it got one, the scope it stands
/// in. `cg` stays `None` until the step is *seen* there: `systemd-run` connects to the bus and
/// creates the unit before it execs, so the pid's own `/proc` entry names the manager's cgroup
/// for a moment first, and a cgroup that is not the one this name asked for is never signalled.
/// When the step ended inside that moment there is no live pid left to read it from, and the
/// manager is asked for the unit's `ControlGroup` instead ([`Step::control_group_from_manager`]).
pub(crate) struct Step {
    pgid: u32,
    unit: Option<String>,
    cg: Option<PathBuf>,
    /// `systemctl`, as the job's PATH found it: `None` is no manager to ask, which the step reports
    /// rather than guesses at.
    ctl: Option<PathBuf>,
    /// The environment a step of this job gets — cleared, plus the job's own `PATH` — so the one
    /// ask of the user manager runs under it, as the probe and the step do (`super::exec`). Without
    /// it that child would inherit this process's environment: an unmanaged surface in a module
    /// whose whole reason is to manage what a step can reach.
    env: Vec<(OsString, OsString)>,
    /// Whether the manager has already been asked for this step's unit, so that one ask is enough.
    asked: bool,
    /// What the log should say when the one ask of the manager was ended for want of an answer.
    manager_note: Option<String>,
    /// agentd's ids as of the last ask of the user manager, the notes that ask left, and when it was.
    agentd: Option<AgentdIds>,
    agentd_notes: Vec<String>,
    agentd_at: Option<Instant>,
    /// A step that waits for a bq slot before its work: its deadline counts from the grant.
    grant: Option<Grant>,
}

/// The grant a step that queues for a bq slot waits for (LA-C038, WORK-26). `marker` is a file the
/// wrapped job writes first, once bq has granted the slot and started it; until it exists the step's
/// deadline has not started, and the step's wait stays bounded by bq's own `--max-wait`. The marker
/// is in the job's own directory, which the executor owns: a step that forges it only starts its own
/// clock early. `seen` is told, once, the slot's name (the marker's text) and the time since the
/// step was started.
pub(crate) struct Grant {
    pub(crate) marker: PathBuf,
    pub(crate) seen: Box<dyn FnMut(&str, Duration)>,
}

impl Step {
    /// The step's deadline counts from the grant of a bq slot, not from its start.
    pub(crate) fn with_grant(mut self, grant: Grant) -> Self {
        self.grant = Some(grant);
        self
    }

    /// The step whose direct child is `pid`, asked into unit `unit` (`None`: no scope, the group
    /// alone is what this step owns), with `ctl` the `systemctl` the job can ask.
    pub(crate) fn new(pgid: u32, unit: Option<String>, ctl: Option<PathBuf>) -> Self {
        Step {
            grant: None,
            pgid,
            unit,
            cg: None,
            ctl,
            env: Vec::new(),
            asked: false,
            manager_note: None,
            agentd: None,
            agentd_notes: Vec::new(),
            agentd_at: None,
        }
    }

    /// The environment the one manager ask runs under: cleared, plus what is given here (the job's own
    /// `PATH` and the rest of what a step gets), as the probe and the step do in `super::exec`.
    pub(crate) fn with_env(mut self, env: Vec<(OsString, OsString)>) -> Self {
        self.env = env;
        self
    }

    /// Whether the scope question is answered for this step: the control group of the unit we
    /// named has been found, or there was never a unit to find.
    pub(crate) fn cgroup_known(&self) -> bool {
        self.unit.is_none() || self.cg.is_some()
    }

    /// Read the step's own `/proc` entry and take it as the scope's control group, but only when
    /// the name at the end of its path is the unit this step asked for. A step that never landed
    /// in that unit stands in the control group of whatever started the job — the daemon, a `bq`
    /// job, the test binary and every other step in it — and that group is never ours to kill.
    pub(crate) fn look_for_cgroup(&mut self, pid: u32) {
        let Some(want) = self.unit.as_ref().map(|u| format!("{u}.scope")) else {
            return;
        };
        // The seam of the scope look: with it on, the step's own `/proc` entry names nothing, which is
        // how a test stands in the gap between `systemd-run` starting and its exec into the unit.
        if blind_to_cgroups() {
            return;
        }
        let Ok(t) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return;
        };
        for line in t.lines() {
            let Some(path) = line.strip_prefix("0::") else {
                continue;
            };
            if path.rsplit('/').next() == Some(want.as_str()) {
                self.cg = Some(PathBuf::from(format!("/sys/fs/cgroup{path}")));
                return;
            }
        }
    }

    /// The unit's control group as the user manager names it, for the step whose direct child was gone
    /// before any read of its `/proc` entry saw the scope: a script that does `setsid … &` and exits
    /// inside the moment `systemd-run` takes to move into its unit leaves no live pid to read the
    /// answer from, and the child it started is out of reach of the group alone. Asked of the unit
    /// *this* step named and no other — the control group of somebody else's unit is not this step's
    /// tree to sweep.
    ///
    /// The ask is bounded by [`MANAGER_ASK_CAP`] and runs under the cleared environment a step gets:
    /// it happens in the step's own wait, before any signal and again before the sweep, so a
    /// `systemctl` that stops answering must not hold the job past every bound this module has. The
    /// cap is its own — the step's deadline is not, because by the time the ask runs the step may
    /// already have ended and there is nothing to signal. A manager that does not answer in time is
    /// ended (SIGKILL to that child alone, through the same screen every other signal goes through)
    /// and read as "no answer", which is the honest reading: a control group nobody stated is not a
    /// tree this step may sweep.
    fn control_group_from_manager(&mut self) -> Option<PathBuf> {
        let (unit, program) = match (&self.unit, &self.ctl) {
            (Some(u), Some(p)) => (u.clone(), p.clone()),
            _ => return None,
        };
        let mut child = Command::new(&program)
            .args([
                "--user",
                "show",
                "-p",
                "ControlGroup",
                "--value",
                &format!("{unit}.scope"),
            ])
            .env_clear()
            .envs(self.env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // The answer is read on a thread of its own, so that waiting for the process never depends on
        // the pipe closing: an ask that is ended must not leave this wait reading for a descendant.
        let mut pipe = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            let _ = tx.send(buf);
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(e) => {
                    let how = end_ask(&mut child);
                    self.manager_note = Some(format!(
                        "the user manager's ask for the control group of {unit} could not be waited on ({e}); {how}"
                    ));
                    return None;
                }
            }
            if started.elapsed() >= MANAGER_ASK_CAP {
                let how = end_ask(&mut child);
                self.manager_note = Some(format!(
                    "the user manager did not answer the ask for the control group of {unit} within {}s; {how}",
                    MANAGER_ASK_CAP.as_secs()
                ));
                return None;
            }
            std::thread::sleep(POLL);
        };
        if !status.success() {
            return None;
        }
        // The read is inside the same cap as the process: what is left of it, and no more.
        let left = MANAGER_ASK_CAP.saturating_sub(started.elapsed());
        let Ok(out) = rx.recv_timeout(left) else {
            self.manager_note = Some(format!(
                "the user manager did not answer the ask for the control group of {unit} within {}s; its answer was not read in time",
                MANAGER_ASK_CAP.as_secs()
            ));
            return None;
        };
        let stdout_str = String::from_utf8(out).ok()?;
        let cg = stdout_str.trim();
        // The same screen the `/proc` route applies, and for the same reason: a control group is
        // about to be walked and killed, so it is only ever the one this step's unit ends in. An
        // alias, a template, or a manager that answers with something broader is no answer at all.
        if !cg.starts_with('/') || Path::new(cg).file_name()?.to_str()? != format!("{unit}.scope") {
            return None;
        }
        // And no segment may be `..` or `.`: a path that climbs out of the cgroup tree, or wanders in it,
        // is not this step's tree, whatever its last component says. Asked of the raw answer, because
        // `Path::components` drops a `.` from an absolute path and would never show it.
        if cg.split('/').any(|s| s == "." || s == "..") {
            return None;
        }
        Some(PathBuf::from(format!("/sys/fs/cgroup{cg}")))
    }

    /// That the step's control group is known, by either route: it is asked of the manager once, and
    /// never again for a step that has already been answered or already failed to be answered there.
    fn recover_control_group(&mut self) {
        if self.cg.is_some() || self.unit.is_none() || self.asked {
            return;
        }
        self.asked = true;
        self.cg = self.control_group_from_manager();
    }

    /// Every process the step still holds: its group's members, and its scope tree's.
    pub(crate) fn survivors(&self) -> Vec<u32> {
        let mut v = group_pids(self.pgid);
        if let Some(cg) = &self.cg {
            v.extend(cgroup_pids(cg));
        }
        if let Some(a) = &self.agentd {
            v.retain(|p| counts_as_left(a.refuses(*p)));
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The deadline's first blow: the group and the scope's pids get SIGTERM, so a step that can
    /// clean up after itself still can.
    pub(crate) fn terminate(&mut self) -> Miss {
        let mut miss = self.round(true);
        signal_group(self.pgid, Signal::TERM, &mut miss);
        for pid in self.scope_pids() {
            signal_pid(pid, Signal::TERM, &mut miss);
        }
        miss
    }

    /// The second blow and every round after it: SIGTERM to nothing and SIGKILL to everything —
    /// the group, the pids of the scope's tree, and `cgroup.kill` of that tree. The group goes
    /// first because it is the only route that reaches work the step handed to a nested scope,
    /// which stands in a sibling control group no list of ours walks.
    pub(crate) fn kill(&mut self) -> Miss {
        let mut miss = self.round(false);
        signal_group(self.pgid, Signal::KILL, &mut miss);
        for pid in self.scope_pids() {
            signal_pid(pid, Signal::KILL, &mut miss);
        }
        if let Some(cg) = &self.cg {
            kill_cgroup_tree(cg, &mut miss);
        }
        miss
    }

    /// Ask the user manager for agentd's main pid, which a step cannot change (the pid of the same
    /// name in the executor's own environment goes stale when agentd restarts under a job unit that
    /// outlives it, so it is only the fallback when the manager does not answer; `MainPID` 0 is no
    /// answer). The order of the sources is: the user manager's `MainPID`, then
    /// `LUCINA_AGENTD_PID` if that process is alive, then unknown — and each way down says so in a
    /// note. Only asked when the scope's tree names a pid at all, and again once the answer is a
    /// second old, or at once when `force`: so every sweep starts from a fresh answer.
    fn guard_round(&mut self, force: bool) {
        let Some(cg) = &self.cg else { return };
        if raw_cgroup_pids(cg).is_empty() {
            return;
        }
        if !force && self.agentd_at.is_some_and(|t| t.elapsed() < AGENTD_REFRESH) {
            return;
        }
        self.agentd_at = Some(Instant::now());
        let mut notes = Vec::new();
        let from_manager = ask_main_pid(self.ctl.as_deref(), &self.env, &mut notes);
        let (pid, _) = choose_pid(from_manager, &mut notes);
        // The identity is read once for the job, from the first manager answer; a sweep that finds it
        // still unknown tries again.
        if identity_get().is_empty() {
            let _ = establish_identity(from_manager, &mut notes);
        }
        self.agentd = Some(match pid {
            Some(p) => agentd_ids(p, &mut notes),
            None => AgentdIds::unknown(),
        });
        self.agentd_notes = notes;
    }

    /// The [`Miss`] a round of signals starts from: carrying agentd's ids, so that every signal of
    /// the round is screened against them, and the notes about where they came from.
    fn round(&mut self, force: bool) -> Miss {
        self.guard_round(force);
        let mut miss = Miss {
            guard: Some(self.agentd.unwrap_or_else(AgentdIds::unknown)),
            ..Miss::default()
        };
        for n in self.agentd_notes.clone() {
            miss.add_note(n);
        }
        miss
    }

    fn scope_pids(&self) -> Vec<u32> {
        self.cg.as_deref().map(cgroup_pids).unwrap_or_default()
    }

    /// That nothing of the step is left, however the step ended: a shell that exits on its own
    /// still leaves a background job behind sometimes, and `cargo` still holds its lock while it
    /// does. SIGTERM, then the grace, then SIGKILL rounds, all of it bounded. Returns what the
    /// last round could not deliver and how many processes were still there when it gave up.
    fn reap(&mut self, grace: Duration) -> (Miss, usize) {
        let start = Instant::now();
        self.guard_round(true);
        let mut left = self.survivors();
        if left.is_empty() {
            return (Miss::default(), 0);
        }
        // The stragglers of a step that ended by itself get the same first blow the deadline
        // gives: one that is only lagging behind answers SIGTERM and cleans up after itself.
        let mut miss = self.terminate();
        loop {
            std::thread::sleep(TICK);
            left = self.survivors();
            let e = start.elapsed();
            if left.is_empty() {
                break;
            }
            if e >= grace {
                miss = self.kill();
            }
            if e >= grace + SLACK {
                left = self.survivors();
                break;
            }
        }
        if let Some(a) = &self.agentd {
            for p in &left {
                if a.refuses(*p) == Some(EXE_UNREADABLE) {
                    note_unreadable(&mut miss, *p);
                }
            }
        }
        (miss, left.len())
    }
}

/// Ends the manager ask that did not answer: SIGKILL to that child alone, through the same screen
/// every other signal goes through, then a reap bounded by [`SLACK`] — never an unbounded `wait`.
/// What it returns is what actually happened, for the job log: a refusal is kept and named (the
/// module rule above), and a child that is still there is said to be left running.
fn end_ask(child: &mut Child) -> String {
    let mut miss = Miss::default();
    signal_pid(child.id(), Signal::KILL, &mut miss);
    let until = Instant::now() + SLACK;
    let reaped = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            _ => break false,
        }
    };
    match (miss.is_empty(), reaped) {
        (true, true) => "killed".to_string(),
        (true, false) => format!(
            "killed, but not reaped within {}s; left running",
            SLACK.as_secs()
        ),
        (false, _) => format!(
            "could not be signalled: {}; {}",
            miss.line(),
            if reaped {
                "it ended by itself"
            } else {
                "left running"
            }
        ),
    }
}

/// How a step's wait ended, which [`super::exec`] turns into the log lines and the outcome.
pub(crate) struct Ended {
    /// The step's exit status, or `None` when the hard bound passed with the step still running
    /// and it was left alone.
    pub(crate) status: Option<ExitStatus>,
    /// Its deadline passed before it ended.
    pub(crate) timed_out: bool,
    /// What the last round of signals could not deliver.
    pub(crate) miss: Miss,
    /// How many of the step's processes were still there when the signals gave up.
    pub(crate) left: usize,
    /// Whether the unit it was asked into was ever seen, and what the job therefore says about it.
    pub(crate) scope: Sighting,
    /// Set when the user manager did not answer its one ask in time, and was ended.
    pub(crate) manager_note: Option<String>,
    /// A step with a [`Grant`]: how long after its start the slot was granted, `None` when it never was.
    pub(crate) granted_after: Option<Duration>,
}

impl Step {
    /// The step's whole life, from its start to the last signal: watch `child` until it ends or
    /// `cap` passes, TERM at the deadline, the `grace`, then KILL in rounds — and when the step
    /// has ended, the same rounds again for everything it left behind, so a step that ran to its
    /// own conclusion is ended *whole* as well. Nothing is waited on past `cap + grace + SLACK`: a
    /// step stuck where no signal reaches it is reported, not held for.
    pub(crate) fn run_to_end(mut self, child: &mut Child, cap: Duration, grace: Duration) -> Ended {
        let pid = child.id();
        let start = Instant::now();
        let mut timed_out = false;
        let mut miss = Miss::default();
        let mut left = 0;
        // A step that waits for a bq slot has its deadline from the grant; one that does not, from its start.
        let mut granted_at: Option<Instant> = None;
        let status = loop {
            let looked = start.elapsed() < SCOPE_LOOK;
            if !self.cgroup_known() && looked {
                self.look_for_cgroup(pid);
            }
            // Looked at before the wait on the child, so a step that ends in the tick it was granted in is seen granted.
            if let Some(g) = self
                .grant
                .as_mut()
                .filter(|g| granted_at.is_none() && g.marker.is_file())
            {
                granted_at = Some(Instant::now());
                let slot = fs::read_to_string(&g.marker).unwrap_or_default();
                (g.seen)(slot.trim(), start.elapsed());
            }
            if let Ok(Some(s)) = child.try_wait() {
                break Some(s);
            }
            let e = match (&self.grant, granted_at) {
                (None, _) => start.elapsed(),
                (Some(_), Some(at)) => at.elapsed(),
                (Some(_), None) => Duration::ZERO,
            };
            // The window in which the step's own `/proc` entry could still have named the unit is over and
            // it named nothing: ask the manager, whose answer does not need a live process of the step.
            if !looked && !self.cgroup_known() {
                self.recover_control_group();
            }
            if e >= cap && !timed_out {
                timed_out = true;
                miss = self.terminate();
            } else if timed_out && e >= cap + grace {
                miss = self.kill();
            }
            if e >= cap + grace + SLACK {
                left = self.survivors().len();
                break None;
            }
            std::thread::sleep(if self.cgroup_known() || !looked {
                POLL
            } else {
                POLL_EARLY
            });
        };
        if status.is_some() {
            // Ours is reaped, so nothing of the step is left to wait for — but what it forked may
            // be, and that is what the sweep is for. Its refusals are the last word on this step,
            // so they are what gets reported. A step that ended before its scope was ever seen gives
            // the manager its one ask first: without that tree the sweep reaches the group alone, and a
            // child that `setsid` away from the group is not in it.
            self.recover_control_group();
            let (sweep_miss, still_there) = self.reap(grace);
            if !sweep_miss.says_nothing() {
                miss = sweep_miss;
            }
            left = still_there;
        }
        Ended {
            status,
            timed_out,
            miss,
            left,
            scope: match &self.unit {
                None => Sighting::NoUnit,
                Some(u) if self.cg.is_some() => Sighting::Seen(u.clone()),
                Some(u) => Sighting::NeverSeen(u.clone()),
            },
            manager_note: self.manager_note.take(),
            granted_after: granted_at.map(|at| at.duration_since(start)),
        }
    }
}

/// What became of the scope a step was asked into, which is what [`super::exec`] says about it. A unit
/// named is a fact about the step's processes only once its control group has been read, from the live
/// child or from the manager; before that the job says nothing, and a step that ends with the group
/// still naming the manager's own cgroup says that a child which left it was out of reach (finding
/// LA-R059, item 3).
#[derive(Debug)]
pub(crate) enum Sighting {
    /// No unit was asked for, so the line said at the step's start already told the whole of it.
    NoUnit,
    Seen(String),
    NeverSeen(String),
}

impl Sighting {
    /// The log line the step's end gets, or `None` for the case with nothing further to say.
    pub(crate) fn line(&self) -> Option<String> {
        match self {
            Sighting::NoUnit => None,
            Sighting::Seen(u) => Some(format!("scope: {u}")),
            Sighting::NeverSeen(u) => Some(format!(
                "scope: {u} was never seen; a child that left the process group was not reachable"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The bookkeeping of the two routes, apart from a job: what a step does when a signal cannot be
    //! screened, and where its control group comes from when its own `/proc` entry never named it.
    //!
    //! Every process here is one the test started and holds the pid of, and no signal in this file
    //! goes anywhere else.

    use super::*;
    use crate::testutil::write_exec;
    use std::os::unix::process::CommandExt;

    /// Ends `child` and reaps it, whatever the test below did or failed to do before it got there.
    struct Reap(Child);

    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The screen of [`signal_pid`] and [`signal_group`] is read from this process's own `/proc`
    /// entry: without it there is nothing to tell a step's grandchild apart from the daemon's group,
    /// and between those two lies a `kill` and the process holding the job (finding LA-R059, item 4).
    /// So an unscreenable target is left alone and the round says so — and a failed read is not kept
    /// as a permanent screen, or one unreadable moment would end the killing for every step after it.
    #[test]
    fn a_signal_that_cannot_be_screened_is_not_aimed() {
        let child = Command::new("/bin/sleep")
            .arg("300")
            .spawn()
            .expect("sleep starts");
        let pid = child.id();
        let mut child = Reap(child);

        let mut miss = Miss::default();
        refuse_own_ids(true);
        signal_pid(pid, Signal::TERM, &mut miss);
        signal_group(pid, Signal::TERM, &mut miss);
        refuse_own_ids(false);

        assert!(
            child.0.try_wait().expect("waitpid works").is_none(),
            "pid {pid} ended, so a signal the screen was meant to hold back went through"
        );
        let line = miss.line();
        assert_eq!(miss.processes, 2, "both rounds should have refused: {line}");
        assert!(
            line.contains("this process's own ids could not be read"),
            "the refusal does not say why nothing was signalled: {line}"
        );
        assert!(line.contains(&format!("{pid} was not signalled")), "{line}");

        // Not cached as this process's screen: the ids are asked again, and the group this process
        // stands in is refused once more. A predicate only — nothing is aimed here, because a screen
        // that has forgotten its own group is a `kill` aimed at the test binary and everything else
        // in the job.
        let own = rustix::process::getpgrp().as_raw_pid() as u32;
        let mut screen = Miss::default();
        assert!(
            !signalable(own, &mut screen),
            "pid {own} is this process's own group: a screen still holding the failed read cannot \
             tell it from a step's process, and would signal it"
        );
        assert!(
            screen.is_empty(),
            "the own group answered with a refusal of a signal that was never aimed: {}",
            screen.line()
        );

        // With the ids readable again the same target is signalled and nothing is reported: this is
        // the same `signal_pid` call as the one refused above.
        let mut after = Miss::default();
        signal_pid(pid, Signal::TERM, &mut after);
        assert!(
            after.is_empty(),
            "a read that failed once screened every signal after it: {}",
            after.line()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(s) = child.0.try_wait().expect("waitpid works") {
                break s;
            }
            if Instant::now() >= deadline {
                panic!("pid {pid} took no notice of the SIGTERM the screen no longer holds back");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            status.code().is_none(),
            "sleep ends on the SIGTERM, not by itself: {status}"
        );
    }

    /// A step whose script runs `setsid … &` and exits inside the moment `systemd-run` takes to move
    /// into its unit leaves no live process to read the unit's name from, and the group signal does
    /// not reach a child that has left the group. The control group is still a fact the manager can
    /// state, so it is asked — of this step's unit and of no other (finding LA-R059, item 3).
    #[test]
    fn a_control_group_never_read_from_proc_is_asked_of_the_manager() {
        let dir = tempfile::tempdir().unwrap();
        let asked = dir.path().join("asked");
        let ctl = dir.path().join("systemctl");
        let unit = "lucina-kill-test-unit-1";
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' /lucina-kill-test-fake/{unit}.scope\n",
                asked.display()
            ),
        );
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);

        let asked = fs::read_to_string(&asked)
            .expect("the step ended with its scope unread and never asked the manager for it");
        let unit_scope = format!("{unit}.scope");
        assert_eq!(
            asked.lines().collect::<Vec<_>>(),
            vec![
                "--user",
                "show",
                "-p",
                "ControlGroup",
                "--value",
                unit_scope.as_str(),
            ],
            "the ask is `systemctl --user show` of ControlGroup, of the unit this step named and no \
             other, and it is one ask"
        );
        match &ended.scope {
            Sighting::Seen(u) => assert_eq!(u, unit),
            other => {
                panic!("a scope the manager named is a seen scope, and this one reported {other:?}")
            }
        }
        assert_eq!(
            ended.scope.line().as_deref(),
            Some(format!("scope: {unit}").as_str()),
            "and the job says the unit as the fact it now is"
        );
        assert_eq!(ended.left, 0, "{:?}", ended.status);
    }

    /// The same recovery on the other side of the step's life: a step that is *still running* when the window
    /// for reading its own `/proc` entry closes is asked of the manager then, before any signal goes out, so
    /// the TERM and the KILL have the tree to work on and not the group alone. Which of the two happened
    /// first is what the step's own trap and the ask write into one file, in the order they landed (finding
    /// LA-R059, item 3).
    #[test]
    fn a_step_still_running_when_the_scope_look_ended_is_asked_before_any_signal() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record");
        let ctl = dir.path().join("systemctl");
        let unit = "lucina-kill-test-unit-4";
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\nprintf 'asked\\n' >> '{}'\nprintf '%s\\n' /lucina-kill-test-fake/{unit}.scope\n",
                record.display()
            ),
        );
        // A group of its own, so the signal at its deadline is aimed at a group that exists and reaches the
        // trap, which writes the second record and leaves. `echo` and not `printf` with an escape: outside
        // quotes the shell would eat the backslash and the word would land as `signalledn`.
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "trap 'echo signalled >> {}; exit' TERM; sleep 30",
                record.display()
            ))
            .process_group(0)
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        // The deadline is past the two seconds for which the step's own `/proc` entry is the route, so the
        // ask has a moment to be the thing that happens while the step is still alive.
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(3),
            Duration::from_millis(200),
        );
        refuse_cgroup_sight(false);

        let record = fs::read_to_string(&record).expect(
            "the step ran past the scope look and never asked the manager for its control group",
        );
        let lines: Vec<&str> = record.lines().collect();
        assert_eq!(
            lines.first(),
            Some(&"asked"),
            "the manager is asked while the step is still running, and before the deadline signals it: {lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| **l == "asked").count(),
            1,
            "one ask for one step: the recovery while it runs and the one before the reap are the same \
             question and it is asked once: {lines:?}"
        );
        assert!(
            lines.contains(&"signalled"),
            "the step's own trap never ran, so this watched nothing being signalled: {lines:?}"
        );
        assert!(
            matches!(&ended.scope, Sighting::Seen(u) if u == unit),
            "the unit asked of the manager is the scope the step is in: {:?}",
            ended.scope
        );
    }

    /// The manager answers with a control group, but not the one this step's unit ends in: an alias,
    /// a template, something broader. A control group is about to be walked and killed, so an answer
    /// that is not this unit's is no answer, and the step keeps the line that says so.
    #[test]
    fn an_answer_that_is_not_this_units_control_group_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        write_exec(
            &ctl,
            "#!/bin/sh\nprintf '%s\\n' /lucina-kill-test-fake/lucina-kill-test-unit-other.scope\n",
        );
        let unit = "lucina-kill-test-unit-3";
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);

        assert!(
            matches!(&ended.scope, Sighting::NeverSeen(u) if u == unit),
            "a control group of somebody else's unit is not this step's tree to sweep: {:?}",
            ended.scope
        );
    }

    /// The other answer from the manager: no such unit, or no manager to ask. The step is worked by
    /// its process group alone, and that is what the job has to say — not the unit's name as if it
    /// had held the step's processes.
    #[test]
    fn a_scope_neither_proc_nor_the_manager_named_says_what_was_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let asks = dir.path().join("asks");
        let ctl = dir.path().join("systemctl");
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\nn=$(cat '{}' 2>/dev/null || echo 0)\necho $((n + 1)) > '{}'\nexit 1\n",
                asks.display(),
                asks.display()
            ),
        );
        let unit = "lucina-kill-test-unit-2";
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);

        let asks = fs::read_to_string(&asks).expect("the manager was never asked");
        assert_eq!(
            asks.trim(),
            "1",
            "one refusal is enough for one step; it is not asked again at every poll"
        );
        match &ended.scope {
            Sighting::NeverSeen(u) => assert_eq!(u, unit),
            other => panic!("a scope nobody named is not a scope this step had: {other:?}"),
        }
        assert_eq!(
            ended.scope.line().as_deref(),
            Some(format!(
                "scope: {unit} was never seen; a child that left the process group was not reachable"
            )
            .as_str()),
            "the line the job gets has to name the reach that is missing, not just the unit"
        );
    }

    /// The step with no unit at all has nothing to see and nothing to ask: its group is the whole of
    /// it, and the line said at its start already told the job so.
    #[test]
    fn a_step_without_a_unit_has_no_scope_to_report() {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);
        let ended = Step::new(pid, None, None).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        assert!(matches!(ended.scope, Sighting::NoUnit));
        assert_eq!(ended.scope.line(), None);
        assert!(
            ended.status.is_some(),
            "the step ended, and its status is its own"
        );
    }

    /// A `systemctl` that stops answering must not hold the step past every bound this module has:
    /// the ask has its own cap ([`MANAGER_ASK_CAP`]), and a manager that does not answer in time is
    /// ended (SIGKILL through [`signal_pid`]) and read as "no answer". The step still ends within
    /// `cap + grace + SLACK + MANAGER_ASK_CAP`, and the scope is reported as never seen.
    #[test]
    fn a_manager_ask_that_does_not_answer_is_capped_and_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        // A fake systemctl that sleeps 60 s: past the cap by a wide margin.
        write_exec(&ctl, "#!/bin/sh\nexec sleep 60\n");
        let unit = "lucina-kill-test-unit-cap";
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let start = Instant::now();
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);
        let elapsed = start.elapsed();

        // The step ends within cap + grace + SLACK + MANAGER_ASK_CAP (5 + 0.1 + 2 + 5 = 12.1 s).
        // Allow a generous margin for the test harness itself.
        let bound = Duration::from_secs(13);
        assert!(
            elapsed < bound,
            "the step took {elapsed:?}, past the bound of {bound:?}",
        );

        assert!(
            ended
                .manager_note
                .as_deref()
                .is_some_and(|n| n.contains("did not answer")),
            "the log must say the manager did not answer: {:?}",
            ended.manager_note
        );
        // The manager did not answer: the scope is never seen, and the step was worked by its group alone.
        assert!(
            matches!(&ended.scope, Sighting::NeverSeen(u) if u == unit),
            "a manager that does not answer in time is read as no answer: {:?}",
            ended.scope
        );
    }

    /// A control group the manager answers with a `..` component is refused: it climbs out of the
    /// scope's own directory, and whatever its last component says, it is not this step's tree to
    /// sweep. The `/proc` route reads a path it built itself and cannot carry one; an answer from
    /// the manager can, and it is refused here.
    #[test]
    fn a_manager_answer_with_a_dotdot_component_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        let unit = "lucina-kill-test-unit-dd";
        // The fake systemctl answers with a path that contains `..` but whose last component is
        // still the unit's scope name: the file_name check passes, but the `..` must be caught.
        write_exec(
            &ctl,
            &format!("#!/bin/sh\nprintf '%s\\n' /sys/fs/cgroup/../x/{unit}.scope\n"),
        );
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);

        assert!(
            matches!(&ended.scope, Sighting::NeverSeen(u) if u == unit),
            "a control group with a `..` component is not this step's tree: {:?}",
            ended.scope
        );
    }

    /// The one ask of the manager runs under the cleared environment a step gets, plus what the job
    /// hands it, and nothing of this process's own. The fake `systemctl` names the scope only when it
    /// sees no `HOME` and the one variable the step was given.
    #[test]
    fn the_manager_ask_runs_under_a_cleared_environment() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        let unit = "lucina-kill-test-unit-env";
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\nif [ -z \"${{HOME+x}}\" ] && [ \"$LUCINA_ASK_MARK\" = given ]; then printf '%s\\n' /user.slice/{unit}.scope; else exit 1; fi\n"
            ),
        );
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl))
            .with_env(vec![("LUCINA_ASK_MARK".into(), "given".into())])
            .run_to_end(
                &mut child.0,
                Duration::from_secs(5),
                Duration::from_millis(100),
            );
        refuse_cgroup_sight(false);

        assert!(
            matches!(&ended.scope, Sighting::Seen(u) if u == unit),
            "the ask must see no HOME and the variable it was given: {:?}",
            ended.scope
        );
    }

    /// When the ask does not answer and the screen refuses the signal that would end it (this process's
    /// own ids unreadable), the step still ends within its bound — the reap is bounded, not a `wait` —
    /// and the log names the refusal and says the ask was left running, not that it was killed.
    #[test]
    fn a_manager_ask_that_cannot_be_signalled_is_bounded_and_names_the_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        let pidfile = dir.path().join("ask-pid");
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 20\n",
                pidfile.display()
            ),
        );
        let unit = "lucina-kill-test-unit-nosig";
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        refuse_own_ids(true);
        let start = Instant::now();
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        let elapsed = start.elapsed();
        refuse_own_ids(false);
        refuse_cgroup_sight(false);

        // The ask is the one thing this test started: end it now, whatever the asserts say.
        if let Some(p) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
            .and_then(Pid::from_raw)
        {
            let _ = kill_process(p, Signal::KILL);
        }

        let bound = MANAGER_ASK_CAP + SLACK + Duration::from_secs(3);
        assert!(
            elapsed < bound,
            "the step took {elapsed:?}, past the bound of {bound:?}"
        );
        let note = ended.manager_note.unwrap_or_default();
        assert!(note.contains("did not answer"), "{note}");
        assert!(note.contains("could not be signalled"), "{note}");
        assert!(note.contains("own ids could not be read"), "{note}");
        assert!(note.contains("left running"), "{note}");
    }

    /// `.` cannot show up in the components of an absolute path, so the check is on the raw answer: a
    /// `/./` segment is refused as `..` is.
    #[test]
    fn a_manager_answer_with_a_dot_segment_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("systemctl");
        let unit = "lucina-kill-test-unit-dot";
        write_exec(
            &ctl,
            &format!("#!/bin/sh\nprintf '%s\\n' /user.slice/./x/{unit}.scope\n"),
        );
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("the step starts");
        let pid = child.id();
        let mut child = Reap(child);

        refuse_cgroup_sight(true);
        let ended = Step::new(pid, Some(unit.into()), Some(ctl)).run_to_end(
            &mut child.0,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        refuse_cgroup_sight(false);

        assert!(
            matches!(&ended.scope, Sighting::NeverSeen(u) if u == unit),
            "a control group with a `.` segment is not this step's tree: {:?}",
            ended.scope
        );
    }
    /// A fake scope tree: a directory with a `cgroup.procs` naming `pids`, and a subdirectory with
    /// its own naming `sub_pids`. `cgroup.kill` is whatever the code under test writes there, so its
    /// absence is the proof that nothing was written.
    fn fake_scope(dir: &Path, pids: &[u32], sub_pids: &[u32]) -> PathBuf {
        let cg = dir.join("fake.scope");
        let sub = cg.join("inner");
        fs::create_dir_all(&sub).unwrap();
        write_procs(&cg, pids);
        write_procs(&sub, sub_pids);
        cg
    }

    fn write_procs(dir: &Path, pids: &[u32]) {
        let list = pids.iter().map(|p| format!("{p}\n")).collect::<String>();
        fs::write(dir.join("cgroup.procs"), list).unwrap();
    }

    fn wait_gone(child: &mut Child, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("waitpid works").is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "{what} was never signalled");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A `sleep` in a group of its own, so that it is nobody's kin by group.
    fn sleeper() -> Reap {
        Reap(
            Command::new("/bin/sleep")
                .arg("300")
                .process_group(0)
                .spawn()
                .expect("sleep starts"),
        )
    }

    /// agentd's ids as the user manager would give them for the process `pid`.
    fn agentd_of(pid: u32) -> AgentdIds {
        AgentdIds {
            pid,
            pgid: parent_and_group(pid).map(|(_, g)| g),
        }
    }

    /// The pid of a process that has ended and been reaped: a step's group with nothing in it.
    fn gone_pid() -> u32 {
        let mut c = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    /// A `systemctl` that answers `MainPID` with the first line of `answer`, a file the test rewrites
    /// between sweeps. `read` and `printf` are the shell's own: the step's environment is cleared.
    fn fake_systemctl(dir: &Path, answer: &Path) -> PathBuf {
        let ctl = dir.join("systemctl");
        write_exec(
            &ctl,
            &format!(
                "#!/bin/sh\nread p < '{}'\nprintf '%s\\n' \"$p\"\n",
                answer.display()
            ),
        );
        ctl
    }

    /// PIPE-01: a step of the daemon's uid can write the daemon's pid into its scope's
    /// `cgroup.procs`, and `cgroup.kill` would then take the daemon with the step. The pid here is this
    /// very process, in a nested group of the tree: the tree is not written, the refusal names the
    /// pid, and the step's other process is still ended, by the pid route.
    #[test]
    fn a_tree_holding_this_process_gets_no_cgroup_kill() {
        let dir = tempfile::tempdir().unwrap();
        let mut other = sleeper();
        let own = std::process::id();
        let cg = fake_scope(dir.path(), &[other.0.id()], &[own]);

        let mut miss = Miss::default();
        kill_cgroup_tree(&cg, &mut miss);

        assert!(
            !cg.join("cgroup.kill").exists() && !cg.join("inner/cgroup.kill").exists(),
            "cgroup.kill was written on a tree that held this process"
        );
        let line = miss.line();
        assert!(
            line.contains(&format!("pid {own} ")) && line.contains("not swept through cgroup.kill"),
            "the refusal does not name the protected pid: {line}"
        );
        wait_gone(&mut other.0, "the step's other process");
    }

    /// The same sweep of a tree with nothing protected in it is the `cgroup.kill` of every directory
    /// of the tree, as it was.
    #[test]
    fn a_tree_without_a_protected_pid_is_swept_through_cgroup_kill() {
        let dir = tempfile::tempdir().unwrap();
        let child = sleeper();
        let cg = fake_scope(dir.path(), &[child.0.id()], &[]);

        let mut miss = Miss::default();
        kill_cgroup_tree(&cg, &mut miss);

        assert_eq!(fs::read(cg.join("cgroup.kill")).unwrap(), b"1\n");
        assert_eq!(fs::read(cg.join("inner/cgroup.kill")).unwrap(), b"1\n");
        assert!(miss.is_empty(), "{}", miss.line());
        drop(child);
    }

    /// With this process's own ids unreadable nothing can be screened, so nothing is swept: a tree
    /// that cannot be proved clear gets no `cgroup.kill`.
    #[test]
    fn a_tree_is_not_swept_when_own_ids_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = sleeper();
        let cg = fake_scope(dir.path(), &[child.0.id()], &[]);

        let mut miss = Miss::default();
        refuse_own_ids(true);
        kill_cgroup_tree(&cg, &mut miss);
        refuse_own_ids(false);

        assert!(
            !cg.join("cgroup.kill").exists(),
            "cgroup.kill was written by a process that could not screen the tree"
        );
        assert!(
            miss.line().contains("own ids could not be read"),
            "{}",
            miss.line()
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "the step was signalled unscreened"
        );
    }

    /// agentd's pid, as the user manager names it, is refused by the `cgroup.kill` route: the tree is
    /// not written, the stand-in lives, the refusal names it as agentd's, and the step's other
    /// process still dies by the pid route.
    #[test]
    fn a_tree_holding_agentds_pid_gets_no_cgroup_kill() {
        let dir = tempfile::tempdir().unwrap();
        let mut stand_in = sleeper();
        let mut other = sleeper();
        let agentd = stand_in.0.id();
        let cg = fake_scope(dir.path(), &[other.0.id()], &[agentd]);

        let mut miss = Miss {
            guard: Some(agentd_of(agentd)),
            ..Miss::default()
        };
        kill_cgroup_tree(&cg, &mut miss);

        assert!(
            !cg.join("cgroup.kill").exists() && !cg.join("inner/cgroup.kill").exists(),
            "cgroup.kill was written on a tree that held agentd's pid"
        );
        let line = miss.line();
        assert!(
            line.contains(&format!("pid {agentd} (agentd's main pid)")),
            "the refusal does not name agentd's pid: {line}"
        );
        wait_gone(&mut other.0, "the step's other process");
        assert!(
            stand_in.0.try_wait().unwrap().is_none(),
            "agentd's stand-in was signalled"
        );
    }

    /// Finding 1: the pid route came first and did not know agentd. A whole `run_to_end` over a tree
    /// that holds agentd's stand-in (in a group of its own) must leave it alive through the TERM and
    /// the KILL, while the step's own process, which ignores TERM, is ended.
    #[test]
    fn a_step_run_to_its_end_never_signals_agentds_pid() {
        let dir = tempfile::tempdir().unwrap();
        let mut stand_in = sleeper();
        let victim = Command::new("/bin/sh")
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut victim = Reap(victim);
        let answer = dir.path().join("mainpid");
        fs::write(&answer, format!("{}\n", stand_in.0.id())).unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        let cg = fake_scope(dir.path(), &[victim.0.id(), stand_in.0.id()], &[]);

        let mut step = Step::new(gone_pid(), None, Some(ctl));
        step.cg = Some(cg);
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let ended = step.run_to_end(
            &mut child,
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        let _ = child.wait();

        assert!(ended.status.is_some());
        wait_gone(&mut victim.0, "the step's own process");
        assert!(
            stand_in.0.try_wait().unwrap().is_none(),
            "agentd's stand-in was signalled by the sweep"
        );
        assert_eq!(
            ended.left, 0,
            "agentd was counted as one of the step's processes"
        );
    }

    /// Finding 2: the user manager is asked at the start of every sweep, so a restart of agentd between
    /// two sweeps moves the protection to the new pid.
    #[test]
    fn the_second_sweep_protects_the_pid_the_manager_names_then() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = sleeper();
        let mut second = sleeper();
        let answer = dir.path().join("mainpid");
        let ctl = fake_systemctl(dir.path(), &answer);
        let gone = gone_pid();

        let mut step = Step::new(gone, None, Some(ctl));
        let cg = fake_scope(dir.path(), &[first.0.id()], &[]);
        step.cg = Some(cg.clone());

        fs::write(&answer, format!("{}\n", first.0.id())).unwrap();
        let miss = step.kill();
        assert!(!cg.join("cgroup.kill").exists(), "{}", miss.line());
        assert!(first.0.try_wait().unwrap().is_none());

        // agentd restarted: the manager names the new pid, and the old one is the step's like any other.
        fs::write(&answer, format!("{}\n", second.0.id())).unwrap();
        write_procs(&cg, &[second.0.id()]);
        step.agentd_at = None;
        let miss = step.kill();
        assert!(!cg.join("cgroup.kill").exists(), "{}", miss.line());
        assert!(
            miss.line()
                .contains(&format!("pid {} (agentd's main pid)", second.0.id())),
            "{}",
            miss.line()
        );
        assert!(
            second.0.try_wait().unwrap().is_none(),
            "the new agentd was signalled"
        );

        // And the pid the manager no longer names is no longer protected.
        write_procs(&cg, &[first.0.id()]);
        step.agentd_at = None;
        let _ = step.kill();
        assert!(cg.join("cgroup.kill").exists());
        wait_gone(&mut first.0, "the old pid, no longer agentd's");
    }

    /// The user manager answers 0: the pid the daemon gave at the job's start is the fallback, and
    /// the note says so.
    #[test]
    fn a_manager_answer_of_zero_falls_back_to_the_pid_given_at_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut stand_in = sleeper();
        let answer = dir.path().join("mainpid");
        fs::write(&answer, "0\n").unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        let cg = fake_scope(dir.path(), &[stand_in.0.id()], &[]);
        let mut step = Step::new(gone_pid(), None, Some(ctl));
        step.cg = Some(cg.clone());

        set_agentd_env_pid(Some(stand_in.0.id()));
        let miss = step.kill();
        set_agentd_env_pid(None);

        assert!(!cg.join("cgroup.kill").exists(), "{}", miss.line());
        assert!(stand_in.0.try_wait().unwrap().is_none());
        let line = miss.line();
        assert!(line.contains("answered MainPID 0"), "{line}");
        assert!(line.contains("fallback the daemon gave"), "{line}");
    }

    /// Neither the manager nor the daemon's pid: agentd is unknown, said in a note of its own that is
    /// not a refused `cgroup.kill` write and does not make the miss non-empty (finding 4).
    #[test]
    fn an_unknown_agentd_is_a_note_not_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let victim = sleeper();
        let answer = dir.path().join("mainpid");
        fs::write(&answer, "0\n").unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        let cg = fake_scope(dir.path(), &[victim.0.id()], &[]);
        let mut step = Step::new(gone_pid(), None, Some(ctl));
        step.cg = Some(cg.clone());

        let miss = step.kill();

        // PIPE-05: with neither the pid nor the executable known nothing can be shown clear of agentd,
        // so the tree is not written (this asserted `exists()` while the sweep failed open).
        assert!(!cg.join("cgroup.kill").exists());
        assert!(miss.is_empty(), "{}", miss.line());
        assert!(!miss.says_nothing());
        let line = miss.line();
        assert!(line.contains("agentd's pid is unknown"), "{line}");
        assert!(
            line.contains("agentd unidentified: no cgroup.kill"),
            "{line}"
        );
        assert!(!line.contains("writes were refused"), "{line}");
    }

    /// A process running a copy of a small binary at `dir/<name>`, in a group of its own, ended and
    /// reaped on every path. It is a copy of the shell (the host sleep is one applet of a multicall
    /// binary that picks its applet by the file name, so a renamed copy of it does not run).
    fn start_copy(dir: &Path, name: &str) -> (Reap, PathBuf) {
        let path = dir.join(name);
        fs::copy("/bin/sh", &path).expect("sh is copied");
        // A file another thread's fork holds open for writing cannot be executed for a moment (ETXTBSY).
        for _ in 0..100 {
            match Command::new(&path)
                .args(["-c", "while :; do sleep 1; done"])
                .process_group(0)
                .spawn()
            {
                Ok(c) => return (Reap(c), path),
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("the copy of sh does not start: {e}"),
            }
        }
        panic!("the copy of sh stayed busy");
    }

    /// PIPE-06: a pid spared only because its `exe` link could not be read is still one of the step's
    /// processes — counted as left over and named with its pid — while a pid positively matched as
    /// agentd is not counted.
    #[test]
    fn a_spared_process_with_an_unreadable_exe_is_left_over_and_named() {
        assert!(counts_as_left(Some(EXE_UNREADABLE)));
        assert!(counts_as_left(None));
        assert!(!counts_as_left(Some("agentd's executable")));
        assert!(!counts_as_left(Some("agentd's main pid")));
        let mut miss = Miss::default();
        note_unreadable(&mut miss, 4242);
        let line = miss.line();
        assert!(
            line.contains("pid 4242") && line.contains("exe unreadable"),
            "{line}"
        );
    }

    /// Resets what a test of the identity sets, whatever the test does after it.
    struct IdentityGuard;

    impl IdentityGuard {
        fn new() -> Self {
            identity_set(Vec::new());
            set_agentd_env_exe(None);
            set_agentd_env_pid(None);
            IdentityGuard
        }
    }

    impl Drop for IdentityGuard {
        fn drop(&mut self) {
            identity_set(Vec::new());
            set_agentd_env_exe(None);
            set_agentd_env_pid(None);
        }
    }

    /// PIPE-05, the coordinator's case: the manager answers nothing, the pid given at the start is stale,
    /// and agentd is a process running the binary the daemon named, in a group of its own inside the
    /// swept tree. The tree gets no `cgroup.kill`, the stand-in is not signalled, the log names it as
    /// agentd's executable, and the step's own process still dies by the pid route.
    #[test]
    fn agentd_is_refused_by_its_executable_when_no_pid_is_known() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (stand_in, exe) = start_copy(dir.path(), "lucina-agentd");
        let mut stand_in = stand_in;
        let mut victim = sleeper();
        let answer = dir.path().join("mainpid");
        fs::write(&answer, "0\n").unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        let cg = fake_scope(dir.path(), &[victim.0.id(), stand_in.0.id()], &[]);
        let mut step = Step::new(gone_pid(), None, Some(ctl));
        step.cg = Some(cg.clone());
        set_agentd_env_pid(Some(gone_pid()));
        set_agentd_env_exe(Some(exe));

        let miss = step.kill();

        assert!(
            !cg.join("cgroup.kill").exists() && !cg.join("inner/cgroup.kill").exists(),
            "cgroup.kill was written on a tree that held agentd's executable"
        );
        let line = miss.line();
        assert!(
            line.contains(&format!("pid {} (agentd's executable)", stand_in.0.id())),
            "{line}"
        );
        wait_gone(&mut victim.0, "the step's own process");
        assert!(
            stand_in.0.try_wait().unwrap().is_none(),
            "the stand-in for agentd was signalled"
        );
    }

    /// A deploy replaced agentd's file: the link of the old process reads `… (deleted)`, which is
    /// removed before the comparison.
    #[test]
    fn a_deleted_executable_link_is_still_agentd() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (child, exe) = start_copy(dir.path(), "lucina-agentd");
        fs::remove_file(&exe).unwrap();
        assert_eq!(read_exe(child.0.id()), ExeRead::Path(exe.clone()));
        identity_set(vec![exe]);

        assert_eq!(
            AgentdIds::unknown().refuses(child.0.id()),
            Some("agentd's executable")
        );
    }

    /// The screen does not over-refuse: with agentd's executable known, a step's own process running
    /// another binary is killed, and a tree with nothing protected in it gets its `cgroup.kill`.
    #[test]
    fn a_step_process_of_another_binary_is_not_refused() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (_stand_in, exe) = start_copy(dir.path(), "lucina-agentd");
        identity_set(vec![exe]);
        let victim = sleeper();
        let cg = fake_scope(dir.path(), &[victim.0.id()], &[]);
        assert_eq!(AgentdIds::unknown().refuses(victim.0.id()), None);

        let mut miss = Miss {
            guard: Some(AgentdIds::unknown()),
            ..Miss::default()
        };
        kill_cgroup_tree(&cg, &mut miss);

        assert_eq!(fs::read(cg.join("cgroup.kill")).unwrap(), b"1\n");
        assert!(miss.says_nothing(), "{}", miss.line());
    }

    /// An exited process is not protected; any other failure to read the link is refused.
    #[test]
    fn an_unreadable_executable_link_is_refused_and_an_exited_process_is_not() {
        assert_eq!(classify_exe_error(Some(2), "x"), ExeRead::Gone);
        assert_eq!(classify_exe_error(Some(3), "x"), ExeRead::Gone);
        assert_eq!(
            classify_exe_error(Some(13), "permission denied"),
            ExeRead::Unreadable("permission denied".into())
        );
        assert_eq!(
            classify_exe_error(None, "odd"),
            ExeRead::Unreadable("odd".into())
        );
        assert_eq!(read_exe(4_000_000_000), ExeRead::Gone);
    }

    /// Neither the pid nor the executable known, a process named `lucina-agentd` is left alone by the
    /// pid route, another is killed, and no tree is written.
    #[test]
    fn with_agentd_unidentified_only_processes_not_named_agentd_are_signalled() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (mut named, _) = start_copy(dir.path(), "lucina-agentd");
        let mut victim = sleeper();
        let cg = fake_scope(dir.path(), &[victim.0.id(), named.0.id()], &[]);

        let mut miss = Miss {
            guard: Some(AgentdIds::unknown()),
            ..Miss::default()
        };
        kill_cgroup_tree(&cg, &mut miss);

        assert!(!cg.join("cgroup.kill").exists());
        assert!(
            miss.line().contains("agentd unidentified: no cgroup.kill"),
            "{}",
            miss.line()
        );
        wait_gone(&mut victim.0, "the step's own process");
        assert!(named.0.try_wait().unwrap().is_none());
    }

    /// With the manager silent at the start, the identity is the daemon's variable, and the line says
    /// `from start` for the pid and for the executable.
    #[test]
    fn the_identity_falls_back_to_the_daemons_variable_and_the_line_says_so() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (stand_in, exe) = start_copy(dir.path(), "lucina-agentd");
        let answer = dir.path().join("mainpid");
        fs::write(&answer, "\n").unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        set_agentd_env_pid(Some(stand_in.0.id()));
        set_agentd_env_exe(Some(exe.clone()));

        let lines = announce_agentd(Some(&ctl), &[]);

        assert_eq!(
            lines[0],
            format!(
                "agentd: pid {} from start, exe {} from start",
                stand_in.0.id(),
                exe.display()
            ),
            "{lines:?}"
        );
        assert_eq!(identity_get(), vec![exe]);
    }

    /// The manager's `MainPID` names a process running agentd's binary: pid and executable are both
    /// from the manager.
    #[test]
    fn the_line_says_manager_when_the_manager_names_agentd() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (stand_in, exe) = start_copy(dir.path(), "lucina-agentd");
        let answer = dir.path().join("mainpid");
        fs::write(&answer, format!("{}\n", stand_in.0.id())).unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);

        let lines = announce_agentd(Some(&ctl), &[]);

        assert_eq!(
            lines[0],
            format!(
                "agentd: pid {} from manager, exe {} from manager",
                stand_in.0.id(),
                exe.display()
            ),
            "{lines:?}"
        );
    }

    /// At service start `MainPID` is the broker's, which execs agentd a moment later: its executable is
    /// not agentd's identity. The note names it, and the identity is the daemon's path, which screens the
    /// real stand-in in the tree.
    #[test]
    fn a_brokers_executable_never_becomes_agentds_identity() {
        let _g = IdentityGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (broker, broker_exe) = start_copy(dir.path(), "infisical-broker");
        let (mut stand_in, exe) = start_copy(dir.path(), "lucina-agentd");
        let mut victim = sleeper();
        let answer = dir.path().join("mainpid");
        fs::write(&answer, format!("{}\n", broker.0.id())).unwrap();
        let ctl = fake_systemctl(dir.path(), &answer);
        set_agentd_env_exe(Some(exe.clone()));
        let cg = fake_scope(dir.path(), &[victim.0.id(), stand_in.0.id()], &[]);
        let mut step = Step::new(gone_pid(), None, Some(ctl));
        step.cg = Some(cg.clone());

        let miss = step.kill();

        assert_eq!(identity_get(), vec![exe]);
        let line = miss.line();
        assert!(
            line.contains("infisical-broker") && line.contains("refused as agentd's identity"),
            "{line}"
        );
        assert!(
            line.contains(&format!("pid {} (agentd's executable)", stand_in.0.id())),
            "{line}"
        );
        assert!(!cg.join("cgroup.kill").exists());
        wait_gone(&mut victim.0, "the step's own process");
        assert!(stand_in.0.try_wait().unwrap().is_none());
        // The same rule for the fallback: a variable naming the broker is no identity either.
        identity_set(Vec::new());
        set_agentd_env_exe(Some(broker_exe));
        let mut notes = Vec::new();
        assert!(resolve_exe(None, &mut notes).is_empty());
        assert!(
            notes.iter().any(|n| n.contains("infisical-broker")),
            "{notes:?}"
        );
    }

    /// Finding 5: agentd's group cannot be read (the pid is not a process) and the note says so.
    #[test]
    fn an_unreadable_agentd_group_is_said() {
        let mut notes = Vec::new();
        let ids = agentd_ids(4_000_000_000, &mut notes);
        assert!(ids.pgid.is_none());
        assert!(notes
            .iter()
            .any(|n| n.contains("process group could not be read")));
    }

    /// Finding 6: a process that stays in the group of another process agentd stands for is protected
    /// as a member of agentd's group, though it is not agentd's pid.
    #[test]
    fn a_member_of_agentds_group_is_protected() {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let mut leader = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 20 & echo $!; wait")
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(leader.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let member: u32 = line.trim().parse().unwrap();
        let leader_pid = leader.id();
        let _leader = Reap(leader);
        struct Member(u32);
        impl Drop for Member {
            fn drop(&mut self) {
                if let Some(p) = Pid::from_raw(self.0 as i32) {
                    let _ = kill_process(p, Signal::KILL);
                }
            }
        }
        let _member = Member(member);
        let mut other = sleeper();
        let cg = fake_scope(dir.path(), &[other.0.id()], &[member]);

        let mut miss = Miss {
            guard: Some(agentd_of(leader_pid)),
            ..Miss::default()
        };
        kill_cgroup_tree(&cg, &mut miss);

        assert!(!cg.join("cgroup.kill").exists());
        let l = miss.line();
        assert!(
            l.contains(&format!(
                "pid {member} (a member of agentd's process group)"
            )),
            "{l}"
        );
        wait_gone(&mut other.0, "the step's other process");
    }

    /// Finding 3: a group made in the tree between the screen and the write is not written. It holds
    /// this process's pid, which the first read never saw.
    #[test]
    fn a_group_made_after_the_screen_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut other = sleeper();
        let cg = fake_scope(dir.path(), &[other.0.id()], &[]);
        let late = cg.join("late");
        BETWEEN.with(|b| {
            *b.borrow_mut() = Some(Box::new({
                let late = late.clone();
                move || {
                    fs::create_dir_all(&late).unwrap();
                    write_procs(&late, &[std::process::id()]);
                }
            }))
        });

        let mut miss = Miss::default();
        kill_cgroup_tree(&cg, &mut miss);

        for d in [&cg, &cg.join("inner"), &late] {
            assert!(
                !d.join("cgroup.kill").exists(),
                "cgroup.kill was written in {} though the tree changed",
                d.display()
            );
        }
        assert!(
            miss.line()
                .contains("changed between the screen and the write"),
            "{}",
            miss.line()
        );
        wait_gone(&mut other.0, "the step's other process");
    }
}

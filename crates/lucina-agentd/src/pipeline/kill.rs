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
    listable(target) && Some(target) != Some(pgid) && Some(target) != Some(ppid)
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

    pub(crate) fn is_empty(&self) -> bool {
        self.processes == 0 && self.cgroups == 0
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

/// Every pid in `cg` and in every control group under it, this process's own excluded by the
/// screen. A step's child can `mkdir` inside its own delegated scope and move a grandchild into
/// it, which drops that grandchild from the scope's list for good, so this descends.
fn cgroup_pids(cg: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let mut dirs = vec![cg.to_path_buf()];
    while let Some(d) = dirs.pop() {
        out.extend(
            fs::read_to_string(d.join("cgroup.procs"))
                .unwrap_or_default()
                .lines()
                .filter_map(|p| p.trim().parse::<u32>().ok()),
        );
        if let Ok(rd) = fs::read_dir(&d) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    out.retain(|p| listable(*p) && !is_zombie(*p));
    out.sort_unstable();
    out.dedup();
    out
}

/// SIGKILL of the step's control group and of every group under it, through `cgroup.kill`: no
/// fork, no pid list, and every thread of every process rather than only the leaders a pid
/// names. Safe to aim at the step's own scope because `systemd-run --scope` execs its payload
/// into it: the scope holds the step and nothing else — the executor of the job stands outside
/// it, unlike a `lucina-ci` launcher, which is why there is nothing here to gather out of it.
/// A refusal is reported, not swallowed: a scope this user was not delegated answers it for
/// every write, and then the signals by group and by pid are all there is.
fn kill_cgroup_tree(cg: &Path, miss: &mut Miss) {
    let mut dirs = vec![cg.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let f = d.join("cgroup.kill");
        if let Err(e) = fs::write(&f, b"1\n") {
            // A kernel without `cgroup.kill`, or a scope not delegated to this user.
            miss.add_cgroup(format!("{e} ({})", f.display()));
        }
        if let Ok(rd) = fs::read_dir(&d) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
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
}

impl Step {
    /// The step whose direct child is `pid`, asked into unit `unit` (`None`: no scope, the group
    /// alone is what this step owns), with `ctl` the `systemctl` the job can ask.
    pub(crate) fn new(pgid: u32, unit: Option<String>, ctl: Option<PathBuf>) -> Self {
        Step {
            pgid,
            unit,
            cg: None,
            ctl,
            env: Vec::new(),
            asked: false,
            manager_note: None,
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
                Err(_) => return None,
            }
            if started.elapsed() >= MANAGER_ASK_CAP {
                // Ended through the same screen every other signal goes through, and only the pid this
                // wait started; then reaped, and read as no answer.
                let mut miss = Miss::default();
                signal_pid(child.id(), Signal::KILL, &mut miss);
                let _ = child.wait();
                self.manager_note = Some(format!(
                    "the user manager did not answer the ask for the control group of {unit} within {}s",
                    MANAGER_ASK_CAP.as_secs()
                ));
                return None;
            }
            std::thread::sleep(POLL);
        };
        if !status.success() {
            return None;
        }
        let out = rx.recv_timeout(SLACK).ok()?;
        let stdout_str = String::from_utf8(out).ok()?;
        let cg = stdout_str.trim();
        // The same screen the `/proc` route applies, and for the same reason: a control group is
        // about to be walked and killed, so it is only ever the one this step's unit ends in. An
        // alias, a template, or a manager that answers with something broader is no answer at all.
        if !cg.starts_with('/') || Path::new(cg).file_name()?.to_str()? != format!("{unit}.scope") {
            return None;
        }
        // And no component may be `..` or `.`: a path that climbs out of the cgroup tree is not this
        // step's tree, whatever its last component says.
        if Path::new(cg).components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
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
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The deadline's first blow: the group and the scope's pids get SIGTERM, so a step that can
    /// clean up after itself still can.
    pub(crate) fn terminate(&mut self) -> Miss {
        let mut miss = Miss::default();
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
        let mut miss = Miss::default();
        signal_group(self.pgid, Signal::KILL, &mut miss);
        for pid in self.scope_pids() {
            signal_pid(pid, Signal::KILL, &mut miss);
        }
        if let Some(cg) = &self.cg {
            kill_cgroup_tree(cg, &mut miss);
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
        (miss, left.len())
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
        let status = loop {
            let looked = start.elapsed() < SCOPE_LOOK;
            if !self.cgroup_known() && looked {
                self.look_for_cgroup(pid);
            }
            if let Ok(Some(s)) = child.try_wait() {
                break Some(s);
            }
            let e = start.elapsed();
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
            if !sweep_miss.is_empty() {
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
}

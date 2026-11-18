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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
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
/// cannot change while this process lives, so the read is kept.
fn own_ids() -> Option<(u32, u32)> {
    static IDS: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *IDS.get_or_init(|| {
        let t = fs::read_to_string("/proc/self/stat").ok()?;
        let mut f = stat_after_comm(&t);
        let _state = f.next()?;
        let ppid = f.next()?.parse().ok()?;
        let pgid = f.next()?.parse().ok()?;
        Some((ppid, pgid))
    })
}

/// Whether a signal may be aimed at `target`: never pid 0 (which `kill` reads as *this* process's
/// group), never pid 1, never this process (it has a job log and a `result.json` to finish), never
/// this process's group (the daemon, the test binary, and every other step of the job) and never
/// its parent. Everything a step's group or scope names besides those is the step's own.
fn signalable(target: u32) -> bool {
    target > 1
        && target != std::process::id()
        && Some(target) != own_ids().map(|(_, pgid)| pgid)
        && Some(target) != own_ids().map(|(ppid, _)| ppid)
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
    if !signalable(pid) {
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
    if !signalable(pgid) {
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
        if !signalable(pid) {
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
    out.retain(|p| signalable(*p) && !is_zombie(*p));
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
pub(crate) struct Step {
    pgid: u32,
    unit: Option<String>,
    cg: Option<PathBuf>,
}

impl Step {
    /// The step whose direct child is `pid`, asked into unit `unit` (`None`: no scope, the group
    /// alone is what this step owns).
    pub(crate) fn new(pgid: u32, unit: Option<String>) -> Self {
        Step {
            pgid,
            unit,
            cg: None,
        }
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
            // so they are what gets reported.
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
        }
    }
}

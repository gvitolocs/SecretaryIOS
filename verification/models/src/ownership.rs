//! Single-writer ownership across the Node → Rust Lucina port.
//!
//! Three models:
//! - [`Cutover`]: phase 1, `deploy/lucina-rs/cutover.sh` / `rollback.sh`
//!   (branch `lucina-rs`): the secretary-server route (p1), the Rust mode
//!   (shadow / live), the channel and scheduler owners, and the one store the
//!   cutover really shares: `data/secretary-jobs.json` (`SECRETARY_JOBS_PATH`
//!   in `cutover.env` is Node's live file; both runtimes read-modify-write it
//!   under in-process locks only).
//! - [`Scheduler`]: the planned phase-2 reminders handover
//!   (`docs/lucina-rust/PORT_PLAN.md` §5 phase 2 and §6): `reminders.json`
//!   writers, the two schedulers, the Node shim.
//! - [`Channels`]: the planned phase-3/4 handovers (Telegram `getUpdates`
//!   behind the gateway lock, Discord without a lock in Node).

use stateright::{Model, Property};

use crate::{Case, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rt {
    Node,
    Rust,
}

// =========================================================================
// Phase 1: cutover / rollback and the jobs file
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Shadow,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Script {
    Idle,
    Cutover(u8),
    Rollback(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobSt {
    Running,
    Done,
    Interrupted,
}

/// A job record in a jobs file: (job, status, owner, owner's boot).
pub type JobRec = (u8, JobSt, Rt, u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobOp {
    Insert(u8),
    Finish(u8),
    MarkInterrupted(u8),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rmw {
    pub op: JobOp,
    pub file: Rt,
    pub snapshot: Vec<JobRec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JobInfo {
    pub runtime: Rt,
    pub boot: u8,
    /// Still being worked on by its runtime (`activeJobIds` / `active`).
    pub active: bool,
    pub inserted: bool,
    /// What the app last saw (it stops polling at a terminal status).
    pub app: Option<JobSt>,
    pub app_not_found: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CutoverState {
    pub p1: Rt,
    pub mode: Mode,
    pub rust_up: bool,
    pub rust_boot: u8,
    pub script: Script,
    pub runs_left: u8,
    pub crashes_left: u8,
    /// `Node`: Lucina/data/secretary-jobs.json; `Rust`: data/lucina-rs/… (fix).
    pub files: [Vec<JobRec>; 2],
    pub rmw: [Option<Rmw>; 2],
    pub jobs: Vec<JobInfo>,
    pub jobs_left: u8,
    pub false_interrupt: bool,
    pub lost_update: bool,
    pub rollback_bad: bool,
    pub rolled_back: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CutoverAction {
    OperatorCutover,
    OperatorRollback,
    ScriptStep,
    ScriptFails,
    RustCrashRestart,
    AppStartJob,
    RuntimeFinish(u8),
    AppPoll(u8),
    RmwWrite(Rt),
}

#[derive(Clone, Debug)]
pub struct Cutover {
    pub jobs: u8,
    pub runs: u8,
    pub crashes: u8,
    /// Fix: Rust keeps its own jobs file (reads Node's read-only for ids it
    /// does not know) and only marks its own jobs from an older boot as
    /// interrupted.
    pub fix_own_jobs_file: bool,
}

fn idx(rt: Rt) -> usize {
    match rt {
        Rt::Node => 0,
        Rt::Rust => 1,
    }
}

impl Cutover {
    fn boot_of(s: &CutoverState, rt: Rt) -> u8 {
        match rt {
            Rt::Node => 0,
            Rt::Rust => s.rust_boot,
        }
    }

    /// The file a runtime writes its job records to.
    fn file_for(&self, s: &CutoverState, rt: Rt) -> Rt {
        match rt {
            Rt::Node => Rt::Node,
            Rt::Rust if self.fix_own_jobs_file => Rt::Rust,
            // Live: Node's file (cutover.env). Shadow: LUCINA_JOBS=false.
            Rt::Rust => {
                let _ = s;
                Rt::Node
            }
        }
    }

    /// A job record as `rt` sees it (the fix reads Node's file read-only too).
    fn lookup(&self, s: &CutoverState, rt: Rt, j: u8) -> Option<JobRec> {
        let own = s.files[idx(self.file_for(s, rt))]
            .iter()
            .find(|r| r.0 == j)
            .copied();
        if own.is_some() || !(self.fix_own_jobs_file && rt == Rt::Rust) {
            return own;
        }
        s.files[idx(Rt::Node)].iter().find(|r| r.0 == j).copied()
    }

    fn rust_serves(s: &CutoverState) -> bool {
        s.rust_up && s.mode == Mode::Live
    }

    fn kill_rust_jobs(s: &mut CutoverState) {
        for j in s.jobs.iter_mut() {
            if j.runtime == Rt::Rust {
                j.active = false;
            }
        }
        s.rmw[idx(Rt::Rust)] = None;
    }

    fn apply(op: JobOp, mut snap: Vec<JobRec>, info: &[JobInfo], rt: Rt, boot: u8) -> Vec<JobRec> {
        match op {
            JobOp::Insert(j) => {
                let _ = info;
                snap.push((j, JobSt::Running, rt, boot));
            }
            JobOp::Finish(j) => {
                for r in snap.iter_mut().filter(|r| r.0 == j) {
                    r.1 = JobSt::Done;
                }
            }
            JobOp::MarkInterrupted(j) => {
                for r in snap
                    .iter_mut()
                    .filter(|r| r.0 == j && r.1 == JobSt::Running)
                {
                    r.1 = JobSt::Interrupted;
                }
            }
        }
        snap.sort();
        snap
    }
}

impl Model for Cutover {
    type State = CutoverState;
    type Action = CutoverAction;

    fn init_states(&self) -> Vec<CutoverState> {
        vec![CutoverState {
            p1: Rt::Node,
            mode: Mode::Shadow,
            rust_up: true,
            rust_boot: 1,
            script: Script::Idle,
            runs_left: self.runs,
            crashes_left: self.crashes,
            files: [Vec::new(), Vec::new()],
            rmw: [None, None],
            jobs: Vec::new(),
            jobs_left: self.jobs,
            false_interrupt: false,
            lost_update: false,
            rollback_bad: false,
            rolled_back: false,
        }]
    }

    fn actions(&self, s: &CutoverState, a: &mut Vec<CutoverAction>) {
        use CutoverAction::*;
        match s.script {
            Script::Idle if s.runs_left > 0 => {
                a.push(OperatorCutover);
                a.push(OperatorRollback);
            }
            Script::Idle => {}
            _ => {
                a.push(ScriptStep);
                a.push(ScriptFails);
            }
        }
        if s.crashes_left > 0 && s.rust_up {
            a.push(RustCrashRestart);
        }
        let target_up = match s.p1 {
            Rt::Node => true,
            Rt::Rust => Self::rust_serves(s),
        };
        if s.jobs_left > 0 && target_up && s.rmw[idx(s.p1)].is_none() {
            a.push(AppStartJob);
        }
        for (j, info) in s.jobs.iter().enumerate() {
            if info.active && info.inserted && s.rmw[idx(info.runtime)].is_none() {
                a.push(RuntimeFinish(j as u8));
            }
            let terminal = matches!(info.app, Some(JobSt::Done) | Some(JobSt::Interrupted))
                || info.app_not_found;
            if info.inserted && !terminal && target_up && s.rmw[idx(s.p1)].is_none() {
                a.push(AppPoll(j as u8));
            }
        }
        for rt in [Rt::Node, Rt::Rust] {
            if s.rmw[idx(rt)].is_some() {
                a.push(RmwWrite(rt));
            }
        }
    }

    fn next_state(&self, last: &CutoverState, action: CutoverAction) -> Option<CutoverState> {
        use CutoverAction::*;
        let mut s = last.clone();
        match action {
            OperatorCutover => {
                s.script = Script::Cutover(0);
                s.runs_left -= 1;
            }
            OperatorRollback => {
                s.script = Script::Rollback(0);
                s.runs_left -= 1;
            }
            ScriptFails => s.script = Script::Idle, // ssh error: set -e stops the script
            ScriptStep => match s.script {
                // 1. preflight: both runtimes answer /health.
                Script::Cutover(0) => {
                    if !s.rust_up {
                        s.script = Script::Idle;
                    } else {
                        s.script = Script::Cutover(1);
                    }
                }
                // 2. chown + cutover.env + restart lucina-rs (health gate).
                Script::Cutover(1) => {
                    s.mode = Mode::Live;
                    s.rust_boot += 1;
                    s.rust_up = true;
                    Self::kill_rust_jobs(&mut s);
                    s.script = Script::Cutover(2);
                }
                // 3. p1: SECRETARY_LUCINA_URL -> :8790, restart secretary-server.
                Script::Cutover(_) => {
                    s.p1 = Rt::Rust;
                    s.script = Script::Idle;
                }
                // 1. p1 back to :8788.
                Script::Rollback(0) => {
                    s.p1 = Rt::Node;
                    s.script = Script::Rollback(1);
                }
                // 2. lucina-rs back to shadow.env + restart.
                Script::Rollback(_) => {
                    s.mode = Mode::Shadow;
                    s.rust_boot += 1;
                    s.rust_up = true;
                    Self::kill_rust_jobs(&mut s);
                    s.script = Script::Idle;
                    s.rolled_back = true;
                    if s.p1 != Rt::Node || s.mode != Mode::Shadow {
                        s.rollback_bad = true;
                    }
                }
                Script::Idle => return None,
            },
            RustCrashRestart => {
                // Restart=on-failure / the health timer: same mode.env.
                s.crashes_left -= 1;
                s.rust_boot += 1;
                Self::kill_rust_jobs(&mut s);
            }
            AppStartJob => {
                let rt = s.p1;
                let j = s.jobs.len() as u8;
                s.jobs.push(JobInfo {
                    runtime: rt,
                    boot: Self::boot_of(&s, rt),
                    active: true,
                    inserted: false,
                    app: None,
                    app_not_found: false,
                });
                s.jobs_left -= 1;
                let file = self.file_for(&s, rt);
                s.rmw[idx(rt)] = Some(Rmw {
                    op: JobOp::Insert(j),
                    file,
                    snapshot: s.files[idx(file)].clone(),
                });
            }
            RuntimeFinish(j) => {
                let rt = s.jobs[j as usize].runtime;
                let file = self.file_for(&s, rt);
                s.rmw[idx(rt)] = Some(Rmw {
                    op: JobOp::Finish(j),
                    file,
                    snapshot: s.files[idx(file)].clone(),
                });
            }
            AppPoll(j) => {
                let rt = s.p1;
                match self.lookup(&s, rt, j) {
                    None => s.jobs[j as usize].app_not_found = true,
                    Some((_, JobSt::Running, owner, boot)) => {
                        let active_here =
                            s.jobs[j as usize].runtime == rt && s.jobs[j as usize].active;
                        let may_mark = if self.fix_own_jobs_file {
                            owner == rt && boot != Self::boot_of(&s, rt)
                        } else {
                            !active_here
                        };
                        if may_mark {
                            // findJob: decide again under the lock, then write.
                            let file = self.file_for(&s, rt);
                            s.rmw[idx(rt)] = Some(Rmw {
                                op: JobOp::MarkInterrupted(j),
                                file,
                                snapshot: s.files[idx(file)].clone(),
                            });
                        } else {
                            s.jobs[j as usize].app = Some(JobSt::Running);
                        }
                    }
                    Some((_, st, _, _)) => s.jobs[j as usize].app = Some(st),
                }
            }
            RmwWrite(rt) => {
                let rmw = s.rmw[idx(rt)].take()?;
                let current = s.files[idx(rmw.file)].clone();
                let boot = Self::boot_of(&s, rt);
                let new = Self::apply(rmw.op, rmw.snapshot, &s.jobs, rt, boot);
                // Lost update: a record, or its progress, present now but not in what we write.
                for r in &current {
                    match new.iter().find(|n| n.0 == r.0) {
                        None => s.lost_update = true,
                        Some(n) if n.1 == JobSt::Running && r.1 != JobSt::Running => {
                            s.lost_update = true
                        }
                        _ => {}
                    }
                }
                if let JobOp::MarkInterrupted(j) = rmw.op {
                    let info = &s.jobs[j as usize];
                    if info.active {
                        s.false_interrupt = true; // its runtime is still running it
                    }
                    if new.iter().any(|r| r.0 == j && r.1 == JobSt::Interrupted) {
                        s.jobs[j as usize].app = Some(JobSt::Interrupted);
                    }
                }
                if let JobOp::Insert(j) = rmw.op {
                    s.jobs[j as usize].inserted = true;
                }
                if let JobOp::Finish(j) = rmw.op {
                    s.jobs[j as usize].active = false;
                }
                s.files[idx(rmw.file)] = new;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("routes_to_live_runtime", |_, s: &CutoverState| {
                s.p1 == Rt::Node || s.mode == Mode::Live
            }),
            Property::always("rollback_restores_node", |_, s: &CutoverState| {
                !s.rollback_bad
            }),
            // Phase 1 never moves a channel or the scheduler: cutover.env and
            // shadow.env set LUCINA_SCHEDULER/TELEGRAM/DISCORD=false, the
            // build has no channels, and the scheduler is refused while
            // LUCINA_REMINDERS_WRITER=node (lib.rs::spawn_background).
            Property::always(
                "single_channel_and_scheduler_owner",
                |_, s: &CutoverState| {
                    // Node keeps its tokens and scheduler in phase 1.
                    let rust = rust_env(s.mode);
                    let rust_scheduler = s.rust_up && rust.scheduler && rust.writer_rust;
                    let rust_telegram = s.rust_up && rust.telegram && BUILD_HAS_CHANNELS;
                    let rust_discord = s.rust_up && rust.discord && BUILD_HAS_CHANNELS;
                    !(rust_scheduler || rust_telegram || rust_discord)
                },
            ),
            Property::always("no_false_interrupted", |_, s: &CutoverState| {
                !s.false_interrupt
            }),
            Property::always("no_lost_job_update", |_, s: &CutoverState| !s.lost_update),
            Property::sometimes("cut_over_with_node_job", |_, s: &CutoverState| {
                s.p1 == Rt::Rust && s.jobs.iter().any(|j| j.runtime == Rt::Node && j.active)
            }),
            Property::sometimes("rolled_back", |_, s: &CutoverState| s.rolled_back),
        ]
    }
}

/// `deploy/lucina-rs/{shadow,cutover}.env`: the flags that decide ownership.
#[derive(Clone, Copy, Debug)]
pub struct RustEnv {
    pub scheduler: bool,
    pub telegram: bool,
    pub discord: bool,
    pub writer_rust: bool,
}

pub fn rust_env(mode: Mode) -> RustEnv {
    match mode {
        // shadow.env: LUCINA_SCHEDULER=false, LUCINA_TELEGRAM_ENABLED=false,
        // LUCINA_DISCORD_ENABLED=false, LUCINA_REMINDERS_WRITER=node
        Mode::Shadow => RustEnv {
            scheduler: false,
            telegram: false,
            discord: false,
            writer_rust: false,
        },
        // cutover.env: the same four values.
        Mode::Live => RustEnv {
            scheduler: false,
            telegram: false,
            discord: false,
            writer_rust: false,
        },
    }
}

/// `lucina-server/src/lib.rs::spawn_background`: channels are not part of
/// the phase-1 build (the flags only log a warning).
pub const BUILD_HAS_CHANNELS: bool = false;

// =========================================================================
// Phase 2: reminders.json writers and the scheduler handover
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RemSt {
    Pending,
    /// Fix: claimed before `sendMessage` (at most once).
    Sending,
    Sent,
    /// Fix: found `Sending` after a restart: not resent, shown with lastError.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Env {
    pub up: bool,
    /// Node: LUCINA_REMINDER_SCHEDULER_ENABLED; Rust: LUCINA_SCHEDULER.
    pub sched: bool,
    /// Node: the shim (LUCINA_REMINDERS_REMOTE_URL); Rust: LUCINA_REMINDERS_WRITER=rust
    /// (the secretary path writes locally instead of forwarding to Node).
    pub flag: bool,
    /// Rust only: `/api/internal/reminders/*` accepts writes (the shim target).
    pub internal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Work {
    /// Read-modify-write of reminders.json: create reminder `r`.
    Create(u8, Vec<(u8, RemSt)>),
    /// Scheduler pass: snapshot, due ids, the ones already sent.
    Tick(Vec<(u8, RemSt)>, Vec<u8>, Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpStep {
    /// Restart Rust with (scheduler, writer=rust, internal endpoint).
    Rust(bool, bool, bool),
    /// Restart Node with (scheduler, shim).
    Node(bool, bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SchedState {
    pub env: [Env; 2],
    pub work: [Option<Work>; 2],
    pub file: Vec<(u8, RemSt)>,
    /// Acknowledged creates (the write landed).
    pub created: Vec<bool>,
    /// A create request is in flight.
    pub requested: Vec<bool>,
    pub deliveries: Vec<u8>,
    pub plan_pos: u8,
    pub crashes_left: u8,
    pub lost_record: bool,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchedAction {
    Operator,
    Crash(Rt),
    CreateViaTelegram(u8),
    CreateViaApp(u8),
    TickRead(Rt),
    TickClaim(Rt),
    TickSend(Rt),
    TickWrite(Rt),
    CreateWrite(Rt),
    Settle,
}

#[derive(Clone, Debug)]
pub struct Scheduler {
    pub reminders: u8,
    pub plan: Vec<OpStep>,
    pub crashes: u8,
    /// Fix: claim (`sending`) before `sendMessage`; a claimed reminder found
    /// after a restart is failed visibly instead of being sent again.
    pub fix_claim: bool,
    /// Fix: the operator restarts Node only when its scheduler is idle
    /// (or Node drains its tick on SIGTERM).
    pub fix_drain: bool,
}

impl Scheduler {
    /// The documented order (PORT_PLAN §5 phase 2): (1) Rust up as the
    /// reminders store with its scheduler off, (2) Node with the shim and
    /// without its scheduler, (3) Rust scheduler on.
    pub fn plan_as_written() -> Vec<OpStep> {
        vec![
            OpStep::Rust(false, true, true),
            OpStep::Node(false, true),
            OpStep::Rust(true, true, true),
        ]
    }
    /// Proposed order: (1) Rust opens only the internal endpoint (its
    /// secretary path still forwards to Node, nobody calls the endpoint
    /// yet); (2) Node restarts with the shim, scheduler off, after draining
    /// its tick: from here Rust is the only writer; (3) Rust writes the
    /// secretary path locally and starts its scheduler.
    pub fn plan_fixed() -> Vec<OpStep> {
        vec![
            OpStep::Rust(false, false, true),
            OpStep::Node(false, true),
            OpStep::Rust(true, true, true),
        ]
    }
    /// Rollback: Rust stops writing first (scheduler off, forwards, internal
    /// endpoint closed: Node's shim calls fail visibly for a few seconds),
    /// then Node takes the file and the scheduler back.
    pub fn rollback_fixed() -> Vec<OpStep> {
        vec![OpStep::Rust(false, false, false), OpStep::Node(true, false)]
    }

    fn writer_of_create(s: &SchedState, via: Rt) -> Option<Rt> {
        let (node, rust) = (s.env[0], s.env[1]);
        match via {
            Rt::Node if !node.up => None,
            // Node with the shim forwards to Rust /api/internal/reminders.
            Rt::Node if node.flag => (rust.up && rust.internal).then_some(Rt::Rust),
            Rt::Node => Some(Rt::Node),
            Rt::Rust if !rust.up => None,
            Rt::Rust if rust.flag => Some(Rt::Rust),
            // writer=node: forwarded to Node's /api/secretary/reminders.
            Rt::Rust => Self::writer_of_create(s, Rt::Node),
        }
    }

    /// Read-modify-writes of reminders.json in flight in both processes at
    /// once (each process only serialises its own).
    fn concurrent_writers(s: &SchedState) -> bool {
        s.work.iter().all(Option::is_some)
    }

    fn scheduler_on(s: &SchedState, rt: Rt) -> bool {
        let e = s.env[idx(rt)];
        match rt {
            Rt::Node => e.up && e.sched,
            // LUCINA_SCHEDULER=true is ignored while LUCINA_REMINDERS_WRITER=node.
            Rt::Rust => e.up && e.sched && e.flag,
        }
    }

    fn write_file(s: &mut SchedState, new: Vec<(u8, RemSt)>) {
        for (r, st) in &s.file {
            match new.iter().find(|n| n.0 == *r) {
                None => s.lost_record = true,
                Some(n) if n.1 == RemSt::Pending && *st != RemSt::Pending => s.lost_record = true,
                _ => {}
            }
        }
        s.file = new;
    }

    fn restart(&self, s: &mut SchedState, rt: Rt) {
        match s.work[idx(rt)].take() {
            Some(Work::Create(r, _)) => s.requested[r as usize] = false, // the client sees an error
            Some(Work::Tick(_, due, _)) if self.fix_claim => {
                // Startup scan: our own claims from the dead tick are failed, not resent.
                for r in s
                    .file
                    .iter_mut()
                    .filter(|r| r.1 == RemSt::Sending && due.contains(&r.0))
                {
                    r.1 = RemSt::Failed;
                }
            }
            _ => {}
        }
    }
}

impl Model for Scheduler {
    type State = SchedState;
    type Action = SchedAction;

    fn init_states(&self) -> Vec<SchedState> {
        let n = self.reminders as usize;
        vec![SchedState {
            // Today: Node schedules and writes; Rust (phase 1) forwards.
            env: [
                Env {
                    up: true,
                    sched: true,
                    flag: false,
                    internal: false,
                },
                Env {
                    up: true,
                    sched: false,
                    flag: false,
                    internal: false,
                },
            ],
            work: [None, None],
            file: Vec::new(),
            created: vec![false; n],
            requested: vec![false; n],
            deliveries: vec![0; n],
            plan_pos: 0,
            crashes_left: self.crashes,
            lost_record: false,
            settled: false,
        }]
    }

    fn actions(&self, s: &SchedState, a: &mut Vec<SchedAction>) {
        use SchedAction::*;
        if s.settled {
            return;
        }
        if (s.plan_pos as usize) < self.plan.len() {
            let step = self.plan[s.plan_pos as usize];
            let node_busy = s.work[0].is_some();
            if !(self.fix_drain && matches!(step, OpStep::Node(..)) && node_busy) {
                a.push(Operator);
            }
        }
        for rt in [Rt::Node, Rt::Rust] {
            let i = idx(rt);
            if s.crashes_left > 0 && s.work[i].is_some() {
                a.push(Crash(rt));
            }
            match &s.work[i] {
                None => {
                    if Self::scheduler_on(s, rt) && s.file.iter().any(|r| r.1 == RemSt::Pending) {
                        a.push(TickRead(rt));
                    }
                }
                Some(Work::Create(..)) => a.push(CreateWrite(rt)),
                Some(Work::Tick(snap, due, sent)) => {
                    let claimed = due
                        .iter()
                        .all(|d| snap.iter().any(|r| r.0 == *d && r.1 == RemSt::Sending));
                    if self.fix_claim && !claimed {
                        a.push(TickClaim(rt));
                    } else if sent.len() < due.len() {
                        a.push(TickSend(rt));
                    } else {
                        a.push(TickWrite(rt));
                    }
                }
            }
        }
        for r in 0..self.reminders {
            if !s.created[r as usize] && !s.requested[r as usize] {
                for (via, act) in [
                    (Rt::Node, CreateViaTelegram(r)),
                    (Rt::Rust, CreateViaApp(r)),
                ] {
                    if let Some(w) = Self::writer_of_create(s, via) {
                        if s.work[idx(w)].is_none() {
                            a.push(act);
                        }
                    }
                }
            }
        }
        let quiet = s.work.iter().all(Option::is_none);
        let all_created = s.created.iter().all(|c| *c);
        let plan_done = s.plan_pos as usize == self.plan.len();
        let nothing_due = !s.file.iter().any(|r| r.1 == RemSt::Pending);
        let no_scheduler = !Self::scheduler_on(s, Rt::Node) && !Self::scheduler_on(s, Rt::Rust);
        if quiet && all_created && plan_done && (nothing_due || no_scheduler) {
            a.push(Settle);
        }
    }

    fn next_state(&self, last: &SchedState, action: SchedAction) -> Option<SchedState> {
        use SchedAction::*;
        let mut s = last.clone();
        match action {
            Operator => {
                match self.plan[s.plan_pos as usize] {
                    OpStep::Rust(sched, writer, internal) => {
                        self.restart(&mut s, Rt::Rust);
                        s.env[1] = Env {
                            up: true,
                            sched,
                            flag: writer,
                            internal,
                        };
                    }
                    OpStep::Node(sched, shim) => {
                        self.restart(&mut s, Rt::Node);
                        s.env[0] = Env {
                            up: true,
                            sched,
                            flag: shim,
                            internal: false,
                        };
                    }
                }
                s.plan_pos += 1;
            }
            Crash(rt) => {
                s.crashes_left -= 1;
                self.restart(&mut s, rt);
            }
            CreateViaTelegram(r) | CreateViaApp(r) => {
                let via = if matches!(action, CreateViaTelegram(_)) {
                    Rt::Node
                } else {
                    Rt::Rust
                };
                let w = Self::writer_of_create(&s, via)?;
                s.requested[r as usize] = true;
                s.work[idx(w)] = Some(Work::Create(r, s.file.clone()));
            }
            CreateWrite(rt) => {
                let Some(Work::Create(r, mut snap)) = s.work[idx(rt)].take() else {
                    return None;
                };
                snap.push((r, RemSt::Pending));
                snap.sort();
                Self::write_file(&mut s, snap);
                s.created[r as usize] = true;
                s.requested[r as usize] = false;
            }
            TickRead(rt) => {
                let snap = s.file.clone();
                let due: Vec<u8> = snap
                    .iter()
                    .filter(|r| r.1 == RemSt::Pending)
                    .map(|r| r.0)
                    .collect();
                s.work[idx(rt)] = Some(Work::Tick(snap, due, Vec::new()));
            }
            TickClaim(rt) => {
                let Some(Work::Tick(mut snap, due, sent)) = s.work[idx(rt)].take() else {
                    return None;
                };
                for r in snap.iter_mut().filter(|r| due.contains(&r.0)) {
                    r.1 = RemSt::Sending;
                }
                Self::write_file(&mut s, snap.clone());
                s.work[idx(rt)] = Some(Work::Tick(snap, due, sent));
            }
            TickSend(rt) => {
                let Some(Work::Tick(snap, due, mut sent)) = s.work[idx(rt)].take() else {
                    return None;
                };
                let r = due[sent.len()];
                s.deliveries[r as usize] += 1;
                sent.push(r);
                s.work[idx(rt)] = Some(Work::Tick(snap, due, sent));
            }
            TickWrite(rt) => {
                let Some(Work::Tick(mut snap, _due, sent)) = s.work[idx(rt)].take() else {
                    return None;
                };
                for r in snap.iter_mut().filter(|r| sent.contains(&r.0)) {
                    r.1 = RemSt::Sent;
                }
                Self::write_file(&mut s, snap);
            }
            Settle => s.settled = true,
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("single_scheduler", |_, s: &SchedState| {
                !(Scheduler::scheduler_on(s, Rt::Node) && Scheduler::scheduler_on(s, Rt::Rust))
            }),
            Property::always("single_reminders_writer", |_, s: &SchedState| {
                !Scheduler::concurrent_writers(s)
            }),
            Property::always("no_lost_reminder_record", |_, s: &SchedState| {
                !s.lost_record
            }),
            Property::always("no_double_delivery", |_, s: &SchedState| {
                s.deliveries.iter().all(|d| *d <= 1)
            }),
            Property::always("no_silent_loss", |_, s: &SchedState| {
                !s.settled
                    || (0..s.created.len()).all(|r| {
                        s.deliveries[r] > 0
                            || s.file
                                .iter()
                                .any(|f| f.0 as usize == r && f.1 == RemSt::Failed)
                    })
            }),
            Property::sometimes("handed_over", |_, s: &SchedState| {
                s.settled && Scheduler::scheduler_on(s, Rt::Rust)
            }),
        ]
    }
}

// =========================================================================
// Phases 3 and 4: Telegram getUpdates and the Discord gateway
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChStep {
    /// Rust: enable (telegram, discord) and restart.
    Rust(bool, bool),
    /// Node: (telegram token, discord token) and restart (SIGTERM releases the lock).
    Node(bool, bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChState {
    /// (up, telegram, discord) per runtime.
    pub env: [(bool, bool, bool); 2],
    /// telegram-<botid>.lock holder and whether its heartbeat is still fresh.
    pub lock: Option<(Rt, bool)>,
    pub plan_pos: u8,
    pub crashes_left: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChAction {
    Operator,
    Kill(Rt),
    Start(Rt),
    LockExpires,
    Acquire(Rt),
}

#[derive(Clone, Debug)]
pub struct Channels {
    pub plan: Vec<ChStep>,
    pub crashes: u8,
    /// HERMES_GATEWAY_LOCK=false on one side (forbidden by PORT_PLAN §6).
    pub lock_disabled: bool,
}

impl Channels {
    fn polls_telegram(&self, s: &ChState, rt: Rt) -> bool {
        let (up, tg, _) = s.env[idx(rt)];
        up && tg && (self.lock_disabled || s.lock.is_some_and(|(h, _)| h == rt))
    }
    fn on_discord(s: &ChState, rt: Rt) -> bool {
        let (up, _, dc) = s.env[idx(rt)];
        up && dc
    }
    fn release(s: &mut ChState, rt: Rt) {
        if s.lock.is_some_and(|(h, _)| h == rt) {
            s.lock = None;
        }
    }
}

impl Model for Channels {
    type State = ChState;
    type Action = ChAction;

    fn init_states(&self) -> Vec<ChState> {
        vec![ChState {
            env: [(true, true, true), (true, false, false)],
            lock: Some((Rt::Node, true)),
            plan_pos: 0,
            crashes_left: self.crashes,
        }]
    }

    fn actions(&self, s: &ChState, a: &mut Vec<ChAction>) {
        if (s.plan_pos as usize) < self.plan.len() {
            a.push(ChAction::Operator);
        }
        for rt in [Rt::Node, Rt::Rust] {
            let (up, tg, _) = s.env[idx(rt)];
            if up && s.crashes_left > 0 {
                a.push(ChAction::Kill(rt));
            }
            if !up {
                a.push(ChAction::Start(rt));
            }
            if up && tg && s.lock.is_none() {
                a.push(ChAction::Acquire(rt));
            }
        }
        if let Some((h, true)) = s.lock {
            if !s.env[idx(h)].0 {
                a.push(ChAction::LockExpires);
            }
        }
    }

    fn next_state(&self, last: &ChState, action: ChAction) -> Option<ChState> {
        let mut s = last.clone();
        match action {
            ChAction::Operator => {
                match self.plan[s.plan_pos as usize] {
                    ChStep::Rust(tg, dc) => {
                        Self::release(&mut s, Rt::Rust);
                        s.env[1] = (true, tg, dc);
                    }
                    ChStep::Node(tg, dc) => {
                        Self::release(&mut s, Rt::Node);
                        s.env[0] = (true, tg, dc);
                    }
                }
                s.plan_pos += 1;
            }
            // SIGKILL / OOM: the lock file stays, live until the heartbeat ages out (90 s).
            ChAction::Kill(rt) => {
                s.crashes_left -= 1;
                s.env[idx(rt)].0 = false;
            }
            // Restart=on-failure / docker restart policy: same env.
            ChAction::Start(rt) => s.env[idx(rt)].0 = true,
            ChAction::LockExpires => s.lock = None,
            ChAction::Acquire(rt) => s.lock = Some((rt, true)),
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("single_telegram_poller", |m: &Channels, s: &ChState| {
                !(m.polls_telegram(s, Rt::Node) && m.polls_telegram(s, Rt::Rust))
            }),
            Property::always("single_discord_gateway", |_, s: &ChState| {
                !(Channels::on_discord(s, Rt::Node) && Channels::on_discord(s, Rt::Rust))
            }),
            Property::sometimes("rust_polls", |m: &Channels, s: &ChState| {
                m.polls_telegram(s, Rt::Rust)
            }),
            Property::sometimes("rust_on_discord", |_, s: &ChState| {
                Channels::on_discord(s, Rt::Rust)
            }),
        ]
    }
}

pub const PROTOCOL: &str = "ownership";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let mut out = Vec::new();
    let c = Cutover {
        jobs: 2,
        runs: if deep { 3 } else { 2 },
        crashes: 1,
        fix_own_jobs_file: false,
    };
    out.push(Case::new(
        PROTOCOL,
        "P1 cutover/rollback + shared jobs file (current)",
        c.clone(),
        None,
        vec![
            ("routes_to_live_runtime", Holds),
            ("rollback_restores_node", Holds),
            ("single_channel_and_scheduler_owner", Holds),
            ("no_false_interrupted", Violated),
            ("no_lost_job_update", Violated),
            ("cut_over_with_node_job", Reached),
            ("rolled_back", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P1 cutover/rollback + fix: own jobs file, owner+boot",
        Cutover {
            fix_own_jobs_file: true,
            ..c
        },
        None,
        vec![
            ("routes_to_live_runtime", Holds),
            ("rollback_restores_node", Holds),
            ("no_false_interrupted", Holds),
            ("no_lost_job_update", Holds),
            ("cut_over_with_node_job", Reached),
            ("rolled_back", Reached),
        ],
    ));

    let sched = Scheduler {
        reminders: 2,
        plan: Scheduler::plan_as_written(),
        crashes: 1,
        fix_claim: false,
        fix_drain: false,
    };
    out.push(Case::new(
        PROTOCOL,
        "P2 scheduler handover as written",
        sched.clone(),
        None,
        vec![
            ("single_scheduler", Holds),
            ("single_reminders_writer", Violated),
            ("no_lost_reminder_record", Violated),
            ("no_double_delivery", Violated),
            ("handed_over", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P2 fixed order, batch commit (today's tick)",
        Scheduler {
            plan: Scheduler::plan_fixed(),
            fix_drain: true,
            ..sched.clone()
        },
        None,
        vec![
            ("single_scheduler", Holds),
            ("single_reminders_writer", Holds),
            ("no_lost_reminder_record", Holds),
            ("no_double_delivery", Violated),
            ("handed_over", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P2 fixed order + claim-before-send + drain",
        Scheduler {
            plan: Scheduler::plan_fixed(),
            fix_drain: true,
            fix_claim: true,
            ..sched.clone()
        },
        None,
        vec![
            ("single_scheduler", Holds),
            ("single_reminders_writer", Holds),
            ("no_lost_reminder_record", Holds),
            ("no_double_delivery", Holds),
            ("no_silent_loss", Holds),
            ("handed_over", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P2 rollback after the fixed handover",
        Scheduler {
            plan: [Scheduler::plan_fixed(), Scheduler::rollback_fixed()].concat(),
            fix_drain: true,
            fix_claim: true,
            ..sched
        },
        None,
        vec![
            ("single_scheduler", Holds),
            ("single_reminders_writer", Holds),
            ("no_lost_reminder_record", Holds),
            ("no_double_delivery", Holds),
            ("no_silent_loss", Holds),
        ],
    ));

    // Phase 3 (Telegram, lock) and phase 4 (Discord, no Node lock).
    let ch = |plan: Vec<ChStep>| Channels {
        plan,
        crashes: 1,
        lock_disabled: false,
    };
    out.push(Case::new(
        PROTOCOL,
        "P3/P4 handover + rollback as documented",
        ch(vec![
            ChStep::Rust(true, false),  // phase 3 (1): Rust poller in standby
            ChStep::Node(false, true),  // (2): Node restarted without the token
            ChStep::Node(false, false), // phase 4: Node without Discord first...
            ChStep::Rust(true, true),   // ...then Rust connects
            ChStep::Rust(false, false), // rollback: stop Rust first
            ChStep::Node(true, true),   // then restore Node
        ]),
        None,
        vec![
            ("single_telegram_poller", Holds),
            ("single_discord_gateway", Holds),
            ("rust_polls", Reached),
            ("rust_on_discord", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P4 Discord started before Node stopped (wrong order)",
        ch(vec![ChStep::Rust(false, true), ChStep::Node(true, false)]),
        None,
        vec![
            ("single_telegram_poller", Holds),
            ("single_discord_gateway", Violated),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "P3 gateway lock disabled (forbidden setting)",
        Channels {
            lock_disabled: true,
            ..ch(vec![ChStep::Rust(true, false), ChStep::Node(false, true)])
        },
        None,
        vec![("single_telegram_poller", Violated)],
    ));
    out
}

//! Confirmation and approval gates.
//!
//! - [`Pending`]: Lucina's pending-action flow (`lucina-tools/src/pending.rs`,
//!   `agent.rs::handle_call`, `lucina-server/src/chat.rs::tools_turn`, branch
//!   `lucina-rs`): a gated tool call becomes a pending action (one per
//!   session, 10-minute expiry) and runs only when a later turn of the same
//!   session is an explicit yes. MCP turns (`[via Lucina MCP…]`, session
//!   `mcp-<caller>`) must never confirm. With `LUCINA_PLANNER=tools`,
//!   `run_turn` calls `tools_turn` before any MCP check and drops `mcp_turn`.
//! - [`Attention`]: the proactive engine's gate (`lucina-proactive/src/
//!   attention.rs`, `engine.rs::tick`, branch `lucina-proactive`): quiet
//!   hours, per-kind cooldown, `budget.max_per_day` unsolicited messages,
//!   the Pulse; messages carry only links and feedback buttons.
//! - [`Meshy`]: credit spending (`lucina-meshy/src/budget.rs`, `service.rs`,
//!   `lucina-tools/src/tools/meshy.rs`, uncommitted work in
//!   `SecretaryIOS-meshy`): plan → owner's yes → reserve (check + reserve
//!   in one step) → steps → settle with the credits Meshy reports.

use stateright::{Model, Property};

use crate::{Case, Expect};

// =========================================================================
// Pending actions
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Msg {
    /// Asks for an external action (the agent calls a gated tool).
    Request,
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Turn {
    pub mcp: bool,
    pub msg: Msg,
    /// The pending action seen at the turn's start (`st.pending.current`).
    pub saw: Option<u8>,
    /// 0 = started, 1 = agent ran, reply not delivered yet.
    pub phase: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PendingState {
    /// Per session (0 = owner's app session, 1 = `mcp-<caller>`): the live
    /// pending action (id, age in ticks, created by turn, shown to the owner).
    pub pending: [Option<(u8, u8, u8, bool)>; 2],
    pub next_id: u8,
    pub turns: Vec<(u8, u8, Turn)>, // (session, turn id, turn)
    pub next_turn: u8,
    pub budget: u8,
    /// Executions: (pending id, session, confirming turn, by MCP, age, created by turn, shown).
    pub executed: Vec<(u8, u8, u8, bool, u8, u8, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PendingAction {
    Message(u8, bool, Msg),
    /// The turn's first step: look up the pending action, maybe run it.
    TurnStart(u8),
    /// The reply reaches the person.
    TurnEnd(u8),
    Tick,
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub budget: u8,
    pub ttl: u8,
    /// MCP callers send messages too.
    pub mcp_messages: bool,
    /// `LUCINA_PLANNER=tools`: tools_turn ignores the MCP marker.
    pub tools_planner: bool,
    /// Fix G1: tools_turn gets `mcp_turn`; an MCP turn never confirms (a yes
    /// is an ordinary message) and gated tools are refused for MCP callers.
    pub fix_mcp: bool,
    /// Fix G2: a pending action becomes confirmable only once the reply that
    /// asked for it was delivered (`shown`).
    pub fix_shown: bool,
}

impl Model for Pending {
    type State = PendingState;
    type Action = PendingAction;

    fn init_states(&self) -> Vec<PendingState> {
        vec![PendingState {
            pending: [None, None],
            next_id: 0,
            turns: Vec::new(),
            next_turn: 0,
            budget: self.budget,
            executed: Vec::new(),
        }]
    }

    fn actions(&self, s: &PendingState, a: &mut Vec<PendingAction>) {
        if s.budget > 0 && s.turns.len() < 2 {
            let sessions: &[(u8, bool)] = if self.mcp_messages {
                &[(0, false), (1, true)]
            } else {
                &[(0, false)]
            };
            for &(session, mcp) in sessions {
                for m in [Msg::Request, Msg::Yes] {
                    a.push(PendingAction::Message(session, mcp, m));
                }
            }
            a.push(PendingAction::Message(0, false, Msg::No));
        }
        for (_, id, t) in &s.turns {
            a.push(if t.phase == 0 {
                PendingAction::TurnStart(*id)
            } else {
                PendingAction::TurnEnd(*id)
            });
        }
        if s.pending.iter().any(Option::is_some) {
            a.push(PendingAction::Tick);
        }
    }

    fn next_state(&self, last: &PendingState, action: PendingAction) -> Option<PendingState> {
        let mut s = last.clone();
        match action {
            PendingAction::Message(session, mcp, msg) => {
                s.budget -= 1;
                let id = s.next_turn;
                s.next_turn += 1;
                s.turns.push((
                    session,
                    id,
                    Turn {
                        mcp,
                        msg,
                        saw: None,
                        phase: 0,
                    },
                ));
            }
            PendingAction::TurnStart(id) => {
                let i = s.turns.iter().position(|t| t.1 == id)?;
                let (session, _, mut turn) = s.turns[i];
                let sidx = session as usize;
                // Expired actions are dropped by current().
                if s.pending[sidx].is_some_and(|p| p.1 >= self.ttl) {
                    s.pending[sidx] = None;
                }
                let mcp_aware = !self.tools_planner || self.fix_mcp;
                let live = s.pending[sidx].filter(|p| !self.fix_shown || p.3);
                turn.saw = live.map(|p| p.0);
                match (live, turn.msg) {
                    (Some(p), Msg::Yes) if !(mcp_aware && turn.mcp) => {
                        // take() + execute(): runs once.
                        s.pending[sidx] = None;
                        s.executed.push((p.0, session, id, turn.mcp, p.1, p.2, p.3));
                    }
                    (Some(_), Msg::No) => s.pending[sidx] = None,
                    _ => {}
                }
                if turn.msg == Msg::Request && !(mcp_aware && turn.mcp && self.fix_mcp) {
                    // JSON planner: the MCP guard refuses external actions
                    // (needs_confirmation + blockedAction) for MCP turns.
                    let refused = turn.mcp && !self.tools_planner;
                    if !refused {
                        let pid = s.next_id;
                        s.next_id += 1;
                        s.pending[sidx] = Some((pid, 0, id, false));
                    }
                }
                turn.phase = 1;
                s.turns[i].2 = turn;
            }
            PendingAction::TurnEnd(id) => {
                let i = s.turns.iter().position(|t| t.1 == id)?;
                let session = s.turns[i].0 as usize;
                if let Some(p) = s.pending[session].as_mut() {
                    if p.2 == id {
                        p.3 = true; // the owner now sees "reply yes to confirm"
                    }
                }
                s.turns.remove(i);
            }
            PendingAction::Tick => {
                for p in s.pending.iter_mut().flatten() {
                    p.1 = (p.1 + 1).min(self.ttl);
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("never_in_the_requesting_turn", |_, s: &PendingState| {
                s.executed.iter().all(|e| e.2 != e.5)
            }),
            Property::always("only_owner_yes_executes", |_, s: &PendingState| {
                s.executed.iter().all(|e| !e.3 && e.1 == 0)
            }),
            Property::always("expired_never_executes", |m: &Pending, s: &PendingState| {
                s.executed.iter().all(|e| e.4 < m.ttl)
            }),
            Property::always("at_most_once", |_, s: &PendingState| {
                let mut ids: Vec<u8> = s.executed.iter().map(|e| e.0).collect();
                let n = ids.len();
                ids.sort();
                ids.dedup();
                ids.len() == n
            }),
            Property::always("confirmed_after_shown", |_, s: &PendingState| {
                s.executed.iter().all(|e| e.6)
            }),
            Property::sometimes("owner_confirmed", |_, s: &PendingState| {
                s.executed.iter().any(|e| !e.3)
            }),
        ]
    }
}

// =========================================================================
// Proactive attention budget
// =========================================================================

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AttnState {
    /// Local hour slot: 0..quiet_from = day, quiet_from.. = quiet hours.
    pub slot: u8,
    pub sent_today: u8,
    pub pulse_sent: bool,
    /// Last send slot per kind (cooldown).
    pub last_sent: [Option<u8>; 2],
    /// Candidates: (kind, decided) waiting to be gated.
    pub candidates: Vec<u8>,
    /// Engines mid-tick: (candidate kind, ledger snapshot: sent_today, last_sent of kind).
    pub ticks: Vec<(u8, u8, Option<u8>)>,
    pub sent_in_quiet: bool,
    pub cooldown_broken: bool,
    pub max_seen: u8,
    pub external_actions: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttnAction {
    Hour,
    /// A tick (daemon `run` or a concurrent `lucina-proactive once`) reads
    /// the ledger and gates one candidate.
    Gate(u8),
    /// The tick sends and records the outreach.
    Send(u8),
    Pulse,
}

#[derive(Clone, Debug)]
pub struct Attention {
    pub candidates: Vec<u8>,
    pub max_per_day: u8,
    pub cooldown: u8,
    pub slots: u8,
    pub quiet_from: u8,
    /// How many ticks may run at once (2 = daemon + `once`).
    pub concurrent_ticks: u8,
    /// Fix G3: a tick holds `pg_try_advisory_lock` for its whole run.
    pub fix_tick_lock: bool,
}

impl Model for Attention {
    type State = AttnState;
    type Action = AttnAction;

    fn init_states(&self) -> Vec<AttnState> {
        vec![AttnState {
            slot: 0,
            sent_today: 0,
            pulse_sent: false,
            last_sent: [None, None],
            candidates: self.candidates.clone(),
            ticks: Vec::new(),
            sent_in_quiet: false,
            cooldown_broken: false,
            max_seen: 0,
            external_actions: false,
        }]
    }

    fn actions(&self, s: &AttnState, a: &mut Vec<AttnAction>) {
        if s.slot + 1 < self.slots && s.ticks.is_empty() {
            a.push(AttnAction::Hour);
        }
        let limit = if self.fix_tick_lock {
            1
        } else {
            self.concurrent_ticks
        };
        if (s.ticks.len() as u8) < limit {
            for (i, _) in s.candidates.iter().enumerate() {
                a.push(AttnAction::Gate(i as u8));
            }
        }
        for i in 0..s.ticks.len() {
            a.push(AttnAction::Send(i as u8));
        }
        if !s.pulse_sent && s.slot < self.quiet_from {
            a.push(AttnAction::Pulse);
        }
    }

    fn next_state(&self, last: &AttnState, action: AttnAction) -> Option<AttnState> {
        let mut s = last.clone();
        match action {
            AttnAction::Hour => s.slot += 1,
            AttnAction::Gate(i) => {
                let kind = s.candidates.remove(i as usize);
                // attention::gate on a ledger read now (engine.rs::ledger).
                let quiet = s.slot >= self.quiet_from;
                let cooling =
                    s.last_sent[kind as usize].is_some_and(|t| s.slot < t + self.cooldown);
                let spent = s.sent_today >= self.max_per_day;
                if quiet {
                    s.candidates.push(kind); // held until quiet hours end
                } else if cooling || spent {
                    // → the Pulse (digest), nothing sent now.
                } else {
                    s.ticks
                        .push((kind, s.sent_today, s.last_sent[kind as usize]));
                }
            }
            AttnAction::Send(i) => {
                let (kind, _, _) = s.ticks.remove(i as usize);
                if s.slot >= self.quiet_from {
                    s.sent_in_quiet = true;
                }
                if s.last_sent[kind as usize].is_some_and(|t| s.slot < t + self.cooldown) {
                    s.cooldown_broken = true;
                }
                s.sent_today += 1;
                s.max_seen = s.max_seen.max(s.sent_today);
                s.last_sent[kind as usize] = Some(s.slot);
                // channels.rs: actions are URL buttons and feedback only.
            }
            AttnAction::Pulse => s.pulse_sent = true,
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("daily_budget", |m: &Attention, s: &AttnState| {
                s.max_seen <= m.max_per_day
            }),
            Property::always("quiet_hours", |_, s: &AttnState| !s.sent_in_quiet),
            Property::always("cooldown", |_, s: &AttnState| !s.cooldown_broken),
            Property::always("never_external_actions", |_, s: &AttnState| {
                !s.external_actions
            }),
            Property::sometimes("budget_used_up", |m: &Attention, s: &AttnState| {
                s.sent_today == m.max_per_day
            }),
        ]
    }
}

// =========================================================================
// Meshy credits
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MJob {
    pub est: u8,
    pub steps: u8,
    pub confirmed: bool,
    /// Credits reserved in the ledger (0 = not reserved).
    pub reserved: u8,
    pub done: u8,
    pub consumed: u8,
    pub finished: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeshyState {
    pub jobs: Vec<MJob>,
    /// Ledger: today's reserved-or-settled credits (`spent_on`).
    pub ledger: u8,
    /// Credits Meshy actually charged today.
    pub charged: u8,
    pub refused: bool,
    pub spent_unconfirmed: bool,
    pub spent_unreserved: bool,
    pub mcp_confirmed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshyAction {
    /// The owner (or an MCP caller) says yes to plan `j`.
    Confirm(u8, bool),
    /// `Service::start`: `Ledger::reserve` (check + reserve, one step).
    Reserve(u8),
    /// One Meshy task; Meshy charges between the estimate and estimate + overrun.
    StepRuns(u8, u8),
    Settle(u8),
}

#[derive(Clone, Debug)]
pub struct Meshy {
    /// (estimate per step, steps)
    pub plans: Vec<(u8, u8)>,
    pub daily_budget: u8,
    /// Meshy may charge up to this much more than the estimate per step
    /// (`consumed_credits` is what Meshy reports; pricing can change).
    pub overrun: u8,
    /// The confirming yes can come from an MCP turn (G1 not fixed).
    pub mcp_can_confirm: bool,
    /// Fix G4: reserve the worst case per step (estimate + margin, the
    /// margin bounding Meshy's drift) and, before each step, stop when the
    /// consumed credits plus the next step's worst case would pass the
    /// reservation.
    pub fix_step_guard: bool,
}

impl Meshy {
    fn worst(&self, j: &MJob) -> u8 {
        if self.fix_step_guard {
            j.est + self.overrun
        } else {
            j.est
        }
    }
}

impl Model for Meshy {
    type State = MeshyState;
    type Action = MeshyAction;

    fn init_states(&self) -> Vec<MeshyState> {
        vec![MeshyState {
            jobs: self
                .plans
                .iter()
                .map(|(e, n)| MJob {
                    est: *e,
                    steps: *n,
                    confirmed: false,
                    reserved: 0,
                    done: 0,
                    consumed: 0,
                    finished: false,
                })
                .collect(),
            ledger: 0,
            charged: 0,
            refused: false,
            spent_unconfirmed: false,
            spent_unreserved: false,
            mcp_confirmed: false,
        }]
    }

    fn actions(&self, s: &MeshyState, a: &mut Vec<MeshyAction>) {
        for (j, job) in s.jobs.iter().enumerate() {
            let j = j as u8;
            if job.finished {
                continue;
            }
            if !job.confirmed {
                a.push(MeshyAction::Confirm(j, false));
                if self.mcp_can_confirm {
                    a.push(MeshyAction::Confirm(j, true));
                }
            } else if job.reserved == 0 {
                a.push(MeshyAction::Reserve(j));
            } else if job.done < job.steps {
                for extra in 0..=self.overrun {
                    a.push(MeshyAction::StepRuns(j, job.est + extra));
                }
            } else {
                a.push(MeshyAction::Settle(j));
            }
        }
    }

    fn next_state(&self, last: &MeshyState, action: MeshyAction) -> Option<MeshyState> {
        let mut s = last.clone();
        match action {
            MeshyAction::Confirm(j, mcp) => {
                s.jobs[j as usize].confirmed = true;
                if mcp {
                    s.mcp_confirmed = true;
                }
            }
            MeshyAction::Reserve(j) => {
                let job = s.jobs[j as usize];
                let total = self.worst(&job) * job.steps;
                if s.ledger + total > self.daily_budget {
                    // OverDailyBudget: nothing spent, the job never starts.
                    s.refused = true;
                    s.jobs[j as usize].finished = true;
                } else {
                    s.ledger += total;
                    s.jobs[j as usize].reserved = total;
                }
            }
            MeshyAction::StepRuns(j, cost) => {
                let job = s.jobs[j as usize];
                if self.fix_step_guard && job.consumed + self.worst(&job) > job.reserved {
                    s.jobs[j as usize].done = job.steps; // stop; settle what was used
                    return Some(s);
                }
                if !job.confirmed {
                    s.spent_unconfirmed = true;
                }
                if job.reserved == 0 {
                    s.spent_unreserved = true;
                }
                s.charged += cost;
                s.jobs[j as usize].done += 1;
                s.jobs[j as usize].consumed += cost;
            }
            MeshyAction::Settle(j) => {
                let job = s.jobs[j as usize];
                // settle(): the entry counts `consumed` from now on.
                s.ledger = s.ledger - job.reserved + job.consumed;
                s.jobs[j as usize].finished = true;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("charged_within_daily_cap", |m: &Meshy, s: &MeshyState| {
                s.charged <= m.daily_budget
            }),
            Property::always("no_spend_without_yes", |_, s: &MeshyState| {
                !s.spent_unconfirmed
            }),
            Property::always("no_spend_without_reservation", |_, s: &MeshyState| {
                !s.spent_unreserved
            }),
            Property::always("only_owner_confirms", |_, s: &MeshyState| !s.mcp_confirmed),
            Property::sometimes("budget_refused_a_plan", |_, s: &MeshyState| s.refused),
        ]
    }
}

pub const PROTOCOL: &str = "gates";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let budget = if deep { 4 } else { 3 };
    let pending = Pending {
        budget,
        ttl: 2,
        mcp_messages: false,
        tools_planner: true,
        fix_mcp: false,
        fix_shown: false,
    };
    let attn = Attention {
        candidates: vec![0, 1, 0, 1],
        max_per_day: 3,
        cooldown: 2,
        slots: 4,
        quiet_from: 3,
        concurrent_ticks: 2,
        fix_tick_lock: false,
    };
    let meshy = Meshy {
        plans: vec![(10, 2), (20, 1)],
        daily_budget: 45,
        overrun: 3,
        mcp_can_confirm: false,
        fix_step_guard: false,
    };
    vec![
        Case::new(
            PROTOCOL,
            "G pending actions, tools planner, owner turns only",
            pending.clone(),
            None,
            vec![
                ("never_in_the_requesting_turn", Holds),
                ("only_owner_yes_executes", Holds),
                ("expired_never_executes", Holds),
                ("at_most_once", Holds),
                ("confirmed_after_shown", Violated),
                ("owner_confirmed", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G pending actions, tools planner + MCP turns (current)",
            Pending {
                mcp_messages: true,
                ..pending.clone()
            },
            None,
            vec![
                ("never_in_the_requesting_turn", Holds),
                ("only_owner_yes_executes", Violated),
                ("expired_never_executes", Holds),
                ("at_most_once", Holds),
                ("owner_confirmed", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G pending actions, tools planner + fixes G1, G2",
            Pending {
                mcp_messages: true,
                fix_mcp: true,
                fix_shown: true,
                ..pending
            },
            None,
            vec![
                ("never_in_the_requesting_turn", Holds),
                ("only_owner_yes_executes", Holds),
                ("expired_never_executes", Holds),
                ("at_most_once", Holds),
                ("confirmed_after_shown", Holds),
                ("owner_confirmed", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G attention budget, daemon + `once` (current)",
            attn.clone(),
            None,
            vec![
                ("daily_budget", Violated),
                ("quiet_hours", Holds),
                ("cooldown", Violated),
                ("never_external_actions", Holds),
                ("budget_used_up", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G attention budget + fix G3 (tick lock)",
            Attention {
                fix_tick_lock: true,
                ..attn
            },
            None,
            vec![
                ("daily_budget", Holds),
                ("quiet_hours", Holds),
                ("cooldown", Holds),
                ("never_external_actions", Holds),
                ("budget_used_up", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G Meshy credits (meshy WIP)",
            meshy.clone(),
            None,
            vec![
                ("charged_within_daily_cap", Violated),
                ("no_spend_without_yes", Holds),
                ("no_spend_without_reservation", Holds),
                ("only_owner_confirms", Holds),
                ("budget_refused_a_plan", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "G Meshy credits + tools planner MCP yes (G1 open)",
            Meshy {
                mcp_can_confirm: true,
                ..meshy.clone()
            },
            None,
            vec![("only_owner_confirms", Violated)],
        ),
        Case::new(
            PROTOCOL,
            "G Meshy credits + fix G4 (per-step guard)",
            Meshy {
                fix_step_guard: true,
                ..meshy
            },
            None,
            vec![
                ("charged_within_daily_cap", Holds),
                ("no_spend_without_yes", Holds),
                ("no_spend_without_reservation", Holds),
                ("only_owner_confirms", Holds),
                ("budget_refused_a_plan", Reached),
            ],
        ),
    ]
}

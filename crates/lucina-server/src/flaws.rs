//! Lucina's flaw tracker on the server (crate `lucina_flaws`, design `docs/design/flaws.md`):
//! `/api/secretary/flaws/*`, the dispatch loop, and the `lucina-rs flaws` CLI.
//!
//! - `GET  /flaws?q=&state=&platform=&status=&limit=`  `{items}`, worst first
//! - `POST /flaws`                     a NewFlaw → 201 `{flaw, possible_duplicates}`
//! - `GET  /flaws/matrix`              one row per live flaw, one cell per platform
//! - `GET|PATCH /flaws/{id}`           one flaw with its targets and history / a FlawPatch
//! - `PUT  /flaws/{id}/targets/{platform}`  a TargetUpdate; closing without evidence is 422
//! - `POST /flaws/{id}/dispatch`       `{platform, caller?}`: start that task now
//! - `GET  /flaws/lanes`               the dispatcher's lane breakers
//! - `POST /flaws/lanes/{lane}/clear`  a lead closes a lane's breaker
//!
//! The loop ([`tick`], every minute): check what running tasks became, start what is due
//! within the policy's caps, and tell the owner the outcomes. Starting a task never closes a
//! target: only evidence does (`lucina_flaws::check_transition`).
//!
//! What keeps the loop from storming (DISP-01, the 00:52 incident of 2026-11-19):
//! - one dispatch lock serialises cap check, claim, create and attach for the loop and for
//!   [`dispatch_one`]; the claim re-checks the counts in SQL (a second process on the file);
//! - caps count agents alive, not rows: fixing rows and what agentd lists with the label `flaw`,
//!   by agent id; at most one new dispatch per tick; no agentd answer means no dispatch;
//! - an ended agent is a lane failure (not an attempt; it opens the lane's persisted breaker)
//!   unless it provably worked, in which case it is a task failure (back-off, then `blocked` for
//!   a lead);
//! - a release or give-back stops the agent it lets go of and only touches the claim it read;
//! - the create carries an idempotency key, and an error on it looks agentd up before it gives
//!   anything back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::Response;
use chrono::Utc;
use lucina_flaws::{
    ClaimCaps, ClaimRef, Flaw, FlawError, FlawPatch, FlawState, Flaws, Gate, LaneState, ListQuery,
    NewFlaw, Platform, Policy, Step, Target, TargetStatus, TargetUpdate, Verdict,
};
use lucina_store::timeline::{Meta, NewItem};
use lucina_tools::tools::reach::{agentd_request, allowed_model};
use serde_json::{json, Value};

use crate::api::{ok, ok_status, query_param, read_json, ApiError};
use crate::AppState;

const BODY_BYTES: usize = 64 * 1024;
const LOOP_EVERY: Duration = Duration::from_secs(60);
/// A claim that never became a task (the server stopped between the two steps).
const STALE_CLAIM_MINUTES: i64 = 5;
const TRIAGE_MINUTES: i64 = 15;
/// An agent that never got past `initializing` (the provider's process didn't answer, whatever it
/// is) is given up on after this many minutes: three of them sat there for hours on 2026-11-13.
const TRIAGE_INIT_MINUTES: i64 = 5;
const ACTOR: &str = "lucina-dispatch";
/// A triage that stopped on a limit this many times is left for the owner: a flaw must not come
/// back for a new agent forever.
const GIVE_UP_AFTER: usize = 3;
/// How far agentd's hand-off chain is followed. A guard against a label that points back.
const HANDOFF_CHAIN_MAX: usize = 8;
/// On an agent agentd handed its run on to: the continuation's id
/// (`lucina_agentd::manager::handoff::CONTINUED_ON_LABEL`; the crates only meet over the socket).
/// The dispatcher's own agents carry the matching opt-out, `lucina.handoff-off`, which keeps agentd
/// from continuing a triage or a fix onto an agent of its own choosing.
const CONTINUED_ON_LABEL: &str = "lucina.continued-on";

/// `LUCINA_FLAWS*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlawSettings {
    /// `LUCINA_FLAWS=off`: no tracker.
    pub enabled: bool,
    /// `LUCINA_FLAWS_DB` (default `<data>/flaws/flaws.db`).
    pub db: PathBuf,
    pub policy: Policy,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/home/nez"), PathBuf::from)
}

impl FlawSettings {
    /// `Err`: the chain the environment asks for can't be run, or the number of triage steps isn't
    /// one, which stops the server rather than quietly dispatching on another agent.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        data_dir: &Path,
    ) -> Result<Self, String> {
        let val = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let policy = Policy::from_lookup(&get, &home())?;
        // A setting that was invalid and replaced by its default: one warning, and the server runs.
        for note in &policy.notes {
            tracing::warn!("flaw tracker: {note}");
        }
        Ok(Self {
            enabled: !matches!(val("LUCINA_FLAWS").as_deref(), Some("off" | "false" | "0")),
            db: val("LUCINA_FLAWS_DB")
                .map_or_else(|| data_dir.join("flaws").join("flaws.db"), PathBuf::from),
            policy,
        })
    }
}

/// A database that can't be opened turns the tracker off with a warning (never a failed start).
pub fn open(s: &FlawSettings) -> Option<Arc<Flaws>> {
    if !s.enabled {
        return None;
    }
    match Flaws::open(&s.db) {
        Ok(f) => {
            tracing::info!(db = %s.db.display(), auto = s.policy.auto_dispatch, "flaw tracker: on");
            Some(Arc::new(f))
        }
        Err(e) => {
            tracing::warn!(db = %s.db.display(), error = %e, "flaw tracker: off");
            None
        }
    }
}

fn api_error(e: FlawError) -> ApiError {
    match &e {
        FlawError::Db(_) => ApiError::new(500, "internal_error", e.to_string()),
        FlawError::NotFound => ApiError::new(404, e.code(), e.to_string()),
        FlawError::EvidenceRequired(_) => ApiError::new(422, e.code(), e.to_string()),
        FlawError::BadTransition { .. } | FlawError::NotClaimable(_) | FlawError::Busy(_) => {
            ApiError::new(409, e.code(), e.to_string())
        }
        FlawError::Invalid(_) => ApiError::new(400, e.code(), e.to_string()),
    }
}

fn redacted(mut v: Value) -> Value {
    lucina_secrets::redactor().redact_json(&mut v);
    v
}

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn list_query(uri: &Uri) -> Result<ListQuery, ApiError> {
    let param = |k: &str| query_param(uri, k).filter(|v| !v.trim().is_empty());
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(
            l.trim()
                .parse::<u32>()
                .map_err(|_| ApiError::bad_request("limit must be a number"))?,
        ),
    };
    Ok(ListQuery {
        q: param("q"),
        state: param("state")
            .map(|s| FlawState::parse(&s))
            .transpose()
            .map_err(api_error)?,
        platform: param("platform")
            .map(|s| Platform::parse(&s))
            .transpose()
            .map_err(api_error)?,
        status: param("status")
            .map(|s| TargetStatus::parse(&s))
            .transpose()
            .map_err(api_error)?,
        limit,
    })
}

/// `/api/secretary/flaws/...` (`parts` after `flaws`).
pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    let flaws = st.flaws.clone().ok_or_else(|| {
        ApiError::new(
            404,
            "flaws_disabled",
            "The flaw tracker is off (LUCINA_FLAWS)",
        )
    })?;
    let only = |allowed: &[&str]| -> Result<(), ApiError> {
        if allowed.contains(&method.as_str()) {
            Ok(())
        } else {
            Err(ApiError::method_not_allowed(allowed))
        }
    };
    let actor = crate::computer::caller(headers);
    let id_ok = |id: &str| -> Result<(), ApiError> {
        if lucina_flaws::store::valid_id(id) {
            Ok(())
        } else {
            Err(api_error(FlawError::NotFound))
        }
    };
    match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            only(&["GET", "POST"])?;
            if *method == Method::GET {
                let items = flaws.list(&list_query(uri)?).map_err(api_error)?;
                return Ok(ok(redacted(json!({"items": items}))));
            }
            let new: NewFlaw = parse(read_json(body, BODY_BYTES).await?)?;
            let r = flaws.report(new, &actor).map_err(api_error)?;
            Ok(ok_status(201, redacted(json!(r))))
        }
        ["matrix"] => {
            only(&["GET"])?;
            Ok(ok(redacted(flaws.matrix().map_err(api_error)?)))
        }
        ["lanes"] => {
            only(&["GET"])?;
            let lanes = flaws.lanes().map_err(api_error)?;
            Ok(ok(redacted(json!({"lanes": lanes}))))
        }
        ["lanes", lane, "clear"] => {
            only(&["POST"])?;
            let cleared = flaws.lane_close(lane).map_err(api_error)?;
            tracing::info!(lane = %lane, cleared, actor = %actor, "flaws: a lead cleared the lane breaker");
            Ok(ok(json!({"lane": lane, "cleared": cleared})))
        }
        [id] => {
            only(&["GET", "PATCH"])?;
            id_ok(id)?;
            if *method == Method::GET {
                return Ok(ok(redacted(json!(flaws.get(id).map_err(api_error)?))));
            }
            let p: FlawPatch = parse(read_json(body, BODY_BYTES).await?)?;
            Ok(ok(redacted(json!(flaws
                .patch(id, p, &actor)
                .map_err(api_error)?))))
        }
        [id, "targets", platform] => {
            only(&["PUT"])?;
            id_ok(id)?;
            let p = Platform::parse(platform).map_err(api_error)?;
            let u: TargetUpdate = parse(read_json(body, BODY_BYTES).await?)?;
            Ok(ok(redacted(json!(flaws
                .set_target(id, p, u, &actor)
                .map_err(api_error)?))))
        }
        [id, "dispatch"] => {
            only(&["POST"])?;
            id_ok(id)?;
            let v = read_json(body, 4096).await?;
            let p = Platform::parse(v.get("platform").and_then(Value::as_str).unwrap_or(""))
                .map_err(api_error)?;
            // Who asked, for the log line: the loop never comes through here.
            let caller = match v.get("caller").and_then(Value::as_str) {
                Some("mcp") => "mcp",
                _ => "post",
            };
            let hands = StateHands { st: st.clone() };
            let t = dispatch_one_as(&flaws, &st.cfg.flaws.policy, &hands, id, p, &actor, caller)
                .await?;
            Ok(ok_status(202, redacted(json!(t))))
        }
        _ => Err(ApiError::not_found()),
    }
}

/// An agent agentd lists as the dispatcher's (the label `flaw`) and not ended: initializing,
/// running or idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alive {
    pub id: String,
    /// The lane it runs on (the label `flaw_lane`), when it carries one.
    pub lane: Option<String>,
    pub flaw: Option<String>,
    pub platform: Option<String>,
    /// A triage agent, not a fix.
    pub triage: bool,
    /// Its status is `idle`: it ended its turn and works at nothing.
    pub idle: bool,
}

/// What the dispatcher does to the world, so a test can stand in for agentd and the owner.
#[async_trait::async_trait]
pub trait Hands: Send + Sync {
    /// A task worktree: `(workspace id, path)`.
    async fn start_task(
        &self,
        title: &str,
        repo: &str,
        branch: &str,
        flaw: &str,
        platform: Option<Platform>,
    ) -> Result<(String, String), String>;
    /// An agent working in `cwd` on one step of the chain: its id. `key`: agentd's create dedups
    /// by it, so a create whose answer was lost and is asked again is one agent, not two.
    #[allow(clippy::too_many_arguments)]
    async fn start_agent(
        &self,
        policy: &Policy,
        step: &Step,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: Value,
        key: Option<&str>,
    ) -> Result<String, String>;
    /// The agent's status; `Ok(None)`: it no longer exists; `Err`: agentd can't say now.
    async fn agent_status(&self, id: &str) -> Result<Option<String>, String>;
    async fn stop_agent(&self, id: &str);
    /// The agents agentd has for this flaw's triage that are not archived, whatever their status:
    /// the one the flaw store tracks, and the one a quota hand-off started in its place (it carries
    /// the same labels). `Err`: agentd can't say now.
    async fn triage_agents(&self, flaw: &str) -> Result<Vec<String>, String>;
    /// The agents agentd has for this flaw's fix on this platform that are not archived, whatever
    /// their status (the label pair `flaw` + `platform`). `Err`: agentd can't say now.
    async fn fix_agents(&self, flaw: &str, platform: Platform) -> Result<Vec<String>, String>;
    /// Every agent of the dispatcher that is alive (initializing, running or idle), triage
    /// included: one list. `Err`: agentd can't say now, and nothing is dispatched.
    async fn alive_agents(&self) -> Result<Vec<Alive>, String>;
    /// The agent agentd handed `id`'s run on to, when it did.
    async fn continued_on(&self, id: &str) -> Option<String>;
    /// Put an agent away for good, so it holds neither a process nor a lane while it does nothing.
    /// `Err`: agentd wouldn't.
    async fn archive_agent(&self, id: &str) -> Result<(), String>;
    /// What the task's merge landed, if it did.
    async fn merged_sha(&self, workspace: &str) -> Result<Option<String>, String>;
    /// Ask through `lucina_decide`: the decision's id.
    async fn ask_owner(&self, flaw: &Flaw, gate: Gate) -> Result<String, String>;
    /// `Some(true)`: go; `Some(false)`: don't; `None`: not answered.
    async fn decision(&self, id: &str) -> Option<bool>;
    /// The agent's error, if it ended on one.
    async fn agent_error(&self, id: &str) -> Option<String>;
    /// Did the agent provably work: at least one tool call on its timeline, or a commit or a
    /// change in its workspace? Only then is an ended agent's failure the task's.
    async fn agent_worked(&self, agent: &str, workspace: Option<&str>) -> bool;
    /// Does this step's lane have quota? A step with no probe lane is always ready. Nothing
    /// starts on a step that doesn't; the chain goes down, never up. May answer from a cache.
    async fn step_ready(&self, step: &Step) -> bool;
    /// One fresh probe of a lane, for a lane whose breaker is open. A probe that cannot run or
    /// that times out is not ready.
    async fn probe_lane(&self, lane: &str) -> bool;
    /// An agent just failed on this step's lane: forget any answer cached about it.
    fn step_out(&self, step: &Step, why: &str);
    /// A lane's breaker just opened: one record in the ops record.
    async fn record_lane_open(&self, lane: &str, why: &str);
    /// Tell him, in his chat and on his phone. `false`: try again next tick.
    async fn tell(&self, text: &str) -> bool;
    /// A line on his timeline that an attempt started, and on which agent. The timeline only: he
    /// hears on the phone about what came of it, not about every start.
    async fn tell_start(&self, text: &str);
    /// The deadlines of the calls made under the dispatch lock.
    fn limits(&self) -> Limits {
        Limits::DEFAULT
    }
}

// ---------------------------------------------------------------- the dispatch lock

/// The deadline of every call made while the dispatch lock is held, so the lock cannot be held
/// for longer than [`MAX_HOLD`] whatever agentd or a probe does. Tests give the fake hands
/// short ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// How long a request (`POST /dispatch`, the MCP tool) waits for the lock before answering busy.
    pub lock_wait: Duration,
    /// `GET /agents`.
    pub list: Duration,
    /// One stop or one archive of an agent.
    pub away: Duration,
    /// The worktree create and the agent create, each.
    pub create: Duration,
    /// The lookup of a target's agents by labels, after a create that errored.
    pub lookup: Duration,
    /// The ops record of an opened breaker.
    pub record: Duration,
    /// One lane probe.
    pub probe_lane: Duration,
    /// All the probes of one hold together.
    pub probe_budget: Duration,
}

/// Agents put away (stopped and archived) in one hold for the sweep of idle agents and the
/// untracked agents of the target started on; the extras after a failed create are another
/// [`MAX_AWAY`]. What is left waits for the next tick.
pub const MAX_AWAY: usize = 3;

impl Limits {
    pub const DEFAULT: Limits = Limits {
        lock_wait: Duration::from_secs(5),
        list: Duration::from_secs(5),
        away: Duration::from_secs(5),
        create: Duration::from_secs(25),
        lookup: Duration::from_secs(5),
        record: Duration::from_secs(5),
        probe_lane: Duration::from_secs(10),
        probe_budget: Duration::from_secs(15),
    };

    /// The longest one hold can last: the list, the sweep and the untracked agents (`MAX_AWAY`
    /// of them, each stopped and archived), all the probes, the two creates, and then the longest
    /// of what follows a failed create (the lookup, `MAX_AWAY` extras put away, the ops record).
    /// 5 + 30 + 15 + 50 + (5 + 40) = 145 s with the defaults.
    pub const fn max_hold(&self) -> Duration {
        let one = self.away.as_secs() * 2;
        let away = one * MAX_AWAY as u64;
        // After a failed create: the lookup, then either the extras and the found agent put away
        // (MAX_AWAY + 1 of them, the longest) or the ops record of an opened lane (shorter).
        Duration::from_secs(
            self.list.as_secs()
                + away
                + self.probe_budget.as_secs()
                + 2 * self.create.as_secs()
                + self.lookup.as_secs()
                + one * (MAX_AWAY as u64 + 1),
        )
    }
}

/// The worst-case time the dispatch lock is held (`Limits::DEFAULT`): 145 s. A hold is
/// milliseconds when agentd and the lanes answer; this is what it can take when none does.
pub const MAX_HOLD: Duration = Limits::DEFAULT.max_hold();

/// A call under the lock, cut at its limit: `None`: it did not answer in time.
async fn within<T>(limit: Duration, fut: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::time::timeout(limit, fut).await.ok()
}

/// The lock of one flaw store: the cap check, the claim, the create and the attach of a dispatch
/// are one step, for the loop and for `dispatch_one` alike. (The claim itself re-checks the
/// counts in SQL, which covers a second process on the same file.) A request waits for it a
/// bounded time and answers busy; the minute loop never waits, it skips the tick.
struct DispatchLock {
    m: Arc<tokio::sync::Mutex<()>>,
    since: std::sync::Mutex<Option<std::time::Instant>>,
}

/// The lock, held. It says it is free again when dropped.
struct Held {
    _g: tokio::sync::OwnedMutexGuard<()>,
    lock: Arc<DispatchLock>,
}

impl Drop for Held {
    fn drop(&mut self) {
        *self.lock.since.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl DispatchLock {
    fn held(self: &Arc<Self>, g: tokio::sync::OwnedMutexGuard<()>) -> Held {
        *self.since.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        Held {
            _g: g,
            lock: self.clone(),
        }
    }

    /// `Err(held for)`: it was not free within `wait`.
    async fn acquire(self: &Arc<Self>, wait: Duration) -> Result<Held, Duration> {
        match tokio::time::timeout(wait, self.m.clone().lock_owned()).await {
            Ok(g) => Ok(self.held(g)),
            Err(_) => Err(self.held_for()),
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<Held> {
        self.m.clone().try_lock_owned().ok().map(|g| self.held(g))
    }

    fn held_for(&self) -> Duration {
        self.since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map_or(Duration::ZERO, |t| t.elapsed())
    }
}

fn dispatch_lock(flaws: &Flaws) -> Arc<DispatchLock> {
    type Locks = std::sync::Mutex<Vec<(usize, Arc<DispatchLock>)>>;
    static LOCKS: Locks = std::sync::Mutex::new(Vec::new());
    let key = flaws as *const Flaws as usize;
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, m)) = locks.iter().find(|(k, _)| *k == key) {
        return m.clone();
    }
    let m = Arc::new(DispatchLock {
        m: Arc::new(tokio::sync::Mutex::new(())),
        since: std::sync::Mutex::new(None),
    });
    locks.push((key, m.clone()));
    m
}

/// Say something once per change of state, not once per tick: `true` when `state` is not what
/// was last said for `key` on this store (`ok` is the quiet state nothing is said about at
/// first).
fn changed(flaws: &Flaws, key: &str, state: &str) -> bool {
    type Said = std::sync::Mutex<HashMap<(usize, String), String>>;
    static SAID: std::sync::OnceLock<Said> = std::sync::OnceLock::new();
    let said = SAID.get_or_init(Default::default);
    let mut said = said.lock().unwrap_or_else(|e| e.into_inner());
    let k = (flaws as *const Flaws as usize, key.to_owned());
    let was = said.get(&k).map_or("ok", String::as_str);
    if was == state {
        return false;
    }
    said.insert(k, state.to_owned());
    true
}

/// The lane a step runs on, the key of its breaker: its probe lane, or its label when it has none.
fn lane_key(step: &Step) -> String {
    step.probe_lane.clone().unwrap_or_else(|| step.label())
}

fn caps_of(policy: &Policy) -> ClaimCaps {
    ClaimCaps {
        per_repo: policy.max_running_per_repo,
        total: policy.max_alive,
        per_lane: policy.max_per_lane,
    }
}

/// What this tick has learned about lanes, so a lane is probed once however many picks ask.
#[derive(Default)]
pub struct Probes {
    ready: HashMap<String, bool>,
    fresh: HashMap<String, bool>,
    /// When the lock was taken: all the probes of one hold share one budget from here.
    held_since: Option<std::time::Instant>,
    /// Lanes whose breaker opened in this tick: not healed in the tick that opened them.
    opened_now: HashSet<String>,
    /// A request (`dispatch_one`), not the loop: its probes are its own, so it also leaves a
    /// lane alone for a minute after it opened, whichever tick or request opened it.
    request: bool,
}

impl Probes {
    /// The lock was just taken: the probe budget starts now.
    fn start_hold(&mut self) {
        self.held_since = Some(std::time::Instant::now());
    }

    /// What one more probe may take: a lane's limit, and what the hold's budget has left.
    fn left(&self, l: &Limits) -> Duration {
        match self.held_since {
            None => l.probe_lane,
            Some(t) => l.probe_budget.saturating_sub(t.elapsed()).min(l.probe_lane),
        }
    }

    /// A probe that does not answer in its time, or that the budget has no time left for, is not
    /// ready.
    async fn ready(&mut self, hands: &dyn Hands, step: &Step) -> bool {
        let key = lane_key(step);
        if let Some(r) = self.ready.get(&key) {
            return *r;
        }
        let wait = self.left(&hands.limits());
        let r = !wait.is_zero() && within(wait, hands.step_ready(step)).await.unwrap_or(false);
        self.ready.insert(key, r);
        r
    }

    async fn fresh(&mut self, hands: &dyn Hands, lane: &str) -> bool {
        if let Some(r) = self.fresh.get(lane) {
            return *r;
        }
        let wait = self.left(&hands.limits());
        let r = !wait.is_zero() && within(wait, hands.probe_lane(lane)).await.unwrap_or(false);
        self.fresh.insert(lane.to_owned(), r);
        r
    }
}

/// Did this lane's breaker open in the last minute?
fn opened_recently(flaws: &Flaws, lane: &str) -> bool {
    flaws
        .lanes()
        .ok()
        .and_then(|ls| ls.into_iter().find(|b| b.lane == lane))
        .and_then(|b| b.opened_at)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
        .is_some_and(|t| flaws.clock_now().signed_duration_since(t).num_seconds() < 60)
}

/// Is this step's lane usable now? Closed: what its (cached) quota probe says. Open: a fresh
/// probe, and a pass makes it half-open. Half-open: yes, for the one trial agent the claim lets
/// through. A breaker that cannot be read is open.
async fn lane_usable(flaws: &Flaws, hands: &dyn Hands, step: &Step, probes: &mut Probes) -> bool {
    let key = lane_key(step);
    match flaws.lane_state(&key).unwrap_or(LaneState::Open) {
        LaneState::Closed => probes.ready(hands, step).await,
        LaneState::HalfOpen => true,
        LaneState::Open => {
            if probes.opened_now.contains(&key) || (probes.request && opened_recently(flaws, &key))
            {
                return false;
            }
            if probes.fresh(hands, &key).await {
                if flaws.lane_half_open(&key).unwrap_or(false) {
                    tracing::info!(lane = %key, "flaws: the lane's probe passed; one agent may try it");
                }
                true
            } else {
                false
            }
        }
    }
}

/// Each open lane is probed on every tick. A passing probe makes it half-open; one that fails,
/// cannot run or times out leaves it open, and is said once per change.
async fn heal_lanes(flaws: &Flaws, hands: &dyn Hands, probes: &mut Probes) {
    for b in flaws.lanes().unwrap_or_default() {
        if b.state != LaneState::Open || probes.opened_now.contains(&b.lane) {
            continue;
        }
        let ok = probes.fresh(hands, &b.lane).await;
        if changed(
            flaws,
            &format!("probe:{}", b.lane),
            if ok { "ok" } else { "not ready" },
        ) {
            if ok {
                tracing::info!(lane = %b.lane, "flaws: the open lane's probe passes now");
            } else {
                tracing::warn!(lane = %b.lane, "flaws: the open lane's probe is not ready (or could not run); asked again next tick");
            }
        }
        if ok && flaws.lane_half_open(&b.lane).unwrap_or(false) {
            tracing::info!(lane = %b.lane, "flaws: the lane is half-open: one agent may try it");
        }
    }
}

/// A lane failed: open its breaker, forget what was cached about it, and say so once in the ops
/// record. A breaker that is open already writes nothing again.
async fn open_lane(flaws: &Flaws, hands: &dyn Hands, lane: &str, why: &str, probes: &mut Probes) {
    hands.step_out(&Step::new("lane", None, Some(lane)), why);
    match flaws.lane_open(lane, why) {
        Ok(true) => {
            // Not healed in this tick: the probe that passes right after a failure says little.
            probes.opened_now.insert(lane.to_owned());
            tracing::warn!(lane, why, "flaws: this lane failed; its breaker is open and nothing is dispatched on it until a probe passes");
            if within(hands.limits().record, hands.record_lane_open(lane, why))
                .await
                .is_none()
            {
                tracing::warn!(
                    lane,
                    "flaws: the ops record of the opened lane did not answer in time"
                );
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(lane, error = %e, "flaws: the lane breaker could not be opened"),
    }
}

/// The agents the dispatcher has alive, counted by agent id across what the store tracks (fixing
/// rows, attached or not, and triage agents) and what agentd lists.
#[derive(Default)]
struct Load {
    ids: HashSet<String>,
    total: usize,
    per_lane: HashMap<String, usize>,
    per_repo: HashMap<String, usize>,
}

impl Load {
    fn of(flaws: &Flaws, policy: &Policy, alive: &[Alive]) -> Self {
        let triage_repo = policy.secretary_repo.display().to_string();
        let mut load = Load::default();
        for (flaw, t) in flaws.in_progress().unwrap_or_default() {
            if t.session.is_some() {
                continue;
            }
            let key = t
                .agent
                .clone()
                .unwrap_or_else(|| format!("claim:{}:{}", flaw.id, t.platform));
            load.add(key, t.lane.as_deref(), t.repo.as_deref());
        }
        for f in flaws.triaging().unwrap_or_default() {
            if let Some(a) = f.triage_agent {
                load.add(a, None, Some(&triage_repo));
            }
        }
        for a in alive {
            load.add(
                a.id.clone(),
                a.lane.as_deref(),
                a.triage.then_some(triage_repo.as_str()),
            );
        }
        load
    }

    fn add(&mut self, key: String, lane: Option<&str>, repo: Option<&str>) {
        if !self.ids.insert(key) {
            return;
        }
        self.total += 1;
        if let Some(l) = lane {
            *self.per_lane.entry(l.to_owned()).or_insert(0) += 1;
        }
        if let Some(r) = repo {
            *self.per_repo.entry(r.to_owned()).or_insert(0) += 1;
        }
    }

    /// Why one more agent on this repo and lane may not start, if it may not.
    fn refusal(&self, policy: &Policy, repo: &str, lane: &str, lane_cap: usize) -> Option<String> {
        if self.total >= policy.max_alive {
            return Some(format!(
                "cap: {} agents already alive in all",
                policy.max_alive
            ));
        }
        if self.per_lane.get(lane).copied().unwrap_or(0) >= lane_cap {
            return Some(format!(
                "cap: {lane_cap} agent already alive on lane {lane}"
            ));
        }
        if self.per_repo.get(repo).copied().unwrap_or(0) >= policy.max_running_per_repo {
            return Some(format!(
                "cap: {} agents already alive in repo {repo}",
                policy.max_running_per_repo
            ));
        }
        None
    }
}

/// The lane's own cap: one while it is half-open, however many the policy allows otherwise.
fn lane_cap(flaws: &Flaws, policy: &Policy, lane: &str) -> usize {
    match flaws.lane_state(lane) {
        Ok(LaneState::HalfOpen) => 1,
        _ => policy.max_per_lane,
    }
}

/// One `GET /agents`, or no dispatch: agentd that cannot be asked is said once per change.
async fn ask_alive(flaws: &Flaws, hands: &dyn Hands) -> Result<Vec<Alive>, String> {
    let answer = match within(hands.limits().list, hands.alive_agents()).await {
        Some(a) => a,
        None => Err("agentd did not answer the list of agents in time".to_owned()),
    };
    match answer {
        Ok(a) => {
            if changed(flaws, "agentd", "ok") {
                tracing::info!("flaws: agentd answers again; dispatch resumes");
            }
            Ok(a)
        }
        Err(e) => {
            if changed(flaws, "agentd", "down") {
                tracing::warn!(error = %e, "flaws: agentd can't be asked which agents are alive; nothing is dispatched until it answers");
            }
            Err(e)
        }
    }
}

/// The `agents` array of agentd's list. An answer without one is not "no agents": it is not an
/// answer, and nothing is decided on it.
fn agent_list(v: &Value) -> Result<&Vec<Value>, String> {
    v["agents"]
        .as_array()
        .ok_or_else(|| "agentd answered without a list of agents".to_owned())
}

/// The ops record's key for one opening of a lane: how many times it has opened, never a time.
fn lane_ops_key(lane: &str, opens: i64) -> String {
    format!("flaw-lane:{lane}:open-{opens}")
}

/// An agent put away: stopped, then archived, each within its limit. `true`: the stop was
/// answered. Neither failing is the end of it for an agent nobody tracks, but a start on its
/// target waits for it.
async fn put_away(hands: &dyn Hands, agent: &str) -> bool {
    let l = hands.limits();
    let stopped = within(l.away, hands.stop_agent(agent)).await.is_some();
    if !stopped {
        tracing::warn!(agent, "flaws: agentd did not answer the stop in time");
    }
    match within(l.away, hands.archive_agent(agent)).await {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            tracing::warn!(agent, error = %e, "flaws: the agent was stopped but not archived")
        }
        None => tracing::warn!(agent, "flaws: agentd did not answer the archive in time"),
    }
    stopped
}

/// How many more agents this hold may put away.
struct Away(usize);

/// Fixing agents that ended their turn and are idle, on a target that no longer holds them (it
/// is `fixed`, closed, or someone else's now): finished, they hold a process and count against
/// every cap for nothing, so they are put away and leave the list this dispatch is decided on.
/// At most the hold's budget of them; the rest wait for the next tick.
async fn sweep_idle(flaws: &Flaws, hands: &dyn Hands, alive: &mut Vec<Alive>, away: &mut Away) {
    let mut gone = Vec::new();
    for a in alive.iter().filter(|a| a.idle && !a.triage) {
        if away.0 == 0 {
            break;
        }
        let (Some(flaw), Some(platform)) = (a.flaw.as_deref(), a.platform.as_deref()) else {
            continue;
        };
        let Ok(platform) = Platform::parse(platform) else {
            continue;
        };
        let holds = flaws.get(flaw).is_ok_and(|d| {
            d.targets.iter().any(|t| {
                t.platform == platform
                    && t.status == TargetStatus::Fixing
                    && t.agent.as_deref() == Some(a.id.as_str())
            })
        });
        if !holds {
            away.0 -= 1;
            if put_away(hands, &a.id).await {
                gone.push(a.id.clone());
            }
        }
    }
    alive.retain(|a| !gone.contains(&a.id));
}

/// An agent that works on a target the store says nobody holds: it cannot be tracked, so it goes
/// before a new one starts. `alive`: the list this dispatch was decided on. `false`: one could
/// not be stopped in time (or the hold's budget is spent), and nothing starts on the target now.
async fn clear_orphans(
    flaws: &Flaws,
    hands: &dyn Hands,
    alive: &[Alive],
    flaw: &str,
    p: Platform,
    away: &mut Away,
) -> bool {
    let held = flaws
        .get(flaw)
        .ok()
        .and_then(|d| d.targets.into_iter().find(|t| t.platform == p))
        .filter(|t| t.status == TargetStatus::Fixing)
        .and_then(|t| t.agent);
    for a in alive.iter().filter(|a| {
        !a.triage
            && a.flaw.as_deref() == Some(flaw)
            && a.platform.as_deref() == Some(p.as_str())
            && held.as_deref() != Some(a.id.as_str())
    }) {
        if away.0 == 0 {
            return false;
        }
        away.0 -= 1;
        tracing::warn!(flaw, platform = %p, agent = %a.id, "flaws: an agent nobody tracks already works on this target; stopping it first");
        if !put_away(hands, &a.id).await {
            return false;
        }
    }
    true
}

fn dispatched_line(
    flaw: &str,
    p: Platform,
    caller: &str,
    attempt: u32,
    lane: &str,
    agent: &str,
) -> String {
    format!(
        "flaws: dispatched flaw={flaw} platform={p} caller={caller} attempt={attempt} lane={lane} agent={agent}"
    )
}

fn refused_line(flaw: &str, p: Platform, caller: &str, reason: &str) -> String {
    format!("flaws: dispatch refused flaw={flaw} platform={p} caller={caller} reason={reason}")
}

/// Who is dispatching, and the counts the claim re-checks.
pub struct StartCtx<'a> {
    /// `loop`, `post` or `mcp`.
    pub caller: &'a str,
    pub caps: ClaimCaps,
}

/// An error answering a create that means the provider or the host refuses new agents: agentd's
/// breaker (`503 PROVIDER_TRIPPED`) or its create caps (`429 AGENT_LIMIT`).
fn create_lane_failure(why: &str) -> bool {
    let w = why.to_lowercase();
    w.starts_with("agentd 503")
        || w.starts_with("agentd 429")
        || w.contains("provider_tripped")
        || w.contains("agent_limit")
        // A create that did not answer in time: agentd may finish it later, and a retry every
        // tick would start and stop an agent for ever. It is the lane's failure.
        || w.starts_with("agentd did not answer")
}

/// Claim, make the worktree, start the agent, in that order. The claim re-checks the caps in SQL;
/// a refusal is a clean `busy` and nothing starts. The create carries an idempotency key. A create
/// that errors looks agentd up by its labels before anything is given back: an agent made
/// although the answer was lost is attached, not orphaned.
#[allow(clippy::too_many_arguments)]
pub async fn start_fix(
    flaws: &Flaws,
    policy: &Policy,
    step: &Step,
    hands: &dyn Hands,
    pick_flaw: &Flaw,
    target: &Target,
    actor: &str,
    ctx: &StartCtx<'_>,
    probes: &mut Probes,
) -> Result<Target, ApiError> {
    let p = target.platform;
    let id = pick_flaw.id.as_str();
    let layer = target.layer.ok_or_else(|| {
        api_error(FlawError::NotClaimable(format!(
            "{id} on {p} has no layer yet"
        )))
    })?;
    let repo = policy.repo_for(p, layer).display().to_string();
    let lane = lane_key(step);
    let claimed = match flaws.claim_capped(id, p, &repo, Some(&lane), &ctx.caps, actor) {
        Ok(c) => c,
        Err(e) => {
            tracing::info!("{}", refused_line(id, p, ctx.caller, &e.to_string()));
            return Err(api_error(e));
        }
    };
    let short = id.trim_start_matches("fl-");
    // The time: a task whose agent failed to start leaves its branch behind for the sweep.
    let branch = format!(
        "flaw-{short}-{p}-{}-{}",
        claimed.attempts,
        Utc::now().format("%H%M%S")
    );
    let title = format!("Flaw {id} on {p}: {}", clip(&pick_flaw.title, 60));
    // One key per claim, never reused: a give-back keeps the attempt number, so it is the claim's
    // own sequence number that makes the key new.
    let key = format!("flaw:{id}:{p}:{}", claimed.claim_seq);
    let started = async {
        allowed_model(&step.provider, step.model.as_deref()).map_err(|e| (None, e))?;
        let lim = hands.limits();
        let late = |what: &str| format!("agentd did not answer the {what} within {} s", lim.create.as_secs());
        let (ws, cwd) = within(lim.create, hands.start_task(&title, &repo, &branch, id, Some(p)))
            .await
            .ok_or_else(|| (None, late("worktree create")))?
            .map_err(|e| (None, e))?;
        let prompt = policy.brief(pick_flaw, &claimed, &cwd);
        let labels = json!({"from": "lucina", "flaw": id, "platform": p, "flaw_lane": lane, "lucina.handoff-off": "1"});
        let agent = within(
            lim.create,
            hands.start_agent(policy, step, &cwd, &prompt, &title, labels, Some(&key)),
        )
        .await
        .ok_or_else(|| (Some(ws.clone()), late("agent create")))?
        .map_err(|e| (Some(ws.clone()), e))?;
        Ok::<_, (Option<String>, String)>((ws, agent))
    }
    .await;
    let claim = claimed.claim_ref();
    match started {
        Ok((ws, agent)) => match flaws.attach(id, p, &ws, &agent, step, actor) {
            Ok(t) => {
                tracing::info!(
                    "{}",
                    dispatched_line(id, p, ctx.caller, t.attempts, &lane, &agent)
                );
                hands
                    .tell_start(&lucina_flaws::policy::start_line(pick_flaw, &t, step))
                    .await;
                Ok(t)
            }
            Err(e) => {
                // The claim was let go while the agent was being made: nobody tracks it.
                tracing::warn!(flaw = id, platform = %p, agent = %agent, error = %e, "flaws: the claim is gone; stopping the agent it was made for");
                put_away(hands, &agent).await;
                Err(api_error(e))
            }
        },
        Err((ws, why)) => {
            tracing::warn!(flaw = id, platform = %p, error = %why, "flaws: task not started");
            // agentd may have made the agent although the answer was lost or late.
            let looked = within(hands.limits().lookup, hands.fix_agents(id, p))
                .await
                .unwrap_or_else(|| Err("agentd did not answer the lookup in time".to_owned()));
            match looked {
                Ok(found) if !found.is_empty() => {
                    for extra in found[1..].iter().take(MAX_AWAY) {
                        put_away(hands, extra).await;
                    }
                    let agent = &found[0];
                    return match flaws.attach(
                        id,
                        p,
                        ws.as_deref().unwrap_or(""),
                        agent,
                        step,
                        actor,
                    ) {
                        Ok(t) => {
                            tracing::info!(
                                "{}",
                                dispatched_line(id, p, ctx.caller, t.attempts, &lane, agent)
                            );
                            hands
                                .tell_start(&lucina_flaws::policy::start_line(pick_flaw, &t, step))
                                .await;
                            Ok(t)
                        }
                        Err(e) => {
                            put_away(hands, agent).await;
                            Err(api_error(e))
                        }
                    };
                }
                Ok(_) => {}
                Err(e) => {
                    // Not knowing is not a reason to give the claim back: the stale-claim sweep
                    // looks again, and a second agent never starts on a guess.
                    tracing::warn!(flaw = id, platform = %p, error = %e, "flaws: agentd can't say whether it made the agent; the claim waits for the sweep");
                    return Err(ApiError::new(503, "dispatch_failed", why));
                }
            }
            let lane_failure = create_lane_failure(&why);
            let _ = flaws.give_back_if(id, p, &claim, &why, lane_failure, actor);
            if lane_failure {
                open_lane(flaws, hands, &lane, &why, probes).await;
            }
            Err(ApiError::new(503, "dispatch_failed", why))
        }
    }
}

/// The step this attempt runs: its own, or the first one further down whose lane is usable.
/// `None`: no step is ready, so nothing starts for this target on this tick.
async fn choose_step<'a>(
    flaws: &Flaws,
    hands: &dyn Hands,
    policy: &'a Policy,
    attempt: u32,
    probes: &mut Probes,
) -> Option<&'a Step> {
    // Probed as far as it takes: the steps above this attempt's own are never asked, and once one
    // says ready the ones below it are not either. `step_for` walks the same rule over what was
    // answered, and reads a step that wasn't probed as not ready.
    let first = policy.start_step(attempt);
    let mut ready = vec![false; first];
    for step in &policy.chain[first..] {
        let r = lane_usable(flaws, hands, step, probes).await;
        ready.push(r);
        if r {
            break;
        }
    }
    policy.step_for(attempt, &ready).map(|i| &policy.chain[i])
}

/// The step this flaw's triage agent ran on: what to mark out when it ends on a limit. A flaw
/// triaged before the chain has no step recorded, which is the first one.
fn triage_ran<'a>(policy: &'a Policy, flaw: &Flaw) -> Option<&'a Step> {
    let last = policy.chain.len().checked_sub(1)?;
    Some(&policy.chain[flaw.triage_step.unwrap_or(0).min(last)])
}

/// Triage's step: the first of the cheap ones whose lane is usable, probed no further than it takes.
/// `None`: every step triage may use is out, so the reports wait.
async fn choose_triage_step(
    flaws: &Flaws,
    hands: &dyn Hands,
    policy: &Policy,
    probes: &mut Probes,
) -> Option<usize> {
    let mut ready = Vec::with_capacity(policy.triage_steps);
    for step in policy.chain.iter().take(policy.triage_steps) {
        let r = lane_usable(flaws, hands, step, probes).await;
        ready.push(r);
        if r {
            break;
        }
    }
    policy.triage_step(&ready)
}

/// `POST /flaws/{id}/dispatch`: the explicit start, under the same gates and caps as the loop.
pub async fn dispatch_one(
    flaws: &Flaws,
    policy: &Policy,
    hands: &dyn Hands,
    id: &str,
    p: Platform,
    actor: &str,
) -> Result<Target, ApiError> {
    dispatch_one_as(flaws, policy, hands, id, p, actor, "post").await
}

/// [`dispatch_one`] for a named caller (`post`, `mcp`), which the log line carries.
pub async fn dispatch_one_as(
    flaws: &Flaws,
    policy: &Policy,
    hands: &dyn Hands,
    id: &str,
    p: Platform,
    actor: &str,
    caller: &str,
) -> Result<Target, ApiError> {
    let d = flaws.get(id).map_err(api_error)?;
    let target = d
        .targets
        .iter()
        .find(|t| t.platform == p)
        .ok_or_else(|| api_error(FlawError::NotFound))?;
    let layer = target.layer.ok_or_else(|| {
        api_error(FlawError::NotClaimable(format!(
            "{id} on {p} has no layer yet"
        )))
    })?;
    if let Verdict::AskOwner(g) = policy.verdict(&d.flaw) {
        let approved = match &d.flaw.decision_id {
            Some(dc) => hands.decision(dc).await == Some(true),
            None => false,
        };
        if !approved {
            return Err(ApiError::new(
                409,
                "needs_owner",
                format!("this fix needs the owner's go first ({g})"),
            ));
        }
    }
    // From here on one dispatch at a time: the loop and every explicit start queue on this lock,
    // and what they read is what they act on.
    let limits = hands.limits();
    let lock = dispatch_lock(flaws);
    let _held = match lock.acquire(limits.lock_wait).await {
        Ok(h) => h,
        Err(held_for) => {
            // The caller is a request, maybe the owner's own chat turn: it is told, not kept.
            let n = limits.max_hold().saturating_sub(held_for).as_secs().max(1);
            tracing::info!(
                "{}",
                refused_line(
                    id,
                    p,
                    caller,
                    &format!(
                        "dispatcher busy: the lock has been held {} s",
                        held_for.as_secs()
                    )
                )
            );
            let mut e = ApiError::new(
                409,
                "dispatcher_busy",
                format!("the dispatcher is busy, retry in {n} s"),
            );
            e.extra = Some(json!({"retry_after_s": n}));
            return Err(e);
        }
    };
    let mut probes = Probes {
        request: true,
        ..Probes::default()
    };
    probes.start_hold();
    let mut alive = ask_alive(flaws, hands)
        .await
        .map_err(|e| ApiError::new(503, "agentd_unavailable", e))?;
    let mut away = Away(MAX_AWAY);
    sweep_idle(flaws, hands, &mut alive, &mut away).await;
    let Some(step) = choose_step(flaws, hands, policy, target.attempts + 1, &mut probes).await
    else {
        tracing::info!(
            "{}",
            refused_line(
                id,
                p,
                caller,
                "lane open: no agent of the chain has a usable lane"
            )
        );
        return Err(ApiError::new(
            503,
            "lane_out",
            "no agent of the chain has quota; it starts by itself when one is back",
        ));
    };
    let repo = policy.repo_for(p, layer).display().to_string();
    let lane = lane_key(step);
    let load = Load::of(flaws, policy, &alive);
    if let Some(why) = load.refusal(policy, &repo, &lane, lane_cap(flaws, policy, &lane)) {
        tracing::info!("{}", refused_line(id, p, caller, &why));
        return Err(ApiError::new(409, "busy", why));
    }
    if !clear_orphans(flaws, hands, &alive, id, p, &mut away).await {
        return Err(ApiError::new(
            503,
            "orphan_not_stopped",
            "an agent nobody tracks works on this target and could not be stopped in time; nothing was started",
        ));
    }
    let ctx = StartCtx {
        caller,
        caps: caps_of(policy),
    };
    start_fix(
        flaws,
        policy,
        step,
        hands,
        &d.flaw,
        target,
        actor,
        &ctx,
        &mut probes,
    )
    .await
}

/// Agents at work per repo, triage included. A session's own work isn't ours to count.
fn running_counts(flaws: &Flaws) -> HashMap<String, usize> {
    let mut n: HashMap<String, usize> = HashMap::new();
    for (_, t) in flaws.in_progress().unwrap_or_default() {
        if t.session.is_none() {
            if let Some(r) = t.repo {
                *n.entry(r).or_insert(0) += 1;
            }
        }
    }
    n
}

fn older_than(flaws: &Flaws, at: Option<&str>, minutes: i64) -> bool {
    at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| flaws.clock_now().signed_duration_since(t).num_minutes() >= minutes)
}

/// An agent's error that means the lane is out of quota, not that the fix failed. One input of
/// [`lane_reason`]; triage still stops on it alone.
fn is_limit(error: &str) -> bool {
    let e = error.to_lowercase();
    ["limit", "quota", "credits", "429", "billing"]
        .iter()
        .any(|w| e.contains(w))
}

/// What an error's words say about the lane: the sign-in refused (agentd's `is_auth_failure`
/// words and the usual others), a limit or quota, or the provider unavailable.
fn lane_reason(error: &str) -> Option<&'static str> {
    let e = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if has(&[
        "failed to authenticate",
        "oauth",
        "unauthorized",
        "not logged in",
        "please log in",
        "sign in",
        "sign-in",
        "invalid api key",
        "authentication",
    ]) {
        Some("sign-in refused")
    } else if is_limit(error) {
        Some("out of quota")
    } else if has(&[
        "unavailable",
        "overloaded",
        "503",
        "not reachable",
        "did not answer",
        "connection refused",
        "provider_tripped",
        "agent_limit",
    ]) {
        Some("provider unavailable")
    } else {
        None
    }
}

/// How an ended agent's claim ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The lane or provider failed, or the agent never provably worked: not an attempt.
    Lane(String),
    /// The agent worked and did not fix it: a counted attempt.
    Task(String),
}

/// Read an ended agent: its error's words say the lane failed; failing that, only an agent that
/// provably worked (a tool call, a commit or a change) failed the task. Everything else, an
/// unknown error included, is the lane's.
async fn classify(
    hands: &dyn Hands,
    agent: &str,
    workspace: Option<&str>,
    error: Option<&str>,
) -> Ended {
    if let Some(e) = error {
        if let Some(r) = lane_reason(e) {
            return Ended::Lane(format!("{r}: {}", clip(e, 200)));
        }
    }
    if hands.agent_worked(agent, workspace).await {
        Ended::Task(match error {
            Some(e) => clip(e, 200),
            None => "the agent ended without a merged commit".to_owned(),
        })
    } else {
        Ended::Lane(match error {
            Some(e) => format!("the agent did no work and ended on: {}", clip(e, 200)),
            None => "the agent ended without doing any work".to_owned(),
        })
    }
}

/// `lane probe`, one tiny call. Only an answer that says ok counts as ready: a probe that cannot
/// run, times out or says anything else is not ready, and is asked again next tick.
async fn probe_lane(lane: &str) -> bool {
    let bin = home().join(".local/bin/lane");
    probe_cmd(
        bin.as_os_str(),
        &["probe", "--lane", lane],
        Limits::DEFAULT.probe_lane,
    )
    .await
}

/// A probe command and what it printed: ready only on an `ok`. A command that cannot run, runs
/// past the time or prints nothing readable is not ready.
async fn probe_cmd(program: &std::ffi::OsStr, args: &[&str], within: Duration) -> bool {
    let run = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(o)) = tokio::time::timeout(within, run).await else {
        return false;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    let outcome = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["outcome"].as_str().map(str::to_owned));
    matches!(outcome.as_deref(), Some("ok" | "ready"))
}

/// What is known about each closed lane of the chain, for this process: `lane -> (ready, until)`.
/// Only a quota answer; whether a lane failed lives in the store's breaker. A ready answer is
/// dropped the moment a failure is seen on that lane.
static LANES: std::sync::Mutex<std::collections::BTreeMap<String, (bool, std::time::Instant)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
const LANE_READY_FOR: Duration = Duration::from_secs(30 * 60);
const LANE_OUT_FOR: Duration = Duration::from_secs(5 * 60);

fn lanes(
) -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, (bool, std::time::Instant)>>
{
    LANES.lock().unwrap_or_else(|e| e.into_inner())
}

fn clip(s: &str, max: usize) -> String {
    lucina_delegate::types::clip(s, max)
}

/// A line the tracker leaves in his chat: a flaw note from her.
fn flaw_line(text: &str) -> NewItem {
    NewItem {
        channel: "other".into(),
        role: "lucina".into(),
        text: text.to_owned(),
        meta: Some(Meta {
            tool: Some("flaw".into()),
            ok: Some(true),
            ..Meta::kind("flaw")
        }),
        ..Default::default()
    }
}

/// Is this claim the trial agent of its half-open lane? Only that one's first turn says the lane
/// works again: any other `running` agent on the lane may have been started before it failed.
fn is_trial(flaws: &Flaws, lane: &str, flaw: &str, t: &Target) -> bool {
    flaws.lane_trial(lane).ok().flatten().as_deref()
        == Some(format!("{flaw}:{}:{}", t.platform, t.claim_seq).as_str())
}

/// A claim with no agent, older than the stale limit: the server stopped between the claim and
/// the attach, or the create's answer was lost. agentd is asked first. An agent that exists is the
/// claim's; none is a give-back, never a counted try.
async fn stale_claim(flaws: &Flaws, hands: &dyn Hands, flaw: &Flaw, t: &Target, claim: &ClaimRef) {
    let p = t.platform;
    match hands.fix_agents(&flaw.id, p).await {
        Err(e) => {
            tracing::warn!(flaw = %flaw.id, platform = %p, error = %e, "flaws: agentd can't say whether the stale claim has an agent; asked again next tick");
        }
        Ok(found) if !found.is_empty() => {
            for extra in &found[1..] {
                put_away(hands, extra).await;
            }
            let step = Step::new("unknown", None, t.lane.as_deref());
            let ws = t.workspace.as_deref().unwrap_or("");
            match flaws.attach(&flaw.id, p, ws, &found[0], &step, ACTOR) {
                Ok(_) => {
                    tracing::info!(flaw = %flaw.id, platform = %p, agent = %found[0], "flaws: a claim with no agent had one in agentd; attached");
                }
                Err(e) => {
                    tracing::warn!(flaw = %flaw.id, platform = %p, error = %e, "flaws: the stale claim changed under the lookup");
                }
            }
        }
        Ok(_) => {
            let _ = flaws.give_back_if(
                &flaw.id,
                p,
                claim,
                "the claim never became a task",
                false,
                ACTOR,
            );
        }
    }
}

/// An agent that ended with no merged commit: its claim ends as a lane failure (no attempt; the
/// lane's breaker opens) or a task failure (a counted attempt with back-off). The agent is put
/// away first, and the claim let go is the one that was read.
#[allow(clippy::too_many_arguments)]
async fn ended(
    flaws: &Flaws,
    policy: &Policy,
    hands: &dyn Hands,
    flaw: &Flaw,
    t: &Target,
    claim: &ClaimRef,
    agent: &str,
    probes: &mut Probes,
) {
    let p = t.platform;
    let error = hands.agent_error(agent).await;
    let class = classify(hands, agent, t.workspace.as_deref(), error.as_deref()).await;
    put_away(hands, agent).await;
    match class {
        Ended::Lane(why) => {
            let _ = flaws.give_back_if(&flaw.id, p, claim, &why, true, ACTOR);
            // The lane failed whoever held the claim by now: open it, and forget it was ready.
            if let Some(lane) = t.lane.as_deref() {
                open_lane(flaws, hands, lane, &why, probes).await;
            }
        }
        Ended::Task(why) => {
            // An agent that provably worked got through its first turn: if it was the trial
            // agent of a half-open lane, that lane works again, whatever became of the task. (A
            // trial that went straight to `idle` never shows `running` to a tick.)
            if let Some(lane) = t.lane.as_deref() {
                if flaws.lane_state(lane).ok() == Some(LaneState::HalfOpen)
                    && is_trial(flaws, lane, &flaw.id, t)
                    && flaws.lane_close(lane).unwrap_or(false)
                {
                    tracing::info!(lane, agent, "flaws: the trial agent got through its first turn and went idle; the lane is closed");
                }
            }
            let _ = flaws.fail_task_if(
                &flaw.id,
                p,
                claim,
                &why,
                policy.max_attempts as usize,
                ACTOR,
            );
        }
    }
}

/// Check what each running task became (Symphony: re-read state after every turn).
async fn reconcile(flaws: &Flaws, policy: &Policy, hands: &dyn Hands, probes: &mut Probes) {
    for (flaw, t) in flaws.in_progress().unwrap_or_default() {
        let p = t.platform;
        // What was read of the claim: every release below only takes effect on this claim.
        let claim = t.claim_ref();
        let Some(agent) = t.agent.clone() else {
            if t.session.is_none()
                && older_than(flaws, t.claimed_at.as_deref(), STALE_CLAIM_MINUTES)
            {
                stale_claim(flaws, hands, &flaw, &t, &claim).await;
            }
            continue;
        };
        if matches!(flaw.state, FlawState::Duplicate | FlawState::Dismissed) {
            put_away(hands, &agent).await;
            let _ = flaws.give_back_if(
                &flaw.id,
                p,
                &claim,
                &format!("the flaw is {}", flaw.state),
                false,
                ACTOR,
            );
            continue;
        }
        if let Some(ws) = &t.workspace {
            if let Ok(Some(sha)) = hands.merged_sha(ws).await {
                if let Err(e) = flaws.record_merge(&flaw.id, p, &sha, ACTOR) {
                    tracing::warn!(flaw = %flaw.id, error = %e, "flaws: merge not recorded");
                }
                continue;
            }
        }
        let status = match hands.agent_status(&agent).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "flaws: agentd can't say; trying next tick");
                continue;
            }
        };
        match status.as_deref() {
            Some(s @ ("running" | "initializing")) => {
                if older_than(flaws, t.claimed_at.as_deref(), policy.agent_minutes) {
                    put_away(hands, &agent).await;
                    let why = format!("stopped after {} minutes", policy.agent_minutes);
                    let _ = flaws.fail_task_if(
                        &flaw.id,
                        p,
                        &claim,
                        &why,
                        policy.max_attempts as usize,
                        ACTOR,
                    );
                } else if s == "running" {
                    // Past its first turn without an error: the trial agent of a half-open lane
                    // has shown the lane works again.
                    if let Some(lane) = t.lane.as_deref() {
                        if flaws.lane_state(lane).ok() == Some(LaneState::HalfOpen)
                            && is_trial(flaws, lane, &flaw.id, &t)
                            && flaws.lane_close(lane).unwrap_or(false)
                        {
                            tracing::info!(lane, agent = %agent, "flaws: the trial agent's first turn succeeded; the lane is closed");
                        }
                    }
                }
            }
            // It ended and nothing was merged: the agent may have changed the target itself, in
            // which case it isn't `fixing` any more and this list didn't include it.
            _ => ended(flaws, policy, hands, &flaw, &t, &claim, &agent, probes).await,
        }
    }
}

/// The agents agentd handed `agent`'s run on to, in order, following the chain its labels leave
/// behind. Empty when it finished on the step it started on.
async fn continuations(hands: &dyn Hands, agent: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = agent.to_owned();
    while out.len() < HANDOFF_CHAIN_MAX {
        let Some(next) = hands.continued_on(&cur).await else {
            break;
        };
        if next == cur || out.contains(&next) {
            break;
        }
        out.push(next.clone());
        cur = next;
    }
    out
}

/// The triage agent and every agent a hand-off carried it on to, put away. An idle agent is a
/// process plus the MCP servers it started, and it works for nobody.
async fn archive_triage(hands: &dyn Hands, agent: &str) {
    let mut ids = vec![agent.to_owned()];
    ids.extend(continuations(hands, agent).await);
    for id in ids.iter().rev() {
        if let Err(e) = hands.archive_agent(id).await {
            tracing::warn!(agent = %id, error = %e, "flaws: the triage agent was not archived");
        }
    }
}

/// An agent agentd handed a run to is on a step triage never uses: take it off what it is doing.
async fn stop_if_running(hands: &dyn Hands, id: &str) {
    let running = matches!(
        hands.agent_status(id).await.ok().flatten().as_deref(),
        Some("running") | Some("initializing")
    );
    if running {
        hands.stop_agent(id).await;
    }
}

/// The triage stopped without doing the work, so another agent may take it — until it has stopped
/// `GIVE_UP_AFTER` times, after which the flaw is the owner's to triage by hand and stops coming
/// back on its own. `true`: the flaw store recorded the outcome, which is what the archiving waits
/// for; `false` leaves the agent tracked, and the next tick tries again.
fn record_stopped_triage(flaws: &Flaws, flaw: &Flaw, why: &str) -> bool {
    let resets = flaws.triage_resets(&flaw.id).unwrap_or_default();
    if resets >= GIVE_UP_AFTER {
        flaws
            .finish_triage(
                &flaw.id,
                &format!("gave up after {resets} limit stops; triage it by hand"),
                ACTOR,
            )
            .is_ok()
    } else {
        flaws.reset_triage(&flaw.id, why, ACTOR).is_ok()
    }
}

async fn run_triage(flaws: &Flaws, policy: &Policy, hands: &dyn Hands, probes: &mut Probes) {
    let repo = policy.secretary_repo.display().to_string();
    let mut busy = running_counts(flaws).get(&repo).copied().unwrap_or(0);
    for flaw in flaws.triaging().unwrap_or_default() {
        let Some(agent) = flaw.triage_agent.clone() else {
            continue;
        };
        // agentd found a quota on the cheap step and carried the run on to an agent of its own
        // choice, in the same directory. Triage never follows it up the chain: the round ends on
        // the cheap agent's own record and the expensive one goes back in the box.
        let handed_on = continuations(hands, &agent).await;
        if let Some(next) = handed_on.first() {
            for a in &handed_on {
                stop_if_running(hands, a).await;
            }
            let why = format!("the run was handed on to agent {next}");
            if let Some(step) = triage_ran(policy, &flaw) {
                hands.step_out(step, &why);
            }
            if record_stopped_triage(flaws, &flaw, &why) {
                archive_triage(hands, &agent).await;
            }
            continue;
        }
        match hands.agent_status(&agent).await {
            // Still `initializing` after five minutes, the provider never answered: three agents sat
            // like this for hours on 2026-11-13. A run that started gets its full fifteen minutes.
            Ok(Some(s)) if s == "running" || s == "initializing" => {
                let init = s == "initializing";
                let minutes = if init {
                    TRIAGE_INIT_MINUTES
                } else {
                    TRIAGE_MINUTES
                };
                if older_than(flaws, Some(&flaw.updated_at), minutes) {
                    hands.stop_agent(&agent).await;
                    let why = if init {
                        "stuck initializing"
                    } else {
                        "stopped after 15 minutes"
                    };
                    if flaws.finish_triage(&flaw.id, why, ACTOR).is_ok() {
                        archive_triage(hands, &agent).await;
                    }
                } else {
                    busy += 1;
                }
            }
            Ok(None) => {
                // agentd doesn't know it any more: nothing to stop and nothing to archive.
                let _ = flaws.finish_triage(&flaw.id, "the agent is gone", ACTOR);
            }
            Ok(Some(_)) => {
                // An agent in error, idle or closed: what it answered says whether the work can be
                // handed to another agent, or the triage ends here.
                let error = hands.agent_error(&agent).await;
                if error.as_deref().and_then(lane_reason).is_some() {
                    let e = error.unwrap_or_default();
                    // Triage keeps to the cheap end of the chain; the step that ran is the one
                    // that failed (out of quota, or a sign-in refused), not always the first: its
                    // lane's breaker opens, and the flaw stays triage-due for the next lane.
                    if let Some(step) = triage_ran(policy, &flaw) {
                        open_lane(flaws, hands, &lane_key(step), &e, probes).await;
                    }
                    if record_stopped_triage(flaws, &flaw, &e) {
                        archive_triage(hands, &agent).await;
                    }
                } else {
                    // Anything else the agent stopped on is an answer, not a lane out of quota: a
                    // second agent would stop on the same thing.
                    let why = match error.as_deref() {
                        Some(e) => format!("the agent ended on an error: {}", clip(e, 120)),
                        None => "the agent ended".to_string(),
                    };
                    if flaws.finish_triage(&flaw.id, &why, ACTOR).is_ok() {
                        archive_triage(hands, &agent).await;
                    }
                }
            }
            Err(_) => busy += 1,
        }
    }
    if !policy.auto_triage {
        return;
    }
    let due = flaws.triage_due().unwrap_or_default();
    if due.is_empty() {
        return;
    }
    // Triage never reaches an expensive step: the first of its `triage_steps` cheap ones that has
    // quota, and the reports wait when none of them has.
    let Some(ran) = choose_triage_step(flaws, hands, policy, probes).await else {
        return;
    };
    let step = &policy.chain[ran];
    // Triage agents are dispatcher agents too: they start under the same lock and the same caps.
    let lock = dispatch_lock(flaws);
    let Some(_held) = lock.try_acquire() else {
        return;
    };
    probes.start_hold();
    let Ok(alive) = ask_alive(flaws, hands).await else {
        return;
    };
    let mut load = Load::of(flaws, policy, &alive);
    let lane = lane_key(step);
    let limits = hands.limits();
    for flaw in due {
        if busy >= policy.max_running_per_repo {
            break;
        }
        if let Some(why) = load.refusal(policy, &repo, &lane, lane_cap(flaws, policy, &lane)) {
            tracing::info!(flaw = %flaw.id, reason = %why, "flaws: triage refused");
            break;
        }
        // One triage per flaw, whatever the flaw store last wrote: agentd may still be holding an
        // agent for it, and the one a hand-off started in its place carries the same labels.
        let known = within(limits.lookup, hands.triage_agents(&flaw.id))
            .await
            .unwrap_or_else(|| Err("agentd did not answer the lookup in time".to_owned()));
        match known {
            Ok(agents) if !agents.is_empty() => {
                let a = &agents[0];
                tracing::info!(flaw = %flaw.id, agent = %a, "flaws: already in triage: {a}");
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(flaw = %flaw.id, error = %e, "flaws: agentd can't say whether the flaw is in triage already");
                continue;
            }
        }
        let short = flaw.id.trim_start_matches("fl-");
        let title = format!("Triage flaw {}: {}", flaw.id, clip(&flaw.title, 60));
        let late = |what: &str| {
            format!(
                "agentd did not answer the {what} within {} s",
                limits.create.as_secs()
            )
        };
        let started = async {
            allowed_model(&step.provider, step.model.as_deref())?;
            let (_, cwd) = within(
                limits.create,
                hands.start_task(
                    &title,
                    &repo,
                    &format!("flaw-{short}-triage-{}", Utc::now().format("%H%M%S")),
                    &flaw.id,
                    None,
                ),
            )
            .await
            .ok_or_else(|| late("worktree create"))??;
            within(
                limits.create,
                hands.start_agent(
                    policy,
                    step,
                    &cwd,
                    &policy.triage_brief(&flaw, &cwd),
                    &title,
                    json!({"from": "lucina", "flaw": flaw.id, "triage": "1", "flaw_lane": lane, "lucina.handoff-off": "1"}),
                    None,
                ),
            )
            .await
            .ok_or_else(|| late("agent create"))?
        }
        .await;
        match started {
            Ok(agent) => {
                let _ = flaws.start_triage(&flaw.id, &agent, false, Some(ran), ACTOR);
                load.add(agent.clone(), Some(&lane), Some(&repo));
            }
            Err(why) => {
                tracing::warn!(flaw = %flaw.id, error = %why, "flaws: triage not started");
                // The same as for a fix: a create that agentd refuses (its breaker or its caps) or
                // does not answer in time is the lane's failure, and the lane opens.
                if create_lane_failure(&why) {
                    open_lane(flaws, hands, &lane, &why, probes).await;
                }
            }
        }
        // One triage start a tick, whatever came of it: the lock is not held for a loop of them.
        break;
    }
}

/// The owner's go for one pick, outside the lock (asking him is a call of its own): `true` for a
/// pick that may start. A gated flaw is asked once and waits; a no dismisses it.
async fn may_start(flaws: &Flaws, hands: &dyn Hands, pick: &lucina_flaws::Pick) -> bool {
    let list = hands.limits().list;
    match pick.verdict {
        Verdict::Start => true,
        Verdict::AskOwner(g) => match &pick.flaw.decision_id {
            None => {
                match within(list, hands.ask_owner(&pick.flaw, g)).await {
                    Some(Ok(dc)) => {
                        let patch = FlawPatch {
                            decision_id: Some(dc),
                            ..FlawPatch::default()
                        };
                        let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                    }
                    Some(Err(e)) => {
                        tracing::warn!(flaw = %pick.flaw.id, error = %e, "flaws: couldn't ask")
                    }
                    None => {
                        tracing::warn!(flaw = %pick.flaw.id, "flaws: the question to the owner did not answer in time")
                    }
                }
                false
            }
            Some(dc) => match within(list, hands.decision(dc)).await.flatten() {
                Some(true) => true,
                Some(false) => {
                    let patch = FlawPatch {
                        dismissed: Some(format!("the owner said no ({dc})")),
                        ..FlawPatch::default()
                    };
                    let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                    false
                }
                None => false,
            },
        },
    }
}

/// What the loop starts on this tick, under the dispatch lock: caps on agents alive (rows and
/// agentd's list, by agent id), at most `max_new_per_tick` new dispatches, an approved gated pick
/// counted like any other. agentd that can't be asked means nothing starts. The loop never
/// waits for the lock: held by a request, the tick is skipped, and said once per change.
async fn dispatch_phase(flaws: &Flaws, policy: &Policy, hands: &dyn Hands, probes: &mut Probes) {
    let due = flaws.due().unwrap_or_default();
    if due.is_empty() {
        return;
    }
    // Who may start is settled first, outside the lock; the caps are the lock's.
    let mut startable = Vec::new();
    for pick in policy.pick(due, &HashMap::new()) {
        if may_start(flaws, hands, &pick).await {
            startable.push(pick);
        }
    }
    if startable.is_empty() {
        return;
    }
    let lock = dispatch_lock(flaws);
    let Some(_held) = lock.try_acquire() else {
        if changed(flaws, "dispatch-lock", "busy") {
            tracing::info!("flaws: dispatcher busy, tick skipped");
        }
        return;
    };
    if changed(flaws, "dispatch-lock", "ok") {
        tracing::info!("flaws: the dispatcher is free again; ticks resume");
    }
    probes.start_hold();
    let Ok(mut alive) = ask_alive(flaws, hands).await else {
        return;
    };
    let mut away = Away(MAX_AWAY);
    sweep_idle(flaws, hands, &mut alive, &mut away).await;
    let mut load = Load::of(flaws, policy, &alive);
    let ctx = StartCtx {
        caller: "loop",
        caps: caps_of(policy),
    };
    let mut started = 0usize;
    for pick in startable {
        let (id, p) = (pick.flaw.id.as_str(), pick.target.platform);
        // An approved gated pick is a start like any other: it counts against every cap below.
        if started >= policy.max_new_per_tick {
            tracing::info!(
                "{}",
                refused_line(
                    id,
                    p,
                    "loop",
                    &format!(
                        "cap: {} new dispatch per tick already started",
                        policy.max_new_per_tick
                    )
                )
            );
            continue;
        }
        // The free steps first: this attempt's own step, or the first ready one below it.
        let Some(step) = choose_step(flaws, hands, policy, pick.target.attempts + 1, probes).await
        else {
            tracing::info!(
                "{}",
                refused_line(id, p, "loop", "lane open: no usable lane for this attempt")
            );
            continue;
        };
        let lane = lane_key(step);
        if let Some(why) = load.refusal(policy, &pick.repo, &lane, lane_cap(flaws, policy, &lane)) {
            tracing::info!("{}", refused_line(id, p, "loop", &why));
            continue;
        }
        if !clear_orphans(flaws, hands, &alive, id, p, &mut away).await {
            tracing::info!(
                "{}",
                refused_line(
                    id,
                    p,
                    "loop",
                    "an untracked agent on this target was not stopped in time"
                )
            );
            continue;
        }
        // An attempt counts whether it worked or not: a create that fails is as much agentd's
        // work as one that succeeds, and a failing one must not be retried in a loop.
        started += 1;
        // A refusal or failure is said already, by the claim's refusal or `task not started`.
        if let Ok(t) = start_fix(
            flaws,
            policy,
            step,
            hands,
            &pick.flaw,
            &pick.target,
            ACTOR,
            &ctx,
            probes,
        )
        .await
        {
            load.add(
                t.agent.clone().unwrap_or_else(|| format!("claim:{id}:{p}")),
                Some(&lane),
                Some(&pick.repo),
            );
        }
    }
}

/// One round of the loop.
pub async fn tick(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
    let mut probes = Probes::default();
    reconcile(flaws, policy, hands, &mut probes).await;
    heal_lanes(flaws, hands, &mut probes).await;
    run_triage(flaws, policy, hands, &mut probes).await;
    if policy.auto_dispatch {
        dispatch_phase(flaws, policy, hands, &mut probes).await;
    }
    let untold = flaws.untold().unwrap_or_default();
    if !untold.is_empty() {
        let lines: Vec<String> = untold
            .iter()
            .map(|o| lucina_flaws::policy::outcome_line(&o.flaw, &o.target, policy))
            .collect();
        let text = if lines.len() == 1 {
            format!("🔧 {}", lines[0])
        } else {
            format!(
                "🔧 Flaws:\n{}",
                lines
                    .iter()
                    .map(|l| format!("- {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        if hands.tell(&text).await {
            for o in &untold {
                let _ = flaws.mark_told(&o.flaw.id, o.target.platform, o.target.status);
            }
        }
    }
}

/// Every minute, while the tracker is on.
pub fn spawn(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    let flaws = st.flaws.clone()?;
    let st = st.clone();
    Some(tokio::spawn(async move {
        let mut every = tokio::time::interval(LOOP_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let hands = StateHands { st: st.clone() };
        loop {
            every.tick().await;
            tick(&flaws, &st.cfg.flaws.policy, &hands).await;
        }
    }))
}

/// The real hands: agentd on its socket, as `service:lucina`, and the owner's chat.
pub struct StateHands {
    pub st: AppState,
}

impl StateHands {
    async fn agentd(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        agentd_request(&crate::agentd::socket_path(), method, path, body).await
    }
}

#[async_trait::async_trait]
impl Hands for StateHands {
    async fn start_task(
        &self,
        title: &str,
        repo: &str,
        branch: &str,
        flaw: &str,
        platform: Option<Platform>,
    ) -> Result<(String, String), String> {
        let body = json!({
            "title": title, "repo": repo, "branch": branch, "owner": "lucina", "session": ACTOR,
            "flaw": flaw, "flawTarget": platform,
        });
        let v = self.agentd("POST", "/pipeline/tasks", Some(&body)).await?;
        let w = &v["workspace"];
        match (w["workspaceId"].as_str(), w["cwd"].as_str()) {
            (Some(id), Some(cwd)) => Ok((id.to_owned(), cwd.to_owned())),
            _ => Err("agentd returned no workspace".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_agent(
        &self,
        policy: &Policy,
        step: &Step,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: Value,
        key: Option<&str>,
    ) -> Result<String, String> {
        let mut body = json!({
            "provider": step.provider, "model": step.model, "thinkingOptionId": policy.effort,
            "modeId": policy.mode, "cwd": cwd, "prompt": prompt, "title": clip(title, 80), "labels": labels,
        });
        if let Some(key) = key {
            body["idempotencyKey"] = json!(key);
        }
        let v = self.agentd("POST", "/agents", Some(&body)).await?;
        v["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "agentd returned no agent id".to_owned())
    }

    async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
        match self.agentd("GET", &format!("/agents/{id}"), None).await {
            Ok(v) => Ok(v["status"].as_str().map(str::to_owned)),
            Err(e) if e.starts_with("agentd 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn stop_agent(&self, id: &str) {
        if let Err(e) = self
            .agentd("POST", &format!("/agents/{id}/stop"), Some(&json!({})))
            .await
        {
            tracing::warn!(agent = id, error = %e, "flaws: couldn't stop the agent");
        }
    }

    /// agentd's own list, filtered the way `lagent ls --label` filters it. The list leaves archived
    /// agents out already; the check stays so the answer doesn't depend on the endpoint's default.
    async fn triage_agents(&self, flaw: &str) -> Result<Vec<String>, String> {
        let v = self.agentd("GET", "/agents", None).await?;
        // Not knowing is not zero agents: nothing is decided on it.
        agent_list(&v)?;
        Ok(v["agents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| {
                        x["archivedAt"].is_null()
                            && x["labels"]["flaw"] == flaw
                            && x["labels"]["triage"] == "1"
                    })
                    .filter_map(|x| x["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn continued_on(&self, id: &str) -> Option<String> {
        let v = self
            .agentd("GET", &format!("/agents/{id}"), None)
            .await
            .ok()?;
        v["labels"][CONTINUED_ON_LABEL]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    async fn archive_agent(&self, id: &str) -> Result<(), String> {
        self.agentd("POST", &format!("/agents/{id}/archive"), Some(&json!({})))
            .await
            .map(|_| ())
    }

    async fn merged_sha(&self, workspace: &str) -> Result<Option<String>, String> {
        let v = self.agentd("GET", "/pipeline/tasks", None).await?;
        Ok(v["tasks"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["workspaceId"] == workspace))
            .and_then(|t| t["mergedSha"].as_str())
            .map(str::to_owned))
    }

    async fn ask_owner(&self, flaw: &Flaw, gate: Gate) -> Result<String, String> {
        let why = match gate {
            Gate::Irreversible => "can't be undone",
            Gate::Credentials => "needs your credentials",
            Gate::Payment => "needs a payment",
        };
        let body = json!({
            "question": format!("Start the fix for \"{}\" ({})? It {why}.", flaw.title, flaw.id),
            "options": ["Start the fix", "Not now"],
            "reversible": gate != Gate::Irreversible,
            "impact": "high",
            "topic": "flaws",
            "context": flaw.words,
            "recommendation": "Start the fix",
            "asking_session": ACTOR,
        });
        let d = crate::decide::create(&self.st, &body)
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(d.decision_id)
    }

    async fn decision(&self, id: &str) -> Option<bool> {
        let d = crate::decide::store(&self.st.cfg.data_dir).get(id, Utc::now())?;
        match d.chosen_index {
            Some(0) => Some(true),
            Some(_) => Some(false),
            None if d.status == "pending" => None,
            None => Some(false),
        }
    }

    async fn agent_error(&self, id: &str) -> Option<String> {
        let v = self
            .agentd("GET", &format!("/agents/{id}"), None)
            .await
            .ok()?;
        v["lastError"].as_str().map(str::to_owned)
    }

    async fn fix_agents(&self, flaw: &str, platform: Platform) -> Result<Vec<String>, String> {
        let v = self.agentd("GET", "/agents", None).await?;
        // Not knowing is not zero agents: nothing is decided on it.
        agent_list(&v)?;
        Ok(v["agents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| {
                        x["archivedAt"].is_null()
                            && x["labels"]["flaw"] == flaw
                            && x["labels"]["platform"] == platform.as_str()
                            && x["labels"]["triage"] != "1"
                    })
                    .filter_map(|x| x["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn alive_agents(&self) -> Result<Vec<Alive>, String> {
        let v = self.agentd("GET", "/agents", None).await?;
        // Not knowing is not zero agents: nothing is decided on it.
        agent_list(&v)?;
        let label = |x: &Value, k: &str| x["labels"][k].as_str().map(str::to_owned);
        Ok(v["agents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| {
                        x["archivedAt"].is_null()
                            && x["labels"]["flaw"].is_string()
                            && matches!(
                                x["status"].as_str(),
                                Some("initializing" | "running" | "idle")
                            )
                    })
                    .filter_map(|x| {
                        Some(Alive {
                            id: x["id"].as_str()?.to_owned(),
                            lane: label(x, "flaw_lane"),
                            flaw: label(x, "flaw"),
                            platform: label(x, "platform"),
                            triage: x["labels"]["triage"] == "1",
                            idle: x["status"] == "idle",
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A tool call on the agent's timeline, or a commit on the task's branch past its base.
    async fn agent_worked(&self, agent: &str, workspace: Option<&str>) -> bool {
        if let Ok(v) = self
            .agentd("GET", &format!("/agents/{agent}/timeline?limit=400"), None)
            .await
        {
            if v["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["item"]["type"] == "tool_call"))
            {
                return true;
            }
        }
        let Some(ws) = workspace else {
            return false;
        };
        let Ok(v) = self.agentd("GET", "/pipeline/tasks", None).await else {
            return false;
        };
        let Some(t) = v["tasks"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["workspaceId"] == ws))
        else {
            return false;
        };
        let (Some(repo), Some(branch)) = (t["repo"].as_str(), t["branch"].as_str()) else {
            return false;
        };
        let base = t["base"]
            .as_str()
            .or_else(|| t["trunk"].as_str())
            .unwrap_or("main");
        let range = format!("{base}..{branch}");
        let out = tokio::process::Command::new("git")
            .args(["-C", repo, "rev-list", "--count", &range, "--"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(10), out).await {
            Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .is_ok_and(|n| n > 0),
            _ => false,
        }
    }

    async fn step_ready(&self, step: &Step) -> bool {
        let Some(lane) = step.probe_lane.as_deref() else {
            return true;
        };
        if let Some((ready, until)) = lanes().get(lane) {
            if std::time::Instant::now() < *until {
                return *ready;
            }
        }
        let ready = probe_lane(lane).await;
        let keep = if ready { LANE_READY_FOR } else { LANE_OUT_FOR };
        tracing::info!(lane, ready, "flaws: a step's lane probed");
        lanes().insert(lane.to_owned(), (ready, std::time::Instant::now() + keep));
        ready
    }

    async fn probe_lane(&self, lane: &str) -> bool {
        // A step with no probe lane has its label for a breaker key: nothing to probe.
        if lane.contains(':') {
            return true;
        }
        let ready = probe_lane(lane).await;
        tracing::info!(lane, ready, "flaws: an open lane probed");
        lanes().insert(
            lane.to_owned(),
            (
                ready,
                std::time::Instant::now() + if ready { LANE_READY_FOR } else { LANE_OUT_FOR },
            ),
        );
        ready
    }

    /// A lane failed: any ready answer cached before it is wrong, and is dropped.
    fn step_out(&self, step: &Step, why: &str) {
        let Some(lane) = step.probe_lane.as_deref() else {
            return;
        };
        tracing::warn!(
            lane,
            why,
            "flaws: an agent failed on this lane; what was cached about it is dropped"
        );
        lanes().remove(lane);
    }

    async fn record_lane_open(&self, lane: &str, why: &str) {
        let Some(ops) = self.st.ops.clone() else {
            return;
        };
        let mut new = lucina_ops::NewRecord::new(
            lucina_ops::Kind::Incident,
            "lucina",
            &clip(
                &format!("Flaw dispatcher: lane {lane} failed and is closed to it"),
                200,
            ),
        );
        new.body = format!(
            "An agent of the flaw dispatcher failed on lane {lane}: {why}. Nothing is dispatched on \
             this lane until a probe passes; one agent then tries it, and its first turn closes the \
             breaker. A lead closes it by hand with `lucina-rs flaws lane clear {lane}`. Refs DISP-01."
        );
        new.severity = Some(lucina_ops::Severity::Medium);
        // One record per opening, keyed by how many times the lane has opened (never a time).
        let opens = self
            .st
            .flaws
            .as_ref()
            .and_then(|f| f.lanes().ok())
            .and_then(|ls| ls.into_iter().find(|b| b.lane == lane))
            .map_or(0, |b| b.opens);
        new.dedup_key = Some(lane_ops_key(lane, opens));
        if let Err(e) = ops.record(new, "lucina").await {
            tracing::warn!(lane, error = %e, "flaws: the lane incident could not be recorded");
        }
    }

    async fn tell_start(&self, text: &str) {
        self.st.timeline.record(flaw_line(text));
    }

    async fn tell(&self, text: &str) -> bool {
        let item = self.st.timeline.record(flaw_line(text));
        let mut body = json!({
            "category": "lucina_message",
            "title": "Flaws",
            "body": clip(text, 480),
            "route": "/",
            "dedupKey": format!("flaws:{:x}", text.bytes().fold(0u64, |h, b| h.wrapping_mul(1099511628211) ^ b as u64)),
        });
        if let Some(item) = item {
            body["timelineId"] = json!(item.id);
        }
        lucina_secrets::redactor().redact_json(&mut body);
        let r = crate::bridges::notify(&self.st.secretary, &body).await;
        if !r.ok {
            tracing::warn!(status = r.status, "flaws: outcome notification failed");
        }
        // The chat has it either way; one failed push isn't worth saying it twice.
        true
    }
}

// ---------------------------------------------------------------- restore

/// Where an old agent's error text comes from, for the restore command.
#[async_trait::async_trait]
pub trait ErrorSource: Send + Sync {
    async fn agent_error(&self, id: &str) -> Option<String>;
}

/// agentd on its socket: the agents the incident left resident still hold their `lastError`.
pub struct SocketErrors;

#[async_trait::async_trait]
impl ErrorSource for SocketErrors {
    async fn agent_error(&self, id: &str) -> Option<String> {
        let v = agentd_request(
            &crate::agentd::socket_path(),
            "GET",
            &format!("/agents/{id}"),
            None,
        )
        .await
        .ok()?;
        v["lastError"].as_str().map(str::to_owned)
    }
}

/// A blocked target the restore command found, or reset.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Restorable {
    pub flaw_id: String,
    pub platform: Platform,
    pub attempts: u32,
    pub last_error: String,
}

/// A blocked target the restore command could not judge, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    pub flaw_id: String,
    pub platform: Platform,
    pub reason: String,
}

/// What the restore command found: the targets it lists (or resets) and the ones it left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub listed: Vec<Restorable>,
    pub skipped: Vec<Skipped>,
    /// Listed targets that `--apply` could not reset, and why.
    pub failed: Vec<Skipped>,
}

/// `lucina-rs flaws restore-lane-failures --since <rfc3339> [--apply]`: the blocked targets whose
/// failures since that time were all lane failures under the dispatcher's classification, judged
/// by the stored agent error text. A target is listed only when every agent started on it since
/// has an error text that names the lane (sign-in, quota, provider unavailable), and no counted
/// task failure is on record for it. `apply` resets exactly the ones listed (attempts 0,
/// `affected`, no back-off); without it nothing changes.
pub async fn restore_lane_failures(
    flaws: &Flaws,
    since: &str,
    apply: bool,
    errors: &dyn ErrorSource,
) -> Result<RestoreReport, FlawError> {
    let since = chrono::DateTime::parse_from_rfc3339(since.trim())
        .map_err(|e| FlawError::Invalid(format!("--since must be an RFC 3339 time: {e}")))?
        .with_timezone(&Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    for look in flaws.blocked_since(&since)? {
        let mut skip = |reason: String| {
            skipped.push(Skipped {
                flaw_id: look.flaw.id.clone(),
                platform: look.target.platform,
                reason,
            })
        };
        if look.failures.iter().any(|f| f.kind == "task") {
            skip(format!(
                "a counted task failure is on record since {since}: the agent worked, a lead decides"
            ));
            continue;
        }
        if look.agents.is_empty() && look.failures.is_empty() {
            skip(format!(
                "no `started` event with an `agent` key and no recorded failure since {since}"
            ));
            continue;
        }
        // What the target itself kept of its last end, for when agentd can no longer say: the
        // stored `last_error`, else the text of its last `released` event.
        let stored = look.target.last_error.clone().or_else(|| {
            flaws.get(&look.flaw.id).ok().and_then(|d| {
                d.events
                    .iter()
                    .rev()
                    .filter(|e| e.kind == "released" && e.platform == Some(look.target.platform))
                    .find_map(|e| {
                        e.detail["error"]
                            .as_str()
                            .or_else(|| e.detail["why"].as_str())
                            .map(str::to_owned)
                    })
            })
        });
        let mut texts: Vec<String> = look
            .failures
            .iter()
            .filter_map(|f| f.error.clone())
            .collect();
        let mut why_not: Option<String> = None;
        for a in &look.agents {
            let text = match errors.agent_error(a).await {
                Some(e) => Some(e),
                // agentd no longer holds it: the failure rows, then the target's own record.
                None => look
                    .failures
                    .iter()
                    .find(|f| f.agent.as_deref() == Some(a.as_str()))
                    .and_then(|f| f.error.clone())
                    .or_else(|| stored.clone().filter(|t| lane_reason(t).is_some())),
            };
            match text {
                Some(e) if lane_reason(&e).is_some() => texts.push(e),
                Some(e) => {
                    why_not = Some(format!(
                        "agent {a} ended on an error that is not a lane failure: {}",
                        clip(&e, 120)
                    ));
                    break;
                }
                None => {
                    why_not = Some(format!(
                        "agentd no longer holds agent {a}, and no stored error text names the lane"
                    ));
                    break;
                }
            }
        }
        if let Some(why) = why_not {
            skip(why);
            continue;
        }
        found.push(Restorable {
            flaw_id: look.flaw.id.clone(),
            platform: look.target.platform,
            attempts: look.target.attempts,
            last_error: texts.pop().unwrap_or_default(),
        });
    }
    let mut failed = Vec::new();
    if apply {
        for r in &found {
            // One target that cannot be reset does not stop the others; it is said.
            if let Err(e) = flaws.restore_target(
                &r.flaw_id,
                r.platform,
                "every failure since was a lane failure",
                "lucina-rs-restore",
            ) {
                failed.push(Skipped {
                    flaw_id: r.flaw_id.clone(),
                    platform: r.platform,
                    reason: format!("reset failed: {e}"),
                });
            }
        }
    }
    Ok(RestoreReport {
        listed: found,
        skipped,
        failed,
    })
}

// ---------------------------------------------------------------- CLI

/// The database for `lucina-rs flaws`: agents and agentd use it beside the running server.
pub fn open_cli(db: Option<PathBuf>) -> anyhow::Result<Flaws> {
    let path = db
        .or_else(|| std::env::var_os("LUCINA_FLAWS_DB").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("LUCINA_DATA_DIR")
                .map(|d| Path::new(&d).join("flaws").join("flaws.db"))
        })
        .unwrap_or_else(|| PathBuf::from("/home/nez/Projects/Lucina/data/flaws/flaws.db"));
    Ok(Flaws::open(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_flaws::{Evidence, EvidenceKind, Layer, Severity};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// The tests that are about something else claim without caps; the crate has no uncapped
    /// claim any more (LA-C035 F6).
    trait ClaimUncapped {
        fn claim(
            &self,
            id: &str,
            p: Platform,
            repo: &str,
            actor: &str,
        ) -> Result<Target, FlawError>;
    }

    impl ClaimUncapped for Flaws {
        fn claim(
            &self,
            id: &str,
            p: Platform,
            repo: &str,
            actor: &str,
        ) -> Result<Target, FlawError> {
            let none = ClaimCaps {
                per_repo: usize::MAX,
                total: usize::MAX,
                per_lane: usize::MAX,
            };
            self.claim_capped(id, p, repo, None, &none, actor)
        }
    }

    #[derive(Default)]
    struct Fake {
        tasks: Mutex<Vec<String>>,
        /// What started: the worktree it works in and the chain step it runs on.
        agents: Mutex<Vec<(String, String)>>,
        /// The labels each agent started with, in `agents` order: `ag{n}` is index n.
        labels: Mutex<Vec<Value>>,
        /// agentd's quota hand-off: the agent it continued a run on to, `from -> to`.
        continued: Mutex<HashMap<String, String>>,
        /// The agents put away through `archive_agent`.
        archived: Mutex<Vec<String>>,
        status: Mutex<HashMap<String, String>>,
        merged: Mutex<HashMap<String, String>>,
        stopped: Mutex<Vec<String>>,
        told: Mutex<Vec<String>>,
        /// The lines the dispatcher wrote in his chat when a try started.
        started: Mutex<Vec<String>>,
        asked: Mutex<Vec<String>>,
        answer: Mutex<Option<bool>>,
        fail_agent: Mutex<bool>,
        errors: Mutex<HashMap<String, String>>,
        /// Every lane answers "out of quota" when probed.
        out: Mutex<bool>,
        /// Lanes that answered "out of quota", each marked out by its own probe or limit.
        lanes_out: Mutex<Vec<String>>,
        /// Agents that provably worked (a tool call on their timeline).
        worked: Mutex<HashSet<String>>,
        /// Every incident the dispatcher wrote in the ops record, one line per opened breaker.
        ops: Mutex<Vec<String>>,
        /// agentd answers no list (`GET /agents` fails).
        list_down: Mutex<bool>,
        /// The lookup of a target's agents by labels fails while the list works.
        lookup_down: Mutex<bool>,
        /// A create makes the agent and then answers with this error, as a timed-out socket does.
        late_error: Mutex<Option<String>>,
        /// A create answers this error and makes nothing.
        create_error: Mutex<Option<String>>,
        /// Every agent dies at once with this error, as the sign-in of 2026-11-19 did.
        dies_with: Mutex<Option<String>>,
        /// The idempotency keys the creates carried, and the agent each made (agentd's dedup).
        keys: Mutex<HashMap<String, String>>,
        key_log: Mutex<Vec<String>>,
        /// What happened, in order: `start agN`, `stop agN`.
        log: Mutex<Vec<String>>,
        /// Every agent create never answers (agentd is stuck on it).
        hang_creates: Mutex<bool>,
        /// Every agent ends its first turn at once, idle, with no tool call and no error.
        idle_dies: Mutex<bool>,
        /// The probe keeps saying ready after a failure (as it did at 00:43:12): `step_out` is ignored.
        ignore_step_out: Mutex<bool>,
        /// The deadlines the hands report (short ones, for the tests that wait on a deadline).
        limits: Mutex<Option<Limits>>,
        /// One-shot pause points a test forces an order with.
        pauses: Mutex<HashMap<&'static str, Arc<Pause>>>,
    }

    /// A pause point: the code under test stops at `reached` until the test says `release`.
    #[derive(Default)]
    struct Pause {
        reached: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl Fake {
        /// Arm a pause at a named point of the fake hands; it fires once.
        fn pause_at(&self, point: &'static str) -> Arc<Pause> {
            let p = Arc::new(Pause::default());
            self.pauses.lock().unwrap().insert(point, p.clone());
            p
        }

        async fn at(&self, point: &'static str) {
            let p = self.pauses.lock().unwrap().remove(point);
            if let Some(p) = p {
                p.reached.notify_one();
                p.release.notified().await;
            }
        }

        /// The agents alive now: not archived, and initializing, running or idle.
        fn alive_now(&self) -> Vec<String> {
            let labels = self.labels.lock().unwrap();
            let status = self.status.lock().unwrap();
            let archived = self.archived.lock().unwrap();
            let stopped = self.stopped.lock().unwrap();
            (0..labels.len())
                .map(|i| format!("ag{i}"))
                .filter(|id| !archived.contains(id) && !stopped.contains(id))
                .filter(|id| {
                    matches!(
                        status.get(id).map(String::as_str),
                        Some("running" | "initializing" | "idle")
                    )
                })
                .collect()
        }

        /// The chain step each started agent ran on, in the order they started.
        fn ran(&self) -> Vec<String> {
            self.agents
                .lock()
                .unwrap()
                .iter()
                .map(|(_, step)| step.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl Hands for Fake {
        async fn start_task(
            &self,
            _title: &str,
            repo: &str,
            branch: &str,
            _flaw: &str,
            _p: Option<Platform>,
        ) -> Result<(String, String), String> {
            self.at("start_task").await;
            self.tasks.lock().unwrap().push(format!("{repo} {branch}"));
            Ok((format!("wks_{branch}"), format!("/w/{branch}")))
        }
        #[allow(clippy::too_many_arguments)]
        async fn start_agent(
            &self,
            _p: &Policy,
            step: &Step,
            cwd: &str,
            prompt: &str,
            _t: &str,
            l: Value,
            key: Option<&str>,
        ) -> Result<String, String> {
            self.at("start_agent").await;
            if *self.hang_creates.lock().unwrap() {
                std::future::pending::<()>().await;
            }
            if *self.fail_agent.lock().unwrap() {
                return Err("agentd is down".into());
            }
            if let Some(e) = self.create_error.lock().unwrap().clone() {
                return Err(e);
            }
            // agentd's create dedups by the key: the same key is the same agent.
            if let Some(key) = key {
                self.key_log.lock().unwrap().push(key.to_owned());
                if let Some(id) = self.keys.lock().unwrap().get(key) {
                    return Ok(id.clone());
                }
            }
            assert!(prompt.contains("<owner_words>"));
            let id = format!("ag{}", self.agents.lock().unwrap().len());
            self.agents
                .lock()
                .unwrap()
                .push((cwd.to_owned(), step.label()));
            self.labels.lock().unwrap().push(l);
            let dead = self.dies_with.lock().unwrap().clone();
            if let Some(err) = &dead {
                self.errors.lock().unwrap().insert(id.clone(), err.clone());
            }
            let status = if dead.is_some() {
                "error"
            } else if *self.idle_dies.lock().unwrap() {
                "idle"
            } else {
                "running"
            };
            self.status
                .lock()
                .unwrap()
                .insert(id.clone(), status.into());
            if let Some(key) = key {
                self.keys.lock().unwrap().insert(key.to_owned(), id.clone());
            }
            self.log.lock().unwrap().push(format!("start {id}"));
            if let Some(e) = self.late_error.lock().unwrap().clone() {
                return Err(e);
            }
            Ok(id)
        }
        async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.status.lock().unwrap().get(id).cloned())
        }
        async fn stop_agent(&self, id: &str) {
            self.at("stop").await;
            self.stopped.lock().unwrap().push(id.to_owned());
            self.log.lock().unwrap().push(format!("stop {id}"));
        }
        async fn fix_agents(&self, flaw: &str, p: Platform) -> Result<Vec<String>, String> {
            self.at("lookup").await;
            if *self.lookup_down.lock().unwrap() || *self.list_down.lock().unwrap() {
                return Err("agentd did not answer".into());
            }
            let labels = self.labels.lock().unwrap();
            let archived = self.archived.lock().unwrap();
            Ok((0..labels.len())
                .filter(|i| {
                    let l = &labels[*i];
                    l["flaw"] == flaw
                        && l["platform"] == p.as_str()
                        && l["triage"] != "1"
                        && !archived.contains(&format!("ag{i}"))
                })
                .map(|i| format!("ag{i}"))
                .collect())
        }
        async fn alive_agents(&self) -> Result<Vec<Alive>, String> {
            self.at("alive").await;
            if *self.list_down.lock().unwrap() {
                return Err("Lucina Agents (agentd) is not reachable".into());
            }
            let alive = self.alive_now();
            let labels = self.labels.lock().unwrap();
            Ok(alive
                .into_iter()
                .filter_map(|id| {
                    let l = &labels[id.trim_start_matches("ag").parse::<usize>().ok()?];
                    l["flaw"].as_str()?;
                    let idle =
                        self.status.lock().unwrap().get(&id).map(String::as_str) == Some("idle");
                    Some(Alive {
                        id,
                        lane: l["flaw_lane"].as_str().map(str::to_owned),
                        flaw: l["flaw"].as_str().map(str::to_owned),
                        platform: l["platform"].as_str().map(str::to_owned),
                        triage: l["triage"] == "1",
                        idle,
                    })
                })
                .collect())
        }
        async fn agent_worked(&self, agent: &str, _workspace: Option<&str>) -> bool {
            self.worked.lock().unwrap().contains(agent)
        }
        async fn probe_lane(&self, lane: &str) -> bool {
            self.at("probe").await;
            !*self.out.lock().unwrap() && !self.lanes_out.lock().unwrap().iter().any(|l| l == lane)
        }
        async fn record_lane_open(&self, lane: &str, why: &str) {
            self.ops.lock().unwrap().push(format!("{lane}: {why}"));
        }
        async fn triage_agents(&self, flaw: &str) -> Result<Vec<String>, String> {
            let agents = self.agents.lock().unwrap();
            let labels = self.labels.lock().unwrap();
            let continued = self.continued.lock().unwrap();
            let archived = self.archived.lock().unwrap();
            let mut out: Vec<String> = Vec::new();
            for i in 0..labels.len().min(agents.len()) {
                let l = &labels[i];
                if l["flaw"] != flaw || l["triage"] != "1" {
                    continue;
                }
                // The hand-off continuation inherits the flaw and triage labels, so agentd answers
                // it in the list as this flaw's triage agent, even once the one it continues is
                // archived.
                let mut id = format!("ag{i}");
                for _ in 0..HANDOFF_CHAIN_MAX {
                    if !archived.contains(&id) && !out.contains(&id) {
                        out.push(id.clone());
                    }
                    match continued.get(&id) {
                        Some(next) => id = next.clone(),
                        None => break,
                    }
                }
            }
            Ok(out)
        }
        async fn continued_on(&self, id: &str) -> Option<String> {
            self.continued.lock().unwrap().get(id).cloned()
        }
        async fn archive_agent(&self, id: &str) -> Result<(), String> {
            self.at("archive").await;
            self.archived.lock().unwrap().push(id.to_owned());
            Ok(())
        }
        async fn merged_sha(&self, w: &str) -> Result<Option<String>, String> {
            Ok(self.merged.lock().unwrap().get(w).cloned())
        }
        async fn ask_owner(&self, f: &Flaw, _g: Gate) -> Result<String, String> {
            self.asked.lock().unwrap().push(f.id.clone());
            Ok("dc-1".into())
        }
        async fn decision(&self, _id: &str) -> Option<bool> {
            *self.answer.lock().unwrap()
        }
        async fn agent_error(&self, id: &str) -> Option<String> {
            self.at("agent_error").await;
            self.errors.lock().unwrap().get(id).cloned()
        }
        async fn step_ready(&self, step: &Step) -> bool {
            self.at("probe").await;
            // A step without a probe lane is always ready; a probe answers for its own lane only.
            let Some(lane) = step.probe_lane.as_deref() else {
                return true;
            };
            !*self.out.lock().unwrap() && !self.lanes_out.lock().unwrap().iter().any(|l| l == lane)
        }
        fn step_out(&self, step: &Step, _why: &str) {
            if *self.ignore_step_out.lock().unwrap() {
                return;
            }
            let Some(lane) = step.probe_lane.as_deref() else {
                return;
            };
            let mut lanes = self.lanes_out.lock().unwrap();
            if !lanes.iter().any(|l| l == lane) {
                lanes.push(lane.to_owned());
            }
        }
        async fn tell(&self, text: &str) -> bool {
            self.told.lock().unwrap().push(text.to_owned());
            true
        }
        fn limits(&self) -> Limits {
            self.limits.lock().unwrap().unwrap_or(Limits::DEFAULT)
        }
        async fn tell_start(&self, text: &str) {
            self.started.lock().unwrap().push(text.to_owned());
        }
    }

    /// The policy of the tests written before the dispatch caps: they pin other rules (the
    /// per-repo cap, the walk down the chain), so the caps of LA-C035 (alive in all, per lane,
    /// new per tick) are wide here. [`strict`] has the real defaults.
    fn policy() -> Policy {
        Policy {
            max_alive: 100,
            max_per_lane: 100,
            max_new_per_tick: 100,
            ..strict()
        }
    }

    /// A fake clock for the store: minutes after 2026-11-19T00:43:00Z, moved by the test. Nothing
    /// in a test waits for time to pass.
    fn with_clock(s: &Flaws) -> Arc<AtomicI64> {
        let minutes = Arc::new(AtomicI64::new(0));
        let m = minutes.clone();
        let start = chrono::DateTime::parse_from_rfc3339("2026-11-19T00:43:00Z")
            .unwrap()
            .with_timezone(&Utc);
        s.set_clock(move || start + chrono::Duration::minutes(m.load(Ordering::SeqCst)));
        minutes
    }

    /// The defaults: 3 alive in all, 1 per lane, 1 new per tick, 2 per repo.
    fn strict() -> Policy {
        Policy {
            auto_triage: false,
            ..Policy::from_lookup(|_| None, Path::new("/h")).unwrap()
        }
    }

    fn due(s: &Flaws, words: &str, p: Platform, gate: Option<Gate>) -> Flaw {
        let f = s
            .report(
                NewFlaw {
                    words: words.into(),
                    severity: Some(Severity::High),
                    affected: vec![p],
                    gate,
                    ..NewFlaw::default()
                },
                "owner",
            )
            .unwrap()
            .flaw
            .flaw;
        s.set_target(
            &f.id,
            p,
            TargetUpdate {
                layer: Some(Layer::Client),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        f
    }

    fn target(s: &Flaws, id: &str, p: Platform) -> Target {
        s.get(id)
            .unwrap()
            .targets
            .into_iter()
            .find(|t| t.platform == p)
            .unwrap()
    }

    #[tokio::test]
    async fn an_affected_target_with_a_layer_starts_by_itself_whatever_the_severity() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        assert!(h.asked.lock().unwrap().is_empty(), "nobody was asked");
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(t.status, TargetStatus::Fixing);
        assert_eq!(t.agent.as_deref(), Some("ag0"));
        // The next tick starts nothing new.
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_cap_leaves_the_rest_for_later_ticks() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let flaws: Vec<Flaw> = (0..4)
            .map(|i| due(&s, &format!("flaw number {i}"), Platform::Web, None))
            .collect();
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2, "two per repo");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        // One lands: a slot opens, and the next starts.
        let first = s.in_progress().unwrap()[0].1.clone();
        h.merged
            .lock()
            .unwrap()
            .insert(first.workspace.clone().unwrap(), "abcdef1234".into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
        assert!(flaws
            .iter()
            .any(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Fixed));
    }

    #[tokio::test]
    async fn a_merge_is_fixed_not_closed_and_he_hears_the_outcome_once() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &p, &h).await;
        let w = target(&s, &f.id, Platform::Web).workspace.unwrap();
        h.merged.lock().unwrap().insert(w, "abcdef1234".into());
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.commit_sha.as_deref()),
            (TargetStatus::Fixed, Some("abcdef1234"))
        );
        assert_ne!(
            s.get(&f.id).unwrap().flaw.state,
            FlawState::Resolved,
            "only evidence closes it"
        );
        let told = h.told.lock().unwrap();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].contains("fixed in abcdef1234") && told[0].contains("not shown working yet"),
            "{told:?}"
        );
    }

    #[tokio::test]
    async fn an_agent_that_ends_without_a_commit_retries_then_blocks_and_he_hears() {
        // LA-C035 changed the rule this pinned: an agent that worked and ended without a commit
        // is a counted task failure, but the retry waits out a back-off (5 minutes, then 10), and
        // the third failure inside a day blocks the target for a lead (it was two tries, at once).
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &p, &h).await;
        h.worked.lock().unwrap().insert("ag0".into());
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            h.tasks.lock().unwrap().len(),
            1,
            "it waits out five minutes"
        );
        clock.fetch_add(5, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        // Started again, on a new branch.
        assert_eq!(h.tasks.lock().unwrap().len(), 2);
        assert!(h.tasks.lock().unwrap()[1].contains("-web-2-"));
        h.worked.lock().unwrap().insert("ag1".into());
        h.status
            .lock()
            .unwrap()
            .insert("ag1".into(), "closed".into());
        tick(&s, &p, &h).await;
        clock.fetch_add(9, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(h.tasks.lock().unwrap().len(), 2, "ten minutes this time");
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(h.tasks.lock().unwrap().len(), 3);
        h.worked.lock().unwrap().insert("ag2".into());
        h.status
            .lock()
            .unwrap()
            .insert("ag2".into(), "closed".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Blocked
        );
        assert_eq!(h.tasks.lock().unwrap().len(), 3, "no fourth try");
        let told = h.told.lock().unwrap().clone();
        assert_eq!(told.len(), 1, "told once: {told:?}");
        assert!(
            told[0].contains("couldn't fix it after 3 tries") && told[0].contains("needs a lead"),
            "{told:?}"
        );
    }

    #[tokio::test]
    async fn a_quota_stop_costs_no_try_and_the_step_below_takes_it() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        // What the Claude CLI really answered on 2026-11-02.
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors.lock().unwrap().insert(
            "ag0".into(),
            "You've hit your weekly limit · resets Nov 4, 12pm (UTC)".into(),
        );
        tick(&s, &p, &h).await;
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.attempts, t.agent.as_deref()),
            (TargetStatus::Fixing, 1, Some("ag1"))
        );
        assert_eq!(
            h.ran()[1],
            p.chain[1].label(),
            "the lane that ran out is the reason, not the try"
        );
        assert!(
            h.told.lock().unwrap().is_empty(),
            "he isn't told it couldn't be fixed"
        );
        // The next try goes back to the cheap end: the step that ran out is only skipped while out.
        h.status
            .lock()
            .unwrap()
            .insert("ag1".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag1".into(), "the provider crashed".into());
        // LA-C035: a crash is the task's only when the agent provably worked, and its retry then
        // waits out the back-off.
        h.worked.lock().unwrap().insert("ag1".into());
        *h.lanes_out.lock().unwrap() = vec![];
        tick(&s, &p, &h).await;
        clock.fetch_add(5, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(
            h.ran()[2],
            p.chain[1].label(),
            "the second counted try starts on its own step, index 1"
        );
    }

    #[tokio::test]
    async fn nothing_starts_while_the_walk_down_finds_no_ready_step() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        *h.out.lock().unwrap() = true;
        for _ in 0..4 {
            tick(&s, &p, &h).await;
        }
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.attempts, t.agent),
            (TargetStatus::Affected, 0, None)
        );
        assert!(
            h.agents.lock().unwrap().is_empty(),
            "no worktree or agent per tick while every step is out"
        );
        assert!(
            h.told.lock().unwrap().is_empty(),
            "he isn't told it couldn't be fixed"
        );
        assert_eq!(
            dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
                .await
                .unwrap_err()
                .code,
            "lane_out"
        );
        // A step is back: it starts by itself.
        *h.out.lock().unwrap() = false;
        tick(&s, &p, &h).await;
        assert_eq!(
            (target(&s, &f.id, Platform::Web).status, h.ran()),
            (TargetStatus::Fixing, vec![p.chain[0].label()])
        );
    }

    #[tokio::test]
    async fn the_walk_down_starts_on_the_first_step_that_has_quota() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        due(&s, "emoji", Platform::Web, None);
        // A limit answered on the free lane earlier; its probe still says out.
        h.step_out(&p.chain[0], "the free lane answered its reset date");
        tick(&s, &p, &h).await;
        assert_eq!(
            h.ran(),
            vec![p.chain[1].label()],
            "it never comes back up to the step it started at"
        );
    }

    #[tokio::test]
    async fn the_start_names_the_agent_that_runs_the_try() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &p, &h).await;
        // His history: what the work was done with.
        let e = s
            .get(&f.id)
            .unwrap()
            .events
            .iter()
            .rev()
            .find(|e| e.kind == "started")
            .unwrap()
            .clone();
        let d = e.detail;
        assert_eq!(
            (d["provider"].as_str(), d["model"].as_str()),
            (Some("opencode"), Some("opencode/deepseek-v4.1-flash-free")),
            "{d}"
        );
        // And the line in his chat.
        let line = h.started.lock().unwrap()[0].clone();
        assert!(
            line.contains("opencode:opencode/deepseek-v4.1-flash-free")
                && line.contains(&f.id)
                && line.contains("try 1"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_target_someone_else_holds_is_left_alone() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        // A session took it on by hand: no dispatcher agent on it.
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::Fixing),
                workspace: Some("/w/hand".into()),
                session: Some("claude-session".into()),
                ..TargetUpdate::default()
            },
            "someone",
        )
        .unwrap();
        tick(&s, &p, &h).await;
        assert!(
            h.agents.lock().unwrap().is_empty(),
            "the dispatcher never starts a second agent on it"
        );
        assert_eq!(
            dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
                .await
                .unwrap_err()
                .code,
            "not_claimable"
        );
        // The same on a target that never moved off "affected": the session holds it, not the status.
        let g = due(&s, "the same on another client", Platform::Ios, None);
        s.set_target(
            &g.id,
            Platform::Ios,
            TargetUpdate {
                session: Some("claude-session".into()),
                ..TargetUpdate::default()
            },
            "someone",
        )
        .unwrap();
        tick(&s, &p, &h).await;
        assert!(
            h.agents.lock().unwrap().is_empty(),
            "a target with a session isn't the dispatcher's to claim"
        );
        assert_eq!(
            dispatch_one(&s, &p, &h, &g.id, Platform::Ios, "t")
                .await
                .unwrap_err()
                .code,
            "not_claimable"
        );
    }

    #[tokio::test]
    async fn a_real_failure_still_counts() {
        // LA-C035: "real" is an agent that provably worked. It is a task failure, and the retry
        // (the second try) comes after the back-off, not in the same round.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), "the provider crashed".into());
        h.worked.lock().unwrap().insert("ag0".into());
        tick(&s, &p, &h).await;
        assert_eq!(s.task_failures(&f.id, Platform::Web).unwrap(), 1);
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "it waits out the back-off"
        );
        clock.fetch_add(5, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &f.id, Platform::Web).attempts,
            2,
            "started again, as the second try"
        );
        // An agent that did no work and crashed is the lane's: no failure is counted for it.
        h.status
            .lock()
            .unwrap()
            .insert("ag1".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag1".into(), "the provider crashed".into());
        tick(&s, &p, &h).await;
        assert_eq!(s.task_failures(&f.id, Platform::Web).unwrap(), 1);
    }

    #[tokio::test]
    async fn a_failed_try_escalates_to_the_next_step_of_the_chain() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        // Room for the whole chain: two tries is the default, and the third step is the point here.
        let p = Policy {
            max_attempts: 4,
            ..policy()
        };
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        assert_eq!(
            h.ran(),
            vec![p.chain[0].label()],
            "a free agent takes the first try"
        );
        for i in 0..3 {
            h.status
                .lock()
                .unwrap()
                .insert(format!("ag{i}"), "error".into());
            h.errors
                .lock()
                .unwrap()
                .insert(format!("ag{i}"), "the provider crashed".into());
            // LA-C035: the agent worked (a counted failure), and its retry waits out the back-off.
            h.worked.lock().unwrap().insert(format!("ag{i}"));
            tick(&s, &p, &h).await;
            clock.fetch_add(130, Ordering::SeqCst);
            tick(&s, &p, &h).await;
            let down = (i + 1).min(p.chain.len() - 1);
            assert_eq!(
                h.ran()[i + 1],
                p.chain[down].label(),
                "try {} is a step further down, and the last one stays there",
                i + 2
            );
        }
        assert_eq!(target(&s, &f.id, Platform::Web).attempts, 4);
    }

    #[tokio::test]
    async fn a_duplicate_stops_its_running_agent() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let a = s
            .report(
                NewFlaw {
                    words: "older one".into(),
                    ..NewFlaw::default()
                },
                "o",
            )
            .unwrap()
            .flaw
            .flaw;
        let f = due(&s, "same one", Platform::Web, None);
        tick(&s, &p, &h).await;
        s.patch(
            &f.id,
            FlawPatch {
                duplicate_of: Some(a.id),
                ..FlawPatch::default()
            },
            "t",
        )
        .unwrap();
        tick(&s, &p, &h).await;
        assert_eq!(h.stopped.lock().unwrap().as_slice(), ["ag0"]);
        assert_ne!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Fixing
        );
    }

    #[tokio::test]
    async fn a_gate_asks_once_and_waits() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "needs a card", Platform::Web, Some(Gate::Payment));
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        assert_eq!(h.asked.lock().unwrap().len(), 1, "asked once");
        assert!(h.agents.lock().unwrap().is_empty());
        assert_eq!(
            dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
                .await
                .unwrap_err()
                .code,
            "needs_owner"
        );
        *h.answer.lock().unwrap() = Some(true);
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_gate_answered_no_dismisses_it() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "needs a card", Platform::Web, Some(Gate::Payment));
        tick(&s, &p, &h).await;
        *h.answer.lock().unwrap() = Some(false);
        tick(&s, &p, &h).await;
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Dismissed);
        assert!(h.agents.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn when_agentd_fails_the_claim_goes_back() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        *h.fail_agent.lock().unwrap() = true;
        let e = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap_err();
        assert_eq!(e.code, "dispatch_failed");
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.agent.clone(), t.attempts),
            (TargetStatus::Affected, None, 0)
        );
        // An outage never blocks a flaw, however long it lasts.
        for _ in 0..5 {
            tick(&s, &p, &h).await;
        }
        assert_eq!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Affected
        );
        *h.fail_agent.lock().unwrap() = false;
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Fixing
        );
    }

    #[tokio::test]
    async fn off_means_no_starts_but_explicit_dispatch_works() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            auto_dispatch: false,
            ..policy()
        };
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        assert!(h.agents.lock().unwrap().is_empty());
        dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap();
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn triage_starts_one_agent_per_report_within_the_cap() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            auto_triage: true,
            ..policy()
        };
        for i in 0..3 {
            s.report(
                NewFlaw {
                    words: format!("report number {i}"),
                    ..NewFlaw::default()
                },
                "o",
            )
            .unwrap();
        }
        // LA-C035: one triage start a tick (the lock is not held for a loop of them), so the two
        // slots fill over two ticks instead of one.
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "one a tick");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2, "two slots");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
    }

    /// The lane a step asks about.
    fn lane(step: &Step) -> String {
        step.probe_lane.clone().unwrap_or_default()
    }

    #[tokio::test]
    async fn triage_takes_a_second_cheap_step_when_the_first_is_out() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            auto_triage: true,
            triage_steps: 2,
            ..policy()
        };
        s.report(
            NewFlaw {
                words: "report number one".into(),
                ..NewFlaw::default()
            },
            "o",
        )
        .unwrap();
        // The free lane answered a limit; its probe still says out.
        h.step_out(&p.chain[0], "the free lane is out");
        tick(&s, &p, &h).await;
        assert_eq!(
            h.ran(),
            vec![p.chain[1].label()],
            "the report is looked at, on the next cheap step"
        );
        assert_eq!(
            s.triaging().unwrap()[0].triage_step,
            Some(1),
            "which step the triage runs on is stored with it"
        );
    }

    #[tokio::test]
    async fn triage_waits_while_its_own_steps_are_all_out() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        // The default: one cheap step, and nothing else.
        let p = Policy {
            auto_triage: true,
            ..policy()
        };
        s.report(
            NewFlaw {
                words: "report number one".into(),
                ..NewFlaw::default()
            },
            "o",
        )
        .unwrap();
        h.step_out(&p.chain[0], "the free lane is out");
        for _ in 0..4 {
            tick(&s, &p, &h).await;
        }
        assert!(
            h.agents.lock().unwrap().is_empty(),
            "the steps below it are more than triage may use"
        );
        // The lane is back: it starts by itself.
        *h.lanes_out.lock().unwrap() = vec![];
        tick(&s, &p, &h).await;
        assert_eq!(h.ran(), vec![p.chain[0].label()]);
    }

    #[tokio::test]
    async fn a_triage_limit_marks_out_the_step_that_ran() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            auto_triage: true,
            triage_steps: 2,
            ..policy()
        };
        s.report(
            NewFlaw {
                words: "report number one".into(),
                ..NewFlaw::default()
            },
            "o",
        )
        .unwrap();
        h.step_out(&p.chain[0], "the free lane is out");
        tick(&s, &p, &h).await;
        assert_eq!(h.ran(), vec![p.chain[1].label()]);
        // It ends on a limit: the lane that ran is the one out, and the first is left as it was.
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors.lock().unwrap().insert(
            "ag0".into(),
            "You've hit your weekly limit · resets Nov 4, 12pm (UTC)".into(),
        );
        tick(&s, &p, &h).await;
        assert_eq!(
            *h.lanes_out.lock().unwrap(),
            vec![lane(&p.chain[0]), lane(&p.chain[1])],
            "the step that ran, not always the first"
        );
        assert_eq!(
            s.triage_due().unwrap().len(),
            1,
            "the report is triable again"
        );
        // Both cheap steps are out now: it waits, and the expensive one below is never used.
        for _ in 0..2 {
            tick(&s, &p, &h).await;
        }
        assert_eq!(
            h.ran(),
            vec![p.chain[1].label()],
            "no second triage agent, and never the last step"
        );
    }

    #[tokio::test]
    async fn a_verified_target_is_told_as_shown_working() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        s.record_merge(&f.id, Platform::Web, "abcdef1", "l")
            .unwrap();
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::Verified),
                evidence: Some(Evidence {
                    kind: EvidenceKind::Screenshot,
                    reference: "a.png".into(),
                    build: None,
                    note: None,
                }),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        tick(&s, &p, &h).await;
        assert!(h.told.lock().unwrap()[0].contains("fixed and shown working"));
    }

    /// A report nobody has looked at, which is what triage is for.
    fn report(s: &Flaws, words: &str) -> Flaw {
        s.report(
            NewFlaw {
                words: words.into(),
                ..NewFlaw::default()
            },
            "o",
        )
        .unwrap()
        .flaw
        .flaw
    }

    fn triaging() -> Policy {
        Policy {
            auto_triage: true,
            ..policy()
        }
    }

    /// What the free step really answered on 2026-11-13.
    const LIMIT: &str = "You've hit your weekly limit · resets Nov 4, 12pm (UTC)";

    /// Move a flaw's round back in time, so a minutes limit is reached without waiting for one.
    /// A second connection: the store keeps its own, and the file is the only thing they share.
    fn age(db: &std::path::Path, id: &str, minutes: i64) {
        let at = (Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339();
        rusqlite::Connection::open(db)
            .unwrap()
            .execute(
                "UPDATE flaws SET updated_at=?1 WHERE id=?2",
                rusqlite::params![at, id],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn one_triage_agent_at_a_time_for_a_flaw() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        // The round gave the triage back while agentd still had the agent at work in the directory
        // it started it in, which is how fl-15bb8251 got a new one every hour for a day.
        s.reset_triage(&f.id, LIMIT, "t").unwrap();
        for _ in 0..3 {
            tick(&s, &p, &h).await;
        }
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "agentd still holds this flaw's one triage"
        );
        // The hand-off continuation inherits the flaw and triage labels, so it stands in the way
        // of a second agent just as much.
        h.continued
            .lock()
            .unwrap()
            .insert("ag0".into(), "deep1".into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        // Both put away: nothing of this flaw's triage lives any more, so a fresh one may start.
        h.archive_agent("ag0").await.unwrap();
        h.archive_agent("deep1").await.unwrap();
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_triage_that_ended_is_archived_once_the_flaw_knows() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            *h.archived.lock().unwrap(),
            vec!["ag0".to_string()],
            "it is not left standing idle"
        );
        assert!(
            s.get(&f.id).unwrap().flaw.triaged_at.is_some(),
            "the triage is finished"
        );
        assert!(s.triage_due().unwrap().is_empty());
        assert_eq!(h.agents.lock().unwrap().len(), 1, "and never tried again");
    }

    #[tokio::test]
    async fn a_triage_stopped_at_the_minutes_limit_is_stopped_and_archived() {
        // A file rather than `in_memory`, so the round can be aged past its fifteen minutes.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let (s, h) = (Flaws::open(&db).unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        // Whatever the clock says, the agent has had its triage's whole fifteen minutes.
        let old = rusqlite::Connection::open(&db).unwrap();
        old.execute(
            "UPDATE flaws SET updated_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            [f.id.as_str()],
        )
        .unwrap();
        drop(old);
        tick(&s, &p, &h).await;
        assert_eq!(*h.stopped.lock().unwrap(), vec!["ag0".to_string()]);
        assert_eq!(
            *h.archived.lock().unwrap(),
            vec!["ag0".to_string()],
            "stopped, then put away"
        );
        assert!(s.get(&f.id).unwrap().flaw.triaged_at.is_some());
        assert_eq!(h.agents.lock().unwrap().len(), 1, "no second agent on it");
    }

    #[tokio::test]
    async fn a_triage_that_was_handed_on_ends_with_the_expensive_agent() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            triage_steps: 2,
            ..triaging()
        };
        let f = report(&s, "report one");
        // The free lane is out: the triage runs on the second cheap step.
        h.step_out(&p.chain[0], "the free lane is out");
        tick(&s, &p, &h).await;
        assert_eq!(h.ran(), vec![p.chain[1].label()]);
        // agentd carried the stopped run on to a deep agent of its own choice. Triage never goes
        // up the chain: that agent's work was nobody's ask, and the cheap one is what we track.
        h.continued
            .lock()
            .unwrap()
            .insert("ag0".into(), "deep1".into());
        h.status
            .lock()
            .unwrap()
            .insert("deep1".into(), "running".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            *h.stopped.lock().unwrap(),
            vec!["deep1".to_string()],
            "stopped first, since it is at work"
        );
        assert_eq!(
            *h.archived.lock().unwrap(),
            vec!["deep1".to_string(), "ag0".to_string()],
            "the continuation and the agent it continues, both away"
        );
        assert_eq!(
            *h.lanes_out.lock().unwrap(),
            vec![lane(&p.chain[0]), lane(&p.chain[1])],
            "the step that ran is the lane out"
        );
        assert_eq!(
            s.triage_resets(&f.id).unwrap(),
            1,
            "the triage is given back, as on a limit"
        );
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "and no third agent this round"
        );
    }

    #[tokio::test]
    async fn the_fourth_limit_stop_leaves_the_triage_to_the_owner() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        for i in 0..GIVE_UP_AFTER {
            *h.lanes_out.lock().unwrap() = vec![];
            tick(&s, &p, &h).await;
            assert_eq!(h.agents.lock().unwrap().len(), i + 1);
            let ag = format!("ag{i}");
            h.status.lock().unwrap().insert(ag.clone(), "error".into());
            h.errors.lock().unwrap().insert(ag, LIMIT.into());
            tick(&s, &p, &h).await;
            assert_eq!(s.triage_resets(&f.id).unwrap(), i + 1, "stop {}", i + 1);
            assert_eq!(
                *h.archived.lock().unwrap(),
                (0..=i).map(|n| format!("ag{n}")).collect::<Vec<_>>(),
                "each one away before the next starts"
            );
        }
        // The fourth stop on the third reset: not a fifth agent.
        *h.lanes_out.lock().unwrap() = vec![];
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 4);
        h.status
            .lock()
            .unwrap()
            .insert("ag3".into(), "error".into());
        h.errors.lock().unwrap().insert(
            "ag3".into(),
            "Free usage limit reached (code: quota)".into(),
        );
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 4, "no fifth triage agent");
        let d = s.get(&f.id).unwrap();
        assert_eq!(
            d.events.iter().filter(|e| e.kind == "triage_reset").count(),
            3,
            "and no fourth reset either"
        );
        assert!(
            d.flaw.triaged_at.is_some(),
            "the triage is finished, not given back"
        );
        assert!(
            d.events.iter().any(|e| e.kind == "triaged"
                && e.detail["why"].as_str()
                    == Some("gave up after 3 limit stops; triage it by hand")),
            "{:?}",
            d.events
        );
        assert!(
            s.triage_due().unwrap().is_empty(),
            "the flaw stops coming back by itself"
        );
        assert!(h.archived.lock().unwrap().contains(&"ag3".to_owned()));
    }

    #[tokio::test]
    async fn a_dispatchers_agent_opts_out_of_the_hand_off() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        // A fix: the owner's yes covers the step it starts on, not wherever a quota leads agentd.
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &policy(), &h).await;
        assert_eq!(h.labels.lock().unwrap().len(), 1);
        // A report, triaged on the cheap step.
        let g = report(&s, "report one");
        tick(&s, &triaging(), &h).await;
        let labels = h.labels.lock().unwrap();
        assert_eq!(labels.len(), 2, "the fix, and one triage");
        for l in labels.iter() {
            assert_eq!(l["lucina.handoff-off"].as_str(), Some("1"), "{l}");
        }
        assert_eq!(labels[0]["flaw"].as_str(), Some(f.id.as_str()));
        assert_eq!(labels[1]["flaw"].as_str(), Some(g.id.as_str()));
        assert_eq!(labels[1]["triage"].as_str(), Some("1"));
    }

    #[tokio::test]
    async fn a_triage_still_initializing_after_five_minutes_is_stopped_and_archived() {
        // A file rather than `in_memory`, so the round can be aged past its five minutes.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let (s, h) = (Flaws::open(&db).unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        // The OpenCode agents of 2026-11-13: three of them, sitting on `initializing` for hours
        // because nothing ever said the provider had not answered.
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "initializing".into());
        tick(&s, &p, &h).await;
        assert!(
            h.stopped.lock().unwrap().is_empty() && h.archived.lock().unwrap().is_empty(),
            "five minutes have not gone"
        );
        age(&db, &f.id, 6);
        tick(&s, &p, &h).await;
        assert_eq!(*h.stopped.lock().unwrap(), vec!["ag0".to_string()]);
        assert_eq!(*h.archived.lock().unwrap(), vec!["ag0".to_string()]);
        let d = s.get(&f.id).unwrap();
        assert!(
            d.events
                .iter()
                .any(|e| e.kind == "triaged"
                    && e.detail["why"].as_str() == Some("stuck initializing")),
            "{:?}",
            d.events
        );
        assert!(
            h.triage_agents(&f.id).await.unwrap().is_empty(),
            "it is out of the way of the next one"
        );
        // The flaw is triaged, not given back: no reset on the way, and nothing new starts.
        assert_eq!(s.triage_resets(&f.id).unwrap(), 0);
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "nothing new starts");
    }

    #[tokio::test]
    async fn a_triage_that_ended_on_another_error_is_finished_not_reset() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), "the provider crashed".into());
        tick(&s, &p, &h).await;
        let d = s.get(&f.id).unwrap();
        assert!(
            d.flaw.triaged_at.is_some(),
            "an answer, not a lane out of quota"
        );
        assert_eq!(s.triage_resets(&f.id).unwrap(), 0, "and no reset");
        assert!(
            d.events.iter().any(|e| e.kind == "triaged"
                && e.detail["why"].as_str()
                    == Some("the agent ended on an error: the provider crashed")),
            "{:?}",
            d.events
        );
        assert_eq!(*h.archived.lock().unwrap(), vec!["ag0".to_string()]);
        assert!(s.triage_due().unwrap().is_empty());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "nothing new starts");
    }

    #[tokio::test]
    async fn a_triage_agent_agentd_does_not_know_is_finished() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = triaging();
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        // No status at all: agentd has forgotten the agent, so there is nothing to archive.
        h.status.lock().unwrap().remove("ag0");
        tick(&s, &p, &h).await;
        let d = s.get(&f.id).unwrap();
        assert!(
            d.events
                .iter()
                .any(|e| e.kind == "triaged"
                    && e.detail["why"].as_str() == Some("the agent is gone")),
            "{:?}",
            d.events
        );
        assert_eq!(
            s.triage_resets(&f.id).unwrap(),
            0,
            "no reset from a gone agent"
        );
        assert!(
            h.archived.lock().unwrap().is_empty(),
            "there was nothing left to put away"
        );
        assert_eq!(*h.stopped.lock().unwrap(), Vec::<String>::new());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "nothing new starts");
    }

    #[tokio::test]
    async fn an_opencode_triage_is_archived_when_it_ends() {
        // LA-C035 condition 2: "connection refused" is a lane failure now (the breaker opens and the
        // flaw stays triage-due), so the error this test ends the round on is one that is not.
        // The tracking is the agent's, not the provider's: an OpenCode step is followed, ended and
        // put away exactly like a Claude one.
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            chain: vec![Step::new("opencode", Some("opencode/big-pickle"), None)],
            ..triaging()
        };
        let f = report(&s, "report one");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        assert_eq!(h.ran(), vec!["opencode:opencode/big-pickle"]);
        assert_eq!(
            h.labels.lock().unwrap()[0]["lucina.handoff-off"].as_str(),
            Some("1"),
            "and it is opted out of the hand-off like any other"
        );
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors.lock().unwrap().insert(
            "ag0".into(),
            "opencode: the run ended on a bad answer".into(),
        );
        tick(&s, &p, &h).await;
        assert_eq!(
            *h.archived.lock().unwrap(),
            vec!["ag0".to_string()],
            "the OpenCode agent is put away too"
        );
        assert!(
            s.get(&f.id)
                .unwrap()
                .events
                .iter()
                .any(|e| e.kind == "triaged"
                    && e.detail["why"].as_str()
                        == Some(
                            "the agent ended on an error: opencode: the run ended on a bad answer"
                        )),
            "and its error is what ended the round"
        );
    }

    // ------------------------------------------------------------ LA-C035: the dispatcher cannot storm

    /// What the Claude lane really answered at 00:43-00:52 on 2026-11-19, about 2 s after each create.
    const SIGN_IN: &str =
        "Failed to authenticate: OAuth session expired and could not be refreshed";

    /// A chain of the one lane the incident had left: Claude sonnet.
    fn one_lane() -> Policy {
        Policy {
            chain: vec![Step::new(
                "claude",
                Some("claude-sonnet-5-5"),
                Some("claude-sonnet"),
            )],
            ..strict()
        }
    }

    /// The log of the dispatcher, as text, for what the lines must say.
    #[derive(Clone, Default)]
    struct LogBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> LogBuf {
            self.clone()
        }
    }

    impl LogBuf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// Capture the INFO lines of this thread for as long as the guard lives.
    fn capture() -> (LogBuf, tracing::subscriber::DefaultGuard) {
        let buf = LogBuf::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        (buf, tracing::subscriber::set_default(sub))
    }

    #[tokio::test]
    async fn the_00_52_incident_one_dispatch_one_opening_one_record_and_nothing_left_alive() {
        // The store as at 00:43: 15 due targets over two repos, one lane left, and an agentd whose
        // every agent dies at sign-in about 2 s after its create (before the next tick). 29 agents
        // came of it (store-policy interleaving 4, server-loop R1, R2 and R7).
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        let clock = with_clock(&s);
        for i in 0..8 {
            due(&s, &format!("web flaw {i}"), Platform::Web, None);
        }
        for i in 0..7 {
            due(&s, &format!("juno flaw {i}"), Platform::Juno, None);
        }
        *h.dies_with.lock().unwrap() = Some(SIGN_IN.into());
        for _ in 0..10 {
            tick(&s, &p, &h).await;
            clock.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "exactly one dispatch in ten ticks"
        );
        let lanes = s.lanes().unwrap();
        assert_eq!(
            lanes
                .iter()
                .map(|l| (l.lane.as_str(), l.state))
                .collect::<Vec<_>>(),
            vec![("claude-sonnet", LaneState::Open)],
            "one breaker, open"
        );
        assert_eq!(
            h.ops.lock().unwrap().len(),
            1,
            "one ops record: {:?}",
            h.ops
        );
        let due_now = s.due().unwrap();
        assert_eq!(due_now.len(), 15, "every target is still due");
        assert!(
            due_now.iter().all(|(_, t)| t.attempts == 0),
            "no attempt was counted"
        );
        assert!(h.alive_now().is_empty(), "no agent left alive");
        assert_eq!(*h.stopped.lock().unwrap(), vec!["ag0".to_string()]);
        assert_eq!(*h.archived.lock().unwrap(), vec!["ag0".to_string()]);
        assert!(h.told.lock().unwrap().is_empty(), "he is told nothing");

        // A probe passes: the lane is half-open and exactly one agent may try it.
        h.lanes_out.lock().unwrap().clear();
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::HalfOpen);
        // Its first turn fails the same way: the lane opens again (a new opening), nothing more.
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        assert_eq!(h.ops.lock().unwrap().len(), 2);
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        // The lane is really back: one agent tries it, its first turn works, the breaker closes.
        *h.dies_with.lock().unwrap() = None;
        h.lanes_out.lock().unwrap().clear();
        // (a failed trial shuts the lane for 5 minutes: LA-C035 condition 1)
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2, "not before the back-off");
        clock.fetch_add(5, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Closed);
        // And the lane's own cap holds from then on: its one agent is alive.
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn with_the_whole_chain_a_dead_provider_costs_one_agent_per_lane_at_most() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        let clock = with_clock(&s);
        for i in 0..6 {
            due(&s, &format!("flaw {i}"), Platform::Web, None);
        }
        *h.dies_with.lock().unwrap() = Some(SIGN_IN.into());
        for _ in 0..10 {
            tick(&s, &p, &h).await;
            clock.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(
            h.ran(),
            p.chain.iter().map(Step::label).collect::<Vec<_>>(),
            "one agent on each lane, and none after"
        );
        assert_eq!(h.ops.lock().unwrap().len(), 3);
        assert!(h.alive_now().is_empty());
    }

    #[tokio::test]
    async fn the_open_breaker_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let p = one_lane();
        {
            let (s, h) = (Flaws::open(&db).unwrap(), Fake::default());
            due(&s, "emoji", Platform::Web, None);
            *h.dies_with.lock().unwrap() = Some(SIGN_IN.into());
            tick(&s, &p, &h).await;
            tick(&s, &p, &h).await;
            assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        }
        // A new process: its memory of lanes is empty and its fake agentd has nothing, but the
        // lane is still open, and while its probe fails nothing is dispatched on it.
        let (s, h) = (Flaws::open(&db).unwrap(), Fake::default());
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        h.step_out(&p.chain[0], "still out");
        for _ in 0..3 {
            tick(&s, &p, &h).await;
        }
        assert!(h.agents.lock().unwrap().is_empty());
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
    }

    #[tokio::test]
    async fn a_probe_that_cannot_run_or_times_out_is_not_ready() {
        use std::ffi::OsStr;
        let within = Duration::from_millis(300);
        let said = |t: &str| format!("{{\"outcome\":\"{t}\"}}");
        // Cannot run, runs past its time, prints nothing, prints something else: not ready.
        assert!(!probe_cmd(OsStr::new("/nonexistent/lane"), &["probe"], within).await);
        assert!(!probe_cmd(OsStr::new("sleep"), &["30"], within).await);
        assert!(!probe_cmd(OsStr::new("true"), &[], within).await);
        assert!(!probe_cmd(OsStr::new("echo"), &["not json"], within).await);
        assert!(!probe_cmd(OsStr::new("echo"), &[said("quota").as_str()], within).await);
        assert!(!probe_cmd(OsStr::new("echo"), &[said("failed").as_str()], within).await);
        // Only an ok is ready.
        assert!(probe_cmd(OsStr::new("echo"), &[said("ok").as_str()], within).await);
    }

    #[tokio::test]
    async fn an_open_lane_whose_probe_cannot_answer_stays_open_and_starts_nothing() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        due(&s, "emoji", Platform::Web, None);
        s.lane_open("claude-sonnet", SIGN_IN).unwrap();
        *h.out.lock().unwrap() = true;
        for _ in 0..4 {
            tick(&s, &p, &h).await;
        }
        assert!(h.agents.lock().unwrap().is_empty());
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        assert_eq!(
            dispatch_one(&s, &p, &h, &s.due().unwrap()[0].0.id, Platform::Web, "t")
                .await
                .unwrap_err()
                .code,
            "lane_out"
        );
        // The probe answers: half-open, and one start.
        *h.out.lock().unwrap() = false;
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    // The server-loop interleavings (disp-01-server-loop.md §5), in its numbering.

    #[tokio::test]
    async fn r1_a_dead_at_sign_in_agent_does_not_free_a_slot_that_the_same_tick_refills() {
        // Two due targets in one repo, the policy's own defaults. Both agents end at sign-in.
        // Today: 2 more agents in the same tick; now the dead agent is put away, the lane opens,
        // and the tick that saw it die starts nothing on that lane. Invariant: agents started
        // never exceed what the caps allow, whatever the agents' state.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        due(&s, "one", Platform::Web, None);
        due(&s, "two", Platform::Web, None);
        *h.dies_with.lock().unwrap() = Some(SIGN_IN.into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "one new dispatch a tick");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        assert!(h.alive_now().is_empty());
    }

    #[tokio::test]
    async fn r2_an_owner_approved_gated_flaw_still_fights_for_the_caps() {
        // Approved gated picks used to take no slot: every one of them started in one tick.
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy {
            max_alive: 3,
            max_per_lane: 100,
            max_new_per_tick: 100,
            ..strict()
        };
        for i in 0..5 {
            due(
                &s,
                &format!("gated {i}"),
                Platform::Web,
                Some(Gate::Payment),
            );
        }
        tick(&s, &p, &h).await;
        assert!(h.agents.lock().unwrap().is_empty(), "asked, not started");
        *h.answer.lock().unwrap() = Some(true);
        tick(&s, &p, &h).await;
        assert_eq!(
            h.agents.lock().unwrap().len(),
            2,
            "the per-repo cap of two holds for approved picks"
        );
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        assert_eq!(s.due().unwrap().len(), 3, "the rest wait, still affected");
        // And the total: another repo takes the third alive, never a fourth.
        for i in 0..3 {
            due(
                &s,
                &format!("juno {i}"),
                Platform::Juno,
                Some(Gate::Payment),
            );
        }
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3, "3 alive in all");
    }

    #[tokio::test]
    async fn r3_a_create_answered_late_keeps_one_agent_and_one_claim() {
        // agentd made the agent; the answer came back as a timeout. The claim must not be given
        // back (that is what let a second agent start on the same target).
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        *h.late_error.lock().unwrap() = Some("Lucina Agents (agentd) timed out".into());
        let t = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap();
        assert_eq!(t.agent.as_deref(), Some("ag0"), "the agent agentd made");
        let again = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap_err();
        assert_eq!(again.code, "not_claimable");
        tick(&s, &p, &h).await;
        assert_eq!(h.tasks.lock().unwrap().len(), 1, "one worktree");
        assert_eq!(h.agents.lock().unwrap().len(), 1, "one agent");
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.attempts, t.agent.as_deref()),
            (TargetStatus::Fixing, 1, Some("ag0"))
        );
        assert!(h.stopped.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn r4_reconcile_never_clears_a_claim_it_did_not_examine() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        let step = p.chain[0].clone();
        // ag0 ended on a lane error; reconcile reads it, then stops at its error read.
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), "You've hit your weekly limit".into());
        let pause = h.pause_at("agent_error");
        let meanwhile = async {
            pause.reached.notified().await;
            // While it is stopped: the claim is given back, taken again and attached to ag1.
            let read = s.get(&f.id).unwrap().targets[0].claim_ref();
            s.give_back_if(&f.id, Platform::Web, &read, "other", false, "t")
                .unwrap()
                .unwrap();
            s.claim(&f.id, Platform::Web, "/r", "t").unwrap();
            s.attach(&f.id, Platform::Web, "w2", "ag1", &step, "t")
                .unwrap();
            pause.release.notify_one();
        };
        let mut probes = Probes::default();
        tokio::join!(reconcile(&s, &p, &h, &mut probes), meanwhile);
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.agent.as_deref()),
            (TargetStatus::Fixing, Some("ag1")),
            "the newer claim is untouched"
        );
        assert!(
            !h.stopped.lock().unwrap().contains(&"ag1".to_string()),
            "and its agent is not stopped"
        );
    }

    #[tokio::test]
    async fn r5_the_caps_hold_when_a_tick_meets_dispatch_one_over_a_slow_probe() {
        // dispatch_one reads the caps, then waits on a probe; the tick meets it there. One lock
        // makes the pair one step: the tick waits, and what it reads is what it acts on.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        let a = due(&s, "one", Platform::Web, None);
        due(&s, "two", Platform::Web, None);
        let pause = h.pause_at("probe");
        let (s, h, p) = (&s, &h, &p);
        let slow = async { dispatch_one(s, p, h, &a.id, Platform::Web, "t").await };
        let rival = async { tick(s, p, h).await };
        let control = async {
            pause.reached.notified().await;
            // The tick is parked on the lock: it has started nothing while the probe waits.
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            assert!(
                h.agents.lock().unwrap().is_empty(),
                "the tick waits its turn"
            );
            pause.release.notify_one();
        };
        let (explicit, _, _) = tokio::join!(slow, rival, control);
        explicit.unwrap();
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "one lane, one agent: never a second from the other source"
        );
        // A third source is told busy, cleanly, with nothing started.
        let b = s.due().unwrap()[0].0.id.clone();
        let e = dispatch_one(s, p, h, &b, Platform::Web, "t")
            .await
            .unwrap_err();
        assert_eq!((e.status.as_u16(), e.code.as_str()), (409, "busy"));
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn r6_a_restart_does_not_charge_a_try_for_a_claim_that_never_saw_its_agent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let (s, h) = (Flaws::open(&db).unwrap(), Fake::default());
        let p = Policy {
            auto_dispatch: false,
            ..policy()
        };
        let clock = with_clock(&s);
        // Two claims the server stopped on before it attached: agentd made an agent for the
        // first, and nothing for the second.
        let (a, b) = (
            due(&s, "has an agent", Platform::Web, None),
            due(&s, "has none", Platform::Web, None),
        );
        for f in [&a, &b] {
            s.claim(&f.id, Platform::Web, "/r", "t").unwrap();
        }
        let step = p.chain[0].clone();
        let labels =
            json!({"from": "lucina", "flaw": a.id, "platform": "web", "lucina.handoff-off": "1"});
        let made = h
            .start_agent(&p, &step, "/w/x", "<owner_words>", "t", labels, None)
            .await
            .unwrap();
        clock.fetch_add(6, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        let (ta, tb) = (
            target(&s, &a.id, Platform::Web),
            target(&s, &b.id, Platform::Web),
        );
        assert_eq!(
            (ta.status, ta.agent.as_deref(), ta.attempts),
            (TargetStatus::Fixing, Some(made.as_str()), 1),
            "the agent agentd had is the claim's"
        );
        assert_eq!(
            (tb.status, tb.attempts, tb.next_try_at),
            (TargetStatus::Affected, 0, None),
            "no agent: given back, and no try was charged"
        );
        assert_eq!(h.agents.lock().unwrap().len(), 1, "nothing new started");
    }

    #[tokio::test]
    async fn r7_an_authentication_failure_is_not_a_failed_fix() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), SIGN_IN.into());
        tick(&s, &p, &h).await;
        let lane0 = p.chain[0].probe_lane.clone().unwrap();
        assert!(
            h.lanes_out.lock().unwrap().contains(&lane0),
            "the lane the agent ran on is out"
        );
        assert_eq!(s.lane_state(&lane0).unwrap(), LaneState::Open);
        assert_eq!(s.task_failures(&f.id, Platform::Web).unwrap(), 0);
        assert!(h.told.lock().unwrap().is_empty(), "he is told nothing");
        assert_eq!(
            h.ran()[1],
            p.chain[1].label(),
            "the next try walks below the lane that failed"
        );
        assert_eq!(target(&s, &f.id, Platform::Web).attempts, 1);
    }

    #[tokio::test]
    async fn r8_one_fix_agent_at_a_time_for_a_target_whatever_the_store_last_wrote() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        // The row is vacated by some other means while agentd still holds ag0 at work.
        let read = target(&s, &f.id, Platform::Web).claim_ref();
        s.give_back_if(&f.id, Platform::Web, &read, "someone", false, "t")
            .unwrap()
            .unwrap();
        tick(&s, &p, &h).await;
        let live: Vec<String> = h.alive_now();
        assert_eq!(live.len(), 1, "one live agent for the target: {live:?}");
        assert!(h.stopped.lock().unwrap().contains(&"ag0".to_string()));
        assert!(h.archived.lock().unwrap().contains(&"ag0".to_string()));
    }

    #[tokio::test]
    async fn r9_a_target_written_by_somebody_else_stops_the_agent_that_held_it() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        // The triage agent's own move (policy.rs: `--status affected`): the claim is let go.
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::Affected),
                ..TargetUpdate::default()
            },
            "a-triage-agent",
        )
        .unwrap();
        tick(&s, &p, &h).await;
        let log = h.log.lock().unwrap().clone();
        assert_eq!(
            log,
            vec!["start ag0", "stop ag0", "start ag1"],
            "the old agent goes before the new one starts"
        );
        assert_eq!(h.alive_now(), vec!["ag1".to_string()]);
    }

    #[tokio::test]
    async fn r10_two_processes_on_one_file_share_the_caps() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let (a, b) = (Flaws::open(&db).unwrap(), Flaws::open(&db).unwrap());
        // One agentd, two processes: each has its own store handle and its own memory.
        let ha = Fake::default();
        let p = strict();
        for i in 0..4 {
            due(&a, &format!("flaw {i}"), Platform::Web, None);
        }
        tick(&a, &p, &ha).await;
        tick(&b, &p, &ha).await;
        assert_eq!(
            ha.agents.lock().unwrap().len(),
            1,
            "the lane's one agent is the other process's, and the claim's counts say so"
        );
        // The same, one level down: the claim itself refuses what the caller's snapshot allowed.
        let due_b = b.due().unwrap();
        let e = b
            .claim_capped(
                &due_b[0].0.id,
                Platform::Web,
                "/r",
                Some(p.chain[0].probe_lane.as_deref().unwrap()),
                &caps_of(&p),
                "b",
            )
            .unwrap_err();
        assert_eq!(e.code(), "busy", "{e}");
    }

    // The store-policy interleavings (disp-01-store-policy.md §5) not covered above or in the store.

    #[tokio::test]
    async fn t2_a_slow_create_whose_claim_was_let_go_is_stopped_not_attached() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        let pause = h.pause_at("start_agent");
        let (s, h, p) = (&s, &h, &p);
        let slow = async { dispatch_one(s, p, h, &f.id, Platform::Web, "t").await };
        let meanwhile = async {
            pause.reached.notified().await;
            // The claim is let go while the agent is being made.
            let read = target(s, &f.id, Platform::Web).claim_ref();
            s.give_back_if(&f.id, Platform::Web, &read, "swept", false, "sweep")
                .unwrap()
                .unwrap();
            pause.release.notify_one();
        };
        let (r, _) = tokio::join!(slow, meanwhile);
        assert!(r.is_err());
        let t = target(s, &f.id, Platform::Web);
        assert_eq!((t.status, t.agent), (TargetStatus::Affected, None));
        assert!(
            h.alive_now().is_empty(),
            "the agent made for a lost claim is put away: {:?}",
            h.log
        );
    }

    #[tokio::test]
    async fn t4_the_dead_agent_is_put_away_before_its_slot_is_used_again() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        due(&s, "one", Platform::Web, None);
        due(&s, "two", Platform::Web, None);
        tick(&s, &p, &h).await;
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), SIGN_IN.into());
        tick(&s, &p, &h).await;
        let log = h.log.lock().unwrap().clone();
        assert_eq!(log.first().map(String::as_str), Some("start ag0"));
        assert_eq!(
            log.get(1).map(String::as_str),
            Some("stop ag0"),
            "stopped (and archived) first: {log:?}"
        );
        assert!(h.archived.lock().unwrap().contains(&"ag0".to_string()));
    }

    #[tokio::test]
    async fn t5_a_create_that_keeps_failing_is_bounded_per_tick() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        for i in 0..4 {
            due(&s, &format!("flaw {i}"), Platform::Web, None);
        }
        *h.fail_agent.lock().unwrap() = true;
        for _ in 0..10 {
            tick(&s, &p, &h).await;
        }
        assert_eq!(
            h.tasks.lock().unwrap().len(),
            10,
            "one try a tick, whatever fails"
        );
        assert!(
            s.due().unwrap().iter().all(|(_, t)| t.attempts == 0),
            "an outage costs no try"
        );
    }

    #[tokio::test]
    async fn t6_one_server_fix_starts_one_agent_from_every_source() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = s
            .report(
                NewFlaw {
                    words: "server bug".into(),
                    severity: Some(Severity::High),
                    affected: vec![Platform::Web, Platform::Ios],
                    ..NewFlaw::default()
                },
                "o",
            )
            .unwrap()
            .flaw
            .flaw;
        for pl in [Platform::Web, Platform::Ios] {
            s.set_target(
                &f.id,
                pl,
                TargetUpdate {
                    layer: Some(Layer::Server),
                    ..TargetUpdate::default()
                },
                "t",
            )
            .unwrap();
        }
        // Both read the same `due`; the second claim must find the first.
        dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap();
        let e = dispatch_one(&s, &p, &h, &f.id, Platform::Ios, "t")
            .await
            .unwrap_err();
        assert_eq!(e.code, "not_claimable");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn t7_a_target_in_back_off_is_not_dispatched_again_in_the_same_budget() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        let clock = with_clock(&s);
        let x = due(&s, "x", Platform::Web, None);
        let y = due(&s, "y", Platform::Web, None);
        let dispatched_first = |t: &Target| t.status == TargetStatus::Fixing;
        tick(&s, &p, &h).await;
        let tx = target(&s, &x.id, Platform::Web);
        let (first, other) = if dispatched_first(&tx) {
            (&x, &y)
        } else {
            (&y, &x)
        };
        // The first one's agent worked and ended: back-off. The other is dispatched, not the first again.
        h.worked.lock().unwrap().insert("ag0".into());
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &first.id, Platform::Web).status,
            TargetStatus::Affected
        );
        assert_eq!(
            target(&s, &other.id, Platform::Web).status,
            TargetStatus::Fixing,
            "the budget went to the target not in back-off"
        );
        assert_eq!(h.agents.lock().unwrap().len(), 2);
    }

    // The agentd-side interleavings (disp-01-agentd-side.md §5): R4 and R5.

    #[tokio::test]
    async fn agentd_r4_every_create_carries_a_key_that_is_new_for_every_claim() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        // Its agent ends on a lane failure (the claim is given back, the attempt number is the
        // same again), and the next claim must not reuse the key and get the dead agent back.
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), SIGN_IN.into());
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        let keys = h.key_log.lock().unwrap().clone();
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert_eq!(keys[0], format!("flaw:{}:web:1", f.id));
        assert_eq!(keys[1], format!("flaw:{}:web:2", f.id));
        assert_eq!(h.agents.lock().unwrap().len(), 2, "two agents, two keys");
    }

    #[tokio::test]
    async fn agentd_r4_a_repeated_create_for_one_claim_is_one_agent() {
        // agentd dedups by the key: asking again for the same claim answers the same agent.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        let t = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap();
        let key = format!("flaw:{}:web:{}", f.id, t.claim_seq);
        let again = h
            .start_agent(
                &p,
                &p.chain[0],
                "/w",
                "<owner_words>",
                "t",
                json!({}),
                Some(&key),
            )
            .await
            .unwrap();
        assert_eq!(Some(again.as_str()), t.agent.as_deref());
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn agentd_r5_an_error_while_agentd_is_silent_keeps_the_claim_for_the_sweep() {
        // The create timed out and the lookup by labels cannot be answered: nobody knows whether
        // an agent exists, so the claim is not given back and no second agent starts.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        *h.late_error.lock().unwrap() = Some("Lucina Agents (agentd) timed out".into());
        *h.lookup_down.lock().unwrap() = true;
        let e = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap_err();
        assert_eq!(e.code, "dispatch_failed");
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!((t.status, t.agent.clone()), (TargetStatus::Fixing, None));
        assert!(s.due().unwrap().is_empty(), "it cannot be claimed again");
        // The sweep, once agentd answers: the agent that exists is the claim's.
        *h.lookup_down.lock().unwrap() = false;
        *h.late_error.lock().unwrap() = None;
        clock.fetch_add(6, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!(
            (t.status, t.agent.as_deref(), t.attempts),
            (TargetStatus::Fixing, Some("ag0"), 1)
        );
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_provider_tripped_or_capped_create_is_a_lane_failure_without_an_attempt() {
        for answer in [
            "agentd 503: Provider claude is unavailable: its last turn failed on authentication",
            "agentd 429: too many agents (AGENT_LIMIT)",
        ] {
            let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
            let f = due(&s, "emoji", Platform::Web, None);
            *h.create_error.lock().unwrap() = Some(answer.into());
            let e = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
                .await
                .unwrap_err();
            assert_eq!(e.code, "dispatch_failed", "{answer}");
            let t = target(&s, &f.id, Platform::Web);
            assert_eq!(
                (t.status, t.attempts, t.agent),
                (TargetStatus::Affected, 0, None),
                "{answer}"
            );
            assert_eq!(
                s.lane_state("claude-sonnet").unwrap(),
                LaneState::Open,
                "{answer}"
            );
            assert_eq!(h.ops.lock().unwrap().len(), 1, "{answer}");
            assert_eq!(s.task_failures(&f.id, Platform::Web).unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn a_lane_failure_overrides_a_ready_probe_and_lets_exactly_one_trial_agent_through() {
        // `LANE_READY_FOR` kept a ready answer for 30 minutes across a failure, and every pair
        // refilled. Now the first failure opens the breaker whatever the probe says; a passing
        // probe (it did say ready at 00:43:12) lets one agent through, not a batch.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        *h.ignore_step_out.lock().unwrap() = true;
        for i in 0..3 {
            due(&s, &format!("flaw {i}"), Platform::Web, None);
        }
        tick(&s, &p, &h).await;
        h.status
            .lock()
            .unwrap()
            .insert("ag0".into(), "error".into());
        h.errors
            .lock()
            .unwrap()
            .insert("ag0".into(), SIGN_IN.into());
        tick(&s, &p, &h).await;
        assert_eq!(h.ops.lock().unwrap().len(), 1, "it did open");
        // LA-C035 condition 1: a breaker opened in a tick is not healed in that same tick.
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        assert_eq!(h.agents.lock().unwrap().len(), 1, "no refill in that tick");
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::HalfOpen);
        assert_eq!(
            h.agents.lock().unwrap().len(),
            2,
            "one trial agent, no refill"
        );
    }

    #[tokio::test]
    async fn a_finished_idle_agent_is_put_away_and_stops_counting_against_the_caps() {
        // After a merge the agent sits idle. Alive means a process that holds a slot, so a
        // finished one must not keep the lane's one slot for ever.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        let a = due(&s, "one", Platform::Web, None);
        let b = due(&s, "two", Platform::Web, None);
        tick(&s, &p, &h).await;
        let first = if target(&s, &a.id, Platform::Web).status == TargetStatus::Fixing {
            &a
        } else {
            &b
        };
        let ws = target(&s, &first.id, Platform::Web).workspace.unwrap();
        h.merged.lock().unwrap().insert(ws, "abcdef1234".into());
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &first.id, Platform::Web).status,
            TargetStatus::Fixed
        );
        assert!(h.archived.lock().unwrap().contains(&"ag0".to_string()));
        assert_eq!(
            h.agents.lock().unwrap().len(),
            2,
            "the lane's slot was free again in the same tick"
        );
    }

    // Caps from two sources, in forced orders.

    fn wide_but(f: impl FnOnce(&mut Policy)) -> Policy {
        let mut p = Policy {
            max_running_per_repo: 100,
            max_alive: 100,
            max_per_lane: 100,
            max_new_per_tick: 100,
            ..strict()
        };
        f(&mut p);
        p
    }

    #[tokio::test]
    async fn at_most_three_agents_alive_in_all_from_the_tick_and_dispatch_one_in_either_order() {
        for tick_first in [true, false] {
            let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
            let p = wide_but(|p| p.max_alive = 3);
            let fs: Vec<Flaw> = (0..6)
                .map(|i| due(&s, &format!("flaw {i}"), Platform::Web, None))
                .collect();
            if tick_first {
                tick(&s, &p, &h).await;
                tick(&s, &p, &h).await;
            }
            for f in &fs {
                let _ = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t").await;
            }
            tick(&s, &p, &h).await;
            assert_eq!(
                h.agents.lock().unwrap().len(),
                3,
                "tick first: {tick_first}"
            );
            let extra = fs
                .iter()
                .find(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Affected)
                .unwrap();
            let e = dispatch_one(&s, &p, &h, &extra.id, Platform::Web, "t")
                .await
                .unwrap_err();
            assert_eq!(
                (e.status.as_u16(), e.code.as_str()),
                (409, "busy"),
                "tick first: {tick_first}"
            );
            assert!(
                e.message.contains("3 agents already alive"),
                "{}",
                e.message
            );
        }
    }

    #[tokio::test]
    async fn at_most_one_agent_alive_per_lane_from_the_tick_and_dispatch_one_in_either_order() {
        for tick_first in [true, false] {
            let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
            let p = wide_but(|p| p.max_per_lane = 1);
            let fs: Vec<Flaw> = (0..3)
                .map(|i| due(&s, &format!("flaw {i}"), Platform::Web, None))
                .collect();
            if tick_first {
                tick(&s, &p, &h).await;
            } else {
                dispatch_one(&s, &p, &h, &fs[0].id, Platform::Web, "t")
                    .await
                    .unwrap();
            }
            // The second source meets the first one's agent on the same lane.
            if tick_first {
                let extra = fs
                    .iter()
                    .find(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Affected)
                    .unwrap();
                let e = dispatch_one(&s, &p, &h, &extra.id, Platform::Web, "t")
                    .await
                    .unwrap_err();
                assert_eq!((e.status.as_u16(), e.code.as_str()), (409, "busy"));
            } else {
                tick(&s, &p, &h).await;
            }
            tick(&s, &p, &h).await;
            assert_eq!(
                h.agents.lock().unwrap().len(),
                1,
                "tick first: {tick_first}"
            );
        }
    }

    #[tokio::test]
    async fn at_most_one_new_dispatch_per_tick_and_dispatch_one_is_not_a_tick() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = wide_but(|p| p.max_new_per_tick = 1);
        let fs: Vec<Flaw> = (0..4)
            .map(|i| due(&s, &format!("flaw {i}"), Platform::Web, None))
            .collect();
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1, "one in a tick");
        // An explicit start is the lead's own: it is not a tick's, and the caps still hold.
        let other = fs
            .iter()
            .find(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Affected)
            .unwrap();
        dispatch_one(&s, &p, &h, &other.id, Platform::Web, "t")
            .await
            .unwrap();
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        tick(&s, &p, &h).await;
        assert_eq!(
            h.agents.lock().unwrap().len(),
            3,
            "one more in the next tick"
        );
    }

    #[tokio::test]
    async fn agentd_that_cannot_be_asked_means_no_dispatch_and_it_is_said_once() {
        let (buf, _log) = capture();
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        *h.list_down.lock().unwrap() = true;
        for _ in 0..3 {
            tick(&s, &p, &h).await;
        }
        assert!(h.tasks.lock().unwrap().is_empty(), "fail closed");
        assert_eq!(
            dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
                .await
                .unwrap_err()
                .code,
            "agentd_unavailable"
        );
        let said = buf.text();
        assert_eq!(
            said.matches("can't be asked which agents are alive")
                .count(),
            1,
            "once per change of state, not per tick: {said}"
        );
        *h.list_down.lock().unwrap() = false;
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        assert!(buf.text().contains("agentd answers again"));
    }

    #[tokio::test]
    async fn every_dispatch_writes_one_line_with_its_caller_and_a_refusal_says_why() {
        let (buf, _log) = capture();
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), strict());
        let fs: Vec<Flaw> = (0..3)
            .map(|i| due(&s, &format!("flaw {i}"), Platform::Web, None))
            .collect();
        tick(&s, &p, &h).await;
        let said = buf.text();
        let loop_line = said
            .lines()
            .filter(|l| l.contains("flaws: dispatched"))
            .collect::<Vec<_>>();
        assert_eq!(loop_line.len(), 1, "{said}");
        let first = fs
            .iter()
            .find(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Fixing)
            .unwrap();
        assert!(
            loop_line[0].ends_with(&format!(
                "flaws: dispatched flaw={} platform=web caller=loop attempt=1 lane=zen-deepseek-flash agent=ag0",
                first.id
            )),
            "{}",
            loop_line[0]
        );
        // POST and MCP name themselves; the lane is full, so these two are refused, with a reason.
        let other: Vec<&Flaw> = fs.iter().filter(|f| f.id != first.id).collect();
        let e = dispatch_one_as(&s, &p, &h, &other[0].id, Platform::Web, "t", "post")
            .await
            .unwrap_err();
        assert_eq!(e.status.as_u16(), 409);
        let _ = dispatch_one_as(&s, &p, &h, &other[1].id, Platform::Web, "t", "mcp").await;
        let said = buf.text();
        assert!(
            said.contains(&format!(
                "flaws: dispatch refused flaw={} platform=web caller=post reason=cap: 1 agent already alive on lane zen-deepseek-flash",
                other[0].id
            )),
            "{said}"
        );
        assert!(said.contains("caller=mcp reason=cap"), "{said}");
        // And a start through each source names it.
        let (s2, h2) = (Flaws::in_memory().unwrap(), Fake::default());
        let g = due(&s2, "other", Platform::Web, None);
        dispatch_one_as(&s2, &p, &h2, &g.id, Platform::Web, "t", "mcp")
            .await
            .unwrap();
        assert!(buf.text().contains(&format!(
            "flaws: dispatched flaw={} platform=web caller=mcp attempt=1 lane=zen-deepseek-flash agent=ag0",
            g.id
        )));
    }

    // Failure classes.

    #[tokio::test]
    async fn the_failure_classes_by_words_and_by_proof_of_work() {
        let h = Fake::default();
        h.worked.lock().unwrap().insert("worker".into());
        // The words say it is the lane's, whoever worked.
        for words in [
            SIGN_IN,
            "You've hit your weekly limit · resets Nov 4, 12pm (UTC)",
            "Free usage limit reached (code: quota)",
            "Provider claude is unavailable",
            "agentd 429: too many agents",
        ] {
            for agent in ["worker", "idler"] {
                assert!(
                    matches!(classify(&h, agent, None, Some(words)).await, Ended::Lane(_)),
                    "{words} / {agent}"
                );
            }
        }
        // Text only: the task's only when the agent provably worked; unknown is the lane's.
        assert!(matches!(
            classify(&h, "worker", None, Some("tests do not pass")).await,
            Ended::Task(_)
        ));
        assert!(matches!(
            classify(&h, "worker", None, None).await,
            Ended::Task(_)
        ));
        assert!(matches!(
            classify(&h, "idler", None, Some("tests do not pass")).await,
            Ended::Lane(_)
        ));
        assert!(matches!(
            classify(&h, "idler", None, None).await,
            Ended::Lane(_)
        ));
    }

    // The restore command.

    struct Errors(HashMap<String, String>);

    #[async_trait::async_trait]
    impl ErrorSource for Errors {
        async fn agent_error(&self, id: &str) -> Option<String> {
            self.0.get(id).cloned()
        }
    }

    /// A target blocked the way the old dispatcher blocked it: two counted releases, each with an
    /// agent started.
    fn blocked_the_old_way(s: &Flaws, words: &str, agents: [&str; 2]) -> Flaw {
        let f = due(s, words, Platform::Web, None);
        for a in agents {
            s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
            s.attach(&f.id, Platform::Web, "w", a, &policy().chain[1], "d")
                .unwrap();
            s.release(
                &f.id,
                Platform::Web,
                "the agent ended without a merged commit",
                2,
                "d",
            )
            .unwrap();
        }
        assert_eq!(
            target(s, &f.id, Platform::Web).status,
            TargetStatus::Blocked
        );
        f
    }

    #[tokio::test]
    async fn restore_lists_lane_failures_without_changing_anything_and_apply_resets_exactly_those()
    {
        let s = Flaws::in_memory().unwrap();
        let victim = blocked_the_old_way(&s, "victim", ["v0", "v1"]);
        let real = blocked_the_old_way(&s, "real failure", ["r0", "r1"]);
        let unknown = blocked_the_old_way(&s, "no text", ["u0", "u1"]);
        let mixed = blocked_the_old_way(&s, "one of each", ["m0", "m1"]);
        let src = Errors(
            [
                ("v0", SIGN_IN),
                ("v1", SIGN_IN),
                ("r0", "tests do not pass"),
                ("r1", "tests do not pass"),
                ("m0", SIGN_IN),
                ("m1", "tests do not pass"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect(),
        );
        let since = "2000-01-01T00:00:00Z";
        // Dry run: the list, and no change.
        let report = restore_lane_failures(&s, since, false, &src).await.unwrap();
        let found = report.listed.clone();
        assert_eq!(
            found,
            vec![Restorable {
                flaw_id: victim.id.clone(),
                platform: Platform::Web,
                attempts: 2,
                last_error: SIGN_IN.into(),
            }]
        );
        // LA-C035 F4: this test used to pin the silence about the others. Now each target that
        // cannot be judged is said, with its flaw, platform and why.
        let reasons: HashMap<String, String> = report
            .skipped
            .iter()
            .map(|k| (k.flaw_id.clone(), k.reason.clone()))
            .collect();
        assert_eq!(report.skipped.len(), 3, "{:?}", report.skipped);
        assert!(
            reasons[&real.id].contains("not a lane failure")
                && reasons[&real.id].contains("tests do not pass"),
            "{reasons:?}"
        );
        assert!(
            reasons[&unknown.id].contains("agentd no longer holds agent u0"),
            "{reasons:?}"
        );
        assert!(
            reasons[&mixed.id].contains("agent m1 ended on an error that is not a lane failure"),
            "{reasons:?}"
        );
        assert!(report.skipped.iter().all(|k| k.platform == Platform::Web));
        for f in [&victim, &real, &unknown, &mixed] {
            assert_eq!(
                target(&s, &f.id, Platform::Web).status,
                TargetStatus::Blocked,
                "a dry run changes nothing"
            );
        }
        // Apply: exactly the listed one.
        let applied = restore_lane_failures(&s, since, true, &src).await.unwrap();
        assert_eq!(applied.listed, found);
        assert_eq!(applied.skipped, report.skipped);
        let t = target(&s, &victim.id, Platform::Web);
        assert_eq!(
            (t.status, t.attempts, t.next_try_at),
            (TargetStatus::Affected, 0, None)
        );
        for f in [&real, &unknown, &mixed] {
            assert_eq!(
                target(&s, &f.id, Platform::Web).status,
                TargetStatus::Blocked,
                "left for a lead: {}",
                f.words
            );
        }
        // A time that is not RFC 3339 is refused.
        assert!(restore_lane_failures(&s, "yesterday", false, &src)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn restore_says_why_a_target_with_nothing_to_judge_is_left() {
        // A dry run of "0 listed" must not read as "no victims" (LA-C035 F4).
        let s = Flaws::in_memory().unwrap();
        let f = due(&s, "never started", Platform::Web, None);
        // Blocked by hand, with no agent ever started and no failure recorded.
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        s.release(&f.id, Platform::Web, "x", 1, "d").unwrap();
        let src = Errors(HashMap::new());
        let r = restore_lane_failures(&s, "2000-01-01T00:00:00Z", false, &src)
            .await
            .unwrap();
        assert!(r.listed.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert!(
            r.skipped[0].reason.contains("no `started` event"),
            "{:?}",
            r.skipped
        );
        // A task failure on record is left to a lead, and says so.
        let g = due(&s, "worked", Platform::Web, None);
        s.claim(&g.id, Platform::Web, "/r", "d").unwrap();
        s.attach(&g.id, Platform::Web, "w", "g0", &policy().chain[0], "d")
            .unwrap();
        let read = target(&s, &g.id, Platform::Web).claim_ref();
        s.fail_task_if(&g.id, Platform::Web, &read, "tests fail", 1, "d")
            .unwrap();
        let r = restore_lane_failures(&s, "2000-01-01T00:00:00Z", false, &src)
            .await
            .unwrap();
        assert!(
            r.skipped
                .iter()
                .any(|k| k.flaw_id == g.id && k.reason.contains("counted task failure")),
            "{:?}",
            r.skipped
        );
    }

    #[tokio::test]
    async fn restore_falls_back_to_the_stored_error_when_agentd_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let s = Flaws::open(&db).unwrap();
        let f = blocked_the_old_way(&s, "stored", ["s0", "s1"]);
        // agentd holds neither agent; the target kept the text of its last end.
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE flaw_targets SET last_error = ?1 WHERE flaw_id = ?2",
                rusqlite::params![SIGN_IN, f.id],
            )
            .unwrap();
        let r = restore_lane_failures(&s, "2000-01-01T00:00:00Z", false, &Errors(HashMap::new()))
            .await
            .unwrap();
        assert_eq!(r.listed.len(), 1, "{:?}", r.skipped);
        assert_eq!(r.listed[0].last_error, SIGN_IN);
    }

    #[tokio::test]
    async fn a_trial_agent_that_goes_straight_to_idle_closes_the_half_open_lane() {
        // LA-C035 F2: only `running` closed it, and `heal_lanes` probes only open lanes, so a trial
        // that ended its turn between two ticks left the lane half-open for good.
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        let clock = with_clock(&s);
        due(&s, "emoji", Platform::Web, None);
        s.lane_open("claude-sonnet", SIGN_IN).unwrap();
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::HalfOpen);
        assert_eq!(h.agents.lock().unwrap().len(), 1, "the one trial agent");
        // Its first turn worked and it went idle before the next tick looked.
        h.worked.lock().unwrap().insert("ag0".into());
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        clock.fetch_add(1, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Closed);
        // An idle trial that did no work and said nothing is not proof: the lane opens again.
        s.lane_open("claude-sonnet", "again").unwrap();
        s.lane_half_open("claude-sonnet").unwrap();
        clock.fetch_add(10, Ordering::SeqCst);
        tick(&s, &p, &h).await;
        let n = h.agents.lock().unwrap().len();
        h.status
            .lock()
            .unwrap()
            .insert(format!("ag{}", n - 1), "idle".into());
        // (its probe does not pass now, so the same tick does not half-open it again)
        *h.out.lock().unwrap() = true;
        tick(&s, &p, &h).await;
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
    }

    // F1: the lock cannot be held for long, and a request or the minute loop never waits on it.

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Deadlines short enough for a test to wait them out.
    fn quick() -> Limits {
        Limits {
            lock_wait: ms(50),
            list: ms(40),
            away: ms(40),
            create: ms(40),
            lookup: ms(40),
            record: ms(40),
            probe_lane: ms(40),
            probe_budget: ms(60),
        }
    }

    #[test]
    fn the_worst_case_hold_is_the_sum_of_the_deadlines_145_seconds() {
        let l = Limits::DEFAULT;
        assert_eq!(l.lock_wait, Duration::from_secs(5));
        assert_eq!(
            l.max_hold(),
            // list + sweep/orphans (3 x stop+archive) + probes + 2 creates
            //   + (lookup + 3 extras x stop+archive + ops record)
            Duration::from_secs(5 + 30 + 15 + 2 * 25 + 5 + 40)
        );
        assert_eq!(MAX_HOLD, Duration::from_secs(145));
        assert_eq!(MAX_HOLD, l.max_hold());
    }

    #[tokio::test]
    async fn a_request_that_cannot_get_the_lock_in_time_is_told_busy_and_starts_nothing() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        // The probe of the one in the lock may take long; the one waiting gives up after 50 ms.
        *h.limits.lock().unwrap() = Some(Limits {
            probe_lane: Duration::from_secs(5),
            probe_budget: Duration::from_secs(5),
            ..quick()
        });
        let a = due(&s, "one", Platform::Web, None);
        let b = due(&s, "two", Platform::Web, None);
        let pause = h.pause_at("probe");
        let (s, h, p) = (&s, &h, &p);
        let slow = async { dispatch_one(s, p, h, &a.id, Platform::Web, "t").await };
        let rival = async {
            pause.reached.notified().await;
            let t0 = std::time::Instant::now();
            let r = dispatch_one(s, p, h, &b.id, Platform::Web, "t").await;
            let took = t0.elapsed();
            pause.release.notify_one();
            (r, took)
        };
        let (first, (second, took)) = tokio::join!(slow, rival);
        first.unwrap();
        let e = second.unwrap_err();
        assert_eq!(
            (e.status.as_u16(), e.code.as_str()),
            (409, "dispatcher_busy")
        );
        assert!(took < Duration::from_secs(2), "answered in {took:?}");
        let n = e.extra.as_ref().unwrap()["retry_after_s"].as_u64().unwrap();
        assert!((1..=145).contains(&n), "{n}");
        assert!(
            e.message.contains(&format!("retry in {n} s")),
            "{}",
            e.message
        );
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "nothing started for the one told busy"
        );
        assert_eq!(
            target(s, &b.id, Platform::Web).status,
            TargetStatus::Affected
        );
    }

    #[tokio::test]
    async fn the_minute_loop_never_waits_for_the_lock_it_skips_the_tick_and_says_so_once() {
        let (buf, _log) = capture();
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        *h.limits.lock().unwrap() = Some(Limits {
            probe_lane: Duration::from_secs(5),
            probe_budget: Duration::from_secs(5),
            ..quick()
        });
        let a = due(&s, "one", Platform::Web, None);
        due(&s, "two", Platform::Web, None);
        let pause = h.pause_at("probe");
        let (s, h, p) = (&s, &h, &p);
        let slow = async { dispatch_one(s, p, h, &a.id, Platform::Web, "t").await };
        let rival = async {
            pause.reached.notified().await;
            for _ in 0..3 {
                // Were it to wait, the lock would never come: the holder waits for this very test.
                let r = tokio::time::timeout(Duration::from_secs(2), tick(s, p, h)).await;
                assert!(r.is_ok(), "the tick waited for the lock");
            }
            pause.release.notify_one();
        };
        let (first, _) = tokio::join!(slow, rival);
        first.unwrap();
        let said = buf.text();
        assert_eq!(
            said.matches("dispatcher busy, tick skipped").count(),
            1,
            "once per change of state, not per tick: {said}"
        );
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "no tick queued behind it"
        );
        tick(s, p, h).await;
        assert!(buf.text().contains("dispatcher is free again"));
    }

    #[tokio::test]
    async fn every_call_made_under_the_lock_is_cut_at_its_limit() {
        // A fake that never answers one call: the dispatch is cut at that call's limit and
        // answers, whichever call it is, instead of holding the lock for ever.
        for point in [
            "alive",
            "start_task",
            "start_agent",
            "lookup",
            "stop",
            "archive",
            "probe",
        ] {
            let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
            *h.limits.lock().unwrap() = Some(quick());
            let f = due(&s, "emoji", Platform::Web, None);
            match point {
                // The create errors, then the lookup by labels never answers.
                "lookup" => *h.late_error.lock().unwrap() = Some("timed out".into()),
                // An agent nobody tracks already works on the target.
                "stop" | "archive" => {
                    let labels = json!({"from": "lucina", "flaw": f.id, "platform": "web"});
                    h.start_agent(&p, &p.chain[0], "/w", "<owner_words>", "t", labels, None)
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let _never_released = h.pause_at(point);
            let t0 = std::time::Instant::now();
            let r = tokio::time::timeout(
                Duration::from_secs(3),
                dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t"),
            )
            .await;
            assert!(r.is_ok(), "{point}: the call under the lock was not cut");
            assert!(t0.elapsed() < Duration::from_secs(2), "{point}");
            match (point, r.unwrap()) {
                ("alive", Err(e)) => assert_eq!(e.code, "agentd_unavailable"),
                ("probe", Ok(t)) => {
                    // The lane that did not answer is not ready: the walk goes below it.
                    assert_eq!(t.status, TargetStatus::Fixing);
                    assert_eq!(h.ran(), vec![p.chain[1].label()]);
                }
                ("stop", Err(e)) => assert_eq!(e.code, "orphan_not_stopped"),
                ("start_task" | "start_agent" | "lookup", Err(e)) => {
                    assert_eq!(e.code, "dispatch_failed", "{point}")
                }
                // An archive that does not answer does not stop the start.
                ("archive", Ok(t)) => assert_eq!(t.status, TargetStatus::Fixing),
                (pt, other) => panic!("{pt}: unexpected {other:?}"),
            }
        }
    }

    // LA-C035, the coordinator's conditions on the review.

    #[tokio::test]
    async fn a_flaw_whose_agents_end_with_no_work_on_a_healthy_lane_is_bounded_in_30_ticks() {
        // Condition 1. `lane probe` always says ok (as it did in the incident), the agents end
        // their turn at once with no tool call and no commit, and three flaws are due. Before: one
        // agent every tick, never blocked, an incident per cycle.
        const AGENTS_AT_MOST: usize = 6;
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        let clock = with_clock(&s);
        *h.ignore_step_out.lock().unwrap() = true;
        *h.idle_dies.lock().unwrap() = true;
        let fs: Vec<Flaw> = (0..3)
            .map(|i| due(&s, &format!("poisoned {i}"), Platform::Web, None))
            .collect();
        for _ in 0..30 {
            clock.fetch_add(1, Ordering::SeqCst);
            tick(&s, &p, &h).await;
        }
        let agents = h.agents.lock().unwrap().len();
        let ops = h.ops.lock().unwrap().len();
        assert!(
            agents <= AGENTS_AT_MOST,
            "{agents} agents in 30 ticks, at most {AGENTS_AT_MOST}"
        );
        assert!(
            ops <= agents,
            "one incident per opening: {ops} for {agents}"
        );
        assert!(
            fs.iter()
                .any(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Blocked),
            "the flaw that kept failing needs a lead"
        );
        assert!(
            fs.iter()
                .all(|f| target(&s, &f.id, Platform::Web).attempts == 0),
            "and none of it cost an attempt"
        );
        assert!(h.alive_now().is_empty(), "nothing left alive");
    }

    #[tokio::test]
    async fn a_triage_agent_dying_at_sign_in_opens_the_breaker_and_leaves_the_flaw_triage_due() {
        // Condition 2: triage used `is_limit` alone, so a sign-in failure finished the triage for
        // good and the next report got an agent on the next tick.
        let p = Policy {
            max_running_per_repo: 1,
            ..triaging()
        };
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let f = report(&s, "report one");
        report(&s, "report two");
        *h.dies_with.lock().unwrap() = Some(SIGN_IN.into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 1);
        tick(&s, &p, &h).await;
        let lane0 = p.chain[0].probe_lane.clone().unwrap();
        assert_eq!(s.lane_state(&lane0).unwrap(), LaneState::Open);
        assert_eq!(h.ops.lock().unwrap().len(), 1);
        assert!(
            s.get(&f.id).unwrap().flaw.triaged_at.is_none(),
            "not finished: it is triage-due again"
        );
        assert!(s.triage_due().unwrap().iter().any(|x| x.id == f.id));
        assert_eq!(
            h.agents.lock().unwrap().len(),
            1,
            "and no agent for the next report on the lane that just failed"
        );
        assert!(h.archived.lock().unwrap().contains(&"ag0".to_string()));
    }

    #[tokio::test]
    async fn only_the_trial_agent_closes_a_half_open_lane() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let a = due(&s, "started before the lane failed", Platform::Web, None);
        let lane0 = p.chain[0].probe_lane.clone().unwrap();
        // An agent on the lane, claimed while it was closed, and running.
        let t = s
            .claim_capped(
                &a.id,
                Platform::Web,
                "/r",
                Some(&lane0),
                &caps_of(&policy()),
                "t",
            )
            .unwrap();
        let labels = json!({"from": "lucina", "flaw": a.id, "platform": "web", "flaw_lane": lane0});
        let id = h
            .start_agent(&p, &p.chain[0], "/w", "<owner_words>", "t", labels, None)
            .await
            .unwrap();
        s.attach(&a.id, Platform::Web, "w", &id, &p.chain[0], "t")
            .unwrap();
        assert_ne!(t.claim_seq, 0);
        // The lane failed (another agent) and a probe passed: half-open, and this one is not its trial.
        s.lane_open(&lane0, "sign-in").unwrap();
        s.lane_half_open(&lane0).unwrap();
        tick(&s, &p, &h).await;
        assert_eq!(
            s.lane_state(&lane0).unwrap(),
            LaneState::HalfOpen,
            "a running agent that is not the trial says nothing about the lane"
        );
    }

    #[test]
    fn an_answer_without_a_list_of_agents_is_not_zero_agents() {
        assert!(agent_list(&json!({})).is_err());
        assert!(agent_list(&json!({"agents": null})).is_err());
        assert!(agent_list(&json!({"error": {"message": "x"}})).is_err());
        assert_eq!(agent_list(&json!({"agents": []})).unwrap().len(), 0);
    }

    #[test]
    fn the_ops_key_of_an_opening_is_a_count_never_a_time() {
        assert_eq!(
            lane_ops_key("claude-sonnet", 3),
            "flaw-lane:claude-sonnet:open-3"
        );
        assert_ne!(
            lane_ops_key("claude-sonnet", 3),
            lane_ops_key("claude-sonnet", 4)
        );
    }

    #[tokio::test]
    async fn restore_continues_past_a_target_it_cannot_reset_and_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flaws.db");
        let s = Flaws::open(&db).unwrap();
        let v = blocked_the_old_way(&s, "resettable", ["v0", "v1"]);
        let w = blocked_the_old_way(&s, "stuck", ["w0", "w1"]);
        // The second cannot be reset: the database refuses.
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER stuck BEFORE UPDATE ON flaw_targets WHEN NEW.flaw_id = '{}' AND NEW.status = 'affected'
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
                w.id
            ))
            .unwrap();
        let src = Errors(
            ["v0", "v1", "w0", "w1"]
                .into_iter()
                .map(|a| (a.to_owned(), SIGN_IN.to_owned()))
                .collect(),
        );
        let r = restore_lane_failures(&s, "2000-01-01T00:00:00Z", true, &src)
            .await
            .unwrap();
        assert_eq!(r.listed.len(), 2);
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert_eq!(r.failed[0].flaw_id, w.id);
        assert!(
            r.failed[0].reason.contains("reset failed"),
            "{:?}",
            r.failed
        );
        assert_eq!(
            target(&s, &v.id, Platform::Web).status,
            TargetStatus::Affected
        );
        assert_eq!(
            target(&s, &w.id, Platform::Web).status,
            TargetStatus::Blocked
        );
    }

    // The coordinator's second round.

    #[tokio::test]
    async fn a_triage_create_that_never_answers_or_is_refused_is_cut_and_starts_at_most_one() {
        // A: the loop over due flaws ran under the lock with no deadline on the calls. Five due
        // flaws, an agentd that never answers a create (or answers 503): the tick ends inside its
        // budget with at most one triage start tried, and the lane's breaker opens.
        for mode in ["hangs", "503"] {
            let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
            let p = triaging();
            *h.limits.lock().unwrap() = Some(quick());
            match mode {
                "hangs" => *h.hang_creates.lock().unwrap() = true,
                _ => {
                    *h.create_error.lock().unwrap() =
                        Some("agentd 503: Provider claude is unavailable".into())
                }
            }
            for i in 0..5 {
                report(&s, &format!("report {i}"));
            }
            let t0 = std::time::Instant::now();
            let r = tokio::time::timeout(Duration::from_secs(3), tick(&s, &p, &h)).await;
            assert!(r.is_ok(), "{mode}: the tick was not cut");
            assert!(t0.elapsed() < Duration::from_secs(2), "{mode}");
            assert!(
                h.tasks.lock().unwrap().len() <= 1,
                "{mode}: at most one triage start: {:?}",
                h.tasks
            );
            assert!(h.agents.lock().unwrap().is_empty(), "{mode}");
            let lane0 = p.chain[0].probe_lane.clone().unwrap();
            assert_eq!(s.lane_state(&lane0).unwrap(), LaneState::Open, "{mode}");
            assert_eq!(s.triage_due().unwrap().len(), 5, "{mode}: all still due");
        }
    }

    #[tokio::test]
    async fn a_create_that_never_answers_is_a_lane_failure_and_bounded_over_30_ticks() {
        // B: a create that hit its deadline with nothing found by the lookup was given back at once
        // with no breaker, so every tick tried again (and an agent agentd finished late was
        // stopped, then another started, without end).
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        let clock = with_clock(&s);
        *h.limits.lock().unwrap() = Some(quick());
        *h.hang_creates.lock().unwrap() = true;
        *h.ignore_step_out.lock().unwrap() = true;
        let fs: Vec<Flaw> = (0..3)
            .map(|i| due(&s, &format!("flaw {i}"), Platform::Web, None))
            .collect();
        for _ in 0..30 {
            clock.fetch_add(1, Ordering::SeqCst);
            let r = tokio::time::timeout(Duration::from_secs(3), tick(&s, &p, &h)).await;
            assert!(r.is_ok(), "a tick was not cut");
        }
        let tries = h.tasks.lock().unwrap().len();
        assert!(tries <= 6, "{tries} creates in 30 ticks, at most 6");
        assert!(
            s.lane_state("claude-sonnet").unwrap() != LaneState::Closed
                || fs
                    .iter()
                    .any(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Blocked),
            "the breaker is open or a target needs a lead"
        );
        assert!(
            fs.iter()
                .all(|f| target(&s, &f.id, Platform::Web).attempts == 0),
            "none of it cost an attempt"
        );
    }

    #[tokio::test]
    async fn a_request_does_not_heal_a_lane_in_the_minute_it_opened() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), one_lane());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        s.lane_open("claude-sonnet", SIGN_IN).unwrap();
        // The probe says ok (it did at 00:43:12), but the lane opened a moment ago.
        let e = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap_err();
        assert_eq!(e.code, "lane_out");
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::Open);
        clock.fetch_add(2, Ordering::SeqCst);
        let t = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t")
            .await
            .unwrap();
        assert_eq!(t.status, TargetStatus::Fixing);
        assert_eq!(s.lane_state("claude-sonnet").unwrap(), LaneState::HalfOpen);
    }

    #[tokio::test]
    async fn a_flaw_blocked_by_lane_failures_is_told_why_not_after_0_tries() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let clock = with_clock(&s);
        let f = due(&s, "emoji", Platform::Web, None);
        for _ in 0..3 {
            let t = s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
            s.give_back_if(&f.id, Platform::Web, &t.claim_ref(), SIGN_IN, true, "d")
                .unwrap();
            clock.fetch_add(6, Ordering::SeqCst);
        }
        assert_eq!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Blocked
        );
        tick(&s, &p, &h).await;
        let told = h.told.lock().unwrap().clone();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            !told[0].contains("after 0 tries")
                && told[0].contains("lane")
                && told[0].contains("needs a lead"),
            "{told:?}"
        );
    }

    #[tokio::test]
    async fn a_triage_agent_ending_on_an_unavailable_provider_opens_the_breaker() {
        let p = Policy {
            max_running_per_repo: 1,
            ..triaging()
        };
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let f = report(&s, "report one");
        *h.dies_with.lock().unwrap() = Some("Provider claude is unavailable".into());
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        let lane0 = p.chain[0].probe_lane.clone().unwrap();
        assert_eq!(s.lane_state(&lane0).unwrap(), LaneState::Open);
        assert!(s.get(&f.id).unwrap().flaw.triaged_at.is_none());
        assert!(s.triage_due().unwrap().iter().any(|x| x.id == f.id));
        assert_eq!(h.ops.lock().unwrap().len(), 1);
    }

    // C and D: the triage switch, and a triage count that never stops the server.

    #[test]
    fn a_bad_triage_count_is_one_warning_and_the_servers_settings_build() {
        for bad in ["0", "abc"] {
            let s = FlawSettings::from_lookup(
                |k| (k == "LUCINA_FLAWS_TRIAGE_STEPS").then(|| bad.to_owned()),
                Path::new("/data"),
            )
            .expect("the server's settings build");
            assert_eq!(s.policy.triage_steps, 1, "{bad}");
            assert_eq!(s.policy.notes.len(), 1, "{bad}");
        }
        // A bad chain still stops it.
        assert!(FlawSettings::from_lookup(
            |k| (k == "LUCINA_FLAWS_CHAIN").then(|| "codex".to_owned()),
            Path::new("/data"),
        )
        .is_err());
    }

    #[tokio::test]
    async fn auto_triage_off_starts_no_triage_agent_on_is_as_today_and_dispatch_is_untouched() {
        // LUCINA_FLAWS_AUTO_TRIAGE=off (the same off values as AUTO_DISPATCH, default on, read by
        // Policy::from_lookup) is checked in the loop before it starts any triage agent.
        let off = Policy::from_lookup(
            |k| (k == "LUCINA_FLAWS_AUTO_TRIAGE").then(|| "off".to_owned()),
            Path::new("/h"),
        )
        .unwrap();
        assert!(!off.auto_triage);
        for v in ["false", "0"] {
            let p = Policy::from_lookup(
                |k| (k == "LUCINA_FLAWS_AUTO_TRIAGE").then(|| v.to_owned()),
                Path::new("/h"),
            )
            .unwrap();
            assert!(!p.auto_triage, "{v}");
        }
        assert!(
            Policy::from_lookup(|_| None, Path::new("/h"))
                .unwrap()
                .auto_triage,
            "on by default"
        );
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let wide = |p: Policy| Policy {
            max_alive: 100,
            max_per_lane: 100,
            max_new_per_tick: 100,
            ..p
        };
        let p = wide(off);
        let f = report(&s, "an untriaged report");
        let fix = due(&s, "a fix", Platform::Web, None);
        for _ in 0..3 {
            tick(&s, &p, &h).await;
        }
        let labels = h.labels.lock().unwrap().clone();
        assert!(
            labels.iter().all(|l| l["triage"] != "1"),
            "with it off no triage agent starts: {labels:?}"
        );
        assert!(s.triage_due().unwrap().iter().any(|x| x.id == f.id));
        assert_eq!(
            target(&s, &fix.id, Platform::Web).status,
            TargetStatus::Fixing,
            "the dispatch path is unchanged"
        );
        // On: as today.
        let on = Policy {
            auto_triage: true,
            ..p
        };
        let (s2, h2) = (Flaws::in_memory().unwrap(), Fake::default());
        report(&s2, "another untriaged report");
        tick(&s2, &on, &h2).await;
        assert!(h2.labels.lock().unwrap().iter().any(|l| l["triage"] == "1"));
    }
}

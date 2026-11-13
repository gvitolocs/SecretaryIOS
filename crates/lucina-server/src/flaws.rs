//! Lucina's flaw tracker on the server (crate `lucina_flaws`, design `docs/design/flaws.md`):
//! `/api/secretary/flaws/*`, the dispatch loop, and the `lucina-rs flaws` CLI.
//!
//! - `GET  /flaws?q=&state=&platform=&status=&limit=`  `{items}`, worst first
//! - `POST /flaws`                     a NewFlaw → 201 `{flaw, possible_duplicates}`
//! - `GET  /flaws/matrix`              one row per live flaw, one cell per platform
//! - `GET|PATCH /flaws/{id}`           one flaw with its targets and history / a FlawPatch
//! - `PUT  /flaws/{id}/targets/{platform}`  a TargetUpdate; closing without evidence is 422
//! - `POST /flaws/{id}/dispatch`       `{platform}`: start that task now
//!
//! The loop ([`tick`], every minute): check what running tasks became, start what is due
//! within the policy's cap, and tell the owner the outcomes. Starting a task never closes a
//! target: only evidence does (`lucina_flaws::check_transition`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::Response;
use chrono::Utc;
use lucina_flaws::{
    Flaw, FlawError, FlawPatch, FlawState, Flaws, Gate, ListQuery, NewFlaw, Platform, Policy, Step,
    Target, TargetStatus, TargetUpdate, Verdict,
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
        Ok(Self {
            enabled: !matches!(val("LUCINA_FLAWS").as_deref(), Some("off" | "false" | "0")),
            db: val("LUCINA_FLAWS_DB")
                .map_or_else(|| data_dir.join("flaws").join("flaws.db"), PathBuf::from),
            policy: Policy::from_lookup(&get, &home())?,
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
        FlawError::BadTransition { .. } | FlawError::NotClaimable(_) => {
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
            let hands = StateHands { st: st.clone() };
            let t = dispatch_one(&flaws, &st.cfg.flaws.policy, &hands, id, p, &actor).await?;
            Ok(ok_status(202, redacted(json!(t))))
        }
        _ => Err(ApiError::not_found()),
    }
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
    /// An agent working in `cwd` on one step of the chain: its id.
    async fn start_agent(
        &self,
        policy: &Policy,
        step: &Step,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: Value,
    ) -> Result<String, String>;
    /// The agent's status; `Ok(None)`: it no longer exists; `Err`: agentd can't say now.
    async fn agent_status(&self, id: &str) -> Result<Option<String>, String>;
    async fn stop_agent(&self, id: &str);
    /// The agents agentd has for this flaw's triage that are not archived, whatever their status:
    /// the one the flaw store tracks, and the one a quota hand-off started in its place (it carries
    /// the same labels). `Err`: agentd can't say now.
    async fn triage_agents(&self, flaw: &str) -> Result<Vec<String>, String>;
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
    /// Does this step's lane have quota? A step with no probe lane is always ready. Nothing
    /// starts on a step that doesn't; the chain goes down, never up.
    async fn step_ready(&self, step: &Step) -> bool;
    /// An agent just hit this step's limit.
    fn step_out(&self, step: &Step, why: &str);
    /// Tell him, in his chat and on his phone. `false`: try again next tick.
    async fn tell(&self, text: &str) -> bool;
    /// A line on his timeline that an attempt started, and on which agent. The timeline only: he
    /// hears on the phone about what came of it, not about every start.
    async fn tell_start(&self, text: &str);
}

/// Claim, make the worktree, start the agent, in that order; any failure gives the claim back.
pub async fn start_fix(
    flaws: &Flaws,
    policy: &Policy,
    step: &Step,
    hands: &dyn Hands,
    pick_flaw: &Flaw,
    target: &Target,
    actor: &str,
) -> Result<Target, ApiError> {
    let p = target.platform;
    let id = pick_flaw.id.as_str();
    let layer = target.layer.ok_or_else(|| {
        api_error(FlawError::NotClaimable(format!(
            "{id} on {p} has no layer yet"
        )))
    })?;
    let repo = policy.repo_for(p, layer).display().to_string();
    let claimed = flaws.claim(id, p, &repo, actor).map_err(api_error)?;
    let short = id.trim_start_matches("fl-");
    // The time: a task whose agent failed to start leaves its branch behind for the sweep.
    let branch = format!(
        "flaw-{short}-{p}-{}-{}",
        claimed.attempts,
        Utc::now().format("%H%M%S")
    );
    let title = format!("Flaw {id} on {p}: {}", clip(&pick_flaw.title, 60));
    let started = async {
        allowed_model(&step.provider, step.model.as_deref())?;
        let (ws, cwd) = hands
            .start_task(&title, &repo, &branch, id, Some(p))
            .await?;
        let prompt = policy.brief(pick_flaw, &claimed, &cwd);
        let labels =
            json!({"from": "lucina", "flaw": id, "platform": p, "lucina.handoff-off": "1"});
        let agent = hands
            .start_agent(policy, step, &cwd, &prompt, &title, labels)
            .await?;
        Ok::<_, String>((ws, agent))
    }
    .await;
    match started {
        Ok((ws, agent)) => {
            let t = flaws
                .attach(id, p, &ws, &agent, step, actor)
                .map_err(api_error)?;
            hands
                .tell_start(&lucina_flaws::policy::start_line(pick_flaw, &t, step))
                .await;
            Ok(t)
        }
        Err(why) => {
            tracing::warn!(flaw = id, platform = %p, error = %why, "flaws: task not started");
            flaws.give_back(id, p, &why, actor).map_err(api_error)?;
            Err(ApiError::new(503, "dispatch_failed", why))
        }
    }
}

/// The step this attempt runs: its own, or the first one further down whose lane is ready.
/// `None`: no step is ready, so nothing starts for this target on this tick.
async fn choose_step<'a>(hands: &dyn Hands, policy: &'a Policy, attempt: u32) -> Option<&'a Step> {
    // Probed as far as it takes: the steps above this attempt's own are never asked, and once one
    // says ready the ones below it are not either. `step_for` walks the same rule over what was
    // answered, and reads a step that wasn't probed as not ready.
    let first = policy.start_step(attempt);
    let mut ready = vec![false; first];
    for step in &policy.chain[first..] {
        let r = hands.step_ready(step).await;
        ready.push(r);
        if r {
            break;
        }
    }
    policy.step_for(attempt, &ready).map(|i| &policy.chain[i])
}

/// The step the last agent started for this target ran, read back from its history entry: what to
/// mark out when it comes back saying it hit the limit.
fn started_step(flaws: &Flaws, id: &str, p: Platform) -> Option<Step> {
    let d = flaws.get(id).ok()?;
    let e = d
        .events
        .iter()
        .rev()
        .find(|e| e.kind == "started" && e.platform == Some(p))?;
    let str_field = |k: &str| e.detail.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(Step::new(
        e.detail.get("provider").and_then(Value::as_str)?,
        str_field("model").as_deref(),
        str_field("probe_lane").as_deref(),
    ))
}

/// The step this flaw's triage agent ran on: what to mark out when it ends on a limit. A flaw
/// triaged before the chain has no step recorded, which is the first one.
fn triage_ran<'a>(policy: &'a Policy, flaw: &Flaw) -> Option<&'a Step> {
    let last = policy.chain.len().checked_sub(1)?;
    Some(&policy.chain[flaw.triage_step.unwrap_or(0).min(last)])
}

/// Triage's step: the first of the cheap ones whose lane is ready, probed no further than it takes.
/// `None`: every step triage may use is out, so the reports wait.
async fn choose_triage_step(hands: &dyn Hands, policy: &Policy) -> Option<usize> {
    let mut ready = Vec::with_capacity(policy.triage_steps);
    for step in policy.chain.iter().take(policy.triage_steps) {
        let r = hands.step_ready(step).await;
        ready.push(r);
        if r {
            break;
        }
    }
    policy.triage_step(&ready)
}

/// `POST /flaws/{id}/dispatch`: the explicit start, under the same gates and cap as the loop.
pub async fn dispatch_one(
    flaws: &Flaws,
    policy: &Policy,
    hands: &dyn Hands,
    id: &str,
    p: Platform,
    actor: &str,
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
    let step = match choose_step(hands, policy, target.attempts + 1).await {
        Some(step) => step,
        None => {
            return Err(ApiError::new(
                503,
                "lane_out",
                "no agent of the chain has quota; it starts by itself when one is back",
            ))
        }
    };
    let repo = policy.repo_for(p, layer).display().to_string();
    if running_counts(flaws).get(&repo).copied().unwrap_or(0) >= policy.max_running_per_repo {
        return Err(ApiError::new(
            409,
            "busy",
            format!(
                "{} tasks already run in this repo",
                policy.max_running_per_repo
            ),
        ));
    }
    start_fix(flaws, policy, step, hands, &d.flaw, target, actor).await
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

fn older_than(at: Option<&str>, minutes: i64) -> bool {
    at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| Utc::now().signed_duration_since(t).num_minutes() >= minutes)
}

/// An agent's error that means the lane is out, not that the fix failed.
fn is_limit(error: &str) -> bool {
    let e = error.to_lowercase();
    ["limit", "quota", "credits", "429", "billing"]
        .iter()
        .any(|w| e.contains(w))
}

/// What is known about each lane of the chain, for this process: `lane -> (ready, until)`. Every
/// step keeps its own answer: one lane being out never says anything about another.
static LANES: std::sync::Mutex<std::collections::BTreeMap<String, (bool, std::time::Instant)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
const LANE_READY_FOR: Duration = Duration::from_secs(30 * 60);
const LANE_OUT_FOR: Duration = Duration::from_secs(60 * 60);

fn lanes(
) -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, (bool, std::time::Instant)>>
{
    LANES.lock().unwrap_or_else(|e| e.into_inner())
}

/// `lane probe`, one tiny call. A probe that can't run or read counts as ready: an agent that hits
/// the limit says so itself.
async fn probe_lane(lane: &str) -> bool {
    let bin = home().join(".local/bin/lane");
    let run = tokio::process::Command::new(bin)
        .args(["probe", "--lane", lane])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(120), run).await else {
        return true;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    let outcome = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["outcome"].as_str().map(str::to_owned));
    !matches!(outcome.as_deref(), Some("quota" | "unavailable"))
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

/// Check what each running task became (Symphony: re-read state after every turn).
async fn reconcile(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
    for (flaw, t) in flaws.in_progress().unwrap_or_default() {
        let p = t.platform;
        let Some(agent) = t.agent.clone() else {
            if t.session.is_none() && older_than(t.claimed_at.as_deref(), STALE_CLAIM_MINUTES) {
                let _ = flaws.release(
                    &flaw.id,
                    p,
                    "the claim never became a task",
                    policy.max_attempts,
                    ACTOR,
                );
            }
            continue;
        };
        if matches!(flaw.state, FlawState::Duplicate | FlawState::Dismissed) {
            hands.stop_agent(&agent).await;
            let _ = flaws.release(
                &flaw.id,
                p,
                &format!("the flaw is {}", flaw.state),
                policy.max_attempts,
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
            Some("running" | "initializing") => {
                if older_than(t.claimed_at.as_deref(), policy.agent_minutes) {
                    hands.stop_agent(&agent).await;
                    let _ = flaws.release(
                        &flaw.id,
                        p,
                        &format!("stopped after {} minutes", policy.agent_minutes),
                        policy.max_attempts,
                        ACTOR,
                    );
                }
            }
            // It ended and nothing was merged: the agent may have changed the target itself, in
            // which case it isn't `fixing` any more and this list didn't include it.
            _ => {
                if let Some(e) = hands.agent_error(&agent).await.filter(|e| is_limit(e)) {
                    // The step that ran is out, not the chain: the next tick looks below it.
                    if let Some(step) = started_step(flaws, &flaw.id, p) {
                        hands.step_out(&step, &e);
                    }
                    let _ = flaws.give_back(&flaw.id, p, &e, ACTOR);
                    continue;
                }
                let _ = flaws.release(
                    &flaw.id,
                    p,
                    "the agent ended without a merged commit",
                    policy.max_attempts,
                    ACTOR,
                );
            }
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

async fn run_triage(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
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
            Ok(Some(s)) if s == "running" || s == "initializing" => {
                if older_than(Some(&flaw.updated_at), TRIAGE_MINUTES) {
                    hands.stop_agent(&agent).await;
                    if flaws
                        .finish_triage(&flaw.id, "stopped after 15 minutes", ACTOR)
                        .is_ok()
                    {
                        archive_triage(hands, &agent).await;
                    }
                } else {
                    busy += 1;
                }
            }
            Ok(_) => {
                match hands.agent_error(&agent).await.filter(|e| is_limit(e)) {
                    Some(e) => {
                        // Triage keeps to the cheap end of the chain; the step that ran is the one
                        // out of quota, not always the first.
                        if let Some(step) = triage_ran(policy, &flaw) {
                            hands.step_out(step, &e);
                        }
                        if record_stopped_triage(flaws, &flaw, &e) {
                            archive_triage(hands, &agent).await;
                        }
                    }
                    None => {
                        if flaws
                            .finish_triage(&flaw.id, "the agent ended", ACTOR)
                            .is_ok()
                        {
                            archive_triage(hands, &agent).await;
                        }
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
    let Some(ran) = choose_triage_step(hands, policy).await else {
        return;
    };
    let step = &policy.chain[ran];
    for flaw in due {
        if busy >= policy.max_running_per_repo {
            break;
        }
        // One triage per flaw, whatever the flaw store last wrote: agentd may still be holding an
        // agent for it, and the one a hand-off started in its place carries the same labels.
        match hands.triage_agents(&flaw.id).await {
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
        let started = async {
            allowed_model(&step.provider, step.model.as_deref())?;
            let (_, cwd) = hands
                .start_task(
                    &title,
                    &repo,
                    &format!("flaw-{short}-triage-{}", Utc::now().format("%H%M%S")),
                    &flaw.id,
                    None,
                )
                .await?;
            hands
                .start_agent(
                    policy,
                    step,
                    &cwd,
                    &policy.triage_brief(&flaw, &cwd),
                    &title,
                    json!({"from": "lucina", "flaw": flaw.id, "triage": "1", "lucina.handoff-off": "1"}),
                )
                .await
        }
        .await;
        match started {
            Ok(agent) => {
                let _ = flaws.start_triage(&flaw.id, &agent, false, Some(ran), ACTOR);
                busy += 1;
            }
            Err(why) => tracing::warn!(flaw = %flaw.id, error = %why, "flaws: triage not started"),
        }
    }
}

/// One round of the loop.
pub async fn tick(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
    reconcile(flaws, policy, hands).await;
    run_triage(flaws, policy, hands).await;
    if policy.auto_dispatch {
        // Triage agents take slots too.
        let mut running = running_counts(flaws);
        let triaging = flaws.triaging().unwrap_or_default().len();
        if triaging > 0 {
            *running
                .entry(policy.secretary_repo.display().to_string())
                .or_insert(0) += triaging;
        }
        let due = flaws.due().unwrap_or_default();
        for pick in policy.pick(due, &running) {
            let verdict = match pick.verdict {
                Verdict::Start => Verdict::Start,
                Verdict::AskOwner(g) => match &pick.flaw.decision_id {
                    None => {
                        match hands.ask_owner(&pick.flaw, g).await {
                            Ok(dc) => {
                                let patch = FlawPatch {
                                    decision_id: Some(dc),
                                    ..FlawPatch::default()
                                };
                                let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                            }
                            Err(e) => {
                                tracing::warn!(flaw = %pick.flaw.id, error = %e, "flaws: couldn't ask")
                            }
                        }
                        continue;
                    }
                    Some(dc) => match hands.decision(dc).await {
                        Some(true) => Verdict::Start,
                        Some(false) => {
                            let patch = FlawPatch {
                                dismissed: Some(format!("the owner said no ({dc})")),
                                ..FlawPatch::default()
                            };
                            let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                            continue;
                        }
                        None => continue,
                    },
                },
            };
            debug_assert_eq!(verdict, Verdict::Start);
            // The free steps first: this attempt's own step, or the first ready one below it.
            let Some(step) = choose_step(hands, policy, pick.target.attempts + 1).await else {
                continue;
            };
            let _ = start_fix(flaws, policy, step, hands, &pick.flaw, &pick.target, ACTOR).await;
        }
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

    async fn start_agent(
        &self,
        policy: &Policy,
        step: &Step,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: Value,
    ) -> Result<String, String> {
        let body = json!({
            "provider": step.provider, "model": step.model, "thinkingOptionId": policy.effort,
            "modeId": policy.mode, "cwd": cwd, "prompt": prompt, "title": clip(title, 80), "labels": labels,
        });
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

    fn step_out(&self, step: &Step, why: &str) {
        let Some(lane) = step.probe_lane.as_deref() else {
            return;
        };
        tracing::warn!(
            lane,
            why,
            "flaws: this step's lane is out; it waits for a probe that says it is back"
        );
        lanes().insert(
            lane.to_owned(),
            (false, std::time::Instant::now() + LANE_OUT_FOR),
        );
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
    use std::sync::Mutex;

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
    }

    impl Fake {
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
            self.tasks.lock().unwrap().push(format!("{repo} {branch}"));
            Ok((format!("wks_{branch}"), format!("/w/{branch}")))
        }
        async fn start_agent(
            &self,
            _p: &Policy,
            step: &Step,
            cwd: &str,
            prompt: &str,
            _t: &str,
            l: Value,
        ) -> Result<String, String> {
            if *self.fail_agent.lock().unwrap() {
                return Err("agentd is down".into());
            }
            assert!(prompt.contains("<owner_words>"));
            let id = format!("ag{}", self.agents.lock().unwrap().len());
            self.agents
                .lock()
                .unwrap()
                .push((cwd.to_owned(), step.label()));
            self.labels.lock().unwrap().push(l);
            self.status
                .lock()
                .unwrap()
                .insert(id.clone(), "running".into());
            Ok(id)
        }
        async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.status.lock().unwrap().get(id).cloned())
        }
        async fn stop_agent(&self, id: &str) {
            self.stopped.lock().unwrap().push(id.to_owned());
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
            self.errors.lock().unwrap().get(id).cloned()
        }
        async fn step_ready(&self, step: &Step) -> bool {
            // A step without a probe lane is always ready; a probe answers for its own lane only.
            let Some(lane) = step.probe_lane.as_deref() else {
                return true;
            };
            !*self.out.lock().unwrap() && !self.lanes_out.lock().unwrap().iter().any(|l| l == lane)
        }
        fn step_out(&self, step: &Step, _why: &str) {
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
        async fn tell_start(&self, text: &str) {
            self.started.lock().unwrap().push(text.to_owned());
        }
    }

    fn policy() -> Policy {
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
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji are boxes", Platform::Web, None);
        tick(&s, &p, &h).await;
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        // Released and started again in the same round, on a new branch.
        assert_eq!(h.tasks.lock().unwrap().len(), 2);
        assert!(h.tasks.lock().unwrap()[1].contains("-web-2-"));
        h.status
            .lock()
            .unwrap()
            .insert("ag1".into(), "closed".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &f.id, Platform::Web).status,
            TargetStatus::Blocked
        );
        assert_eq!(h.tasks.lock().unwrap().len(), 2, "no third try");
        assert!(h.told.lock().unwrap()[0].contains("couldn't fix it after 2 tries"));
    }

    #[tokio::test]
    async fn a_quota_stop_costs_no_try_and_the_step_below_takes_it() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
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
        *h.lanes_out.lock().unwrap() = vec![];
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
            .insert("ag0".into(), "the provider crashed".into());
        tick(&s, &p, &h).await;
        assert_eq!(
            target(&s, &f.id, Platform::Web).attempts,
            2,
            "released and started again"
        );
    }

    #[tokio::test]
    async fn a_failed_try_escalates_to_the_next_step_of_the_chain() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        // Room for the whole chain: two tries is the default, and the third step is the point here.
        let p = Policy {
            max_attempts: 4,
            ..policy()
        };
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
}

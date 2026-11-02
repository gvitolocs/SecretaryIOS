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
    Flaw, FlawError, FlawPatch, FlawState, Flaws, Gate, ListQuery, NewFlaw, Platform, Policy, Target,
    TargetStatus, TargetUpdate, Verdict,
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
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let val = |k: &str| get(k).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        Self {
            enabled: !matches!(val("LUCINA_FLAWS").as_deref(), Some("off" | "false" | "0")),
            db: val("LUCINA_FLAWS_DB").map_or_else(|| data_dir.join("flaws").join("flaws.db"), PathBuf::from),
            policy: Policy::from_lookup(&get, &home()),
        }
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
        FlawError::BadTransition { .. } | FlawError::NotClaimable(_) => ApiError::new(409, e.code(), e.to_string()),
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
        Some(l) => Some(l.trim().parse::<u32>().map_err(|_| ApiError::bad_request("limit must be a number"))?),
    };
    Ok(ListQuery {
        q: param("q"),
        state: param("state").map(|s| FlawState::parse(&s)).transpose().map_err(api_error)?,
        platform: param("platform").map(|s| Platform::parse(&s)).transpose().map_err(api_error)?,
        status: param("status").map(|s| TargetStatus::parse(&s)).transpose().map_err(api_error)?,
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
    let flaws = st
        .flaws
        .clone()
        .ok_or_else(|| ApiError::new(404, "flaws_disabled", "The flaw tracker is off (LUCINA_FLAWS)"))?;
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
    match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
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
            Ok(ok(redacted(json!(flaws.patch(id, p, &actor).map_err(api_error)?))))
        }
        [id, "targets", platform] => {
            only(&["PUT"])?;
            id_ok(id)?;
            let p = Platform::parse(platform).map_err(api_error)?;
            let u: TargetUpdate = parse(read_json(body, BODY_BYTES).await?)?;
            Ok(ok(redacted(json!(flaws.set_target(id, p, u, &actor).map_err(api_error)?))))
        }
        [id, "dispatch"] => {
            only(&["POST"])?;
            id_ok(id)?;
            let v = read_json(body, 4096).await?;
            let p = Platform::parse(v.get("platform").and_then(Value::as_str).unwrap_or("")).map_err(api_error)?;
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
    async fn start_task(&self, title: &str, repo: &str, branch: &str, flaw: &str, platform: Option<Platform>) -> Result<(String, String), String>;
    /// An agent working in `cwd`: its id.
    async fn start_agent(&self, policy: &Policy, cwd: &str, prompt: &str, title: &str, labels: Value) -> Result<String, String>;
    /// The agent's status; `Ok(None)`: it no longer exists; `Err`: agentd can't say now.
    async fn agent_status(&self, id: &str) -> Result<Option<String>, String>;
    async fn stop_agent(&self, id: &str);
    /// What the task's merge landed, if it did.
    async fn merged_sha(&self, workspace: &str) -> Result<Option<String>, String>;
    /// Ask through `lucina_decide`: the decision's id.
    async fn ask_owner(&self, flaw: &Flaw, gate: Gate) -> Result<String, String>;
    /// `Some(true)`: go; `Some(false)`: don't; `None`: not answered.
    async fn decision(&self, id: &str) -> Option<bool>;
    /// Tell him, in his chat and on his phone. `false`: try again next tick.
    async fn tell(&self, text: &str) -> bool;
}

/// Claim, make the worktree, start the agent, in that order; any failure gives the claim back.
pub async fn start_fix(flaws: &Flaws, policy: &Policy, hands: &dyn Hands, pick_flaw: &Flaw, target: &Target, repo: &str, actor: &str) -> Result<Target, ApiError> {
    let p = target.platform;
    let id = pick_flaw.id.as_str();
    let claimed = flaws.claim(id, p, repo, actor).map_err(api_error)?;
    let short = id.trim_start_matches("fl-");
    // The time: a task whose agent failed to start leaves its branch behind for the sweep.
    let branch = format!("flaw-{short}-{p}-{}-{}", claimed.attempts, Utc::now().format("%H%M%S"));
    let title = format!("Flaw {id} on {p}: {}", clip(&pick_flaw.title, 60));
    let started = async {
        allowed_model(&policy.provider, Some(&policy.model))?;
        let (ws, cwd) = hands.start_task(&title, repo, &branch, id, Some(p)).await?;
        let prompt = policy.brief(pick_flaw, &claimed, &cwd);
        let labels = json!({"from": "lucina", "flaw": id, "platform": p});
        let agent = hands.start_agent(policy, &cwd, &prompt, &title, labels).await?;
        Ok::<_, String>((ws, agent))
    }
    .await;
    match started {
        Ok((ws, agent)) => flaws.attach(id, p, &ws, &agent, actor).map_err(api_error),
        Err(why) => {
            tracing::warn!(flaw = id, platform = %p, error = %why, "flaws: task not started");
            flaws.give_back(id, p, &why, actor).map_err(api_error)?;
            Err(ApiError::new(503, "dispatch_failed", why))
        }
    }
}

/// `POST /flaws/{id}/dispatch`: the explicit start, under the same gates and cap as the loop.
pub async fn dispatch_one(flaws: &Flaws, policy: &Policy, hands: &dyn Hands, id: &str, p: Platform, actor: &str) -> Result<Target, ApiError> {
    let d = flaws.get(id).map_err(api_error)?;
    let target = d.targets.iter().find(|t| t.platform == p).ok_or_else(|| api_error(FlawError::NotFound))?;
    let layer = target.layer.ok_or_else(|| api_error(FlawError::NotClaimable(format!("{id} on {p} has no layer yet"))))?;
    if let Verdict::AskOwner(g) = policy.verdict(&d.flaw) {
        let approved = match &d.flaw.decision_id {
            Some(dc) => hands.decision(dc).await == Some(true),
            None => false,
        };
        if !approved {
            return Err(ApiError::new(409, "needs_owner", format!("this fix needs the owner's go first ({g})")));
        }
    }
    let repo = policy.repo_for(p, layer).display().to_string();
    if running_counts(flaws).get(&repo).copied().unwrap_or(0) >= policy.max_running_per_repo {
        return Err(ApiError::new(409, "busy", format!("{} tasks already run in this repo", policy.max_running_per_repo)));
    }
    start_fix(flaws, policy, hands, &d.flaw, target, &repo, actor).await
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

fn clip(s: &str, max: usize) -> String {
    lucina_delegate::types::clip(s, max)
}

/// Check what each running task became (Symphony: re-read state after every turn).
async fn reconcile(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
    for (flaw, t) in flaws.in_progress().unwrap_or_default() {
        let p = t.platform;
        let Some(agent) = t.agent.clone() else {
            if t.session.is_none() && older_than(t.claimed_at.as_deref(), STALE_CLAIM_MINUTES) {
                let _ = flaws.release(&flaw.id, p, "the claim never became a task", policy.max_attempts, ACTOR);
            }
            continue;
        };
        if matches!(flaw.state, FlawState::Duplicate | FlawState::Dismissed) {
            hands.stop_agent(&agent).await;
            let _ = flaws.release(&flaw.id, p, &format!("the flaw is {}", flaw.state), policy.max_attempts, ACTOR);
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
                    let _ = flaws.release(&flaw.id, p, &format!("stopped after {} minutes", policy.agent_minutes), policy.max_attempts, ACTOR);
                }
            }
            // It ended and nothing was merged: the agent may have changed the target itself, in
            // which case it isn't `fixing` any more and this list didn't include it.
            _ => {
                let _ = flaws.release(&flaw.id, p, "the agent ended without a merged commit", policy.max_attempts, ACTOR);
            }
        }
    }
}

async fn run_triage(flaws: &Flaws, policy: &Policy, hands: &dyn Hands) {
    let repo = policy.secretary_repo.display().to_string();
    let mut busy = running_counts(flaws).get(&repo).copied().unwrap_or(0);
    for flaw in flaws.triaging().unwrap_or_default() {
        let Some(agent) = flaw.triage_agent.clone() else { continue };
        match hands.agent_status(&agent).await {
            Ok(Some(s)) if s == "running" || s == "initializing" => {
                if older_than(Some(&flaw.updated_at), TRIAGE_MINUTES) {
                    hands.stop_agent(&agent).await;
                    let _ = flaws.finish_triage(&flaw.id, "stopped after 15 minutes", ACTOR);
                } else {
                    busy += 1;
                }
            }
            Ok(_) => {
                let _ = flaws.finish_triage(&flaw.id, "the agent ended", ACTOR);
            }
            Err(_) => busy += 1,
        }
    }
    if !policy.auto_triage {
        return;
    }
    for flaw in flaws.triage_due().unwrap_or_default() {
        if busy >= policy.max_running_per_repo {
            break;
        }
        let short = flaw.id.trim_start_matches("fl-");
        let title = format!("Triage flaw {}: {}", flaw.id, clip(&flaw.title, 60));
        let started = async {
            allowed_model(&policy.provider, Some(&policy.model))?;
            let (_, cwd) = hands.start_task(&title, &repo, &format!("flaw-{short}-triage-{}", Utc::now().format("%H%M%S")), &flaw.id, None).await?;
            hands
                .start_agent(policy, &cwd, &policy.triage_brief(&flaw, &cwd), &title, json!({"from": "lucina", "flaw": flaw.id, "triage": "1"}))
                .await
        }
        .await;
        match started {
            Ok(agent) => {
                let _ = flaws.start_triage(&flaw.id, &agent, false, ACTOR);
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
            *running.entry(policy.secretary_repo.display().to_string()).or_insert(0) += triaging;
        }
        let due = flaws.due().unwrap_or_default();
        for pick in policy.pick(due, &running) {
            let verdict = match pick.verdict {
                Verdict::Start => Verdict::Start,
                Verdict::AskOwner(g) => match &pick.flaw.decision_id {
                    None => {
                        match hands.ask_owner(&pick.flaw, g).await {
                            Ok(dc) => {
                                let patch = FlawPatch { decision_id: Some(dc), ..FlawPatch::default() };
                                let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                            }
                            Err(e) => tracing::warn!(flaw = %pick.flaw.id, error = %e, "flaws: couldn't ask"),
                        }
                        continue;
                    }
                    Some(dc) => match hands.decision(dc).await {
                        Some(true) => Verdict::Start,
                        Some(false) => {
                            let patch = FlawPatch { dismissed: Some(format!("the owner said no ({dc})")), ..FlawPatch::default() };
                            let _ = flaws.patch(&pick.flaw.id, patch, ACTOR);
                            continue;
                        }
                        None => continue,
                    },
                },
            };
            debug_assert_eq!(verdict, Verdict::Start);
            let _ = start_fix(flaws, policy, hands, &pick.flaw, &pick.target, &pick.repo, ACTOR).await;
        }
    }
    let untold = flaws.untold().unwrap_or_default();
    if !untold.is_empty() {
        let lines: Vec<String> = untold.iter().map(|o| lucina_flaws::policy::outcome_line(&o.flaw, &o.target, policy)).collect();
        let text = if lines.len() == 1 { format!("🔧 {}", lines[0]) } else { format!("🔧 Flaws:\n{}", lines.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n")) };
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
    async fn agentd(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        agentd_request(&crate::agentd::socket_path(), method, path, body).await
    }
}

#[async_trait::async_trait]
impl Hands for StateHands {
    async fn start_task(&self, title: &str, repo: &str, branch: &str, flaw: &str, platform: Option<Platform>) -> Result<(String, String), String> {
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

    async fn start_agent(&self, policy: &Policy, cwd: &str, prompt: &str, title: &str, labels: Value) -> Result<String, String> {
        let body = json!({
            "provider": policy.provider, "model": policy.model, "thinkingOptionId": policy.effort,
            "modeId": policy.mode, "cwd": cwd, "prompt": prompt, "title": clip(title, 80), "labels": labels,
        });
        let v = self.agentd("POST", "/agents", Some(&body)).await?;
        v["id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned).ok_or_else(|| "agentd returned no agent id".to_owned())
    }

    async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
        match self.agentd("GET", &format!("/agents/{id}"), None).await {
            Ok(v) => Ok(v["status"].as_str().map(str::to_owned)),
            Err(e) if e.starts_with("agentd 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn stop_agent(&self, id: &str) {
        if let Err(e) = self.agentd("POST", &format!("/agents/{id}/stop"), Some(&json!({}))).await {
            tracing::warn!(agent = id, error = %e, "flaws: couldn't stop the agent");
        }
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
        let d = crate::decide::create(&self.st, &body).await.map_err(|e| format!("{e:?}"))?;
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

    async fn tell(&self, text: &str) -> bool {
        let item = self.st.timeline.record(NewItem {
            channel: "other".into(),
            role: "lucina".into(),
            text: text.to_owned(),
            meta: Some(Meta { tool: Some("flaw".into()), ok: Some(true), ..Meta::kind("flaw") }),
            ..Default::default()
        });
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
        .or_else(|| std::env::var_os("LUCINA_DATA_DIR").map(|d| Path::new(&d).join("flaws").join("flaws.db")))
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
        agents: Mutex<Vec<String>>,
        status: Mutex<HashMap<String, String>>,
        merged: Mutex<HashMap<String, String>>,
        stopped: Mutex<Vec<String>>,
        told: Mutex<Vec<String>>,
        asked: Mutex<Vec<String>>,
        answer: Mutex<Option<bool>>,
        fail_agent: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl Hands for Fake {
        async fn start_task(&self, _title: &str, repo: &str, branch: &str, _flaw: &str, _p: Option<Platform>) -> Result<(String, String), String> {
            self.tasks.lock().unwrap().push(format!("{repo} {branch}"));
            Ok((format!("wks_{branch}"), format!("/w/{branch}")))
        }
        async fn start_agent(&self, _p: &Policy, cwd: &str, prompt: &str, _t: &str, _l: Value) -> Result<String, String> {
            if *self.fail_agent.lock().unwrap() {
                return Err("agentd is down".into());
            }
            assert!(prompt.contains("<owner_words>"));
            let id = format!("ag{}", self.agents.lock().unwrap().len());
            self.agents.lock().unwrap().push(cwd.to_owned());
            self.status.lock().unwrap().insert(id.clone(), "running".into());
            Ok(id)
        }
        async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.status.lock().unwrap().get(id).cloned())
        }
        async fn stop_agent(&self, id: &str) {
            self.stopped.lock().unwrap().push(id.to_owned());
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
        async fn tell(&self, text: &str) -> bool {
            self.told.lock().unwrap().push(text.to_owned());
            true
        }
    }

    fn policy() -> Policy {
        Policy { auto_triage: false, ..Policy::from_lookup(|_| None, Path::new("/h")) }
    }

    fn due(s: &Flaws, words: &str, p: Platform, gate: Option<Gate>) -> Flaw {
        let f = s.report(NewFlaw { words: words.into(), severity: Some(Severity::High), affected: vec![p], gate, ..NewFlaw::default() }, "owner").unwrap().flaw.flaw;
        s.set_target(&f.id, p, TargetUpdate { layer: Some(Layer::Client), ..TargetUpdate::default() }, "t").unwrap();
        f
    }

    fn target(s: &Flaws, id: &str, p: Platform) -> Target {
        s.get(id).unwrap().targets.into_iter().find(|t| t.platform == p).unwrap()
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
        let flaws: Vec<Flaw> = (0..4).map(|i| due(&s, &format!("flaw number {i}"), Platform::Web, None)).collect();
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2, "two per repo");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        // One lands: a slot opens, and the next starts.
        let first = s.in_progress().unwrap()[0].1.clone();
        h.merged.lock().unwrap().insert(first.workspace.clone().unwrap(), "abcdef1234".into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
        assert!(flaws.iter().any(|f| target(&s, &f.id, Platform::Web).status == TargetStatus::Fixed));
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
        assert_eq!((t.status, t.commit_sha.as_deref()), (TargetStatus::Fixed, Some("abcdef1234")));
        assert_ne!(s.get(&f.id).unwrap().flaw.state, FlawState::Resolved, "only evidence closes it");
        let told = h.told.lock().unwrap();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("fixed in abcdef1234") && told[0].contains("not shown working yet"), "{told:?}");
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
        h.status.lock().unwrap().insert("ag1".into(), "closed".into());
        tick(&s, &p, &h).await;
        assert_eq!(target(&s, &f.id, Platform::Web).status, TargetStatus::Blocked);
        assert_eq!(h.tasks.lock().unwrap().len(), 2, "no third try");
        assert!(h.told.lock().unwrap()[0].contains("couldn't fix it after 2 tries"));
    }

    #[tokio::test]
    async fn a_duplicate_stops_its_running_agent() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let a = s.report(NewFlaw { words: "older one".into(), ..NewFlaw::default() }, "o").unwrap().flaw.flaw;
        let f = due(&s, "same one", Platform::Web, None);
        tick(&s, &p, &h).await;
        s.patch(&f.id, FlawPatch { duplicate_of: Some(a.id), ..FlawPatch::default() }, "t").unwrap();
        tick(&s, &p, &h).await;
        assert_eq!(h.stopped.lock().unwrap().as_slice(), ["ag0"]);
        assert_ne!(target(&s, &f.id, Platform::Web).status, TargetStatus::Fixing);
    }

    #[tokio::test]
    async fn a_gate_asks_once_and_waits() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "needs a card", Platform::Web, Some(Gate::Payment));
        tick(&s, &p, &h).await;
        tick(&s, &p, &h).await;
        assert_eq!(h.asked.lock().unwrap().len(), 1, "asked once");
        assert!(h.agents.lock().unwrap().is_empty());
        assert_eq!(dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t").await.unwrap_err().code, "needs_owner");
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
        let e = dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t").await.unwrap_err();
        assert_eq!(e.code, "dispatch_failed");
        let t = target(&s, &f.id, Platform::Web);
        assert_eq!((t.status, t.agent.clone(), t.attempts), (TargetStatus::Affected, None, 0));
        // An outage never blocks a flaw, however long it lasts.
        for _ in 0..5 {
            tick(&s, &p, &h).await;
        }
        assert_eq!(target(&s, &f.id, Platform::Web).status, TargetStatus::Affected);
        *h.fail_agent.lock().unwrap() = false;
        tick(&s, &p, &h).await;
        assert_eq!(target(&s, &f.id, Platform::Web).status, TargetStatus::Fixing);
    }

    #[tokio::test]
    async fn off_means_no_starts_but_explicit_dispatch_works() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy { auto_dispatch: false, ..policy() };
        let f = due(&s, "emoji", Platform::Web, None);
        tick(&s, &p, &h).await;
        assert!(h.agents.lock().unwrap().is_empty());
        dispatch_one(&s, &p, &h, &f.id, Platform::Web, "t").await.unwrap();
        assert_eq!(h.agents.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn triage_starts_one_agent_per_report_within_the_cap() {
        let (s, h) = (Flaws::in_memory().unwrap(), Fake::default());
        let p = Policy { auto_triage: true, ..policy() };
        for i in 0..3 {
            s.report(NewFlaw { words: format!("report number {i}"), ..NewFlaw::default() }, "o").unwrap();
        }
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2, "two slots");
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 2);
        h.status.lock().unwrap().insert("ag0".into(), "idle".into());
        tick(&s, &p, &h).await;
        assert_eq!(h.agents.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_verified_target_is_told_as_shown_working() {
        let (s, h, p) = (Flaws::in_memory().unwrap(), Fake::default(), policy());
        let f = due(&s, "emoji", Platform::Web, None);
        s.record_merge(&f.id, Platform::Web, "abcdef1", "l").unwrap();
        s.set_target(&f.id, Platform::Web, TargetUpdate { status: Some(TargetStatus::Verified), evidence: Some(Evidence { kind: EvidenceKind::Screenshot, reference: "a.png".into(), build: None, note: None }), ..TargetUpdate::default() }, "t").unwrap();
        tick(&s, &p, &h).await;
        assert!(h.told.lock().unwrap()[0].contains("fixed and shown working"));
    }
}

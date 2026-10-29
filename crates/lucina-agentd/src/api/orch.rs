//! Phase 6 routes: schedules and heartbeats, search, the prompt index, detach and usage.
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | GET | `/schedules` | | `{schedules: [schedule]}` |
//! | POST | `/schedules` | schedule input (Paseo's shape or the short one; `agentId: "self"` from an agent) | schedule (201) |
//! | GET | `/schedules/{id}` | | schedule |
//! | PATCH | `/schedules/{id}` | `{name, prompt, cadence, cron, timezone, everyMs, maxRuns, expiresAt, newAgentConfig}` | schedule |
//! | DELETE | `/schedules/{id}` | | `{scheduleId, deleted}` |
//! | GET | `/schedules/{id}/logs` | | `{runs}` (oldest first) |
//! | POST | `/schedules/{id}/{pause,resume,run-once}` | | schedule |
//! | GET | `/agents/search` | `?q=&limit=&archived=` | `{query, results}` |
//! | GET | `/agents/{id}/prompts` | | `{epoch, prompts: [{seq, timestamp, preview}]}` |
//! | POST | `/agents/{id}/detach` | | snapshot |
//! | GET | `/usage` | | `{providers, schedules, since}` |
//!
//! Changes to a schedule check its creator ([`Manager::may_change_schedule`]).

use std::collections::BTreeMap;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_agents::schedule::ScheduleStatus;
use lucina_agents::AgentStatus;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{to_json, who, ApiError, ApiResult};
use crate::manager::{Caller, Manager, ScheduleInput};
use crate::peer::Peer;

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/schedules", get(list).post(create))
        .route("/schedules/{id}", get(inspect).patch(update).delete(delete))
        .route("/schedules/{id}/logs", get(logs))
        .route("/schedules/{id}/pause", post(pause))
        .route("/schedules/{id}/resume", post(resume))
        .route("/schedules/{id}/run-once", post(run_once))
        .route("/agents/search", get(search))
        .route("/agents/{id}/prompts", get(prompts))
        .route("/agents/{id}/detach", post(detach))
        .route("/usage", get(usage))
}

async fn list(State(m): State<Manager>) -> ApiResult {
    to_json(json!({"schedules": m.schedules()}))
}

async fn create(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(mut input): Json<ScheduleInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let caller = who(&m, &peer, &headers);
    if input.agent_id.as_deref() == Some("self") {
        input.agent_id = match &caller {
            Caller::Agent(id) => Some(id.clone()),
            _ => return Err(ApiError(StatusCode::BAD_REQUEST, "INVALID_REQUEST".into(), "`agentId: self` works only from inside an agent".into())),
        };
    }
    let s = m.create_schedule(&caller, input)?;
    Ok((StatusCode::CREATED, to_json(s)?))
}

async fn inspect(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    to_json(m.schedule(&id)?)
}

async fn update(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> ApiResult {
    to_json(m.update_schedule_with(&who(&m, &peer, &headers), &id, &patch)?)
}

async fn delete(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    m.delete_schedule(&who(&m, &peer, &headers), &id)?;
    to_json(json!({"scheduleId": id, "deleted": true}))
}

async fn logs(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let mut runs = m.schedule(&id)?.runs;
    runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    to_json(json!({"runs": runs}))
}

async fn pause(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    to_json(m.pause_schedule(&who(&m, &peer, &headers), &id)?)
}

async fn resume(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    to_json(m.resume_schedule(&who(&m, &peer, &headers), &id)?)
}

async fn run_once(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    to_json(m.run_schedule_once(&who(&m, &peer, &headers), &id).await?)
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    archived: Option<bool>,
}

async fn search(State(m): State<Manager>, Query(q): Query<SearchQuery>) -> ApiResult {
    let results = m.search(&q.q, q.limit.unwrap_or(20), q.archived.unwrap_or(true));
    to_json(json!({"query": q.q, "results": results}))
}

async fn prompts(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    let (epoch, prompts) = m.prompts(&id)?;
    to_json(json!({"epoch": epoch, "prompts": prompts}))
}

async fn detach(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
    to_json(m.detach(&id)?)
}

#[derive(Default)]
struct Totals {
    agents: u64,
    running: u64,
    input: f64,
    cached: f64,
    output: f64,
    cost: f64,
}

/// Token use of the agents' last turns, by provider, and the schedules by status.
async fn usage(State(m): State<Manager>) -> ApiResult {
    let mut by: BTreeMap<String, Totals> = BTreeMap::new();
    let mut since: Option<String> = None;
    for a in m.snapshots(true) {
        let t = by.entry(a.provider.clone()).or_default();
        t.agents += 1;
        if matches!(a.status, AgentStatus::Running | AgentStatus::Initializing) {
            t.running += 1;
        }
        if let Some(u) = &a.last_usage {
            let f = |n: &Option<serde_json::Number>| n.as_ref().and_then(|n| n.as_f64()).unwrap_or(0.0);
            t.input += f(&u.input_tokens);
            t.cached += f(&u.cached_input_tokens);
            t.output += f(&u.output_tokens);
            t.cost += f(&u.total_cost_usd);
        }
        if since.as_ref().is_none_or(|s| a.created_at < *s) {
            since = Some(a.created_at.clone());
        }
    }
    let providers: Vec<Value> = by
        .into_iter()
        .map(|(p, t)| {
            json!({"provider": p, "agents": t.agents, "running": t.running, "inputTokens": t.input, "cachedInputTokens": t.cached,
                   "outputTokens": t.output, "totalCostUsd": (t.cost * 10_000.0).round() / 10_000.0})
        })
        .collect();
    let schedules = m.schedules();
    let count = |st: ScheduleStatus| schedules.iter().filter(|s| s.status == st).count();
    to_json(json!({
        "providers": providers,
        "schedules": {"active": count(ScheduleStatus::Active), "paused": count(ScheduleStatus::Paused), "completed": count(ScheduleStatus::Completed)},
        "since": since,
    }))
}

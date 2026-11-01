//! Tasks, the merge and deploy queue, device leases and the board ([`crate::pipeline`]).
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | GET/POST | `/pipeline/tasks` | / `{title, repo, branch, owner, session}` | `{tasks}` / `{task, workspace}` |
//! | POST | `/pipeline/tasks/adopt` | `{path, title, owner, session}` | `{task, workspace}` |
//! | POST | `/pipeline/merge` | `{workspace, keep, allow, requestedBy}` | job |
//! | POST | `/pipeline/deploy` | `{runtime, repo, note, force, requestedBy}` | job |
//! | GET | `/pipeline/jobs` | `?limit=` | `{jobs}` (newest first) |
//! | GET | `/pipeline/jobs/{id}` | `?waitMs=` (long-poll until done, up to 120 s) | job |
//! | GET | `/pipeline/jobs/{id}/log` | `?offset=` | `{offset, text, state}` |
//! | POST | `/pipeline/jobs/{id}/cancel` | | job |
//! | GET | `/pipeline/board` | | `{workspaces, unmanaged, queue, recentJobs, leases, deploys}` |
//! | GET | `/devices` | | `{devices: [{device, about, lease}]}` |
//! | POST | `/devices/{id}/lease` | `{holder, purpose, minutes, workspaceId}` | lease (409 when someone else holds it) |
//! | POST | `/devices/{id}/release` | `{holder, force, reason}` | `{device, released}` |
//!
//! Merges may come from the owner, `lagent` on the host and agentd's own agents; deploys
//! and forced lease releases only from the owner or `lagent` on the host (never an MCP token).

use std::time::Duration;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_agents::permission_gate::Origin;
use serde::Deserialize;
use serde_json::json;

use super::workspaces::view;
use super::{to_json, ApiError, ApiResult};
use crate::manager::{AdoptTask, DeployRequest, LeaseRequest, Manager, MergeRequest, ReleaseRequest, StartTask};
use crate::peer::{origin, Peer};

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/pipeline/tasks", get(tasks).post(start))
        .route("/pipeline/tasks/adopt", post(adopt))
        .route("/pipeline/merge", post(merge))
        .route("/pipeline/deploy", post(deploy))
        .route("/pipeline/jobs", get(jobs))
        .route("/pipeline/jobs/{id}", get(job))
        .route("/pipeline/jobs/{id}/log", get(log))
        .route("/pipeline/jobs/{id}/cancel", post(cancel))
        .route("/pipeline/board", get(board))
        .route("/devices", get(devices))
        .route("/devices/{id}/lease", post(lease))
        .route("/devices/{id}/release", post(release))
}

fn allow(m: &Manager, peer: &Peer, headers: &HeaderMap, agents_too: bool, what: &str) -> Result<(), ApiError> {
    match origin(peer, headers, &m.provider_pids()) {
        Origin::Local | Origin::OwnerDevice => Ok(()),
        Origin::Agent if agents_too => Ok(()),
        _ => Err(ApiError(StatusCode::FORBIDDEN, "OWNER_ONLY".into(), format!("Only the owner or lagent on the host can {what}"))),
    }
}

async fn tasks(State(m): State<Manager>) -> ApiResult {
    to_json(json!({"tasks": m.pipeline().tasks()}))
}

async fn start(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(req): Json<StartTask>) -> ApiResult {
    allow(&m, &peer, &headers, true, "start a task")?;
    let (task, w) = m.start_task(req).await?;
    to_json(json!({"task": task, "workspace": view(&m, &w)}))
}

async fn adopt(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(req): Json<AdoptTask>) -> ApiResult {
    allow(&m, &peer, &headers, false, "adopt a worktree")?;
    let (task, w) = m.adopt_task(req).await?;
    to_json(json!({"task": task, "workspace": view(&m, &w)}))
}

async fn merge(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(req): Json<MergeRequest>) -> ApiResult {
    allow(&m, &peer, &headers, true, "merge")?;
    to_json(m.queue_merge(req).await?)
}

async fn deploy(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(req): Json<DeployRequest>) -> ApiResult {
    allow(&m, &peer, &headers, false, "deploy")?;
    to_json(m.queue_deploy(req).await?)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct JobsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    offset: Option<u64>,
}

async fn jobs(State(m): State<Manager>, Query(q): Query<JobsQuery>) -> ApiResult {
    to_json(json!({"jobs": m.pipeline().jobs(q.limit.unwrap_or(30).min(500))}))
}

async fn job(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<JobsQuery>) -> ApiResult {
    let wait = Duration::from_millis(q.wait_ms.unwrap_or(0).min(120_000));
    to_json(m.wait_job(&id, wait).await?)
}

async fn log(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<JobsQuery>) -> ApiResult {
    let (offset, text) = m.job_log(&id, q.offset.unwrap_or(0))?;
    let state = m.job(&id)?.state;
    to_json(json!({"offset": offset, "text": text, "state": state}))
}

async fn cancel(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    allow(&m, &peer, &headers, true, "cancel a job")?;
    to_json(m.cancel_job(&id).await?)
}

async fn board(State(m): State<Manager>) -> ApiResult {
    to_json(m.board().await)
}

async fn devices(State(m): State<Manager>) -> ApiResult {
    to_json(json!({"devices": m.leases()}))
}

async fn lease(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<LeaseRequest>) -> ApiResult {
    allow(&m, &peer, &headers, true, "lease a device")?;
    to_json(m.lease_device(&id, req)?)
}

async fn release(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<ReleaseRequest>) -> ApiResult {
    allow(&m, &peer, &headers, !req.force, "release someone else's lease")?;
    to_json(m.release_device(&id, req)?)
}

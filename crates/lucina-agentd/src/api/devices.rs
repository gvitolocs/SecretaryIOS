//! Installs on leased devices (the owner's iPhone): [`crate::devices`]. The device list,
//! leases and releases are the pipeline's (`/devices`, `/devices/{id}/lease|release`).
//!
//! | Method | Path | Body | Returns |
//! |---|---|---|---|
//! | POST | `/devices/{d}/install` | `{holder, workspaceId, commit, test, approval, dryRun, build, note}` | `{device, kind, commit, appex, log, deployGuard}` (takes minutes: it builds) |
//! | POST | `/devices/{d}/grants` | `{holder, workspaceId, commit, approval}` | `{device, grant, requireCleanTip}` (a test run the Mac starts) |
//! | GET | `/devices/{d}/grants/{id}` | | `{device, grant}`, or `DEVICE_REFUSED` when it may not be used |
//! | POST | `/devices/{d}/grants/{id}/done` | `{exit, command, dirty, appex}` | `{recorded}` |
//! | GET | `/devices/{d}/history` | `?limit=` | `{device, installed, history: [event]}` |
//!
//! Writes come from `lagent` on this host, an agent's process or an owner device; an
//! MCP token or an unidentified caller only reads. Errors: `DEVICE_NOT_FOUND` (404),
//! `DEVICE_INVALID` (400), `DEVICE_FORBIDDEN` (403), `DEVICE_REFUSED`, `DEVICE_BUSY`
//! (409), `DEVICE_FAILED` (502).

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_agents::permission_gate::Origin;
use serde::Deserialize;

use super::{to_json, ApiError, ApiResult};
use crate::devices::{DevError, GrantDone, GrantRequest, InstallRequest};
use crate::manager::Manager;
use crate::peer::{self, Peer};

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/devices/{d}/history", get(history))
        .route("/devices/{d}/install", post(install))
        .route("/devices/{d}/grants", post(grant))
        .route("/devices/{d}/grants/{id}", get(grant_check))
        .route("/devices/{d}/grants/{id}/done", post(grant_done))
}

impl From<DevError> for ApiError {
    fn from(e: DevError) -> Self {
        let status = match e {
            DevError::NotFound(_) => StatusCode::NOT_FOUND,
            DevError::Invalid(_) => StatusCode::BAD_REQUEST,
            DevError::Refused(_) | DevError::Busy(_) => StatusCode::CONFLICT,
            DevError::Failed(_) => StatusCode::BAD_GATEWAY,
        };
        ApiError(status, e.code().into(), e.to_string())
    }
}

fn writer(m: &Manager, p: &Peer, headers: &HeaderMap) -> Result<(), ApiError> {
    match peer::origin(p, headers, &m.provider_pids()) {
        Origin::Local | Origin::Agent | Origin::OwnerDevice => Ok(()),
        _ => Err(ApiError(StatusCode::FORBIDDEN, "DEVICE_FORBIDDEN".into(), "only the owner's lagent, an agent or an owner device installs on devices".into())),
    }
}

fn body<T: for<'de> Deserialize<'de>>(v: serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| DevError::Invalid(e.to_string()).into())
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

async fn history(State(m): State<Manager>, Path(d): Path<String>, Query(q): Query<Limit>) -> ApiResult {
    to_json(m.devices().history_tail(&d, q.limit.unwrap_or(20).min(500))?)
}

async fn install(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(d): Path<String>, Json(b): Json<serde_json::Value>) -> ApiResult {
    writer(&m, &p, &headers)?;
    let req: InstallRequest = body(b)?;
    to_json(m.devices().install(&m, &d, &req).await?)
}

async fn grant(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(d): Path<String>, Json(b): Json<serde_json::Value>) -> ApiResult {
    writer(&m, &p, &headers)?;
    let req: GrantRequest = body(b)?;
    to_json(m.devices().grant(&m, &d, &req).await?)
}

async fn grant_check(State(m): State<Manager>, Path((d, id)): Path<(String, String)>) -> ApiResult {
    to_json(m.devices().grant_check(&m, &d, &id)?)
}

async fn grant_done(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path((d, id)): Path<(String, String)>, Json(b): Json<serde_json::Value>) -> ApiResult {
    writer(&m, &p, &headers)?;
    let done: GrantDone = body(b)?;
    to_json(m.devices().grant_done(&d, &id, &done)?)
}

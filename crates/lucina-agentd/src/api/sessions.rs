//! Claude sessions ([`crate::manager`] `sessions`), for `lagent session` on the host: the hook on
//! the Mac reaches these over SSH. secretary-server doesn't forward `/sessions`.
//!
//! | Method | Path | Body | Returns |
//! |---|---|---|---|
//! | GET | `/sessions` | | `{sessions}` |
//! | POST | `/sessions/beat` | `SessionBeat` | `BeatReply` |
//! | POST | `/sessions/{id}/ack` | `{commandIds}` | `{acked}` |

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_agents::external::{SessionAck, SessionBeat};
use lucina_agents::permission_gate::Origin;
use serde_json::json;

use super::{to_json, ApiError, ApiResult};
use crate::manager::Manager;
use crate::peer::{origin, Peer};

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/sessions", get(sessions))
        .route("/sessions/beat", post(beat))
        .route("/sessions/{id}/ack", post(ack))
}

/// Only `lagent` on the host speaks for a session: no device, MCP token or agentd agent does.
fn local_only(m: &Manager, peer: &Peer, headers: &HeaderMap) -> Result<(), ApiError> {
    match origin(peer, headers, &m.provider_pids()) {
        Origin::Local => Ok(()),
        _ => Err(ApiError(StatusCode::FORBIDDEN, "LOCAL_ONLY".into(), "Only lagent on the host reports for a Claude session".into())),
    }
}

async fn sessions(State(m): State<Manager>) -> ApiResult {
    to_json(json!({"sessions": m.sessions_view()}))
}

async fn beat(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(beat): Json<SessionBeat>) -> ApiResult {
    local_only(&m, &peer, &headers)?;
    to_json(m.session_beat(beat).await?)
}

async fn ack(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<SessionAck>) -> ApiResult {
    local_only(&m, &peer, &headers)?;
    to_json(json!({"acked": m.session_ack(&id, &body.command_ids)?}))
}

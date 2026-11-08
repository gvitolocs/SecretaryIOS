//! Rewind and fork (parity row A16; the manager side is `manager/rewind.rs`).
//!
//! | Method | Path | Body | Returns |
//! |---|---|---|---|
//! | POST | `/agents/{id}/rewind` | `{messageId, mode: conversation\|files\|both, allowMainCheckout?}` | `{agentId, messageId, mode, ok, epoch, removedRows}` |
//! | POST | `/agents/{id}/fork-context` | `{boundaryCursor?: {epoch, seq}, boundaryMessageId?}` | `{agentId, attachment, itemCount, boundaryCursor, boundaryMessageId, error: null}` |
//! | POST | `/agents/{id}/fork` | `{boundaryCursor?, boundaryMessageId?, prompt?, title?, provider?, model?, modeId?, thinkingOptionId?, cwd?, workspaceId?, labels?, idempotencyKey?, allowMainCheckout?}` | `{agent, forkedFrom, itemCount, boundaryCursor, boundaryMessageId}` |
//!
//! Errors are the daemon's usual `{error: {code, message}}`: `REWIND_UNSUPPORTED` (422),
//! `REWIND_TARGET_NOT_FOUND` (404), `REWIND_FORBIDDEN` (403), `NOT_YOUR_AGENT` (403),
//! `WORKSPACE_GUARD` (403), `FORK_BOUNDARY_UNAVAILABLE` (409), `AGENT_BUSY` (409).

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use lucina_agents::timeline::Cursor;
use serde::Deserialize;
use serde_json::json;

use super::{quote, to_json, who, ApiResult};
use crate::manager::{ForkRequest, Manager, RewindRequest};
use crate::peer::Peer;

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/agents/{id}/rewind", post(rewind))
        .route("/agents/{id}/fork-context", post(fork_context))
        .route("/agents/{id}/fork", post(fork))
}

async fn rewind(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(mut body): Json<RewindRequest>) -> ApiResult {
    let id = m.resolve(&id)?;
    let caller = who(&m, &peer, &headers);
    // Only the owner may rewind files in a main checkout.
    body.allow_main_checkout &= caller.is_owner();
    to_json(m.rewind(&caller, &id, body).await?)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BoundaryBody {
    #[serde(default)]
    boundary_cursor: Option<Cursor>,
    #[serde(default)]
    boundary_message_id: Option<String>,
}

async fn fork_context(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<BoundaryBody>) -> ApiResult {
    let id = m.resolve(&id)?;
    let ctx = m.fork_context(&who(&m, &peer, &headers), &id, body.boundary_cursor.as_ref(), body.boundary_message_id.as_deref())?;
    to_json(json!({
        "agentId": id,
        "attachment": ctx.attachment,
        "itemCount": ctx.item_count,
        "boundaryCursor": ctx.boundary_cursor,
        "boundaryMessageId": ctx.boundary_message_id,
        "error": null,
    }))
}

async fn fork(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(mut body): Json<ForkRequest>) -> ApiResult {
    let id = m.resolve(&id)?;
    let caller = who(&m, &peer, &headers);
    // The owner's text that looks like an agent envelope is quoted as theirs, as in `send`.
    body.prompt = body.prompt.map(quote);
    to_json(m.fork(&caller, &id, body).await?)
}

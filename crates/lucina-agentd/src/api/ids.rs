//! Identifiers of runs, changes, versions and findings ([`crate::manager::ids`]).
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | POST | `/ids` | `{kind, lead \| area \| code, title, by?, spec?, where?, base?, result?, refs?, state?, lane?, model?, chain?, repo?, workspace?, branch?, agent?}` | the row (owner only) |
//! | POST | `/ids/{id}/state` | `{state, result?, refs?, title?, by?, spec?, where?, base?, …}` | the new row (owner only) |
//! | POST | `/ids/import` | `{tsv, areas?, file?, dryRun?}` | `{imported, annotations, skipped}` (owner only) |
//! | GET | `/ids` | `?lead=&kind=&state=&q=` | `{ids, chainBrokenAt, readOnly}` |
//! | GET | `/ids/{id}` | | `{view, rows}` |
//! | GET | `/ids/{id}/trace` | | the chain from the id |
//! | GET | `/ids/trace/commit` | `?repo=&sha=` | what the commit says about its run |

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::pipeline::allow;
use super::{to_json, ApiResult};
use crate::manager::{ImportIds, Manager, NewId, StateId};
use crate::peer::Peer;

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/ids", get(list).post(issue))
        .route("/ids/import", post(import))
        .route("/ids/trace/commit", get(trace_commit))
        .route("/ids/{id}", get(show))
        .route("/ids/{id}/state", post(state))
        .route("/ids/{id}/trace", get(trace))
}

async fn issue(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(req): Json<NewId>,
) -> ApiResult {
    allow(&m, &peer, &headers, false, "issue an id")?;
    to_json(m.ids_new(req)?)
}

async fn state(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<StateId>,
) -> ApiResult {
    allow(&m, &peer, &headers, false, "change the state of an id")?;
    to_json(m.ids_state(&id, req)?)
}

async fn import(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(req): Json<ImportIds>,
) -> ApiResult {
    allow(&m, &peer, &headers, false, "import ids")?;
    to_json(m.ids_import(req)?)
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    lead: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

async fn list(State(m): State<Manager>, Query(q): Query<ListQuery>) -> ApiResult {
    to_json(m.ids_list(
        q.lead.as_deref(),
        q.kind.as_deref(),
        q.state.as_deref(),
        q.q.as_deref(),
    )?)
}

async fn show(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    to_json(m.ids_get(&id)?)
}

async fn trace(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    to_json(m.ids_trace(&id).await?)
}

#[derive(Deserialize, Default)]
struct CommitQuery {
    #[serde(default)]
    repo: String,
    #[serde(default)]
    sha: String,
}

async fn trace_commit(State(m): State<Manager>, Query(q): Query<CommitQuery>) -> ApiResult {
    to_json(m.ids_trace_commit(&q.repo, &q.sha).await?)
}

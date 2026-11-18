//! Work jobs: delegated worker runs that go through `lane` ([`crate::manager::work`]).
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | POST | `/work` | `{lead, change, priority?, cost?, lane \| chain, workspace, verify?, expect?, title?, name?, session?, spec}` | job (owner only) |
//! | GET | `/work` | `?lead=&direct=&old=` | `{jobs, leads, warning}` |
//! | GET | `/work/{id}` | | job |
//! | POST | `/work/{id}/wait` | `{timeoutSecs}` | job (final, or current when the time is up) |

use std::time::Duration;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::pipeline::allow;
use super::{to_json, ApiResult};
use crate::manager::{Manager, StartWork};
use crate::peer::Peer;

/// The longest one wait call holds on; `lagent work wait` asks again after it. Three hours on
/// one connection is more than the socket is for: the wait is a poll, not a stream.
const WAIT_MAX_SECS: u64 = 300;

/// What one wait call gets: the time asked for, never more than [`WAIT_MAX_SECS`].
fn wait_cap(secs: Option<u64>) -> u64 {
    secs.unwrap_or(WAIT_MAX_SECS).min(WAIT_MAX_SECS)
}

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/work", get(list).post(start))
        .route("/work/{id}", get(show))
        .route("/work/{id}/wait", post(wait))
}

async fn start(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(req): Json<StartWork>,
) -> ApiResult {
    allow(&m, &peer, &headers, false, "start a work job")?;
    to_json(m.work_start(req).await?)
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    lead: Option<String>,
    #[serde(default)]
    direct: Option<bool>,
    #[serde(default)]
    old: Option<bool>,
}

async fn list(State(m): State<Manager>, Query(q): Query<ListQuery>) -> ApiResult {
    let lead = q.lead.filter(|l| !l.is_empty());
    to_json(
        m.work_list(lead, q.direct.unwrap_or(true), q.old.unwrap_or(false))
            .await,
    )
}

async fn show(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    to_json(m.work_get(&id)?)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WaitBody {
    #[serde(default)]
    timeout_secs: Option<u64>,
}

async fn wait(
    State(m): State<Manager>,
    Path(id): Path<String>,
    body: Option<Json<WaitBody>>,
) -> ApiResult {
    let secs = wait_cap(body.and_then(|Json(b)| b.timeout_secs));
    to_json(m.work_wait(&id, Duration::from_secs(secs)).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_wait_call_holds_the_connection_for_minutes_not_hours() {
        // The number itself is what this is about: three hours on one connection is not what
        // the socket is for. Restoring it to 10800 fails here, not only in relation to itself.
        assert_eq!(WAIT_MAX_SECS, 300, "minutes, not hours");
        // The CLI asks again every 300 s, so the total it waits is its own business; what
        // reaches the daemon is capped here, as pipeline caps its long-poll.
        assert_eq!(wait_cap(Some(10_800)), 300);
        assert_eq!(wait_cap(None), 300);
        assert_eq!(wait_cap(Some(30)), 30);
        assert_eq!(wait_cap(Some(0)), 0);
    }
}

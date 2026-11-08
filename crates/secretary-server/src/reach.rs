//! Lucina's mailbox for Claude, Grok and Lucina Agents: `/v1/lucina/reach/*`,
//! forwarded to Lucina's `/api/secretary/reach/*` (crate `lucina-server`, module
//! `reach`).
//!
//! Lucina can message Claude (issue reports, requests the owner asked her to pass
//! on). With no live Claude channel the message waits in this mailbox and a Claude
//! session answers it; the reply goes back into the owner's conversation with
//! Lucina, who relays it.
//!
//! Owner devices only (device token or web session; Lucina's service token is not
//! accepted), like `/v1/ops` and `/v1/computer`. The MCP reaches these routes with
//! the owner device token it was configured with and names its client in
//! `X-Secretary-Caller` (`mcp:claude`); a reply carries that name to Lucina inside
//! `from`, so she knows which session answered. Query values and the thread id are
//! checked here (`Call::reach_*`) before anything travels on.
//!
//! - `GET  /v1/lucina/reach/threads?to=&status=&limit=`  `{threads: [...]}`, newest first
//!   (`to` claude|grok|agents, `status` open|all|answered|failed|queued|sent, `limit` 1-100)
//! - `GET  /v1/lucina/reach/threads/{id}`                one thread plus its `envelope`
//!   (id = `rt-` and 12 lowercase hex digits); an unknown id is 404 `thread_not_found`
//! - `POST /v1/lucina/reach/threads/{id}/reply`          `{from?, text}` (≤ 16 KiB body,
//!   text ≤ 8000 characters, from ≤ 60) → `{thread, status, delivered}`

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::computer::caller;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// A reply is at most 8000 characters (32 KB as UTF-8) plus a short `from`.
pub const REPLY_BODY_LIMIT: usize = 64 * 1024;
/// Longest `from` a client may give; the caller's name is appended after it.
const FROM_MAX: usize = 60;

async fn forward(st: &AppState, call: Call) -> ApiResult {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(data),
    ))
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ThreadsQuery {
    to: Option<String>,
    status: Option<String>,
    limit: Option<String>,
}

async fn threads(State(st): State<AppState>, QueryArgs(q): QueryArgs<ThreadsQuery>) -> ApiResult {
    forward(
        &st,
        Call::reach_threads(q.to.as_deref(), q.status.as_deref(), q.limit.as_deref())?,
    )
    .await
}

/// A single thread: Lucina answers an unknown id with a bare 404 `not_found`, which
/// the shared mapping reads as "route missing" (502); here it is the thread.
async fn forward_thread(st: &AppState, call: Call) -> ApiResult {
    match st.lucina()?.call_with_status::<Value>(&call).await {
        Ok((status, data)) => Ok((
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            Json(data),
        )),
        Err(secretary_core::Error::Api {
            status: 404, code, ..
        }) if code == "not_found" => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "thread_not_found",
            "no such thread",
        )),
        Err(e) => Err(e.into()),
    }
}

async fn thread(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult {
    forward_thread(&st, Call::reach_thread(&id)?).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyBody {
    text: String,
    #[serde(default)]
    from: Option<String>,
}

/// What Lucina is told as `from`: the name the session gave (default `Claude`)
/// and the MCP client that sent it, e.g. `coordinator (mcp:claude)`. A reply from
/// the app itself (no MCP caller) is the name alone.
fn reply_from(given: Option<&str>, caller: &str) -> Result<String, ApiError> {
    let name = match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) if n.chars().count() > FROM_MAX => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("from: must be at most {FROM_MAX} characters"),
            ))
        }
        Some(n) if n.chars().any(char::is_control) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "from: must be one line without control characters",
            ))
        }
        Some(n) => n,
        None => "Claude",
    };
    Ok(if caller == "app" {
        name.to_owned()
    } else {
        format!("{name} ({caller})")
    })
}

async fn reply(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    if !body.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "body must be a JSON object",
        ));
    }
    let body: ReplyBody = serde_json::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", e.to_string()))?;
    let from = reply_from(body.from.as_deref(), &caller(&headers))?;
    forward_thread(&st, Call::reach_reply(&id, Some(&from), &body.text)?).await
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/lucina/reach/threads", get(threads))
        .route("/v1/lucina/reach/threads/{id}", get(thread))
        .route(
            "/v1/lucina/reach/threads/{id}/reply",
            post(reply).layer(DefaultBodyLimit::max(REPLY_BODY_LIMIT)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_names_the_session_and_the_mcp_client() {
        assert_eq!(
            reply_from(Some(" coordinator "), "mcp:claude").unwrap(),
            "coordinator (mcp:claude)"
        );
        assert_eq!(
            reply_from(None, "mcp:claude").unwrap(),
            "Claude (mcp:claude)"
        );
        assert_eq!(
            reply_from(Some("  "), "mcp:grok").unwrap(),
            "Claude (mcp:grok)"
        );
        assert_eq!(reply_from(Some("owner app"), "app").unwrap(), "owner app");
        assert_eq!(reply_from(None, "app").unwrap(), "Claude");
        assert!(reply_from(Some(&"f".repeat(60)), "app").is_ok());
        assert!(reply_from(Some(&"f".repeat(61)), "app").is_err());
        assert!(reply_from(Some("a\nb"), "app").is_err());
        assert!(reply_from(Some("a\u{7}b"), "app").is_err());
    }
}

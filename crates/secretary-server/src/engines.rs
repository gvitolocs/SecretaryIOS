//! The local feature engines (gym, body) and their `/v1` routes.
//!
//! Each engine is the same Rust core the iOS app links (`secretary-ffi`),
//! persisted under `$SECRETARY_DATA_DIR` (`gym.json`, `body.json`; atomic
//! 0600 writes, an exclusive lock file per engine). Requests run on
//! `spawn_blocking` behind a mutex; the HTTP layer only maps engine codes to
//! statuses and never interprets commands.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::error::{ApiError, QueryArgs};
use crate::AppState;

pub const GYM_FILE: &str = "gym.json";
pub const BODY_FILE: &str = "body.json";

pub struct Engines {
    pub gym: Arc<Mutex<secretary_gym::Engine>>,
    pub body: Arc<Mutex<secretary_body::Engine>>,
}

impl Engines {
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let gym = secretary_gym::Engine::open(data_dir.join(GYM_FILE))
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("opening {}", data_dir.join(GYM_FILE).display()))?;
        let body = secretary_body::Engine::open(data_dir.join(BODY_FILE))
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("opening {}", data_dir.join(BODY_FILE).display()))?;
        Ok(Self {
            gym: Arc::new(Mutex::new(gym)),
            body: Arc::new(Mutex::new(body)),
        })
    }
}

/// Run `f` on the engine off the async runtime. A poisoned mutex is
/// recovered: engines only ever swap in fully validated, persisted state, so
/// a panic mid-command cannot leave a half-applied state behind.
pub async fn with_engine<E, R>(
    engine: &Arc<Mutex<E>>,
    f: impl FnOnce(&mut E) -> R + Send + 'static,
) -> Result<R, ApiError>
where
    E: Send + 'static,
    R: Send + 'static,
{
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = engine.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    })
    .await
    .map_err(|_| ApiError::internal("engine task failed"))
}

/// Engine result → HTTP status.
pub fn status_for(ok: bool, code: Option<&str>) -> StatusCode {
    match (ok, code) {
        (true, _) => StatusCode::OK,
        (false, Some("conflict")) => StatusCode::CONFLICT,
        (false, Some("storage_error")) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

#[derive(Deserialize)]
pub struct TzQuery {
    pub timezone: Option<String>,
}

fn query_request(tz: Option<String>, kind: &str) -> String {
    json!({"timezone": tz.unwrap_or_else(|| "UTC".into()), "command": {"type": kind}}).to_string()
}

/// POST body → the raw request string for the engine. Malformed JSON is the
/// engine's business (`invalid_command`, 422); a missing JSON content type
/// is 415 like every other `/v1` route.
pub(crate) fn command_body(
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<String, Box<Response>> {
    let json_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| {
            let v = v.trim();
            v.eq_ignore_ascii_case("application/json")
                || (v.starts_with("application/") && v.ends_with("+json"))
        });
    if !json_type {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "expected Content-Type: application/json",
        )
        .into_response()
        .into());
    }
    let bytes = body
        .map_err(|r| {
            if r.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    "request body exceeds the limit for this route",
                )
            } else {
                ApiError::new(StatusCode::BAD_REQUEST, "bad_request", r.body_text())
            }
            .into_response()
        })
        .map_err(Box::new)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(
                json!({"ok": false, "replayed": false, "code": "invalid_command",
                        "error": "Request body must be UTF-8"}),
            ),
        )
            .into_response()
            .into()
    })
}

macro_rules! engine_handlers {
    ($field:ident, $state:ident, $command:ident, $query:ident) => {
        async fn $query(st: &AppState, request: String) -> Result<Response, ApiError> {
            let r = with_engine(&st.engines.$field, move |e| e.handle(&request)).await?;
            Ok((status_for(r.ok, r.code.as_deref()), Json(r)).into_response())
        }

        async fn $state(
            State(st): State<AppState>,
            QueryArgs(q): QueryArgs<TzQuery>,
        ) -> Result<Response, ApiError> {
            $query(&st, query_request(q.timezone, "snapshot")).await
        }

        async fn $command(
            State(st): State<AppState>,
            headers: HeaderMap,
            body: Result<Bytes, BytesRejection>,
        ) -> Result<Response, ApiError> {
            match command_body(&headers, body) {
                Ok(request) => $query(&st, request).await,
                Err(resp) => Ok(*resp),
            }
        }
    };
}

engine_handlers!(gym, gym_state, gym_command, gym_request);
engine_handlers!(body, body_state, body_command, body_request);

async fn gym_clock(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<TzQuery>,
) -> Result<Response, ApiError> {
    gym_request(&st, query_request(q.timezone, "clock")).await
}

/// Device-authenticated engine routes (`auth::require_device` is applied by
/// the caller).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/gym/state", get(gym_state))
        .route("/v1/gym/clock", get(gym_clock))
        .route(
            "/v1/gym/command",
            post(gym_command).layer(DefaultBodyLimit::max(secretary_gym::MAX_INPUT)),
        )
        .route("/v1/body/state", get(body_state))
        .route("/v1/body/command", post(body_command))
}

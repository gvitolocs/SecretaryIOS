//! Tasks (`/v1/tasks/*`): the `secretary-tasks` engine (`tasks.json` in the
//! data dir) behind the gym/body protocol, and the Todoist sync worker.
//!
//! - `GET /v1/tasks/state?view=&id=&query=&days=&timezone=` → engine
//!   response with the sidebar and one list (`today` by default; `inbox`,
//!   `upcoming`, `project`, `label`, `filter`, `search`, `completed`).
//! - `POST /v1/tasks/command` `{op_id, expected_revision?, timezone?,
//!   command}`; the owner's zone is used when `timezone` is missing.
//! - `POST /v1/tasks/sync`: sync with Todoist now (also
//!   `POST /v1/links/todoist/sync`).
//! - `GET /v1/tasks/signals?timezone=`: today's load, overdue tasks and
//!   routine streaks (Lucina, the proactive engine).
//!
//! Callers: the owner's devices or Lucina's service token
//! ([`crate::auth::require_device_or_service`]).
//!
//! The worker runs a cycle every `SECRETARY_TODOIST_SYNC_SECS` (90 s), 1.5 s
//! after a local edit, and when a delayed command (a delete's undo window)
//! becomes due: pull (incremental with the stored sync token, full the
//! first time or after a rejected command) → push the queued commands →
//! completed history (every 10 minutes). 401/403 marks the Links card
//! `reconnect`; 429 waits `Retry-After`; other failures back off up to 15
//! minutes.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::engines::{command_body, status_for, with_engine};
use crate::error::{ApiError, QueryArgs};
use crate::todoist::{self, Client, PendingStates, TdError};
use crate::AppState;

pub const TASKS_FILE: &str = "tasks.json";
const COMPLETED_EVERY: Duration = Duration::from_secs(600);
const MAX_BACKOFF: Duration = Duration::from_secs(900);

#[derive(Default)]
struct Worker {
    backoff_until: Option<Instant>,
    failures: u32,
    last_completed: Option<Instant>,
}

pub struct TasksHub {
    pub engine: Arc<Mutex<secretary_tasks::Engine>>,
    pub client: Client,
    /// Wakes the worker (a local edit was queued).
    pub wake: tokio::sync::Notify,
    sync_lock: tokio::sync::Mutex<()>,
    pub pending: PendingStates,
    worker: Mutex<Worker>,
}

impl TasksHub {
    pub fn open(data_dir: &Path, cfg: todoist::TodoistConfig) -> anyhow::Result<Self> {
        let engine = secretary_tasks::Engine::open(data_dir.join(TASKS_FILE))
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("opening {}", data_dir.join(TASKS_FILE).display()))?;
        Ok(Self {
            engine: Arc::new(Mutex::new(engine)),
            client: Client::new(cfg)?,
            wake: tokio::sync::Notify::new(),
            sync_lock: tokio::sync::Mutex::new(()),
            pending: PendingStates::default(),
            worker: Mutex::new(Worker::default()),
        })
    }

    fn worker(&self) -> std::sync::MutexGuard<'_, Worker> {
        self.worker.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Open tasks and queued edits (the Links card).
pub async fn link_counts(st: &AppState) -> (u32, u32) {
    with_engine(&st.tasks.engine, |e| {
        (
            secretary_tasks::views::open_tasks(e.state()).count() as u32,
            e.state().outbox.len() as u32,
        )
    })
    .await
    .unwrap_or((0, 0))
}

fn engine_err(e: String) -> TdError {
    TdError::Http(0, format!("local storage: {e}"))
}

fn rfc3339(ms: u64) -> String {
    secretary_tasks::ms_to_utc(ms)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

async fn cycle(st: &AppState, token: &str) -> Result<Value, TdError> {
    let hub = &st.tasks;
    let tok = with_engine(&hub.engine, |e| e.sync_token())
        .await
        .map_err(|_| engine_err("task failed".into()))?;
    let full = tok.is_none();
    let pulled = hub.client.sync(token, tok.as_deref(), &[]).await?;
    let pull = with_engine(&hub.engine, move |e| {
        e.apply_sync(&pulled, secretary_tasks::now_ms())
    })
    .await
    .map_err(|_| engine_err("task failed".into()))?
    .map_err(engine_err)?;
    let (batch, tok) = with_engine(&hub.engine, |e| {
        (e.outbox_batch(secretary_tasks::now_ms()), e.sync_token())
    })
    .await
    .map_err(|_| engine_err("task failed".into()))?;
    let mut push = None;
    if !batch.is_empty() {
        let n = batch.len();
        let answer = hub.client.sync(token, tok.as_deref(), &batch).await?;
        let rep = with_engine(&hub.engine, move |e| {
            e.apply_sync(&answer, secretary_tasks::now_ms())
        })
        .await
        .map_err(|_| engine_err("task failed".into()))?
        .map_err(engine_err)?;
        tracing::info!(
            sent = n,
            ok = rep.pushed_ok,
            rejected = rep.rejected,
            retried = rep.retried,
            conflicts = rep.conflicts,
            "todoist push"
        );
        push = Some(rep);
    }
    let last = hub.worker().last_completed;
    let due = full || last.is_none_or(|t| t.elapsed() >= COMPLETED_EVERY);
    if due {
        let now = secretary_tasks::now_ms();
        let since = rfc3339(now.saturating_sub(7 * secretary_tasks::DAY_MS));
        let until = rfc3339(now + secretary_tasks::DAY_MS);
        match hub.client.completed(token, &since, &until).await {
            Ok(items) => {
                let _ = with_engine(&hub.engine, move |e| {
                    e.apply_completed(&items, secretary_tasks::now_ms())
                })
                .await;
                hub.worker().last_completed = Some(Instant::now());
            }
            Err(TdError::Unauthorized) => return Err(TdError::Unauthorized),
            Err(e) => tracing::warn!(error = %e, "todoist completed history"),
        }
    }
    Ok(json!({
        "full": full || pull.full,
        "changed": pull.changed || push.as_ref().is_some_and(|p| p.changed),
        "pushed": push.as_ref().map_or(0, |p| p.pushed_ok),
        "rejected": push.as_ref().map_or(0, |p| p.rejected),
        "conflicts": pull.conflicts + push.as_ref().map_or(0, |p| p.conflicts),
    }))
}

/// One sync cycle (serialized). `NotConnected` when no account is linked.
pub async fn sync_once(st: &AppState) -> Result<Value, TdError> {
    let hub = &st.tasks;
    let _one = hub.sync_lock.lock().await;
    let backoff = hub.worker().backoff_until;
    if let Some(until) = backoff {
        let now = Instant::now();
        if until > now {
            return Err(TdError::RateLimited((until - now).as_secs().max(1)));
        }
    }
    let result = match todoist::access_token(st).await {
        Ok(token) => cycle(st, &token).await,
        Err(e) => Err(e),
    };
    match &result {
        Ok(_) => {
            {
                let mut w = hub.worker();
                w.failures = 0;
                w.backoff_until = None;
            }
            todoist::note_sync(st, None).await;
        }
        Err(TdError::NotConnected) => {}
        Err(TdError::Unauthorized) => {
            tracing::warn!("todoist refused the token; Links shows reconnect");
            todoist::mark_reconnect(st, "Todoist refused the token; reconnect in Links").await;
        }
        Err(TdError::RateLimited(secs)) => {
            let secs = *secs;
            hub.worker().backoff_until = Some(Instant::now() + Duration::from_secs(secs));
            let retry = secretary_tasks::now_ms() + secs * 1000;
            let msg = format!("Todoist asked to slow down; retrying in {secs} s");
            let _ = with_engine(&hub.engine, move |e| {
                e.set_sync_status(
                    secretary_tasks::SyncStatus::RateLimited,
                    Some(msg),
                    Some(retry),
                )
            })
            .await;
        }
        Err(e) => {
            let wait = {
                let mut w = hub.worker();
                w.failures = w.failures.saturating_add(1);
                let wait = Duration::from_secs(15u64 << w.failures.min(6)).min(MAX_BACKOFF);
                w.backoff_until = Some(Instant::now() + wait);
                wait
            };
            tracing::warn!(error = %e, "todoist sync failed");
            let msg = e.to_string();
            let retry = secretary_tasks::now_ms() + wait.as_millis() as u64;
            let m2 = msg.clone();
            let _ = with_engine(&hub.engine, move |e| {
                e.set_sync_status(secretary_tasks::SyncStatus::Error, Some(m2), Some(retry))
            })
            .await;
            todoist::note_sync(st, Some(msg)).await;
        }
    }
    result
}

/// `POST /v1/tasks/sync` (and `/v1/links/todoist/sync`).
pub async fn sync_now(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    match tokio::time::timeout(Duration::from_secs(40), sync_once(&st)).await {
        Err(_) => Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "todoist_timeout",
            "Todoist did not answer in time; the sync continues in the background",
        )),
        Ok(Ok(report)) => Ok(Json(json!({"ok": true, "report": report}))),
        Ok(Err(TdError::NotConnected)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "todoist_not_connected",
            "Connect Todoist in Links first",
        )),
        Ok(Err(TdError::Unauthorized)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "todoist_reconnect",
            "Todoist refused the token; reconnect in Links",
        )),
        Ok(Err(TdError::RateLimited(s))) => Err(ApiError::rate_limited(Duration::from_secs(s))),
        Ok(Err(e)) => Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "todoist_unavailable",
            e.to_string(),
        )),
    }
}

pub fn spawn_sync_loop(st: AppState) {
    let every = st.tasks.client.cfg.sync_every;
    if every.is_zero() {
        return;
    }
    let mut stop = st.shutdown.subscribe();
    tokio::spawn(async move {
        let mut wait = Duration::from_secs(5);
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = st.tasks.wake.notified() => {
                    // let a burst of edits land in one push
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                _ = stop.changed() => return,
            }
            let r = sync_once(&st).await;
            if let Err(e) = &r {
                tracing::debug!(error = %e, "tasks sync");
            }
            wait = every;
            let now = secretary_tasks::now_ms();
            if let Ok(Some(t)) = with_engine(&st.tasks.engine, |e| {
                e.state()
                    .outbox
                    .iter()
                    .map(|o| o.not_before_ms)
                    .filter(|t| *t > secretary_tasks::now_ms())
                    .min()
            })
            .await
            {
                wait = wait.min(Duration::from_millis(t.saturating_sub(now) + 500));
            }
            let backoff = st.tasks.worker().backoff_until;
            if let Some(until) = backoff {
                wait = wait.max(until.saturating_duration_since(Instant::now()));
            }
        }
    });
}

// -- engine routes -------------------------------------------------------------------

#[derive(Deserialize)]
struct StateQuery {
    timezone: Option<String>,
    view: Option<String>,
    id: Option<String>,
    query: Option<String>,
    days: Option<u32>,
}

async fn run(st: &AppState, request: String) -> Result<Response, ApiError> {
    let r = with_engine(&st.tasks.engine, move |e| e.handle(&request)).await?;
    if r.ok && !r.replayed && r.queued {
        st.tasks.wake.notify_one();
    }
    Ok((status_for(r.ok, r.code.as_deref()), Json(r)).into_response())
}

async fn state(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<StateQuery>,
) -> Result<Response, ApiError> {
    let tz = q.timezone.unwrap_or_else(|| st.links.tz.name().to_owned());
    let mut view = serde_json::Map::new();
    if let Some(v) = q.view {
        view.insert("kind".into(), json!(v));
    }
    if let Some(v) = q.id {
        view.insert("id".into(), json!(v));
    }
    if let Some(v) = q.query {
        view.insert("query".into(), json!(v));
    }
    if let Some(v) = q.days {
        view.insert("days".into(), json!(v));
    }
    run(
        &st,
        json!({"timezone": tz, "command": {"type": "snapshot", "view": view}}).to_string(),
    )
    .await
}

async fn command(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let raw = match command_body(&headers, body) {
        Ok(r) => r,
        Err(resp) => return Ok(*resp),
    };
    // the owner's zone when the caller (Lucina, MCP) sends none
    let request = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(mut m)) if !m.contains_key("timezone") => {
            m.insert("timezone".into(), json!(st.links.tz.name()));
            Value::Object(m).to_string()
        }
        _ => raw,
    };
    run(&st, request).await
}

#[derive(Deserialize)]
struct TzQuery {
    timezone: Option<String>,
}

async fn signals(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<TzQuery>,
) -> Result<Json<Value>, ApiError> {
    let tz: chrono_tz::Tz = match q.timezone {
        Some(t) => t.parse().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "timezone: unknown IANA timezone",
            )
        })?,
        None => st.links.tz,
    };
    let s = with_engine(&st.tasks.engine, move |e| {
        secretary_tasks::signals::signals(e.state(), tz, secretary_tasks::now_ms())
    })
    .await?;
    Ok(Json(json!(s)))
}

/// Tasks summary for `/v1/insights/summary`.
pub async fn insights(st: &AppState, tz: chrono_tz::Tz) -> Option<Value> {
    with_engine(&st.tasks.engine, move |e| {
        serde_json::to_value(secretary_tasks::signals::signals(
            e.state(),
            tz,
            secretary_tasks::now_ms(),
        ))
        .ok()
    })
    .await
    .ok()
    .flatten()
}

/// Device or service token.
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/tasks/state", get(self::state))
        .route(
            "/v1/tasks/command",
            post(command).layer(DefaultBodyLimit::max(secretary_tasks::MAX_INPUT)),
        )
        .route("/v1/tasks/sync", post(sync_now))
        .route("/v1/tasks/signals", get(signals))
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::auth::require_device_or_service,
        ))
}

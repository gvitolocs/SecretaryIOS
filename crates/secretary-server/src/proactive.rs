//! Lucina's proactive outreach (the engine is `crates/lucina-proactive` on
//! the `lucina-rs` branch; it runs next to Lucina and reaches the app
//! through a timeline item with `meta.kind: "proactive"` plus `POST /v1/notify`).
//!
//! Device routes (Bearer, or cookie + `X-Secretary-CSRF: 1`):
//! - `POST /v1/proactive/outreach/{id}/feedback {action}`: the owner tapped
//!   "Utile" (`useful`), "Non ora" (`not_now`) or "Basta così" (`stop`) on
//!   an outreach, or opened one of its content buttons (`opened`). Stored;
//!   the same tap again returns the first record (`200`, else `201`).
//!
//! Service routes (Lucina's `svc_` token):
//! - `GET /v1/proactive/feedback?after=&limit=` → `{items, cursor}`: the
//!   taps after the cursor, oldest first. The engine pulls this on every
//!   tick, which is how feedback reaches it (it runs behind the tunnel and
//!   takes no inbound connections).
//! - `GET /v1/notify/settings` → `{settings, present}`: quiet hours, Do Not
//!   Disturb, language and switches, so the engine never schedules a
//!   message into quiet hours (the server enforces them anyway).
//!
//! Taps are kept in `proactive-feedback.jsonl` (0600) in the data dir, at
//! most [`MAX_KEPT`] (the oldest are dropped). No message text is stored.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

pub const FILE: &str = "proactive-feedback.jsonl";
pub const MAX_KEPT: usize = 5000;
pub const MAX_PAGE: usize = 500;
pub const ACTIONS: [&str; 4] = ["useful", "not_now", "stop", "opened"];

/// One tap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tap {
    /// `fb_<13-digit epoch ms>_<random>`: sorts by time.
    pub id: String,
    pub outreach_id: String,
    pub action: String,
    /// Epoch ms.
    pub at: i64,
    pub device: String,
    /// `app` (web or iOS).
    pub source: String,
}

/// `po_` + 26 Crockford base-32 characters (the engine's ULID).
pub fn valid_outreach_id(s: &str) -> bool {
    s.len() == 29
        && s.starts_with("po_")
        && s[3..].bytes().all(|b| {
            b.is_ascii_digit()
                || (b.is_ascii_uppercase() && !matches!(b, b'I' | b'L' | b'O' | b'U'))
        })
}

pub struct FeedbackStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FeedbackStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(FILE),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> std::io::Result<Vec<Tap>> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => Ok(t
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Record a tap; `(created, tap)`. An identical (outreach, action) tap
    /// returns the stored one.
    pub fn record(
        &self,
        outreach_id: &str,
        action: &str,
        device: &str,
        now_ms: i64,
    ) -> std::io::Result<(bool, Tap)> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.read()?;
        if let Some(t) = all
            .iter()
            .find(|t| t.outreach_id == outreach_id && t.action == action)
        {
            return Ok((false, t.clone()));
        }
        let tap = Tap {
            id: format!("fb_{now_ms:013}_{}", crate::devices::random_b64::<6>()?),
            outreach_id: outreach_id.to_owned(),
            action: action.to_owned(),
            at: now_ms,
            device: device.to_owned(),
            source: "app".into(),
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if all.len() >= MAX_KEPT {
            // Rewrite without the oldest (atomic: temp file + rename).
            all.push(tap.clone());
            let keep = &all[all.len() - MAX_KEPT..];
            let mut text = String::new();
            for t in keep {
                text.push_str(&serde_json::to_string(t).map_err(std::io::Error::other)?);
                text.push('\n');
            }
            let tmp = self.path.with_extension("jsonl.tmp");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
        } else {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.path)?;
            f.write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&tap).map_err(std::io::Error::other)?
                )
                .as_bytes(),
            )?;
        }
        Ok((true, tap))
    }

    /// Taps with an id after `after` (oldest first), at most `limit`.
    pub fn after(&self, after: Option<&str>, limit: usize) -> std::io::Result<Vec<Tap>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.read()?;
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all
            .into_iter()
            .filter(|t| after.is_none_or(|a| t.id.as_str() > a))
            .take(limit)
            .collect())
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn io_error(e: std::io::Error) -> ApiError {
    tracing::error!(error = %e, "proactive feedback store failed");
    ApiError::internal("feedback store unavailable")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackBody {
    action: String,
}

async fn feedback(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    UrlPath(id): UrlPath<String>,
    JsonBody(b): JsonBody<FeedbackBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !valid_outreach_id(&id) {
        return Err(bad("id: an outreach id (po_…)"));
    }
    if !ACTIONS.contains(&b.action.as_str()) {
        return Err(bad("action: one of useful, not_now, stop, opened"));
    }
    let store = st.proactive.clone();
    let device = ident.device_id.clone();
    let now = chrono::Utc::now().timestamp_millis();
    let (created, tap) =
        tokio::task::spawn_blocking(move || store.record(&id, &b.action, &device, now))
            .await
            .map_err(|_| ApiError::internal("feedback store failed"))?
            .map_err(io_error)?;
    if created {
        tracing::info!(outreach = %tap.outreach_id, action = %tap.action, "proactive feedback");
    }
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"feedback": tap})),
    ))
}

#[derive(Deserialize)]
struct PageQuery {
    after: Option<String>,
    limit: Option<String>,
}

async fn list(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = match q.limit.as_deref() {
        None | Some("") => MAX_PAGE,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_PAGE).contains(n))
            .ok_or_else(|| bad(format!("limit: an integer from 1 to {MAX_PAGE}")))?,
    };
    let after = q.after.filter(|a| !a.is_empty());
    if let Some(a) = &after {
        secretary_core::validate::id("after", a)?;
    }
    let store = st.proactive.clone();
    let cursor = after.clone();
    let items = tokio::task::spawn_blocking(move || store.after(after.as_deref(), limit))
        .await
        .map_err(|_| ApiError::internal("feedback store failed"))?
        .map_err(io_error)?;
    let cursor = items.last().map(|t| t.id.clone()).or(cursor);
    Ok(Json(json!({"items": items, "cursor": cursor})))
}

async fn notify_settings(State(st): State<AppState>) -> Json<Value> {
    Json(json!({"settings": st.notify.settings.get(), "present": st.notify.present()}))
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/proactive/outreach/{id}/feedback", post(feedback))
}

/// Behind `require_service`.
pub fn service_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/proactive/feedback", get(list))
        .route("/v1/notify/settings", get(notify_settings))
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::service::require_service,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(valid_outreach_id("po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F"));
        assert!(!valid_outreach_id("po_01K85Z3Q4X5Y6Z7A8B9C0D1E2"));
        assert!(!valid_outreach_id("po_01K85Z3Q4X5Y6Z7A8B9C0D1E2I"));
        assert!(!valid_outreach_id("xx_01K85Z3Q4X5Y6Z7A8B9C0D1E2F"));
        assert!(!valid_outreach_id("po_../../../etc/passwd_aaaaaa"));
    }

    #[test]
    fn store_dedups_pages_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let s = FeedbackStore::new(dir.path());
        let o = "po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F";
        let (c1, a) = s.record(o, "useful", "dev_1", 1_000).unwrap();
        let (c2, b) = s.record(o, "useful", "dev_2", 2_000).unwrap();
        assert!(c1 && !c2);
        assert_eq!(a, b);
        let (_, c) = s.record(o, "stop", "dev_1", 3_000).unwrap();
        assert_eq!(s.after(None, 10).unwrap(), vec![a.clone(), c.clone()]);
        assert_eq!(s.after(Some(&a.id), 10).unwrap(), vec![c.clone()]);
        assert!(s.after(Some(&c.id), 10).unwrap().is_empty());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

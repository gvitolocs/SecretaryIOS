//! Server health: the sentinel on each host (nezopt today) posts a full
//! snapshot of its watched units, timers, containers and disks every run;
//! the app's "Server health" plugin reads them back.
//!
//! - `POST /v1/system/health` (service token, body ≤ [`BODY_LIMIT`]): one
//!   snapshot. Validated, stored per host, and diffed against that host's
//!   previous snapshot: every check whose `state` changed becomes an event
//!   `{at, host, check_id, name, from, to, detail}`. A snapshot whose `at` is
//!   not newer than the stored one (a retry, or one that arrived late) is
//!   acknowledged and ignored, so replays never invent events.
//! - `GET /v1/system/health` (device or session) →
//!   `{hosts: [{host, received_at, snapshot, stale}], events}`: `stale` when
//!   the host has not posted for [`STALE_AFTER`]; events newest first, the
//!   last [`KEEP_EVENTS`], at most [`MAX_EVENTS_SHOWN`].
//!
//! Notifications are not sent from here: the sentinel posts them itself to
//! `POST /v1/notify` (`category: "system"`, `route: "/server-health"`).
//!
//! State lives in `system-health.json` (0600) in the data dir, written
//! through atomically like the notifier's files.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ApiError, JsonBody};
use crate::notify::store::JsonFile;
use crate::AppState;

pub const FILE: &str = "system-health.json";
pub const BODY_LIMIT: usize = 512 * 1024;
/// A host that has not posted for this long shows as unreachable.
pub const STALE_AFTER: Duration = Duration::minutes(15);
pub const KEEP_EVENTS: Duration = Duration::days(30);
/// Hard cap on stored events (a flapping check cannot grow the file without bound).
pub const MAX_EVENTS_KEPT: usize = 10_000;
pub const MAX_EVENTS_SHOWN: usize = 200;
pub const MAX_HOSTS: usize = 16;
pub const MAX_CHECKS: usize = 2000;
pub const MAX_DISKS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State3 {
    Ok,
    Warn,
    Crit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Unit,
    Timer,
    Container,
    K3s,
    Forward,
    Log,
    Disk,
    Deadman,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Group {
    Lucina,
    Pokoin,
    CardRails,
    #[serde(rename = "AI")]
    Ai,
    Homelab,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disk {
    pub mount: String,
    pub used_pct: f64,
    pub free_gb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub disks: Vec<Disk>,
    /// `null` when the sensor is unknown.
    #[serde(default)]
    pub cpu_temp_c: Option<f64>,
    pub load: [f64; 3],
    pub mem_used_pct: f64,
    #[serde(default)]
    pub swap_used_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Stable key, `<kind>:<name>`.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub group: Group,
    pub state: State3,
    pub detail: String,
    /// When this state began (RFC 3339).
    pub since: String,
    #[serde(default)]
    pub restarts_1h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub host: String,
    /// RFC 3339, the host's clock.
    pub at: String,
    pub overall: State3,
    pub metrics: Metrics,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostEntry {
    pub received_at: DateTime<Utc>,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub host: String,
    pub check_id: String,
    pub name: String,
    pub from: State3,
    pub to: State3,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HealthState {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostEntry>,
    /// Oldest first.
    #[serde(default)]
    pub events: Vec<Event>,
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn text(field: &str, s: &str, max: usize) -> Result<(), ApiError> {
    if s.trim().is_empty() || s.chars().count() > max || s.chars().any(char::is_control) {
        return Err(bad(format!(
            "{field}: 1 to {max} characters, no control characters"
        )));
    }
    Ok(())
}

fn time(field: &str, s: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| bad(format!("{field}: an RFC 3339 time")))
}

fn pct(field: &str, v: f64) -> Result<(), ApiError> {
    if !(0.0..=100.0).contains(&v) {
        return Err(bad(format!("{field}: 0 to 100")));
    }
    Ok(())
}

fn non_negative(field: &str, v: f64) -> Result<(), ApiError> {
    if !v.is_finite() || v < 0.0 {
        return Err(bad(format!("{field}: a number ≥ 0")));
    }
    Ok(())
}

/// Check a posted snapshot; returns its `at`.
pub fn validate(s: &Snapshot) -> Result<DateTime<Utc>, ApiError> {
    if s.host.is_empty()
        || s.host.len() > 64
        || !s
            .host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(bad("host: 1 to 64 of a-z, 0-9, '-', '_', '.'"));
    }
    let at = time("at", &s.at)?;
    let m = &s.metrics;
    if m.disks.len() > MAX_DISKS {
        return Err(bad(format!("metrics.disks: at most {MAX_DISKS}")));
    }
    for d in &m.disks {
        text("metrics.disks.mount", &d.mount, 256)?;
        pct("metrics.disks.used_pct", d.used_pct)?;
        non_negative("metrics.disks.free_gb", d.free_gb)?;
    }
    if let Some(t) = m.cpu_temp_c {
        if !(-50.0..=200.0).contains(&t) {
            return Err(bad("metrics.cpu_temp_c: -50 to 200, or null"));
        }
    }
    for l in m.load {
        non_negative("metrics.load", l)?;
    }
    pct("metrics.mem_used_pct", m.mem_used_pct)?;
    if let Some(v) = m.swap_used_pct {
        pct("metrics.swap_used_pct", v)?;
    }
    if s.checks.len() > MAX_CHECKS {
        return Err(bad(format!("checks: at most {MAX_CHECKS}")));
    }
    let mut ids = HashSet::new();
    for c in &s.checks {
        text("checks.id", &c.id, 200)?;
        text("checks.name", &c.name, 200)?;
        if c.detail.chars().count() > 500 || c.detail.chars().any(char::is_control) {
            return Err(bad("checks.detail: at most 500 characters"));
        }
        time("checks.since", &c.since)?;
        if !ids.insert(c.id.as_str()) {
            return Err(bad(format!("checks.id: {} appears twice", c.id)));
        }
    }
    Ok(at)
}

/// The events between two consecutive snapshots of one host: every check
/// present in both whose state changed. Checks that appear or disappear
/// make no event.
pub fn diff(prev: &Snapshot, next: &Snapshot, at: DateTime<Utc>) -> Vec<Event> {
    let before: BTreeMap<&str, State3> = prev
        .checks
        .iter()
        .map(|c| (c.id.as_str(), c.state))
        .collect();
    next.checks
        .iter()
        .filter_map(|c| {
            let from = *before.get(c.id.as_str())?;
            (from != c.state).then(|| Event {
                at,
                host: next.host.clone(),
                check_id: c.id.clone(),
                name: c.name.clone(),
                from,
                to: c.state,
                detail: c.detail.clone(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    Stored {
        events: usize,
    },
    /// Not newer than the stored snapshot of that host.
    Ignored,
}

pub struct HealthStore {
    file: JsonFile<HealthState>,
}

impl HealthStore {
    pub fn open(data_dir: &Path) -> Self {
        Self {
            file: JsonFile::open(data_dir, FILE),
        }
    }

    /// Store a validated snapshot received at `now`.
    pub fn ingest(
        &self,
        snap: Snapshot,
        at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Ingested, ApiError> {
        self.file
            .update(|st| {
                let mut events = 0;
                match st.hosts.get(&snap.host) {
                    Some(prev) => {
                        let prev_at = DateTime::parse_from_rfc3339(&prev.snapshot.at)
                            .map(|t| t.with_timezone(&Utc))
                            .ok();
                        if prev_at.is_some_and(|p| at <= p) {
                            return Ok(Ingested::Ignored);
                        }
                        let new = diff(&prev.snapshot, &snap, at);
                        events = new.len();
                        st.events.extend(new);
                    }
                    None if st.hosts.len() >= MAX_HOSTS => {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "too_many_hosts",
                            format!("at most {MAX_HOSTS} hosts"),
                        ));
                    }
                    None => {}
                }
                let cutoff = now - KEEP_EVENTS;
                st.events.retain(|e| e.at >= cutoff);
                st.events.sort_by_key(|e| e.at);
                if st.events.len() > MAX_EVENTS_KEPT {
                    let extra = st.events.len() - MAX_EVENTS_KEPT;
                    st.events.drain(..extra);
                }
                st.hosts.insert(
                    snap.host.clone(),
                    HostEntry {
                        received_at: now,
                        snapshot: snap,
                    },
                );
                Ok(Ingested::Stored { events })
            })
            .map_err(|e| {
                tracing::warn!(error = %e, "cannot write {FILE}");
                ApiError::internal("cannot store the snapshot")
            })?
    }

    /// The `GET /v1/system/health` body at `now`.
    pub fn view(&self, now: DateTime<Utc>) -> Value {
        self.file.read(|st| {
            let hosts: Vec<Value> = st
                .hosts
                .iter()
                .map(|(host, h)| {
                    json!({
                        "host": host,
                        "received_at": h.received_at,
                        "snapshot": h.snapshot,
                        "stale": now - h.received_at > STALE_AFTER,
                    })
                })
                .collect();
            let cutoff = now - KEEP_EVENTS;
            let events: Vec<&Event> = st
                .events
                .iter()
                .rev()
                .filter(|e| e.at >= cutoff)
                .take(MAX_EVENTS_SHOWN)
                .collect();
            json!({"hosts": hosts, "events": events})
        })
    }
}

async fn post_snapshot(
    State(st): State<AppState>,
    JsonBody(snap): JsonBody<Snapshot>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let at = validate(&snap)?;
    let store = st.system_health.clone();
    let outcome = tokio::task::spawn_blocking(move || store.ingest(snap, at, Utc::now()))
        .await
        .map_err(|_| ApiError::internal("health task failed"))??;
    Ok(match outcome {
        Ingested::Stored { events } => (
            StatusCode::OK,
            Json(json!({"ok": true, "stored": true, "events": events})),
        ),
        Ingested::Ignored => (StatusCode::OK, Json(json!({"ok": true, "stored": false}))),
    })
}

async fn get_health(State(st): State<AppState>) -> Json<Value> {
    Json(st.system_health.view(Utc::now()))
}

pub fn store(data_dir: &Path) -> Arc<HealthStore> {
    Arc::new(HealthStore::open(data_dir))
}

/// Behind `require_device` (the app).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/system/health", get(get_health))
}

/// Behind `require_service` (the sentinel).
pub fn service_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/v1/system/health",
            post(post_snapshot).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::service::require_service,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str, state: State3) -> Check {
        Check {
            id: id.into(),
            kind: Kind::Unit,
            name: id.trim_start_matches("unit:").into(),
            group: Group::Lucina,
            state,
            detail: "active".into(),
            since: "2026-10-27T09:00:00Z".into(),
            restarts_1h: 0,
        }
    }

    fn snap(at: &str, checks: Vec<Check>) -> Snapshot {
        Snapshot {
            host: "nezopt".into(),
            at: at.into(),
            overall: State3::Ok,
            metrics: Metrics {
                disks: vec![Disk {
                    mount: "/".into(),
                    used_pct: 59.1,
                    free_gb: 736.0,
                }],
                cpu_temp_c: None,
                load: [1.2, 1.4, 1.6],
                mem_used_pct: 62.0,
                swap_used_pct: Some(10.0),
            },
            checks,
        }
    }

    #[test]
    fn diff_only_changed_checks_present_in_both() {
        let a = snap(
            "2026-10-27T10:00:00Z",
            vec![check("unit:a", State3::Ok), check("unit:b", State3::Warn)],
        );
        let b = snap(
            "2026-10-27T10:05:00Z",
            vec![
                check("unit:a", State3::Crit),
                check("unit:b", State3::Warn),
                check("unit:new", State3::Crit),
            ],
        );
        let ev = diff(&a, &b, time("at", &b.at).unwrap());
        assert_eq!(ev.len(), 1);
        assert_eq!(
            (ev[0].check_id.as_str(), ev[0].from, ev[0].to),
            ("unit:a", State3::Ok, State3::Crit)
        );
    }

    #[test]
    fn validation() {
        let ok = snap("2026-10-27T10:00:00Z", vec![check("unit:a", State3::Ok)]);
        assert!(validate(&ok).is_ok());
        let mut s = ok.clone();
        s.host = "bad host".into();
        assert!(validate(&s).is_err());
        let mut s = ok.clone();
        s.at = "yesterday".into();
        assert!(validate(&s).is_err());
        let mut s = ok.clone();
        s.metrics.mem_used_pct = 101.0;
        assert!(validate(&s).is_err());
        let mut s = ok.clone();
        s.metrics.load[0] = f64::NAN;
        assert!(validate(&s).is_err());
        let mut s = ok.clone();
        s.checks.push(check("unit:a", State3::Ok));
        assert!(validate(&s).is_err());
    }

    #[test]
    fn retention_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let store = HealthStore::open(dir.path());
        let now = time("now", "2026-10-27T10:00:00Z").unwrap();
        let old = Event {
            at: now - Duration::days(31),
            host: "nezopt".into(),
            check_id: "unit:x".into(),
            name: "x".into(),
            from: State3::Ok,
            to: State3::Crit,
            detail: String::new(),
        };
        store.file.update(|s| s.events.push(old)).unwrap();
        let a = snap("2026-10-27T10:00:00Z", vec![check("unit:a", State3::Ok)]);
        let at = validate(&a).unwrap();
        assert_eq!(
            store.ingest(a.clone(), at, now).unwrap(),
            Ingested::Stored { events: 0 }
        );
        assert!(store.file.read(|s| s.events.is_empty()));
        assert_eq!(store.ingest(a, at, now).unwrap(), Ingested::Ignored);
    }
}

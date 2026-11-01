//! The ops record (crate `lucina_ops`): `/api/secretary/ops/*`, its
//! background loop, Lucina's own incidents and the `lucina-rs ops` CLI.
//!
//! - `POST /ops/records`      a NewRecord → 201 `Recorded` (200 when it was a duplicate)
//! - `GET  /ops/records/{id}` one record
//! - `GET  /ops/search?q=&project=&kind=&since=&until=&limit=`
//! - `PUT  /ops/page`         `{path, content}`: a whole `Map/` page (the project-map session)
//!
//! - `GET  /ops/ingest`        the Obsidian ingest's status (queue depth, last run, next try)
//! - `POST /ops/ingest`        `{cold?}`: ingest now (in the background) → 202, 409 when running
//!
//! The reporter is `x-lucina-caller` (`mcp:claude`, ...), set by secretary-server.
//! On when `LUCINA_OPS_VAULT_DIR` names the synced folder; every 10 minutes the
//! server sends records Honcho has not got yet (the CLI never calls Honcho) and
//! moves Syncthing conflict copies aside.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::Response;
use chrono::NaiveDate;
use chrono_tz::Tz;
use lucina_memory::{HonchoClient, HonchoConfig, MemoryMode};
use lucina_ops::{Kind, NewRecord, Ops, OpsConfig, OpsError, SearchQuery};
use serde_json::{json, Value};

use crate::api::{ok, ok_status, query_param, read_json, ApiError};
use crate::AppState;

/// Default synced folder on nezopt (the CLI's default; the server needs the env).
pub const DEFAULT_VAULT_DIR: &str = "/home/nez/data/lucina-vault/Lucina";
const RECORD_BODY_BYTES: usize = 256 * 1024;
const PAGE_BODY_BYTES: usize = lucina_ops::vault::MAX_PAGE_BYTES + 16 * 1024;
const LOOP_EVERY: Duration = Duration::from_secs(600);
const INGEST_EVERY: Duration = Duration::from_secs(300);

/// `LUCINA_OPS_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsSettings {
    /// `LUCINA_OPS_VAULT_DIR`; `None`: the ops record is off.
    pub vault_dir: Option<PathBuf>,
    /// `LUCINA_OPS_STATE_DIR` (default `<data>/ops`).
    pub state_dir: PathBuf,
    pub honcho_session: String,
    pub honcho_peer: String,
    /// `LUCINA_VAULT_INGEST=on` and its `LUCINA_VAULT_INGEST_*` settings
    /// (crate `lucina_vault_ingest`; the time zone is set at open); `None`: off.
    pub ingest: Option<lucina_vault_ingest::IngestConfig>,
}

impl OpsSettings {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let non_empty = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let state_dir = non_empty("LUCINA_OPS_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("ops"));
        let ingest = match non_empty("LUCINA_VAULT_INGEST").as_deref() {
            Some("on" | "true" | "1") => match lucina_vault_ingest::IngestConfig::from_lookup(
                &get,
                &state_dir.join("ingest"),
                chrono_tz::Europe::Rome,
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "vault ingest: off (bad settings)");
                    None
                }
            },
            _ => None,
        };
        Self {
            vault_dir: non_empty("LUCINA_OPS_VAULT_DIR").map(PathBuf::from),
            state_dir,
            honcho_session: non_empty("LUCINA_OPS_HONCHO_SESSION")
                .unwrap_or_else(|| lucina_ops::DEFAULT_HONCHO_SESSION.into()),
            honcho_peer: non_empty("LUCINA_OPS_HONCHO_PEER")
                .unwrap_or_else(|| lucina_ops::DEFAULT_HONCHO_PEER.into()),
            ingest,
        }
    }
}

/// Open the ops record for the server: Honcho as Lucina's memory is set
/// (writes only in `readwrite`, reads in `readonly` too). A vault that cannot
/// be opened turns the record off with a warning (never a failed start).
pub fn open(
    s: &OpsSettings,
    time_zone: Tz,
    mode: MemoryMode,
    honcho: &HonchoConfig,
) -> Option<Arc<Ops>> {
    let vault = s.vault_dir.as_ref()?;
    let client = match mode {
        MemoryMode::Off => None,
        _ => match HonchoClient::new(honcho.clone()) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "ops: Honcho client unusable; records stay local");
                None
            }
        },
    };
    let mut cfg = OpsConfig::new(vault, &s.state_dir, time_zone);
    cfg.honcho_session = s.honcho_session.clone();
    cfg.honcho_peer = s.honcho_peer.clone();
    cfg.honcho_write = mode == MemoryMode::ReadWrite;
    match Ops::open(cfg, client) {
        Ok(o) => {
            tracing::info!(vault = %vault.display(), "ops record: on");
            Some(Arc::new(o))
        }
        Err(e) => {
            tracing::warn!(vault = %vault.display(), error = %e, "ops record: off (vault or store not usable)");
            None
        }
    }
}

/// The Obsidian ingest over the ops record (needs the record on).
pub fn open_ingest(
    s: &OpsSettings,
    time_zone: Tz,
    ops: Option<&Arc<Ops>>,
) -> Option<Arc<lucina_vault_ingest::Ingest>> {
    let mut cfg = s.ingest.clone()?;
    let Some(ops) = ops else {
        tracing::warn!("vault ingest: off (the ops record is off)");
        return None;
    };
    cfg.time_zone = time_zone;
    match lucina_vault_ingest::Ingest::open(cfg, ops.clone()) {
        Ok(i) => {
            tracing::info!(state = %i.config().state_dir.display(), model = %i.config().model, "vault ingest: on");
            Some(Arc::new(i))
        }
        Err(e) => {
            tracing::warn!(error = %e, "vault ingest: off");
            None
        }
    }
}

fn api_error(e: OpsError) -> ApiError {
    match &e {
        OpsError::Io(_) => ApiError::new(500, "internal_error", e.to_string()),
        OpsError::NotFound => ApiError::new(404, "record_not_found", "No such record"),
        _ => ApiError::new(400, e.code(), e.to_string()),
    }
}

/// A JSON body as a NewRecord (unknown fields are an error naming them).
pub fn parse_record(v: Value) -> Result<NewRecord, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new(400, "bad_request", e.to_string()))
}

fn redacted(mut v: Value) -> Value {
    lucina_secrets::redactor().redact_json(&mut v);
    v
}

fn search_query(uri: &Uri) -> Result<SearchQuery, ApiError> {
    let date = |k: &'static str| -> Result<Option<NaiveDate>, ApiError> {
        match query_param(uri, k).filter(|v| !v.trim().is_empty()) {
            None => Ok(None),
            Some(v) => NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d")
                .map(Some)
                .map_err(|_| ApiError::bad_request(format!("{k} must be YYYY-MM-DD"))),
        }
    };
    let kind = match query_param(uri, "kind").filter(|v| !v.trim().is_empty()) {
        None => None,
        Some(k) => Some(
            serde_json::from_value::<Kind>(json!(k.trim())).map_err(|_| {
                ApiError::bad_request("kind must be deploy, incident, decision, status or finding")
            })?,
        ),
    };
    let limit = match query_param(uri, "limit").filter(|v| !v.trim().is_empty()) {
        None => None,
        Some(l) => Some(
            l.trim()
                .parse::<u32>()
                .map_err(|_| ApiError::bad_request("limit must be a number"))?,
        ),
    };
    Ok(SearchQuery {
        q: query_param(uri, "q").unwrap_or_default(),
        project: query_param(uri, "project"),
        kind,
        since: date("since")?,
        until: date("until")?,
        limit,
    })
}

/// `/api/secretary/ops/...` (`parts` after `ops`).
pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    let ops = st.ops.clone().ok_or_else(|| {
        ApiError::new(
            404,
            "ops_disabled",
            "The ops record is off (LUCINA_OPS_VAULT_DIR)",
        )
    })?;
    let only = |allowed: &[&str]| -> Result<(), ApiError> {
        if allowed.contains(&method.as_str()) {
            Ok(())
        } else {
            Err(ApiError::method_not_allowed(allowed))
        }
    };
    match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["records"] => {
            only(&["POST"])?;
            let new = parse_record(read_json(body, RECORD_BODY_BYTES).await?)?;
            let source = crate::computer::caller(headers);
            let r = ops.record(new, &source).await.map_err(api_error)?;
            let status = if r.duplicate { 200 } else { 201 };
            Ok(ok_status(status, redacted(json!(r))))
        }
        ["records", id] => {
            only(&["GET"])?;
            let r = ops
                .get(id)
                .map_err(api_error)?
                .ok_or_else(|| api_error(OpsError::NotFound))?;
            Ok(ok(redacted(json!(r))))
        }
        ["search"] => {
            only(&["GET"])?;
            let q = search_query(uri)?;
            let r = ops.search(q).await.map_err(api_error)?;
            Ok(ok(redacted(json!(r))))
        }
        ["page"] => {
            only(&["PUT"])?;
            let v = read_json(body, PAGE_BODY_BYTES).await?;
            let path = v.get("path").and_then(Value::as_str).unwrap_or("");
            let content = v
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::bad_request("content must be a string"))?;
            let changed = ops
                .vault()
                .write_page(path, content)
                .map_err(|e| api_error(e.into()))?;
            Ok(ok(json!({"path": path, "changed": changed})))
        }
        ["ingest"] => {
            only(&["GET", "POST"])?;
            let ingest = st.ingest.clone().ok_or_else(|| {
                ApiError::new(
                    404,
                    "ingest_disabled",
                    "The vault ingest is off (LUCINA_VAULT_INGEST)",
                )
            })?;
            if *method == Method::GET {
                let s = ingest
                    .status()
                    .map_err(|e| ApiError::new(500, "internal_error", e.to_string()))?;
                return Ok(ok(json!(s)));
            }
            let v = read_json(body, 4096).await.unwrap_or_else(|_| json!({}));
            let cold = v.get("cold").and_then(Value::as_bool).unwrap_or(true);
            if ingest.running() {
                return Ok(ok_status(
                    409,
                    json!({"started": false, "reason": "a run is in progress"}),
                ));
            }
            tokio::spawn(async move {
                if let Err(e) = ingest.scan(chrono::Utc::now()).await {
                    tracing::warn!(error = %e, "vault ingest: scan failed");
                }
                let opts = lucina_vault_ingest::RunOpts {
                    force: true,
                    cold,
                    limit: None,
                };
                if let Err(e) = ingest.run(opts, chrono::Utc::now()).await {
                    tracing::warn!(error = %e, "vault ingest: manual run failed");
                }
            });
            Ok(ok_status(202, json!({"started": true, "cold": cold})))
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Every 5 minutes (from a minute after start): queue what is new, then
/// ingest while the local Qwen is free (the crate backs off on its own).
pub fn spawn_ingest(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    let ingest = st.ingest.clone()?;
    Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut every = tokio::time::interval(INGEST_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            if let Err(e) = ingest.tick().await {
                tracing::warn!(error = %e, "vault ingest: tick failed");
            }
        }
    }))
}

/// Every 10 minutes (and at start): Honcho catch-up, then the conflict sweep.
/// A failing tick is logged; the loop never ends.
pub fn spawn(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    let ops = st.ops.clone()?;
    Some(tokio::spawn(async move {
        let mut every = tokio::time::interval(LOOP_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            match ops.sync_pending().await {
                Ok(r) if r.attempted > 0 => {
                    tracing::info!(synced = r.synced, failed = r.failed, "ops: Honcho catch-up")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "ops: Honcho catch-up failed"),
            }
            match ops.sweep_conflicts() {
                Ok(r) if !r.moved.is_empty() => {
                    tracing::info!(
                        moved = r.moved.len(),
                        reinserted = r.reinserted,
                        "ops: sync conflicts moved aside"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "ops: conflict sweep failed"),
            }
        }
    }))
}

/// The record for a delivery-path alert: one incident per path and day, and
/// a status line when it works again.
pub fn delivery_record(path: &str, down: bool, text: &str, local_date: NaiveDate) -> NewRecord {
    let v = if down {
        json!({
            "kind": "incident",
            "project": "lucina",
            "title": format!("Delivery path {path} is down"),
            "body": text,
            "host": "nezopt",
            "severity": "medium",
            "dedup_key": format!("delivery:{path}:{local_date}"),
        })
    } else {
        json!({
            "kind": "status",
            "project": "lucina",
            "title": format!("Delivery path {path} works again"),
            "body": text,
            "host": "nezopt",
            "dedup_key": format!("delivery:{path}:up:{local_date}"),
        })
    };
    serde_json::from_value(v).expect("delivery record fields are valid")
}

/// Lucina records her own delivery incidents (fire and forget).
pub fn delivery_alert(st: &AppState, path: &str, down: bool, text: &str) {
    let Some(ops) = st.ops.clone() else {
        return;
    };
    let date = chrono::Utc::now()
        .with_timezone(&st.cfg.time_zone)
        .date_naive();
    let rec = delivery_record(path, down, text, date);
    tokio::spawn(async move {
        if let Err(e) = ops.record(rec, "lucina").await {
            tracing::warn!(error = %e, "ops: delivery incident not recorded");
        }
    });
}

// ---------------------------------------------------------------- CLI

/// The CLI's ops record: no Honcho (the server's loop sends the records).
pub fn open_cli(
    vault: Option<PathBuf>,
    state: Option<PathBuf>,
    tz: Option<String>,
) -> anyhow::Result<Ops> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let vault = vault
        .or_else(|| env("LUCINA_OPS_VAULT_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_VAULT_DIR));
    let state = state
        .or_else(|| env("LUCINA_OPS_STATE_DIR").map(PathBuf::from))
        .or_else(|| env("LUCINA_DATA_DIR").map(|d| Path::new(&d).join("ops")))
        .unwrap_or_else(|| PathBuf::from("/home/nez/Projects/Lucina/data/ops"));
    let tz_name = tz
        .or_else(|| env("LUCINA_REMINDER_TIMEZONE"))
        .or_else(|| env("LUCINA_TIMEZONE"))
        .unwrap_or_else(|| "Europe/Rome".into());
    let tz: Tz = tz_name
        .parse()
        .map_err(|_| anyhow::anyhow!("unknown time zone {tz_name}"))?;
    let mut cfg = OpsConfig::new(vault, state, tz);
    cfg.honcho_write = false;
    Ok(Ops::open(cfg, None)?)
}

/// Records from stdin: one JSON object, an array, or JSON lines.
pub fn parse_records_input(text: &str) -> anyhow::Result<Vec<Value>> {
    let t = text.trim();
    if t.is_empty() {
        anyhow::bail!("no record on stdin");
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return match v {
            Value::Array(items) => Ok(items),
            other => Ok(vec![other]),
        };
    }
    t.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings() {
        let s = OpsSettings::from_lookup(|_| None, Path::new("/d"));
        assert_eq!(s.vault_dir, None);
        assert_eq!(s.state_dir, PathBuf::from("/d/ops"));
        assert_eq!(s.honcho_session, "lucina_ops");
        let s = OpsSettings::from_lookup(
            |k| (k == "LUCINA_OPS_VAULT_DIR").then(|| " /v ".to_owned()),
            Path::new("/d"),
        );
        assert_eq!(s.vault_dir, Some(PathBuf::from("/v")));
    }

    #[test]
    fn delivery_records() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let down = serde_json::to_value(delivery_record("telegram", true, "down", d)).unwrap();
        assert_eq!(down["kind"], "incident");
        assert_eq!(down["dedup_key"], "delivery:telegram:2026-10-28");
        let up = serde_json::to_value(delivery_record("telegram", false, "up", d)).unwrap();
        assert_eq!(up["kind"], "status");
    }

    #[test]
    fn stdin_shapes() {
        assert_eq!(parse_records_input(r#"{"a":1}"#).unwrap().len(), 1);
        assert_eq!(
            parse_records_input(r#"[{"a":1},{"a":2}]"#).unwrap().len(),
            2
        );
        assert_eq!(
            parse_records_input("{\"a\":1}\n\n{\"a\":2}\n")
                .unwrap()
                .len(),
            2
        );
        assert!(parse_records_input("").is_err());
        assert!(parse_records_input("{\"a\":1}\nnope").is_err());
    }
}

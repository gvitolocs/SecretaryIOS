//! The Homelab catalog (nezopt `~/Projects/Homelab/catalog`): what runs where, what depends
//! on it and whether it is healthy. The `catalog` CLI owns the data and the logic; these
//! routes run it and pass its JSON through, and the tree adds this daemon's agents and
//! workspaces to each project (matched by cwd against the project's checkouts and worktrees).
//!
//! | Method | Path | Query | Returns |
//! |---|---|---|---|
//! | GET | `/catalog` | | the tree: `{generated_at, hosts, drift, counts, groups: [{name, level, projects: [{id, level, paths, components, agents, workspaces}]}]}` |
//! | GET | `/catalog/status` | | what is not healthy, worst first |
//! | GET | `/catalog/components/{id}` | | one component with health, dependents, last deploy |
//! | GET | `/catalog/impact/{ids}` | comma-separated ids | `{down, broken, degraded, public_hostnames_broken}` |
//! | GET | `/catalog/health` | `?id=&live=true` | the last scan, or rescan that component's host first (`live` needs `id`) |
//! | GET | `/catalog/drift` | | `{unknown, missing, skipped_hosts, discovered}` |
//! | GET | `/catalog/graph` | | `{nodes, edges}` |
//! | GET | `/catalog/events` | `?limit=` | health flips, newest last |
//!
//! Errors: `CATALOG_UNAVAILABLE` (502) when the CLI is missing or fails, `CATALOG_NOT_FOUND`
//! (404) for an unknown id, `CATALOG_BUSY` (409) while another live scan runs.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiResult};
use crate::manager::Manager;

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/catalog", get(tree))
        .route("/catalog/status", get(status))
        .route("/catalog/components/{id}", get(component))
        .route("/catalog/impact/{ids}", get(impact))
        .route("/catalog/health", get(health))
        .route("/catalog/drift", get(drift))
        .route("/catalog/graph", get(graph))
        .route("/catalog/events", get(events))
}

/// `LUCINA_CATALOG_BIN`, else `~/.local/bin/catalog`.
fn bin() -> String {
    std::env::var("LUCINA_CATALOG_BIN")
        .unwrap_or_else(|_| format!("{}/.local/bin/catalog", std::env::var("HOME").unwrap_or_default()))
}

/// One live rescan at a time (it SSHes to a host).
static LIVE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
}

async fn run(args: &[&str], timeout: Duration) -> ApiResult {
    let unavailable = |m: String| ApiError(StatusCode::BAD_GATEWAY, "CATALOG_UNAVAILABLE".into(), m);
    let mut cmd = tokio::process::Command::new(bin());
    cmd.arg("--json").args(args).kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| unavailable(format!("catalog {} timed out", args.join(" "))))?
        .map_err(|e| unavailable(format!("catalog: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_owned();
        if msg.contains("no component") {
            return Err(ApiError(StatusCode::NOT_FOUND, "CATALOG_NOT_FOUND".into(), msg));
        }
        return Err(unavailable(msg));
    }
    serde_json::from_slice(&out.stdout).map(Json).map_err(|e| unavailable(format!("catalog output: {e}")))
}

const QUICK: Duration = Duration::from_secs(20);

fn under(cwd: &str, path: &str) -> bool {
    cwd == path || cwd.strip_prefix(path).is_some_and(|rest| rest.starts_with('/'))
}

/// The project whose checkout (or worktree) holds `cwd`; the longest path wins.
fn project_of<'a>(cwd: &str, paths: &'a [(String, String)]) -> Option<&'a str> {
    paths.iter().filter(|(p, _)| under(cwd, p)).max_by_key(|(p, _)| p.len()).map(|(_, id)| id.as_str())
}

async fn tree(State(m): State<Manager>) -> ApiResult {
    let Json(mut t) = run(&["tree"], QUICK).await?;
    let mut paths: Vec<(String, String)> = Vec::new();
    for g in t["groups"].as_array().into_iter().flatten() {
        for p in g["projects"].as_array().into_iter().flatten() {
            let id = p["id"].as_str().unwrap_or_default().to_owned();
            for path in p["paths"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                paths.push((path.to_owned(), id.clone()));
            }
        }
    }
    let mut agents: std::collections::HashMap<String, Vec<Value>> = Default::default();
    for a in m.list(false) {
        if let Some(pid) = project_of(&a.cwd, &paths) {
            agents.entry(pid.to_owned()).or_default().push(json!({
                "id": a.id, "shortId": a.short_id, "title": a.title, "provider": a.provider,
                "status": a.status, "cwd": a.cwd, "updatedAt": a.updated_at,
                "requiresAttention": a.requires_attention,
            }));
        }
    }
    let mut workspaces: std::collections::HashMap<String, Vec<Value>> = Default::default();
    for w in m.workspaces().list(false) {
        if w.archived_at.is_some() {
            continue;
        }
        if let Some(pid) = project_of(&w.cwd, &paths) {
            workspaces.entry(pid.to_owned()).or_default().push(json!({
                "workspaceId": w.workspace_id, "name": w.name(), "branch": w.branch, "cwd": w.cwd,
            }));
        }
    }
    for g in t["groups"].as_array_mut().into_iter().flatten() {
        for p in g["projects"].as_array_mut().into_iter().flatten() {
            let id = p["id"].as_str().unwrap_or_default().to_owned();
            p["agents"] = json!(agents.remove(&id).unwrap_or_default());
            p["workspaces"] = json!(workspaces.remove(&id).unwrap_or_default());
        }
    }
    Ok(Json(t))
}

async fn status() -> ApiResult {
    run(&["status"], QUICK).await
}

async fn component(Path(id): Path<String>) -> ApiResult {
    if !valid_id(&id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "CATALOG_BAD_ID".into(), format!("bad id `{id}`")));
    }
    run(&["get", &id], QUICK).await
}

async fn impact(Path(ids): Path<String>) -> ApiResult {
    let ids: Vec<&str> = ids.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if ids.is_empty() || ids.len() > 20 || !ids.iter().all(|i| valid_id(i)) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "CATALOG_BAD_ID".into(), "1-20 comma-separated component ids".into()));
    }
    let mut args = vec!["impact"];
    args.extend(ids);
    run(&args, QUICK).await
}

#[derive(Deserialize, Default)]
struct HealthQuery {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    live: bool,
}

async fn health(Query(q): Query<HealthQuery>) -> ApiResult {
    let mut args = vec!["health"];
    if let Some(id) = q.id.as_deref() {
        if !valid_id(id) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "CATALOG_BAD_ID".into(), format!("bad id `{id}`")));
        }
        args.push(id);
    }
    if !q.live {
        return run(&args, QUICK).await;
    }
    if q.id.is_none() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "CATALOG_BAD_ID".into(), "live needs an id (one host is rescanned)".into()));
    }
    let Ok(_guard) = LIVE.try_lock() else {
        return Err(ApiError(StatusCode::CONFLICT, "CATALOG_BUSY".into(), "a live scan is already running".into()));
    };
    args.push("--live");
    run(&args, Duration::from_secs(150)).await
}

async fn drift() -> ApiResult {
    run(&["drift"], QUICK).await
}

async fn graph() -> ApiResult {
    run(&["graph"], QUICK).await
}

#[derive(Deserialize, Default)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<u32>,
}

async fn events(Query(q): Query<EventsQuery>) -> ApiResult {
    let limit = q.limit.unwrap_or(50).clamp(1, 500).to_string();
    run(&["events", "--limit", &limit], QUICK).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_goes_to_the_most_specific_project() {
        let paths = vec![
            ("/home/nez/Projects/Lucina/SecretaryIOS".to_string(), "lucina".to_string()),
            ("/home/nez/Projects/.worktrees/SecretaryIOS/catalog-map".to_string(), "lucina".to_string()),
            ("/home/nez/Projects/AI/3dLora".to_string(), "3dlora".to_string()),
            ("/home/nez/Projects/AI/3dLora/ComfyUI".to_string(), "comfy".to_string()),
        ];
        assert_eq!(project_of("/home/nez/Projects/.worktrees/SecretaryIOS/catalog-map/crates", &paths), Some("lucina"));
        assert_eq!(project_of("/home/nez/Projects/Lucina/SecretaryIOS", &paths), Some("lucina"));
        assert_eq!(project_of("/home/nez/Projects/Lucina/SecretaryIOS-old", &paths), None, "a sibling with the same prefix is not inside");
        assert_eq!(project_of("/home/nez/Projects/AI/3dLora/ComfyUI/x", &paths), Some("comfy"));
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("pokoin-db-writer"));
        assert!(valid_id("codevira@cardvault"));
        assert!(!valid_id("../etc"));
        assert!(!valid_id("a b"));
        assert!(!valid_id(""));
    }
}

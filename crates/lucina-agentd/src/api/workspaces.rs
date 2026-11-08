//! Projects, workspaces, diffs, files and checkout operations.
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | GET/POST | `/projects` | `{path}` | `{projects}` / project |
//! | PATCH/DELETE | `/projects/{id}` | `{name}` (null resets) | project / `{projectId, removedWorkspaceIds}` |
//! | POST | `/projects/{id}/move` | `{path, name}` | `{project, moved, skipped}` (the checkout moved on disk) |
//! | GET/POST | `/workspaces` | `?background=true` / create request | `{workspaces}` / `{workspace, setupSkippedReason}` |
//! | GET/PATCH | `/workspaces/{id}` | `{title, pinned}` | workspace |
//! | POST | `/workspaces/{id}/labels` | `{label, assigned}` | workspace |
//! | POST | `/workspaces/{id}/archive` | | `{workspaceId, archivedAt}` |
//! | POST | `/workspaces/{id}/setup` | | `{workspaceId, status}` |
//! | POST | `/workspaces/sweep` | | `{removed: [{workspaceId, reason}]}` |
//! | GET | `/workspaces/{id}/diff` | `?mode=uncommitted\|base\|all&baseRef=&ignoreWhitespace=` | `{cwd, files, diffTooLarge, error}` |
//! | GET | `/workspaces/{id}/status` | | checkout status |
//! | GET | `/workspaces/{id}/files` | `?path=` | `{path, entries}` |
//! | GET/PUT | `/workspaces/{id}/file` | `?path=&maxBytes=&acceptBinary=` / `{path, content, expectedRevision}` | file |
//! | POST | `/workspaces/{id}/download-token` | `{path}` | `{token, url}` |
//! | GET | `/files/download` | `?token=` | the file (one use, 60 s) |
//! | POST | `/workspaces/{id}/commit` | `{message, addAll}` | `{sha}` |
//! | POST | `/workspaces/{id}/push` | | `{branch}` |
//! | GET/POST | `/workspaces/{id}/pr` | `{title, body, baseRef}` | PR status / `{url}` |
//! | POST | `/clone` | `{repo, dir, protocol}` | `{repo, checkoutPath, project}` |
//!
//! Pushing, opening a PR, allowing an untrusted workspace's scripts, archiving and
//! deleting a project need the owner (not an MCP token or an agent's process).

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_agents::permission_gate::Origin;
use lucina_agents::workspace::WorkspaceRecord;
use lucina_agents::worktree_guard::Trigger;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{to_json, ApiError, ApiResult};
use crate::manager::{Manager, ManagerError};
use crate::peer::{origin, Peer};
use crate::workspaces::{checkout, files, CreateWorkspace, WsError};

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/projects", get(projects).post(create_project))
        .route("/projects/{id}", axum::routing::patch(rename_project).delete(delete_project))
        .route("/projects/{id}/move", post(move_project))
        .route("/workspaces", get(list).post(create))
        .route("/workspaces/sweep", post(sweep))
        .route("/workspaces/{id}", get(inspect).patch(update))
        .route("/workspaces/{id}/labels", post(label))
        .route("/workspaces/{id}/archive", post(archive))
        .route("/workspaces/{id}/setup", post(setup))
        .route("/workspaces/{id}/diff", get(diff))
        .route("/workspaces/{id}/status", get(status))
        .route("/workspaces/{id}/files", get(list_files))
        .route("/workspaces/{id}/file", get(read_file).put(write_file))
        .route("/workspaces/{id}/download-token", post(download_token))
        .route("/files/download", get(download))
        .route("/workspaces/{id}/commit", post(commit))
        .route("/workspaces/{id}/push", post(push))
        .route("/workspaces/{id}/pr", get(pr_status).post(pr_create))
        .route("/clone", post(clone))
}

fn bad(code: &str, message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, code.into(), message.into())
}

fn owner_only(m: &Manager, peer: &Peer, headers: &HeaderMap, what: &str) -> Result<(), ApiError> {
    match origin(peer, headers, &m.provider_pids()) {
        Origin::Local | Origin::OwnerDevice => Ok(()),
        _ => Err(ApiError(StatusCode::FORBIDDEN, "OWNER_ONLY".into(), format!("Only the owner can {what}"))),
    }
}

/// A workspace as clients see it: the record plus its name, project and isolation.
pub fn view(m: &Manager, w: &WorkspaceRecord) -> Value {
    let mut v = serde_json::to_value(w).unwrap_or(Value::Null);
    v["name"] = json!(w.name());
    v["projectDisplayName"] = json!(m.workspaces().project_name(w));
    v["isolation"] = json!(if w.kind == lucina_agents::workspace::WorkspaceKind::Worktree { "worktree" } else { "local" });
    v
}

fn workspace(m: &Manager, id: &str) -> Result<WorkspaceRecord, ApiError> {
    m.workspaces().get(id).filter(|w| w.archived_at.is_none()).ok_or_else(|| ManagerError::from(WsError::NotFound(format!("Workspace not found: {id}"))).into())
}

fn project_view(p: &lucina_agents::workspace::ProjectRecord) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    v["name"] = json!(p.name());
    v
}

async fn projects(State(m): State<Manager>) -> ApiResult {
    to_json(json!({"projects": m.workspaces().projects().iter().map(project_view).collect::<Vec<_>>()}))
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

async fn create_project(State(m): State<Manager>, Json(b): Json<PathBody>) -> ApiResult {
    let p = m.workspaces().create_project(&b.path).await.map_err(ManagerError::from)?;
    to_json(project_view(&p))
}

#[derive(Deserialize)]
struct NameBody {
    #[serde(default)]
    name: Option<String>,
}

async fn rename_project(State(m): State<Manager>, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult {
    let p = m.workspaces().rename_project(&id, b.name.filter(|n| !n.trim().is_empty())).map_err(ManagerError::from)?;
    to_json(project_view(&p))
}

#[derive(Deserialize)]
struct MoveBody {
    path: String,
    #[serde(default)]
    name: Option<String>,
}

async fn move_project(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<MoveBody>) -> ApiResult {
    owner_only(&m, &peer, &headers, "move a project")?;
    to_json(m.move_project(&id, &b.path, b.name)?)
}

async fn delete_project(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    owner_only(&m, &peer, &headers, "delete a project")?;
    let ids = m.workspaces().delete_project(&id).map_err(ManagerError::from)?;
    for w in &ids {
        m.archive_workspace(w, Trigger::Archive, None).await?;
    }
    to_json(json!({"projectId": id, "removedWorkspaceIds": ids}))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    background: bool,
}

async fn list(State(m): State<Manager>, Query(q): Query<ListQuery>) -> ApiResult {
    let mut out = Vec::new();
    for w in m.workspaces().list(q.background) {
        let w = m.workspaces().refresh(&w.workspace_id).await.unwrap_or(w);
        out.push(view(&m, &w));
    }
    to_json(json!({"workspaces": out}))
}

async fn create(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(req): Json<CreateWorkspace>) -> ApiResult {
    let (mut w, skipped) = m.create_workspace(req).await?;
    // An MCP token's workspaces carry `mcp:<token>`: its scripts may run there.
    if let super::terminals::Principal::Mcp(token) = super::terminals::principal(&m, &peer, &headers) {
        w = m.workspaces().set_label(&w.workspace_id, &super::terminals::mcp_label(&token), true).map_err(ManagerError::from)?;
    }
    to_json(json!({"workspace": view(&m, &w), "setupSkippedReason": skipped}))
}

async fn inspect(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let w = m.workspaces().get(&id).ok_or_else(|| ManagerError::from(WsError::NotFound("Workspace not found".into())))?;
    to_json(view(&m, &w))
}

#[derive(Deserialize)]
struct UpdateBody {
    /// A string sets the title, null resets it; absent leaves it.
    #[serde(default, with = "serde_with_absent")]
    title: Option<Option<String>>,
    #[serde(default)]
    pinned: Option<bool>,
}

mod serde_with_absent {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
        Ok(Some(Option::<String>::deserialize(d)?))
    }
}

async fn update(State(m): State<Manager>, Path(id): Path<String>, Json(b): Json<UpdateBody>) -> ApiResult {
    let mut w = workspace(&m, &id)?;
    if let Some(title) = b.title {
        w = m.workspaces().rename(&id, title).map_err(ManagerError::from)?;
    }
    if let Some(p) = b.pinned {
        w = m.workspaces().set_pinned(&id, p).map_err(ManagerError::from)?;
    }
    to_json(view(&m, &w))
}

#[derive(Deserialize)]
struct LabelBody {
    label: String,
    assigned: bool,
}

async fn label(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<LabelBody>) -> ApiResult {
    workspace(&m, &id)?;
    // `mcp:<token>` gives a token the workspace's scripts: only the owner sets it by hand.
    if b.label.trim().to_lowercase().starts_with("mcp:") {
        owner_only(&m, &peer, &headers, "set mcp: labels")?;
    }
    let w = m.workspaces().set_label(&id, &b.label, b.assigned).map_err(ManagerError::from)?;
    to_json(view(&m, &w))
}

async fn archive(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    owner_only(&m, &peer, &headers, "archive a workspace")?;
    let w = m.archive_workspace(&id, Trigger::Archive, None).await?;
    to_json(json!({"workspaceId": id, "archivedAt": w.archived_at, "note": w.removed_reason}))
}

async fn setup(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    owner_only(&m, &peer, &headers, "allow and run a workspace's setup")?;
    let status = m.workspaces().run_setup(&id).map_err(ManagerError::from)?;
    to_json(json!({"workspaceId": id, "status": status}))
}

async fn sweep(State(m): State<Manager>) -> ApiResult {
    let removed: Vec<Value> = m.sweep().await.into_iter().map(|(id, reason)| json!({"workspaceId": id, "reason": reason})).collect();
    to_json(json!({"removed": removed}))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DiffQuery {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    ignore_whitespace: bool,
}

async fn diff(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<DiffQuery>) -> ApiResult {
    let w = workspace(&m, &id)?;
    let mode = q.mode.unwrap_or_else(|| "all".into());
    let dir = PathBuf::from(&w.cwd);
    match checkout::diff(&dir, &mode, q.base_ref.as_deref(), w.base_ref.as_deref(), q.ignore_whitespace).await {
        Ok(d) => to_json(json!({"cwd": w.cwd, "files": d.files, "diffTooLarge": d.too_large, "error": null})),
        Err(e) => Err(bad("DIFF_FAILED", e)),
    }
}

async fn status(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let w = workspace(&m, &id)?;
    to_json(checkout::status(std::path::Path::new(&w.cwd), w.base_ref.as_deref(), w.owned_worktree).await)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FileQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    accept_binary: bool,
}

async fn list_files(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<FileQuery>) -> ApiResult {
    let w = workspace(&m, &id)?;
    let rel = q.path.unwrap_or_default();
    let rel = if rel == "." { String::new() } else { rel };
    let (path, entries) = files::list(std::path::Path::new(&w.cwd), &rel).map_err(|e| bad("FILES_FAILED", e))?;
    to_json(json!({"path": path, "entries": entries}))
}

async fn read_file(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<FileQuery>) -> ApiResult {
    let w = workspace(&m, &id)?;
    let rel = q.path.ok_or_else(|| bad("MISSING_PATH", "path is required"))?;
    to_json(files::read(std::path::Path::new(&w.cwd), &rel, q.max_bytes, q.accept_binary).map_err(|e| bad("FILES_FAILED", e))?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteBody {
    path: String,
    content: String,
    #[serde(default)]
    expected_revision: Option<String>,
}

async fn write_file(State(m): State<Manager>, Path(id): Path<String>, Json(b): Json<WriteBody>) -> ApiResult {
    let w = workspace(&m, &id)?;
    to_json(files::write(std::path::Path::new(&w.cwd), &b.path, &b.content, b.expected_revision.as_deref()).map_err(|e| bad("FILES_FAILED", e))?)
}

async fn download_token(State(m): State<Manager>, Path(id): Path<String>, Json(b): Json<PathBody>) -> ApiResult {
    let w = workspace(&m, &id)?;
    let token = m.workspaces().tokens().create(std::path::Path::new(&w.cwd), &b.path).map_err(|e| bad("FILES_FAILED", e))?;
    to_json(json!({"token": token, "url": format!("/files/download?token={token}"), "expiresInSec": 60}))
}

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}

async fn download(State(m): State<Manager>, Query(q): Query<TokenQuery>) -> Response {
    let Some(path) = m.workspaces().tokens().take(&q.token) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "TOKEN_INVALID", "message": "Unknown or used download token"}}))).into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let name = path.file_name().map(|n| n.to_string_lossy().replace('"', "")).unwrap_or_default();
            (
                [(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "FILES_FAILED", "message": e.to_string()}}))).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitBody {
    message: String,
    #[serde(default)]
    add_all: bool,
    /// `{name, email}` for this commit.
    #[serde(default)]
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    name: String,
    email: String,
}

async fn commit(State(m): State<Manager>, Path(id): Path<String>, Json(b): Json<CommitBody>) -> ApiResult {
    let w = workspace(&m, &id)?;
    let sha = checkout::commit(std::path::Path::new(&w.cwd), &b.message, b.add_all, b.author.as_ref().map(|a| (a.name.as_str(), a.email.as_str()))).await.map_err(|e| bad("COMMIT_FAILED", e))?;
    to_json(json!({"workspaceId": id, "sha": sha}))
}

async fn push(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    owner_only(&m, &peer, &headers, "push")?;
    let w = workspace(&m, &id)?;
    let branch = checkout::push(std::path::Path::new(&w.cwd)).await.map_err(|e| bad("PUSH_FAILED", e))?;
    to_json(json!({"workspaceId": id, "branch": branch}))
}

async fn pr_status(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let w = workspace(&m, &id)?;
    to_json(checkout::pr_status(std::path::Path::new(&w.cwd)).await.map_err(|e| bad("PR_FAILED", e))?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrBody {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    base_ref: Option<String>,
}

async fn pr_create(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<PrBody>) -> ApiResult {
    owner_only(&m, &peer, &headers, "open a pull request")?;
    let w = workspace(&m, &id)?;
    to_json(checkout::pr_create(std::path::Path::new(&w.cwd), &b.title, &b.body, b.base_ref.as_deref()).await.map_err(|e| bad("PR_FAILED", e))?)
}

#[derive(Deserialize)]
struct CloneBody {
    repo: String,
    dir: String,
    #[serde(default)]
    protocol: Option<String>,
}

/// `owner/repo` or a URL into a clone URL.
fn clone_url(repo: &str, protocol: Option<&str>) -> Result<(String, String), String> {
    let repo = repo.trim().trim_end_matches('/');
    let name = repo.rsplit(['/', ':']).next().unwrap_or(repo).trim_end_matches(".git").to_string();
    if name.is_empty() || name.starts_with('.') || name.contains("..") {
        return Err(format!("Invalid repository: {repo}"));
    }
    if repo.contains("://") || repo.starts_with("git@") {
        return Ok((repo.to_string(), name));
    }
    let (owner, rname) = repo.split_once('/').ok_or_else(|| format!("Expected owner/repo or a URL, got {repo}"))?;
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok(owner) || !ok(rname) {
        return Err(format!("Invalid repository: {repo}"));
    }
    Ok(match protocol {
        Some("ssh") => (format!("git@github.com:{owner}/{rname}.git"), name),
        _ => (format!("https://github.com/{owner}/{rname}.git"), name),
    })
}

async fn clone(State(m): State<Manager>, Json(b): Json<CloneBody>) -> ApiResult {
    let (url, name) = clone_url(&b.repo, b.protocol.as_deref()).map_err(|e| bad("CLONE_FAILED", e))?;
    let parent = PathBuf::from(&b.dir);
    if !parent.is_dir() {
        return Err(bad("CLONE_FAILED", format!("Not a directory: {}", b.dir)));
    }
    let dest = parent.join(&name);
    if dest.exists() {
        return Err(bad("CLONE_FAILED", format!("{} already exists", dest.display())));
    }
    let out = tokio::process::Command::new("git")
        .args(["clone", "--", &url])
        .arg(&dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| bad("CLONE_FAILED", e.to_string()))?;
    if !out.status.success() {
        return Err(bad("CLONE_FAILED", String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let project = m.workspaces().create_project(&dest.display().to_string()).await.map_err(ManagerError::from)?;
    to_json(json!({"repo": b.repo, "checkoutPath": dest.display().to_string(), "project": project_view(&project)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_urls() {
        assert_eq!(clone_url("gvitolocs/pokoin", None).unwrap(), ("https://github.com/gvitolocs/pokoin.git".into(), "pokoin".into()));
        assert_eq!(clone_url("gvitolocs/pokoin", Some("ssh")).unwrap().0, "git@github.com:gvitolocs/pokoin.git");
        assert_eq!(clone_url("https://example.com/a/b.git", None).unwrap().1, "b");
        assert!(clone_url("../../etc", None).is_err());
        assert!(clone_url("a b/c", None).is_err());
    }
}

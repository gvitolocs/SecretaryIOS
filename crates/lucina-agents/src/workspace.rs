//! Projects, workspaces, diffs and the file explorer (Paseo `ProjectRecord`,
//! `WorkspaceRecord`, `ParsedDiffFileSchema`, `FileExplorer*Schema`; design:
//! "Workspaces and worktrees").

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use crate::worktree_guard::Kind as WorkspaceKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    /// `prj_` + 16 hex.
    pub project_id: String,
    pub root_path: String,
    /// `git` or `non_git`.
    pub kind: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
}

impl ProjectRecord {
    pub fn name(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.display_name)
    }
}

/// One `paseo.json` setup command's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCommand {
    pub index: usize,
    pub command: String,
    pub cwd: String,
    pub log: String,
    /// `running`, `completed`, `failed`, `skipped`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Paseo `workspace_setup_progress` (the `worktree_setup` detail).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupSnapshot {
    /// `none` (nothing to run), `running`, `completed`, `failed`, `blocked`.
    pub status: String,
    #[serde(default)]
    pub commands: Vec<SetupCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A worktree made from a fork's change request: its scripts don't run until the owner
/// allows them (`workspace setup`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntrustedSource {
    pub kind: String,
    pub forge: String,
    pub number: u64,
    pub head_repository: String,
}

/// Paseo `firstAgentBranchAutoName`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoName {
    /// `pending` or `attempted`.
    pub status: String,
    pub placeholder_branch_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    /// `wks_` + 16 hex.
    pub workspace_id: String,
    pub project_id: String,
    pub cwd: String,
    pub kind: WorkspaceKind,
    /// The branch, else the last path segment.
    pub display_name: String,
    /// The owner's title (Paseo "Model B": it names the workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The ref a worktree was made from (e.g. `refs/heads/lucina-rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// The commit the worktree started at: a branch still there has no work to lose,
    /// but the merge sweep only removes branches that moved past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_repo_root: Option<String>,
    /// agentd made this worktree (with `wt add`): it may remove it.
    #[serde(default)]
    pub owned_worktree: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untrusted_source: Option<UntrustedSource>,
    #[serde(default)]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupSnapshot>,
    /// `PASEO_WORKTREE_PORT`, allocated once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_name: Option<AutoName>,
    /// Why agentd removed it (`merged into lucina-rs`), when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_reason: Option<String>,
}

impl WorkspaceRecord {
    pub fn name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.display_name)
    }

    /// Paseo `toWorkspaceRow` (`workspace ls/create --json`).
    pub fn row(&self, project: &str) -> Value {
        serde_json::json!({
            "workspaceId": self.workspace_id,
            "project": project,
            "name": self.name(),
            "isolation": if self.kind == WorkspaceKind::Worktree { "worktree" } else { "local" },
            "cwd": self.cwd,
            "background": self.background,
        })
    }
}

/// Paseo `DiffLineSchema` (without syntax tokens).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// `add`, `remove`, `context`, `header`.
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub additions: u64,
    pub deletions: u64,
    pub hunks: Vec<DiffHunk>,
    /// `ok`, `too_large` or `binary`.
    pub status: String,
}

/// Paseo `FileExplorerEntrySchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    /// `file` or `directory`.
    pub kind: String,
    pub size: u64,
    pub modified_at: String,
}

/// Paseo `FileExplorerFileSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub path: String,
    /// `text`, `image` or `binary`.
    pub kind: String,
    /// `utf-8`, `base64` or `none`.
    pub encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub size: u64,
    pub modified_at: String,
    /// `dev:ino:size:mtime_ns`, for optimistic writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// Extra project config (`paseo.json`); unknown keys pass through.
pub type ProjectConfig = Map<String, Value>;

//! `lagent project …`, `lagent workspace …`, `lagent clone`, and how `lagent run`
//! picks its workspace (Paseo `commands/project/*`, `commands/workspace/*`,
//! `commands/clone.js`, `run.js resolveRunWorkspace`). Outputs and error codes are
//! Paseo's; `workspace diff|files|cat|status|sweep` are Lucina's.

use serde_json::{json, Value};

use super::{single, table, CmdError, Output, ProjectCommand, RunArgs, WorkspaceCommand, WorktreeArgs};
use crate::client::{Client, ClientError};

fn failed(code: &str) -> impl Fn(ClientError) -> CmdError + '_ {
    move |e| match e {
        ClientError::Api { message, .. } => CmdError::new(code, message),
        other => other.into(),
    }
}

/// Paseo `toWorkspaceRow`.
pub fn row(w: &Value) -> Value {
    json!({
        "workspaceId": w["workspaceId"],
        "project": w["projectDisplayName"],
        "name": w["name"],
        "isolation": w["isolation"],
        "cwd": w["cwd"],
        "background": w["background"].as_bool().unwrap_or(false),
    })
}

fn rows_output(items: Vec<Value>, background: bool) -> Output {
    let mut headers = vec!["WORKSPACE ID", "PROJECT", "NAME", "ISOLATION", "CWD"];
    if background {
        headers.push("BACKGROUND");
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|i| {
            let mut r: Vec<String> = ["workspaceId", "project", "name", "isolation", "cwd"].iter().map(|k| super::s(&i[*k])).collect();
            if background {
                r.push(if i["background"] == true { "yes".into() } else { String::new() });
            }
            r
        })
        .collect();
    Output { text: table(&headers, &rows), json: Some(Value::Array(items)) }
}

fn project_row(p: &Value) -> Value {
    json!({"projectId": p["projectId"], "name": p["name"], "kind": p["kind"], "path": p["rootPath"]})
}

/// Paseo `buildWorkspaceSource` (its checks run before the daemon is asked).
fn source(isolation: &str, path: Option<String>, project: Option<String>, w: &WorktreeArgs) -> Result<Value, String> {
    let worktree_opts = [&w.mode, &w.worktree_slug, &w.new_branch, &w.base, &w.branch, &w.pr_number, &w.forge].iter().any(|o| o.is_some());
    let mut v = json!({"isolation": isolation, "path": path, "projectId": project});
    match isolation {
        "local" => {
            if worktree_opts {
                return Err("Worktree options require --isolation worktree".into());
            }
        }
        "worktree" => {
            let mode = w.mode.clone().unwrap_or_else(|| "branch-off".into());
            match mode.as_str() {
                "branch-off" => {
                    if w.branch.is_some() || w.pr_number.is_some() || w.forge.is_some() {
                        return Err("--branch, --pr-number, and --forge require a checkout mode".into());
                    }
                }
                "checkout-branch" => {
                    if w.branch.is_none() {
                        return Err("--branch is required for --mode checkout-branch".into());
                    }
                    if w.new_branch.is_some() || w.base.is_some() || w.pr_number.is_some() || w.forge.is_some() {
                        return Err("--new-branch, --base, --pr-number, and --forge are not valid for --mode checkout-branch".into());
                    }
                }
                "checkout-pr" => {
                    let n = w.pr_number.as_deref().filter(|n| !n.is_empty()).ok_or("--pr-number is required for --mode checkout-pr")?;
                    let n: u64 = n.parse().ok().filter(|n| *n > 0).ok_or("--pr-number must be a positive integer")?;
                    if w.new_branch.is_some() || w.base.is_some() || w.branch.is_some() {
                        return Err("--new-branch, --base, and --branch are not valid for --mode checkout-pr".into());
                    }
                    v["prNumber"] = json!(n);
                }
                other => return Err(format!("Unsupported worktree mode: {other}")),
            }
            v["mode"] = json!(mode);
            v["worktreeSlug"] = json!(w.worktree_slug);
            v["newBranch"] = json!(w.new_branch);
            v["base"] = json!(w.base);
            v["branch"] = json!(w.branch);
            v["forge"] = json!(w.forge);
        }
        other => return Err(format!("Unsupported workspace isolation: {other}")),
    }
    Ok(v)
}

/// `lagent run`'s workspace (Paseo's precedence): `--workspace`, then
/// `$LUCINA_WORKSPACE_ID` (set inside agents), then a new one (`--new-workspace`, or a
/// local one for the cwd). Returns `(workspace id, cwd)`.
pub async fn for_run(client: &Client, a: &RunArgs, cwd: &str) -> Result<(String, String), CmdError> {
    let new_ws = a.new_workspace.clone().or_else(|| a.worktree.as_ref().map(|_| "worktree".to_string()));
    if let Some(k) = &a.new_workspace {
        if k != "local" && k != "worktree" {
            return Err(CmdError::new("INVALID_OPTIONS", format!("Unsupported new workspace kind: {k}")).details("Use --new-workspace local or --new-workspace worktree"));
        }
        if a.worktree.is_some() {
            return Err(CmdError::new("INVALID_OPTIONS", "--new-workspace and --worktree cannot be combined").details("Use --new-workspace worktree and the supported worktree options"));
        }
    }
    let wt = WorktreeArgs {
        mode: a.worktree_mode.clone(),
        worktree_slug: a.worktree_slug.clone().or_else(|| a.worktree.clone()),
        new_branch: a.new_branch.clone(),
        base: a.base.clone(),
        branch: a.branch.clone(),
        pr_number: a.pr_number.clone(),
        forge: a.forge.clone(),
    };
    let has_wt_opts = [&a.worktree_mode, &a.worktree_slug, &a.new_branch, &a.base, &a.branch, &a.pr_number, &a.forge].iter().any(|o| o.is_some());
    if has_wt_opts && new_ws.as_deref() != Some("worktree") {
        return Err(CmdError::new("INVALID_OPTIONS", "Worktree options require --new-workspace worktree").details("Usage: lagent run --new-workspace worktree [worktree options] <prompt>"));
    }
    let src = source(new_ws.as_deref().unwrap_or("local"), Some(cwd.to_string()), None, &wt).map_err(|m| CmdError::new("INVALID_OPTIONS", m))?;
    if new_ws.is_some() && a.workspace.is_some() {
        let which = if a.new_workspace.is_some() { "--new-workspace" } else { "--worktree" };
        return Err(CmdError::new("INVALID_OPTIONS", format!("{which} and --workspace cannot be combined")).details("Select an existing workspace or explicitly create a new one"));
    }
    let existing = if new_ws.is_some() { None } else { a.workspace.clone().or_else(|| std::env::var("LUCINA_WORKSPACE_ID").ok().filter(|s| !s.trim().is_empty())) };
    if let Some(id) = existing {
        let id = id.trim().to_string();
        eprintln!("Using workspace {id}");
        let w = client.get(&format!("/workspaces/{id}")).await.map_err(|_| CmdError::new("WORKSPACE_NOT_FOUND", format!("Workspace not found: {id}")))?;
        if !w["archivedAt"].is_null() {
            return Err(CmdError::new("WORKSPACE_NOT_FOUND", format!("Workspace not found: {id}")));
        }
        return Ok((id, w["cwd"].as_str().unwrap_or(cwd).to_string()));
    }
    let mut body = src;
    if new_ws.as_deref() == Some("worktree") {
        body["prompt"] = json!(a.prompt);
        body["provider"] = json!(a.provider);
    }
    let created = client.post("/workspaces", &body).await.map_err(failed("WORKSPACE_CREATE_FAILED"))?;
    let w = &created["workspace"];
    let id = w["workspaceId"].as_str().unwrap_or_default().to_string();
    let name = w["name"].as_str().unwrap_or_default();
    let label = match (w["isolation"].as_str(), w["branch"].as_str()) {
        (Some("worktree"), Some(b)) => format!("{name} ({b})"),
        _ => name.to_string(),
    };
    eprintln!("Created workspace {id} - {label}");
    if let Some(reason) = created["setupSkippedReason"].as_str() {
        eprintln!("{reason}");
    }
    eprintln!("Tip: pass --workspace <id> (or set LUCINA_WORKSPACE_ID) to run in an existing workspace.");
    Ok((id, w["cwd"].as_str().unwrap_or(cwd).to_string()))
}

pub async fn project(client: &Client, cmd: &ProjectCommand) -> Result<Output, CmdError> {
    match cmd {
        ProjectCommand::Create { path } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let path = cwd.join(path.as_deref().unwrap_or("."));
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            let p = client.post("/projects", &json!({"path": path.display().to_string()})).await.map_err(failed("PROJECT_CREATE_FAILED"))?;
            Ok(single(project_row(&p), &["projectId", "name", "kind", "path"]))
        }
        ProjectCommand::Ls => {
            let v = client.get("/projects").await?;
            let items: Vec<Value> = v["projects"].as_array().cloned().unwrap_or_default().iter().map(project_row).collect();
            let rows: Vec<Vec<String>> = items.iter().map(|p| ["projectId", "name", "kind", "path"].iter().map(|k| super::s(&p[*k])).collect()).collect();
            Ok(Output { text: table(&["PROJECT ID", "NAME", "KIND", "PATH"], &rows), json: Some(Value::Array(items)) })
        }
        ProjectCommand::Rename { id, name, reset } => {
            let name = if *reset {
                if name.is_some() {
                    return Err(CmdError::new("INVALID_OPTIONS", "--reset cannot be combined with a name"));
                }
                None
            } else {
                let n = name.as_deref().map(str::trim).unwrap_or_default();
                if n.is_empty() {
                    return Err(CmdError::new("MISSING_NAME", "Project name cannot be empty").details("Usage: lagent project rename <project-id> <name> | --reset"));
                }
                Some(n.to_string())
            };
            let p = client.request("PATCH", &format!("/projects/{id}"), Some(&json!({"name": name}))).await.map_err(failed("PROJECT_RENAME_FAILED"))?;
            Ok(single(json!({"projectId": id, "name": p["customName"]}), &["projectId", "name"]))
        }
        ProjectCommand::Move { id, path, name } => {
            let path = std::fs::canonicalize(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.clone());
            let v = client.post(&format!("/projects/{id}/move"), &json!({"path": path, "name": name})).await.map_err(failed("PROJECT_MOVE_FAILED"))?;
            let count = |k: &str| v[k].as_array().map(Vec::len).unwrap_or(0);
            let mut text = format!("{} is at {}; {} workspace(s) moved", super::s(&v["project"]["displayName"]), super::s(&v["project"]["rootPath"]), count("moved"));
            for w in v["skipped"].as_array().map(Vec::as_slice).unwrap_or_default() {
                text.push_str(&format!("\nkept (its new directory doesn't exist): {} {}", super::s(&w["workspaceId"]), super::s(&w["cwd"])));
            }
            Ok(Output { text, json: Some(v) })
        }
        ProjectCommand::Delete { id } => {
            let v = client.request("DELETE", &format!("/projects/{id}"), None).await.map_err(failed("PROJECT_DELETE_FAILED"))?;
            let n = v["removedWorkspaceIds"].as_array().map(Vec::len).unwrap_or(0);
            Ok(Output { text: table(&["PROJECT ID", "REMOVED WORKSPACES"], &[vec![id.clone(), n.to_string()]]), json: Some(v) })
        }
    }
}

pub async fn workspace(client: &Client, cmd: &WorkspaceCommand) -> Result<Output, CmdError> {
    match cmd {
        WorkspaceCommand::Setup { id } => {
            let v = client.post(&format!("/workspaces/{id}/setup"), &json!({})).await.map_err(failed("WORKSPACE_SETUP_FAILED"))?;
            Ok(single(json!({"workspaceId": id, "status": v["status"]}), &["workspaceId", "status"]))
        }
        WorkspaceCommand::Create { isolation, path, project, title, worktree, background } => {
            let path = path.clone().or_else(|| if project.is_some() && isolation == "worktree" { None } else { std::env::current_dir().ok().map(|p| p.display().to_string()) });
            let mut body = source(isolation, path, project.clone(), worktree).map_err(|m| CmdError::new("WORKSPACE_CREATE_FAILED", m))?;
            body["title"] = json!(title);
            body["background"] = json!(background);
            let v = client.post("/workspaces", &body).await.map_err(failed("WORKSPACE_CREATE_FAILED"))?;
            Ok(single(row(&v["workspace"]), &["workspaceId", "project", "name", "isolation", "cwd"]))
        }
        WorkspaceCommand::Ls { background } => {
            let v = client.get(&format!("/workspaces?background={background}")).await?;
            let items: Vec<Value> = v["workspaces"].as_array().cloned().unwrap_or_default().iter().map(row).collect();
            Ok(rows_output(items, *background))
        }
        WorkspaceCommand::Rename { id, title, reset } => {
            let title = if *reset {
                if title.is_some() {
                    return Err(CmdError::new("INVALID_OPTIONS", "--reset cannot be combined with a title").details("Pass a title to rename the workspace, or --reset to revert to the derived name"));
                }
                None
            } else {
                let t = title.as_deref().map(str::trim).unwrap_or_default();
                if t.is_empty() {
                    return Err(CmdError::new("MISSING_TITLE", "Title cannot be empty").details("Usage: lagent workspace rename <workspace-id> <title> | --reset"));
                }
                Some(t.to_string())
            };
            let v = client.request("PATCH", &format!("/workspaces/{id}"), Some(&json!({"title": title}))).await.map_err(|e| match e {
                ClientError::Api { status: 404, .. } => CmdError::new("WORKSPACE_RENAME_FAILED", "Workspace not found"),
                other => failed("WORKSPACE_RENAME_FAILED")(other),
            })?;
            Ok(single(json!({"workspaceId": id, "title": v["title"]}), &["workspaceId", "title"]))
        }
        WorkspaceCommand::Archive { id } => {
            let v = client.post(&format!("/workspaces/{id}/archive"), &json!({})).await.map_err(failed("WORKSPACE_ARCHIVE_FAILED"))?;
            if let Some(note) = v["note"].as_str() {
                eprintln!("{note}");
            }
            Ok(single(json!({"workspaceId": id, "status": "archived", "archivedAt": v["archivedAt"]}), &["workspaceId", "status", "archivedAt"]))
        }
        WorkspaceCommand::Diff { id, mode, base, ignore_whitespace } => {
            let mut q = format!("/workspaces/{id}/diff?mode={mode}&ignoreWhitespace={ignore_whitespace}");
            if let Some(b) = base {
                q.push_str(&format!("&baseRef={}", b.replace('&', "%26")));
            }
            let v = client.get(&q).await.map_err(failed("DIFF_FAILED"))?;
            let mut text = String::new();
            for f in v["files"].as_array().cloned().unwrap_or_default() {
                text.push_str(&format!("{} (+{} -{}){}\n", super::s(&f["path"]), f["additions"], f["deletions"], if f["status"] == "ok" { String::new() } else { format!(" [{}]", super::s(&f["status"])) }));
                for h in f["hunks"].as_array().cloned().unwrap_or_default() {
                    for l in h["lines"].as_array().cloned().unwrap_or_default() {
                        let prefix = match l["type"].as_str() {
                            Some("add") => "+",
                            Some("remove") => "-",
                            Some("context") => " ",
                            _ => "",
                        };
                        text.push_str(&format!("{prefix}{}\n", super::s(&l["content"])));
                    }
                }
            }
            if v["diffTooLarge"] == true {
                text.push_str("(diff too large)\n");
            }
            Ok(Output { text: text.trim_end().to_string(), json: Some(v) })
        }
        WorkspaceCommand::Files { id, path } => {
            let q = format!("/workspaces/{id}/files?path={}", path.clone().unwrap_or_default().replace('&', "%26"));
            let v = client.get(&q).await.map_err(failed("FILES_FAILED"))?;
            let rows: Vec<Vec<String>> = v["entries"].as_array().cloned().unwrap_or_default().iter().map(|e| vec![super::s(&e["kind"]), super::s(&e["size"]), super::s(&e["path"])]).collect();
            Ok(Output { text: table(&["KIND", "SIZE", "PATH"], &rows), json: Some(v) })
        }
        WorkspaceCommand::Cat { id, path } => {
            let v = client.get(&format!("/workspaces/{id}/file?path={}", path.replace('&', "%26"))).await.map_err(failed("FILES_FAILED"))?;
            let text = if v["encoding"] == "utf-8" { super::s(&v["content"]) } else { format!("({} file, {} bytes)", super::s(&v["kind"]), v["size"]) };
            Ok(Output { text, json: Some(v) })
        }
        WorkspaceCommand::Status { id } => {
            let v = client.get(&format!("/workspaces/{id}/status")).await.map_err(failed("STATUS_FAILED"))?;
            let rows: Vec<Vec<String>> = ["currentBranch", "baseRef", "isDirty", "aheadBehind", "upstreamRef", "remoteUrl"].iter().map(|k| vec![k.to_string(), super::s(&v[*k])]).collect();
            Ok(Output { text: table(&["KEY", "VALUE"], &rows), json: Some(v) })
        }
        WorkspaceCommand::Sweep => {
            let v = client.post("/workspaces/sweep", &json!({})).await?;
            let rows: Vec<Vec<String>> = v["removed"].as_array().cloned().unwrap_or_default().iter().map(|r| vec![super::s(&r["workspaceId"]), super::s(&r["reason"])]).collect();
            Ok(Output { text: table(&["WORKSPACE ID", "REASON"], &rows), json: Some(v["removed"].clone()) })
        }
    }
}

pub async fn clone(client: &Client, a: &super::CloneArgs) -> Result<Output, CmdError> {
    let v = client.post("/clone", &json!({"repo": a.repo, "dir": a.dir, "protocol": a.protocol})).await.map_err(failed("CLONE_FAILED"))?;
    let data = json!({"repo": v["repo"], "checkoutPath": v["checkoutPath"], "projectId": v["project"]["projectId"]});
    Ok(single(data, &["repo", "checkoutPath", "projectId"]))
}

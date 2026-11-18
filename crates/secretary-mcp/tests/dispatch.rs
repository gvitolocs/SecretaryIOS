//! Every tool the server lists must reach an implementation: calling a listed
//! tool answers with something other than "unknown tool" (SECMCP-01).

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn every_listed_tool_is_dispatched() {
    let h = start().await;
    let scopes = secretary_mcp::scope::Scope::ALL
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let t = h.issue("claude-code", &scopes);

    let names = h.tool_names(&t).await;
    assert!(names.len() > 50, "{names:?}");
    for flaw in [
        "flaw_report",
        "flaw_search",
        "flaw_status",
        "flaw_matrix",
        "flaw_target",
        "flaw_dispatch",
    ] {
        assert!(names.iter().any(|n| n == flaw), "{flaw} is not listed");
    }

    let mut undispatched = Vec::new();
    for name in &names {
        let (err, text) = h.call(&t, name, json!({})).await;
        if err && text.contains("unknown tool") {
            undispatched.push(format!("{name}: {text}"));
        }
    }
    assert!(
        undispatched.is_empty(),
        "listed tools nothing dispatches to:\n{}",
        undispatched.join("\n")
    );
}

#[tokio::test]
async fn flaw_tools_reach_the_flaw_backend() {
    let h = start().await;
    let scopes = secretary_mcp::scope::Scope::ALL
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let t = h.issue("claude-code", &scopes);

    // `flaw_matrix` takes no arguments: the call must arrive at the tracker.
    let (err, text) = h.call(&t, "flaw_matrix", json!({})).await;
    assert_eq!(h.mock.requests("/v1/flaws/matrix").len(), 1, "{text}");
    // The mock server has no tracker: a refusal is fine, "unknown tool" is not.
    assert!(err && !text.contains("unknown tool"), "{text}");
}

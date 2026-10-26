//! The `secretary-mcp token …` CLI against a running server sharing the
//! data directory.

mod common;

use std::process::Command;

use common::*;
use serde_json::json;

fn cli(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_secretary-mcp"))
        .args(args)
        .env("SECRETARY_MCP_DATA_DIR", dir)
        .output()
        .expect("run secretary-mcp");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn token_issue_list_revoke() {
    let h = start().await;
    let dir = h.data_dir();

    let (code, out, err) = cli(
        &dir,
        &[
            "token",
            "issue",
            "--name",
            "claude-code",
            "--scopes",
            "read,chat,write",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let token = out.trim().to_owned();
    assert!(token.starts_with("mcp_") && token.len() == 47, "{out}");
    assert!(err.contains("shown only now"));
    let (code, out, _) = cli(&dir, &["token", "issue", "--name", "grok"]);
    assert_eq!(code, 0);
    let grok = out.trim().to_owned();

    // the running server picks up the new tokens without a restart
    assert_eq!(h.tool_names(&token).await.len(), 30);
    let (e, text) = h
        .call(&grok, "ask_lucina", json!({"message": "ciao"}))
        .await;
    assert!(!e && text.contains("from grok"), "{text}");
    assert_eq!(
        h.tool_names(&grok).await.len(),
        20,
        "default scopes are read,chat"
    );

    let (code, out, _) = cli(&dir, &["token", "list"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("claude-code")
            && out.contains("scopes=read,chat,write")
            && out.contains("active"),
        "{out}"
    );
    assert!(
        out.contains("grok") && out.contains("scopes=read,chat "),
        "{out}"
    );
    assert!(
        !out.contains(&token[4..]),
        "the token itself is never listed"
    );

    let (code, out, _) = cli(&dir, &["token", "revoke", "claude-code"]);
    assert_eq!(code, 0);
    assert!(out.contains("revoked tok_"), "{out}");
    let (status, _, _) = h.rpc_raw(Some(&token), "tools/list", json!({})).await;
    assert_eq!(
        status, 401,
        "a revoked token is refused on the next request"
    );
    assert_eq!(
        h.tool_names(&grok).await.len(),
        20,
        "other tokens keep working"
    );
    let (code, out, _) = cli(&dir, &["token", "list"]);
    assert_eq!(code, 0);
    assert!(out.contains("REVOKED"));
    let (code, _, err) = cli(&dir, &["token", "revoke", "claude-code"]);
    assert_eq!(code, 1, "{err}");

    // bad input
    let (code, _, err) = cli(&dir, &["token", "issue", "--name", "Claude Code"]);
    assert_ne!(code, 0);
    assert!(err.contains("name must be"), "{err}");
    let (code, _, err) = cli(
        &dir,
        &["token", "issue", "--name", "x", "--scopes", "read,admin"],
    );
    assert_ne!(code, 0);
    assert!(err.contains("unknown scope"), "{err}");

    // files are private
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.join("tokens.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let (code, out, _) = cli(&dir, &["oauth", "list"]);
    assert_eq!(code, 0);
    assert!(out.contains("active grants: 0"), "{out}");
}

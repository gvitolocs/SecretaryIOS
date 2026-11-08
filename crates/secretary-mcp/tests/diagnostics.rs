//! `diagnostics_recent`, `diagnostics_get`, `diagnostics_file` against a real
//! lucina-diagnostics, through `/mcp`.

mod common;

use common::*;
use serde_json::{json, Value};

const DIAG_TOKEN: &str = "diag-service-token-0123456789abcdef";

/// A running lucina-diagnostics with one Secretary Code crash (log + minidump)
/// and one iOS error; returns its URL, the crash id and the temp dir.
async fn diagnostics_service() -> (String, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_string_lossy().into_owned();
    let cfg = lucina_diagnostics::Config::from_lookup(|k| match k {
        "LUCINA_DIAGNOSTICS_DIR" => Some(data.clone()),
        _ => None,
    });
    let app = lucina_diagnostics::App::new(cfg)
        .unwrap()
        .with_tokens(None, Some(DIAG_TOKEN));
    let router = lucina_diagnostics::api::router(app);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });

    let http = reqwest::Client::new();
    let report = json!({
        "app": "secretary-code",
        "app_version": "0.9.3",
        "os": "macOS 26.1",
        "device": "mac-1",
        "kind": "crash",
        "message": "the background service timed out after 30000 ms",
        "stack": "Error: background service timed out\n    at waitReady (/Applications/Secretary.app/Contents/Resources/app.asar/out/main/index.js:812:11)",
        "context": {"window": "main", "engine_version": "1.4.2"},
        "occurred_at": "2026-10-26T18:44:00Z",
    });
    let mut log = String::new();
    for i in 0..400 {
        log.push_str(&format!(
            "19:43:{:02} line {i} waiting for sidecar\n",
            i % 60
        ));
    }
    log.push_str("19:44:00 sidecar password=hunter2 never answered /health\n");
    let b = "BoundaryMcp";
    let mut body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"report\"\r\n\r\n{report}\r\n\
         --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"main.log\"\r\nContent-Type: text/plain\r\n\r\n{log}\r\n\
         --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"crash.dmp\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(b"MDMP\x93\xa7\x00\x00\x01\x02");
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let r: Value = http
        .post(format!("{url}/v1/diagnostics/reports"))
        .bearer_auth(DIAG_TOKEN)
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_owned();
    for _ in 0..2 {
        http.post(format!("{url}/v1/diagnostics/reports"))
            .bearer_auth(DIAG_TOKEN)
            .json(&json!({"app": "ios", "app_version": "3.1", "device": "iphone", "kind": "error", "message": "sync failed: 502"}))
            .send()
            .await
            .unwrap();
    }
    (url, id, dir)
}

#[tokio::test]
async fn diagnostics_tools_end_to_end() {
    let (url, id, _dir) = diagnostics_service().await;
    let h = start_with(|cfg, _| {
        cfg.diagnostics_url = Some(url.clone());
        cfg.diagnostics_token = Some(DIAG_TOKEN.into());
        cfg.timezone = chrono_tz::Europe::Copenhagen;
    })
    .await;
    let read = h.issue("claude-code", "read");
    let names = h.tool_names(&read).await;
    for t in ["diagnostics_recent", "diagnostics_get", "diagnostics_file"] {
        assert!(names.contains(&t.to_owned()), "{t} listed for read");
    }

    let (err, text) = h.call(&read, "diagnostics_recent", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("2 report(s)"), "{text}");
    assert!(text.contains(&id), "{text}");
    assert!(
        text.contains("Secretary Code crash · Mon 26 Oct 2026 19:44"),
        "{text}"
    );
    assert!(text.contains("at waitReady (index.js)"), "{text}");
    assert!(
        text.contains("iOS app error") && text.contains("×2"),
        "{text}"
    );

    let (err, text) = h
        .call(
            &read,
            "diagnostics_recent",
            json!({"app": "ios", "kind": "error", "since": "7d"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("1 report(s)") && !text.contains(&id),
        "{text}"
    );
    let (err, text) = h
        .call(&read, "diagnostics_recent", json!({"kind": "hang"}))
        .await;
    assert!(!err && text == "No diagnostics reports match.", "{text}");
    let (err, text) = h
        .call(&read, "diagnostics_recent", json!({"app": "windows"}))
        .await;
    assert!(err, "{text}");
    let (err, text) = h
        .call(&read, "diagnostics_recent", json!({"limit": 1}))
        .await;
    assert!(!err && text.contains("before = \""), "{text}");

    let (err, text) = h.call(&read, "diagnostics_get", json!({"id": id})).await;
    assert!(!err, "{text}");
    assert!(
        text.contains("the background service timed out after 30000 ms"),
        "{text}"
    );
    assert!(text.contains("\"engine_version\": \"1.4.2\""), "{text}");
    assert!(
        text.contains("crash.dmp") && text.contains("binary"),
        "{text}"
    );
    assert!(text.contains("End of main.log"), "{text}");
    assert!(
        text.contains("password=[redacted] never answered /health"),
        "{text}"
    );
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("line 0 waiting"), "only the tail: {text}");

    let (err, text) = h
        .call(
            &read,
            "diagnostics_file",
            json!({"id": id, "name": "main.log", "from_end": false, "max_chars": 100}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("first ") && text.contains("line 0 waiting"),
        "{text}"
    );
    let (err, text) = h
        .call(
            &read,
            "diagnostics_file",
            json!({"id": id, "name": "main.log", "max_chars": 60}),
        )
        .await;
    assert!(!err && text.contains("never answered /health"), "{text}");
    let (err, text) = h
        .call(
            &read,
            "diagnostics_file",
            json!({"id": id, "name": "crash.dmp"}),
        )
        .await;
    assert!(err && text.contains("binary"), "{text}");
    let (err, text) = h
        .call(
            &read,
            "diagnostics_file",
            json!({"id": id, "name": "../index.sqlite"}),
        )
        .await;
    assert!(err, "{text}");
    let (err, text) = h
        .call(
            &read,
            "diagnostics_get",
            json!({"id": "dg_0000000000000000"}),
        )
        .await;
    assert!(err && text.contains("Not found"), "{text}");
}

#[tokio::test]
async fn diagnostics_tools_hidden_without_backend() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    let names = h.tool_names(&t).await;
    assert!(
        !names.iter().any(|n| n.starts_with("diagnostics_")),
        "{names:?}"
    );
    let (status, _, v) = h
        .rpc_raw(
            Some(&t),
            "tools/call",
            json!({"name": "diagnostics_recent", "arguments": {}}),
        )
        .await;
    assert_eq!(status, 200);
    assert!(v["error"].is_object(), "unknown tool: {v}");
}

#[tokio::test]
async fn diagnostics_unreachable_is_a_tool_error() {
    let h = start_with(|cfg, _| {
        cfg.diagnostics_url = Some("http://127.0.0.1:1".into());
        cfg.diagnostics_token = Some(DIAG_TOKEN.into());
    })
    .await;
    let t = h.issue("claude-code", "read");
    let (err, text) = h.call(&t, "diagnostics_recent", json!({})).await;
    assert!(err && text.contains("not reachable"), "{text}");
}

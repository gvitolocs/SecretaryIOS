//! `diagnostics_*` tools against a real lucina-diagnostics.

mod common;

use common::*;
use serde_json::{json, Value};

const DIAG_TOKEN: &str = "diag-service-token-0123456789";
const BOUNDARY: &str = "mcpdiagBOUNDARY";

struct Diag {
    url: String,
    _dir: tempfile::TempDir,
}

async fn diagnostics() -> Diag {
    let dir = tempfile::tempdir().unwrap();
    let cfg = lucina_diagnostics::Config::for_dir(dir.path().join("data"));
    let app = lucina_diagnostics::App::new(cfg)
        .unwrap()
        .with_tokens(None, Some(DIAG_TOKEN));
    let router = lucina_diagnostics::api::router(app);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    Diag { url, _dir: dir }
}

async fn ingest(d: &Diag, report: Value, files: &[(&str, &str, Vec<u8>)]) -> String {
    let mut b = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"report\"\r\n\r\n{report}\r\n"
    )
    .into_bytes();
    for (field, name, data) in files {
        b.extend(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\n\r\n").into_bytes());
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend(format!("--{BOUNDARY}--\r\n").into_bytes());
    let r = reqwest::Client::new()
        .post(format!("{}/v1/diagnostics/reports", d.url))
        .bearer_auth(DIAG_TOKEN)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(b)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let v: Value = r.json().await.unwrap();
    v["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn recent_get_and_file() {
    let d = diagnostics().await;
    let mut log = String::new();
    for i in 1..=300 {
        log.push_str(&format!("18:43 INFO step {i}\n"));
    }
    log.push_str(
        "18:44 ERROR background service did not answer in 30 s; password=hunter2hunter2\n",
    );
    let mut dump = b"MDMP".to_vec();
    dump.extend([0u8; 64]);
    let crash = ingest(
        &d,
        json!({"app": "secretary-code", "app_version": "1.4.2", "os": "macOS 15.1", "device": "nez-mbp",
               "kind": "crash", "message": "Background service timed out after 30012ms",
               "stack": "Error: Background service timed out\n    at startSidecar (/Applications/Secretary Code.app/index.js:10:5)",
               "context": {"window": "main", "engine": "1.0.200"}, "occurred_at": "2026-10-26T18:44:00Z"}),
        &[("file", "main.log", log.into_bytes()), ("minidump", "crash.dmp", dump)],
    )
    .await;
    let err = ingest(
        &d,
        json!({"app": "ios", "kind": "error", "message": "memo upload 409"}),
        &[],
    )
    .await;

    let url = d.url.clone();
    let h = start_with(move |cfg, _| {
        cfg.diagnostics_url = Some(url);
        cfg.diagnostics_token = Some(DIAG_TOKEN.into());
    })
    .await;
    let t = h.issue("claude-code", "read");
    assert!(h
        .tool_names(&t)
        .await
        .contains(&"diagnostics_get".to_owned()));

    let (e, text) = h.call(&t, "diagnostics_recent", json!({})).await;
    assert!(!e, "{text}");
    assert!(text.starts_with("2 reports"), "{text}");
    assert!(
        text.contains(&format!(
            "- {crash} · Secretary Code 1.4.2 · crash · 2026-10-26 19:44"
        )),
        "{text}"
    );
    assert!(
        text.contains("Background service timed out after 30012ms"),
        "{text}"
    );
    let (_, text) = h
        .call(&t, "diagnostics_recent", json!({"kind": "error"}))
        .await;
    assert!(text.contains(&err) && !text.contains(&crash), "{text}");
    let (_, text) = h
        .call(&t, "diagnostics_recent", json!({"app": "android"}))
        .await;
    assert_eq!(text, "No diagnostics reports match.");
    let (e, text) = h
        .call(&t, "diagnostics_recent", json!({"app": "fridge"}))
        .await;
    assert!(e && text.contains("must be one of"), "{text}");

    let (e, text) = h.call(&t, "diagnostics_get", json!({"id": crash})).await;
    assert!(!e, "{text}");
    assert!(
        text.starts_with("Secretary Code 1.4.2 crashed: 2026-10-26 19:44"),
        "{text}"
    );
    assert!(text.contains("at startSidecar"), "{text}");
    assert!(text.contains("\"engine\": \"1.0.200\""), "{text}");
    assert!(
        text.contains("- crash.dmp (minidump, 68 B; binary"),
        "{text}"
    );
    assert!(
        text.contains("End of main.log (earlier lines left out):"),
        "{text}"
    );
    assert!(
        text.contains("ERROR background service did not answer in 30 s; password=[redacted]"),
        "{text}"
    );
    assert!(!text.contains("hunter2"), "{text}");
    let (_, text) = h
        .call(&t, "diagnostics_get", json!({"id": crash, "log_lines": 3}))
        .await;
    let tail = text.split("End of main.log").nth(1).unwrap();
    let tail = tail.split("\n\n").next().unwrap();
    assert_eq!(tail.lines().count(), 4, "header + 3 lines: {tail}");
    assert!(
        tail.contains("step 300") && !tail.contains("step 297"),
        "{tail}"
    );

    let (e, text) = h
        .call(
            &t,
            "diagnostics_file",
            json!({"id": crash, "name": "main.log", "from": "head", "max_chars": 1000}),
        )
        .await;
    assert!(!e, "{text}");
    assert!(
        text.starts_with("main.log (")
            && text.contains("first 1000 characters:\n18:43 INFO step 1\n"),
        "{text}"
    );
    let (e, text) = h
        .call(
            &t,
            "diagnostics_file",
            json!({"id": crash, "name": "main.log"}),
        )
        .await;
    assert!(
        !e && text.contains("whole file") && text.ends_with("password=[redacted]\n"),
        "{text}"
    );
    let (e, text) = h
        .call(
            &t,
            "diagnostics_file",
            json!({"id": crash, "name": "crash.dmp"}),
        )
        .await;
    assert!(e && text.contains("binary file"), "{text}");
    let (e, text) = h
        .call(
            &t,
            "diagnostics_get",
            json!({"id": "dg_00000000000000000000"}),
        )
        .await;
    assert!(e && text.contains("No such diagnostics report"), "{text}");
    let (e, _) = h.call(&t, "diagnostics_get", json!({"id": "../etc"})).await;
    assert!(e);
}

#[tokio::test]
async fn not_configured_is_a_tool_error() {
    let h = start().await;
    let t = h.issue("claude-code", "read");
    let (e, text) = h.call(&t, "diagnostics_recent", json!({})).await;
    assert!(e && text.contains("not configured"), "{text}");
}

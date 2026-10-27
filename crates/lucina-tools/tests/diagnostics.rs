//! The `diagnostics` tool against a real lucina-diagnostics.

use std::sync::Arc;
use std::time::Duration;

use lucina_tools::agent;
use lucina_tools::{Ctx, Registry, ToolEnv, ToolSelection};
use serde_json::{json, Value};

const TOKEN: &str = "diag-service-token-0123456789";

#[tokio::test]
async fn recent_get_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let app =
        lucina_diagnostics::App::new(lucina_diagnostics::Config::for_dir(dir.path().join("data")))
            .unwrap()
            .with_tokens(None, Some(TOKEN));
    let router = lucina_diagnostics::api::router(app);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });

    let b = "B0und4ry";
    let report = json!({"app": "secretary-code", "app_version": "1.4.2", "kind": "crash", "device": "nez-mbp",
        "message": "Background service timed out after 30012ms",
        "stack": "Error: Background service timed out\n    at startSidecar (index.js:10:5)"});
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"report\"\r\n\r\n{report}\r\n--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"main.log\"\r\n\r\nINFO boot\nERROR sidecar did not answer (token=abcdef123456)\n\r\n--{b}--\r\n"
    );
    let r: Value = reqwest::Client::new()
        .post(format!("{url}/v1/diagnostics/reports"))
        .bearer_auth(TOKEN)
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_owned();

    let token_file = dir.path().join("service.token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let mut env = ToolEnv::new(dir.path(), "Europe/Copenhagen".parse().unwrap());
    env.diagnostics_url = Some(url);
    env.diagnostics_token_file = Some(token_file);
    let ctx = Ctx::new(Arc::new(env), "web", "s");
    let reg = Registry::builtin(&ToolSelection::All);
    let call = |args: Value| {
        let (reg, ctx) = (&reg, &ctx);
        async move {
            let (ok, out, _) =
                agent::execute(reg, "diagnostics", args, ctx, Duration::from_secs(10)).await;
            (ok, out)
        }
    };

    let (ok, out) = call(json!({"action": "recent", "app": "secretary-code"})).await;
    assert!(ok, "{out}");
    let s = out.to_string();
    assert!(
        s.contains(&id) && s.contains("Background service timed out after 30012ms"),
        "{s}"
    );
    let (ok, out) = call(json!({"action": "get", "id": id})).await;
    assert!(ok, "{out}");
    let s = out.to_string();
    assert!(
        s.contains("startSidecar") && s.contains("ERROR sidecar did not answer (token=[redacted])"),
        "{s}"
    );
    assert!(!s.contains("abcdef123456"), "{s}");
    let (ok, out) = call(json!({"action": "file", "id": id, "name": "main.log"})).await;
    assert!(ok && out.to_string().contains("INFO boot"), "{out}");
    let (ok, _) = call(json!({"action": "get", "id": "../../etc"})).await;
    assert!(!ok);
}

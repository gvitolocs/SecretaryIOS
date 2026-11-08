//! Lucina → dsh-remote (Harness) jobs: a slow `dsh_prompt` is followed, not failed
//! (2026-10-28: three jobs "dsh-remote unreachable" while dsh-remote was routing the prompt).

use std::time::Duration;

use lucina_server::jobs::{run_harness, DshConfig};
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct Dsh;

impl Respond for Dsh {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let tool = body["params"]["name"].as_str().unwrap_or("");
        let text = |v: Value| json!({"jsonrpc": "2.0", "id": body["id"], "result": {"content": [{"type": "text", "text": v.to_string()}]}});
        match tool {
            // Routing the prompt takes longer than one RPC.
            "dsh_prompt" => ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(1500))
                .set_body_json(text(json!({"status": "running"}))),
            _ => ResponseTemplate::new(200).set_body_json(text(json!({"status": "completed", "output": "done: logs look fine"}))),
        }
    }
}

fn cfg(url: String) -> DshConfig {
    let mut c = DshConfig::from_lookup(&|k: &str| (k == "DSH_REMOTE_TOKEN").then(|| "t".to_owned()), true, "test");
    c.url = url;
    c.rpc_timeout = Duration::from_millis(300);
    c.poll = Duration::from_millis(50);
    c.timeout = Duration::from_secs(5);
    c
}

#[tokio::test]
async fn a_slow_prompt_is_followed_by_its_request_id() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST")).respond_with(Dsh).mount(&server).await;
    let out = run_harness(&reqwest::Client::new(), &cfg(server.uri()), "check the logs").await;
    assert_eq!(out.unwrap(), "done: logs look fine");
}

#[tokio::test]
async fn a_refused_connection_says_so() {
    let err = run_harness(&reqwest::Client::new(), &cfg("http://127.0.0.1:9".into()), "x").await.unwrap_err();
    assert!(err.contains("connection refused"), "{err}");
}

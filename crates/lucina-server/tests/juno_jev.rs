//! A Juno Chat turn on the JSON planner (the live default): Jev's first-tool
//! guess starts the Juno tool loop, and a confirmed laptop action runs on the
//! owner's yes. Its own test binary: the process-wide Jev client reads its
//! settings from the environment once.

mod common;

use base64::Engine as _;
use common::{harness, script, TOKEN};
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\njev-screen";

/// Intent: always chat. First tool: a Juno tool for laptop messages, else none.
struct FakeJev;

impl Respond for FakeJev {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let message = body["state"]["message"].as_str().unwrap_or("");
        let answers = if body["questions"].get("first_tool").is_some() {
            let tool = match message {
                "show me the laptop screen" => "juno_screenshot",
                "delete old.txt on the laptop" => "juno_call",
                _ => "none",
            };
            let other = if tool == "none" { "time" } else { "none" };
            json!({"first_tool": {"type": "choice", "choice": tool, "confidence": 0.93, "probabilities": {tool: 0.93, other: 0.07}}})
        } else {
            json!({
                "intent": {"type": "choice", "choice": "chat", "confidence": 0.95, "probabilities": {"chat": 0.95, "new_topic": 0.05}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.9, "probabilities": {"all": 0.9, "today": 0.1}},
                "needs_memory": {"type": "noul", "noul": 0.1},
                "needs_tools": {"type": "noul", "noul": 0.1},
            })
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": answers,
        }))
    }
}

/// The laptop: answers every call it is handed with `answer(tool)`, until stopped.
fn laptop(h: &common::H, answer: fn(&str) -> Value) -> tokio::task::JoinHandle<()> {
    let (http, next, result) = (
        h.http.clone(),
        h.url("/device/juno/next"),
        h.url("/device/juno/result"),
    );
    tokio::spawn(async move {
        let device = json!({"id": "dev1", "name": "Juno"});
        loop {
            let b: Value = http
                .post(&next)
                .bearer_auth(TOKEN)
                .json(&json!({"device": device, "waitSecs": 2}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let call = &b["data"]["call"];
            if call.is_null() {
                continue;
            }
            http.post(&result)
                .bearer_auth(TOKEN)
                .json(&json!({"device": device, "id": call["id"], "ok": true,
                    "result": answer(call["tool"].as_str().unwrap())}))
                .send()
                .await
                .unwrap();
        }
    })
}

fn answer(tool: &str) -> Value {
    match tool {
        "screenshot" => json!({"text": "Files in front", "image": {
            "data": base64::engine::general_purpose::STANDARD.encode(PNG), "mimeType": "image/png"}}),
        "delete_file" => json!({"text": "deleted /data/home/old.txt"}),
        other => json!({"text": format!("{other} done")}),
    }
}

#[tokio::test]
async fn juno_json_planner_turns_reach_the_laptop() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(FakeJev)
        .mount(&server)
        .await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", server.uri()));
    assert!(
        lucina_jev::shared().available(),
        "the first use of the shared client in this binary"
    );

    let rules = json!({"rules": [
        {"kind": "agent", "afterTool": true, "contains": "screen", "content": "Files is in front."},
        {"kind": "agent", "contains": "screen", "toolCalls": [{"name": "juno_screenshot", "arguments": {}}]},
        {"kind": "agent", "afterTool": true, "contains": "delete", "content": "That needs your yes first."},
        {"kind": "agent", "contains": "delete", "toolCalls": [{"name": "juno_call",
            "arguments": {"tool": "delete_file", "arguments": {"path": "/data/home/old.txt"}}}]},
    ], "default": {"content": "{\"action\":\"reply\",\"text\":\"Hi from the planner.\"}"}});
    let h = harness(script(rules), |_| {}).await;
    let pc = laptop(&h, answer);
    while h.state.juno.status()["devices"][0]["connected"] != json!(true) {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let chat = |m: &'static str| {
        let h = &h;
        async move {
            let (s, b) = h
                .post(
                    "/chat",
                    json!({"message": m, "sessionId": "juno-dev1", "channel": "ios"}),
                )
                .await;
            assert_eq!(s, 200, "{b}");
            b["data"].clone()
        }
    };

    // Jev names juno_screenshot: the Juno loop runs and the reply shows the screen.
    let d = chat("show me the laptop screen").await;
    assert_eq!(d["reply"], "Files is in front.", "{d}");
    let image = d["blocks"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["type"] == "image"))
        .unwrap_or_else(|| panic!("no image block: {d}"));
    assert!(image["src"]
        .as_str()
        .unwrap()
        .starts_with("/v1/juno/screenshots/"));

    // A delete waits for the owner's yes, then runs on the laptop.
    let d = chat("delete old.txt on the laptop").await;
    assert_eq!(d["reply"], "That needs your yes first.", "{d}");
    let pending = h
        .state
        .pending
        .current("secretary_juno-dev1", chrono::Utc::now())
        .unwrap();
    assert_eq!(pending.tool, "juno_call");
    assert_eq!(pending.summary, "juno_call delete_file /data/home/old.txt");
    chat("yes").await;
    assert!(h
        .state
        .pending
        .current("secretary_juno-dev1", chrono::Utc::now())
        .is_none());
    let lines = h.timeline_lines();
    assert!(
        lines.iter().any(|l| l["text"]
            .as_str()
            .is_some_and(|t| t.contains("juno_call delete_file /data/home/old.txt → ok"))),
        "the confirmed delete ran"
    );

    // From the iPhone too: Jev's guess reaches the laptop, and the turn knows it is online.
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "show me the laptop screen", "sessionId": "ios-main", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["reply"], "Files is in front.", "{b}");
    assert!(h
        .fake
        .requests()
        .iter()
        .any(|r| r.to_string().contains("is online: juno_* tools reach it")));

    // Something else from Juno's Chat stays with the planner, with the device fact.
    let d = chat("how are you").await;
    assert!(
        d["reply"]
            .as_str()
            .unwrap()
            .contains("Hi from the planner."),
        "{d}"
    );
    let saw_fact = h
        .fake
        .requests()
        .iter()
        .any(|r| r.to_string().contains("The owner is writing from Juno"));
    assert!(saw_fact);
    pc.abort();
}

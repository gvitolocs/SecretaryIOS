//! DeepSeek client against a mocked API (wiremock).

use std::sync::atomic::Ordering;
use std::time::Duration;

use lucina_llm::{ChatModel, ChatRequest, DeepSeek, DeepSeekConfig, KeySource, LlmError, Message, Tool};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn client(server: &MockServer, key: KeySource) -> DeepSeek {
    DeepSeek::new(DeepSeekConfig {
        base_url: format!("{}/v1", server.uri()),
        key,
        timeout: Duration::from_secs(5),
        max_retries: 2,
        backoff: Duration::from_millis(10),
        ..DeepSeekConfig::default()
    })
    .unwrap()
}

fn req() -> ChatRequest {
    ChatRequest {
        messages: vec![Message::system("s"), Message::user("quanto fa 17% di 230?")],
        tools: vec![Tool::function("calculator", "math", json!({"type": "object"}))],
        tool_choice: Some("auto".into()),
        purpose: "test",
        ..ChatRequest::default()
    }
}

#[tokio::test]
async fn tool_call_round_trip_with_key_file() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test-file"))
        .and(body_partial_json(json!({"model": "deepseek-flash", "tool_choice": "auto", "stream": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "deepseek-flash",
            "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "calculator", "arguments": "{\"expression\":\"17% * 230\"}"}}]}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile_dir();
    let key = dir.join("deepseek.key");
    std::fs::write(&key, "sk-test-file\n").unwrap();
    let ds = client(&server, KeySource::File(key));
    assert!(ds.has_key());
    let r = ds.complete(req()).await.unwrap();
    assert_eq!(r.tool_calls()[0].function.name, "calculator");
    assert_eq!(r.usage.unwrap().total_tokens, 15);
    assert_eq!(ds.usage.prompt_tokens.load(Ordering::Relaxed), 12);
    // the key never shows in Debug output
    assert!(!format!("{ds:?}").contains("sk-test-file"));
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-llm-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Flaky(std::sync::atomic::AtomicUsize);
impl Respond for Flaky {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => ResponseTemplate::new(429).insert_header("retry-after", "0"),
            1 => ResponseTemplate::new(503),
            _ => ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "39,1"}}]
            })),
        }
    }
}

#[tokio::test]
async fn retries_429_and_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Flaky(Default::default()))
        .expect(3)
        .mount(&server)
        .await;
    let ds = client(&server, KeySource::Inline("k".into()));
    let r = ds.chat(req()).await.unwrap();
    assert_eq!(r.message.text(), "39,1");
}

#[tokio::test]
async fn does_not_retry_400_and_reports_no_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad tool schema"))
        .expect(1)
        .mount(&server)
        .await;
    let ds = client(&server, KeySource::Inline("k".into()));
    match ds.chat(req()).await {
        Err(LlmError::Status { status: 400, body }) => assert!(body.contains("bad tool schema")),
        other => panic!("{other:?}"),
    }
    assert_eq!(ds.usage.failures.load(Ordering::Relaxed), 1);
    let none = client(&server, KeySource::File("/nonexistent/key".into()));
    assert!(matches!(none.chat(req()).await, Err(LlmError::NoKey)));
}

#[tokio::test]
async fn times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let ds = DeepSeek::new(DeepSeekConfig {
        base_url: server.uri(),
        key: KeySource::Inline("k".into()),
        timeout: Duration::from_millis(1000),
        max_retries: 0,
        ..DeepSeekConfig::default()
    })
    .unwrap();
    assert!(matches!(ds.chat(req()).await, Err(LlmError::Timeout)));
}

#[tokio::test]
async fn streams_content_and_tool_calls() {
    let server = MockServer::start().await;
    let sse = [
        r#"data: {"model":"deepseek-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"Ci"}}]}"#,
        ": keep-alive",
        r#"data: {"choices":[{"index":0,"delta":{"content":"ao"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c9","type":"function","function":{"name":"weather","arguments":"{\"loc"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ation\":\"Copenhagen\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"stream": true, "stream_options": {"include_usage": true}})))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(sse))
        .mount(&server)
        .await;
    let ds = client(&server, KeySource::Inline("k".into()));
    let mut seen = String::new();
    let r = ds.stream(req(), |d| seen.push_str(d)).await.unwrap();
    assert_eq!(seen, "Ciao");
    assert_eq!(r.message.text(), "Ciao");
    let call = &r.tool_calls()[0];
    assert_eq!(call.id, "c9");
    assert_eq!(call.function.arguments, r#"{"location":"Copenhagen"}"#);
    assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(r.usage.unwrap().completion_tokens, 7);
}

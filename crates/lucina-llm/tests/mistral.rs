//! The Mistral dialect against a mocked API (wiremock): what goes out differs from DeepSeek, what comes back may be chunks.

use std::time::Duration;

use lucina_llm::{
    ChatModel, ChatRequest, DeepSeek, DeepSeekConfig, Dialect, FunctionCall, KeySource, Message,
    Role, Tool, ToolCall,
};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> DeepSeek {
    DeepSeek::new(DeepSeekConfig {
        base_url: format!("{}/v1", server.uri()),
        key: KeySource::Inline("mistral-test".into()),
        timeout: Duration::from_secs(5),
        max_retries: 1,
        backoff: Duration::from_millis(10),
        model: "mistral-small-latest".into(),
        dialect: Dialect::Mistral,
    })
    .unwrap()
}

fn tool_loop_request() -> ChatRequest {
    ChatRequest {
        messages: vec![
            Message::user("che ore sono a Roma?"),
            Message {
                role: Role::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_dsml_0".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "clock".into(),
                        arguments: "{}".into(),
                    },
                }]),
                tool_call_id: None,
                reasoning_content: Some("thinks".into()),
            },
            Message::tool_result("call_dsml_0", "12:00"),
        ],
        tools: vec![Tool::function("clock", "time", json!({"type": "object"}))],
        tool_choice: Some("auto".into()),
        thinking: Some(false),
        purpose: "test",
        ..ChatRequest::default()
    }
}

async fn sent(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests.last().unwrap().body).unwrap()
}

#[test]
fn settings_come_from_mistral_variables_only() {
    let cfg = DeepSeekConfig::mistral_from_lookup(|k| match k {
        "MISTRAL_API_KEY" => Some(" m-key ".into()),
        "MISTRAL_MODEL" => Some("mistral-large-latest".into()),
        "DEEPSEEK_API_KEY" => Some("not-this-one".into()),
        _ => None,
    });
    assert_eq!(cfg.base_url, "https://api.mistral.ai/v1");
    assert_eq!(cfg.model, "mistral-large-latest");
    assert_eq!(cfg.dialect, Dialect::Mistral);
    assert!(matches!(&cfg.key, KeySource::Inline(k) if k == "m-key"));
    // A DeepSeek config stays what it was.
    let ds = DeepSeekConfig::from_lookup(|k| (k == "MISTRAL_API_KEY").then(|| "no".to_owned()));
    assert_eq!(ds.dialect, Dialect::DeepSeek);
    assert!(matches!(ds.key, KeySource::None));
}

#[tokio::test]
async fn request_follows_mistral_rules() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer mistral-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "mistral-small-latest",
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "Mezzogiorno."}}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11}
        })))
        .mount(&server)
        .await;
    let r = client(&server).complete(tool_loop_request()).await.unwrap();
    assert_eq!(r.message.text(), "Mezzogiorno.");

    let body = sent(&server).await;
    assert!(body.get("thinking").is_none(), "{body}");
    assert!(body.get("stream_options").is_none(), "{body}");
    let call_id = body["messages"][1]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(call_id.len(), 9);
    assert!(call_id.bytes().all(|b| b.is_ascii_alphanumeric()), "{call_id}");
    assert_eq!(body["messages"][2]["tool_call_id"], call_id, "the result still matches its call");
    assert!(body["messages"][1].get("reasoning_content").is_none(), "{body}");
}

#[tokio::test]
async fn a_valid_tool_id_is_kept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        })))
        .mount(&server)
        .await;
    let mut req = tool_loop_request();
    req.messages[1].tool_calls.as_mut().unwrap()[0].id = "aB3dE6gH9".into();
    req.messages[2].tool_call_id = Some("aB3dE6gH9".into());
    client(&server).complete(req).await.unwrap();
    let body = sent(&server).await;
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "aB3dE6gH9");
    assert_eq!(body["messages"][2]["tool_call_id"], "aB3dE6gH9");
}

#[tokio::test]
async fn deepseek_requests_are_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        })))
        .mount(&server)
        .await;
    let ds = DeepSeek::new(DeepSeekConfig {
        base_url: format!("{}/v1", server.uri()),
        key: KeySource::Inline("k".into()),
        ..DeepSeekConfig::default()
    })
    .unwrap();
    ds.complete(tool_loop_request()).await.unwrap();
    let body = sent(&server).await;
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_dsml_0");
    assert_eq!(body["messages"][1]["reasoning_content"], "thinks");
}

#[tokio::test]
async fn chunked_content_gives_only_the_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": [{"type": "text", "text": "hmm"}]},
                {"type": "text", "text": "Ciao "},
                {"type": "text", "text": "Giuseppe"}
            ]}}]
        })))
        .mount(&server)
        .await;
    let r = client(&server).complete(tool_loop_request()).await.unwrap();
    assert_eq!(r.message.text(), "Ciao Giuseppe");
}

#[tokio::test]
async fn null_content_with_tool_calls_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": "",
                "tool_calls": [{"id": "aB3dE6gH9", "function": {"name": "clock", "arguments": "{}"}}]}}]
        })))
        .mount(&server)
        .await;
    let r = client(&server).complete(tool_loop_request()).await.unwrap();
    assert_eq!(r.tool_calls()[0].function.name, "clock");
}

#[tokio::test]
async fn streaming_text_and_tool_calls() {
    let server = MockServer::start().await;
    let sse = [
        json!({"model": "mistral-small-latest", "choices": [{"delta": {"role": "assistant", "content": ""}}]}),
        json!({"choices": [{"delta": {"content": [{"type": "thinking", "thinking": [{"type": "text", "text": "hmm"}]}]}}]}),
        json!({"choices": [{"delta": {"content": "Ci"}}]}),
        json!({"choices": [{"delta": {"content": [{"type": "text", "text": "ao"}]}}]}),
        // Mistral sends a whole call in one delta, with an index.
        json!({"choices": [{"delta": {"tool_calls": [{"id": "aB3dE6gH9", "index": 0, "function": {"name": "clock", "arguments": "{\"tz\":\"Rome\"}"}}]}, "finish_reason": "tool_calls"}],
               "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}}),
    ]
    .iter()
    .map(|v| format!("data: {v}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n";
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let mut seen = Vec::new();
    let r = client(&server)
        .stream(tool_loop_request(), |d| seen.push(d.to_owned()))
        .await
        .unwrap();
    assert_eq!(seen, ["Ci", "ao"], "only answer text reaches the listener");
    assert_eq!(r.message.text(), "Ciao");
    assert_eq!(r.tool_calls()[0].function.arguments, "{\"tz\":\"Rome\"}");
    assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(r.usage.unwrap().total_tokens, 10);
    assert!(sent(&server).await.get("stream_options").is_none());
}

#[tokio::test]
async fn a_rate_limit_is_retried_then_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("{\"message\":\"Requests rate limit exceeded\"}"))
        .expect(2) // the first try and one retry
        .mount(&server)
        .await;
    let err = client(&server).complete(tool_loop_request()).await.unwrap_err();
    assert!(err.to_string().contains("429"), "{err}");
    assert!(err.retryable());
}

#[tokio::test]
async fn a_key_pasted_with_invisible_characters_still_works() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer abc_123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    // A zero-width space and a no-break space, as a paste brings them, around and inside the key.
    let cfg = DeepSeekConfig::mistral_from_lookup(|k| match k {
        "MISTRAL_API_KEY" => Some("\u{200b}abc\u{a0}_123 \n".to_owned()),
        "MISTRAL_BASE_URL" => Some(format!("{}/v1", server.uri())),
        _ => None,
    });
    let r = DeepSeek::new(cfg).unwrap().complete(tool_loop_request()).await.unwrap();
    assert_eq!(r.message.text(), "ok");
    // Only invisible characters: no key at all.
    let none = DeepSeekConfig::mistral_from_lookup(|k| (k == "MISTRAL_API_KEY").then(|| "\u{200b} ".to_owned()));
    assert!(matches!(none.key, KeySource::None));
}

#[test]
fn the_key_never_shows_in_debug() {
    let cfg = DeepSeekConfig::mistral_from_lookup(|k| (k == "MISTRAL_API_KEY").then(|| "m-secret-value".to_owned()));
    assert!(!format!("{cfg:?}").contains("m-secret-value"));
}

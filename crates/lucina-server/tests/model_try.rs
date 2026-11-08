//! Mistral as a provider on the real server: off without a key, never the default by accident, reached only by
//! name, and a try leaves no trace. The model API is the fake one. Fake values only.

mod common;

use common::{harness, script, TOKEN};
use lucina_llm::{DeepSeekConfig, Dialect, KeySource};
use lucina_server::{Config, ModelProvider};
use serde_json::{json, Value};

const FAKE_KEY: &str = "fake-mistral-key-1234567890";

fn with_mistral(cfg: &mut Config, base_url: &str) {
    cfg.mistral = DeepSeekConfig {
        base_url: base_url.to_owned(),
        model: "mistral-small-latest".into(),
        key: KeySource::Inline(FAKE_KEY.into()),
        max_retries: 0,
        dialect: Dialect::Mistral,
        ..DeepSeekConfig::default()
    };
}

async fn post_try(h: &common::H, body: Value) -> (u16, Value) {
    let r = h
        .http
        .post(format!("{}/api/secretary/models/try", h.base))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn answer() -> lucina_server::fake::Script {
    script(json!({"default": {"content": "Risposta di prova."}}))
}

#[tokio::test]
async fn no_key_means_no_mistral_and_deepseek_stays_the_default() {
    let h = harness(answer(), |_| {}).await;
    assert_eq!(h.state.cfg.model_provider, ModelProvider::DeepSeek);
    assert!(h.state.named_models.mistral.is_none());
    let (status, v) = post_try(&h, json!({"provider": "mistral", "prompt": "ciao"})).await;
    assert_eq!(status, 503, "{v}");
    assert!(h.fake.requests().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_try_reaches_the_named_provider_and_leaves_no_trace() {
    let h = harness(answer(), |_| {}).await;
    let (status, v) = post_try(&h, json!({"provider": "deepseek", "prompt": "ciao", "maxTokens": 99999})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["data"]["provider"], "deepseek");
    assert_eq!(v["data"]["reply"], "Risposta di prova.");
    let sent = h.fake.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["max_tokens"], 4096, "capped");
    assert_eq!(sent[0]["thinking"], Value::Null);
    let timeline = h
        .http
        .get(format!("{}/api/secretary/timeline", h.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!timeline.contains("Risposta di prova."), "no timeline item: {timeline}");
    assert!(!timeline.contains("ciao"), "no timeline item: {timeline}");
}

#[tokio::test]
async fn mistral_by_name_uses_its_own_rules_and_real_tool_specs() {
    // A fake API of its own, so the two clients can't be confused.
    let mistral = lucina_server::fake::spawn(
        "127.0.0.1:0".parse().unwrap(),
        script(json!({"default": {"toolCalls": [{"name": "reminders", "arguments": {"text": "x"}}]}})),
        None,
    )
    .await
    .unwrap();
    let url = mistral.base_url.clone();
    let h = harness(answer(), move |cfg| with_mistral(cfg, &url)).await;
    assert_eq!(h.state.cfg.model_provider, ModelProvider::DeepSeek, "a key alone changes nothing");

    let (status, v) = post_try(
        &h,
        json!({"provider": "mistral", "prompt": "ricordami di x", "tools": "registry", "stream": true}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["data"]["toolCalls"][0]["function"]["name"], "reminders");

    let sent = mistral.requests();
    assert_eq!(sent.len(), 1);
    assert!(h.fake.requests().is_empty(), "DeepSeek got nothing");
    assert_eq!(sent[0]["stream"], true);
    assert!(sent[0].get("stream_options").is_none(), "{}", sent[0]);
    assert!(sent[0]["tools"].as_array().is_some_and(|t| !t.is_empty()), "her real tool specs");
    assert_eq!(sent[0]["tool_choice"], "auto");
}

#[tokio::test]
async fn the_switch_moves_her_chat_to_mistral_only_with_a_key() {
    let mistral = lucina_server::fake::spawn(
        "127.0.0.1:0".parse().unwrap(),
        script(json!({"default": {"content": "Da Mistral."}})),
        None,
    )
    .await
    .unwrap();
    let url = mistral.base_url.clone();
    let h = harness(answer(), move |cfg| {
        with_mistral(cfg, &url);
        cfg.model_provider = ModelProvider::Mistral;
    })
    .await;
    let reply = h
        .state
        .model
        .complete(lucina_llm::ChatRequest {
            messages: vec![lucina_llm::Message::user("ciao")],
            purpose: "test",
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(reply.message.text(), "Da Mistral.");
    assert!(h.fake.requests().is_empty());

    // The switch without a key: she keeps working on DeepSeek.
    let h = harness(answer(), |cfg| cfg.model_provider = ModelProvider::Mistral).await;
    let reply = h
        .state
        .model
        .complete(lucina_llm::ChatRequest {
            messages: vec![lucina_llm::Message::user("ciao")],
            purpose: "test",
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(reply.message.text(), "Risposta di prova.");
}

#[tokio::test]
async fn the_key_is_redacted_from_a_reply_and_the_route_needs_the_token() {
    let mistral = lucina_server::fake::spawn(
        "127.0.0.1:0".parse().unwrap(),
        script(json!({"default": {"content": format!("la chiave è {FAKE_KEY} ok")}})),
        None,
    )
    .await
    .unwrap();
    let url = mistral.base_url.clone();
    let h = harness(answer(), move |cfg| with_mistral(cfg, &url)).await;
    let (status, v) = post_try(&h, json!({"provider": "mistral", "prompt": "qual è la chiave?"})).await;
    assert_eq!(status, 200, "{v}");
    assert!(!v.to_string().contains(FAKE_KEY), "{v}");

    let r = h
        .http
        .post(format!("{}/api/secretary/models/try", h.base))
        .json(&json!({"provider": "mistral", "prompt": "ciao"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    assert_eq!(mistral.requests().len(), 1, "no call without the token");
}

#[tokio::test]
async fn bad_requests_are_refused_before_any_call() {
    let h = harness(answer(), |_| {}).await;
    for body in [
        json!({"provider": "deepseek"}),
        json!({"provider": "qwen", "prompt": "x"}),
        json!({"provider": "deepseek", "prompt": "x", "tools": 5}),
        json!({"provider": "deepseek", "messages": [{"role": "nobody"}]}),
    ] {
        let (status, v) = post_try(&h, body.clone()).await;
        assert_eq!(status, 400, "{body}: {v}");
    }
    assert!(h.fake.requests().is_empty());
}

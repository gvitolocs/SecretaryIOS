//! `POST /api/secretary/models/try`: one model call through a named provider, on the same path as a turn
//! (the redacting wrapper, her keys, her tool specs), so a provider can be checked and benchmarked without
//! changing what she answers with. No turn runs: nothing reaches the timeline, Honcho or recent chat, and no
//! tool is executed. The `lucina-rs model-try` CLI calls it. Not forwarded by secretary-server.
//!
//! Body: `{provider?: "deepseek"|"mistral"|"raffy", model?, prompt? | messages?, system?, tools?: "registry" | [...],
//! toolChoice?, stream?, maxTokens?, temperature?, json?, thinking?}`.

use std::sync::Arc;
use std::time::Instant;

use lucina_llm::{ChatModel, ChatRequest, LlmError, Message, Tool};
use serde_json::{json, Value};

use crate::api::ApiError;

/// The longest answer one try may ask for.
const MAX_TOKENS: u32 = 4096;

/// The chat models a try can name, each behind the redactor. Mistral and raffy are here only when they have a key.
pub struct NamedModels {
    pub deepseek: Arc<dyn ChatModel>,
    pub mistral: Option<Arc<dyn ChatModel>>,
    /// The tailnet llama-server (`RAFFY_*`).
    pub raffy: Option<Arc<dyn ChatModel>>,
}

fn messages(body: &Value) -> Result<Vec<Message>, ApiError> {
    let mut out = Vec::new();
    if let Some(system) = body["system"].as_str() {
        out.push(Message::system(system));
    }
    if let Some(list) = body.get("messages").filter(|m| !m.is_null()) {
        let list: Vec<Message> = serde_json::from_value(list.clone())
            .map_err(|e| ApiError::bad_request(format!("messages: {e}")))?;
        out.extend(list);
    }
    if let Some(prompt) = body["prompt"].as_str() {
        out.push(Message::user(prompt));
    }
    if out.is_empty() {
        return Err(ApiError::bad_request("prompt or messages is required"));
    }
    Ok(out)
}

fn upstream_error(e: LlmError) -> ApiError {
    let status = match &e {
        LlmError::Status { status: 429, .. } => 429,
        LlmError::NoKey => 503,
        LlmError::Timeout => 504,
        _ => 502,
    };
    ApiError::new(status, "model_error", e.to_string())
}

pub async fn handle(st: &crate::AppState, body: &Value) -> Result<Value, ApiError> {
    let provider = body["provider"].as_str().unwrap_or("mistral");
    let model = match provider {
        "deepseek" => st.named_models.deepseek.clone(),
        "mistral" => st.named_models.mistral.clone().ok_or_else(|| {
            ApiError::new(
                503,
                "no_key",
                "Mistral has no key here (MISTRAL_API_KEY / MISTRAL_KEY_FILE)",
            )
        })?,
        "raffy" => st.named_models.raffy.clone().ok_or_else(|| {
            ApiError::new(
                503,
                "no_key",
                "the tailnet llama-server has no key (RAFFY_API_KEY / RAFFY_KEY_FILE)",
            )
        })?,
        other => return Err(ApiError::bad_request(format!("unknown provider {other}"))),
    };
    let tools: Vec<Tool> = match body.get("tools") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) if s == "registry" => st.registry.specs(),
        Some(list) => serde_json::from_value(list.clone())
            .map_err(|e| ApiError::bad_request(format!("tools: {e}")))?,
    };
    let request = ChatRequest {
        model: body["model"].as_str().map(str::to_owned),
        messages: messages(body)?,
        tool_choice: (!tools.is_empty())
            .then(|| body["toolChoice"].as_str().unwrap_or("auto").to_owned()),
        tools,
        temperature: body["temperature"].as_f64().map(|t| t as f32),
        max_tokens: Some(
            body["maxTokens"]
                .as_u64()
                .map_or(1024, |n| n.min(u64::from(MAX_TOKENS)) as u32),
        ),
        purpose: "model_try",
        json_object: body["json"].as_bool().unwrap_or(false),
        thinking: body["thinking"].as_bool(),
    };
    let started = Instant::now();
    let mut first_delta = None;
    let response = if body["stream"].as_bool().unwrap_or(false) {
        let mut on_delta = |_: &str| {
            first_delta.get_or_insert_with(|| started.elapsed().as_millis() as u64);
        };
        model.complete_streaming(request, &mut on_delta).await
    } else {
        model.complete(request).await
    }
    .map_err(upstream_error)?;
    Ok(json!({
        "provider": provider,
        "model": response.model,
        "reply": response.message.content,
        "toolCalls": response.message.tool_calls,
        "finishReason": response.finish_reason,
        "usage": response.usage,
        "elapsedMs": started.elapsed().as_millis() as u64,
        "firstDeltaMs": first_delta,
    }))
}

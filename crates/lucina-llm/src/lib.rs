//! DeepSeek chat client (OpenAI-compatible `/chat/completions`).
//!
//! - Chat completions with **tool/function calling** ([`Tool`], [`ToolCall`]).
//! - **SSE streaming** ([`DeepSeek::stream`]): content deltas to a callback,
//!   tool-call fragments assembled into whole calls.
//! - Per-request timeout, **retry with backoff** on 429/5xx and transport
//!   errors (honouring `Retry-After`), no retry on other 4xx.
//! - **Token usage** logged per call (`tracing` target `lucina_llm::usage`)
//!   and summed in [`UsageTotals`].
//! - The API key is read from `DEEPSEEK_KEY_FILE` (re-read every 5 s, so a
//!   rotation lands without a restart) or `DEEPSEEK_API_KEY`; it is never
//!   logged or printed (`Debug` redacts it).
//!
//! [`ChatModel`] is the seam the agent loop uses; tests plug in a scripted
//! model instead of the network.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const DEFAULT_MODEL: &str = "deepseek-flash";
pub const PRO_MODEL: &str = "deepseek-v4-pro";

// ---------------------------------------------------------------- wire types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments, as the model produced them.
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// DeepSeek thinking models: kept and sent back within a tool loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl Message {
    fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, content)
        }
    }
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// A function the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl Tool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub prompt_cache_hit_tokens: u64,
    #[serde(default)]
    pub prompt_cache_miss_tokens: u64,
}

/// A chat request. `model: None` uses the client's default model.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    /// `"auto"` (default with tools), `"none"`, `"required"`.
    pub tool_choice: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Free-form purpose for the usage log (`chat`, `briefing`, ...).
    pub purpose: &'static str,
    /// JSON output mode (`response_format: {type: "json_object"}`); the
    /// prompt must ask for JSON and show the shape.
    pub json_object: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub message: Message,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub model: String,
}

impl ChatResponse {
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.message.tool_calls.as_deref().unwrap_or(&[])
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no DeepSeek API key (DEEPSEEK_KEY_FILE / DEEPSEEK_API_KEY)")]
    NoKey,
    #[error("DeepSeek HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("DeepSeek request timed out")]
    Timeout,
    #[error("DeepSeek transport: {0}")]
    Transport(String),
    #[error("DeepSeek response: {0}")]
    Decode(String),
}

impl LlmError {
    pub fn retryable(&self) -> bool {
        match self {
            LlmError::Status { status, .. } => *status == 429 || *status >= 500,
            LlmError::Timeout | LlmError::Transport(_) => true,
            LlmError::NoKey | LlmError::Decode(_) => false,
        }
    }
}

/// What the agent loop needs from a model.
#[async_trait::async_trait]
pub trait ChatModel: Send + Sync {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, LlmError>;
}

// ---------------------------------------------------------------- client

/// Where the key comes from. Never printed.
#[derive(Clone)]
pub enum KeySource {
    File(PathBuf),
    Inline(String),
    None,
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::File(p) => write!(f, "File({})", p.display()),
            KeySource::Inline(_) => f.write_str("Inline(<redacted>)"),
            KeySource::None => f.write_str("None"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub base_url: String,
    pub model: String,
    pub key: KeySource,
    pub timeout: Duration,
    pub max_retries: u32,
    /// First backoff step; doubles per retry (plus jitter), capped at 10 s.
    pub backoff: Duration,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            model: DEFAULT_MODEL.into(),
            key: KeySource::None,
            timeout: Duration::from_millis(90_000),
            max_retries: 3,
            backoff: Duration::from_millis(500),
        }
    }
}

impl DeepSeekConfig {
    /// `DEEPSEEK_BASE_URL`, `DEEPSEEK_MODEL`, `DEEPSEEK_TIMEOUT_MS`,
    /// `DEEPSEEK_MAX_RETRIES`, `DEEPSEEK_API_KEY` (wins, as in Node) or
    /// `DEEPSEEK_KEY_FILE`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut cfg = Self::default();
        if let Some(v) = get("DEEPSEEK_BASE_URL") {
            cfg.base_url = v.trim().trim_end_matches('/').to_owned();
        }
        if let Some(v) = get("DEEPSEEK_MODEL") {
            cfg.model = v.trim().to_owned();
        }
        if let Some(ms) = get("DEEPSEEK_TIMEOUT_MS").and_then(|v| v.trim().parse::<u64>().ok()) {
            cfg.timeout = Duration::from_millis(ms.max(1000));
        }
        if let Some(n) = get("DEEPSEEK_MAX_RETRIES").and_then(|v| v.trim().parse::<u32>().ok()) {
            cfg.max_retries = n.min(8);
        }
        cfg.key = match (get("DEEPSEEK_API_KEY"), get("DEEPSEEK_KEY_FILE")) {
            (Some(k), _) if !k.trim().is_empty() => KeySource::Inline(k.trim().to_owned()),
            (_, Some(f)) if !f.trim().is_empty() => KeySource::File(PathBuf::from(f.trim())),
            _ => KeySource::None,
        };
        cfg
    }
}

/// Totals since start (exposed on the health route).
#[derive(Debug, Default)]
pub struct UsageTotals {
    pub calls: AtomicU64,
    pub failures: AtomicU64,
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
}

impl UsageTotals {
    pub fn snapshot(&self) -> Value {
        serde_json::json!({
            "calls": self.calls.load(Ordering::Relaxed),
            "failures": self.failures.load(Ordering::Relaxed),
            "promptTokens": self.prompt_tokens.load(Ordering::Relaxed),
            "completionTokens": self.completion_tokens.load(Ordering::Relaxed),
        })
    }
}

struct KeyCache {
    value: String,
    checked: Option<Instant>,
}

pub struct DeepSeek {
    cfg: DeepSeekConfig,
    http: reqwest::Client,
    key_cache: Mutex<KeyCache>,
    pub usage: Arc<UsageTotals>,
}

impl std::fmt::Debug for DeepSeek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeek").field("cfg", &self.cfg).finish()
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "<[Tool]>::is_empty")]
    tools: &'a [Tool],
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: Message,
    #[serde(default)]
    finish_reason: Option<String>,
}

fn jitter(max_ms: u64) -> u64 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    u64::from(u16::from_le_bytes(b)) % max_ms.max(1)
}

impl DeepSeek {
    pub fn new(cfg: DeepSeekConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-llm/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| LlmError::Transport(e.without_url().to_string()))?;
        Ok(Self {
            cfg,
            http,
            key_cache: Mutex::new(KeyCache {
                value: String::new(),
                checked: None,
            }),
            usage: Arc::new(UsageTotals::default()),
        })
    }

    pub fn config(&self) -> &DeepSeekConfig {
        &self.cfg
    }

    /// Is a key available right now (without revealing it)?
    pub fn has_key(&self) -> bool {
        self.api_key().is_some()
    }

    fn api_key(&self) -> Option<String> {
        match &self.cfg.key {
            KeySource::Inline(k) => Some(k.clone()),
            KeySource::None => None,
            KeySource::File(path) => {
                let mut cache = self.key_cache.lock().unwrap_or_else(|e| e.into_inner());
                if cache
                    .checked
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
                {
                    cache.value = std::fs::read_to_string(path)
                        .map(|s| s.trim().to_owned())
                        .unwrap_or_default();
                    cache.checked = Some(Instant::now());
                }
                (!cache.value.is_empty()).then(|| cache.value.clone())
            }
        }
    }

    fn body<'a>(&'a self, req: &'a ChatRequest, model: &'a str, stream: bool) -> WireRequest<'a> {
        WireRequest {
            model,
            messages: &req.messages,
            tools: &req.tools,
            tool_choice: if req.tools.is_empty() {
                None
            } else {
                req.tool_choice.as_deref()
            },
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            stream,
            stream_options: stream.then(|| serde_json::json!({"include_usage": true})),
            response_format: req
                .json_object
                .then(|| serde_json::json!({"type": "json_object"})),
        }
    }

    /// One HTTP attempt; returns the response once the status is 2xx.
    async fn send_once(
        &self,
        req: &ChatRequest,
        model: &str,
        stream: bool,
    ) -> Result<reqwest::Response, (LlmError, Option<Duration>)> {
        let key = self.api_key().ok_or((LlmError::NoKey, None))?;
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let mut builder = self
            .http
            .post(url)
            .bearer_auth(key)
            .json(&self.body(req, model, stream));
        if !stream {
            builder = builder.timeout(self.cfg.timeout);
        }
        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                (LlmError::Timeout, None)
            } else {
                (LlmError::Transport(e.without_url().to_string()), None)
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| Duration::from_secs(s.min(10)));
        let body: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        Err((
            LlmError::Status {
                status: status.as_u16(),
                body,
            },
            retry_after,
        ))
    }

    /// Send with retries on 429/5xx/transport errors.
    async fn send(
        &self,
        req: &ChatRequest,
        model: &str,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let mut attempt = 0u32;
        loop {
            match self.send_once(req, model, stream).await {
                Ok(resp) => return Ok(resp),
                Err((err, retry_after)) => {
                    if !err.retryable() || attempt >= self.cfg.max_retries {
                        return Err(err);
                    }
                    let base = self.cfg.backoff.as_millis() as u64 * (1u64 << attempt.min(5));
                    let wait = retry_after.unwrap_or_else(|| {
                        Duration::from_millis((base + jitter(base / 2 + 1)).min(10_000))
                    });
                    tracing::warn!(model, attempt = attempt + 1, wait_ms = wait.as_millis() as u64, error = %err, "deepseek retry");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
            }
        }
    }

    fn record(&self, purpose: &str, model: &str, usage: Option<&Usage>, started: Instant) {
        self.usage.calls.fetch_add(1, Ordering::Relaxed);
        let u = usage.cloned().unwrap_or_default();
        self.usage
            .prompt_tokens
            .fetch_add(u.prompt_tokens, Ordering::Relaxed);
        self.usage
            .completion_tokens
            .fetch_add(u.completion_tokens, Ordering::Relaxed);
        tracing::info!(
            target: "lucina_llm::usage",
            purpose,
            model,
            prompt_tokens = u.prompt_tokens,
            completion_tokens = u.completion_tokens,
            total_tokens = u.total_tokens,
            cache_hit_tokens = u.prompt_cache_hit_tokens,
            ms = started.elapsed().as_millis() as u64,
            "deepseek usage"
        );
    }

    /// Non-streaming completion.
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let started = Instant::now();
        let model = req.model.clone().unwrap_or_else(|| self.cfg.model.clone());
        let result = async {
            let resp = self.send(&req, &model, false).await?;
            let text = resp.text().await.map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    LlmError::Transport(e.without_url().to_string())
                }
            })?;
            let wire: WireResponse =
                serde_json::from_str(&text).map_err(|e| LlmError::Decode(e.to_string()))?;
            let choice = wire
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| LlmError::Decode("no choices".into()))?;
            Ok(ChatResponse {
                message: choice.message,
                finish_reason: choice.finish_reason,
                usage: wire.usage,
                model: wire.model.unwrap_or_else(|| model.clone()),
            })
        }
        .await;
        match &result {
            Ok(r) => self.record(req.purpose, &model, r.usage.as_ref(), started),
            Err(e) => {
                self.usage.failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(purpose = req.purpose, model = %model, error = %e, "deepseek call failed");
            }
        }
        result
    }

    /// Streaming completion: `on_delta` gets each content fragment; the
    /// assembled response (tool calls included) is returned at the end.
    pub async fn stream(
        &self,
        req: ChatRequest,
        mut on_delta: impl FnMut(&str) + Send,
    ) -> Result<ChatResponse, LlmError> {
        let started = Instant::now();
        let model = req.model.clone().unwrap_or_else(|| self.cfg.model.clone());
        let resp = match self.send(&req, &model, true).await {
            Ok(r) => r,
            Err(e) => {
                self.usage.failures.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
        };
        let mut acc = StreamAccumulator::default();
        let mut buffer = String::new();
        let mut bytes = resp.bytes_stream();
        let deadline = tokio::time::Instant::now() + self.cfg.timeout;
        loop {
            let next = tokio::time::timeout_at(deadline, bytes.next())
                .await
                .map_err(|_| LlmError::Timeout)?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|e| LlmError::Transport(e.without_url().to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                if acc.feed_line(line.trim_end_matches(['\r', '\n']), &mut on_delta)? {
                    let r = acc.finish(&model);
                    self.record(req.purpose, &model, r.usage.as_ref(), started);
                    return Ok(r);
                }
            }
        }
        if !buffer.trim().is_empty() {
            acc.feed_line(buffer.trim(), &mut on_delta)?;
        }
        let r = acc.finish(&model);
        self.record(req.purpose, &model, r.usage.as_ref(), started);
        Ok(r)
    }
}

#[async_trait::async_trait]
impl ChatModel for DeepSeek {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        self.chat(request).await
    }
}

/// Assembles OpenAI-style `chat.completion.chunk` events.
#[derive(Default)]
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    model: Option<String>,
}

impl StreamAccumulator {
    /// Feed one SSE line. Returns `true` on `data: [DONE]`.
    pub fn feed_line(
        &mut self,
        line: &str,
        on_delta: &mut (impl FnMut(&str) + ?Sized),
    ) -> Result<bool, LlmError> {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(false); // comments (": keep-alive"), event:, id:, blank
        };
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        if data.is_empty() {
            return Ok(false);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| LlmError::Decode(format!("stream chunk: {e}")))?;
        if let Some(m) = v.get("model").and_then(Value::as_str) {
            self.model = Some(m.to_owned());
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = serde_json::from_value(u.clone()).ok();
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Ok(false);
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(fr.to_owned());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(false);
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                self.content.push_str(text);
                on_delta(text);
            }
        }
        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
            self.reasoning.push_str(text);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(self.calls.len() as u64) as usize;
                while self.calls.len() <= index {
                    self.calls.push(ToolCall {
                        id: String::new(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: String::new(),
                            arguments: String::new(),
                        },
                    });
                }
                let slot = &mut self.calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    slot.id = id.to_owned();
                }
                if let Some(f) = call.get("function") {
                    if let Some(name) = f.get("name").and_then(Value::as_str) {
                        slot.function.name.push_str(name);
                    }
                    if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                        slot.function.arguments.push_str(args);
                    }
                }
            }
        }
        Ok(false)
    }

    pub fn finish(self, model: &str) -> ChatResponse {
        let calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|c| !c.function.name.is_empty())
            .collect();
        ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: (!self.content.is_empty()).then_some(self.content),
                tool_calls: (!calls.is_empty()).then_some(calls),
                tool_call_id: None,
                reasoning_content: (!self.reasoning.is_empty()).then_some(self.reasoning),
            },
            finish_reason: self.finish_reason,
            usage: self.usage,
            model: self.model.unwrap_or_else(|| model.to_owned()),
        }
    }
}

// ---------------------------------------------------------------- test double

/// A scripted model for tests: returns the queued responses in order and
/// records every request.
#[derive(Default)]
pub struct ScriptedModel {
    responses: Mutex<std::collections::VecDeque<Result<ChatResponse, LlmError>>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_text(&self, text: &str) -> &Self {
        self.push(Ok(ChatResponse {
            message: Message::assistant(text),
            finish_reason: Some("stop".into()),
            usage: None,
            model: "scripted".into(),
        }))
    }

    /// One assistant turn with tool calls: `(name, arguments json)`.
    pub fn push_calls(&self, calls: &[(&str, Value)]) -> &Self {
        let calls = calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("call_{}_{i}", self.requests_len()),
                kind: "function".into(),
                function: FunctionCall {
                    name: (*name).into(),
                    arguments: args.to_string(),
                },
            })
            .collect();
        self.push(Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: Some(calls),
                tool_call_id: None,
                reasoning_content: None,
            },
            finish_reason: Some("tool_calls".into()),
            usage: None,
            model: "scripted".into(),
        }))
    }

    pub fn push(&self, r: Result<ChatResponse, LlmError>) -> &Self {
        self.responses.lock().unwrap().push_back(r);
        self
    }

    fn requests_len(&self) -> usize {
        self.responses.lock().unwrap().len()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl ChatModel for ScriptedModel {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(LlmError::Decode("scripted model exhausted".into())))
    }
}

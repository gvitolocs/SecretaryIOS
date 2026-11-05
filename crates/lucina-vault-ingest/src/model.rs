//! The local Qwen through the gateway's OpenAI-compatible API: one
//! non-streaming chat completion per item, thinking off, JSON out.
//!
//! The URL must be a loopback address (checked in [`crate::IngestConfig`]):
//! there is no remote or paid fallback, by design.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

/// A warm request; a cold one loads the model first.
const WARM_TIMEOUT: Duration = Duration::from_secs(300);
const COLD_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TOKENS: u32 = 1800;

/// The bearer from a key file (never logged).
pub fn read_key(p: Option<&Path>) -> Option<String> {
    let s = std::fs::read_to_string(p?).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[derive(Debug)]
pub enum ModelError {
    /// The gateway turned the request away (busy, switching, lease): not the
    /// item's fault; wait and try again later.
    Busy(String),
    /// Anything else (timeouts, bad answers): counts as a failed attempt.
    Failed(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Busy(s) => write!(f, "busy: {s}"),
            ModelError::Failed(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Qwen {
    http: reqwest::Client,
    base: String,
    key: Option<std::path::PathBuf>,
    model: String,
}

impl Qwen {
    pub fn new(base: &str, key: Option<std::path::PathBuf>, model: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_owned(),
            key,
            model: model.to_owned(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// One answer to `system` + `user`.
    pub async fn complete(
        &self,
        system: &str,
        user: &str,
        cold: bool,
    ) -> Result<String, ModelError> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": 0.2,
            "max_tokens": MAX_TOKENS,
            "stream": false,
            "response_format": {"type": "json_object"},
            // Qwen 3.x: answer directly, no thinking tokens.
            "chat_template_kwargs": {"enable_thinking": false},
        });
        let mut req = self
            .http
            .post(format!("{}/v1/chat/completions", self.base))
            .timeout(if cold { COLD_TIMEOUT } else { WARM_TIMEOUT })
            .header("x-client", "lucina-vault-ingest")
            .json(&body);
        if let Some(k) = read_key(self.key.as_deref()) {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| {
            let connect = e.is_connect();
            let msg = e.without_url().to_string();
            if connect {
                ModelError::Busy(format!("gateway unreachable: {msg}"))
            } else {
                ModelError::Failed(format!("request failed: {msg}"))
            }
        })?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ModelError::Failed(format!("answer unreadable: {}", e.without_url())))?;
        if status == 429 || status == 503 {
            return Err(ModelError::Busy(format!(
                "gateway answered {status}: {}",
                crate::clip(&text, 200)
            )));
        }
        if !(200..300).contains(&status) {
            return Err(ModelError::Failed(format!(
                "gateway answered {status}: {}",
                crate::clip(&text, 200)
            )));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| ModelError::Failed(format!("answer is not JSON: {e}")))?;
        let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
        if content.trim().is_empty() {
            return Err(ModelError::Failed("empty answer".into()));
        }
        Ok(content.to_owned())
    }
}

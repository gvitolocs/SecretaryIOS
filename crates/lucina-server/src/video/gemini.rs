//! A small Gemini `generateContent` client: text and images in, text (or JSON) out.
//! Lucina's own key (`GEMINI_API_KEY`, ~/.config/lucina-rs/gemini.env), read when called.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use serde_json::{json, Value};

use super::VideoConfig;

pub enum Part {
    Text(String),
    /// A JPEG or PNG on disk.
    Image(PathBuf),
}

fn key() -> anyhow::Result<String> {
    std::env::var("GEMINI_API_KEY")
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow!("GEMINI_API_KEY is not set"))
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

async fn body(system: &str, parts: &[Part], json_out: bool) -> anyhow::Result<Value> {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            Part::Text(t) => out.push(json!({"text": t})),
            Part::Image(path) => {
                let bytes = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("reading {}", path.display()))?;
                out.push(json!({"inlineData": {
                    "mimeType": mime(path),
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                }}));
            }
        }
    }
    let mut config = json!({"temperature": 0.2});
    if json_out {
        config["responseMimeType"] = json!("application/json");
    }
    Ok(json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": [{"role": "user", "parts": out}],
        "generationConfig": config,
    }))
}

/// One call to `model`; the reply's text.
async fn call(
    http: &reqwest::Client,
    base: &str,
    model: &str,
    body: &Value,
    timeout: Duration,
) -> anyhow::Result<String> {
    let key = key()?;
    let r = http
        .post(format!(
            "{}/models/{model}:generateContent",
            base.trim_end_matches('/')
        ))
        .header("x-goog-api-key", key)
        .timeout(timeout)
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow!("{model}: {}", e.without_url()))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v["error"]["message"].as_str().unwrap_or("no message");
        bail!(
            "{model}: HTTP {}: {}",
            status.as_u16(),
            msg.chars().take(300).collect::<String>()
        );
    }
    let text: String = v["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|ps| ps.iter().filter_map(|p| p["text"].as_str()).collect())
        .unwrap_or_default();
    if text.trim().is_empty() {
        let why = v["candidates"][0]["finishReason"]
            .as_str()
            .or(v["promptFeedback"]["blockReason"].as_str())
            .unwrap_or("empty answer");
        bail!("{model}: {why}");
    }
    Ok(text)
}

/// `generateContent` with the configured model, then the fallback model once.
pub async fn generate(
    http: &reqwest::Client,
    cfg: &VideoConfig,
    system: &str,
    parts: &[Part],
    json_out: bool,
    timeout: Duration,
) -> anyhow::Result<String> {
    let body = body(system, parts, json_out).await?;
    match call(http, &cfg.gemini_url, &cfg.gemini_model, &body, timeout).await {
        Ok(t) => Ok(t),
        Err(e) if cfg.gemini_fallback_model != cfg.gemini_model => {
            tracing::warn!(error = %e, "gemini: trying the fallback model");
            call(
                http,
                &cfg.gemini_url,
                &cfg.gemini_fallback_model,
                &body,
                timeout,
            )
            .await
            .map_err(|f| anyhow!("{e:#}; {f:#}"))
        }
        Err(e) => Err(e),
    }
}

/// [`generate`] with a JSON answer, parsed. Tolerates a ```json fence.
pub async fn generate_json(
    http: &reqwest::Client,
    cfg: &VideoConfig,
    system: &str,
    parts: &[Part],
    timeout: Duration,
) -> anyhow::Result<Value> {
    let text = generate(http, cfg, system, parts, true, timeout).await?;
    parse_json(&text)
}

pub fn parse_json(text: &str) -> anyhow::Result<Value> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .map(|s| s.trim_end().trim_end_matches("```"))
        .unwrap_or(t);
    serde_json::from_str(t.trim()).context("the model's answer is not JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_with_or_without_a_fence() {
        assert_eq!(parse_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(parse_json("```json\n{\"a\":2}\n```").unwrap()["a"], 2);
        assert!(parse_json("not json").is_err());
    }
}

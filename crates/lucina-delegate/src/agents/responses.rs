//! The Responses API (`POST /v1/responses`), shared by the xAI (Grok) and
//! OpenAI (ChatGPT) API backends: one input, optional server-side search
//! tools, the output text back.

use std::time::Duration;

use serde_json::{json, Value};

/// The text of a Responses API answer (`output_text`, or the message parts).
pub fn output_text(v: &Value) -> String {
    if let Some(t) = v["output_text"].as_str() {
        return t.trim().to_owned();
    }
    let mut parts = Vec::new();
    for item in v["output"].as_array().into_iter().flatten() {
        if item["type"] != "message" {
            continue;
        }
        for c in item["content"].as_array().into_iter().flatten() {
            if let Some(t) = c["text"].as_str() {
                parts.push(t.trim().to_owned());
            }
        }
    }
    parts.join("\n\n").trim().to_owned()
}

/// One call. `tools`: e.g. `[{"type":"web_search"}]`.
pub async fn call(
    http: &reqwest::Client,
    base: &str,
    key: &lucina_secrets::SecretValue,
    model: &str,
    input: &str,
    tools: Vec<Value>,
    timeout: Duration,
) -> Result<String, String> {
    let mut body = json!({"model": model, "input": input});
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    let auth = lucina_secrets::outbound::bearer(key).map_err(|e| e.to_string())?;
    let resp = http
        .post(format!("{}/v1/responses", base.trim_end_matches('/')))
        .header(reqwest::header::AUTHORIZATION, auth)
        .timeout(timeout)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{}", e.without_url()))?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let msg = v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .unwrap_or("");
        return Err(format!(
            "HTTP {status}: {}",
            msg.chars().take(200).collect::<String>()
        ));
    }
    let text = output_text(&v);
    if text.is_empty() {
        return Err("empty answer".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn posts_and_reads_the_answer() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer test-key-123456"))
            .and(body_partial_json(json!({"model": "grok-4", "tools": [{"type": "web_search"}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": [
                    {"type": "web_search_call"},
                    {"type": "message", "content": [{"type": "output_text", "text": "Rust 1.95 is out."}]}
                ]
            })))
            .mount(&s)
            .await;
        let key = lucina_secrets::SecretValue::new("test-key-123456".into());
        let out = call(
            &reqwest::Client::new(),
            &s.uri(),
            &key,
            "grok-4",
            "news?",
            vec![json!({"type": "web_search"})],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(out, "Rust 1.95 is out.");
        assert_eq!(output_text(&json!({"output_text": " hi "})), "hi");
    }

    #[tokio::test]
    async fn reports_http_errors() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
            )
            .mount(&s)
            .await;
        let key = lucina_secrets::SecretValue::new("k-123456789".into());
        let e = call(
            &reqwest::Client::new(),
            &s.uri(),
            &key,
            "m",
            "x",
            vec![],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert_eq!(e, "HTTP 401: bad key");
    }
}

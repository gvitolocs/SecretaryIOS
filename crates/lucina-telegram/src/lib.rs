//! Minimal Bot API transport. Credentials and raw request URLs never escape errors.
pub mod lease;
pub mod lease_decision;
pub mod outcome;

use outcome::{classify, SendOutcome, SendStage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub outcome: SendOutcome,
    pub http_status: Option<u16>,
    pub error_code: Option<i64>,
    pub retry_after: Option<u64>,
    message: String,
}

#[derive(Clone)]
pub struct BotApi {
    // Deliberately no Debug implementation, including for the client (URLs contain credentials).
    token: String,
    base_url: String,
    client: reqwest::Client,
    timeout: Duration,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SendOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
    pub disable_web_page_preview: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    result: Option<Value>,
    error_code: Option<i64>,
    description: Option<String>,
    parameters: Option<Parameters>,
}
#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}

impl BotApi {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, "https://api.telegram.org")
    }
    pub fn with_base_url(token: String, base_url: &str) -> Self {
        Self {
            token,
            base_url: base_url.trim_end_matches('/').into(),
            // No redirects: never forward the credential-bearing URL to another host.
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("TLS client"),
            timeout: Duration::from_secs(30),
        }
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    fn error(&self, outcome: SendOutcome, status: Option<u16>, message: &str) -> ApiError {
        ApiError {
            outcome,
            http_status: status,
            error_code: None,
            retry_after: None,
            message: message.replace(&self.token, "[REDACTED]"),
        }
    }
    fn transport_error(&self, e: reqwest::Error, status: Option<u16>) -> ApiError {
        let stage = if e.is_connect() || e.is_builder() {
            SendStage::Connect
        } else {
            SendStage::Sent
        };
        // Never format reqwest errors: even their Debug includes token-bearing URLs.
        let reason = if e.is_timeout() {
            "Telegram request timed out"
        } else if stage == SendStage::Connect {
            "Telegram connection failed"
        } else {
            "Telegram response interrupted"
        };
        self.error(
            classify(stage, status.filter(|s| *s >= 400), None),
            status,
            reason,
        )
    }
    async fn call(&self, method: &str, body: Value, timeout: Duration) -> Result<Value, ApiError> {
        let response = self
            .client
            .post(format!("{}/bot{}/{method}", self.base_url, self.token))
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| self.transport_error(e, None))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| self.transport_error(e, Some(status)))?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| {
            // A malformed success response might follow acceptance; do not resend it.
            self.error(
                if status >= 400 {
                    SendOutcome::NotDelivered
                } else {
                    SendOutcome::Uncertain
                },
                Some(status),
                "Invalid Telegram response",
            )
        })?;
        if envelope.ok {
            return Ok(envelope.result.unwrap_or(Value::Null));
        }
        let mut error = self.error(
            classify(SendStage::Sent, Some(status), Some(false)),
            Some(status),
            envelope
                .description
                .as_deref()
                .unwrap_or("Telegram rejected request"),
        );
        error.error_code = envelope.error_code;
        error.retry_after = envelope.parameters.and_then(|p| p.retry_after);
        Err(error)
    }
    pub async fn get_me(&self) -> Result<Value, ApiError> {
        self.call("getMe", json!({}), self.timeout).await
    }
    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_s: u64,
        allowed_updates: &[&str],
    ) -> Result<Value, ApiError> {
        self.call(
            "getUpdates",
            json!({"offset":offset,"timeout":timeout_s,"allowed_updates":allowed_updates}),
            Duration::from_secs(timeout_s.saturating_add(15)),
        )
        .await
    }
    pub async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        opts: &SendOptions,
    ) -> Result<Value, ApiError> {
        let mut body = serde_json::to_value(opts).expect("options serialize");
        body["chat_id"] = json!(chat_id);
        body["text"] = json!(text);
        self.call("sendMessage", body, self.timeout).await
    }
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        opts: &SendOptions,
    ) -> Result<Vec<Value>, ApiError> {
        let mut sent = Vec::new();
        for part in split_text(text) {
            match self.send_message(chat_id, part, opts).await {
                Ok(value) => sent.push(value),
                Err(mut error) => {
                    if !sent.is_empty() {
                        error.outcome = SendOutcome::Uncertain;
                        error.message = format!("partial Telegram delivery; {}", error.message);
                    }
                    return Err(error);
                }
            }
        }
        Ok(sent)
    }
    pub async fn send_chat_action(&self, chat_id: &str, action: &str) -> Result<Value, ApiError> {
        self.call(
            "sendChatAction",
            json!({"chat_id":chat_id,"action":action}),
            self.timeout,
        )
        .await
    }
    pub async fn get_file(&self, file_id: &str) -> Result<Value, ApiError> {
        self.call("getFile", json!({"file_id":file_id}), self.timeout)
            .await
    }
    pub async fn delete_webhook(&self, drop_pending_updates: bool) -> Result<Value, ApiError> {
        self.call(
            "deleteWebhook",
            json!({"drop_pending_updates":drop_pending_updates}),
            self.timeout,
        )
        .await
    }
    pub async fn download_file(
        &self,
        file_path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ApiError> {
        if file_path.split('/').any(|p| p == "..") || file_path.contains(['?', '#']) {
            return Err(self.error(
                SendOutcome::NotDelivered,
                None,
                "Invalid Telegram file path",
            ));
        }
        let mut response = self
            .client
            .get(format!(
                "{}/file/bot{}/{}",
                self.base_url,
                self.token,
                file_path.trim_start_matches('/')
            ))
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| self.transport_error(e, None))?;
        if !response.status().is_success() {
            return Err(self.error(
                SendOutcome::NotDelivered,
                Some(response.status().as_u16()),
                "Telegram download rejected",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| self.transport_error(e, None))?
        {
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err(self.error(
                    SendOutcome::NotDelivered,
                    None,
                    "Telegram file exceeds byte limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

/// Preserve text exactly; prefer the last newline, then space, inside 4096 UTF-16 units.
pub fn split_text(mut text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    while text.encode_utf16().count() > 4096 {
        let mut units = 0;
        let mut end = 0;
        for (i, c) in text.char_indices() {
            if units + c.len_utf16() > 4096 {
                break;
            }
            units += c.len_utf16();
            end = i + c.len_utf8();
        }
        let prefix = &text[..end];
        let cut = prefix
            .rfind('\n')
            .or_else(|| prefix.rfind(' '))
            .map(|i| i + 1)
            .unwrap_or(end);
        parts.push(&text[..cut]);
        text = &text[cut..];
    }
    if !text.is_empty() {
        parts.push(text);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    const TOKEN: &str = "123:TEST_SECRET";
    #[tokio::test]
    async fn methods_and_options() {
        let server = MockServer::start().await;
        let bot = BotApi::with_base_url(TOKEN.into(), &server.uri());
        for (method_name, body) in [
            ("getMe", json!({})),
            (
                "getUpdates",
                json!({"offset":7,"timeout":2,"allowed_updates":["message"]}),
            ),
            (
                "sendMessage",
                json!({"chat_id":"42","text":"hello","disable_web_page_preview":true,"reply_to_message_id":9,"parse_mode":"HTML"}),
            ),
            ("sendChatAction", json!({"chat_id":"42","action":"typing"})),
            ("getFile", json!({"file_id":"file"})),
            ("deleteWebhook", json!({"drop_pending_updates":true})),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/bot{TOKEN}/{method_name}")))
                .and(body_json(body))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":true})),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        bot.get_me().await.unwrap();
        bot.get_updates(7, 2, &["message"]).await.unwrap();
        bot.send_message(
            "42",
            "hello",
            &SendOptions {
                reply_to_message_id: Some(9),
                disable_web_page_preview: true,
                parse_mode: Some("HTML".into()),
            },
        )
        .await
        .unwrap();
        bot.send_chat_action("42", "typing").await.unwrap();
        bot.get_file("file").await.unwrap();
        bot.delete_webhook(true).await.unwrap();
    }
    #[tokio::test]
    async fn rejection_timeout_and_redaction() {
        for status in [429, 500] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).respond_with(ResponseTemplate::new(status).set_body_json(json!({"ok":false,"error_code":status,"description":format!("bad {TOKEN}"),"parameters":{"retry_after":4}}))).mount(&server).await;
            let error = BotApi::with_base_url(TOKEN.into(), &server.uri())
                .get_me()
                .await
                .unwrap_err();
            assert_eq!(error.outcome, SendOutcome::NotDelivered);
            assert_eq!(error.retry_after, Some(4));
            assert!(!format!("{error} {error:?}").contains(TOKEN));
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_json(json!({"ok":true,"result":true})),
            )
            .mount(&server)
            .await;
        let error = BotApi::with_base_url(TOKEN.into(), &server.uri())
            .with_timeout(Duration::from_millis(30))
            .get_me()
            .await
            .unwrap_err();
        assert_eq!(error.outcome, SendOutcome::Uncertain);
        assert!(!format!("{error} {error:?}").contains(TOKEN));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let error = BotApi::with_base_url(TOKEN.into(), &format!("http://{addr}"))
            .get_me()
            .await
            .unwrap_err();
        assert_eq!(error.outcome, SendOutcome::NotDelivered);
        assert!(!format!("{error} {error:?}").contains(TOKEN));
    }
    #[test]
    fn splitting_preserves_utf16_and_boundaries() {
        for text in [
            "a".repeat(4097),
            "😀".repeat(4097),
            format!(
                "{}\n{} {}",
                "x".repeat(4000),
                "y".repeat(99),
                "z".repeat(4100)
            ),
        ] {
            let parts = split_text(&text);
            assert!(parts.iter().all(|p| p.encode_utf16().count() <= 4096));
            assert_eq!(parts.concat(), text);
        }
        assert_eq!(
            split_text(&format!("{}\n{}", "x".repeat(4000), "y".repeat(200)))[0].len(),
            4001
        );
    }
    #[tokio::test]
    async fn sends_long_text_in_order_and_partial_delivery_is_uncertain() {
        let server = MockServer::start().await;
        let bot = BotApi::with_base_url(TOKEN.into(), &server.uri());
        let text = format!("{}\n{}", "a".repeat(4000), "b".repeat(200));
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":{}})))
            .expect(2)
            .mount(&server)
            .await;
        bot.send_text("42", &text, &SendOptions::default())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].body_json::<Value>().unwrap()["text"],
            format!("{}\n", "a".repeat(4000))
        );
        assert_eq!(
            requests[1].body_json::<Value>().unwrap()["text"],
            "b".repeat(200)
        );
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":{}})))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let error = bot
            .send_text("42", &text, &SendOptions::default())
            .await
            .unwrap_err();
        assert_eq!(error.outcome, SendOutcome::Uncertain);
    }
    #[tokio::test]
    async fn downloads_are_bounded() {
        let server = MockServer::start().await;
        Mock::given(path(format!("/file/bot{TOKEN}/files/test")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1; 20]))
            .mount(&server)
            .await;
        let bot = BotApi::with_base_url(TOKEN.into(), &server.uri());
        assert_eq!(bot.download_file("files/test", 20).await.unwrap().len(), 20);
        let error = bot.download_file("files/test", 19).await.unwrap_err();
        assert!(!format!("{error:?}").contains(TOKEN));
        assert!(bot.download_file("../test", 100).await.is_err());
    }
}

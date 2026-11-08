//! The Node Lucina's Telegram bot as an outbox (`src/meshy-bridge.js` on
//! branch `secretary-live`, on only with `LUCINA_MESHY=true` there).
//!
//! - `POST /meshy/plan` `{pendingId, text}`: the plan with ✅/❌ buttons; a tap
//!   calls back `POST /api/secretary/meshy/confirm` on this Lucina.
//! - `POST /meshy/preview` `{text, modelId, video}` (`video`: base64 MP4,
//!   at most 8 MB): the turntable video, sent with `sendVideo`.
//!
//! `LUCINA_MESHY_TELEGRAM_URL` + `LUCINA_MESHY_TELEGRAM_TOKEN_FILE` (bearer,
//! read per call). Unset: nothing goes to Telegram (the app still gets it).

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct TelegramBridge {
    http: reqwest::Client,
    url: Option<String>,
    token_file: Option<PathBuf>,
}

pub const MAX_VIDEO_BYTES: usize = 8 * 1024 * 1024;

impl TelegramBridge {
    pub fn new(url: Option<String>, token_file: Option<PathBuf>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60))
                .build()
                .expect("http client"),
            url: url
                .map(|u| u.trim().trim_end_matches('/').to_owned())
                .filter(|u| !u.is_empty()),
            token_file,
        }
    }

    pub fn configured(&self) -> bool {
        self.url.is_some() && self.token_file.is_some()
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<(), String> {
        let url = self.url.as_ref().ok_or("telegram bridge not configured")?;
        let token = self
            .token_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .ok_or("telegram bridge token missing")?;
        let resp = self
            .http
            .post(format!("{url}{path}"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "telegram bridge unreachable".to_owned())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "telegram bridge answered {}",
                resp.status().as_u16()
            ))
        }
    }

    /// The plan with confirm buttons.
    pub async fn send_plan(&self, pending_id: &str, text: &str) -> Result<(), String> {
        self.post(
            "/meshy/plan",
            json!({"pendingId": pending_id, "text": text}),
        )
        .await
    }

    /// The finished model's turntable video.
    pub async fn send_preview(&self, model_id: &str, text: &str, mp4: &[u8]) -> Result<(), String> {
        if mp4.len() > MAX_VIDEO_BYTES {
            return Err("preview too large for Telegram".into());
        }
        let video = base64::engine::general_purpose::STANDARD.encode(mp4);
        self.post(
            "/meshy/preview",
            json!({"modelId": model_id, "text": text, "video": video}),
        )
        .await
    }

    /// A plain text message (job failed, …).
    pub async fn send_text(&self, text: &str) -> Result<(), String> {
        self.post("/meshy/text", json!({"text": text})).await
    }
}

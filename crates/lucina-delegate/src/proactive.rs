//! The proactive engine's door to delegation (`lucina-proactive` runs as its
//! own process). It may only ask for **read-only research** and shares the
//! same budget, switches and audit, because it goes through the Rust
//! Lucina: `POST /api/secretary/delegate` with `origin: "proactive"`.
//!
//! While the engine runs dry (`dry_run = true`, the default) it calls with
//! `dryRun: true`: Lucina answers with the agent she would pick and her
//! self-review, and nothing is sent to any agent.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::Kind;

/// What the proactive engine asks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRequest {
    pub task: String,
    /// `research` or `question` (anything else is refused by Lucina).
    pub kind: Kind,
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

/// A small client for `lucina-proactive`.
pub struct ProactiveClient {
    pub http: reqwest::Client,
    /// The Rust Lucina, e.g. `http://127.0.0.1:8790`.
    pub base_url: String,
    /// The Secretary API bearer token.
    pub token: String,
}

impl ProactiveClient {
    /// Ask Lucina to research something (or, dry, to say how she would).
    pub async fn research(&self, req: &ResearchRequest) -> Result<Value, String> {
        let body = json!({
            "task": req.task,
            "kind": req.kind.as_str(),
            "origin": "proactive",
            "dryRun": req.dry_run,
            "lang": req.lang,
        });
        let resp = self
            .http
            .post(format!(
                "{}/api/secretary/delegate",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(60))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            Err(format!(
                "HTTP {status}: {}",
                v["error"]["code"].as_str().unwrap_or("")
            ))
        }
    }
}

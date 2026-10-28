//! Push notifications through secretary-server's notify module (`POST /v1/notify`,
//! category `agent`, service token). The server stores each one in the owner's inbox
//! and pushes it unless one of the owner's apps is in front (Paseo: in-app instead of
//! push while a client was active in the last 3 minutes), outside quiet hours.
//!
//! Configured by `LUCINA_AGENTD_NOTIFY_URL` (e.g. `http://127.0.0.1:8787`) and
//! `LUCINA_AGENTD_NOTIFY_TOKEN` (a `svc_` service token, handed over by the Infisical
//! broker's `exec`). Without them notifications are off; sending never blocks or
//! fails a run.

use serde::Serialize;
use serde_json::Value;

pub const URL_ENV: &str = "LUCINA_AGENTD_NOTIFY_URL";
pub const TOKEN_ENV: &str = "LUCINA_AGENTD_NOTIFY_TOKEN";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    pub category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub body: String,
    pub route: String,
    pub dedup_key: String,
    pub data: Value,
}

#[derive(Clone)]
pub struct Notifier {
    url: String,
    token: String,
    http: reqwest::Client,
}

impl Notifier {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var(URL_ENV).ok().filter(|u| !u.is_empty())?;
        let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty())?;
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().ok()?;
        Some(Notifier { url: format!("{}/v1/notify", url.trim_end_matches('/')), token, http })
    }

    /// Send in the background; failures are logged.
    pub fn send(&self, push: Push) {
        let me = self.clone();
        tokio::spawn(async move {
            let res = me.http.post(&me.url).bearer_auth(&me.token).json(&push).send().await;
            match res {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => tracing::warn!(status = %r.status(), key = %push.dedup_key, "notify refused"),
                Err(e) => tracing::warn!(error = %e, key = %push.dedup_key, "notify failed"),
            }
        });
    }
}

//! "Secretary Code crashed at 19:44": a new crash signature (kind `crash` or
//! `fatal_error`) reaches the owner the way memo notifications do: the app's
//! inbox (secretary-server `POST /v1/notify` with Lucina's service token) and
//! Telegram from Lucina's bot. At most `per_hour` a rolling hour; the next one
//! sent mentions how many were held back. Repeats of a known signature never
//! notify.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use chrono_tz::Tz;
use serde_json::json;

use crate::model::{clip, first_line, Kind, Report};

#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub secretary_url: Option<String>,
    pub service_token_file: Option<PathBuf>,
    pub telegram_token: Option<String>,
    pub telegram_chat: Option<String>,
    pub per_hour: usize,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            secretary_url: None,
            service_token_file: None,
            telegram_token: None,
            telegram_chat: None,
            per_hour: 4,
        }
    }
}

impl NotifyConfig {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let on = |k: &str| {
            get(k)
                .map(|v| v.trim().to_ascii_lowercase())
                .is_none_or(|v| v != "off" && v != "false" && v != "0")
        };
        let telegram_token = get("TELEGRAM_BOT_TOKEN_LUCINA")
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .or_else(|| {
                get("TELEGRAM_BOT_TOKEN_LUCINA_FILE")
                    .and_then(|f| std::fs::read_to_string(f.trim()).ok())
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty())
            });
        Self {
            secretary_url: on("LUCINA_DIAGNOSTICS_NOTIFY_APP")
                .then(|| get("LUCINA_SECRETARY_SERVER_URL"))
                .flatten()
                .map(|u| u.trim().trim_end_matches('/').to_owned()),
            service_token_file: get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE")
                .map(|f| PathBuf::from(f.trim())),
            telegram_token: on("LUCINA_DIAGNOSTICS_NOTIFY_TELEGRAM")
                .then_some(telegram_token)
                .flatten(),
            telegram_chat: get("LUCINA_DIAGNOSTICS_TELEGRAM_CHAT_ID")
                .or_else(|| get("TELEGRAM_OWNER_CHAT_ID"))
                .or_else(|| {
                    get("TELEGRAM_ALLOWED_CHAT_IDS")
                        .and_then(|v| v.split(',').next().map(str::to_owned))
                })
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            per_hour: get("LUCINA_DIAGNOSTICS_NOTIFY_PER_HOUR")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(4),
        }
    }

    pub fn enabled(&self) -> bool {
        (self.secretary_url.is_some() && self.service_token_file.is_some())
            || (self.telegram_token.is_some() && self.telegram_chat.is_some())
    }
}

fn verb(k: Kind) -> &'static str {
    match k {
        Kind::Crash => "crashed",
        Kind::FatalError => "hit a fatal error",
        Kind::Hang => "hung",
        Kind::Error => "reported an error",
        Kind::UserReport => "sent a problem report",
    }
}

/// Title and body of the notification (times in the owner's zone).
pub fn message(r: &Report, tz: Tz, held_back: usize) -> (String, String) {
    let at = r.occurred_at.with_timezone(&tz);
    let today = Utc::now().with_timezone(&tz).date_naive();
    let when = if at.date_naive() == today {
        at.format("at %H:%M").to_string()
    } else {
        at.format("on %-d %b at %H:%M").to_string()
    };
    let title = clip(&format!("{} {}", capital(r.app.label()), verb(r.kind)), 120);
    let mut body = format!(
        "{} {} {when}: {}",
        capital(r.app.label()),
        verb(r.kind),
        first_line(&r.message, 300)
    );
    let meta: Vec<&str> = [r.app_version.as_str(), r.os.as_str(), r.device.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if !meta.is_empty() {
        body.push_str(&format!("\n{}", meta.join(" · ")));
    }
    if let Some(f) = r.signature.first().filter(|f| !f.starts_with("message: ")) {
        body.push_str(&format!("\n{}", clip(f, 200)));
    }
    if held_back > 0 {
        body.push_str(&format!(
            "\n(+{held_back} other new crash signature{} held back this hour)",
            if held_back == 1 { "" } else { "s" }
        ));
    }
    body.push_str(&format!(
        "\n\nAsk Lucina or a Claude session about it: diagnostics_get {}",
        r.id
    ));
    (title, body)
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

pub struct Notifier {
    cfg: NotifyConfig,
    tz: Tz,
    sent: Mutex<(VecDeque<Instant>, usize)>,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(cfg: NotifyConfig, tz: Tz) -> Self {
        Self {
            cfg,
            tz,
            sent: Mutex::new((VecDeque::new(), 0)),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client"),
        }
    }

    /// Takes a slot of the hourly budget: `Some(held back since the last one)`.
    fn slot(&self, now: Instant) -> Option<usize> {
        let mut g = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        let (times, held) = &mut *g;
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(3600))
        {
            times.pop_front();
        }
        if times.len() >= self.cfg.per_hour {
            *held += 1;
            return None;
        }
        times.push_back(now);
        Some(std::mem::take(held))
    }

    /// Called for a newly stored report; sends in the background when it is a
    /// new crash or fatal error and the hourly budget allows. Returns whether
    /// a notification was started.
    pub fn maybe_send(self: &std::sync::Arc<Self>, r: &Report) -> bool {
        if !r.kind.notifies() || r.count != 1 || !self.cfg.enabled() {
            return false;
        }
        let Some(held) = self.slot(Instant::now()) else {
            tracing::info!(id = %r.id, "crash notification held back (hourly limit)");
            return false;
        };
        let (title, body) = message(r, self.tz, held);
        let me = self.clone();
        let id = r.id.clone();
        let fp = r.fingerprint.clone();
        tokio::spawn(async move {
            match me.send(&title, &body, &id, &fp).await {
                Ok(ch) => tracing::info!(%id, channels = ?ch, "crash notification sent"),
                Err(e) => tracing::warn!(%id, error = %e, "crash notification failed"),
            }
        });
        true
    }

    async fn send(
        &self,
        title: &str,
        body: &str,
        id: &str,
        fingerprint: &str,
    ) -> Result<Vec<&'static str>, String> {
        let cfg = &self.cfg;
        let mut sent = Vec::new();
        let mut errors = Vec::new();
        if let (Some(base), Some(tf)) = (&cfg.secretary_url, &cfg.service_token_file) {
            match std::fs::read_to_string(tf) {
                Ok(token) => {
                    let r = self
                        .http
                        .post(format!("{base}/v1/notify"))
                        .bearer_auth(token.trim())
                        .json(&json!({
                            "category": "system",
                            "title": title,
                            "body": clip(body, 1900),
                            "dedupKey": format!("diagnostics:{fingerprint}"),
                            "data": {"diagnosticsReportId": id}
                        }))
                        .send()
                        .await;
                    match r {
                        Ok(r) if r.status().is_success() => sent.push("app"),
                        Ok(r) => errors.push(format!("app notify HTTP {}", r.status())),
                        Err(e) => errors.push(format!("app notify: {}", e.without_url())),
                    }
                }
                Err(e) => errors.push(format!("service token: {e}")),
            }
        }
        if let (Some(token), Some(chat)) = (&cfg.telegram_token, &cfg.telegram_chat) {
            let r = self
                .http
                .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
                .json(&json!({"chat_id": chat, "text": body, "disable_web_page_preview": true}))
                .send()
                .await;
            match r {
                Ok(r) if r.status().is_success() => sent.push("telegram"),
                Ok(r) => errors.push(format!("telegram HTTP {}", r.status())),
                // reqwest errors can carry the URL (with the bot token): keep only the kind.
                Err(e) => errors.push(format!(
                    "telegram: {}",
                    if e.is_timeout() {
                        "timeout"
                    } else {
                        "transport error"
                    }
                )),
            }
        }
        if sent.is_empty() && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        for e in errors {
            tracing::warn!(error = %e, "crash notification channel failed");
        }
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_budget_and_held_back_count() {
        let n = Notifier::new(
            NotifyConfig {
                per_hour: 2,
                ..Default::default()
            },
            chrono_tz::Europe::Copenhagen,
        );
        let t = Instant::now();
        assert_eq!(n.slot(t), Some(0));
        assert_eq!(n.slot(t), Some(0));
        assert_eq!(n.slot(t), None);
        assert_eq!(n.slot(t), None);
        assert_eq!(n.slot(t + Duration::from_secs(3600)), Some(2));
    }
}

//! "Secretary Code crashed at 19:44: …": a short note to the owner when a
//! new fingerprint of kind `crash` or `fatal_error` arrives. Same path as
//! memos: the app's inbox (secretary-server `POST /v1/notify` with Lucina's
//! service token) and a Telegram message from Lucina's bot.
//!
//! Rate limit: at most one notification per app every `min_interval`; the
//! ones held back are counted and mentioned in the next.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono_tz::Tz;
use serde_json::json;

use crate::model::{AppName, Report};

#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub secretary_url: Option<String>,
    pub service_token_file: Option<PathBuf>,
    pub telegram_token: Option<String>,
    pub telegram_chat: Option<String>,
    pub min_interval: Duration,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            secretary_url: None,
            service_token_file: None,
            telegram_token: None,
            telegram_chat: None,
            min_interval: Duration::from_secs(600),
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
            min_interval: Duration::from_secs(
                get("LUCINA_DIAGNOSTICS_NOTIFY_MIN_INTERVAL_SEC")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(600),
            ),
        }
    }

    pub fn enabled(&self) -> bool {
        (self.secretary_url.is_some() && self.service_token_file.is_some())
            || (self.telegram_token.is_some() && self.telegram_chat.is_some())
    }
}

/// Decides which new crashes are announced.
pub struct Gate {
    min_interval: Duration,
    last: Mutex<HashMap<AppName, (Instant, u32)>>,
}

impl Gate {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(HashMap::new()),
        }
    }

    /// `Some(held back since the last one)` when this one may go out.
    pub fn admit(&self, app: AppName, now: Instant) -> Option<u32> {
        let mut m = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(&app) {
            Some((t, held)) if now.saturating_duration_since(*t) < self.min_interval => {
                *held += 1;
                None
            }
            Some((t, held)) => {
                let n = *held;
                *t = now;
                *held = 0;
                Some(n)
            }
            None => {
                m.insert(app, (now, 0));
                Some(0)
            }
        }
    }
}

fn first_line(s: &str, max: usize) -> String {
    let l = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if l.chars().count() > max {
        format!("{}…", l.chars().take(max).collect::<String>())
    } else {
        l.to_owned()
    }
}

/// Title and body: "Secretary Code crashed at 19:44: <message>".
pub fn message(r: &Report, tz: Tz, held_back: u32) -> (String, String) {
    let when = r.occurred_at.with_timezone(&tz);
    let today = chrono::Utc::now().with_timezone(&tz).date_naive();
    let clock = if when.date_naive() == today {
        when.format("%H:%M").to_string()
    } else {
        when.format("%-d %b %H:%M").to_string()
    };
    let title = format!("{} {}", r.app.label(), r.kind.verb());
    let mut body = format!(
        "{} {} at {clock}: {}",
        r.app.label(),
        r.kind.verb(),
        first_line(&r.message, 240)
    );
    if let Some(f) = r.top_frames.first().filter(|f| !f.is_empty()) {
        body.push_str(&format!("\nat {}", first_line(f, 160)));
    }
    let mut meta = vec![format!("v{}", r.app_version)];
    if !r.os.is_empty() {
        meta.push(r.os.clone());
    }
    if !r.attachments.is_empty() {
        meta.push(format!("{} attachment(s)", r.attachments.len()));
    }
    body.push_str(&format!("\n{}", meta.join(" · ")));
    if held_back > 0 {
        body.push_str(&format!(
            "\n(+{held_back} other new problem(s) since the last notice)"
        ));
    }
    body.push_str(&format!(
        "\nAsk Lucina about it, or diagnostics_get {}.",
        r.id
    ));
    (title, body)
}

/// Sends to every configured channel; Ok with the channels that accepted it.
pub async fn send(
    cfg: &NotifyConfig,
    r: &Report,
    title: &str,
    body: &str,
) -> Result<Vec<&'static str>, String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut sent = Vec::new();
    let mut errors = Vec::new();
    if let (Some(base), Some(tf)) = (&cfg.secretary_url, &cfg.service_token_file) {
        match std::fs::read_to_string(tf) {
            Ok(token) => {
                let clipped: String = body.chars().take(1900).collect();
                let res = http
                    .post(format!("{base}/v1/notify"))
                    .bearer_auth(token.trim())
                    .json(&json!({
                        "category": "lucina_message",
                        "title": title,
                        "body": clipped,
                        "channel": "diagnostics",
                        "dedupKey": format!("diagnostics:{}", r.fingerprint),
                        "data": {"reportId": r.id, "app": r.app.as_str(), "kind": r.kind.as_str()}
                    }))
                    .send()
                    .await;
                match res {
                    Ok(x) if x.status().is_success() => sent.push("app"),
                    Ok(x) => errors.push(format!("app notify HTTP {}", x.status())),
                    Err(e) => errors.push(format!("app notify: {}", e.without_url())),
                }
            }
            Err(e) => errors.push(format!("service token: {e}")),
        }
    }
    if let (Some(token), Some(chat)) = (&cfg.telegram_token, &cfg.telegram_chat) {
        let res = http
            .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
            .json(&json!({"chat_id": chat, "text": body, "disable_web_page_preview": true}))
            .send()
            .await;
        match res {
            Ok(x) if x.status().is_success() => sent.push("telegram"),
            Ok(x) => errors.push(format!("telegram HTTP {}", x.status())),
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
        tracing::warn!(error = %e, "diagnostics notification channel failed");
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_holds_back_and_counts() {
        let g = Gate::new(Duration::from_secs(600));
        let t0 = Instant::now();
        assert_eq!(g.admit(AppName::SecretaryCode, t0), Some(0));
        assert_eq!(g.admit(AppName::SecretaryCode, t0), None);
        assert_eq!(g.admit(AppName::SecretaryCode, t0), None);
        assert_eq!(g.admit(AppName::Ios, t0), Some(0), "per app");
        assert_eq!(
            g.admit(AppName::SecretaryCode, t0 + Duration::from_secs(601)),
            Some(2)
        );
    }
}

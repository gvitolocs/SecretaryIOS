//! "Your memo is ready": the app's inbox (secretary-server `POST /v1/notify`
//! with Lucina's service token) and a Telegram message from Lucina's bot to
//! the owner's chat (the phone buzzes; app push needs APNs, which is off).
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::model::{Memo, Summary};
use crate::render::labels;

#[derive(Debug, Clone, Default)]
pub struct NotifyConfig {
    pub secretary_url: Option<String>,
    pub service_token_file: Option<PathBuf>,
    pub telegram_token: Option<String>,
    pub telegram_chat: Option<String>,
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
            secretary_url: on("LUCINA_MEMO_NOTIFY_APP")
                .then(|| get("LUCINA_SECRETARY_SERVER_URL"))
                .flatten()
                .map(|u| u.trim().trim_end_matches('/').to_owned()),
            service_token_file: get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE")
                .map(|f| PathBuf::from(f.trim())),
            telegram_token: on("LUCINA_MEMO_NOTIFY_TELEGRAM")
                .then_some(telegram_token)
                .flatten(),
            telegram_chat: get("LUCINA_MEMO_TELEGRAM_CHAT_ID")
                .or_else(|| get("TELEGRAM_OWNER_CHAT_ID"))
                .or_else(|| {
                    get("TELEGRAM_ALLOWED_CHAT_IDS")
                        .and_then(|v| v.split(',').next().map(str::to_owned))
                })
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
        }
    }
}

fn ready_title(lang: &str) -> &'static str {
    match lang {
        "it" => "Memo pronto",
        "da" => "Memo klar",
        _ => "Memo ready",
    }
}

/// The text both channels send.
pub fn message(m: &Memo, s: &Summary, url: &str) -> (String, String) {
    let l = labels(&s.language);
    let mut body = format!("🎙 {}\n\n{}", s.title, s.overview);
    if !s.action_items.is_empty() {
        body.push_str(&format!("\n\n{}:", l.action_items));
        for i in s.action_items.iter().take(6) {
            body.push_str(&format!("\n• {}", i.text));
        }
    }
    body.push_str(&format!("\n\n{url}"));
    let _ = m;
    (ready_title(&s.language).to_owned(), body)
}

/// Sends to every configured channel; Ok when at least one accepted it
/// (or none is configured).
pub async fn send(
    cfg: &NotifyConfig,
    m: &Memo,
    s: &Summary,
    url: &str,
    telegram: bool,
) -> Result<Vec<&'static str>, String> {
    let (title, body) = message(m, s, url);
    send_message(
        cfg,
        &Outgoing {
            title: &title,
            body: &body,
            route: &format!("/memos/{}", m.id),
            channel: "memo",
            dedup_key: &format!("memo:{}:{}", m.id, m.revision),
            data: json!({"memoId": m.id}),
        },
        telegram,
    )
    .await
}

/// One message to the owner: the app inbox (deduplicated by `dedup_key`) and, when
/// `telegram`, Lucina's Telegram bot (no deduplication: callers mark first, kernel K6/MT8).
pub struct Outgoing<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub route: &'a str,
    pub channel: &'a str,
    pub dedup_key: &'a str,
    pub data: serde_json::Value,
}

pub async fn send_message(
    cfg: &NotifyConfig,
    o: &Outgoing<'_>,
    telegram: bool,
) -> Result<Vec<&'static str>, String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let body = o.body.to_owned();
    let mut sent = Vec::new();
    let mut errors = Vec::new();
    if let (Some(base), Some(tf)) = (&cfg.secretary_url, &cfg.service_token_file) {
        match std::fs::read_to_string(tf) {
            Ok(token) => {
                let clipped: String = body.chars().take(1900).collect();
                let r = http
                    .post(format!("{base}/v1/notify"))
                    .bearer_auth(token.trim())
                    .json(&json!({
                        "category": "lucina_message",
                        "title": o.title,
                        "body": clipped,
                        "route": o.route,
                        "channel": o.channel,
                        "dedupKey": o.dedup_key,
                        "data": o.data
                    }))
                    .send()
                    .await;
                match r {
                    Ok(r) if r.status().is_success() => sent.push("app"),
                    Ok(r) => errors.push(format!("app notify HTTP {}", r.status())),
                    Err(e) => errors.push(format!("app notify: {e}")),
                }
            }
            Err(e) => errors.push(format!("service token: {e}")),
        }
    }
    // Telegram messages are limited to 4096 characters.
    let body: String = body.chars().take(4000).collect();
    if let (true, Some(token), Some(chat)) = (telegram, &cfg.telegram_token, &cfg.telegram_chat) {
        let r = http
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
        tracing::warn!(error = %e, "memo notification channel failed");
    }
    Ok(sent)
}

//! Secretary app notifications for the deal watch (the Node Lucina's
//! `src/app-notify.js` `notifyApp`): `POST <LUCINA_SECRETARY_SERVER_URL>/v1/notify`
//! with the service token from `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (read per
//! call), only when `LUCINA_APP_NOTIFY=true`.
//!
//! Same body, limits and retry as Node: the body is clipped to 2000 and the
//! title to 120 UTF-16 units (`…`), the dedup key to 200; one retry after a
//! network error, a timeout or a 5xx; never throws, and logs no message text.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::js::{utf16_len, utf16_prefix};
use crate::watch::{
    Deliver, Delivery, Notification, NOTIFY_CATEGORY, NOTIFY_CHANNEL, NOTIFY_ROUTE,
};

const MAX_BODY_CHARS: usize = 2000;
const MAX_TITLE_CHARS: usize = 120;

/// Node's `NOTIFY_CATEGORIES`.
pub const NOTIFY_CATEGORIES: &[&str] = &[
    "lucina_message",
    "reminder",
    "calendar",
    "device_confirm",
    "shopping",
    "presence",
    "system",
];

/// `{ok, status, outcome?, code?}` of `notifyApp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyResult {
    /// `outcome` is the server's `status` (created | duplicate | disabled | present).
    Ok { status: u16, outcome: String },
    /// `code`: disabled | not_configured | bad_notification | unreachable |
    /// timeout | the server's error code | `http_<status>`.
    Err { status: u16, code: String },
}

/// One app notification (`notifyApp`'s argument).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppNotification {
    pub category: String,
    pub title: Option<String>,
    pub body: String,
    pub route: Option<String>,
    pub channel: Option<String>,
    pub dedup_key: Option<String>,
}

pub struct AppNotify {
    http: reqwest::Client,
    /// `LUCINA_APP_NOTIFY`.
    pub enabled: bool,
    /// `LUCINA_SECRETARY_SERVER_URL`.
    pub url: Option<String>,
    /// `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`.
    pub token_file: Option<PathBuf>,
    pub timeout: Duration,
    pub retry_delay: Duration,
}

impl std::fmt::Debug for AppNotify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppNotify")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field("token_file", &self.token_file)
            .finish_non_exhaustive()
    }
}

/// `clip(text, max)`: trimmed, and `…` past `max` UTF-16 units.
fn clip(text: &str, max: usize) -> String {
    let value = text.trim();
    if utf16_len(value) > max {
        format!("{}…", utf16_prefix(value, max - 1).trim_end())
    } else {
        value.to_owned()
    }
}

fn valid_channel(c: &str) -> bool {
    (1..=32).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_lowercase())
}

impl AppNotify {
    pub fn new(enabled: bool, url: Option<String>, token_file: Option<PathBuf>) -> Self {
        Self {
            http: reqwest::Client::new(),
            enabled,
            url: url.filter(|u| !u.trim().is_empty()),
            token_file,
            timeout: Duration::from_millis(8000),
            retry_delay: Duration::from_millis(1000),
        }
    }

    fn token(&self) -> String {
        self.token_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    }

    /// The request body, or `None` for a `bad_notification`.
    pub fn body(n: &AppNotification) -> Option<Value> {
        let text = clip(&n.body, MAX_BODY_CHARS);
        if !NOTIFY_CATEGORIES.contains(&n.category.as_str()) || text.is_empty() {
            return None;
        }
        let mut body = Map::new();
        body.insert("category".into(), json!(n.category));
        body.insert("body".into(), json!(text));
        if let Some(title) = n.title.as_deref().filter(|t| !t.is_empty()) {
            body.insert("title".into(), json!(clip(title, MAX_TITLE_CHARS)));
        }
        if let Some(route) = n
            .route
            .as_deref()
            .filter(|r| r.starts_with('/') && !r.starts_with("//"))
        {
            body.insert("route".into(), json!(route));
        }
        if let Some(key) = n.dedup_key.as_deref().filter(|k| !k.is_empty()) {
            body.insert("dedupKey".into(), json!(utf16_prefix(key, 200)));
        }
        if let Some(channel) = n.channel.as_deref().filter(|c| valid_channel(c)) {
            body.insert("channel".into(), json!(channel));
        }
        Some(Value::Object(body))
    }

    /// `notifyApp(notification)`.
    pub async fn notify(&self, n: &AppNotification) -> NotifyResult {
        let err = |status: u16, code: &str| NotifyResult::Err {
            status,
            code: code.into(),
        };
        if !self.enabled {
            return err(0, "disabled");
        }
        let token = self.token();
        let Some(url) = self.url.as_deref().filter(|_| !token.is_empty()) else {
            return err(0, "not_configured");
        };
        let Some(body) = Self::body(n) else {
            return err(0, "bad_notification");
        };
        let endpoint = format!("{}/v1/notify", url.trim_end_matches('/'));
        let mut last = err(0, "unreachable");
        for attempt in 0..2 {
            let sent = self
                .http
                .post(&endpoint)
                .bearer_auth(&token)
                .header("accept", "application/json")
                .json(&body)
                .timeout(self.timeout)
                .send()
                .await;
            match sent {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let ok = resp.status().is_success();
                    let data: Value = resp.json().await.unwrap_or(Value::Null);
                    if ok {
                        return NotifyResult::Ok {
                            status,
                            outcome: data["status"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or("created")
                                .to_owned(),
                        };
                    }
                    let code = data["error"]["code"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map_or_else(|| format!("http_{status}"), str::to_owned);
                    last = NotifyResult::Err { status, code };
                    if status < 500 {
                        break;
                    }
                }
                Err(e) => {
                    last = err(
                        0,
                        if e.is_timeout() {
                            "timeout"
                        } else {
                            "unreachable"
                        },
                    );
                }
            }
            if attempt == 0 {
                tokio::time::sleep(self.retry_delay).await;
            }
        }
        if let NotifyResult::Err { status, code } = &last {
            tracing::warn!(category = %n.category, status, code = %code, "app notification failed");
        }
        last
    }
}

#[async_trait]
impl Deliver for AppNotify {
    /// The deal watch's app delivery: `{category: 'shopping', title, body,
    /// route: '/shopping/map', channel: 'lucina', dedupKey}`.
    async fn deliver(&self, notification: &Notification) -> Delivery {
        let result = self
            .notify(&AppNotification {
                category: NOTIFY_CATEGORY.into(),
                title: Some(notification.title.clone()),
                body: notification.body.clone(),
                route: Some(NOTIFY_ROUTE.into()),
                channel: Some(NOTIFY_CHANNEL.into()),
                dedup_key: Some(notification.dedup_key.clone()),
            })
            .await;
        match result {
            NotifyResult::Ok { .. } => Delivery::Delivered,
            NotifyResult::Err { code, .. } if code == "disabled" || code == "not_configured" => {
                Delivery::NotConfigured
            }
            NotifyResult::Err { .. } => Delivery::Failed {
                reason: "send failed".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_like_node() {
        assert_eq!(clip("  hi  ", 10), "hi");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abc defghij", 5), "abc…");
        assert_eq!(utf16_len(&clip(&"x".repeat(3000), 2000)), 2000);
    }

    #[test]
    fn the_body_has_nodes_fields() {
        let n = AppNotification {
            category: "shopping".into(),
            title: Some("T".into()),
            body: " B ".into(),
            route: Some("//evil".into()),
            channel: Some("Lucina".into()),
            dedup_key: Some("k".into()),
        };
        assert_eq!(
            AppNotify::body(&n).unwrap(),
            json!({"category": "shopping", "body": "B", "title": "T", "dedupKey": "k"})
        );
        let bad = AppNotification {
            category: "nope".into(),
            body: "x".into(),
            ..AppNotification::default()
        };
        assert_eq!(AppNotify::body(&bad), None);
    }
}

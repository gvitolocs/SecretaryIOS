//! Delivery.
//!
//! - **Secretary app**: a timeline item (`role: lucina`, `channel: other`,
//!   `meta.kind: "proactive"` with `priority`, `actions`, `outreachId`,
//!   `topic` and `why`) appended to Lucina's `timeline.jsonl`, plus
//!   `POST /v1/notify` (category `lucina_message`, route
//!   `/?outreach=<id>`, `timelineId` so the server's own timeline ingestion
//!   never notifies twice). The server enforces quiet hours, DND and "app
//!   in front"; its answer is recorded as is.
//! - **Telegram**: Bot API `sendMessage` with Lucina's bot token, plain
//!   text and one URL button to the app (`<public_url>/?outreach=<id>`),
//!   no callback buttons (getUpdates belongs to the Node poller). By
//!   default only when the app push reached no device (`telegram =
//!   "fallback"`).
//!
//! Dry-run builds everything and sends nothing.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::config::{ChannelsCfg, TelegramMode};
use crate::lang;
use crate::model::{Action, Kind, Priority};
use crate::secretary::SecretaryApi;
use crate::text;

/// Appends one JSONL line to the owner timeline.
#[async_trait::async_trait]
pub trait TimelineSink: Send + Sync {
    async fn append(&self, line: &str) -> Result<()>;
    fn describe(&self) -> String;
}

/// Direct `O_APPEND` (one write per line, so concurrent writers' lines stay whole).
pub struct FileSink(pub PathBuf);

#[async_trait::async_trait]
impl TimelineSink for FileSink {
    async fn append(&self, line: &str) -> Result<()> {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.0)
            .await
            .with_context(|| format!("opening {}", self.0.display()))?;
        f.write_all(format!("{line}\n").as_bytes()).await?;
        f.flush().await?;
        Ok(())
    }
    fn describe(&self) -> String {
        format!("file {}", self.0.display())
    }
}

/// `docker exec -i <container> sh -c 'cat >> "$0"' <path>`: the Node
/// Lucina's container owns `timeline.jsonl` (root, 0644), and its store
/// picks up lines appended by other processes on its next read.
pub struct DockerSink {
    pub container: String,
    pub path: String,
}

#[async_trait::async_trait]
impl TimelineSink for DockerSink {
    async fn append(&self, line: &str) -> Result<()> {
        let mut child = tokio::process::Command::new("docker")
            .args([
                "exec",
                "-i",
                &self.container,
                "sh",
                "-c",
                "cat >> \"$0\"",
                &self.path,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("docker exec")?;
        let mut stdin = child.stdin.take().context("docker stdin")?;
        stdin.write_all(format!("{line}\n").as_bytes()).await?;
        drop(stdin);
        let out = child.wait_with_output().await?;
        if !out.status.success() {
            bail!(
                "docker exec failed ({}): {}",
                out.status,
                text::clip(&String::from_utf8_lossy(&out.stderr), 200)
            );
        }
        Ok(())
    }
    fn describe(&self) -> String {
        format!("docker {}:{}", self.container, self.path)
    }
}

pub struct NullSink;

#[async_trait::async_trait]
impl TimelineSink for NullSink {
    async fn append(&self, _line: &str) -> Result<()> {
        Ok(())
    }
    fn describe(&self) -> String {
        "off".into()
    }
}

pub fn sink_from(cfg: &ChannelsCfg) -> Arc<dyn TimelineSink> {
    match cfg.timeline_sink.as_str() {
        "file" => Arc::new(FileSink(cfg.timeline_path.clone().unwrap_or_default())),
        "docker" => Arc::new(DockerSink {
            container: cfg.docker_container.clone(),
            path: cfg.docker_timeline_path.clone(),
        }),
        _ => Arc::new(NullSink),
    }
}

#[async_trait::async_trait]
pub trait Telegram: Send + Sync {
    /// Send a plain-text message with one URL button; the message id.
    async fn send(&self, chat_id: &str, text: &str, button: Option<(&str, &str)>) -> Result<i64>;
}

pub struct BotApi {
    token: String,
    http: reqwest::Client,
    base: String,
}

impl BotApi {
    pub fn new(token: String) -> Result<Self> {
        Ok(Self {
            token,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
            base: "https://api.telegram.org".into(),
        })
    }
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }
}

#[async_trait::async_trait]
impl Telegram for BotApi {
    async fn send(&self, chat_id: &str, text: &str, button: Option<(&str, &str)>) -> Result<i64> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text::clip(text, 4000),
            "disable_web_page_preview": true,
        });
        if let Some((label, url)) = button {
            body["reply_markup"] = json!({"inline_keyboard": [[{"text": label, "url": url}]]});
        }
        // The token is part of the URL: never let reqwest put it in an error.
        let resp = self
            .http
            .post(format!("{}/bot{}/sendMessage", self.base, self.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("telegram transport: {}", e.without_url()))?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!(
                "telegram HTTP {status}: {}",
                v.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
            );
        }
        Ok(v.pointer("/result/message_id")
            .and_then(Value::as_i64)
            .unwrap_or(0))
    }
}

/// One message to deliver.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub outreach_id: String,
    pub kind: Kind,
    pub priority: Priority,
    pub text: String,
    pub actions: Vec<Action>,
    pub why: String,
    pub language: String,
    pub now: DateTime<Utc>,
    /// `meta.place` (an event with a location; see [`event_place`]).
    pub place: Option<Value>,
}

/// The map card under a calendar reminder (`docs/lucina-message-format.md`,
/// `place` without `type`): the event's title, location, start, end and id,
/// checked by the v1 block normalizer. `None` without a location.
pub fn event_place(event: &Value) -> Option<Value> {
    let address = event["location"].as_str().map(str::trim).unwrap_or("");
    if address.is_empty() {
        return None;
    }
    let mut place = lucina_store::rich::normalize_block(&json!({
        "type": "place", "kind": "event", "title": event["title"], "address": address,
        "when": event["start"], "end": event["end"], "eventId": event["id"],
    }))?;
    place.as_object_mut()?.remove("type");
    Some(place)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub channel: String,
    pub status: String,
    pub result: Value,
    pub timeline_id: Option<String>,
}

pub struct Channels {
    pub cfg: ChannelsCfg,
    pub api: Arc<dyn SecretaryApi>,
    pub sink: Arc<dyn TimelineSink>,
    pub telegram: Option<Arc<dyn Telegram>>,
    pub chat_id: Option<String>,
}

impl Channels {
    pub fn deep_link(&self, outreach_id: &str) -> String {
        format!(
            "{}/?outreach={outreach_id}",
            self.cfg.public_url.trim_end_matches('/')
        )
    }

    /// The timeline line (Node's item format, plus the proactive meta).
    pub fn timeline_item(&self, m: &Outgoing, id: &str) -> Value {
        let mut item = json!({
            "id": id,
            "ts": m.now.to_rfc3339_opts(SecondsFormat::Millis, true),
            "channel": "other",
            "role": "lucina",
            "text": text::clip(&m.text, 8000),
            "meta": {
                "kind": "proactive",
                "topic": m.kind.as_str(),
                "priority": m.priority.as_str(),
                "outreachId": m.outreach_id,
                "why": text::clip(&m.why, 400),
                "actions": m.actions,
            },
        });
        if let Some(place) = &m.place {
            item["meta"]["place"] = place.clone();
        }
        item
    }

    pub fn notify_body(&self, m: &Outgoing, timeline_id: Option<&str>) -> Value {
        let mut b = json!({
            "category": "lucina_message",
            "title": lang::notify_title(&m.language),
            "body": text::clip(&m.text, 2000),
            "route": format!("/?outreach={}", m.outreach_id),
            "dedupKey": format!("proactive:{}", m.outreach_id),
            "channel": "other",
            "data": {"outreachId": m.outreach_id, "kind": m.kind.as_str(), "priority": m.priority.as_str()},
        });
        if let Some(t) = timeline_id {
            b["timelineId"] = json!(t);
        }
        b
    }

    fn telegram_planned(&self, m: &Outgoing) -> bool {
        match self.cfg.telegram {
            TelegramMode::Off => false,
            TelegramMode::Always => true,
            TelegramMode::High => m.priority == Priority::High,
            TelegramMode::Fallback => false,
        }
    }

    pub async fn deliver(&self, m: &Outgoing, dry_run: bool) -> Delivery {
        let timeline_id = lucina_store::ulid::new_id(m.now.timestamp_millis());
        let item = self.timeline_item(m, &timeline_id);
        let notify = self.notify_body(m, self.cfg.app.then_some(timeline_id.as_str()));
        let telegram_ready = self.telegram.is_some() && self.chat_id.is_some();
        if dry_run {
            let mut channels = Vec::new();
            if self.cfg.app {
                channels.push("app");
            }
            let tg = if self.telegram_planned(m) {
                channels.push("telegram");
                "yes"
            } else if self.cfg.telegram == TelegramMode::Fallback
                || self.cfg.telegram == TelegramMode::High
            {
                "if the app push reaches no device"
            } else {
                "no"
            };
            return Delivery {
                channel: if channels.is_empty() {
                    "none".into()
                } else {
                    channels.join("+")
                },
                status: "dry_run".into(),
                result: json!({
                    "timelineSink": self.sink.describe(),
                    "wouldAppend": item,
                    "wouldNotify": notify,
                    "telegram": tg,
                    "telegramConfigured": telegram_ready,
                    "deepLink": self.deep_link(&m.outreach_id),
                }),
                timeline_id: None,
            };
        }

        let mut result = json!({});
        let mut used = Vec::new();
        let mut failures = 0;
        let mut written_id = None;
        let mut app_reached = false;
        if self.cfg.app {
            match self.sink.append(&item.to_string()).await {
                Ok(()) => {
                    written_id = Some(timeline_id.clone());
                    result["timeline"] = json!({"ok": true, "sink": self.sink.describe()});
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!(error = %e, "timeline append failed");
                    result["timeline"] =
                        json!({"ok": false, "error": text::clip(&e.to_string(), 200)});
                }
            }
            let notify = self.notify_body(m, written_id.as_deref());
            match self.api.notify(&notify).await {
                Ok(v) => {
                    let status = v
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("created")
                        .to_owned();
                    let pushed = v.get("pushed").and_then(Value::as_u64).unwrap_or(0);
                    // "present": the app is in front, he sees the timeline item.
                    app_reached = status == "present"
                        || (status == "created" && pushed > 0)
                        || written_id.is_some() && status == "duplicate";
                    result["notify"] = json!({"ok": true, "status": status, "pushed": pushed, "deferred": v.get("deferred")});
                    used.push("app");
                }
                Err(e) => {
                    failures += 1;
                    result["notify"] = json!({"ok": false, "error": e.to_string()});
                    if written_id.is_some() {
                        used.push("app");
                    }
                }
            }
        }
        let want_tg = self.telegram_planned(m)
            || (matches!(
                self.cfg.telegram,
                TelegramMode::Fallback | TelegramMode::High
            ) && !app_reached);
        if want_tg {
            match (&self.telegram, &self.chat_id) {
                (Some(tg), Some(chat)) => {
                    let link = self.deep_link(&m.outreach_id);
                    match tg
                        .send(
                            chat,
                            &m.text,
                            Some((lang::telegram_button(&m.language), &link)),
                        )
                        .await
                    {
                        Ok(id) => {
                            used.push("telegram");
                            result["telegram"] = json!({"ok": true, "messageId": id});
                        }
                        Err(e) => {
                            failures += 1;
                            result["telegram"] =
                                json!({"ok": false, "error": text::clip(&e.to_string(), 200)});
                        }
                    }
                }
                _ => result["telegram"] = json!({"ok": false, "error": "not configured"}),
            }
        }
        let status = match (used.is_empty(), failures) {
            (true, _) => "failed",
            (false, 0) => "sent",
            _ => "partial",
        };
        Delivery {
            channel: if used.is_empty() {
                "none".into()
            } else {
                used.join("+")
            },
            status: status.into(),
            result,
            timeline_id: written_id,
        }
    }
}

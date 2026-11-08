//! Telegram reminder delivery followed by best-effort recent chat, timeline and app effects.
use chrono::Utc;
use lucina_store::recent::RecentChat;
use lucina_store::reminders::Reminder;
use lucina_store::scheduler::{DeliveryOutcome, ReminderSink, TimelineSink};
use lucina_telegram::outcome::SendOutcome;
use lucina_telegram::{BotApi, SendOptions};
use serde_json::json;
use std::sync::Arc;

pub struct TelegramReminderSink {
    pub bot: BotApi,
    pub runtime: tokio::runtime::Handle,
    pub recent: Arc<RecentChat>,
    /// With the Telegram bot here: Node's chat buffer, used only while the
    /// poller holds the lease (before that Node may still write it).
    pub telegram_recent: Option<(Arc<RecentChat>, lucina_telegram::poller::SharedHealth)>,
    pub timeline: TimelineSink,
    pub secretary: crate::bridges::Secretary,
    pub app_notify: bool,
}
impl ReminderSink for TelegramReminderSink {
    fn deliver(&self, reminder: &Reminder, text: &str) -> DeliveryOutcome {
        self.runtime.block_on(async {
            let opts = SendOptions {
                disable_web_page_preview: true,
                ..Default::default()
            };
            if let Err(e) = self.bot.send_text(&reminder.chat_id, text, &opts).await {
                return match e.outcome {
                    SendOutcome::NotDelivered => DeliveryOutcome::NotDelivered(e.to_string()),
                    _ => DeliveryOutcome::Uncertain(e.to_string()),
                };
            }
            let recent = match &self.telegram_recent {
                Some((r, h)) if h.lock().unwrap_or_else(|e| e.into_inner()).lease_held => r,
                _ => &self.recent,
            };
            recent.append(&reminder.chat_id, "assistant", text, Utc::now());
            let item = self.timeline.record(reminder, text);
            if self.app_notify && self.timeline.is_owner(reminder) {
                let mut body = json!({
                    "category":"reminder", "body":text, "route":"/", "channel":"telegram",
                    "dedupKey":format!("reminder:{}",reminder.id),
                });
                if let Some(item) = item {
                    body["timelineId"] = json!(item.id);
                }
                // Notify is independent of reminder status and must not prolong the store lock.
                let secretary = self.secretary.clone();
                tokio::spawn(async move {
                    let result = crate::bridges::notify(&secretary, &body).await;
                    if !result.ok {
                        tracing::warn!(status = result.status, "reminder app notification failed");
                    }
                });
            }
            DeliveryOutcome::Delivered
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_store::reminders::{format_delivery, CreateReminder};
    use lucina_store::scheduler::{tick_with_health, SchedulerHealth};
    use lucina_store::{ReminderStore, TimelineStore};
    use std::sync::Mutex;
    use std::time::Duration;
    use wiremock::{
        matchers::{body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    const TOKEN: &str = "123:TEST_SECRET";
    struct Fixture {
        _dir: tempfile::TempDir,
        store: Arc<ReminderStore>,
        sink: Arc<TelegramReminderSink>,
        health: Arc<Mutex<SchedulerHealth>>,
    }
    impl Fixture {
        fn new(bot: &MockServer, app: &MockServer, timeout: Duration) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let token_file = dir.path().join("service.token");
            std::fs::write(&token_file, "test-service-token").unwrap();
            let store = Arc::new(ReminderStore::new(dir.path().join("r.json"), 100));
            store
                .create(CreateReminder {
                    text: "stretch".into(),
                    due_at: "2020-01-01T00:00:00.000Z".into(),
                    chat_id: "42".into(),
                    user_peer_id: "giuseppe".into(),
                    user_label: String::new(),
                    persona: "Lucina".into(),
                    time_zone: "UTC".into(),
                    dedupe_key: None,
                    source: "secretary".into(),
                })
                .unwrap();
            let sink = Arc::new(TelegramReminderSink {
                bot: BotApi::with_base_url(TOKEN.into(), &bot.uri()).with_timeout(timeout),
                runtime: tokio::runtime::Handle::current(),
                recent: Arc::new(RecentChat::new(Some(dir.path().join("recent.json")))),
                telegram_recent: None,
                timeline: TimelineSink {
                    timeline: Arc::new(TimelineStore::new(
                        dir.path().join("t.jsonl"),
                        1 << 20,
                        true,
                    )),
                    owner_chat_id: "42".into(),
                    owner_peer_id: "giuseppe".into(),
                },
                secretary: crate::bridges::Secretary {
                    http: reqwest::Client::new(),
                    url: Some(app.uri()),
                    token_file: Some(token_file),
                },
                app_notify: true,
            });
            Self {
                _dir: dir,
                store,
                sink,
                health: Arc::new(Mutex::new(SchedulerHealth {
                    enabled: true,
                    ..Default::default()
                })),
            }
        }
        async fn tick(&self) {
            let (store, sink, health) =
                (self.store.clone(), self.sink.clone(), self.health.clone());
            tokio::task::spawn_blocking(move || {
                tick_with_health(&store, sink.as_ref(), "Lucina", &health)
            })
            .await
            .unwrap();
        }
    }
    #[tokio::test]
    async fn reminder_delivered_once_with_node_text_options_recent_timeline_and_notify() {
        let bot = MockServer::start().await;
        let app = MockServer::start().await;
        let f = Fixture::new(&bot, &app, Duration::from_secs(2));
        let reminder = f.store.list(true).unwrap().remove(0);
        let text = format_delivery(&reminder);
        assert_eq!(text, "⏰ Reminder\n\nstretch");
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .and(body_json(
                json!({"chat_id":"42","text":text,"disable_web_page_preview":true}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok":true,"result":{"message_id":1}})),
            )
            .expect(1)
            .mount(&bot)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/notify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
            .expect(1)
            .mount(&app)
            .await;
        f.tick().await;
        f.tick().await;
        assert_eq!(f.store.list(true).unwrap()[0].status, "sent");
        assert!(f.store.list(true).unwrap()[0].sent_at.is_some());
        assert_eq!(f.sink.recent.turns("42")[0].text, text);
        let (items, _) = f.sink.timeline.timeline.list(None, 10).unwrap();
        assert_eq!(items.len(), 1);
        let item: serde_json::Value = serde_json::from_str(&items[0].raw).unwrap();
        assert_eq!(item["meta"], json!({"kind":"reminder","isDm":true}));
        for _ in 0..100 {
            if !app.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let requests = app.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].body_json::<serde_json::Value>().unwrap(),
            json!({"category":"reminder","body":text,"route":"/","channel":"telegram","dedupKey":format!("reminder:{}",reminder.id),"timelineId":item["id"]})
        );
        let h = f.health.lock().unwrap();
        assert_eq!(h.delivered, 1);
        assert_eq!(h.uncertain, 0);
        assert!(h.last_tick_at.is_some());
    }
    #[tokio::test]
    async fn rejected_sends_stay_pending() {
        for status in [429, 500] {
            let bot = MockServer::start().await;
            let app = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_json(
                        json!({"ok":false,"description":format!("rejected {TOKEN}")}),
                    ),
                )
                .expect(2)
                .mount(&bot)
                .await;
            let f = Fixture::new(&bot, &app, Duration::from_secs(1));
            f.tick().await;
            f.tick().await;
            let r = f.store.list(true).unwrap().remove(0);
            assert_eq!(r.status, "pending");
            assert!(r.last_error.is_some());
            assert!(!r.last_error.unwrap().contains(TOKEN));
            assert_eq!(f.health.lock().unwrap().not_delivered, 2);
            assert!(app.received_requests().await.unwrap().is_empty());
            assert!(f.sink.recent.turns("42").is_empty());
        }
    }
    #[tokio::test]
    async fn read_timeout_is_failed_and_second_tick_does_not_resend() {
        let bot = MockServer::start().await;
        let app = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(300))
                    .set_body_json(json!({"ok":true,"result":{}})),
            )
            .expect(1)
            .mount(&bot)
            .await;
        let f = Fixture::new(&bot, &app, Duration::from_millis(50));
        f.tick().await;
        f.tick().await;
        let r = f.store.list(true).unwrap().remove(0);
        assert_eq!(r.status, "failed");
        assert!(r
            .last_error
            .as_ref()
            .unwrap()
            .starts_with("delivery uncertain; not resent:"));
        assert!(!r.last_error.unwrap().contains(TOKEN));
        assert_eq!(f.health.lock().unwrap().uncertain, 1);
        assert!(app.received_requests().await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn stale_claim_is_not_resent_and_other_personas_are_not_delivered() {
        let bot = MockServer::start().await;
        let app = MockServer::start().await;
        let f = Fixture::new(&bot, &app, Duration::from_secs(1));
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.store.path()).unwrap()).unwrap();
        file["reminders"][0]["status"] = json!("sending");
        let mut other = file["reminders"][0].clone();
        other["id"] = json!("other");
        other["status"] = json!("pending");
        other["persona"] = json!("Poko");
        file["reminders"].as_array_mut().unwrap().push(other);
        std::fs::write(f.store.path(), serde_json::to_vec(&file).unwrap()).unwrap();
        f.tick().await;
        let all = f.store.list(true).unwrap();
        assert_eq!(all[0].status, "failed");
        assert_eq!(
            all[0].last_error.as_deref(),
            Some("interrupted; not resent")
        );
        assert_eq!(all[1].status, "pending");
        assert!(bot.received_requests().await.unwrap().is_empty());
    }
}

//! Due-reminder scheduler. **Disabled by default** (`LUCINA_SCHEDULER=false`):
//! while the Node Lucina is live it owns delivery, and two writers of
//! `reminders.json` would race. When enabled here, a delivery is recorded in
//! the owner timeline (Telegram delivery is not part of this crate).

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;

use crate::reminders::{format_delivery, Reminder, ReminderStore};
use crate::timeline::{Meta, NewItem, TimelineStore};

/// Only definite rejection is retried; ambiguous sends must never be resent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Delivered,
    NotDelivered(String),
    Uncertain(String),
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerHealth {
    pub enabled: bool,
    pub last_tick_at: Option<String>,
    pub last_error: Option<String>,
    pub delivered: u64,
    pub not_delivered: u64,
    pub uncertain: u64,
}

/// Where a due reminder goes.
pub trait ReminderSink: Send + Sync + 'static {
    fn deliver(&self, reminder: &Reminder, text: &str) -> DeliveryOutcome;
}

/// Records owner reminders in the timeline as Node does
/// (`channel: "telegram"`, `meta: {kind: "reminder", isDm}`).
pub struct TimelineSink {
    pub timeline: Arc<TimelineStore>,
    pub owner_chat_id: String,
    pub owner_peer_id: String,
}

impl TimelineSink {
    pub fn record(&self, reminder: &Reminder, text: &str) -> Option<crate::timeline::TimelineItem> {
        let is_owner = (!self.owner_chat_id.is_empty() && reminder.chat_id == self.owner_chat_id)
            || (!reminder.user_peer_id.is_empty() && reminder.user_peer_id == self.owner_peer_id);
        if is_owner {
            return self.timeline.record(NewItem {
                channel: "telegram".into(),
                role: "lucina".into(),
                text: text.to_owned(),
                meta: Some(Meta {
                    is_dm: Some(reminder.chat_id == self.owner_chat_id),
                    ..Meta::kind("reminder")
                }),
                ..NewItem::default()
            });
        }
        None
    }
    pub fn is_owner(&self, reminder: &Reminder) -> bool {
        (!self.owner_chat_id.is_empty() && reminder.chat_id == self.owner_chat_id)
            || (!reminder.user_peer_id.is_empty() && reminder.user_peer_id == self.owner_peer_id)
    }
}
impl ReminderSink for TimelineSink {
    fn deliver(&self, reminder: &Reminder, text: &str) -> DeliveryOutcome {
        self.record(reminder, text);
        DeliveryOutcome::Delivered
    }
}

/// One scheduler pass (compatibility helper for callers without health reporting).
pub fn tick(store: &ReminderStore, sink: &dyn ReminderSink, persona: &str) -> usize {
    tick_with_health(
        store,
        sink,
        persona,
        &Mutex::new(SchedulerHealth::default()),
    )
}

pub fn tick_with_health(
    store: &ReminderStore,
    sink: &dyn ReminderSink,
    _persona: &str,
    health: &Mutex<SchedulerHealth>,
) -> usize {
    let result = store.process_due(Utc::now(), |r| sink.deliver(r, &format_delivery(r)));
    let mut h = health.lock().unwrap_or_else(|e| e.into_inner());
    h.last_tick_at = Some(crate::timeutil::now_iso());
    h.last_error = None;
    match result {
        Ok(due) => {
            for r in &due {
                match r.status.as_str() {
                    "sent" => h.delivered += 1,
                    "pending" => h.not_delivered += 1,
                    "failed" => h.uncertain += 1,
                    _ => {}
                }
                if r.last_error.is_some() {
                    h.last_error = r.last_error.clone();
                }
            }
            due.len()
        }
        Err(e) => {
            tracing::error!(error = %e, "reminder scheduler tick failed");
            h.last_error = Some(e.to_string());
            0
        }
    }
}

/// Start the loop (only when `LUCINA_SCHEDULER=true`).
pub fn spawn(
    store: Arc<ReminderStore>,
    sink: Arc<dyn ReminderSink>,
    persona: String,
    every: Duration,
    initial_delay: Duration,
    health: Arc<Mutex<SchedulerHealth>>,
) -> tokio::task::JoinHandle<()> {
    tracing::info!(
        poll_ms = every.as_millis() as u64,
        "reminder scheduler started"
    );
    tokio::spawn(async move {
        // Node starts a 15s initial timer and a poll interval measured from startup.
        let start = tokio::time::Instant::now();
        let every = every.max(Duration::from_millis(1));
        let mut interval = tokio::time::interval_at(start + every, every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let initial = tokio::time::sleep(initial_delay);
        tokio::pin!(initial);
        let mut initial_done = false;
        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = &mut initial, if !initial_done => { initial_done = true; },
            }
            let (s, k, p, h) = (store.clone(), sink.clone(), persona.clone(), health.clone());
            if let Err(e) =
                tokio::task::spawn_blocking(move || tick_with_health(&s, k.as_ref(), &p, &h)).await
            {
                let mut h = health.lock().unwrap_or_else(|e| e.into_inner());
                h.last_tick_at = Some(crate::timeutil::now_iso());
                h.last_error = Some(format!("scheduler task failed: {e}"));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminders::CreateReminder;

    #[test]
    fn delivers_owner_reminders_to_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReminderStore::new(dir.path().join("r.json"), 100);
        store
            .create(CreateReminder {
                text: "stretch".into(),
                due_at: "2020-01-01T00:00:00.000Z".into(),
                chat_id: "42".into(),
                user_peer_id: "giuseppe".into(),
                user_label: String::new(),
                persona: "Lucina".into(),
                time_zone: "Europe/Rome".into(),
                dedupe_key: None,
                source: "secretary".into(),
            })
            .unwrap();
        let timeline = Arc::new(TimelineStore::new(
            dir.path().join("t.jsonl"),
            1 << 20,
            true,
        ));
        let sink = TimelineSink {
            timeline: timeline.clone(),
            owner_chat_id: "42".into(),
            owner_peer_id: "giuseppe".into(),
        };
        assert_eq!(tick(&store, &sink, "Lucina"), 1);
        assert_eq!(tick(&store, &sink, "Lucina"), 0);
        let (items, _) = timeline.list(None, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]
            .raw
            .contains(r#""meta":{"kind":"reminder","isDm":true}"#));
        assert!(items[0].raw.contains("⏰ Reminder\\n\\nstretch"));
    }
}

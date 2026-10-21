//! Due-reminder scheduler. **Disabled by default** (`LUCINA_SCHEDULER=false`):
//! while the Node Lucina is live it owns delivery, and two writers of
//! `reminders.json` would race. When enabled here, a delivery is recorded in
//! the owner timeline (Telegram delivery is not part of this crate).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::reminders::{format_delivery, Reminder, ReminderStore};
use crate::timeline::{Meta, NewItem, TimelineStore};

/// Where a due reminder goes. `Err` keeps it pending with `lastError`.
pub trait ReminderSink: Send + Sync + 'static {
    fn deliver(&self, reminder: &Reminder, text: &str) -> Result<(), String>;
}

/// Records owner reminders in the timeline as Node does
/// (`channel: "telegram"`, `meta: {kind: "reminder", isDm}`).
pub struct TimelineSink {
    pub timeline: Arc<TimelineStore>,
    pub owner_chat_id: String,
    pub owner_peer_id: String,
}

impl ReminderSink for TimelineSink {
    fn deliver(&self, reminder: &Reminder, text: &str) -> Result<(), String> {
        let is_owner = (!self.owner_chat_id.is_empty() && reminder.chat_id == self.owner_chat_id)
            || (!reminder.user_peer_id.is_empty() && reminder.user_peer_id == self.owner_peer_id);
        if is_owner {
            self.timeline.record(NewItem {
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
        Ok(())
    }
}

/// One scheduler pass.
pub fn tick(store: &ReminderStore, sink: &dyn ReminderSink, persona: &str) -> usize {
    match store.process_due(Utc::now(), |r| {
        if !r.persona.is_empty() && r.persona != persona {
            return Ok(());
        }
        sink.deliver(r, &format_delivery(r))
    }) {
        Ok(due) => {
            if !due.is_empty() {
                tracing::info!(count = due.len(), "reminder scheduler tick delivered");
            }
            due.len()
        }
        Err(e) => {
            tracing::error!(error = %e, "reminder scheduler tick failed");
            0
        }
    }
}

/// Start the loop (only when `LUCINA_SCHEDULER=true`).
pub fn spawn(store: Arc<ReminderStore>, sink: Arc<dyn ReminderSink>, persona: String, every: Duration) -> tokio::task::JoinHandle<()> {
    tracing::info!(poll_ms = every.as_millis() as u64, "reminder scheduler started");
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(every.max(Duration::from_secs(1)));
        loop {
            interval.tick().await;
            let (s, k, p) = (store.clone(), sink.clone(), persona.clone());
            let _ = tokio::task::spawn_blocking(move || tick(&s, k.as_ref(), &p)).await;
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
        let timeline = Arc::new(TimelineStore::new(dir.path().join("t.jsonl"), 1 << 20, true));
        let sink = TimelineSink {
            timeline: timeline.clone(),
            owner_chat_id: "42".into(),
            owner_peer_id: "giuseppe".into(),
        };
        assert_eq!(tick(&store, &sink, "Lucina"), 1);
        assert_eq!(tick(&store, &sink, "Lucina"), 0);
        let (items, _) = timeline.list(None, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].raw.contains(r#""meta":{"kind":"reminder","isDm":true}"#));
        assert!(items[0].raw.contains("⏰ Reminder\\n\\nstretch"));
    }
}

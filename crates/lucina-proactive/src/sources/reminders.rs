//! Lucina's reminders (the Node store, read only):
//!
//! - `reminder_overdue`: still `pending` more than `overdue_grace_minutes`
//!   after it was due, so the Node scheduler did not deliver it (a
//!   `lastError` makes that certain);
//! - `due_no_plan`: a deadline-like reminder (scadenza, deadline, pay,
//!   renew, file, frist, ...) due within `due_no_plan_days` with nothing in
//!   the calendar about it. One event per day-bucket, so it is looked at
//!   again when it gets closer.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;

use super::{ReminderFeed, Source};
use crate::config::Config;
use crate::model::{Event, Kind};
use crate::score;
use crate::secretary::SecretaryApi;
use crate::store::Store;
use crate::text;

const DEADLINE_WORDS: &[&str] = &[
    "scadenza",
    "scade",
    "deadline",
    "due",
    "pagare",
    "paga",
    "pay",
    "payment",
    "pagamento",
    "rinnova",
    "rinnovo",
    "renew",
    "renewal",
    "file",
    "submit",
    "invia",
    "consegna",
    "consegnare",
    "dichiarazione",
    "tax",
    "tasse",
    "vat",
    "iva",
    "moms",
    "frist",
    "betal",
    "betaling",
    "forny",
    "indsend",
    "expiry",
    "expires",
    "prenota",
    "book",
];

pub fn looks_like_deadline(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| DEADLINE_WORDS.contains(&w))
}

pub struct ReminderSource {
    pub cfg: Arc<Config>,
    pub feed: Arc<dyn ReminderFeed>,
    pub api: Arc<dyn SecretaryApi>,
}

#[async_trait::async_trait]
impl Source for ReminderSource {
    fn name(&self) -> &'static str {
        "reminders"
    }

    fn schedule(&self) -> (Duration, Duration) {
        let s = &self.cfg.sources.reminders;
        (
            Duration::from_secs(s.interval_secs),
            Duration::from_secs(s.jitter_secs),
        )
    }

    async fn observe(&self, now: DateTime<Utc>, _store: &dyn Store) -> Result<Vec<Event>> {
        let sc = &self.cfg.sources;
        let pending: Vec<_> = self
            .feed
            .reminders(now)?
            .into_iter()
            .filter(|r| r.status == "pending")
            .collect();
        let mut out = Vec::new();
        let grace = chrono::Duration::minutes(sc.overdue_grace_minutes);
        for r in &pending {
            let Some(due) = r.due() else { continue };
            if due + grace < now && now - due < chrono::Duration::days(2) {
                let late = (now - due).num_minutes();
                let err = r.last_error.as_deref().is_some_and(|e| !e.is_empty());
                out.push(
                    Event::new(
                        "reminders",
                        Kind::ReminderOverdue,
                        format!("reminder_overdue:{}", r.id),
                        now,
                        text::clip(&r.text, 120),
                    )
                    .expires(due + chrono::Duration::hours(12))
                    .scores(score::reminder_overdue(late, err))
                    .signal(format!(
                        "reminder due {late} min ago is still pending{}",
                        if err { " (delivery error)" } else { "" }
                    ))
                    .payload(json!({"reminder": {"text": r.text, "dueAt": r.due_at}, "minutesLate": late})),
                );
            }
        }
        let horizon = now + chrono::Duration::days(sc.due_no_plan_days);
        let deadlines: Vec<_> = pending
            .iter()
            .filter(|r| looks_like_deadline(&r.text))
            .filter_map(|r| r.due().map(|d| (r, d)))
            .filter(|(_, d)| *d > now && *d <= horizon)
            .collect();
        if deadlines.is_empty() {
            return Ok(out);
        }
        let calendar = self.api.calendar_events(now, horizon).await.ok();
        for (r, due) in deadlines {
            let planned = calendar
                .as_ref()
                .is_some_and(|evs| evs.iter().any(|e| text::related(&e.title, &r.text)));
            if planned {
                continue;
            }
            let hours = (due - now).num_hours();
            let bucket = match hours {
                h if h <= 24 => "1d",
                h if h <= 48 => "2d",
                _ => "3d",
            };
            let mut scores = score::due_no_plan(hours);
            if calendar.is_none() {
                // Could not look at the calendar: less sure there is no plan.
                scores.confidence = 0.65;
            }
            out.push(
                Event::new(
                    "reminders",
                    Kind::DueNoPlan,
                    format!("due_no_plan:{}:{bucket}", r.id),
                    now,
                    text::clip(&r.text, 120),
                )
                .expires(due)
                .scores(scores)
                .signal(format!(
                    "deadline-like reminder due in {hours} h, nothing in the calendar for it"
                ))
                .payload(
                    json!({"reminder": {"text": r.text, "dueAt": r.due_at}, "hoursLeft": hours}),
                ),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_words() {
        assert!(looks_like_deadline(
            "Danish VAT Q3 2026 (deadline 1 December 2026)"
        ));
        assert!(looks_like_deadline("Scadenza accesso premium BlockP"));
        assert!(looks_like_deadline("Renew pokoin.com before expiry"));
        assert!(!looks_like_deadline("Chiama la mamma"));
    }
}

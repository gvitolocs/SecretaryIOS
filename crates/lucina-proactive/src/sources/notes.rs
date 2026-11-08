//! Notes → `note_for_event`: a pinned note whose topic has a calendar event
//! coming up (after the meeting-prep window, within 36 hours), e.g. the
//! pinned `craftit-meeting` briefing and tomorrow's CRAFT-IT4SD meeting.
//! It goes to the Pulse; the meeting itself gets a prep message later.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;

use super::Source;
use crate::config::Config;
use crate::model::{Event, Kind};
use crate::score;
use crate::secretary::SecretaryApi;
use crate::store::Store;
use crate::text;

pub const HORIZON_HOURS: i64 = 36;

pub struct NotesSource {
    pub cfg: Arc<Config>,
    pub api: Arc<dyn SecretaryApi>,
}

#[async_trait::async_trait]
impl Source for NotesSource {
    fn name(&self) -> &'static str {
        "notes"
    }

    fn schedule(&self) -> (Duration, Duration) {
        let s = &self.cfg.sources.notes;
        (
            Duration::from_secs(s.interval_secs),
            Duration::from_secs(s.jitter_secs),
        )
    }

    async fn observe(&self, now: DateTime<Utc>, _store: &dyn Store) -> Result<Vec<Event>> {
        let pinned: Vec<_> = self
            .api
            .notes(None)
            .await?
            .into_iter()
            .filter(|n| n.pinned && !n.archived)
            .collect();
        if pinned.is_empty() {
            return Ok(Vec::new());
        }
        let from = now + chrono::Duration::minutes(self.cfg.sources.prep_max_lead_minutes);
        let to = now + chrono::Duration::hours(HORIZON_HOURS);
        let events = self.api.calendar_events(from, to).await?;
        let mut out = Vec::new();
        for n in &pinned {
            let topic = format!(
                "{} {} {}",
                n.title,
                n.slug.replace('-', " "),
                n.tags.join(" ")
            );
            for e in events.iter().filter(|e| text::related(&e.title, &topic)) {
                let Some(start) = e.starts() else { continue };
                let hours = (start - now).num_hours();
                out.push(
                    Event::new(
                        "notes",
                        Kind::NoteForEvent,
                        format!("note_for_event:{}:{}", n.slug, e.id),
                        now,
                        format!("{} → {}", text::clip(&n.title, 60), text::clip(&e.title, 60)),
                    )
                    .expires(start)
                    .scores(score::note_for_event(hours))
                    .signal(format!("pinned note \"{}\" matches an event in {hours} h", n.slug))
                    .payload(json!({
                        "note": {"slug": n.slug, "title": n.title, "excerpt": text::clip(&n.excerpt, 300)},
                        "event": {"id": e.id, "title": e.title, "start": e.start, "end": e.end, "allDay": e.all_day, "location": e.location},
                    })),
                );
            }
        }
        Ok(out)
    }
}

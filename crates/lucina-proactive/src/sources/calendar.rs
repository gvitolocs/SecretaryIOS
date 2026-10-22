//! Calendar → meeting prep. A timed event that starts within the prep
//! window (`prep_min_lead_minutes`–`prep_max_lead_minutes`, default
//! 20–90 min) becomes a `meeting_prep` event with what Lucina can prepare:
//! related notes (search plus pinned notes on the same topic), Honcho
//! memory and recent timeline mentions. With nothing to prepare it scores
//! below the send threshold (the calendar app already reminds him).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::{MemoryFeed, Source, TimelineFeed};
use crate::config::Config;
use crate::model::{Event, Kind};
use crate::score;
use crate::secretary::{CalEvent, NoteSummary, SecretaryApi};
use crate::store::Store;
use crate::text;

pub struct CalendarSource {
    pub cfg: Arc<Config>,
    pub api: Arc<dyn SecretaryApi>,
    pub memory: Option<Arc<dyn MemoryFeed>>,
    pub timeline: Arc<dyn TimelineFeed>,
}

/// Notes related to `title`: a search on its keywords plus pinned notes on the same topic.
pub async fn related_notes(api: &dyn SecretaryApi, title: &str) -> Vec<NoteSummary> {
    let mut out: Vec<NoteSummary> = Vec::new();
    let keys = text::keywords(title);
    // The two longest keywords are the most specific.
    let mut ks: Vec<&String> = keys.iter().collect();
    ks.sort_by_key(|k| std::cmp::Reverse(k.chars().count()));
    for k in ks.into_iter().take(2) {
        if let Ok(found) = api.notes(Some(k)).await {
            for n in found {
                if !out.iter().any(|o| o.slug == n.slug) {
                    out.push(n);
                }
            }
        }
    }
    if let Ok(all) = api.notes(None).await {
        for n in all.into_iter().filter(|n| n.pinned) {
            let topic = format!(
                "{} {} {}",
                n.title,
                n.slug.replace('-', " "),
                n.tags.join(" ")
            );
            if text::related(title, &topic) && !out.iter().any(|o| o.slug == n.slug) {
                out.push(n);
            }
        }
    }
    // Keep only notes really about it (a search hit on a common word is not).
    out.retain(|n| {
        let topic = format!(
            "{} {} {} {}",
            n.title,
            n.slug.replace('-', " "),
            n.tags.join(" "),
            n.excerpt
        );
        text::related(title, &topic)
    });
    // Lucina's own prep notes are outputs, not material.
    out.retain(|n| !n.tags.iter().any(|t| t == "lucina-prep"));
    out.truncate(3);
    out
}

fn event_facts(e: &CalEvent, now: DateTime<Utc>) -> Value {
    json!({
        "title": e.title,
        "start": e.start,
        "end": e.end,
        "location": e.location,
        "calendar": e.calendar,
        "description": e.description.as_deref().map(|d| text::clip(d, 400)),
        "startsInMinutes": (e.start_ms - now.timestamp_millis()) / 60_000,
    })
}

#[async_trait::async_trait]
impl Source for CalendarSource {
    fn name(&self) -> &'static str {
        "calendar"
    }

    fn schedule(&self) -> (Duration, Duration) {
        let s = &self.cfg.sources.calendar;
        (
            Duration::from_secs(s.interval_secs),
            Duration::from_secs(s.jitter_secs),
        )
    }

    async fn observe(&self, now: DateTime<Utc>, _store: &dyn Store) -> Result<Vec<Event>> {
        let sc = &self.cfg.sources;
        let from = now + chrono::Duration::minutes(sc.prep_min_lead_minutes);
        let to = now + chrono::Duration::minutes(sc.prep_max_lead_minutes);
        let events = self
            .api
            .calendar_events(now, to + chrono::Duration::minutes(1))
            .await?;
        let mut out = Vec::new();
        for e in events.iter().filter(|e| !e.all_day) {
            let Some(start) = e.starts() else { continue };
            if start < from || start > to {
                continue;
            }
            let minutes = (start - now).num_minutes();
            let notes = related_notes(self.api.as_ref(), &e.title).await;
            // The two best notes in full (clipped), the rest as excerpts.
            let mut note_facts = Vec::new();
            for (i, n) in notes.iter().enumerate() {
                let full =
                    if i < 2 {
                        self.api.note(&n.slug).await.ok().flatten().and_then(|v| {
                            v.get("content").and_then(Value::as_str).map(str::to_owned)
                        })
                    } else {
                        None
                    };
                let (body, truncated) = match full {
                    Some(c) => (text::clip(&c, 3000), c.chars().count() > 3000),
                    None => (n.excerpt.clone(), true),
                };
                note_facts.push(json!({"slug": n.slug, "title": n.title, "tags": n.tags, "content": body, "truncated": truncated}));
            }
            let mut memory = Vec::new();
            if let Some(m) = &self.memory {
                memory = m
                    .search(&e.title, 5)
                    .await
                    .into_iter()
                    .filter(|c| text::related(&e.title, c))
                    .map(|c| text::clip(&c, 300))
                    .take(3)
                    .collect();
            }
            let mentions: Vec<String> = self
                .timeline
                .items(now - chrono::Duration::days(14), now)?
                .into_iter()
                .rev()
                .filter(|i| i.role == "user" && text::related(&e.title, &i.text))
                .take(3)
                .map(|i| text::clip(&i.text, 300))
                .collect();
            let material = !notes.is_empty() || !memory.is_empty() || !mentions.is_empty();
            let signal = if material {
                format!(
                    "\"{}\" starts in {minutes} min; material found: {} note(s), {} memory, {} mention(s)",
                    text::clip(&e.title, 60),
                    notes.len(),
                    memory.len(),
                    mentions.len()
                )
            } else {
                format!(
                    "\"{}\" starts in {minutes} min; nothing to prepare",
                    text::clip(&e.title, 60)
                )
            };
            let ev = Event::new(
                "calendar",
                Kind::MeetingPrep,
                format!("meeting_prep:{}:{}", e.id, e.start_ms),
                now,
                text::clip(&e.title, 120),
            )
            .expires(start - chrono::Duration::minutes(5))
            .scores(score::meeting_prep(
                minutes,
                sc.prep_max_lead_minutes,
                material,
            ))
            .signal(signal)
            .payload(json!({
                "event": event_facts(e, now),
                "notes": note_facts,
                "memory": memory,
                "mentions": mentions,
            }));
            out.push(ev);
        }
        Ok(out)
    }
}

//! The timeline → "something we were following". An owner message from the
//! last `thread_lookback_days` that
//!
//! - asks to be told ("fammi sapere", "let me know", "lad mig vide", ...) or
//!   is about something pending or broken ("non funziona", "down", "in
//!   attesa", "deploy", "refund", "virker ikke", ...),
//! - and went quiet: no later owner message on the same topic, and at least
//!   `thread_quiet_hours` old,
//!
//! is an open thread. Rules pick and rank them (asked-to-be-told first,
//! then newest); DeepSeek judges only the top `judge_max_per_tick` ("resolved
//! / still open / unknown", from the evidence given, never guessing), at
//! most `judge_max_per_day` times a day, and each verdict is cached until
//! new evidence arrives or `judge_recheck_hours` pass.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Entry, Source, TimelineFeed};
use crate::config::Config;
use crate::model::{Event, Kind};
use crate::score;
use crate::store::Store;
use crate::text;

const FOLLOW: &[&str] = &[
    "fammi sapere",
    "fammelo sapere",
    "tienimi aggiornat",
    "aggiornami",
    "avvisami",
    "dimmi quando",
    "dimmi se",
    "fammi un fischio",
    "let me know",
    "keep me posted",
    "keep me updated",
    "tell me when",
    "tell me if",
    "notify me",
    "ping me",
    "lad mig vide",
    "sig til når",
    "sig til hvis",
    "giv besked",
    "hold mig opdateret",
];
const STATUS: &[&str] = &[
    "non funziona",
    "non va",
    "è giù",
    "down",
    "errore",
    "error",
    "bug",
    "problema",
    "rotto",
    "broken",
    "failing",
    "fallisce",
    "fallito",
    "crash",
    "bloccat",
    "stuck",
    "lentissim",
    "in attesa",
    "aspetto",
    "waiting",
    "pending",
    "sospes",
    "deploy",
    "build",
    "rimborso",
    "refund",
    "spedizion",
    "shipping",
    "pacco",
    "tracking",
    "virker ikke",
    "fejl",
    "venter",
    "nede",
    "ancora rotto",
    "still broken",
    "not working",
    "risolto?",
    "fixed?",
];

pub fn follow_requested(text: &str) -> bool {
    let l = text.to_lowercase();
    FOLLOW.iter().any(|m| l.contains(m))
}

pub fn about_something_pending(text: &str) -> bool {
    let l = text.to_lowercase();
    STATUS.iter().any(|m| {
        // Whole-word for the short English ones ("down" not "download").
        if m.len() <= 5 && m.is_ascii() && !m.contains(' ') {
            l.split(|c: char| !c.is_alphanumeric()).any(|w| w == *m)
        } else {
            l.contains(m)
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub anchor: Entry,
    pub reply: Option<Entry>,
    /// Later items on the same topic (Lucina's, e.g. a delivered reminder).
    pub later: Vec<Entry>,
    pub follow: bool,
}

/// Find the quiet open threads in `items` (oldest first), best first.
pub fn find_threads(
    items: &[Entry],
    now: DateTime<Utc>,
    lookback_days: i64,
    quiet_hours: i64,
) -> Vec<Thread> {
    let owner: Vec<&Entry> = items
        .iter()
        .filter(|e| e.role == "user")
        .filter(|e| e.meta.get("via").and_then(Value::as_str) != Some("mcp"))
        .filter(|e| e.text.trim().chars().count() >= 8)
        .collect();
    let mut out = Vec::new();
    for (i, m) in owner.iter().enumerate() {
        if m.ts < now - chrono::Duration::days(lookback_days)
            || m.ts > now - chrono::Duration::hours(quiet_hours)
        {
            continue;
        }
        let follow = follow_requested(&m.text);
        if !follow && !about_something_pending(&m.text) {
            continue;
        }
        if owner[i + 1..]
            .iter()
            .any(|n| text::related(&m.text, &n.text))
        {
            continue; // he came back to it: the later message is the thread
        }
        let after: Vec<&Entry> = items
            .iter()
            .filter(|e| e.ts > m.ts || (e.ts == m.ts && e.id > m.id))
            .collect();
        let reply = after
            .iter()
            .find(|e| e.role == "lucina")
            .map(|e| (*e).clone());
        let later = after
            .iter()
            .filter(|e| e.role == "lucina" && Some(&e.id) != reply.as_ref().map(|r| &r.id))
            .filter(|e| text::related(&m.text, &e.text))
            .take(5)
            .map(|e| (*e).clone())
            .collect();
        out.push(Thread {
            anchor: (*m).clone(),
            reply,
            later,
            follow,
        });
    }
    out.sort_by(|a, b| b.follow.cmp(&a.follow).then(b.anchor.ts.cmp(&a.anchor.ts)));
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgement {
    /// `resolved`, `open` or `unknown`.
    pub status: String,
    pub confidence: f32,
    /// One sentence, in the owner's language if possible.
    pub summary: String,
}

/// "Resolved / still open?" (DeepSeek in production, scripted in tests).
#[async_trait::async_trait]
pub trait Judge: Send + Sync {
    async fn judge(&self, facts: &Value) -> Option<Judgement>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cached {
    status: String,
    confidence: f32,
    summary: String,
    judged_at: Option<DateTime<Utc>>,
    evidence: usize,
}

pub struct ThreadSource {
    pub cfg: Arc<Config>,
    pub timeline: Arc<dyn TimelineFeed>,
    pub judge: Option<Arc<dyn Judge>>,
}

impl ThreadSource {
    async fn health_lines(&self, store: &dyn Store) -> Vec<Value> {
        let mut out = Vec::new();
        for t in &self.cfg.health.targets {
            if let Ok(Some(s)) = store.get_state("health", &t.name).await {
                out.push(json!({"service": t.name, "down": s.get("down"), "lastError": s.get("lastError"), "checkedAt": s.get("checkedAt")}));
            }
        }
        out
    }
}

fn facts(t: &Thread, now: DateTime<Utc>, health: &[Value]) -> Value {
    let item = |e: &Entry| json!({"at": e.ts, "text": text::clip(&e.text, 600)});
    json!({
        "now": now,
        "ownerMessage": item(&t.anchor),
        "lucinaReply": t.reply.as_ref().map(item),
        "laterRelated": t.later.iter().map(item).collect::<Vec<_>>(),
        "serviceHealth": health,
        "askedToBeTold": t.follow,
    })
}

#[async_trait::async_trait]
impl Source for ThreadSource {
    fn name(&self) -> &'static str {
        "timeline"
    }

    fn schedule(&self) -> (Duration, Duration) {
        let s = &self.cfg.sources.timeline;
        (
            Duration::from_secs(s.interval_secs),
            Duration::from_secs(s.jitter_secs),
        )
    }

    async fn observe(&self, now: DateTime<Utc>, store: &dyn Store) -> Result<Vec<Event>> {
        let sc = &self.cfg.sources;
        let Some(judge) = &self.judge else {
            return Ok(Vec::new()); // nothing verifies them: say nothing
        };
        let items = self.timeline.items(
            now - chrono::Duration::days(sc.thread_lookback_days + 1),
            now,
        )?;
        let threads = find_threads(&items, now, sc.thread_lookback_days, sc.thread_quiet_hours);
        if threads.is_empty() {
            return Ok(Vec::new());
        }
        let tz = self.cfg.tz();
        let today = now.with_timezone(&tz).date_naive().to_string();
        let mut budget = store
            .get_state("threads", "budget")
            .await?
            .filter(|v| v.get("date").and_then(Value::as_str) == Some(today.as_str()))
            .and_then(|v| v.get("count").and_then(Value::as_u64))
            .unwrap_or(0) as usize;
        let health = self.health_lines(store).await;
        let mut judged_now = 0;
        let mut out = Vec::new();
        for t in threads.iter().take(sc.judge_max_per_tick.max(1) * 2) {
            let key = t.anchor.id.clone();
            let cached: Option<Cached> = store
                .get_state("threads", &key)
                .await?
                .and_then(|v| serde_json::from_value(v).ok());
            let fresh = cached.as_ref().is_some_and(|c| {
                c.evidence == t.later.len()
                    && c.judged_at
                        .is_some_and(|j| now - j < chrono::Duration::hours(sc.judge_recheck_hours))
            });
            let verdict = if fresh {
                cached
            } else if judged_now < sc.judge_max_per_tick && budget < sc.judge_max_per_day {
                judged_now += 1;
                budget += 1;
                let j = judge.judge(&facts(t, now, &health)).await;
                let c = j.map(|j| Cached {
                    status: j.status,
                    confidence: j.confidence.clamp(0.0, 1.0),
                    summary: j.summary,
                    judged_at: Some(now),
                    evidence: t.later.len(),
                });
                if let Some(c) = &c {
                    store
                        .put_state("threads", &key, &serde_json::to_value(c)?, now)
                        .await?;
                }
                c
            } else {
                cached
            };
            let Some(v) = verdict else { continue };
            let payload = json!({
                "ownerMessage": {"at": t.anchor.ts, "text": text::clip(&t.anchor.text, 400)},
                "summary": v.summary,
                "askedToBeTold": t.follow,
            });
            match v.status.as_str() {
                "resolved" => out.push(
                    Event::new(
                        "timeline",
                        Kind::ThreadResolved,
                        format!("thread_resolved:{key}"),
                        now,
                        text::clip(&t.anchor.text, 120),
                    )
                    .expires(now + chrono::Duration::hours(24))
                    .scores(score::thread_resolved(v.confidence, t.follow))
                    .signal(format!(
                        "a thread from {} looks resolved (judge {:.2}){}",
                        t.anchor.ts.with_timezone(&tz).format("%d/%m %H:%M"),
                        v.confidence,
                        if t.follow {
                            "; he asked to be told"
                        } else {
                            ""
                        }
                    ))
                    .payload(payload),
                ),
                "open" => out.push(
                    Event::new(
                        "timeline",
                        Kind::ThreadOpen,
                        format!("thread_open:{key}:{today}"),
                        now,
                        text::clip(&t.anchor.text, 120),
                    )
                    .expires(now + chrono::Duration::hours(20))
                    .scores(score::thread_open(v.confidence))
                    .signal(format!(
                        "a thread from {} is still open (judge {:.2})",
                        t.anchor.ts.with_timezone(&tz).format("%d/%m %H:%M"),
                        v.confidence
                    ))
                    .payload(payload),
                ),
                _ => {}
            }
        }
        store
            .put_state(
                "threads",
                "budget",
                &json!({"date": today, "count": budget}),
                now,
            )
            .await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn e(id: &str, ts: &str, role: &str, text: &str) -> Entry {
        Entry {
            id: id.into(),
            ts: at(ts),
            role: role.into(),
            channel: "telegram".into(),
            text: text.into(),
            meta: Value::Null,
        }
    }

    #[test]
    fn markers() {
        assert!(follow_requested("Controlla pokoin.com e fammi sapere"));
        assert!(about_something_pending("il deploy di cardvault è bloccato"));
        assert!(about_something_pending("is the site down?"));
        assert!(!about_something_pending("download the file"));
        assert!(!about_something_pending("buongiorno! che tempo fa?"));
    }

    #[test]
    fn quiet_threads_only() {
        let now = at("2026-10-22T12:00:00Z");
        let items = vec![
            e(
                "A",
                "2026-10-21T08:00:00Z",
                "user",
                "Il checkout di pokoin non funziona, fammi sapere",
            ),
            e("B", "2026-10-21T08:00:05Z", "lucina", "Ci guardo"),
            e(
                "C",
                "2026-10-21T09:00:00Z",
                "user",
                "Il build di cardvault fallisce ancora",
            ),
            e(
                "D",
                "2026-10-21T15:00:00Z",
                "user",
                "cardvault build adesso ok?",
            ),
            e(
                "E",
                "2026-10-22T10:00:00Z",
                "user",
                "la stampante è rotta di nuovo",
            ),
            e("F", "2026-10-18T10:00:00Z", "user", "il server è down"),
            e(
                "G",
                "2026-10-21T08:30:00Z",
                "user",
                "che bella giornata oggi",
            ),
            e(
                "H",
                "2026-10-21T20:00:00Z",
                "lucina",
                "Promemoria: checkout pokoin da ricontrollare",
            ),
        ];
        let t = find_threads(&items, now, 3, 12);
        let ids: Vec<&str> = t.iter().map(|t| t.anchor.id.as_str()).collect();
        // A (asked to be told) first; C superseded by D; E too recent; F too old; G no marker.
        assert_eq!(ids, vec!["A", "D"]);
        assert!(t[0].follow);
        assert_eq!(t[0].reply.as_ref().unwrap().id, "B");
        assert_eq!(t[0].later.len(), 1);
    }
}

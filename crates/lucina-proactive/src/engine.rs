//! The loop: observe → dedup → score → decide → gate → compose → send →
//! record, plus the feedback pull and the Daily Pulse. [`Engine::tick`]
//! takes the time as an argument, so the replay drives the same code.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::attention::{self, Device, Gate, GateInput, Ledger, Presence, Quiet};
use crate::channels::{Channels, Outgoing};
use crate::compose::{ComposeRequest, Composer};
use crate::config::Config;
use crate::lang;
use crate::model::{
    new_outreach_id, Action, Candidate, Decision, Event, Feedback, FeedbackAction, Kind, Outreach,
    Priority,
};
use crate::pulse::{self, PulseInput};
use crate::score::{self, Verdict};
use crate::secretary::SecretaryApi;
use crate::sources::{DeviceFeed, PresenceFeed, ReminderFeed, Source, TimelineFeed};
use crate::store::Store;
use crate::text;

pub struct Deps {
    pub cfg: Arc<Config>,
    pub store: Arc<dyn Store>,
    pub api: Arc<dyn SecretaryApi>,
    pub sources: Vec<Arc<dyn Source>>,
    pub composer: Arc<dyn Composer>,
    pub channels: Channels,
    pub presence: Arc<dyn PresenceFeed>,
    pub device: Arc<dyn DeviceFeed>,
    pub timeline: Arc<dyn TimelineFeed>,
    pub reminders: Arc<dyn ReminderFeed>,
}

/// One decision, for the log and the replay report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DecisionLog {
    pub at: DateTime<Utc>,
    pub kind: Kind,
    pub source: String,
    pub subject: String,
    pub signal: String,
    pub score: f32,
    pub decision: Decision,
    pub reason: String,
    pub outreach_id: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TickReport {
    pub observed: Vec<String>,
    pub new_events: usize,
    pub decisions: Vec<DecisionLog>,
    pub outreach: Vec<Outreach>,
    /// `sent`, `dry_run`, `suppressed: ...`, `waiting: ...`, or empty.
    pub pulse: String,
    pub errors: Vec<String>,
}

pub struct Engine {
    pub d: Deps,
    next_due: Mutex<HashMap<&'static str, DateTime<Utc>>>,
    settings: Mutex<Option<(DateTime<Utc>, Option<Value>)>>,
}

/// Local midnight of `now`'s day.
pub fn day_start(cfg: &Config, now: DateTime<Utc>) -> DateTime<Utc> {
    attention::local_at(now, cfg.tz(), 0, 0)
}

fn jitter(max: std::time::Duration) -> Duration {
    let ms = max.as_millis() as u64;
    if ms == 0 {
        return Duration::zero();
    }
    Duration::milliseconds((Uuid::new_v4().as_u128() % u128::from(ms)) as i64)
}

/// A note slug for a meeting prep (`prep-20261022-craft-it4sd`).
pub fn prep_slug(title: &str, now: DateTime<Utc>, cfg: &Config) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let date = now.with_timezone(&cfg.tz()).format("%Y%m%d");
    let s = format!("prep-{date}-{}", slug.trim_matches('-'));
    let s: String = s.chars().take(64).collect();
    s.trim_end_matches('-').to_owned()
}

impl Engine {
    pub fn new(d: Deps) -> Self {
        Self {
            d,
            next_due: Mutex::new(HashMap::new()),
            settings: Mutex::new(None),
        }
    }

    fn cfg(&self) -> &Config {
        &self.d.cfg
    }

    /// Quiet hours / DND: the server's settings (cached 5 min), else the fallback.
    pub async fn quiet(&self, now: DateTime<Utc>, live: bool) -> Quiet {
        let cached = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|(at, _)| live && now - *at < Duration::minutes(5) && now >= *at);
        let settings = match cached {
            Some((_, v)) => v,
            None => {
                let v = match self.d.api.notify_settings().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(error = %e, "notification settings unavailable");
                        None
                    }
                };
                *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, v.clone()));
                v
            }
        };
        settings
            .as_ref()
            .and_then(|v| Quiet::from_settings(v, now))
            .unwrap_or_else(|| Quiet::fallback(self.cfg(), now))
    }

    /// The owner's language: his last messages, else the app's, else the config's.
    pub fn language(&self, now: DateTime<Utc>, quiet: &Quiet) -> String {
        let fallback = quiet
            .language
            .clone()
            .unwrap_or_else(|| self.cfg().language.clone());
        let items = self
            .d
            .timeline
            .items(now - Duration::days(3), now)
            .unwrap_or_default();
        let owner: Vec<&str> = items
            .iter()
            .rev()
            .filter(|e| e.role == "user")
            .map(|e| e.text.as_str())
            .collect();
        lang::follow_owner(owner, &fallback)
    }

    async fn ledger(&self, kind: Kind, now: DateTime<Utc>) -> Result<Ledger> {
        let s = &self.d.store;
        let b = &self.cfg().budget;
        Ok(Ledger {
            sent_today: s.unsolicited_since(day_start(self.cfg(), now)).await?,
            last_sent_of_kind: s.last_outreach_of_kind(kind).await?,
            not_now_until: s
                .last_feedback(kind, FeedbackAction::NotNow)
                .await?
                .map(|t| t + Duration::hours(i64::from(b.not_now_hours))),
            muted_until: s
                .last_feedback(kind, FeedbackAction::Stop)
                .await?
                .map(|t| t + Duration::days(i64::from(b.stop_days))),
        })
    }

    /// Pull the feedback taps from secretary-server (cursor in source_state).
    pub async fn pull_feedback(&self, now: DateTime<Utc>) -> Result<usize> {
        let store = &self.d.store;
        let cursor = store
            .get_state("feedback", "cursor")
            .await?
            .and_then(|v| v.get("after").and_then(Value::as_str).map(str::to_owned));
        let Some(v) = self.d.api.feedback_since(cursor.as_deref()).await? else {
            return Ok(0);
        };
        let mut n = 0;
        for item in v
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            let Some(action) = FeedbackAction::parse(&s("action")) else {
                continue;
            };
            let Some(o) = store.outreach(&s("outreachId")).await? else {
                continue;
            };
            let at = item
                .get("at")
                .and_then(Value::as_i64)
                .and_then(DateTime::from_timestamp_millis)
                .unwrap_or(now);
            let f = Feedback {
                id: Uuid::new_v4(),
                outreach_id: o.id.clone(),
                action,
                kind: o.kind,
                source: item
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("app")
                    .to_owned(),
                external_id: Some(s("id")).filter(|x| !x.is_empty()),
                created_at: at,
            };
            if store.record_feedback(&f).await? {
                n += 1;
                tracing::info!(outreach = %o.id, kind = %o.kind, action = action.as_str(), "feedback recorded");
            }
        }
        if let Some(c) = v.get("cursor").and_then(Value::as_str) {
            store
                .put_state("feedback", "cursor", &json!({"after": c}), now)
                .await?;
        }
        Ok(n)
    }

    fn log(
        &self,
        report: &mut TickReport,
        now: DateTime<Utc>,
        c: &Candidate,
        e: &Event,
        outreach: Option<&str>,
    ) {
        tracing::info!(
            target: "lucina_proactive::decision",
            kind = %c.kind,
            source = %e.source,
            decision = c.decision.as_str(),
            score = format!("{:.2}", c.score),
            reason = %c.reason,
            outreach = outreach.unwrap_or(""),
            "decision"
        );
        report.decisions.push(DecisionLog {
            at: now,
            kind: c.kind,
            source: e.source.clone(),
            subject: e.subject.clone(),
            signal: e.signal.clone(),
            score: c.score,
            decision: c.decision,
            reason: c.reason.clone(),
            outreach_id: outreach.map(str::to_owned),
        });
    }

    /// One pass of the loop at `now`.
    pub async fn tick(&self, now: DateTime<Utc>, live: bool) -> Result<TickReport> {
        let mut report = TickReport::default();
        let cfg = self.d.cfg.clone();
        let store = self.d.store.clone();

        if let Err(e) = self.pull_feedback(now).await {
            report.errors.push(format!("feedback: {e}"));
        }
        let quiet = self.quiet(now, live).await;
        let presence = self.d.presence.at(now);
        let device = self.d.device.at(now);
        let language = self.language(now, &quiet);

        // 1. observe the sources that are due
        let mut fresh: Vec<Event> = Vec::new();
        for src in &self.d.sources {
            let due = self
                .next_due
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(src.name())
                .copied();
            if due.is_some_and(|d| d > now) {
                continue;
            }
            let (every, jit) = src.schedule();
            let next =
                now + Duration::from_std(every).unwrap_or(Duration::minutes(5)) + jitter(jit);
            self.next_due
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(src.name(), next);
            report.observed.push(src.name().to_owned());
            match src.observe(now, store.as_ref()).await {
                Ok(events) => fresh.extend(events),
                Err(e) => {
                    tracing::warn!(source = src.name(), error = %e, "source failed");
                    report.errors.push(format!("{}: {e}", src.name()));
                }
            }
        }

        // 2. dedup and score
        let mut queue: Vec<(Candidate, Event)> = Vec::new();
        for e in fresh {
            if !store.insert_event(&e).await? {
                continue;
            }
            report.new_events += 1;
            let s = score::evaluate(&cfg.weights, &cfg.thresholds, &e.scores);
            let mut c = Candidate {
                id: Uuid::new_v4(),
                event_id: e.id,
                kind: e.kind,
                scores: e.scores,
                score: s.score,
                decision: Decision::Drop,
                reason: s.reason.clone(),
                priority: s.priority,
                created_at: now,
                decided_at: now,
                hold_until: None,
                expires_at: e.expires_at,
                pulse_id: None,
            };
            match s.verdict {
                Verdict::Drop | Verdict::Digest => {
                    c.decision = if s.verdict == Verdict::Drop {
                        Decision::Drop
                    } else {
                        Decision::Digest
                    };
                    store.save_candidate(&c).await?;
                    self.log(&mut report, now, &c, &e, None);
                }
                Verdict::Send => queue.push((c, e)),
            }
        }
        // Held ones whose wait is over (presence holds re-check every few minutes).
        for (c, e) in store.held().await? {
            if c.hold_until.is_none_or(|u| u <= now) {
                queue.push((c, e));
            }
        }
        queue.sort_by(|a, b| b.0.score.total_cmp(&a.0.score));

        // 3. gate, compose, send
        for (mut c, e) in queue {
            let ledger = self.ledger(c.kind, now).await?;
            let g = attention::gate(
                &cfg,
                now,
                &GateInput {
                    kind: c.kind,
                    expires_at: c.expires_at,
                    quiet: &quiet,
                    presence: &presence,
                    device: &device,
                    ledger: &ledger,
                },
            );
            let base = score::evaluate(&cfg.weights, &cfg.thresholds, &c.scores).reason;
            c.decided_at = now;
            match g {
                Gate::Send => {
                    let o = self
                        .send(
                            now,
                            &c,
                            &e,
                            &language,
                            &quiet,
                            &presence,
                            &format!("{base}; sent"),
                            live,
                        )
                        .await?;
                    c.decision = Decision::Sent;
                    c.hold_until = None;
                    c.reason = format!("{base}; sent ({})", o.status);
                    store.save_candidate(&c).await?;
                    self.log(&mut report, now, &c, &e, Some(&o.id));
                    report.outreach.push(o);
                }
                Gate::Hold { until, reason } => {
                    let first = c.decision != Decision::Hold
                        || c.reason != format!("{base}; held: {reason}");
                    c.decision = Decision::Hold;
                    c.hold_until = Some(until);
                    c.reason = format!("{base}; held: {reason}");
                    store.save_candidate(&c).await?;
                    if first {
                        self.log(&mut report, now, &c, &e, None);
                    }
                }
                Gate::Digest { reason } => {
                    c.decision = Decision::Digest;
                    c.hold_until = None;
                    c.reason = format!("{base}; to the Pulse: {reason}");
                    store.save_candidate(&c).await?;
                    self.log(&mut report, now, &c, &e, None);
                }
                Gate::Drop { reason } => {
                    c.decision = Decision::Drop;
                    c.hold_until = None;
                    c.reason = format!("{base}; dropped: {reason}");
                    store.save_candidate(&c).await?;
                    self.log(&mut report, now, &c, &e, None);
                }
            }
        }

        // 4. the Daily Pulse
        match self
            .pulse(now, &quiet, &presence, &device, &language, live)
            .await
        {
            Ok((p, o)) => {
                report.pulse = p;
                report.outreach.extend(o);
            }
            Err(e) => report.errors.push(format!("pulse: {e}")),
        }
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        now: DateTime<Utc>,
        c: &Candidate,
        e: &Event,
        language: &str,
        quiet: &Quiet,
        presence: &Presence,
        reason: &str,
        live: bool,
    ) -> Result<Outreach> {
        let cfg = self.cfg();
        let dry = cfg.dry_run;
        let id = new_outreach_id(now);
        let p = &e.payload;
        let has_material = ["notes", "memory", "mentions"].iter().any(|k| {
            p.get(*k)
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
        });
        let want_prep = e.kind == Kind::MeetingPrep && has_material && cfg.channels.save_prep_notes;
        let prep_title = p
            .pointer("/event/title")
            .and_then(Value::as_str)
            .unwrap_or(&e.subject)
            .to_owned();
        let slug = prep_slug(&prep_title, now, cfg);
        let mut facts = p.clone();
        if let Some(o) = facts.as_object_mut() {
            o.insert("subject".into(), json!(e.subject));
            if want_prep {
                o.insert("prepNote".into(), json!(slug));
            }
        }
        let composed = self
            .d
            .composer
            .compose(&ComposeRequest {
                kind: e.kind,
                language: language.to_owned(),
                priority: c.priority,
                facts,
                local_time: now
                    .with_timezone(&cfg.tz())
                    .format("%a %d/%m %H:%M")
                    .to_string(),
                want_prep,
            })
            .await;

        let mut result = json!({"composer": composed.via, "gate": {"quiet": quiet.source, "presence": presence.status}});
        let mut actions: Vec<Action> = Vec::new();
        if let Some(prep) = composed.prep.as_deref().filter(|_| want_prep) {
            let title = prep_title.as_str();
            if dry {
                result["prepNote"] = json!({"wouldCreate": slug, "content": prep});
                actions.push(lang::open_note_action(language, &slug));
            } else {
                match self
                    .d
                    .api
                    .create_note(
                        &format!("Prep: {}", text::clip(title, 150)),
                        prep,
                        &slug,
                        &["lucina-prep", "briefing"],
                    )
                    .await
                {
                    Ok(s) => {
                        result["prepNote"] = json!({"created": s});
                        actions.push(lang::open_note_action(language, &s));
                    }
                    Err(err) => result["prepNote"] = json!({"error": err.to_string()}),
                }
            }
        }
        if actions.is_empty() {
            let note = p
                .pointer("/notes/0/slug")
                .or_else(|| p.pointer("/note/slug"))
                .and_then(Value::as_str);
            if let Some(slug) = note {
                actions.push(lang::open_note_action(language, slug));
            }
        }
        if e.kind == Kind::DueNoPlan {
            actions.push(lang::open_calendar_action(language));
        }
        actions.truncate(2);
        actions.extend(lang::feedback_actions(language));

        let why = format!("{} · {}", e.signal, reason);
        let out = Outgoing {
            outreach_id: id.clone(),
            kind: e.kind,
            priority: c.priority,
            text: composed.message.clone(),
            actions: actions.clone(),
            why: why.clone(),
            language: language.to_owned(),
            now,
        };
        let delivery = self.d.channels.deliver(&out, dry).await;
        if let (Some(r), Some(o)) = (delivery.result.as_object(), result.as_object_mut()) {
            for (k, v) in r {
                o.insert(k.clone(), v.clone());
            }
        }
        let o = Outreach {
            id,
            candidate_id: Some(c.id),
            kind: e.kind,
            channel: delivery.channel,
            message: composed.message,
            priority: c.priority,
            actions,
            why,
            language: language.to_owned(),
            dry_run: dry,
            status: delivery.status,
            result,
            sent_at: now,
            timeline_id: delivery.timeline_id,
            unsolicited: true,
        };
        self.d.store.record_outreach(&o).await?;
        if dry && live {
            tracing::info!(target: "lucina_proactive::dry_run", outreach = %o.id, kind = %o.kind, channel = %o.channel, message = %o.message, "WOULD SEND");
        }
        Ok(o)
    }

    async fn pulse(
        &self,
        now: DateTime<Utc>,
        quiet: &Quiet,
        presence: &Presence,
        device: &Device,
        language: &str,
        live: bool,
    ) -> Result<(String, Option<Outreach>)> {
        let cfg = self.cfg();
        if !cfg.pulse.enabled {
            return Ok((String::new(), None));
        }
        let tz = cfg.tz();
        let (Some(at), Some(deadline)) = (
            attention::parse_hhmm(&cfg.pulse.time),
            attention::parse_hhmm(&cfg.pulse.deadline),
        ) else {
            return Ok((String::new(), None));
        };
        let m = attention::local_minutes(now, tz);
        if m < at || m >= deadline {
            return Ok((String::new(), None));
        }
        let today = now.with_timezone(&tz).date_naive().to_string();
        let store = &self.d.store;
        let last = store
            .get_state("pulse", "last")
            .await?
            .unwrap_or(Value::Null);
        if last.get("date").and_then(Value::as_str) == Some(today.as_str()) {
            return Ok((String::new(), None));
        }
        // "Basta così" on a Pulse mutes the Pulse like any other kind.
        if let Some(t) = store
            .last_feedback(Kind::Pulse, FeedbackAction::Stop)
            .await?
        {
            let until = t + Duration::days(i64::from(cfg.budget.stop_days));
            if until > now {
                return Ok((
                    format!(
                        "suppressed: muted (\"Basta così\") until {}",
                        until.with_timezone(&tz).format("%Y-%m-%d")
                    ),
                    None,
                ));
            }
        }
        if quiet.quiet {
            return Ok(("waiting: quiet hours".into(), None));
        }
        if cfg.budget.hold_while_gaming && presence.gaming.is_some() {
            return Ok(("waiting: gaming".into(), None));
        }
        if device
            .focus
            .as_deref()
            .is_some_and(|f| attention::HOLD_FOCUS.contains(&f.to_ascii_lowercase().as_str()))
        {
            return Ok(("waiting: focus mode".into(), None));
        }
        let since = last
            .get("at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .filter(|t| *t > now - Duration::days(3))
            .unwrap_or(now - Duration::hours(24));
        let digests = store.digests(since).await?;
        let end_of_day = attention::local_at(now, tz, 1, 0);
        let today_events = self
            .d
            .api
            .calendar_events(now, end_of_day)
            .await
            .unwrap_or_default();
        let reminders = self.d.reminders.reminders(now).unwrap_or_default();
        let draft = pulse::build(
            cfg,
            now,
            &PulseInput {
                digests: &digests,
                today_events: &today_events,
                reminders: &reminders,
            },
        );
        let previous: Vec<String> = last
            .get("keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|k| k.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let new_items = draft.new_items(&previous);
        if draft.is_empty() || new_items == 0 {
            let why = if draft.is_empty() {
                "nothing to say"
            } else {
                "nothing new since the last Pulse"
            };
            store
                .put_state(
                    "pulse",
                    "last",
                    &json!({"date": today, "at": last.get("at").cloned().unwrap_or(json!(now)), "keys": previous, "suppressed": why}),
                    now,
                )
                .await?;
            tracing::info!(target: "lucina_proactive::decision", kind = "pulse", decision = "silent", reason = why, "decision");
            return Ok((format!("suppressed: {why}"), None));
        }
        let composed = self
            .d
            .composer
            .compose(&ComposeRequest {
                kind: Kind::Pulse,
                language: language.to_owned(),
                priority: Priority::Low,
                facts: draft.facts(),
                local_time: now.with_timezone(&tz).format("%a %d/%m %H:%M").to_string(),
                want_prep: false,
            })
            .await;
        let id = new_outreach_id(now);
        let why = format!(
            "Daily Pulse at {}: {} important, {} done, {} to watch ({} new)",
            cfg.pulse.time,
            draft.important.len(),
            draft.completed.len(),
            draft.watch.len(),
            new_items
        );
        let actions = lang::feedback_actions(language);
        let out = Outgoing {
            outreach_id: id.clone(),
            kind: Kind::Pulse,
            priority: Priority::Low,
            text: composed.message.clone(),
            actions: actions.clone(),
            why: why.clone(),
            language: language.to_owned(),
            now,
        };
        let delivery = self.d.channels.deliver(&out, cfg.dry_run).await;
        let mut result = delivery.result;
        result["composer"] = json!(composed.via);
        result["items"] = draft.facts();
        let o = Outreach {
            id: id.clone(),
            candidate_id: None,
            kind: Kind::Pulse,
            channel: delivery.channel,
            message: composed.message,
            priority: Priority::Low,
            actions,
            why,
            language: language.to_owned(),
            dry_run: cfg.dry_run,
            status: delivery.status.clone(),
            result,
            sent_at: now,
            timeline_id: delivery.timeline_id,
            unsolicited: false,
        };
        store.record_outreach(&o).await?;
        for (mut c, _) in digests {
            if draft.candidates().contains(&c.id) {
                c.decision = Decision::Pulsed;
                c.pulse_id = Some(id.clone());
                c.reason = format!("{}; in the Pulse {id}", c.reason);
                store.save_candidate(&c).await?;
            }
        }
        store
            .put_state(
                "pulse",
                "last",
                &json!({"date": today, "at": now, "keys": draft.keys(), "outreachId": id}),
                now,
            )
            .await?;
        if cfg.dry_run && live {
            tracing::info!(target: "lucina_proactive::dry_run", outreach = %o.id, kind = "pulse", message = %o.message, "WOULD SEND");
        }
        Ok((delivery.status, Some(o)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prep_slugs() {
        let cfg = Config::default();
        let now = DateTime::parse_from_rfc3339("2026-10-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            prep_slug("CRAFT-IT4SD — meeting with Marianne & Jonas", now, &cfg),
            "prep-20261022-craft-it4sd-meeting-with-marianne-jonas"
        );
        assert!(prep_slug(&"x".repeat(200), now, &cfg).len() <= 64);
        assert_eq!(prep_slug("Æble!", now, &cfg), "prep-20261022-ble");
    }
}

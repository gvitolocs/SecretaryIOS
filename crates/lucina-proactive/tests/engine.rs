//! The engine end to end with mocked sources and channels: dedup, the
//! attention gate (quiet hours, gaming, budget, cooldown), feedback, the
//! Daily Pulse, health hysteresis, threads, delivery and dry-run.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use lucina_proactive::attention::{Device, Presence};
use lucina_proactive::channels::{Channels, Telegram, TimelineSink};
use lucina_proactive::compose::TemplateComposer;
use lucina_proactive::config::{Config, TelegramMode};
use lucina_proactive::engine::{Deps, Engine};
use lucina_proactive::model::{Decision, Kind};
use lucina_proactive::secretary::{ApiError, CalEvent, NoteSummary, SecretaryApi};
use lucina_proactive::sources::calendar::CalendarSource;
use lucina_proactive::sources::health::HealthSource;
use lucina_proactive::sources::notes::NotesSource;
use lucina_proactive::sources::reminders::ReminderSource;
use lucina_proactive::sources::threads::{Judge, Judgement, ThreadSource};
use lucina_proactive::sources::{
    DeviceFeed, Entry, HealthProbe, PresenceFeed, Probe, ReminderFeed, Source, TimelineFeed,
};
use lucina_proactive::store::{MemStore, Store};
use lucina_store::Reminder;
use serde_json::{json, Value};

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

// ------------------------------------------------------------------ mocks

#[derive(Default)]
struct Api {
    events: Mutex<Vec<CalEvent>>,
    notes: Mutex<Vec<NoteSummary>>,
    settings: Mutex<Option<Value>>,
    feedback: Mutex<Vec<Value>>,
    notified: Mutex<Vec<Value>>,
    created: Mutex<Vec<(String, String)>>,
    pushed: Mutex<u64>,
}

#[async_trait::async_trait]
impl SecretaryApi for Api {
    async fn calendar_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>, ApiError> {
        let (f, t) = (from.timestamp_millis(), to.timestamp_millis());
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.start_ms < t && e.end_ms > f)
            .cloned()
            .collect())
    }
    async fn notes(&self, query: Option<&str>) -> Result<Vec<NoteSummary>, ApiError> {
        let all = self.notes.lock().unwrap().clone();
        Ok(match query {
            None => all,
            Some(q) => all
                .into_iter()
                .filter(|n| {
                    format!("{} {} {}", n.title, n.slug, n.excerpt)
                        .to_lowercase()
                        .contains(&q.to_lowercase())
                })
                .collect(),
        })
    }
    async fn note(&self, _: &str) -> Result<Option<Value>, ApiError> {
        Ok(None)
    }
    async fn create_note(
        &self,
        title: &str,
        _: &str,
        slug: &str,
        _: &[&str],
    ) -> Result<String, ApiError> {
        self.created
            .lock()
            .unwrap()
            .push((title.into(), slug.into()));
        Ok(slug.into())
    }
    async fn notify(&self, body: &Value) -> Result<Value, ApiError> {
        self.notified.lock().unwrap().push(body.clone());
        Ok(json!({"status": "created", "pushed": *self.pushed.lock().unwrap(), "deferred": false}))
    }
    async fn notify_settings(&self) -> Result<Option<Value>, ApiError> {
        Ok(self.settings.lock().unwrap().clone())
    }
    async fn feedback_since(&self, after: Option<&str>) -> Result<Option<Value>, ApiError> {
        let items: Vec<Value> = self
            .feedback
            .lock()
            .unwrap()
            .iter()
            .filter(|f| after.is_none_or(|a| f["id"].as_str().unwrap() > a))
            .cloned()
            .collect();
        let cursor = items.last().map(|f| f["id"].clone());
        Ok(Some(json!({"items": items, "cursor": cursor})))
    }
}

#[derive(Default)]
struct Feeds {
    reminders: Mutex<Vec<Reminder>>,
    timeline: Mutex<Vec<Entry>>,
    presence: Mutex<Presence>,
}

impl ReminderFeed for Feeds {
    fn reminders(&self, _: DateTime<Utc>) -> Result<Vec<Reminder>> {
        Ok(self.reminders.lock().unwrap().clone())
    }
}
impl TimelineFeed for Feeds {
    fn items(&self, since: DateTime<Utc>, now: DateTime<Utc>) -> Result<Vec<Entry>> {
        Ok(self
            .timeline
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.ts >= since && e.ts <= now)
            .cloned()
            .collect())
    }
}
impl PresenceFeed for Feeds {
    fn at(&self, _: DateTime<Utc>) -> Presence {
        self.presence.lock().unwrap().clone()
    }
}
impl DeviceFeed for Feeds {
    fn at(&self, _: DateTime<Utc>) -> Device {
        Device::default()
    }
}

#[derive(Default)]
struct Sink(Mutex<Vec<String>>);
#[async_trait::async_trait]
impl TimelineSink for Sink {
    async fn append(&self, line: &str) -> Result<()> {
        self.0.lock().unwrap().push(line.to_owned());
        Ok(())
    }
    fn describe(&self) -> String {
        "memory".into()
    }
}

#[derive(Default)]
struct Tg(Mutex<Vec<(String, String, String)>>);
#[async_trait::async_trait]
impl Telegram for Tg {
    async fn send(&self, chat: &str, text: &str, button: Option<(&str, &str)>) -> Result<i64> {
        self.0.lock().unwrap().push((
            chat.into(),
            text.into(),
            button.map(|b| b.1.to_owned()).unwrap_or_default(),
        ));
        Ok(7)
    }
}

#[derive(Default)]
struct Probes(Mutex<HashMap<String, VecDeque<bool>>>);
#[async_trait::async_trait]
impl HealthProbe for Probes {
    async fn probe(&self, url: &str, _: StdDuration, _: u64) -> Probe {
        let ok = self
            .0
            .lock()
            .unwrap()
            .get_mut(url)
            .and_then(VecDeque::pop_front)
            .unwrap_or(true);
        Probe {
            ok,
            status: if ok { 200 } else { 503 },
            ms: 50,
            error: (!ok).then(|| "HTTP 503".into()),
        }
    }
}

struct ScriptJudge(Judgement);
#[async_trait::async_trait]
impl Judge for ScriptJudge {
    async fn judge(&self, _: &Value) -> Option<Judgement> {
        Some(self.0.clone())
    }
}

struct World {
    engine: Engine,
    api: Arc<Api>,
    feeds: Arc<Feeds>,
    sink: Arc<Sink>,
    tg: Arc<Tg>,
    store: Arc<MemStore>,
    probes: Arc<Probes>,
}

fn config(dry_run: bool) -> Config {
    let mut c = Config {
        dry_run,
        ..Config::default()
    };
    c.channels.telegram = TelegramMode::Fallback;
    c.health.targets.truncate(1); // Secretary only
                                  // Every source every tick (the tests drive time in 1-minute steps).
    for s in [
        &mut c.sources.calendar,
        &mut c.sources.reminders,
        &mut c.sources.notes,
        &mut c.sources.timeline,
    ] {
        s.interval_secs = 60;
        s.jitter_secs = 0;
    }
    c.health.interval_secs = 60;
    c.health.jitter_secs = 0;
    // The Pulse has its own tests.
    c.pulse.enabled = false;
    c
}

fn with_pulse() -> Config {
    let mut c = config(false);
    c.pulse.enabled = true;
    c
}

fn world(cfg: Config) -> World {
    let cfg = Arc::new(cfg);
    let api = Arc::new(Api::default());
    *api.pushed.lock().unwrap() = 1;
    let feeds = Arc::new(Feeds::default());
    let sink = Arc::new(Sink::default());
    let tg = Arc::new(Tg::default());
    let store = Arc::new(MemStore::new());
    let probes = Arc::new(Probes::default());
    let judge: Arc<dyn Judge> = Arc::new(ScriptJudge(Judgement {
        status: "resolved".into(),
        confidence: 0.85,
        summary: "Il checkout di Pokoin ora funziona.".into(),
    }));
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(CalendarSource {
            cfg: cfg.clone(),
            api: api.clone(),
            memory: None,
            timeline: feeds.clone(),
        }),
        Arc::new(ReminderSource {
            cfg: cfg.clone(),
            feed: feeds.clone(),
            api: api.clone(),
        }),
        Arc::new(NotesSource {
            cfg: cfg.clone(),
            api: api.clone(),
        }),
        Arc::new(ThreadSource {
            cfg: cfg.clone(),
            timeline: feeds.clone(),
            judge: Some(judge),
        }),
        Arc::new(HealthSource {
            cfg: cfg.clone(),
            probe: probes.clone(),
            env_urls: Default::default(),
        }),
    ];
    let channels = Channels {
        cfg: cfg.channels.clone(),
        api: api.clone(),
        sink: sink.clone(),
        telegram: Some(tg.clone()),
        chat_id: Some("42".into()),
    };
    let engine = Engine::new(Deps {
        cfg,
        store: store.clone(),
        api: api.clone(),
        sources,
        composer: Arc::new(TemplateComposer),
        channels,
        presence: feeds.clone(),
        device: feeds.clone(),
        timeline: feeds.clone(),
        reminders: feeds.clone(),
    });
    World {
        engine,
        api,
        feeds,
        sink,
        tg,
        store,
        probes,
    }
}

fn event(id: &str, title: &str, start: DateTime<Utc>) -> CalEvent {
    serde_json::from_value(json!({
        "id": id, "source": "google", "title": title,
        "start": start.to_rfc3339(), "end": (start + Duration::hours(1)).to_rfc3339(),
        "allDay": false, "startMs": start.timestamp_millis(), "endMs": (start + Duration::hours(1)).timestamp_millis(),
    }))
    .unwrap()
}

fn note(slug: &str, title: &str, pinned: bool) -> NoteSummary {
    serde_json::from_value(json!({"id": format!("n_{slug}"), "slug": slug, "title": title, "pinned": pinned, "tags": ["work"], "excerpt": "Marianne offered paid hours"})).unwrap()
}

fn reminder(id: &str, due: DateTime<Utc>, text: &str, status: &str) -> Reminder {
    serde_json::from_value(json!({"id": id, "text": text, "dueAt": due.to_rfc3339(), "status": status, "createdAt": "2026-10-01T00:00:00Z"})).unwrap()
}

fn quiet_settings(start: &str, end: &str) -> Value {
    json!({"settings": {"timeZone": "Europe/Copenhagen", "quietHours": {"enabled": true, "start": start, "end": end}, "dndUntil": null, "language": "it", "categories": {"lucina_message": true}}})
}

// 10:00 local (CEST) on 2026-10-22.
const NOON_ISH: &str = "2026-10-22T08:00:00Z";

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn meeting_prep_is_sent_once_with_prep_note_actions_and_push() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.api.events.lock().unwrap().push(event(
        "google:abc",
        "CRAFT-IT4SD meeting with Marianne",
        now + Duration::minutes(60),
    ));
    w.api.notes.lock().unwrap().push(note(
        "craftit-meeting",
        "CRAFT-IT4SD — meeting with Marianne & Jonas",
        true,
    ));

    let r = w.engine.tick(now, false).await.unwrap();
    assert_eq!(r.outreach.len(), 1, "{:#?}", r.decisions);
    let o = &r.outreach[0];
    assert_eq!(
        (o.kind, o.status.as_str(), o.channel.as_str()),
        (Kind::MeetingPrep, "sent", "app")
    );
    // The prep was saved as a note (a draft, nothing sent anywhere).
    let created = w.api.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1);
    assert!(created[0].1.starts_with("prep-20261022-craft-it4sd"));
    // Timeline item: proactive meta, open-note + 3 feedback buttons.
    let lines = w.sink.0.lock().unwrap().clone();
    assert_eq!(lines.len(), 1);
    let item: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(item["role"], "lucina");
    assert_eq!(item["meta"]["kind"], "proactive");
    assert_eq!(item["meta"]["topic"], "meeting_prep");
    assert_eq!(item["meta"]["outreachId"], o.id.as_str());
    let ids: Vec<&str> = item["meta"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["open_note", "useful", "not_now", "stop"]);
    assert!(item["meta"]["why"]
        .as_str()
        .unwrap()
        .contains("starts in 60 min"));
    // Push: lucina_message, deep link route, the timeline id for dedup.
    let n = w.api.notified.lock().unwrap().clone();
    assert_eq!(n.len(), 1);
    assert_eq!(n[0]["category"], "lucina_message");
    assert_eq!(n[0]["route"], format!("/?outreach={}", o.id));
    assert_eq!(n[0]["timelineId"], item["id"]);
    // The push reached a device: no Telegram.
    assert!(w.tg.0.lock().unwrap().is_empty());

    // Next ticks: the same meeting is not considered again.
    for m in 1..5 {
        let r = w
            .engine
            .tick(now + Duration::minutes(m), false)
            .await
            .unwrap();
        assert!(r.outreach.is_empty());
    }
    assert_eq!(w.sink.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn meeting_without_material_waits_for_the_pulse() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.api
        .events
        .lock()
        .unwrap()
        .push(event("google:x", "Dentista", now + Duration::minutes(45)));
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(r.outreach.is_empty());
    let d = &r.decisions[0];
    assert_eq!((d.kind, d.decision), (Kind::MeetingPrep, Decision::Digest));
    assert!(d.reason.contains("for the Daily Pulse"), "{}", d.reason);
}

#[tokio::test]
async fn telegram_only_when_the_push_reaches_no_device() {
    let w = world(config(false));
    *w.api.pushed.lock().unwrap() = 0;
    let now = at(NOON_ISH);
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        now - Duration::minutes(40),
        "Paga la bolletta",
        "pending",
    ));
    let r = w.engine.tick(now, false).await.unwrap();
    assert_eq!(r.outreach.len(), 1);
    assert_eq!(r.outreach[0].channel, "app+telegram");
    let sent = w.tg.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "42");
    assert_eq!(
        sent[0].2,
        format!("https://getsecretary.tech/?outreach={}", r.outreach[0].id)
    );
}

#[tokio::test]
async fn dry_run_records_and_sends_nothing() {
    let w = world(config(true));
    let now = at(NOON_ISH);
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        now - Duration::minutes(40),
        "Paga la bolletta",
        "pending",
    ));
    let r = w.engine.tick(now, false).await.unwrap();
    assert_eq!(r.outreach.len(), 1);
    let o = &r.outreach[0];
    assert!(o.dry_run && o.status == "dry_run");
    assert_eq!(o.result["wouldAppend"]["meta"]["kind"], "proactive");
    assert!(w.sink.0.lock().unwrap().is_empty());
    assert!(w.api.notified.lock().unwrap().is_empty());
    assert!(w.tg.0.lock().unwrap().is_empty());
    // It still counts for the budget and the cooldown (a faithful dry run).
    assert_eq!(
        w.store
            .unsolicited_since(now - Duration::hours(1))
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn quiet_hours_hold_then_send() {
    let w = world(config(false));
    // 23:00 local; quiet 22:30–07:30.
    let now = at("2026-10-22T21:00:00Z");
    *w.api.settings.lock().unwrap() = Some(quiet_settings("22:30", "07:30"));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        now - Duration::minutes(30),
        "Prendi la medicina",
        "pending",
    ));
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(r.outreach.is_empty());
    assert_eq!(r.decisions[0].decision, Decision::Hold);
    assert!(
        r.decisions[0].reason.contains("quiet hours"),
        "{}",
        r.decisions[0].reason
    );
    // Still quiet at 02:00.
    assert!(w
        .engine
        .tick(at("2026-10-23T00:00:00Z"), false)
        .await
        .unwrap()
        .outreach
        .is_empty());
    // 07:31 local: sent (it expires 12 h after it was due, so it is still useful).
    let r = w
        .engine
        .tick(at("2026-10-23T05:31:00Z"), false)
        .await
        .unwrap();
    assert_eq!(r.outreach.len(), 1);
}

#[tokio::test]
async fn quiet_hours_until_it_is_stale_goes_to_the_pulse() {
    let w = world(config(false));
    let now = at("2026-10-22T21:00:00Z");
    *w.api.settings.lock().unwrap() = Some(quiet_settings("22:30", "07:30"));
    w.api
        .notes
        .lock()
        .unwrap()
        .push(note("dinner", "Dinner with Marianne", false));
    // A meeting at 23:50 local (expires 5 min before): quiet hours outlast it.
    w.api.events.lock().unwrap().push(event(
        "google:late",
        "Dinner with Marianne",
        now + Duration::minutes(50),
    ));
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(r.outreach.is_empty());
    let d = r
        .decisions
        .iter()
        .find(|d| d.kind == Kind::MeetingPrep)
        .unwrap();
    assert_eq!(d.decision, Decision::Digest);
    assert!(d.reason.contains("stale"), "{}", d.reason);
}

#[tokio::test]
async fn gaming_holds_until_he_stops() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    *w.feeds.presence.lock().unwrap() = Presence {
        status: "online".into(),
        gaming: Some("League of Legends".into()),
        online_since: None,
    };
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        now - Duration::minutes(30),
        "Chiama Jonas",
        "pending",
    ));
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(r.outreach.is_empty());
    assert!(r.decisions[0].reason.contains("League of Legends"));
    *w.feeds.presence.lock().unwrap() = Presence {
        status: "online".into(),
        ..Presence::default()
    };
    // The hold re-checks after 5 minutes.
    assert!(w
        .engine
        .tick(now + Duration::minutes(2), false)
        .await
        .unwrap()
        .outreach
        .is_empty());
    assert_eq!(
        w.engine
            .tick(now + Duration::minutes(6), false)
            .await
            .unwrap()
            .outreach
            .len(),
        1
    );
}

#[tokio::test]
async fn daily_budget_and_cooldown() {
    let mut cfg = config(false);
    cfg.budget
        .cooldown_minutes
        .insert("reminder_overdue".into(), 0);
    let w = world(cfg);
    let now = at(NOON_ISH);
    for i in 0..4 {
        w.feeds.reminders.lock().unwrap().push(reminder(
            &format!("r{i}"),
            now - Duration::minutes(30 + i),
            &format!("Cosa {i}"),
            "pending",
        ));
    }
    let r = w.engine.tick(now, false).await.unwrap();
    assert_eq!(r.outreach.len(), 3);
    let last = r
        .decisions
        .iter()
        .find(|d| d.decision == Decision::Digest)
        .unwrap();
    assert!(
        last.reason.contains("daily budget used: 3 of 3"),
        "{}",
        last.reason
    );

    // With the default 60-min cooldown, a second one of the same kind waits for the Pulse.
    let w = world(config(false));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "a",
        now - Duration::minutes(30),
        "Uno",
        "pending",
    ));
    assert_eq!(w.engine.tick(now, false).await.unwrap().outreach.len(), 1);
    w.feeds.reminders.lock().unwrap().push(reminder(
        "b",
        now - Duration::minutes(20),
        "Due",
        "pending",
    ));
    let r = w
        .engine
        .tick(now + Duration::minutes(10), false)
        .await
        .unwrap();
    assert!(r.outreach.is_empty());
    assert!(
        r.decisions[0].reason.contains("cooldown"),
        "{}",
        r.decisions[0].reason
    );
}

#[tokio::test]
async fn feedback_stop_mutes_and_not_now_holds() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.feeds.reminders.lock().unwrap().push(reminder(
        "a",
        now - Duration::minutes(30),
        "Uno",
        "pending",
    ));
    let first = w.engine.tick(now, false).await.unwrap().outreach.remove(0);
    // He taps "Basta così".
    w.api.feedback.lock().unwrap().push(json!({"id": "fb_0001", "outreachId": first.id, "action": "stop", "at": (now + Duration::minutes(5)).timestamp_millis()}));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "b",
        now + Duration::minutes(50),
        "Due",
        "pending",
    ));
    let r = w
        .engine
        .tick(now + Duration::hours(2), false)
        .await
        .unwrap();
    assert!(r.outreach.is_empty());
    let d = r
        .decisions
        .iter()
        .find(|d| d.kind == Kind::ReminderOverdue)
        .unwrap();
    assert_eq!(d.decision, Decision::Drop);
    assert!(d.reason.contains("Basta così"), "{}", d.reason);
    // The same tap pulled again is not recorded twice.
    assert_eq!(
        w.engine
            .pull_feedback(now + Duration::hours(3))
            .await
            .unwrap(),
        0
    );

    // "Non ora" holds the kind for 6 hours.
    let w = world(config(false));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "a",
        now - Duration::minutes(30),
        "Uno",
        "pending",
    ));
    let first = w.engine.tick(now, false).await.unwrap().outreach.remove(0);
    w.api.feedback.lock().unwrap().push(json!({"id": "fb_0002", "outreachId": first.id, "action": "not_now", "at": (now + Duration::minutes(5)).timestamp_millis()}));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "b",
        now + Duration::minutes(60),
        "Due",
        "pending",
    ));
    let r = w
        .engine
        .tick(now + Duration::hours(2), false)
        .await
        .unwrap();
    assert!(r.outreach.is_empty());
    assert!(r
        .decisions
        .iter()
        .any(|d| d.decision == Decision::Hold && d.reason.contains("Non ora")));
    assert_eq!(
        w.engine
            .tick(now + Duration::hours(6) + Duration::minutes(6), false)
            .await
            .unwrap()
            .outreach
            .len(),
        1
    );
}

#[tokio::test]
async fn daily_pulse_sent_then_silent_when_nothing_is_new() {
    let w = world(with_pulse());
    // 07:59 local: not yet.
    let morning = at("2026-10-22T05:59:00Z");
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        at("2026-10-22T16:00:00Z"),
        "Chiama il commercialista",
        "pending",
    ));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r2",
        at("2026-10-24T08:00:00Z"),
        "Scadenza IVA: invia la dichiarazione",
        "pending",
    ));
    let r = w.engine.tick(morning, false).await.unwrap();
    assert_eq!(r.pulse, "");
    // 08:00: the due-no-plan candidate (2 days) is a digest; the Pulse goes out.
    let r = w
        .engine
        .tick(morning + Duration::minutes(1), false)
        .await
        .unwrap();
    assert_eq!(r.pulse, "sent");
    let pulse: Vec<_> = w
        .store
        .outreach_between(morning, morning + Duration::hours(1))
        .await
        .unwrap();
    let p = pulse.iter().find(|o| o.kind == Kind::Pulse).unwrap();
    assert!(!p.unsolicited);
    assert!(
        p.message.contains("18:00 Chiama il commercialista"),
        "{}",
        p.message
    );
    assert!(p.message.contains("Scadenza IVA"), "{}", p.message);
    // Once a day.
    assert_eq!(
        w.engine
            .tick(morning + Duration::minutes(30), false)
            .await
            .unwrap()
            .pulse,
        ""
    );
    // The Pulse does not use the daily budget.
    assert_eq!(
        w.store
            .unsolicited_since(morning - Duration::hours(8))
            .await
            .unwrap(),
        0
    );

    // Next morning: the same deadline only, already in yesterday's Pulse: silent.
    w.feeds.reminders.lock().unwrap().retain(|r| r.id == "r2");
    let r = w
        .engine
        .tick(at("2026-10-23T06:00:00Z"), false)
        .await
        .unwrap();
    assert_eq!(r.pulse, "suppressed: nothing new since the last Pulse");
}

#[tokio::test]
async fn empty_day_no_pulse_and_quiet_morning_waits() {
    let w = world(with_pulse());
    let r = w
        .engine
        .tick(at("2026-10-22T06:00:00Z"), false)
        .await
        .unwrap();
    assert_eq!(r.pulse, "suppressed: nothing to say");
    assert!(w.sink.0.lock().unwrap().is_empty());

    let w = world(with_pulse());
    *w.api.settings.lock().unwrap() = Some(quiet_settings("22:30", "08:30"));
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        at("2026-10-22T16:00:00Z"),
        "Chiama il commercialista",
        "pending",
    ));
    assert_eq!(
        w.engine
            .tick(at("2026-10-22T06:00:00Z"), false)
            .await
            .unwrap()
            .pulse,
        "waiting: quiet hours"
    );
    assert_eq!(
        w.engine
            .tick(at("2026-10-22T06:31:00Z"), false)
            .await
            .unwrap()
            .pulse,
        "sent"
    );
}

#[tokio::test]
async fn sustained_outage_alerts_once_and_recovery_follows() {
    let w = world(config(false));
    let url = "https://getsecretary.tech/v1/health";
    w.probes.0.lock().unwrap().insert(
        url.into(),
        VecDeque::from(vec![
            false, true, false, false, false, false, false, true, true,
        ]),
    );
    let t0 = at(NOON_ISH);
    let mut sent = Vec::new();
    for m in 0..9 {
        let r = w
            .engine
            .tick(t0 + Duration::minutes(2 * m), false)
            .await
            .unwrap();
        sent.extend(r.outreach.into_iter().map(|o| (m, o.kind)));
    }
    // Blip at 0 min ignored; failing from 4 min, down once there are 3+ fails
    // spanning 5+ min (10 min, tick 5); back after 2 good checks (16 min, tick 8).
    // The recovery follows an alert, so it is sent too (cooldown 0 for recoveries).
    assert_eq!(
        sent,
        vec![(5, Kind::ServiceDown), (8, Kind::ServiceRecovered)]
    );
}

#[tokio::test]
async fn followed_thread_resolved_is_sent() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.feeds.timeline.lock().unwrap().extend([
        Entry {
            id: "01K8A".into(),
            ts: now - Duration::hours(20),
            role: "user".into(),
            channel: "telegram".into(),
            text: "Il checkout di pokoin non funziona, fammi sapere quando torna".into(),
            meta: Value::Null,
        },
        Entry {
            id: "01K8B".into(),
            ts: now - Duration::hours(20) + Duration::seconds(5),
            role: "lucina".into(),
            channel: "telegram".into(),
            text: "Va bene, ti tengo d'occhio la cosa".into(),
            meta: Value::Null,
        },
    ]);
    let r = w.engine.tick(now, false).await.unwrap();
    let o = r
        .outreach
        .iter()
        .find(|o| o.kind == Kind::ThreadResolved)
        .expect("resolved thread sent");
    assert!(
        o.message.contains("Il checkout di Pokoin ora funziona."),
        "{}",
        o.message
    );
    assert!(o.why.contains("he asked to be told"));
}

#[tokio::test]
async fn pinned_note_for_tomorrows_meeting_is_pulse_material() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.api.notes.lock().unwrap().push(note(
        "craftit-meeting",
        "CRAFT-IT4SD — meeting with Marianne & Jonas",
        true,
    ));
    w.api
        .events
        .lock()
        .unwrap()
        .push(event("google:t", "CRAFT-IT4SD", now + Duration::hours(24)));
    let r = w.engine.tick(now, false).await.unwrap();
    let d = r
        .decisions
        .iter()
        .find(|d| d.kind == Kind::NoteForEvent)
        .unwrap();
    assert_eq!(d.decision, Decision::Digest, "{}", d.reason);
}

/// Verus kernel `attention_engine.rs` E6: "Basta così" on the Daily Pulse mutes
/// the Pulse for `stop_days` (before the fix the Pulse kept coming).
#[tokio::test]
async fn basta_cosi_on_the_pulse_mutes_the_pulse() {
    let w = world(with_pulse());
    let morning = at("2026-10-22T06:00:00Z");
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r1",
        at("2026-10-22T16:00:00Z"),
        "Chiama il commercialista",
        "pending",
    ));
    let r = w.engine.tick(morning, false).await.unwrap();
    assert_eq!(r.pulse, "sent");
    let pulse = r.outreach.iter().find(|o| o.kind == Kind::Pulse).unwrap();
    w.api.feedback.lock().unwrap().push(json!({"id": "fb_0100", "outreachId": pulse.id, "action": "stop", "at": (morning + Duration::minutes(5)).timestamp_millis()}));
    // Next morning there is something new: still no Pulse.
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r3",
        at("2026-10-23T16:00:00Z"),
        "Ritira il pacco",
        "pending",
    ));
    let r = w
        .engine
        .tick(at("2026-10-23T06:00:00Z"), false)
        .await
        .unwrap();
    assert!(r.pulse.starts_with("suppressed: muted"), "{}", r.pulse);
    assert!(r.outreach.iter().all(|o| o.kind != Kind::Pulse));
    // After 30 days it is back.
    w.feeds.reminders.lock().unwrap().push(reminder(
        "r4",
        at("2026-11-22T16:00:00Z"),
        "Rinnova l'assicurazione",
        "pending",
    ));
    let r = w
        .engine
        .tick(at("2026-11-22T07:00:00Z"), false)
        .await
        .unwrap();
    assert_eq!(r.pulse, "sent");
}

/// G3 (docs/verification.md): the attention budget and the cooldown are
/// read per candidate and only hold within one tick; a `once` run next to
/// the `run` daemon could pass the gate twice. A tick now holds the store's
/// tick lock for its whole run and does nothing when another tick has it.
#[tokio::test]
async fn a_tick_is_skipped_while_another_tick_holds_the_lock() {
    let w = world(config(false));
    let now = at(NOON_ISH);
    w.api.events.lock().unwrap().push(event(
        "google:abc",
        "CRAFT-IT4SD meeting with Marianne",
        now + Duration::minutes(60),
    ));
    w.api.notes.lock().unwrap().push(note(
        "craftit-meeting",
        "CRAFT-IT4SD — meeting with Marianne & Jonas",
        true,
    ));
    // Another process's tick (the daemon) is running.
    let other = w.store.try_tick_lock().await.unwrap().expect("free");
    assert!(w.store.try_tick_lock().await.unwrap().is_none());
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(r.skipped);
    assert!(r.outreach.is_empty() && r.decisions.is_empty());
    assert!(w.sink.0.lock().unwrap().is_empty(), "nothing sent");
    drop(other);
    // Then the next tick runs, and sends once.
    let r = w.engine.tick(now, false).await.unwrap();
    assert!(!r.skipped);
    assert_eq!(r.outreach.len(), 1);
    assert!(w.store.try_tick_lock().await.unwrap().is_some(), "released");
}

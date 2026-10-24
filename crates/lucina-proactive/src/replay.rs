//! The 24-hour replay: the real engine, in dry-run, ticking every few
//! minutes through one local day against the owner's real data, read only:
//!
//! - calendar and notes: read once from secretary-server (service token);
//! - reminders: `reminders.json`, each one as it was at the tick (created
//!   after → absent, sent or cancelled after → still pending);
//! - timeline: items up to the tick;
//! - presence: the watcher's history up to the tick;
//! - health: one live check per target at the start, held for the day (a
//!   past outage cannot be replayed);
//! - quiet hours: the server's settings if readable, else the fallback;
//! - store: in memory. Nothing is written anywhere, nothing is sent.
//!
//! DeepSeek writes the would-be messages (unless `--no-llm`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use lucina_store::Reminder;
use serde_json::{json, Value};

use crate::attention;
use crate::config::{Config, Secrets};
use crate::engine::{DecisionLog, TickReport};
use crate::model::Outreach;
use crate::secretary::{ApiError, CalEvent, NoteSummary, SecretaryApi};
use crate::setup;
use crate::sources::{FilePresence, HealthProbe, Probe, ReminderFeed};
use crate::store::MemStore;

/// Reminders as they were at `now`.
pub struct ReplayReminders {
    pub all: Vec<Reminder>,
}

fn parse_ts(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        Value::Number(n) => n.as_i64().and_then(DateTime::from_timestamp_millis),
        _ => None,
    }
}

impl ReminderFeed for ReplayReminders {
    fn reminders(&self, now: DateTime<Utc>) -> Result<Vec<Reminder>> {
        Ok(self
            .all
            .iter()
            .filter(|r| parse_ts(&r.created_at).is_none_or(|c| c <= now))
            .map(|r| {
                let mut r = r.clone();
                let later = |t: Option<&String>| {
                    t.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .is_some_and(|t| t > now)
                };
                if (r.status == "sent" && later(r.sent_at.as_ref()))
                    || (r.status == "cancelled" && later(r.cancelled_at.as_ref()))
                {
                    r.status = "pending".into();
                    r.last_error = None;
                }
                r
            })
            .collect())
    }
}

/// secretary-server, read once and cached; writes refused.
pub struct CachedSecretary {
    pub inner: Arc<dyn SecretaryApi>,
    pub window: (DateTime<Utc>, DateTime<Utc>),
    events: tokio::sync::OnceCell<Vec<CalEvent>>,
    notes: Mutex<HashMap<String, Vec<NoteSummary>>>,
    settings: tokio::sync::OnceCell<Option<Value>>,
    pub calls: Mutex<Vec<String>>,
    /// Hypothetical events (`--what-if`), added to the real ones.
    pub extra: Mutex<Vec<CalEvent>>,
}

impl CachedSecretary {
    pub fn new(inner: Arc<dyn SecretaryApi>, window: (DateTime<Utc>, DateTime<Utc>)) -> Self {
        Self {
            inner,
            window,
            events: tokio::sync::OnceCell::new(),
            notes: Mutex::new(HashMap::new()),
            settings: tokio::sync::OnceCell::new(),
            calls: Mutex::new(Vec::new()),
            extra: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl SecretaryApi for CachedSecretary {
    async fn calendar_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>, ApiError> {
        let all = self
            .events
            .get_or_try_init(|| async {
                self.calls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("GET /v1/calendar/events".into());
                self.inner
                    .calendar_events(self.window.0, self.window.1)
                    .await
            })
            .await?;
        let (f, t) = (from.timestamp_millis(), to.timestamp_millis());
        let extra = self.extra.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Ok(all
            .iter()
            .chain(extra.iter())
            .filter(|e| e.start_ms < t && e.end_ms.max(e.start_ms + 1) > f)
            .cloned()
            .collect())
    }
    async fn notes(&self, query: Option<&str>) -> Result<Vec<NoteSummary>, ApiError> {
        let key = query.unwrap_or("").to_owned();
        if let Some(v) = self
            .notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(v.clone());
        }
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("GET /v1/notes q={key:?}"));
        let v = self.inner.notes(query).await?;
        self.notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, v.clone());
        Ok(v)
    }
    async fn note(&self, key: &str) -> Result<Option<Value>, ApiError> {
        self.inner.note(key).await
    }
    async fn create_note(&self, _: &str, _: &str, _: &str, _: &[&str]) -> Result<String, ApiError> {
        Err(ApiError::Transport("replay: writes are refused".into()))
    }
    async fn notify(&self, _: &Value) -> Result<Value, ApiError> {
        Err(ApiError::Transport("replay: writes are refused".into()))
    }
    async fn notify_settings(&self) -> Result<Option<Value>, ApiError> {
        Ok(self
            .settings
            .get_or_init(|| async { self.inner.notify_settings().await.ok().flatten() })
            .await
            .clone())
    }
    async fn feedback_since(&self, _: Option<&str>) -> Result<Option<Value>, ApiError> {
        Ok(None)
    }
}

/// One live probe per URL, then the same answer all day.
pub struct FrozenProbe {
    pub inner: Arc<dyn HealthProbe>,
    seen: tokio::sync::Mutex<HashMap<String, Probe>>,
    /// The replay's clock, for hypothetical outages.
    pub clock: Mutex<DateTime<Utc>>,
    /// `--what-if-down`: (URL, from, to).
    pub outages: Vec<(String, DateTime<Utc>, DateTime<Utc>)>,
}

impl FrozenProbe {
    pub fn new(inner: Arc<dyn HealthProbe>) -> Self {
        Self {
            inner,
            seen: tokio::sync::Mutex::new(HashMap::new()),
            clock: Mutex::new(DateTime::<Utc>::MIN_UTC),
            outages: Vec::new(),
        }
    }
    pub async fn snapshot(&self) -> HashMap<String, Probe> {
        self.seen.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl HealthProbe for FrozenProbe {
    async fn probe(&self, url: &str, timeout: StdDuration, slow_ms: u64) -> Probe {
        let now = *self.clock.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .outages
            .iter()
            .any(|(u, f, t)| u == url && now >= *f && now < *t)
        {
            return Probe {
                ok: false,
                status: 503,
                ms: 40,
                error: Some("HTTP 503 (what-if outage)".into()),
            };
        }
        let mut seen = self.seen.lock().await;
        if let Some(p) = seen.get(url) {
            return p.clone();
        }
        let p = self.inner.probe(url, timeout, slow_ms).await;
        seen.insert(url.to_owned(), p.clone());
        p
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ReplayReport {
    pub day: String,
    pub time_zone: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub step_minutes: i64,
    pub generated_at: DateTime<Utc>,
    pub composer: String,
    pub data: Value,
    pub outreach: Vec<Outreach>,
    pub decisions: Vec<DecisionLog>,
    pub pulse: Vec<(DateTime<Utc>, String)>,
    pub errors: Vec<(DateTime<Utc>, String)>,
    /// Hypothetical additions (`--what-if`, `--what-if-down`).
    pub what_if: Vec<String>,
    /// What the sources looked at, for the "why silent" part.
    pub looked_at: Value,
}

pub struct ReplayOptions {
    pub day: NaiveDate,
    pub step_minutes: i64,
    pub llm: bool,
    /// Hypothetical timed events: (local start, title), one hour long.
    pub what_if_events: Vec<(NaiveTime, String)>,
    /// Hypothetical outages: (health target name, local from, local to).
    pub what_if_down: Vec<(String, NaiveTime, NaiveTime)>,
}

/// `HH:MM Title` → (time, title).
pub fn parse_what_if_event(s: &str) -> Result<(NaiveTime, String)> {
    let (t, title) = s
        .trim()
        .split_once(' ')
        .ok_or_else(|| anyhow::anyhow!("expected \"HH:MM Title\""))?;
    let t = NaiveTime::parse_from_str(t, "%H:%M")?;
    Ok((t, title.trim().to_owned()))
}

/// `Name=HH:MM-HH:MM` → (name, from, to).
pub fn parse_what_if_down(s: &str) -> Result<(String, NaiveTime, NaiveTime)> {
    let (name, span) = s
        .rsplit_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected \"Name=HH:MM-HH:MM\""))?;
    let (a, b) = span
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("expected HH:MM-HH:MM"))?;
    Ok((
        name.trim().to_owned(),
        NaiveTime::parse_from_str(a.trim(), "%H:%M")?,
        NaiveTime::parse_from_str(b.trim(), "%H:%M")?,
    ))
}

fn local(day: NaiveDate, t: NaiveTime, tz: chrono_tz::Tz) -> DateTime<Utc> {
    chrono::TimeZone::from_local_datetime(&tz, &day.and_time(t))
        .earliest()
        .map(|x| x.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

pub async fn run(cfg: Config, secrets: &Secrets, opts: ReplayOptions) -> Result<ReplayReport> {
    let mut cfg = cfg;
    cfg.dry_run = true;
    cfg.compose.llm = opts.llm;
    cfg.compose.judge = opts.llm;
    let tz = cfg.tz();
    let noon = opts.day.and_hms_opt(12, 0, 0).unwrap_or_default();
    let anchor = chrono::TimeZone::from_local_datetime(&tz, &noon)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let from = attention::local_at(anchor, tz, 0, 0);
    let to = attention::local_at(anchor, tz, 1, 0);

    let mut parts = setup::live_parts(&cfg, secrets)?;
    let cached = Arc::new(CachedSecretary::new(
        parts.api.clone(),
        (from - Duration::days(1), to + Duration::days(3)),
    ));
    parts.api = cached.clone();
    let (reminders_path, _, presence_dir, _) = crate::sources::node_paths(&cfg.paths.node_data_dir);
    let all = lucina_store::ReminderStore::new(reminders_path, 10_000).list(true)?;
    let reminder_count = all.len();
    let reminders_snapshot = ReplayReminders { all: all.clone() };
    parts.reminders = Arc::new(ReplayReminders { all });
    let mut presence = FilePresence::new(presence_dir);
    presence.history = true;
    parts.presence = Arc::new(presence);
    let mut what_if = Vec::new();
    {
        let mut extra = cached.extra.lock().unwrap_or_else(|e| e.into_inner());
        for (i, (t, title)) in opts.what_if_events.iter().enumerate() {
            let start = local(opts.day, *t, tz);
            let end = start + Duration::hours(1);
            extra.push(CalEvent {
                id: format!("whatif:{i}"),
                source: "what-if".into(),
                calendar: Some("what-if".into()),
                title: title.clone(),
                start: start.to_rfc3339(),
                end: end.to_rfc3339(),
                all_day: false,
                start_ms: start.timestamp_millis(),
                end_ms: end.timestamp_millis(),
                location: None,
                description: None,
            });
            what_if.push(format!(
                "calendar event \"{title}\" at {}",
                t.format("%H:%M")
            ));
        }
    }
    let mut frozen = FrozenProbe::new(parts.probe.clone());
    for (name, a, b) in &opts.what_if_down {
        let url = cfg
            .health
            .targets
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .and_then(|t| {
                t.url.clone().or_else(|| {
                    t.url_env
                        .as_ref()
                        .and_then(|e| secrets.env_urls.get(e).cloned())
                })
            })
            .ok_or_else(|| anyhow::anyhow!("--what-if-down: no health target named {name}"))?;
        frozen
            .outages
            .push((url, local(opts.day, *a, tz), local(opts.day, *b, tz)));
        what_if.push(format!(
            "{name} down from {} to {}",
            a.format("%H:%M"),
            b.format("%H:%M")
        ));
    }
    let frozen = Arc::new(frozen);
    parts.probe = frozen.clone();
    parts.telegram = None;
    let composer = if opts.llm && parts.model.is_some() {
        "deepseek"
    } else {
        "template"
    }
    .to_owned();
    let timeline = parts.timeline.clone();

    let cfg = Arc::new(cfg);
    let store = Arc::new(MemStore::new());
    let engine = setup::engine(cfg.clone(), secrets, store.clone(), parts);

    let mut outreach = Vec::new();
    let mut decisions = Vec::new();
    let mut pulse = Vec::new();
    let mut errors = Vec::new();
    let mut t = from;
    while t < to {
        *frozen.clock.lock().unwrap_or_else(|e| e.into_inner()) = t;
        let r: TickReport = engine.tick(t, false).await?;
        outreach.extend(r.outreach);
        decisions.extend(r.decisions);
        if !r.pulse.is_empty() {
            pulse.push((t, r.pulse));
        }
        errors.extend(r.errors.into_iter().map(|e| (t, e)));
        t += Duration::minutes(opts.step_minutes.max(1));
    }
    let tl = timeline.items(from, to).unwrap_or_default();
    let owner_msgs = tl.iter().filter(|e| e.role == "user").count();
    let events = cached
        .calendar_events(from, to)
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    let notes = cached.notes(None).await.map(|v| v.len()).unwrap_or(0);
    let settings = cached.notify_settings().await.ok().flatten();
    let probes = frozen.snapshot().await;
    let calls = cached
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let looked_at = looked_at(&cfg, &cached, &timeline, &reminders_snapshot, to).await;
    Ok(ReplayReport {
        day: opts.day.to_string(),
        time_zone: cfg.time_zone.clone(),
        from,
        to,
        step_minutes: opts.step_minutes,
        generated_at: Utc::now(),
        composer,
        data: json!({
            "timelineItemsThatDay": tl.len(),
            "ownerMessagesThatDay": owner_msgs,
            "calendarEventsThatDay": events,
            "notes": notes,
            "remindersInStore": reminder_count,
            "quietHoursFrom": if settings.is_some() { "server" } else { "fallback config" },
            "health": probes.iter().map(|(u, p)| json!({"url": u, "ok": p.ok, "status": p.status, "ms": p.ms, "error": p.error})).collect::<Vec<_>>(),
            "readOnlyCalls": calls,
        }),
        outreach,
        decisions,
        pulse,
        errors,
        what_if,
        looked_at,
    })
}

/// Why it was quiet: what each source had to work with at the end of the day.
async fn looked_at(
    cfg: &Config,
    api: &CachedSecretary,
    timeline: &Arc<dyn crate::sources::TimelineFeed>,
    reminders: &ReplayReminders,
    to: DateTime<Utc>,
) -> Value {
    let tz = cfg.tz();
    let now = to.min(Utc::now());
    let pinned: Vec<String> = api
        .notes(None)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.pinned)
        .map(|n| n.slug)
        .collect();
    let pending: Vec<Reminder> = reminders
        .reminders(now)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.status == "pending")
        .collect();
    let mut deadlines: Vec<(DateTime<Utc>, String)> = pending
        .iter()
        .filter(|r| crate::sources::reminders::looks_like_deadline(&r.text))
        .filter_map(|r| {
            r.due()
                .filter(|d| *d > now)
                .map(|d| (d, crate::text::clip(&r.text, 60)))
        })
        .collect();
    deadlines.sort();
    let sc = &cfg.sources;
    let items = timeline
        .items(now - Duration::days(sc.thread_lookback_days + 1), now)
        .unwrap_or_default();
    let threads = crate::sources::threads::find_threads(
        &items,
        now,
        sc.thread_lookback_days,
        sc.thread_quiet_hours,
    );
    let via_mcp = items
        .iter()
        .filter(|e| e.role == "user" && e.meta.get("via").and_then(Value::as_str) == Some("mcp"))
        .count();
    json!({
        "pinnedNotes": pinned,
        "pendingReminders": pending.len(),
        "nextDeadlineLikeReminders": deadlines.iter().take(3).map(|(d, t)| json!({"due": d.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string(), "text": t})).collect::<Vec<_>>(),
        "ownerMessagesInThreadWindow": items.iter().filter(|e| e.role == "user").count(),
        "ownerMessagesViaMcpExcluded": via_mcp,
        "openThreadCandidates": threads.len(),
    })
}

/// The report for a human: what would have been sent, when and why, then
/// every decision.
pub fn render(r: &ReplayReport, tz: chrono_tz::Tz) -> String {
    let hm = |t: DateTime<Utc>| t.with_timezone(&tz).format("%H:%M").to_string();
    let mut s = format!(
        "# Lucina proactive: 24-hour dry-run replay, {} ({})\n\nTicks every {} min from {} to {} local. Composer: {}. Generated {}.\n\n",
        r.day,
        r.time_zone,
        r.step_minutes,
        hm(r.from),
        hm(r.to),
        r.composer,
        r.generated_at.with_timezone(&tz).format("%Y-%m-%d %H:%M")
    );
    if !r.what_if.is_empty() {
        s.push_str(&format!(
            "**HYPOTHETICAL** additions to the real data (not real events): {}.\n\n",
            r.what_if.join("; ")
        ));
    }
    s.push_str(&format!(
        "What the sources had to work with: {}\n\n",
        serde_json::to_string(&r.looked_at).unwrap_or_default()
    ));
    s.push_str(&format!(
        "Data: {}\n\n",
        serde_json::to_string(&r.data).unwrap_or_default()
    ));
    s.push_str(&format!("## Would have sent: {}\n\n", r.outreach.len()));
    if r.outreach.is_empty() {
        s.push_str("Nothing. Lucina would have stayed silent all day.\n\n");
    }
    for o in &r.outreach {
        s.push_str(&format!(
            "### {} · {} · {} · priority {} · via {}\n\n{}\n\nButtons: {}\n\nWhy: {}\n\n",
            hm(o.sent_at),
            o.kind,
            o.channel,
            o.priority.as_str(),
            o.result
                .get("composer")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            o.message,
            o.actions
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
            o.why
        ));
        if let Some(n) = o
            .result
            .pointer("/prepNote/wouldCreate")
            .and_then(Value::as_str)
        {
            s.push_str(&format!("Prep note it would save: `{n}`\n\n"));
            if let Some(c) = o
                .result
                .pointer("/prepNote/content")
                .and_then(Value::as_str)
            {
                s.push_str(&format!("```markdown\n{}\n```\n\n", c.trim()));
            }
        }
    }
    s.push_str("## Daily Pulse\n\n");
    if r.pulse.is_empty() {
        s.push_str("No Pulse evaluation.\n\n");
    }
    for (t, p) in &r.pulse {
        s.push_str(&format!("- {}: {p}\n", hm(*t)));
    }
    s.push_str(&format!("\n## Every decision ({})\n\n", r.decisions.len()));
    for d in &r.decisions {
        s.push_str(&format!(
            "- {} {} [{}] {} score {:.2}: {}. {}\n",
            hm(d.at),
            d.kind,
            d.source,
            d.decision.as_str(),
            d.score,
            d.subject,
            d.reason
        ));
    }
    if !r.errors.is_empty() {
        s.push_str(&format!("\n## Errors ({})\n\n", r.errors.len()));
        let mut seen = std::collections::BTreeSet::new();
        for (t, e) in &r.errors {
            if seen.insert(e.clone()) {
                s.push_str(&format!("- {} {e}\n", hm(*t)));
            }
        }
    }
    s
}

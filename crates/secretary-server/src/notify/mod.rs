//! Notifications (`docs/notifications.md`): an inbox with read state,
//! per-category settings with quiet hours, Do Not Disturb and per-device
//! overrides, Web Push to browsers and home-screen web apps, an APNs
//! adapter for the native app (off until configured) and the producers:
//!
//! - [`ingest`]: Lucina's timeline stream (messages, reminders, device
//!   commands, shopping changes by Lucina);
//! - `POST /v1/notify`: Lucina pushes directly (service token);
//! - the phone's device state: calendar events, `calendarLeadMinutes`
//!   before they start ([`Notifier::tick`]);
//! - the server itself: Lucina unreachable for 5 minutes / back
//!   (`presence`), a new device signing in (`system`).
//!
//! Every notification goes through [`Notifier::publish`]: dedup (explicit
//! key or `tl:<timeline id>`), the category switch, presence (Lucina
//! messages are dropped while the owner has the app in front), quiet
//! hours / DND (stored, pushed later as one digest), then the inbox and
//! the push targets each device allows.

pub mod apns;
pub mod ingest;
pub mod model;
pub mod push;
pub mod routes;
pub mod settings;
pub mod store;
pub mod text;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use secretary_core::models::{DeviceState, TimelineItem};
use serde_json::{json, Value};

pub use model::{Category, NewNotification, Notification, Outcome};
use push::{Payload, PushConfig, PushOpts, Pusher, Subscription};
use store::{CalendarEvent, Inbox, NotifyState, SettingsFile, StateFile};

use crate::device::SeenIds;
use crate::AppState;

#[derive(Debug, Clone)]
pub struct NotifyConfig {
    /// Run the timeline ingester (needs the Lucina token). `SECRETARY_NOTIFY_INGEST`.
    pub ingest: bool,
    pub push: PushConfig,
    /// Calendar / digest / offline check interval.
    pub tick: Duration,
    /// Timeline items older than this make no notification (replays after downtime).
    pub stale: Duration,
    /// A presence heartbeat counts for this long.
    pub presence_ttl: Duration,
    /// Lucina unreachable this long → a `presence` notification.
    pub offline_after: Duration,
    /// Unread Lucina messages within this window are counted in the push title.
    pub burst_window: Duration,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            ingest: true,
            push: PushConfig::default(),
            tick: Duration::from_secs(30),
            stale: Duration::from_secs(6 * 3600),
            presence_ttl: Duration::from_secs(90),
            offline_after: Duration::from_secs(300),
            burst_window: Duration::from_secs(1800),
        }
    }
}

#[derive(Default)]
struct Link {
    down_since: Option<i64>,
    announced: bool,
}

enum Admit {
    Skip(Outcome),
    Stored(Box<Notification>, bool),
}

pub struct Notifier {
    pub cfg: NotifyConfig,
    pub inbox: Inbox,
    pub settings: SettingsFile,
    pub state: StateFile,
    pub pusher: Pusher,
    presence: Mutex<HashMap<String, Instant>>,
    seen: SeenIds,
    admit_lock: Mutex<()>,
    link: Mutex<Link>,
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Epoch ms of a device-state `start` (RFC 3339 or a number).
fn start_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .ok()
                .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
        }
        _ => None,
    }
}

/// Timed (not all-day) events from a device snapshot.
pub fn calendar_events(state: &DeviceState, now: i64) -> Vec<CalendarEvent> {
    state
        .events
        .iter()
        .filter(|e| e["allDay"] != Value::Bool(true))
        .filter_map(|e| {
            let id = e["id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)?;
            let start = start_ms(&e["start"])?;
            (start > now - 3_600_000).then(|| CalendarEvent {
                id: id.to_owned(),
                title: model::clip(e["title"].as_str().unwrap_or(""), 120),
                start,
            })
        })
        .take(200)
        .collect()
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> io::Result<R> + Send + 'static,
) -> io::Result<R> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?
}

impl Notifier {
    pub fn open(dir: &Path, cfg: NotifyConfig) -> anyhow::Result<Self> {
        crate::devices::ensure_dir(dir)?;
        Ok(Self {
            inbox: Inbox::open(dir),
            settings: SettingsFile::open(dir, store::SETTINGS_FILE),
            state: StateFile::open(dir, store::STATE_FILE),
            pusher: Pusher::open(dir, cfg.push.clone())?,
            presence: Mutex::default(),
            seen: SeenIds::new(4096),
            admit_lock: Mutex::default(),
            link: Mutex::default(),
            cfg,
        })
    }

    // -- presence ------------------------------------------------------------

    /// A client heartbeat: the app is (`active`) or is no longer in front on `device`.
    pub fn set_presence(&self, device: &str, active: bool) {
        let mut p = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        if active {
            p.insert(device.to_owned(), Instant::now());
        } else {
            p.remove(device);
        }
    }

    /// Whether any device reported the app in front within the TTL.
    pub fn present(&self) -> bool {
        let ttl = self.cfg.presence_ttl;
        let mut p = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|_, at| at.elapsed() < ttl);
        !p.is_empty()
    }

    // -- publishing ------------------------------------------------------------

    fn admit(&self, n: NewNotification, now: i64) -> io::Result<Admit> {
        let _guard = self.admit_lock.lock().unwrap_or_else(|e| e.into_inner());
        let category = n.category.unwrap_or(Category::System);
        let keys = n.keys();
        if keys.iter().any(|k| self.seen.contains(k)) || self.inbox.has_any_key(&keys) {
            return Ok(Admit::Skip(Outcome::Duplicate));
        }
        let remember = || {
            for k in &keys {
                self.seen.insert(k);
            }
        };
        let settings = self.settings.get();
        if !settings.categories.get(category) {
            remember();
            return Ok(Admit::Skip(Outcome::Disabled));
        }
        if category == Category::LucinaMessage && self.present() {
            remember();
            return Ok(Admit::Skip(Outcome::Present));
        }
        let quiet = settings.quiet_at(now)
            && !(category == Category::Reminder && settings.reminders_bypass_quiet);
        let notification = Notification {
            id: model::new_id(now),
            ts: now,
            category,
            title: n.title,
            body: n.body,
            route: n
                .route
                .unwrap_or_else(|| category.default_route().to_owned()),
            channel: n.channel,
            timeline_id: n.timeline_id,
            dedup_key: n.dedup_key,
            data: n.data,
            read: false,
            read_at: None,
            deferred: quiet && settings.digest,
        };
        self.inbox.add(notification.clone())?;
        remember();
        // Agents (Paseo): while an app is in front the inbox shows it, no push.
        let in_app = category == Category::Agent && self.present();
        Ok(Admit::Stored(Box::new(notification), !quiet && !in_app))
    }

    /// Store and (unless quiet) push one notification.
    pub async fn publish(self: &Arc<Self>, n: NewNotification) -> io::Result<Outcome> {
        let me = self.clone();
        let now = model::now_ms();
        match blocking(move || me.admit(n, now)).await? {
            Admit::Skip(o) => Ok(o),
            Admit::Stored(notification, push) => {
                let pushed = if push {
                    let targets = self.targets_for(&notification, now);
                    let count = targets.len();
                    if count > 0 {
                        let opts = PushOpts::for_category(notification.category);
                        tokio::spawn(push::deliver_all(self.clone(), targets, opts));
                    }
                    count
                } else {
                    0
                };
                Ok(Outcome::Created {
                    deferred: !push,
                    notification,
                    pushed,
                })
            }
        }
    }

    fn sub_allows(settings: &settings::Settings, c: Category, s: &Subscription) -> bool {
        settings.allows(c, &s.id) && settings.allows(c, &s.device_id)
    }

    /// The subscriptions this notification goes to, each with its payload.
    pub fn targets_for(&self, n: &Notification, now: i64) -> Vec<(Subscription, Payload)> {
        let settings = self.settings.get();
        let unread = self.inbox.unread();
        let burst = if n.category == Category::LucinaMessage {
            self.inbox
                .unread_since(Category::LucinaMessage, now - ms(self.cfg.burst_window))
        } else {
            0
        };
        self.pusher
            .list()
            .into_iter()
            .filter(|s| Self::sub_allows(&settings, n.category, s))
            .map(|s| {
                let l = text::lang(s.lang.as_deref().or(Some(settings.language.as_str())));
                let title = match &n.title {
                    Some(t) => t.clone(),
                    None if burst >= 2 => text::lucina_burst(burst, l),
                    None if n.category == Category::Agent => {
                        text::agent_title(n.data.as_ref().and_then(|d| d["reason"].as_str()), l).to_owned()
                    }
                    None => text::title(n.category, l).to_owned(),
                };
                let body = if settings.previews {
                    n.body.clone()
                } else {
                    text::hidden_body(l).to_owned()
                };
                let payload = Payload {
                    id: Some(n.id.clone()),
                    category: n.category.as_str().to_owned(),
                    title,
                    body,
                    route: n.route.clone(),
                    tag: Some(
                        n.category
                            .collapse_key()
                            .map_or_else(|| n.id.clone(), str::to_owned),
                    ),
                    ts: n.ts,
                    unread,
                    digest: false,
                };
                (s, payload)
            })
            .collect()
    }

    /// A timeline item from the ingester; remembers its id as the resume point.
    pub async fn ingest(self: &Arc<Self>, item: &TimelineItem, sse_id: Option<String>) {
        let now = model::now_ms();
        if let Some(n) = ingest::classify(item, now, ms(self.cfg.stale)) {
            if let Err(e) = self.publish(n).await {
                tracing::warn!(error = %e, "could not store a notification");
            }
        }
        let id = sse_id.unwrap_or_else(|| item.id.to_text());
        if secretary_core::validate::id("id", &id).is_ok() {
            let me = self.clone();
            let saved = blocking(move || me.state.update(|s| s.last_event_id = Some(id))).await;
            if let Err(e) = saved {
                tracing::warn!(error = %e, "could not save the timeline position");
            }
        }
    }

    /// Lucina's stream connected (`true`) or failed (`false`).
    pub async fn lucina_link(self: &Arc<Self>, up: bool) {
        let now = model::now_ms();
        let back = {
            let mut l = self.link.lock().unwrap_or_else(|e| e.into_inner());
            if up {
                let since = l.down_since.take();
                std::mem::take(&mut l.announced).then_some(since).flatten()
            } else {
                l.down_since.get_or_insert(now);
                None
            }
        };
        if let Some(since) = back {
            let lang = self.settings.read(|s| s.language.clone());
            let mut n = NewNotification::new(Category::Presence, text::lucina_online(&lang));
            n.dedup_key = Some(format!("presence:up:{since}"));
            let _ = self.publish(n).await;
        }
    }

    /// The phone's snapshot: keep its upcoming timed events for calendar notifications.
    pub fn device_state(&self, device_id: &str, snapshot: &DeviceState) -> io::Result<()> {
        let events = calendar_events(snapshot, model::now_ms());
        let device = device_id.to_owned();
        self.state.update(|s| {
            if events.is_empty() {
                s.calendar.remove(&device);
            } else {
                s.calendar.insert(device, events);
            }
        })
    }

    /// Periodic work: calendar notifications, the digest after quiet hours,
    /// the "Lucina unreachable" notice.
    pub async fn tick(self: &Arc<Self>, now: i64) {
        let settings = self.settings.get();

        // Calendar: `lead` minutes before start (catching up until a minute after).
        if settings.categories.calendar {
            let lead = i64::from(settings.calendar_lead_minutes) * 60_000;
            let due: Vec<CalendarEvent> = self.state.read(|s: &NotifyState| {
                let mut v: Vec<CalendarEvent> = s
                    .calendar
                    .values()
                    .flatten()
                    .filter(|e| e.start - lead <= now && now <= e.start + 60_000)
                    .cloned()
                    .collect();
                v.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
                v.dedup_by(|a, b| a.id == b.id && a.start == b.start);
                v
            });
            for e in due {
                let body = if e.title.is_empty() {
                    settings.local_hhmm(e.start)
                } else {
                    format!("{} · {}", e.title, settings.local_hhmm(e.start))
                };
                let mut n = NewNotification::new(Category::Calendar, body);
                n.dedup_key = Some(format!("cal:{}:{}", e.id, e.start));
                n.data = Some(json!({"eventId": e.id, "start": e.start}));
                if let Err(err) = self.publish(n).await {
                    tracing::warn!(error = %err, "could not store a calendar notification");
                }
            }
        }

        // Digest of what quiet hours / DND held back.
        if !settings.quiet_at(now) {
            let me = self.clone();
            match blocking(move || me.inbox.take_deferred()).await {
                Ok(held) if !held.is_empty() && settings.digest => {
                    self.push_digest(&held, &settings, now).await;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "could not read held notifications"),
            }
        }

        // Lucina unreachable for a while.
        let announce = {
            let mut l = self.link.lock().unwrap_or_else(|e| e.into_inner());
            match l.down_since {
                Some(since) if !l.announced && now - since >= ms(self.cfg.offline_after) => {
                    l.announced = true;
                    Some(since)
                }
                _ => None,
            }
        };
        if let Some(since) = announce {
            let mut n =
                NewNotification::new(Category::Presence, text::lucina_offline(&settings.language));
            n.dedup_key = Some(format!("presence:down:{since}"));
            let _ = self.publish(n).await;
        }
    }

    async fn push_digest(
        self: &Arc<Self>,
        held: &[Notification],
        settings: &settings::Settings,
        now: i64,
    ) {
        let unread = self.inbox.unread();
        let mut targets = Vec::new();
        for s in self.pusher.list() {
            let mut counts: Vec<(Category, usize)> = Vec::new();
            for c in Category::ALL {
                let k = held
                    .iter()
                    .filter(|n| n.category == c && Self::sub_allows(settings, c, &s))
                    .count();
                if k > 0 {
                    counts.push((c, k));
                }
            }
            if counts.is_empty() {
                continue;
            }
            let l = text::lang(s.lang.as_deref().or(Some(settings.language.as_str())));
            let payload = Payload {
                id: None,
                category: "digest".into(),
                title: text::digest_title(l).into(),
                body: text::digest_body(&counts, l),
                route: "/".into(),
                tag: Some("digest".into()),
                ts: now,
                unread,
                digest: true,
            };
            targets.push((s, payload));
        }
        if !targets.is_empty() {
            let opts = PushOpts {
                urgent: false,
                topic: Some("digest".into()),
            };
            tokio::spawn(push::deliver_all(self.clone(), targets, opts));
        }
    }

    /// Send a test push to `targets` (all of the caller's when empty) and wait.
    pub async fn test_push(
        self: &Arc<Self>,
        subs: Vec<Subscription>,
    ) -> Vec<(String, push::Delivery)> {
        let settings = self.settings.get();
        let unread = self.inbox.unread();
        let now = model::now_ms();
        let targets = subs
            .into_iter()
            .map(|s| {
                let l = text::lang(s.lang.as_deref().or(Some(settings.language.as_str())));
                let payload = Payload {
                    id: None,
                    category: "system".into(),
                    title: "Secretary".into(),
                    body: text::test_body(l).into(),
                    route: "/".into(),
                    tag: Some("test".into()),
                    ts: now,
                    unread,
                    digest: false,
                };
                (s, payload)
            })
            .collect();
        push::deliver_all(
            self.clone(),
            targets,
            PushOpts {
                urgent: true,
                topic: None,
            },
        )
        .await
    }
}

/// Resolves once the server is shutting down (drops the watch guard, so
/// the future stays `Send`).
pub async fn stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

/// Start the ingester and the ticker (from `serve`, and from tests).
pub fn spawn_workers(st: &AppState) {
    if st.notify.cfg.ingest && st.lucina.is_some() {
        tokio::spawn(ingest::run(st.clone()));
    }
    let st = st.clone();
    tokio::spawn(async move {
        let mut shutdown = st.shutdown.subscribe();
        let mut every = tokio::time::interval(st.notify.cfg.tick);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = every.tick() => st.notify.tick(model::now_ms()).await,
                () = stopped(&mut shutdown) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notifier(dir: &Path) -> Arc<Notifier> {
        Arc::new(Notifier::open(dir, NotifyConfig::default()).unwrap())
    }

    #[tokio::test]
    async fn publish_dedups_and_respects_switches_presence_and_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        let mut m = NewNotification::new(Category::Reminder, "Call the dentist");
        m.timeline_id = Some("01A".into());
        assert_eq!(n.publish(m.clone()).await.unwrap().status(), "created");
        assert_eq!(n.publish(m.clone()).await.unwrap(), Outcome::Duplicate);
        // Same reminder from /v1/notify with the timeline id: still one.
        let mut via_lucina = NewNotification::new(Category::Reminder, "Call the dentist");
        via_lucina.dedup_key = Some("reminder:42".into());
        via_lucina.timeline_id = Some("01A".into());
        assert_eq!(n.publish(via_lucina).await.unwrap(), Outcome::Duplicate);
        assert_eq!(n.inbox.unread(), 1);

        n.settings
            .update(|s| s.categories.shopping = false)
            .unwrap();
        let s = NewNotification::new(Category::Shopping, "Milk added");
        assert_eq!(n.publish(s).await.unwrap(), Outcome::Disabled);

        n.set_presence("dev_1", true);
        let l = NewNotification::new(Category::LucinaMessage, "Ciao");
        assert_eq!(n.publish(l.clone()).await.unwrap(), Outcome::Present);
        n.set_presence("dev_1", false);
        assert!(matches!(
            n.publish(l).await.unwrap(),
            Outcome::Created { .. }
        ));

        // DND: stored, deferred for the digest, not pushed. Reminders bypass.
        n.settings
            .update(|s| s.dnd_until = Some(model::now_ms() + 60_000))
            .unwrap();
        let o = n
            .publish(NewNotification::new(Category::LucinaMessage, "later"))
            .await
            .unwrap();
        assert!(matches!(o, Outcome::Created { deferred: true, .. }));
        let o = n
            .publish(NewNotification::new(Category::Reminder, "now"))
            .await
            .unwrap();
        assert!(matches!(
            o,
            Outcome::Created {
                deferred: false,
                ..
            }
        ));
        assert_eq!(
            n.inbox
                .page(None, 50, false)
                .items
                .iter()
                .filter(|x| x.deferred)
                .count(),
            1
        );
        // DND over: the tick takes the held one for the digest.
        n.settings.update(|s| s.dnd_until = None).unwrap();
        n.tick(model::now_ms()).await;
        assert_eq!(
            n.inbox
                .page(None, 50, false)
                .items
                .iter()
                .filter(|x| x.deferred)
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn calendar_events_fire_lead_minutes_before_once() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        let now = model::now_ms();
        let in5 = now + 5 * 60_000;
        let snapshot: DeviceState = serde_json::from_value(json!({
            "events": [
                {"id": "E1", "title": "Dentist", "start": in5, "allDay": false},
                {"id": "E2", "title": "Holiday", "start": in5, "allDay": true},
                {"id": "E3", "title": "Later", "start": now + 3 * 3_600_000},
                {"id": "", "title": "no id", "start": in5}
            ]
        }))
        .unwrap();
        n.device_state("dev_phone", &snapshot).unwrap();
        assert_eq!(n.state.read(|s| s.calendar["dev_phone"].len()), 2);
        n.tick(now).await;
        n.tick(now + 1000).await;
        let items = n.inbox.page(None, 50, false).items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, Category::Calendar);
        assert!(items[0].body.starts_with("Dentist · "));
        assert_eq!(
            items[0].dedup_key.as_deref(),
            Some(format!("cal:E1:{in5}").as_str())
        );
    }

    #[tokio::test]
    async fn agent_notices_stay_in_the_inbox_while_an_app_is_in_front() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        n.set_presence("dev_1", true);
        let mut a = NewNotification::new(Category::Agent, "Run npm test");
        a.dedup_key = Some("perm:agent-1/req-1".into());
        a.data = Some(json!({"kind": "agent", "reason": "permission", "agentId": "agent-1", "requestId": "req-1"}));
        // Unlike a Lucina message it is kept (Paseo: in-app instead of a push).
        assert!(matches!(n.publish(a.clone()).await.unwrap(), Outcome::Created { pushed: 0, .. }));
        assert_eq!(n.publish(a).await.unwrap(), Outcome::Duplicate);
        assert_eq!(text::agent_title(Some("permission"), "it"), "Un agente chiede il permesso");
        assert_eq!(text::agent_title(None, "en"), "Agent finished");
        n.settings.update(|s| s.categories.agent = false).unwrap();
        assert_eq!(n.publish(NewNotification::new(Category::Agent, "done")).await.unwrap(), Outcome::Disabled);
    }

    #[tokio::test]
    async fn lucina_offline_and_back_are_presence_notices() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        n.settings.update(|s| s.categories.presence = true).unwrap();
        n.lucina_link(false).await;
        let now = model::now_ms();
        n.tick(now).await;
        assert_eq!(n.inbox.unread(), 0, "not yet");
        n.tick(now + 301_000).await;
        n.tick(now + 400_000).await;
        assert_eq!(n.inbox.unread(), 1);
        n.lucina_link(true).await;
        let items = n.inbox.page(None, 50, false).items;
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.category == Category::Presence));
        n.lucina_link(true).await;
        assert_eq!(n.inbox.unread(), 2);
    }
}

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
//!
//! Before that, a timeline item passes [`reply_route`]: a reply goes where he
//! asked (silent when the device he asked on shows it, held [`reply_route::GRACE`]
//! for a client that gets no pushes, pushed if that client is gone by then), test
//! and agent traffic never pushes, Lucina on her own pushes by category.

pub mod apns;
pub mod ingest;
pub mod live;
pub mod model;
pub mod push;
pub mod reply_route;
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
    /// How long a reply to a client without pushes waits for that client ([`reply_route::GRACE`]).
    pub reply_grace: Duration,
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
            reply_grace: reply_route::GRACE,
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

/// Which devices a push goes to (by the device that registered the subscription).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    All,
    Only(String),
    Except(String),
}

impl Audience {
    pub fn includes(&self, device_id: &str) -> bool {
        match self {
            Audience::All => true,
            Audience::Only(d) => d == device_id,
            Audience::Except(d) => d != device_id,
        }
    }
}

pub struct Notifier {
    pub cfg: NotifyConfig,
    pub inbox: Inbox,
    pub settings: SettingsFile,
    pub state: StateFile,
    pub pusher: Pusher,
    /// The approval Live Activity's push tokens ([`live`]).
    pub live: live::LiveTokens,
    presence: Mutex<HashMap<String, Instant>>,
    seen: SeenIds,
    admit_lock: Mutex<()>,
    link: Mutex<Link>,
    /// Epoch ms of each device's latest request (its start and its end).
    activity: Mutex<HashMap<String, i64>>,
    /// Open timeline streams and voice sockets per device.
    links: Arc<Mutex<HashMap<String, usize>>>,
}

/// A device's open timeline stream or voice socket; closes on drop.
pub struct DeviceLink {
    device: String,
    links: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for DeviceLink {
    fn drop(&mut self) {
        let mut l = self.links.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = l.get_mut(&self.device) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                l.remove(&self.device);
            }
        }
    }
}

/// The notification kind [`reply_route`] decides on.
fn route_kind(c: Category) -> reply_route::Kind {
    match c {
        Category::Reminder => reply_route::Kind::Reminder,
        Category::DeviceConfirm => reply_route::Kind::DeviceConfirm,
        Category::Shopping => reply_route::Kind::Shopping,
        _ => reply_route::Kind::Message,
    }
}

/// A reply's own clock may run a little ahead of ours: requests up to this much before it count.
const CLOCK_SLACK_MS: i64 = 1000;

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
/// Where Lucina's own calendar keeps its upcoming events in the notify state
/// (beside the devices' snapshots).
pub const OWN_CALENDAR_KEY: &str = "lucina-calendar";

/// The Calendar tab on an event: `/calendar?event=<merged id>&at=<ms>`.
/// Lucina's own events carry their merged id (`lucina:…`); a phone's carry
/// the EventKit id, which the calendar shows as `iphone:<id>@<start>`.
pub fn calendar_route(e: &CalendarEvent) -> String {
    let id = if e.id.starts_with("lucina:") {
        e.id.clone()
    } else {
        format!("iphone:{}@{}", e.id, e.start)
    };
    let mut u = reqwest::Url::parse("http://app/calendar").expect("static url");
    u.query_pairs_mut()
        .append_pair("event", &id)
        .append_pair("at", &e.start.to_string());
    format!("{}?{}", u.path(), u.query().unwrap_or_default())
}

/// Which calendar alert is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Alert {
    /// The owner's `calendarLeadMinutes` before the start.
    Lead,
    /// One of the event's own alerts (epoch ms).
    At(i64),
    /// Time to leave (epoch ms).
    Leave(i64),
}

/// How long a missed alert is still sent (the server was down, a tick was late).
const ALERT_CATCH_UP_MS: i64 = 5 * 60_000;

/// The alert of `e` due at `now`, if any: the leave alert first, then the
/// latest of its own alerts, else (no alerts of its own) the lead.
pub fn due_alert(e: &CalendarEvent, lead: i64, now: i64) -> Option<Alert> {
    let started = now > e.start + 60_000;
    let fires = |at: i64| at <= now && now <= at + ALERT_CATCH_UP_MS && !started;
    if let Some(at) = e.leave_at.filter(|at| fires(*at)) {
        return Some(Alert::Leave(at));
    }
    match &e.alerts {
        None => (e.start - lead <= now && !started).then_some(Alert::Lead),
        Some(alerts) => alerts
            .iter()
            .copied()
            .filter(|at| fires(*at))
            .max()
            .map(Alert::At),
    }
}

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
                alerts: None,
                leave_at: None,
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
            live: live::LiveTokens::open(dir),
            presence: Mutex::default(),
            seen: SeenIds::new(4096),
            admit_lock: Mutex::default(),
            link: Mutex::default(),
            activity: Mutex::default(),
            links: Arc::default(),
            cfg,
        })
    }

    // -- the asking device ---------------------------------------------------

    /// `device` made a request (called at its start and at its end).
    pub fn saw(&self, device: &str) {
        let mut a = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        a.insert(device.to_owned(), model::now_ms());
    }

    /// Whether `device` made a request at or after `since` (epoch ms).
    pub fn active_since(&self, device: &str, since: i64) -> bool {
        let a = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        a.get(device).is_some_and(|at| *at >= since)
    }

    /// `device` opened a timeline stream or voice socket; it counts until the guard drops.
    pub fn link(&self, device: &str) -> DeviceLink {
        let mut l = self.links.lock().unwrap_or_else(|e| e.into_inner());
        *l.entry(device.to_owned()).or_insert(0) += 1;
        DeviceLink {
            device: device.to_owned(),
            links: self.links.clone(),
        }
    }

    /// Whether `device` holds an open timeline stream or voice socket.
    pub fn connected(&self, device: &str) -> bool {
        let l = self.links.lock().unwrap_or_else(|e| e.into_inner());
        l.get(device).is_some_and(|n| *n > 0)
    }

    /// Whether `device` holds a push subscription itself (the iPhone app).
    pub fn push_device(&self, device: &str) -> bool {
        self.pusher.list().iter().any(|s| s.device_id == device)
    }

    /// Whether the app on `device` reported itself in front within the TTL.
    pub fn present_on(&self, device: &str) -> bool {
        let ttl = self.cfg.presence_ttl;
        let p = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        p.get(device).is_some_and(|at| at.elapsed() < ttl)
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
        let replace = category.replaces_by_key() && n.dedup_key.is_some();
        if !replace && (keys.iter().any(|k| self.seen.contains(k)) || self.inbox.has_any_key(&keys))
        {
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
            && !category.always_on()
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
        if replace {
            if let Some(k) = &notification.dedup_key {
                self.inbox.remove_key(k)?;
            }
        }
        self.inbox.add(notification.clone())?;
        remember();
        // Agents (Paseo): while an app is in front the inbox shows it, no push.
        let in_app = category == Category::Agent && self.present();
        Ok(Admit::Stored(Box::new(notification), !quiet && !in_app))
    }

    /// Store and (unless quiet) push one notification to every device.
    pub async fn publish(self: &Arc<Self>, n: NewNotification) -> io::Result<Outcome> {
        self.publish_to(n, &Audience::All).await
    }

    /// [`Self::publish`], with the push going only to `audience` (the inbox
    /// entry is the same either way).
    pub async fn publish_to(
        self: &Arc<Self>,
        n: NewNotification,
        audience: &Audience,
    ) -> io::Result<Outcome> {
        let me = self.clone();
        let now = model::now_ms();
        match blocking(move || me.admit(n, now)).await? {
            Admit::Skip(o) => Ok(o),
            Admit::Stored(notification, push) => {
                let pushed = if push {
                    let targets = self.targets_for_audience(&notification, now, audience);
                    let count = targets.len();
                    if count > 0 {
                        let opts = PushOpts::for_notification(&notification);
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

    /// Push an already stored notification to `audience` (the escalation of
    /// an approval to the devices that were not asked first). Returns how
    /// many subscriptions were sent to.
    pub async fn push_stored(self: &Arc<Self>, n: &Notification, audience: &Audience) -> usize {
        let targets = self.targets_for_audience(n, model::now_ms(), audience);
        let count = targets.len();
        if count > 0 {
            let opts = PushOpts::for_notification(n);
            push::deliver_all(self.clone(), targets, opts).await;
        }
        count
    }

    /// The subscriptions this notification goes to, each with its payload.
    pub fn targets_for(&self, n: &Notification, now: i64) -> Vec<(Subscription, Payload)> {
        self.targets_for_audience(n, now, &Audience::All)
    }

    /// [`Self::targets_for`], narrowed to `audience`.
    pub fn targets_for_audience(
        &self,
        n: &Notification,
        now: i64,
        audience: &Audience,
    ) -> Vec<(Subscription, Payload)> {
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
            .filter(|s| audience.includes(&s.device_id))
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
                let approval = (n.category == Category::Approval)
                    .then(|| {
                        let d = n.data.as_ref()?;
                        Some((
                            d["approvalId"].as_str()?.to_owned(),
                            d["sensitive"].as_bool().unwrap_or(false),
                            d["payloadHash"].as_str().map(str::to_owned),
                            d["detailsCut"].as_bool().unwrap_or(false),
                        ))
                    })
                    .flatten();
                // Lucina's decision ({kind: "decision", decisionId, options}); its options only with previews on.
                let decision = n
                    .data
                    .as_ref()
                    .filter(|d| d["kind"] == "decision")
                    .and_then(|d| {
                        let id = d["decisionId"].as_str()?.to_owned();
                        let options = d["options"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_owned)).collect())
                            .filter(|_: &Vec<String>| settings.previews)
                            .unwrap_or_default();
                        Some((id, options))
                    });
                let payload = Payload {
                    id: Some(n.id.clone()),
                    category: n.category.as_str().to_owned(),
                    title,
                    body,
                    route: n.route.clone(),
                    tag: Some(push::tag_for(n)),
                    ts: n.ts,
                    unread,
                    digest: false,
                    url: approval.as_ref().map(|_| n.route.clone()),
                    sensitive: approval.as_ref().map(|(_, s, _, _)| *s),
                    // The exact commands, for the lock screen's expanded view; hidden with previews off.
                    detail: approval
                        .as_ref()
                        .filter(|_| settings.previews)
                        .and_then(|_| n.data.as_ref()?["details"].as_str())
                        .filter(|d| !d.trim().is_empty())
                        .map(str::to_owned),
                    detail_cut: approval.as_ref().is_some_and(|(_, _, _, cut)| *cut),
                    payload_hash: approval.as_ref().and_then(|(_, _, h, _)| h.clone()),
                    approval_id: approval.map(|(id, _, _, _)| id),
                    decision_id: decision.as_ref().map(|(id, _)| id.clone()),
                    decision_options: decision.map(|(_, o)| o).unwrap_or_default(),
                };
                (s, payload)
            })
            .collect()
    }

    /// A timeline item from the ingester; remembers its id as the resume point.
    /// `test_device`: the device is flagged `test` in the registry.
    pub async fn ingest(
        self: &Arc<Self>,
        item: &TimelineItem,
        sse_id: Option<String>,
        test_device: &(dyn Fn(&str) -> bool + Sync),
    ) {
        let now = model::now_ms();
        if let Some(n) = ingest::classify(item, now, ms(self.cfg.stale)) {
            let meta = item.meta.clone().unwrap_or(Value::Null);
            let channel = String::from(item.channel.clone());
            let turn = reply_route::Turn::of(&channel, item.reply_to.is_some(), &meta);
            let origin = turn.origin(test_device, |d| self.push_device(d), |d| self.present_on(d));
            let kind = route_kind(n.category.unwrap_or(Category::LucinaMessage));
            let id = item.id.to_text();
            match reply_route::route(kind, origin) {
                reply_route::Route::Silent => {
                    tracing::info!(timeline_id = %id, ?origin, ?kind, "no push: the device he asked on shows it, or test/agent traffic");
                }
                reply_route::Route::Now => {
                    if let Err(e) = self.publish(n).await {
                        tracing::warn!(error = %e, "could not store a notification");
                    }
                }
                reply_route::Route::Grace => {
                    let device = turn.device.clone().unwrap_or_default();
                    let written = ingest::ts_ms(&item.ts).unwrap_or(now).min(now) - CLOCK_SLACK_MS;
                    tracing::info!(timeline_id = %id, %device, "reply held for the asking client");
                    let me = self.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(me.cfg.reply_grace).await;
                        let connected = me.connected(&device);
                        let active = me.active_since(&device, written);
                        if reply_route::grace_push(connected, active) {
                            tracing::info!(timeline_id = %id, %device, "asking client gone: pushing the reply");
                            if let Err(e) = me.publish(n).await {
                                tracing::warn!(error = %e, "could not store a notification");
                            }
                        } else {
                            tracing::info!(timeline_id = %id, %device, connected, active, "asking client has the reply: no push");
                        }
                    });
                }
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

    /// Lucina's own calendar's upcoming timed events, kept like a phone's.
    pub fn own_events(&self, events: Vec<CalendarEvent>) -> io::Result<()> {
        self.state.update(|s| {
            if events.is_empty() {
                s.calendar.remove(OWN_CALENDAR_KEY);
            } else {
                s.calendar.insert(OWN_CALENDAR_KEY.to_owned(), events);
            }
        })
    }

    /// Periodic work: calendar notifications, the digest after quiet hours,
    /// the "Lucina unreachable" notice.
    pub async fn tick(self: &Arc<Self>, now: i64) {
        let settings = self.settings.get();

        // Calendar: `lead` minutes before start, or at the event's own alerts
        // (catching up for a few minutes, never after the start).
        if settings.categories.calendar {
            let lead = i64::from(settings.calendar_lead_minutes) * 60_000;
            let due: Vec<(CalendarEvent, Alert)> = self.state.read(|s: &NotifyState| {
                let mut v: Vec<(CalendarEvent, Alert)> = s
                    .calendar
                    .values()
                    .flatten()
                    .filter_map(|e| due_alert(e, lead, now).map(|a| (e.clone(), a)))
                    .collect();
                v.sort_by(|(a, x), (b, y)| (a.start, &a.id, x).cmp(&(b.start, &b.id, y)));
                v.dedup_by(|(a, x), (b, y)| a.id == b.id && a.start == b.start && x == y);
                v
            });
            for (e, alert) in due {
                let body = if e.title.is_empty() {
                    settings.local_hhmm(e.start)
                } else {
                    format!("{} · {}", e.title, settings.local_hhmm(e.start))
                };
                let mut n = NewNotification::new(Category::Calendar, body);
                n.dedup_key = Some(match alert {
                    Alert::Lead => format!("cal:{}:{}", e.id, e.start),
                    Alert::At(at) => format!("cal:{}:{}:{at}", e.id, e.start),
                    Alert::Leave(_) => format!("cal:{}:{}:leave", e.id, e.start),
                });
                if matches!(alert, Alert::Leave(_)) {
                    n.title =
                        Some(text::leave_title(text::lang(Some(&settings.language))).to_owned());
                }
                n.route = Some(calendar_route(&e));
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
                url: None,
                approval_id: None,
                sensitive: None,
                detail: None,
                detail_cut: false,
                payload_hash: None,
                decision_id: None,
                decision_options: Vec::new(),
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
                    url: None,
                    approval_id: None,
                    sensitive: None,
                    detail: None,
                    detail_cut: false,
                    payload_hash: None,
                    decision_id: None,
                    decision_options: Vec::new(),
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
        // A tap opens the Calendar tab on the phone's event.
        assert_eq!(
            items[0].route,
            format!("/calendar?event=iphone%3AE1%40{in5}&at={in5}")
        );
    }

    #[tokio::test]
    async fn lucina_calendar_events_notify_and_open_the_calendar() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        let now = model::now_ms();
        let in5 = now + 5 * 60_000;
        n.own_events(vec![CalendarEvent {
            id: "lucina:ev_abc".into(),
            title: "Call Anna".into(),
            start: in5,
            alerts: None,
            leave_at: None,
        }])
        .unwrap();
        n.tick(now).await;
        let items = n.inbox.page(None, 50, false).items;
        assert_eq!(items.len(), 1);
        assert!(items[0].body.starts_with("Call Anna · "));
        assert_eq!(
            items[0].route,
            format!("/calendar?event=lucina%3Aev_abc&at={in5}")
        );
        n.own_events(vec![]).unwrap();
        assert!(n.state.read(|s| !s.calendar.contains_key(OWN_CALENDAR_KEY)));
    }

    #[test]
    fn own_alerts_replace_the_lead() {
        let start = 10 * 3_600_000;
        let min = 60_000;
        let lead = 15 * min;
        let mut e = CalendarEvent {
            id: "lucina:ev_a".into(),
            title: "x".into(),
            start,
            alerts: None,
            leave_at: None,
        };
        assert_eq!(due_alert(&e, lead, start - 20 * min), None);
        assert_eq!(due_alert(&e, lead, start - 10 * min), Some(Alert::Lead));
        assert_eq!(due_alert(&e, lead, start + 2 * min), None);
        // Its own alerts: one hour and one day before; none at the lead.
        e.alerts = Some(vec![start - 60 * min, start - 1440 * min]);
        assert_eq!(due_alert(&e, lead, start - 10 * min), None);
        assert_eq!(
            due_alert(&e, lead, start - 58 * min),
            Some(Alert::At(start - 60 * min))
        );
        assert_eq!(due_alert(&e, lead, start - 50 * min), None);
        // "None" chosen: nothing at all.
        e.alerts = Some(vec![]);
        assert_eq!(due_alert(&e, lead, start - 10 * min), None);
        e.leave_at = Some(start - 25 * min);
        assert_eq!(
            due_alert(&e, lead, start - 24 * min),
            Some(Alert::Leave(start - 25 * min))
        );
    }

    #[tokio::test]
    async fn time_to_leave_has_its_own_title() {
        let dir = tempfile::tempdir().unwrap();
        let n = notifier(dir.path());
        let now = model::now_ms();
        let start = now + 30 * 60_000;
        n.own_events(vec![CalendarEvent {
            id: "lucina:ev_l".into(),
            title: "Kitchen".into(),
            start,
            alerts: Some(vec![]),
            leave_at: Some(now - 60_000),
        }])
        .unwrap();
        n.tick(now).await;
        let items = n.inbox.page(None, 50, false).items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title.as_deref(), Some("Time to leave"));
        n.tick(now + 60_000).await;
        assert_eq!(n.inbox.page(None, 50, false).items.len(), 1);
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
        assert_eq!(text::agent_title(Some("permission"), "it"), "Agent needs permission");
        assert_eq!(text::agent_title(None, "en"), "Agent finished");
        n.settings.update(|s| s.categories.agent = false).unwrap();
        assert_eq!(n.publish(NewNotification::new(Category::Agent, "done")).await.unwrap(), Outcome::Disabled);
    }

    // -- a reply goes where he asked (reply_route; docs/notifications.md) ------

    const IPHONE: &str = "dev_iphone";
    const DSI: &str = "dev_dsi";
    const JUNO: &str = "dev_juno";
    const PROBE: &str = "dev_probe";
    const GRACE: Duration = Duration::from_millis(150);

    fn routed(dir: &Path) -> Arc<Notifier> {
        let cfg = NotifyConfig {
            reply_grace: GRACE,
            ..NotifyConfig::default()
        };
        let n = Arc::new(Notifier::open(dir, cfg).unwrap());
        n.pusher
            .upsert(Subscription {
                id: "ps_iphone".into(),
                kind: push::Kind::Apns,
                device_id: IPHONE.into(),
                device_name: "iPhone".into(),
                label: "iPhone (app)".into(),
                endpoint: "00".repeat(32),
                p256dh: String::new(),
                auth: String::new(),
                lang: Some("en".into()),
                created_at: 0,
                last_success_at: None,
                failures: 0,
                last_error: None,
            })
            .unwrap();
        n
    }

    fn reply(id: &str, channel: &str, meta: Value) -> TimelineItem {
        let ts = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        serde_json::from_value(json!({"id": id, "ts": ts, "channel": channel, "role": "lucina",
            "text": "Here you go", "replyTo": "01USER", "meta": meta}))
        .unwrap()
    }

    async fn ingest(n: &Arc<Notifier>, item: &TimelineItem) {
        n.ingest(item, None, &|d: &str| d == PROBE).await;
    }

    fn inbox_has(n: &Notifier, id: &str) -> bool {
        n.inbox
            .page(None, 50, false)
            .items
            .iter()
            .any(|x| x.timeline_id.as_deref() == Some(id))
    }

    async fn after_grace() {
        tokio::time::sleep(GRACE * 3).await;
    }

    #[tokio::test]
    async fn dsi_turn_stays_on_the_dsi_while_it_is_connected() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        let _socket = n.link(DSI);
        ingest(&n, &reply("01DSI", "web", json!({"source": "fortuna-chat", "fromDevice": DSI}))).await;
        assert!(!inbox_has(&n, "01DSI"), "held, not pushed at once");
        after_grace().await;
        assert!(!inbox_has(&n, "01DSI"), "the DSi had it: the iPhone stays silent");
    }

    #[tokio::test]
    async fn juno_turn_stays_on_juno_when_it_polled_after_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        ingest(&n, &reply("01JUNO", "web", json!({"source": "juno-chat", "device": JUNO}))).await;
        n.saw(JUNO);
        after_grace().await;
        assert!(!inbox_has(&n, "01JUNO"));
    }

    #[tokio::test]
    async fn asking_client_gone_gets_the_push_after_the_grace() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        {
            // The DSi had a socket, then went away before the reply.
            let _gone = n.link(DSI);
        }
        ingest(&n, &reply("01GONE", "web", json!({"source": "fortuna-chat", "fromDevice": DSI}))).await;
        assert!(!inbox_has(&n, "01GONE"));
        after_grace().await;
        assert!(inbox_has(&n, "01GONE"), "the reply must not be lost");
    }

    #[tokio::test]
    async fn iphone_turn_in_front_is_silent_and_in_background_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        n.set_presence(IPHONE, true);
        ingest(&n, &reply("01FRONT", "ios", json!({"source": "ios-chat", "device": IPHONE}))).await;
        assert!(!inbox_has(&n, "01FRONT"));
        n.set_presence(IPHONE, false);
        ingest(&n, &reply("01AWAY", "ios", json!({"source": "ios-chat", "device": IPHONE}))).await;
        assert!(inbox_has(&n, "01AWAY"), "app in the background: pushed at once");
    }

    #[tokio::test]
    async fn probe_test_and_agent_traffic_never_notifies() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        ingest(&n, &reply("01P1", "web", json!({"source": "probe"}))).await;
        ingest(&n, &reply("01P2", "ios", json!({"device": PROBE, "deviceName": "Voice probe"}))).await;
        ingest(&n, &reply("01P3", "web", json!({"source": "test-dsi", "fromDevice": DSI}))).await;
        ingest(&n, &reply("01AG", "web", json!({"via": "mcp", "caller": "claude"}))).await;
        let mut r = reply("01PR", "web", json!({"kind": "reminder", "source": "probe"}));
        r.reply_to = None;
        ingest(&n, &r).await;
        after_grace().await;
        assert_eq!(n.inbox.page(None, 50, false).items.len(), 0);
    }

    #[tokio::test]
    async fn reminders_and_proactive_messages_still_push() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        let mut r = reply("01REM", "telegram", json!({"kind": "reminder", "isDm": true}));
        r.reply_to = None;
        ingest(&n, &r).await;
        assert!(inbox_has(&n, "01REM"));
        let mut p = reply("01PRO", "other", json!({"kind": "proactive"}));
        p.reply_to = None;
        ingest(&n, &p).await;
        assert!(inbox_has(&n, "01PRO"));
        // A reminder made in a DSi turn pushes at once too.
        let _socket = n.link(DSI);
        ingest(&n, &reply("01DREM", "web", json!({"kind": "reminder", "fromDevice": DSI}))).await;
        assert!(inbox_has(&n, "01DREM"));
        // His settings still apply.
        n.settings.update(|s| s.categories.reminder = false).unwrap();
        let mut off = reply("01OFF", "web", json!({"kind": "reminder"}));
        off.reply_to = None;
        ingest(&n, &off).await;
        assert!(!inbox_has(&n, "01OFF"));
    }

    #[tokio::test]
    async fn telegram_replies_stay_on_telegram() {
        let dir = tempfile::tempdir().unwrap();
        let n = routed(dir.path());
        ingest(&n, &reply("01TG", "telegram", json!(null))).await;
        after_grace().await;
        assert!(!inbox_has(&n, "01TG"));
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

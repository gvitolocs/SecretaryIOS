//! Signal sources. Each is a small [`Source`] impl that turns what it reads
//! into [`Event`]s with their score inputs; MVP 4 adds email, GitHub and
//! Pokoin servers the same way.
//!
//! Sources read through "feeds" (the data as of `now`), so the 24-hour
//! replay can run the same code against a day of recorded data:
//! [`ReminderFeed`], [`TimelineFeed`], [`PresenceFeed`], [`DeviceFeed`],
//! [`HealthProbe`], [`MemoryFeed`] and [`crate::secretary::SecretaryApi`].

pub mod calendar;
pub mod health;
pub mod notes;
pub mod reminders;
pub mod threads;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use lucina_store::Reminder;
use serde_json::Value;

use crate::attention::{Device, Presence};
use crate::model::Event;
use crate::store::Store;

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &'static str;
    /// Base interval and maximum jitter between observations.
    fn schedule(&self) -> (Duration, Duration);
    /// What is worth considering as of `now`. Dedup is the engine's job:
    /// returning the same event again is fine.
    async fn observe(&self, now: DateTime<Utc>, store: &dyn Store) -> Result<Vec<Event>>;
}

// ------------------------------------------------------------------ feeds

/// Lucina's reminders as of `now` (the Node store, read only).
pub trait ReminderFeed: Send + Sync {
    fn reminders(&self, now: DateTime<Utc>) -> Result<Vec<Reminder>>;
}

/// One timeline item, as much as the sources need.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub role: String,
    pub channel: String,
    pub text: String,
    pub meta: Value,
}

/// The owner timeline: items with `since <= ts <= now`, oldest first.
pub trait TimelineFeed: Send + Sync {
    fn items(&self, since: DateTime<Utc>, now: DateTime<Utc>) -> Result<Vec<Entry>>;
}

pub trait PresenceFeed: Send + Sync {
    fn at(&self, now: DateTime<Utc>) -> Presence;
}

pub trait DeviceFeed: Send + Sync {
    fn at(&self, now: DateTime<Utc>) -> Device;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub ok: bool,
    pub status: u16,
    pub ms: u64,
    pub error: Option<String>,
}

#[async_trait::async_trait]
pub trait HealthProbe: Send + Sync {
    async fn probe(&self, url: &str, timeout: Duration, slow_ms: u64) -> Probe;
}

/// Honcho memory search (owner's messages), read only.
#[async_trait::async_trait]
pub trait MemoryFeed: Send + Sync {
    async fn search(&self, query: &str, limit: u32) -> Vec<String>;
}

// ------------------------------------------------------------------ file feeds

/// `reminders.json` (status as stored).
pub struct FileReminders {
    store: lucina_store::ReminderStore,
}

impl FileReminders {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            store: lucina_store::ReminderStore::new(path, 10_000),
        }
    }
}

impl ReminderFeed for FileReminders {
    fn reminders(&self, _now: DateTime<Utc>) -> Result<Vec<Reminder>> {
        Ok(self.store.list(true)?)
    }
}

/// `timeline.jsonl` and its rotated `.1` (read only, parsed per call; the
/// file is a few hundred KB).
pub struct FileTimeline {
    path: PathBuf,
}

impl FileTimeline {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

pub fn parse_entry(line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line).ok()?;
    let ts = DateTime::parse_from_rfc3339(v.get("ts")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    Some(Entry {
        id: v.get("id")?.as_str()?.to_owned(),
        ts,
        role: v.get("role")?.as_str()?.to_owned(),
        channel: v
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("other")
            .to_owned(),
        text: v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        meta: v.get("meta").cloned().unwrap_or(Value::Null),
    })
}

impl TimelineFeed for FileTimeline {
    fn items(&self, since: DateTime<Utc>, now: DateTime<Utc>) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let rotated = PathBuf::from(format!("{}.1", self.path.display()));
        for p in [rotated.as_path(), self.path.as_path()] {
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            out.extend(
                text.lines()
                    .filter_map(parse_entry)
                    .filter(|e| e.ts >= since && e.ts <= now),
            );
        }
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }
}

/// The presence watcher's files (`<data>/presence/discord.json{,l}`).
/// Live: the current state (when fresh). Replay: the history up to `now`.
pub struct FilePresence {
    dir: PathBuf,
    /// Use the history (replay) instead of the current file.
    pub history: bool,
    /// The current file is trusted this long after `updatedAt`.
    pub max_age: chrono::Duration,
}

impl FilePresence {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            history: false,
            max_age: chrono::Duration::minutes(15),
        }
    }
}

fn playing(activities: Option<&Value>) -> Option<String> {
    activities?.as_array()?.iter().find_map(|a| {
        (a.get("type").and_then(Value::as_str) == Some("playing")).then(|| {
            a.get("name")
                .and_then(Value::as_str)
                .unwrap_or("a game")
                .to_owned()
        })
    })
}

fn ts(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.get(key)?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl FilePresence {
    fn history_at(&self, now: DateTime<Utc>) -> Presence {
        let text = std::fs::read_to_string(self.dir.join("discord.jsonl")).unwrap_or_default();
        let mut p = Presence::default();
        let mut games: Vec<String> = Vec::new();
        for v in text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            let Some(at) = ts(&v, "at") else { continue };
            if at > now {
                break;
            }
            match v.get("event").and_then(Value::as_str) {
                Some("status") => {
                    let to = v.get("to").and_then(Value::as_str).unwrap_or("").to_owned();
                    let was_online = matches!(p.status.as_str(), "online" | "idle" | "dnd");
                    let now_online = matches!(to.as_str(), "online" | "idle" | "dnd");
                    if now_online && !was_online {
                        p.online_since = Some(at);
                    }
                    if !now_online {
                        p.online_since = None;
                        games.clear();
                    }
                    p.status = to;
                }
                Some("activity_start") | Some("activity_stop") => {
                    let a = v.get("activity").cloned().unwrap_or(Value::Null);
                    if a.get("type").and_then(Value::as_str) != Some("playing") {
                        continue;
                    }
                    let name = a
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("a game")
                        .to_owned();
                    if v.get("event").and_then(Value::as_str) == Some("activity_start") {
                        games.push(name);
                    } else {
                        games.retain(|g| *g != name);
                    }
                }
                Some("watcher") if v.get("state").and_then(Value::as_str) == Some("stop") => {
                    p = Presence::default();
                    games.clear();
                }
                _ => {}
            }
        }
        p.gaming = games.last().cloned();
        p
    }
}

impl PresenceFeed for FilePresence {
    fn at(&self, now: DateTime<Utc>) -> Presence {
        if self.history {
            return self.history_at(now);
        }
        let Some(v) = std::fs::read_to_string(self.dir.join("discord.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return Presence::default();
        };
        // A stale file (watcher down) says nothing.
        if ts(&v, "updatedAt").is_none_or(|u| now - u > self.max_age) {
            return Presence::default();
        }
        let status = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let online = matches!(status.as_str(), "online" | "idle" | "dnd");
        Presence {
            gaming: playing(v.get("activities")),
            online_since: if online { ts(&v, "since") } else { None },
            status,
        }
    }
}

/// `device-context.jsonl` (written by the Node Lucina when the iPhone
/// reports; may not exist): the last line at or before `now` that is at
/// most 2 hours old, its `focus`.
pub struct FileDevice {
    path: PathBuf,
}

impl FileDevice {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl DeviceFeed for FileDevice {
    fn at(&self, now: DateTime<Utc>) -> Device {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Device::default();
        };
        let mut last: Option<Value> = None;
        for v in text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            let at = ts(&v, "ts")
                .or_else(|| ts(&v, "at"))
                .or_else(|| ts(&v, "receivedAt"));
            if at.is_some_and(|t| t <= now && now - t <= chrono::Duration::hours(2)) {
                last = Some(v);
            }
        }
        Device {
            focus: last.and_then(|v| {
                v.get("focus")
                    .or_else(|| v.pointer("/state/focus"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        }
    }
}

/// `GET` the URL: ok on 2xx/3xx answered within `slow_ms`.
pub struct HttpProbe {
    http: reqwest::Client,
}

impl HttpProbe {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!(
                    "lucina-proactive/",
                    env!("CARGO_PKG_VERSION"),
                    " (health)"
                ))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()?,
        })
    }
}

#[async_trait::async_trait]
impl HealthProbe for HttpProbe {
    async fn probe(&self, url: &str, timeout: Duration, slow_ms: u64) -> Probe {
        let start = Instant::now();
        let r = self.http.get(url).timeout(timeout).send().await;
        let ms = start.elapsed().as_millis() as u64;
        match r {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let good = resp.status().is_success() || resp.status().is_redirection();
                let slow = ms > slow_ms;
                Probe {
                    ok: good && !slow,
                    status,
                    ms,
                    error: if !good {
                        Some(format!("HTTP {status}"))
                    } else if slow {
                        Some(format!("slow: {ms} ms"))
                    } else {
                        None
                    },
                }
            }
            Err(e) => Probe {
                ok: false,
                status: 0,
                ms,
                error: Some(if e.is_timeout() {
                    "timeout".into()
                } else if e.is_connect() {
                    "connection failed".into()
                } else {
                    "request failed".into()
                }),
            },
        }
    }
}

/// Honcho through `lucina-memory` (owner peer search).
pub struct HonchoMemory {
    pub memory: lucina_memory::Memory,
}

impl HonchoMemory {
    /// The proactive engine's memory surface: background work whose output
    /// goes to the owner alone (his app timeline, his Telegram chat), so it
    /// may read his personal memory and never writes it.
    pub fn surface() -> lucina_memory::TurnSurface {
        lucina_memory::TurnSurface::classify(
            &lucina_memory::TurnFacts::background(
                lucina_memory::surface::BackgroundOrigin::Proactive,
                true,
            ),
            &lucina_memory::surface::OwnerIds::default(),
        )
    }
}

#[async_trait::async_trait]
impl MemoryFeed for HonchoMemory {
    async fn search(&self, query: &str, limit: u32) -> Vec<String> {
        match self.memory.search(&Self::surface(), query, limit).await {
            Ok(v) => v
                .into_iter()
                .map(|m| m.content)
                .filter(|c| !c.trim().is_empty())
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "memory search failed");
                Vec::new()
            }
        }
    }
}

/// The Node Lucina's data files under `dir`.
pub fn node_paths(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        dir.join("reminders.json"),
        dir.join("timeline.jsonl"),
        dir.join("presence"),
        dir.join("device-context.jsonl"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn presence_history_and_current() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("discord.jsonl"),
            [
                r#"{"event":"watcher","at":"2026-10-22T08:00:00Z","state":"start"}"#,
                r#"{"event":"status","at":"2026-10-22T09:00:00Z","from":"offline","to":"online"}"#,
                r#"{"event":"activity_start","at":"2026-10-22T19:00:00Z","activity":{"type":"playing","name":"League of Legends"}}"#,
                r#"{"event":"activity_stop","at":"2026-10-22T20:30:00Z","activity":{"type":"playing","name":"League of Legends"}}"#,
                r#"{"event":"status","at":"2026-10-22T21:00:00Z","from":"online","to":"offline"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let mut f = FilePresence::new(dir.path());
        f.history = true;
        assert_eq!(f.at(at("2026-10-22T08:30:00Z")).status, "");
        let p = f.at(at("2026-10-22T19:30:00Z"));
        assert_eq!(p.status, "online");
        assert_eq!(p.gaming.as_deref(), Some("League of Legends"));
        assert_eq!(p.online_since, Some(at("2026-10-22T09:00:00Z")));
        assert_eq!(f.at(at("2026-10-22T20:45:00Z")).gaming, None);
        assert_eq!(f.at(at("2026-10-22T22:00:00Z")).status, "offline");

        std::fs::write(
            dir.path().join("discord.json"),
            r#"{"status":"dnd","activities":[{"type":"playing","name":"Hades"}],"since":"2026-10-22T09:00:00Z","updatedAt":"2026-10-22T10:00:00Z"}"#,
        )
        .unwrap();
        let live = FilePresence::new(dir.path());
        let p = live.at(at("2026-10-22T10:05:00Z"));
        assert_eq!(
            (p.status.as_str(), p.gaming.as_deref()),
            ("dnd", Some("Hades"))
        );
        // Stale: unknown.
        assert_eq!(live.at(at("2026-10-22T12:00:00Z")), Presence::default());
    }

    #[test]
    fn timeline_and_device_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("timeline.jsonl");
        std::fs::write(
            &p,
            "{\"id\":\"01K0000000000000000000000A\",\"ts\":\"2026-10-22T08:00:00.000Z\",\"channel\":\"telegram\",\"role\":\"user\",\"text\":\"ciao\"}\nnot json\n{\"id\":\"01K0000000000000000000000B\",\"ts\":\"2026-10-22T09:00:00.000Z\",\"channel\":\"web\",\"role\":\"lucina\",\"text\":\"ehi\",\"meta\":{\"kind\":\"note\"}}\n",
        )
        .unwrap();
        let t = FileTimeline::new(&p);
        let all = t
            .items(at("2026-10-22T00:00:00Z"), at("2026-10-22T23:00:00Z"))
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].meta["kind"], "note");
        assert_eq!(
            t.items(at("2026-10-22T00:00:00Z"), at("2026-10-22T08:30:00Z"))
                .unwrap()
                .len(),
            1
        );

        let d = dir.path().join("device-context.jsonl");
        std::fs::write(
            &d,
            "{\"ts\":\"2026-10-22T08:00:00Z\",\"focus\":\"sleep\"}\n",
        )
        .unwrap();
        let f = FileDevice::new(&d);
        assert_eq!(
            f.at(at("2026-10-22T08:30:00Z")).focus.as_deref(),
            Some("sleep")
        );
        assert_eq!(f.at(at("2026-10-22T11:00:00Z")).focus, None);
        assert_eq!(
            FileDevice::new(dir.path().join("missing")).at(at("2026-10-22T08:30:00Z")),
            Device::default()
        );
    }
}

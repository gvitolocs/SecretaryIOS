//! Presence files under `<data>/presence/`:
//!
//! - `discord.json`: the current [`PresenceState`] (atomic write, 0600).
//! - `discord.jsonl`: transitions only, one JSON object per line:
//!   `{"at","event":"status","from","to","clientStatus"}`,
//!   `{"at","event":"activity_start"|"activity_stop","activity"}`,
//!   `{"at","event":"watcher","state":"start"|"stop"}`.
//!   Lines older than the retention (`LUCINA_PRESENCE_RETENTION_DAYS`, 90)
//!   are pruned; past [`MAX_HISTORY_BYTES`] the oldest half is dropped too.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::presence::{parse_iso, Activity, ClientStatus, PresenceState};

pub const CURRENT_FILE: &str = "discord.json";
pub const HISTORY_FILE: &str = "discord.jsonl";
pub const DEFAULT_RETENTION_DAYS: i64 = 90;
pub const MAX_HISTORY_BYTES: u64 = 8 * 1024 * 1024;
/// Readers treat an older `discord.json` as stale (watcher down).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(600);

/// One history line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HistoryEvent {
    Status {
        at: String,
        from: Option<String>,
        to: String,
        #[serde(rename = "clientStatus")]
        client_status: ClientStatus,
    },
    ActivityStart {
        at: String,
        activity: Activity,
    },
    ActivityStop {
        at: String,
        activity: Activity,
    },
    Watcher {
        at: String,
        state: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        inferred: bool,
    },
}

impl HistoryEvent {
    pub fn at(&self) -> &str {
        match self {
            HistoryEvent::Status { at, .. }
            | HistoryEvent::ActivityStart { at, .. }
            | HistoryEvent::ActivityStop { at, .. }
            | HistoryEvent::Watcher { at, .. } => at,
        }
    }
}

/// The presence directory and its writer side.
#[derive(Debug, Clone)]
pub struct PresenceStore {
    pub dir: PathBuf,
    pub retention_days: i64,
    pub max_history_bytes: u64,
    last_prune: Option<DateTime<Utc>>,
}

impl PresenceStore {
    pub fn new(dir: impl Into<PathBuf>, retention_days: i64) -> Self {
        Self {
            dir: dir.into(),
            retention_days: retention_days.max(1),
            max_history_bytes: MAX_HISTORY_BYTES,
            last_prune: None,
        }
    }

    /// `<data_dir>/presence`.
    pub fn for_data_dir(data_dir: &Path, retention_days: i64) -> Self {
        Self::new(data_dir.join("presence"), retention_days)
    }

    pub fn current_path(&self) -> PathBuf {
        self.dir.join(CURRENT_FILE)
    }

    pub fn history_path(&self) -> PathBuf {
        self.dir.join(HISTORY_FILE)
    }

    pub fn read_current(&self) -> Option<PresenceState> {
        read_current(&self.dir)
    }

    pub fn write_current(&self, state: &PresenceState) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        lucina_store::atomic::write_atomic(&self.current_path(), &bytes)
    }

    /// Append transitions; prunes at most once an hour.
    pub fn append(&mut self, events: &[HistoryEvent], now: DateTime<Utc>) -> std::io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::new();
        for e in events {
            serde_json::to_writer(&mut buf, e).map_err(std::io::Error::other)?;
            buf.push(b'\n');
        }
        let mut f = lucina_store::atomic::open_append(&self.history_path())?;
        f.write_all(&buf)?;
        f.flush()?;
        drop(f);
        self.maybe_prune(now)
    }

    pub fn maybe_prune(&mut self, now: DateTime<Utc>) -> std::io::Result<()> {
        if self
            .last_prune
            .is_some_and(|t| now - t < chrono::Duration::hours(1))
        {
            return Ok(());
        }
        self.last_prune = Some(now);
        self.prune(now).map(|_| ())
    }

    /// Drop lines older than the retention (and unparsable ones); past the
    /// size cap keep the newest half. Returns the number of lines dropped.
    pub fn prune(&self, now: DateTime<Utc>) -> std::io::Result<usize> {
        let path = self.history_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let cutoff = now - chrono::Duration::days(self.retention_days);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut kept: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                serde_json::from_str::<HistoryEvent>(l)
                    .ok()
                    .and_then(|e| parse_iso(e.at()))
                    .is_some_and(|t| t >= cutoff)
            })
            .collect();
        let size: usize = kept.iter().map(|l| l.len() + 1).sum();
        if size as u64 > self.max_history_bytes {
            let drop = kept.len() / 2;
            kept.drain(..drop);
        }
        let dropped = lines.len() - kept.len();
        if dropped > 0 {
            let mut out = kept.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            lucina_store::atomic::write_atomic(&path, out.as_bytes())?;
        }
        Ok(dropped)
    }

    pub fn last_event(&self) -> Option<HistoryEvent> {
        read_history(&self.dir).pop()
    }
}

pub fn read_current(dir: &Path) -> Option<PresenceState> {
    let bytes = std::fs::read(dir.join(CURRENT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Every parsable history line, oldest first.
pub fn read_history(dir: &Path) -> Vec<HistoryEvent> {
    std::fs::read_to_string(dir.join(HISTORY_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn is_online(status: &str) -> bool {
    matches!(status, "online" | "idle" | "dnd")
}

/// Local midnight of `now`'s day in `tz`, as UTC.
pub fn day_start(now: DateTime<Utc>, tz: Tz) -> DateTime<Utc> {
    let local = now.with_timezone(&tz);
    tz.with_ymd_and_hms(local.year(), local.month(), local.day(), 0, 0, 0)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now - chrono::Duration::hours(24))
}

/// Seconds online (online, idle, dnd) per status in `[from, to]`, from the
/// history. A status line opens an interval; the next status line or any
/// watcher line closes it; the open interval ends at `open_end`.
pub fn online_seconds(
    events: &[HistoryEvent],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    open_end: DateTime<Utc>,
) -> Vec<(String, i64)> {
    let mut totals: Vec<(String, i64)> = Vec::new();
    let mut add = |status: &str, a: DateTime<Utc>, b: DateTime<Utc>| {
        let (s, e) = (a.max(from), b.min(to));
        if e > s && is_online(status) {
            let secs = (e - s).num_seconds();
            match totals.iter_mut().find(|(k, _)| k == status) {
                Some((_, v)) => *v += secs,
                None => totals.push((status.to_owned(), secs)),
            }
        }
    };
    let mut cur: Option<(String, DateTime<Utc>)> = None;
    for e in events {
        let Some(at) = parse_iso(e.at()) else {
            continue;
        };
        match e {
            HistoryEvent::Status { to: status, .. } => {
                if let Some((s, since)) = cur.take() {
                    add(&s, since, at);
                }
                cur = Some((status.clone(), at));
            }
            HistoryEvent::Watcher { .. } => {
                if let Some((s, since)) = cur.take() {
                    add(&s, since, at);
                }
            }
            _ => {}
        }
    }
    if let Some((s, since)) = cur {
        add(&s, since, open_end);
    }
    totals
}

/// What the server and the tool report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceReport {
    /// A watcher has written `discord.json` at least once.
    pub available: bool,
    /// `discord.json` is newer than the max age (watcher alive).
    pub fresh: bool,
    pub age_seconds: Option<i64>,
    #[serde(flatten)]
    pub state: Option<PresenceState>,
    pub today_online_minutes: i64,
    pub today_by_status: serde_json::Map<String, Value>,
    pub summary: String,
}

impl PresenceReport {
    pub fn build(dir: &Path, tz: Tz, now: DateTime<Utc>, max_age: Duration) -> Self {
        let state = read_current(dir);
        let updated = state.as_ref().and_then(|s| parse_iso(&s.updated_at));
        let age = updated.map(|u| (now - u).num_seconds().max(0));
        let fresh = age.is_some_and(|a| a as u64 <= max_age.as_secs());
        let open_end = if fresh {
            now
        } else {
            updated.unwrap_or(now).min(now)
        };
        let events = read_history(dir);
        let totals = online_seconds(&events, day_start(now, tz), now, open_end);
        let total: i64 = totals.iter().map(|(_, s)| s).sum();
        let mut by = serde_json::Map::new();
        for (k, s) in &totals {
            by.insert(k.clone(), Value::from(s / 60));
        }
        let summary = match &state {
            Some(s) if fresh => summary_line(s, tz, now),
            Some(_) => format!(
                "Discord: presence unknown (watcher data is {} min old)",
                age.unwrap_or(0) / 60
            ),
            None => "Discord: presence unknown (watcher not running)".into(),
        };
        Self {
            available: state.is_some(),
            fresh,
            age_seconds: age,
            state,
            today_online_minutes: total / 60,
            today_by_status: by,
            summary,
        }
    }
}

fn hhmm(s: &str, tz: Tz, now: DateTime<Utc>) -> Option<String> {
    let t = parse_iso(s)?.with_timezone(&tz);
    let n = now.with_timezone(&tz);
    Some(if t.date_naive() == n.date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%a %d %b %H:%M").to_string()
    })
}

fn status_words(s: &str) -> &str {
    match s {
        "dnd" => "do not disturb",
        other => other,
    }
}

/// One activity as a phrase: `playing Elden Ring since 21:10`.
pub fn activity_phrase(a: &Activity, tz: Tz, now: DateTime<Utc>) -> String {
    let since = a
        .started_at
        .as_deref()
        .and_then(|s| hhmm(s, tz, now))
        .map(|t| format!(" since {t}"))
        .unwrap_or_default();
    match a.kind.as_str() {
        "custom" => format!(
            "custom status \"{}\"",
            a.state.as_deref().unwrap_or_default()
        ),
        "listening" if a.name == "Spotify" => {
            let song = a.details.as_deref().unwrap_or("something");
            match a.state.as_deref() {
                Some(artist) => format!("listening to \"{song}\" by {artist} on Spotify"),
                None => format!("listening to \"{song}\" on Spotify"),
            }
        }
        "listening" => format!("listening to {}{since}", a.name),
        "watching" => format!("watching {}{since}", a.name),
        "streaming" => format!("streaming {}{since}", a.name),
        "competing" => format!("competing in {}{since}", a.name),
        "playing" => {
            let detail = a
                .details
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            format!("playing {}{detail}{since}", a.name)
        }
        _ => format!("{}{since}", a.name),
    }
}

/// `Discord: online on desktop since 19:02; playing Elden Ring since 21:10`.
pub fn summary_line(s: &PresenceState, tz: Tz, now: DateTime<Utc>) -> String {
    let since = hhmm(&s.since, tz, now)
        .map(|t| format!(" since {t}"))
        .unwrap_or_default();
    if !is_online(&s.status) {
        return format!("Discord: offline{since}");
    }
    let platforms = s.client_status.platforms();
    let on = if platforms.is_empty() {
        String::new()
    } else {
        format!(" on {}", platforms.join(" and "))
    };
    let mut parts = vec![format!("Discord: {}{on}{since}", status_words(&s.status))];
    parts.extend(s.activities.iter().map(|a| activity_phrase(a, tz, now)));
    parts.join("; ")
}

/// The context line for a chat turn: only when the watcher is alive.
pub fn context_line(dir: &Path, tz: Tz, now: DateTime<Utc>, max_age: Duration) -> Option<String> {
    let s = read_current(dir)?;
    let age = (now - parse_iso(&s.updated_at)?).num_seconds();
    (age >= 0 && age as u64 <= max_age.as_secs()).then(|| summary_line(&s, tz, now))
}

/// Today's history lines (for the tool), newest last, at most `max`.
pub fn today_events(dir: &Path, tz: Tz, now: DateTime<Utc>, max: usize) -> Vec<HistoryEvent> {
    let start = day_start(now, tz);
    let mut v: Vec<HistoryEvent> = read_history(dir)
        .into_iter()
        .filter(|e| parse_iso(e.at()).is_some_and(|t| t >= start))
        .collect();
    let skip = v.len().saturating_sub(max);
    v.drain(..skip);
    v
}

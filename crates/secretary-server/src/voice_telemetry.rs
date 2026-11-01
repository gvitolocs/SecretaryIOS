//! LucinaVoice device telemetry (protocol sections 4 and 5): what the speaker
//! sends over the voice socket, the server's own per-turn spans, their
//! storage, and the read API.
//!
//! **Ingest** ([`Store::ingest`], from [`crate::voice_ws`]). The device's text
//! frames `hello` (specs, boot reason) and `telemetry` (a batch of events
//! `boot|wifi|link|turn|stats|error`, each `{"t": <device uptime ms>, "k": ...}`)
//! are *untrusted data*. A frame over [`MAX_FRAME`] (16 KiB), or not JSON, is
//! ignored; a batch keeps its first [`MAX_EVENTS`] (32) events; an event over
//! [`MAX_EVENT_BYTES`] (1 KiB), of an unknown kind or without an object shape is
//! dropped; of the rest only the keys known for the kind are kept, numbers are
//! clamped to a sane range, strings are cut and stripped of control characters
//! and `<` `>`. Nothing here can fail a voice turn: errors are logged and
//! dropped. A device time `t` becomes wall time as
//! `received_at - (now - t)` (the difference clamped to 0..7 days).
//!
//! **Server spans.** [`Store::record_turn`] is called for every turn of the
//! socket and of `POST /v1/voice` with the cumulative spans (ms since the end
//! of speech, as in `done.ms`), the transcript *length* (never the text), the
//! speech length and the audio bytes of the turn. The device later reports its
//! own `turn` event; the two are matched by time (within 30 s, nearest first,
//! in either order) into one record with `device_first_audio_ms`. An
//! unmatched device turn stands alone. When Lucina sent `route` / `ack`
//! (protocol section 6), the spans gain `route` and `ack` and the record gains
//! `intent`, `confidence`, `jev_ms`, `fast` and `source` (never text).
//!
//! **Storage**, under `SECRETARY_DATA_DIR/voice/<device id>/` (0700/0600):
//! - `events.jsonl`: one JSON object per line, `{"at", "k", "t"?, ...}`:
//!   the device's events and the server's own `server_turn` records. Appended
//!   at once; at [`ROTATE_AT`] (2 MiB) it moves to `events.jsonl.1`, replacing
//!   the older one, so two files are kept.
//! - `status.json`: specs, the last stats, `last_seen`, byte and energy
//!   counters, the last 20 turns. Written atomically (temp file + rename) at
//!   most every [`FLUSH_EVERY`] seconds per device, on disconnect and by the
//!   flusher task ([`spawn_flusher`]). Memory per device is bounded: one status
//!   and no events (they are read from the files when asked).
//!
//! **Counters.** Bytes and energy come from the device's `stats` events,
//! which carry totals since boot: the server adds the difference to the previous
//! sample (a smaller value means a reboot, so the new value counts in full; the
//! very first sample counts in full only when the device booted in the last
//! 10 minutes). "Today" is the date in `SECRETARY_TIMEZONE`; the today counters
//! reset when it changes.
//!
//! **API** (behind device auth, [`routes`]):
//! - `GET /v1/voice/devices`
//! - `GET /v1/voice/devices/{id}/status`
//! - `GET /v1/voice/devices/{id}/events?since=&before=&kind=&limit=`
//!
//! *Who sees what.* Only the owner's browser session ([`Via::Cookie`]) sees
//! every device. Every device token (a speaker, the phone app, a speaker
//! paired but not connected yet) sees only its own id (`403 owner_only` for
//! any other id, even an unknown one). An id with no voice data is `404
//! device_not_found`.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::auth::{Identity, Via};
use crate::error::{ApiError, QueryArgs};
use crate::voice_pipe::{RouteInfo, Shared};
use crate::AppState;

/// Largest `hello` / `telemetry` frame taken in.
pub const MAX_FRAME: usize = 16 * 1024;
/// Events taken from one batch.
pub const MAX_EVENTS: usize = 32;
/// Largest event, serialized.
pub const MAX_EVENT_BYTES: usize = 1024;
/// `events.jsonl` moves aside at this size.
pub const ROTATE_AT: u64 = 2 * 1024 * 1024;
/// `status.json` is written at most this often.
pub const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// Turns kept in `status.json`.
pub const KEEP_TURNS: usize = 20;
/// A server turn and a device turn this close in time are the same turn.
pub const MATCH_WINDOW_MS: i64 = 30_000;
/// Devices held in memory.
const MAX_DEVICES: usize = 64;
/// A first stats sample counts in full when the device booted this recently.
const FRESH_BOOT_MS: f64 = 600_000.0;
const MAX_SKEW_MS: i64 = 7 * 24 * 3600 * 1000;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

// -- cleaning untrusted data ----------------------------------------------------

#[derive(Clone, Copy)]
enum Ty {
    /// A finite number, clamped to the range.
    N(f64, f64),
    /// A string of at most this many characters.
    S(usize),
    B,
    O(&'static [(&'static str, Ty)]),
}

type Spec = &'static [(&'static str, Ty)];

const SPANS: Spec = &[
    ("stt", Ty::N(0.0, 1e7)),
    ("lucina_first", Ty::N(0.0, 1e7)),
    ("tts_first", Ty::N(0.0, 1e7)),
    ("first_audio", Ty::N(0.0, 1e7)),
    ("total", Ty::N(0.0, 1e7)),
    ("route", Ty::N(0.0, 1e7)),
    ("ack", Ty::N(0.0, 1e7)),
];
const POWER: Spec = &[
    ("est_mw", Ty::N(0.0, 1e6)),
    ("rail_mv", Ty::N(0.0, 1e5)),
    ("rail_ma", Ty::N(0.0, 1e5)),
    ("energy_mwh", Ty::N(0.0, 1e9)),
];
const BOOT: Spec = &[("reset", Ty::S(32)), ("seq", Ty::N(0.0, 1e9))];
const WIFI: Spec = &[
    ("up", Ty::B),
    ("rssi", Ty::N(-130.0, 0.0)),
    ("reason", Ty::S(64)),
];
const LINK: Spec = &[("up", Ty::B), ("ms", Ty::N(0.0, 1e7)), ("err", Ty::S(64))];
const TURN: Spec = &[
    ("ok", Ty::B),
    ("err", Ty::S(64)),
    ("mode", Ty::S(8)),
    ("speech_ms", Ty::N(0.0, 1e7)),
    ("first_audio_ms", Ty::N(0.0, 1e7)),
    ("total_ms", Ty::N(0.0, 1e7)),
    ("up_bytes", Ty::N(0.0, 1e10)),
    ("down_bytes", Ty::N(0.0, 1e10)),
    ("server_ms", Ty::O(SPANS)),
];
const STATS: Spec = &[
    ("uptime_ms", Ty::N(0.0, 1e13)),
    ("heap_free", Ty::N(0.0, 1e9)),
    ("heap_min_free", Ty::N(0.0, 1e9)),
    ("rssi", Ty::N(-130.0, 0.0)),
    ("up_bytes", Ty::N(0.0, 1e13)),
    ("down_bytes", Ty::N(0.0, 1e13)),
    ("tx_air_ms", Ty::N(0.0, 1e13)),
    ("cpu_mhz", Ty::N(0.0, 1000.0)),
    ("temp_c", Ty::N(-100.0, 200.0)),
    ("power", Ty::O(POWER)),
];
const ERROR: Spec = &[("what", Ty::S(64)), ("detail", Ty::S(160))];
const SPECS: Spec = &[
    ("chip", Ty::S(16)),
    ("rev", Ty::S(16)),
    ("cores", Ty::N(0.0, 64.0)),
    ("cpu_mhz", Ty::N(0.0, 1000.0)),
    ("flash_bytes", Ty::N(0.0, 1e10)),
    ("psram_bytes", Ty::N(0.0, 1e10)),
    ("fw", Ty::S(32)),
    ("commit", Ty::S(16)),
    ("mac", Ty::S(8)),
    ("heap_total", Ty::N(0.0, 1e9)),
    ("ina", Ty::S(16)),
    ("temp_sensor", Ty::B),
];

/// The kinds a device may send, and their keys.
const KINDS: &[(&str, Spec)] = &[
    ("boot", BOOT),
    ("wifi", WIFI),
    ("link", LINK),
    ("turn", TURN),
    ("stats", STATS),
    ("error", ERROR),
];

fn clean_str(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '<' && *c != '>')
        .take(max)
        .collect()
}

fn number(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        json!(f as i64)
    } else {
        json!((f * 100.0).round() / 100.0)
    }
}

/// Only the keys of `spec`, each of the right type and size.
fn clean(src: &Value, spec: Spec) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = src.as_object() else {
        return out;
    };
    for (key, ty) in spec {
        let Some(v) = obj.get(*key) else { continue };
        let cleaned = match ty {
            Ty::N(lo, hi) => v
                .as_f64()
                .filter(|f| f.is_finite())
                .map(|f| number(f.clamp(*lo, *hi))),
            Ty::S(max) => v.as_str().map(|s| json!(clean_str(s, *max))),
            Ty::B => v.as_bool().map(Value::Bool),
            Ty::O(sub) => {
                let m = clean(v, sub);
                (!m.is_empty()).then_some(Value::Object(m))
            }
        };
        if let Some(c) = cleaned {
            out.insert((*key).to_owned(), c);
        }
    }
    out
}

/// One clean device event: when it happened (wall time, ms), its kind, its fields.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceEvent {
    pub at_ms: i64,
    pub kind: &'static str,
    pub body: Map<String, Value>,
}

/// The clean events of a `telemetry` frame.
pub fn parse_events(frame: &Value, received_ms: i64) -> Vec<DeviceEvent> {
    let now = frame["now"].as_f64().filter(|n| n.is_finite());
    let Some(events) = frame["events"].as_array() else {
        return vec![];
    };
    let mut out = vec![];
    for ev in events.iter().take(MAX_EVENTS) {
        if ev.to_string().len() > MAX_EVENT_BYTES {
            continue;
        }
        let Some((kind, spec)) = ev["k"]
            .as_str()
            .and_then(|k| KINDS.iter().find(|(name, _)| *name == k))
        else {
            continue;
        };
        let mut body = clean(ev, spec);
        if *kind == "turn"
            && !matches!(
                body.get("mode").and_then(Value::as_str),
                Some("link" | "http")
            )
        {
            body.remove("mode");
        }
        if body.is_empty() {
            continue;
        }
        let t = ev["t"].as_f64().filter(|t| t.is_finite());
        let skew = match (now, t) {
            (Some(n), Some(t)) => ((n - t) as i64).clamp(0, MAX_SKEW_MS),
            _ => 0,
        };
        if let Some(t) = t {
            body.insert("t".into(), number(t.clamp(0.0, 1e13)));
        }
        out.push(DeviceEvent {
            at_ms: received_ms - skew,
            kind,
            body,
        });
    }
    out
}

// -- stored shapes ----------------------------------------------------------------

/// Spans in ms since the end of speech (cumulative, not durations).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spans {
    pub stt: Option<u64>,
    pub lucina_first: Option<u64>,
    pub tts_first: Option<u64>,
    pub first_audio: Option<u64>,
    pub total: Option<u64>,
    /// The `route` event from Lucina (voice fast path); absent without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<u64>,
    /// The filler audio went out; absent when none was played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<u64>,
}

impl Spans {
    fn from_map(m: &Value) -> Self {
        let g = |k: &str| m[k].as_f64().map(|f| f.max(0.0) as u64);
        Self {
            stt: g("stt"),
            lucina_first: g("lucina_first"),
            tts_first: g("tts_first"),
            first_audio: g("first_audio"),
            total: g("total"),
            route: g("route"),
            ack: g("ack"),
        }
    }
}

/// What the server knows of one turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub at_ms: i64,
    pub ok: bool,
    pub err: Option<String>,
    pub transcript_chars: u64,
    pub speech_ms: u64,
    pub spans: Spans,
    pub up_bytes: u64,
    pub down_bytes: u64,
    /// How Lucina routed the turn (intent, confidence, `jev_ms`, `fast`,
    /// `source`); absent for a Lucina without the voice fast path. No text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct DeviceTurn {
    at_ms: i64,
    ok: Option<bool>,
    err: Option<String>,
    first_audio_ms: Option<u64>,
    speech_ms: Option<u64>,
    up_bytes: Option<u64>,
    down_bytes: Option<u64>,
    server_ms: Option<Spans>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Turn {
    at_ms: i64,
    server: Option<TurnRecord>,
    device: Option<DeviceTurn>,
}

impl Turn {
    fn to_json(&self) -> Value {
        let s = self.server.as_ref();
        let d = self.device.as_ref();
        let spans = s
            .map(|s| s.spans.clone())
            .or_else(|| d.and_then(|d| d.server_ms.clone()))
            .unwrap_or_default();
        let mut v = json!({
            "at": ms_to_rfc(self.at_ms),
            "ok": s.map(|s| s.ok).or_else(|| d.and_then(|d| d.ok)).unwrap_or(false),
            "transcript_chars": s.map_or(0, |s| s.transcript_chars),
            "speech_ms": s.map(|s| s.speech_ms).or_else(|| d.and_then(|d| d.speech_ms)).unwrap_or(0),
            "spans": spans,
            "up_bytes": s.map(|s| s.up_bytes).or_else(|| d.and_then(|d| d.up_bytes)).unwrap_or(0),
            "down_bytes": s.map(|s| s.down_bytes).or_else(|| d.and_then(|d| d.down_bytes)).unwrap_or(0),
        });
        if let Some(e) = s
            .and_then(|s| s.err.clone())
            .or_else(|| d.and_then(|d| d.err.clone()))
        {
            v["err"] = json!(e);
        }
        if let Some(ms) = d.and_then(|d| d.first_audio_ms) {
            v["device_first_audio_ms"] = json!(ms);
        }
        if let Some(r) = s.and_then(|s| s.route.as_ref()) {
            put_route(&mut v, r);
        }
        v
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Prev {
    up: Option<f64>,
    down: Option<f64>,
    energy: Option<f64>,
}

/// What `status.json` holds.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    name: String,
    specs: Option<Map<String, Value>>,
    boot: Option<Map<String, Value>>,
    live: Option<Map<String, Value>>,
    last_seen_ms: Option<i64>,
    day: Option<String>,
    today_up: u64,
    today_down: u64,
    total_up: u64,
    total_down: u64,
    energy_today: f64,
    prev: Option<Prev>,
    turns: Vec<Turn>,
}

struct Dev {
    saved: Saved,
    /// Open voice sockets of this device.
    connections: u32,
    connected_since: Option<i64>,
    /// Size of `events.jsonl`.
    events_len: u64,
    dirty: bool,
    last_write: Option<Instant>,
}

fn ms_to_rfc(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

/// A query bound in the same fixed-width form `at` is stored in, so strings compare as times.
fn normalize_time(s: &str) -> Option<String> {
    let d = DateTime::parse_from_rfc3339(s).ok()?;
    Some(ms_to_rfc(d.timestamp_millis()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn delta(now: Option<f64>, prev: Option<f64>, fresh: bool) -> f64 {
    match (now, prev) {
        // Normal: the difference. A smaller value means the counter restarted (a reboot).
        (Some(n), Some(p)) if n >= p => n - p,
        (Some(n), Some(_)) => n,
        // No previous sample: all of it only if the device booted just now.
        (Some(n), None) if fresh => n,
        _ => 0.0,
    }
}

impl Dev {
    fn new(name: &str) -> Self {
        Self {
            saved: Saved {
                name: name.to_owned(),
                ..Saved::default()
            },
            connections: 0,
            connected_since: None,
            events_len: 0,
            dirty: false,
            last_write: None,
        }
    }

    fn load(dir: &Path, id: &str, name: &str) -> Self {
        let mut d = Self::new(name);
        if let Ok(mut f) = File::open(dir.join(id).join("status.json")) {
            let mut text = String::new();
            if f.read_to_string(&mut text).is_ok() {
                match serde_json::from_str::<Saved>(&text) {
                    Ok(s) => {
                        d.saved = s;
                        if !name.is_empty() {
                            d.saved.name = name.to_owned();
                        }
                    }
                    Err(e) => {
                        tracing::warn!(device = id, error = %e, "voice status.json unreadable; starting over")
                    }
                }
            }
        }
        d.events_len = fs::metadata(dir.join(id).join("events.jsonl")).map_or(0, |m| m.len());
        d
    }

    fn roll(&mut self, today: NaiveDate) {
        let day = today.to_string();
        if self.saved.day.as_deref() != Some(day.as_str()) {
            self.saved.today_up = 0;
            self.saved.today_down = 0;
            self.saved.energy_today = 0.0;
            self.saved.day = Some(day);
            self.dirty = true;
        }
    }

    fn apply_stats(&mut self, stats: &Map<String, Value>, today: NaiveDate) {
        self.roll(today);
        let f = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_f64);
        let up = f(stats, "up_bytes");
        let down = f(stats, "down_bytes");
        let energy = stats
            .get("power")
            .and_then(Value::as_object)
            .and_then(|p| f(p, "energy_mwh"));
        let fresh = f(stats, "uptime_ms").is_some_and(|u| u <= FRESH_BOOT_MS);
        let prev = self.saved.prev.clone().unwrap_or_default();
        let d_up = delta(up, prev.up, fresh) as u64;
        let d_down = delta(down, prev.down, fresh) as u64;
        let d_energy = delta(energy, prev.energy, fresh);
        self.saved.today_up += d_up;
        self.saved.total_up += d_up;
        self.saved.today_down += d_down;
        self.saved.total_down += d_down;
        self.saved.energy_today += d_energy;
        self.saved.prev = Some(Prev {
            up: up.or(prev.up),
            down: down.or(prev.down),
            energy: energy.or(prev.energy),
        });
        self.dirty = true;
    }

    fn trim_turns(&mut self) {
        self.saved.turns.sort_by_key(|t| t.at_ms);
        let extra = self.saved.turns.len().saturating_sub(KEEP_TURNS);
        self.saved.turns.drain(..extra);
    }

    /// The turn with a free `want_*` side nearest to `at_ms`, within the window.
    fn nearest(&self, at_ms: i64, wants_server: bool) -> Option<usize> {
        self.saved
            .turns
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if wants_server {
                    t.server.is_none()
                } else {
                    t.device.is_none()
                }
            })
            .map(|(i, t)| (i, (t.at_ms - at_ms).abs()))
            .filter(|(_, diff)| *diff <= MATCH_WINDOW_MS)
            .min_by_key(|(_, diff)| *diff)
            .map(|(i, _)| i)
    }

    fn add_server_turn(&mut self, rec: TurnRecord) {
        match self.nearest(rec.at_ms, true) {
            Some(i) => {
                self.saved.turns[i].at_ms = rec.at_ms;
                self.saved.turns[i].server = Some(rec);
            }
            None => self.saved.turns.push(Turn {
                at_ms: rec.at_ms,
                server: Some(rec),
                device: None,
            }),
        }
        self.trim_turns();
        self.dirty = true;
    }

    fn add_device_turn(&mut self, dev: DeviceTurn) {
        match self.nearest(dev.at_ms, false) {
            Some(i) => self.saved.turns[i].device = Some(dev),
            None => self.saved.turns.push(Turn {
                at_ms: dev.at_ms,
                server: None,
                device: Some(dev),
            }),
        }
        self.trim_turns();
        self.dirty = true;
    }

    fn est_mw(&self) -> Option<Value> {
        self.saved
            .live
            .as_ref()?
            .get("power")?
            .get("est_mw")
            .cloned()
    }

    fn rssi(&self) -> Option<Value> {
        self.saved.live.as_ref()?.get("rssi").cloned()
    }

    fn summary(&self, id: &str) -> Value {
        let mut v = json!({
            "id": id,
            "name": self.saved.name,
            "online": self.connections > 0,
            "last_seen": self.saved.last_seen_ms.map(ms_to_rfc),
            "fw": self.saved.specs.as_ref().and_then(|s| s.get("fw")).cloned().unwrap_or(Value::Null),
        });
        if let Some(r) = self.rssi() {
            v["rssi"] = r;
        }
        if let Some(p) = self.est_mw() {
            v["est_mw"] = p;
        }
        v
    }

    fn status(&mut self, id: &str, today: NaiveDate) -> Value {
        self.roll(today);
        let mut turns: Vec<&Turn> = self.saved.turns.iter().collect();
        turns.sort_by_key(|t| std::cmp::Reverse(t.at_ms));
        let measured = self
            .saved
            .live
            .as_ref()
            .and_then(|l| l.get("power"))
            .is_some_and(|p| p.get("rail_mv").is_some());
        let mut v = json!({
            "id": id,
            "name": self.saved.name,
            "online": self.connections > 0,
            "last_seen": self.saved.last_seen_ms.map(ms_to_rfc),
            "specs": self.saved.specs,
            "boot": self.saved.boot,
            "live": self.saved.live,
            "network": {
                "today_up_bytes": self.saved.today_up,
                "today_down_bytes": self.saved.today_down,
                "total_up_bytes": self.saved.total_up,
                "total_down_bytes": self.saved.total_down,
            },
            "power": {
                "est_mw": self.est_mw(),
                "measured": measured,
                "energy_mwh_today": (self.saved.energy_today * 100.0).round() / 100.0,
            },
            "turns": turns.iter().map(|t| t.to_json()).collect::<Vec<_>>(),
        });
        if let (Some(since), true) = (self.connected_since, self.connections > 0) {
            v["connected_since"] = json!(ms_to_rfc(since));
        }
        v
    }
}

// -- the store ---------------------------------------------------------------------

pub struct Store {
    dir: PathBuf,
    tz: chrono_tz::Tz,
    rotate_at: u64,
    inner: Mutex<HashMap<String, Dev>>,
}

/// What `GET .../events` asks for.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    /// Events at or after this (RFC 3339).
    pub since: Option<String>,
    /// Events strictly before this (RFC 3339).
    pub before: Option<String>,
    pub kind: Option<String>,
    pub limit: usize,
}

impl Store {
    pub fn new(data_dir: &Path, tz: chrono_tz::Tz) -> Self {
        Self {
            dir: data_dir.join("voice"),
            tz,
            rotate_at: ROTATE_AT,
            inner: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_rotation(mut self, bytes: u64) -> Self {
        self.rotate_at = bytes;
        self
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Dev>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn today(&self, now: DateTime<Utc>) -> NaiveDate {
        now.with_timezone(&self.tz).date_naive()
    }

    /// The device's state, loaded from disk on first use. `None` for an
    /// unusable id or when too many devices are held.
    fn dev<'a>(
        &self,
        map: &'a mut HashMap<String, Dev>,
        id: &str,
        name: &str,
    ) -> Option<&'a mut Dev> {
        if !valid_id(id) {
            return None;
        }
        if !map.contains_key(id) {
            if map.len() >= MAX_DEVICES {
                return None;
            }
            map.insert(id.to_owned(), Dev::load(&self.dir, id, name));
        }
        let d = map.get_mut(id)?;
        if !name.is_empty() && d.saved.name != name {
            d.saved.name = name.to_owned();
            d.dirty = true;
        }
        Some(d)
    }

    /// The state of a device that already has voice data (memory or disk);
    /// never creates one.
    fn existing<'a>(&self, map: &'a mut HashMap<String, Dev>, id: &str) -> Option<&'a mut Dev> {
        if !valid_id(id) || !(map.contains_key(id) || self.dev_dir(id).is_dir()) {
            return None;
        }
        self.dev(map, id, "")
    }

    fn dev_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    fn ensure_dir(&self, id: &str) -> std::io::Result<PathBuf> {
        let dir = self.dev_dir(id);
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        Ok(dir)
    }

    /// Append one event line, rotating first when the file is full.
    fn append(&self, d: &mut Dev, id: &str, line: &Value) {
        let mut text = line.to_string();
        text.push('\n');
        let r = (|| -> std::io::Result<()> {
            let dir = self.ensure_dir(id)?;
            let path = dir.join("events.jsonl");
            if d.events_len > 0 && d.events_len + text.len() as u64 > self.rotate_at {
                fs::rename(&path, dir.join("events.jsonl.1"))?;
                d.events_len = 0;
            }
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&path)?;
            f.write_all(text.as_bytes())?;
            d.events_len += text.len() as u64;
            Ok(())
        })();
        if let Err(e) = r {
            tracing::warn!(device = id, error = %e, "voice events not stored");
        }
    }

    fn write_status(&self, d: &mut Dev, id: &str) {
        let r = (|| -> std::io::Result<()> {
            let dir = self.ensure_dir(id)?;
            let tmp = dir.join("status.json.tmp");
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec(&d.saved).map_err(std::io::Error::other)?)?;
            f.sync_all()?;
            fs::rename(&tmp, dir.join("status.json"))
        })();
        if let Err(e) = r {
            tracing::warn!(device = id, error = %e, "voice status not stored");
        }
        d.dirty = false;
        d.last_write = Some(Instant::now());
    }

    /// Write `d`'s status if it changed and `FLUSH_EVERY` has passed (or `force`).
    fn maybe_flush(&self, d: &mut Dev, id: &str, force: bool) {
        if d.dirty && (force || d.last_write.is_none_or(|t| t.elapsed() >= FLUSH_EVERY)) {
            self.write_status(d, id);
        }
    }

    /// Write every changed status now.
    pub fn flush(&self) {
        let mut map = self.lock();
        for (id, d) in map.iter_mut() {
            self.maybe_flush(d, id, true);
        }
    }

    /// Write the statuses that changed and were not written for a while.
    fn flush_due(&self) {
        let mut map = self.lock();
        for (id, d) in map.iter_mut() {
            self.maybe_flush(d, id, false);
        }
    }

    // -- the socket's side

    pub fn connect_at(&self, id: &str, name: &str, now: DateTime<Utc>) {
        let mut map = self.lock();
        if let Some(d) = self.dev(&mut map, id, name) {
            d.roll(self.today(now));
            if d.connections == 0 {
                d.connected_since = Some(now.timestamp_millis());
            }
            d.connections += 1;
            d.saved.last_seen_ms = Some(now.timestamp_millis());
            d.dirty = true;
            self.maybe_flush(d, id, false);
        }
    }

    pub fn disconnect_at(&self, id: &str, now: DateTime<Utc>) {
        let mut map = self.lock();
        if let Some(d) = map.get_mut(id) {
            d.connections = d.connections.saturating_sub(1);
            if d.connections == 0 {
                d.connected_since = None;
            }
            d.saved.last_seen_ms = Some(now.timestamp_millis());
            d.dirty = true;
            self.maybe_flush(d, id, true);
        }
    }

    /// Any frame from the device: it is alive. Memory only.
    pub fn touch_at(&self, id: &str, now: DateTime<Utc>) {
        if let Some(d) = self.lock().get_mut(id) {
            d.saved.last_seen_ms = Some(now.timestamp_millis());
        }
    }

    /// A `hello` or `telemetry` text frame. Never fails; bad input is dropped.
    pub fn ingest_at(&self, id: &str, name: &str, kind: &str, text: &str, now: DateTime<Utc>) {
        if text.len() > MAX_FRAME {
            tracing::debug!(
                device = id,
                kind,
                bytes = text.len(),
                "oversized telemetry frame dropped"
            );
            return;
        }
        let Ok(frame) = serde_json::from_str::<Value>(text) else {
            tracing::debug!(device = id, kind, "malformed telemetry frame dropped");
            return;
        };
        let received = now.timestamp_millis();
        let mut map = self.lock();
        let Some(d) = self.dev(&mut map, id, name) else {
            return;
        };
        let today = self.today(now);
        d.saved.last_seen_ms = Some(received);
        match kind {
            "hello" => {
                let specs = clean(&frame["specs"], SPECS);
                let boot = clean(&frame["boot"], BOOT);
                if specs.is_empty() && boot.is_empty() {
                    return;
                }
                if !specs.is_empty() {
                    d.saved.specs = Some(specs);
                }
                if !boot.is_empty() {
                    d.saved.boot = Some(boot);
                }
                d.dirty = true;
            }
            "telemetry" => {
                for ev in parse_events(&frame, received) {
                    let at = ms_to_rfc(ev.at_ms);
                    let mut line = ev.body.clone();
                    line.insert("at".into(), json!(at));
                    line.insert("k".into(), json!(ev.kind));
                    match ev.kind {
                        "stats" => {
                            let mut live = ev.body.clone();
                            live.insert("at".into(), json!(at));
                            d.saved.live = Some(live);
                            d.apply_stats(&ev.body, today);
                        }
                        "turn" => d.add_device_turn(DeviceTurn {
                            at_ms: ev.at_ms,
                            ok: ev.body.get("ok").and_then(Value::as_bool),
                            err: ev
                                .body
                                .get("err")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            first_audio_ms: ev.body.get("first_audio_ms").and_then(Value::as_u64),
                            speech_ms: ev.body.get("speech_ms").and_then(Value::as_u64),
                            up_bytes: ev.body.get("up_bytes").and_then(Value::as_u64),
                            down_bytes: ev.body.get("down_bytes").and_then(Value::as_u64),
                            server_ms: ev.body.get("server_ms").map(Spans::from_map),
                        }),
                        "boot" => {
                            d.saved.boot = Some(clean(&Value::Object(ev.body.clone()), BOOT));
                            d.dirty = true;
                        }
                        _ => {}
                    }
                    self.append(d, id, &Value::Object(line));
                }
            }
            _ => return,
        }
        self.maybe_flush(d, id, false);
    }

    /// The server's record of a turn (the socket's or `POST /v1/voice`'s).
    pub fn record_turn_at(&self, id: &str, name: &str, rec: TurnRecord, now: DateTime<Utc>) {
        let mut map = self.lock();
        let Some(d) = self.dev(&mut map, id, name) else {
            return;
        };
        d.roll(self.today(now));
        let mut line = json!({
            "at": ms_to_rfc(rec.at_ms),
            "k": "server_turn",
            "ok": rec.ok,
            "transcript_chars": rec.transcript_chars,
            "speech_ms": rec.speech_ms,
            "spans": rec.spans,
            "up_bytes": rec.up_bytes,
            "down_bytes": rec.down_bytes,
        });
        if let Some(e) = &rec.err {
            line["err"] = json!(e);
        }
        if let Some(r) = &rec.route {
            put_route(&mut line, r);
        }
        self.append(d, id, &line);
        d.add_server_turn(rec);
        self.maybe_flush(d, id, false);
    }

    // -- the API's side

    /// Whether the device has sent a `hello`: it is a speaker.
    pub fn is_speaker(&self, id: &str) -> bool {
        let mut map = self.lock();
        self.existing(&mut map, id)
            .is_some_and(|d| d.saved.specs.is_some())
    }

    /// Whether there is voice data for `id`.
    pub fn known(&self, id: &str) -> bool {
        if !valid_id(id) {
            return false;
        }
        self.lock().contains_key(id) || self.dev_dir(id).is_dir()
    }

    pub fn status_at(&self, id: &str, now: DateTime<Utc>) -> Option<Value> {
        let mut map = self.lock();
        let today = self.today(now);
        Some(self.existing(&mut map, id)?.status(id, today))
    }

    /// Every device with voice data (or just `only`).
    pub fn list(&self, only: Option<&str>) -> Vec<Value> {
        let mut ids: Vec<String> = match only {
            Some(id) => vec![id.to_owned()],
            None => {
                let mut ids: Vec<String> = self.lock().keys().cloned().collect();
                if let Ok(rd) = fs::read_dir(&self.dir) {
                    for e in rd.flatten() {
                        if let Some(n) = e.file_name().to_str() {
                            if valid_id(n) && !ids.iter().any(|i| i == n) {
                                ids.push(n.to_owned());
                            }
                        }
                    }
                }
                ids
            }
        };
        ids.sort();
        let mut map = self.lock();
        ids.iter()
            .filter_map(|id| self.existing(&mut map, id).map(|d| d.summary(id)))
            .collect()
    }

    /// Events, newest first, and the cursor (`at` of the oldest returned)
    /// when more remain.
    pub fn events(&self, id: &str, q: &EventQuery) -> Option<(Vec<Value>, Option<String>)> {
        if !self.known(id) {
            return None;
        }
        let limit = q.limit.clamp(1, MAX_LIMIT);
        let dir = self.dev_dir(id);
        let _guard = self.lock(); // no rotation while reading
        let mut all: Vec<Value> = vec![];
        for name in ["events.jsonl.1", "events.jsonl"] {
            let Ok(text) = fs::read_to_string(dir.join(name)) else {
                continue;
            };
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let Some(at) = v["at"].as_str() else { continue };
                if q.since.as_deref().is_some_and(|s| at < s)
                    || q.before.as_deref().is_some_and(|b| at >= b)
                    || q.kind.as_deref().is_some_and(|k| v["k"] != k)
                {
                    continue;
                }
                all.push(v);
            }
        }
        all.reverse();
        all.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
        let more = all.len() > limit;
        all.truncate(limit);
        let next = more
            .then(|| all.last().and_then(|e| e["at"].as_str().map(str::to_owned)))
            .flatten();
        Some((all, next))
    }

    // -- with the real clock

    pub fn connect(&self, id: &str, name: &str) {
        self.connect_at(id, name, Utc::now());
    }

    pub fn disconnect(&self, id: &str) {
        self.disconnect_at(id, Utc::now());
    }

    pub fn touch(&self, id: &str) {
        self.touch_at(id, Utc::now());
    }

    pub fn ingest(&self, id: &str, name: &str, kind: &str, text: &str) {
        self.ingest_at(id, name, kind, text, Utc::now());
    }

    pub fn record_turn(&self, id: &str, name: &str, rec: TurnRecord) {
        self.record_turn_at(id, name, rec, Utc::now());
    }
}

/// The route's fields, flat, on a turn object.
fn put_route(v: &mut Value, r: &RouteInfo) {
    v["intent"] = json!(r.intent);
    v["confidence"] = json!(r.confidence);
    v["jev_ms"] = json!(r.jev_ms);
    v["fast"] = json!(r.fast);
    v["source"] = json!(r.source);
}

/// What the caller knows of a finished turn; the rest is in [`Shared`].
pub struct TurnInfo {
    /// The error code, or `None` for a good turn.
    pub err: Option<String>,
    pub transcript_chars: u64,
    pub speech_ms: u64,
    /// Audio bytes received for the turn.
    pub up_bytes: u64,
}

/// Record a finished turn for `ident`'s device.
pub fn finish_turn(st: &AppState, ident: &Identity, shared: &Shared, info: TurnInfo) {
    let total = shared.elapsed_ms();
    st.voice_log.record_turn(
        &ident.device_id,
        &ident.device_name,
        TurnRecord {
            at_ms: wall_ms_ago(total),
            ok: info.err.is_none(),
            err: info.err,
            transcript_chars: info.transcript_chars,
            speech_ms: info.speech_ms,
            spans: Spans {
                stt: shared.stt(),
                lucina_first: shared.lucina_first(),
                tts_first: shared.tts_first(),
                first_audio: shared.first_audio(),
                total: Some(total),
                route: shared.route_ms(),
                ack: shared.ack_ms(),
            },
            up_bytes: info.up_bytes,
            down_bytes: shared.down_bytes(),
            route: shared.route().cloned(),
        },
    );
}

/// A recording the socket turned down before any work (too short, no speech).
pub fn record_no_speech(st: &AppState, ident: &Identity, up_bytes: u64) {
    st.voice_log.record_turn(
        &ident.device_id,
        &ident.device_name,
        TurnRecord {
            at_ms: wall_ms_ago(0),
            ok: false,
            err: Some("no_speech".into()),
            speech_ms: up_bytes / 32,
            up_bytes,
            ..TurnRecord::default()
        },
    );
}

/// The wall time (ms) `elapsed_ms` before now.
pub fn wall_ms_ago(elapsed_ms: u64) -> i64 {
    Utc::now().timestamp_millis() - i64::try_from(elapsed_ms).unwrap_or(0)
}

/// Marks a device online while a voice socket is open.
pub struct ConnGuard {
    st: AppState,
    id: String,
}

impl ConnGuard {
    pub fn open(st: &AppState, ident: &Identity) -> Self {
        st.voice_log.connect(&ident.device_id, &ident.device_name);
        Self {
            st: st.clone(),
            id: ident.device_id.clone(),
        }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.st.voice_log.disconnect(&self.id);
    }
}

/// Writes changed statuses every [`FLUSH_EVERY`] and once more on shutdown.
pub fn spawn_flusher(st: &AppState) {
    let st = st.clone();
    tokio::spawn(async move {
        let mut shutdown = st.shutdown.subscribe();
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        loop {
            tokio::select! {
                _ = tick.tick() => st.voice_log.flush_due(),
                _ = shutdown.changed() => {
                    st.voice_log.flush();
                    return;
                }
            }
        }
    });
}

// -- HTTP ----------------------------------------------------------------------------

/// Every device token (speaker, phone app, a speaker paired but not connected yet) sees only its own
/// id; only the owner's browser session sees every device.
fn only_self(_st: &AppState, ident: &Identity) -> bool {
    !matches!(ident.via, Via::Cookie)
}

fn check_access(st: &AppState, ident: &Identity, id: &str) -> Result<(), ApiError> {
    if only_self(st, ident) && id != ident.device_id {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "a LucinaVoice device sees only itself",
        ));
    }
    if !st.voice_log.known(id) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "no voice data for this device",
        ));
    }
    Ok(())
}

async fn list(State(st): State<AppState>, Extension(ident): Extension<Identity>) -> Json<Value> {
    let only = only_self(&st, &ident).then_some(ident.device_id.as_str());
    Json(Value::Array(st.voice_log.list(only)))
}

async fn status(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_access(&st, &ident, &id)?;
    st.voice_log
        .status_at(&id, Utc::now())
        .map(Json)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "device_not_found",
                "no voice data for this device",
            )
        })
}

#[derive(Deserialize)]
struct EventsParams {
    since: Option<String>,
    before: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
}

fn bad(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

async fn events(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    UrlPath(id): UrlPath<String>,
    QueryArgs(p): QueryArgs<EventsParams>,
) -> Result<Json<Value>, ApiError> {
    check_access(&st, &ident, &id)?;
    let time = |name: &str, v: &Option<String>| -> Result<Option<String>, ApiError> {
        v.as_deref()
            .map(|s| normalize_time(s).ok_or_else(|| bad(&format!("{name}: an RFC 3339 time"))))
            .transpose()
    };
    let q = EventQuery {
        since: time("since", &p.since)?,
        before: time("before", &p.before)?,
        kind: match p.kind {
            Some(k)
                if k.len() <= 16
                    && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    && !k.is_empty() =>
            {
                Some(k)
            }
            Some(_) => return Err(bad("kind: lowercase letters and _")),
            None => None,
        },
        limit: p.limit.unwrap_or(DEFAULT_LIMIT),
    };
    let (events, next) = st.voice_log.events(&id, &q).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "no voice data for this device",
        )
    })?;
    let mut v = json!({"events": events});
    if let Some(n) = next {
        v["next"] = json!(n);
    }
    Ok(Json(v))
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/voice/devices", get(list))
        .route("/v1/voice/devices/{id}/status", get(status))
        .route("/v1/voice/devices/{id}/events", get(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "dev_abc";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn store(dir: &Path) -> Store {
        Store::new(dir, chrono_tz::Europe::Copenhagen)
    }

    fn hello() -> String {
        json!({"type": "hello", "now": 5000, "specs": {
            "chip": "esp32", "rev": "v3.1", "cores": 2, "cpu_mhz": 240, "fw": "0.4.1",
            "commit": "abc1234", "mac": "da:b8", "ina": "ina219", "temp_sensor": false,
            "evil": "<script>", "flash_bytes": 4194304},
            "boot": {"reset": "poweron", "seq": 7}})
        .to_string()
    }

    fn stats(up: u64, down: u64, mwh: f64, uptime: u64) -> Value {
        json!({"t": uptime, "k": "stats", "uptime_ms": uptime, "heap_free": 100000, "rssi": -60,
            "up_bytes": up, "down_bytes": down, "cpu_mhz": 240,
            "power": {"est_mw": 410, "rail_mv": 5000, "rail_ma": 82, "energy_mwh": mwh}})
    }

    fn frame(now: u64, events: Vec<Value>) -> String {
        json!({"type": "telemetry", "now": now, "dropped": 0, "events": events}).to_string()
    }

    fn server_turn(at_ms: i64) -> TurnRecord {
        TurnRecord {
            at_ms,
            ok: true,
            err: None,
            transcript_chars: 24,
            speech_ms: 1500,
            spans: Spans {
                stt: Some(150),
                lucina_first: Some(900),
                tts_first: Some(1700),
                first_audio: Some(1720),
                total: Some(4000),
                route: None,
                ack: None,
            },
            up_bytes: 48_000,
            down_bytes: 96_000,
            route: None,
        }
    }

    #[test]
    fn hello_keeps_only_known_clean_keys() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.connect_at(ID, "Speaker", at("2026-11-01T10:00:00Z"));
        s.ingest_at(ID, "Speaker", "hello", &hello(), at("2026-11-01T10:00:01Z"));
        let v = s.status_at(ID, at("2026-11-01T10:00:02Z")).unwrap();
        assert_eq!(v["online"], true);
        assert_eq!(v["specs"]["chip"], "esp32");
        assert_eq!(v["specs"]["fw"], "0.4.1");
        assert_eq!(v["specs"]["ina"], "ina219");
        assert_eq!(v["specs"]["temp_sensor"], false);
        assert!(v["specs"].get("evil").is_none());
        assert_eq!(v["boot"], json!({"reset": "poweron", "seq": 7}));
        assert!(s.is_speaker(ID));
        assert!(!s.is_speaker("dev_other"));
    }

    #[test]
    fn telemetry_updates_stats_counters_and_wall_times() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.connect_at(ID, "Speaker", at("2026-11-01T10:00:00Z"));
        // Received at 10:00:10; the device's `now` is 100 s, an event at t = 90 s.
        let ev = vec![
            json!({"t": 90_000, "k": "wifi", "up": true, "rssi": -55}),
            stats(1000, 5000, 1.0, 100_000),
        ];
        s.ingest_at(
            ID,
            "Speaker",
            "telemetry",
            &frame(100_000, ev),
            at("2026-11-01T10:00:10Z"),
        );
        let (events, _) = s
            .events(
                ID,
                &EventQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(events.len(), 2);
        // Newest first: the stats (t = now) then the wifi event, 10 s earlier.
        assert_eq!(events[0]["k"], "stats");
        assert_eq!(events[0]["at"], "2026-11-01T10:00:10.000Z");
        assert_eq!(events[1]["k"], "wifi");
        assert_eq!(events[1]["at"], "2026-11-01T10:00:00.000Z");
        assert_eq!(events[1]["rssi"], -55);

        // A later sample: the counters add the difference.
        let ev = vec![stats(1600, 5900, 1.5, 400_000)];
        s.ingest_at(
            ID,
            "Speaker",
            "telemetry",
            &frame(400_000, ev),
            at("2026-11-01T10:05:10Z"),
        );
        let v = s.status_at(ID, at("2026-11-01T10:05:11Z")).unwrap();
        // First sample counted in full (booted 100 s before), then +600 / +900.
        assert_eq!(v["network"]["today_up_bytes"], 1600);
        assert_eq!(v["network"]["today_down_bytes"], 5900);
        assert_eq!(v["network"]["total_up_bytes"], 1600);
        assert_eq!(v["power"]["energy_mwh_today"], 1.5);
        assert_eq!(v["power"]["measured"], true);
        assert_eq!(v["power"]["est_mw"], 410);
        assert_eq!(v["live"]["uptime_ms"], 400_000);
        assert_eq!(v["connected_since"], "2026-11-01T10:00:00.000Z");

        // A reboot: the totals restart, the new value counts in full.
        let ev = vec![stats(200, 300, 0.1, 60_000)];
        s.ingest_at(
            ID,
            "Speaker",
            "telemetry",
            &frame(60_000, ev),
            at("2026-11-01T10:10:00Z"),
        );
        let v = s.status_at(ID, at("2026-11-01T10:10:01Z")).unwrap();
        assert_eq!(v["network"]["total_up_bytes"], 1800);
        assert_eq!(v["network"]["total_down_bytes"], 6200);
    }

    #[test]
    fn a_first_sample_of_an_old_boot_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let ev = vec![stats(9_000_000, 9_000_000, 99.0, 86_400_000)];
        s.ingest_at(
            ID,
            "Speaker",
            "telemetry",
            &frame(86_400_000, ev),
            at("2026-11-01T10:00:00Z"),
        );
        let v = s.status_at(ID, at("2026-11-01T10:00:01Z")).unwrap();
        assert_eq!(v["network"]["total_up_bytes"], 0);
        assert_eq!(v["power"]["energy_mwh_today"], 0.0);
    }

    #[test]
    fn today_counters_roll_over_at_local_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        // 22:30 UTC on 1 Nov is 23:30 in Copenhagen (CET): still the 1st.
        let t1 = at("2026-11-01T22:30:00Z");
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(1000, vec![stats(1000, 2000, 2.0, 1000)]),
            t1,
        );
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(2000, vec![stats(1500, 2500, 2.5, 301_000)]),
            at("2026-11-01T22:50:00Z"),
        );
        let v = s.status_at(ID, at("2026-11-01T22:55:00Z")).unwrap();
        assert_eq!(v["network"]["today_up_bytes"], 1500);
        assert_eq!(v["power"]["energy_mwh_today"], 2.5);
        // 23:10 UTC is 00:10 on the 2nd locally: today starts over, totals stay.
        let v = s.status_at(ID, at("2026-11-01T23:10:00Z")).unwrap();
        assert_eq!(v["network"]["today_up_bytes"], 0);
        assert_eq!(v["network"]["today_down_bytes"], 0);
        assert_eq!(v["power"]["energy_mwh_today"], 0.0);
        assert_eq!(v["network"]["total_up_bytes"], 1500);
        // New traffic counts for the new day.
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(3000, vec![stats(1700, 2600, 2.6, 601_000)]),
            at("2026-11-01T23:20:00Z"),
        );
        let v = s.status_at(ID, at("2026-11-01T23:21:00Z")).unwrap();
        assert_eq!(v["network"]["today_up_bytes"], 200);
        assert_eq!(v["network"]["total_up_bytes"], 1700);
    }

    #[test]
    fn untrusted_telemetry_is_cleaned_or_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let now = at("2026-11-01T10:00:00Z");
        let long = "x".repeat(2000);
        let mut events = vec![
            json!({"t": 1, "k": "error", "what": "<b>boom</b>\u{7}", "detail": "d".repeat(500), "extra": 1}),
            json!({"t": 2, "k": "wifi", "up": "yes", "rssi": 99999}),
            json!({"t": 3, "k": "nope", "x": 1}),
            json!({"t": 4, "k": "error", "what": long}),
            json!({"t": 5, "k": "turn", "ok": true, "mode": "<img>", "total_ms": -5, "server_ms": {"stt": 12, "bad": 1}}),
            json!("not an object"),
            json!({"k": "link", "up": true}),
        ];
        // Past the 32nd, nothing is taken.
        for i in 0..40 {
            events.push(json!({"t": 10 + i, "k": "link", "up": true, "ms": i}));
        }
        s.ingest_at(ID, "S", "telemetry", &frame(100, events), now);
        let (all, _) = s
            .events(
                ID,
                &EventQuery {
                    limit: 500,
                    ..Default::default()
                },
            )
            .unwrap();
        let err = all.iter().find(|e| e["k"] == "error").unwrap();
        assert_eq!(err["what"], "bboom/b");
        assert_eq!(err["detail"].as_str().unwrap().len(), 160);
        assert!(err.get("extra").is_none());
        let wifi = all.iter().find(|e| e["k"] == "wifi").unwrap();
        assert!(wifi.get("up").is_none());
        assert_eq!(wifi["rssi"], 0);
        let turn = all.iter().find(|e| e["k"] == "turn").unwrap();
        assert!(turn.get("mode").is_none());
        assert_eq!(turn["total_ms"], 0);
        assert_eq!(turn["server_ms"], json!({"stt": 12}));
        assert!(all.iter().all(|e| e["k"] != "nope"));
        // Of the first 32 events: error, wifi, turn and the link without a time,
        // and 25 links.
        assert_eq!(all.len(), 29, "{}", all.len());

        // Oversized and malformed frames are ignored whole.
        let before = s
            .events(
                ID,
                &EventQuery {
                    limit: 500,
                    ..Default::default()
                },
            )
            .unwrap()
            .0
            .len();
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &format!("{{\"events\":[],\"pad\":\"{}\"}}", "p".repeat(MAX_FRAME)),
            now,
        );
        s.ingest_at(ID, "S", "telemetry", "{nope", now);
        s.ingest_at(ID, "S", "hello", "[]", now);
        s.ingest_at(ID, "S", "telemetry", "null", now);
        s.ingest_at(ID, "S", "bogus", "{}", now);
        let after = s
            .events(
                ID,
                &EventQuery {
                    limit: 500,
                    ..Default::default()
                },
            )
            .unwrap()
            .0
            .len();
        assert_eq!(before, after);
        assert!(!s.is_speaker(ID));
    }

    #[test]
    fn a_device_clock_far_off_cannot_place_events_in_the_future_or_past() {
        let evs = parse_events(
            &json!({"now": 10, "events": [
                {"t": 50, "k": "link", "up": true},
                {"t": 0, "k": "link", "up": false}]}),
            1_000_000_000,
        );
        // t after now: the receive time. Else received - (now - t).
        assert_eq!(evs[0].at_ms, 1_000_000_000);
        assert_eq!(evs[1].at_ms, 1_000_000_000 - 10);
        let evs = parse_events(
            &json!({"now": 1e12, "events": [{"t": 0, "k": "link", "up": true}]}),
            1_000_000_000_000,
        );
        assert_eq!(evs[0].at_ms, 1_000_000_000_000 - MAX_SKEW_MS);
        // No `now`: the receive time.
        let evs = parse_events(&json!({"events": [{"t": 3, "k": "link", "up": true}]}), 77);
        assert_eq!(evs[0].at_ms, 77);
    }

    #[test]
    fn server_and_device_turns_merge_in_either_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let t0 = at("2026-11-01T10:00:00Z");
        let ms = t0.timestamp_millis();
        let dev_turn = |t: u64, first: u64| {
            json!({"t": t, "k": "turn", "ok": true, "mode": "link", "speech_ms": 1500,
                "first_audio_ms": first, "total_ms": 4300, "up_bytes": 50000, "down_bytes": 100000,
                "server_ms": {"stt": 150, "total": 4000}})
        };
        // Server first, device 5 s later.
        s.record_turn_at(ID, "S", server_turn(ms), t0);
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(9000, vec![dev_turn(9000, 1900)]),
            at("2026-11-01T10:00:05Z"),
        );
        // Device first (10:01:00), server turn 4 s after it.
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(70_000, vec![dev_turn(70_000, 2100)]),
            at("2026-11-01T10:01:00Z"),
        );
        s.record_turn_at(
            ID,
            "S",
            server_turn(ms + 64_000),
            at("2026-11-01T10:01:04Z"),
        );
        // A device turn the server never saw.
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(500_000, vec![dev_turn(500_000, 3000)]),
            at("2026-11-01T10:10:00Z"),
        );

        let v = s.status_at(ID, at("2026-11-01T10:11:00Z")).unwrap();
        let turns = v["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 3, "{turns:?}");
        // Newest first: the device-only turn.
        assert_eq!(turns[0]["device_first_audio_ms"], 3000);
        assert_eq!(turns[0]["transcript_chars"], 0);
        assert_eq!(turns[0]["spans"]["stt"], 150);
        // The merged ones carry the server's spans and the device's measure.
        for (t, first) in [(&turns[1], 2100), (&turns[2], 1900)] {
            assert_eq!(t["device_first_audio_ms"], first);
            assert_eq!(t["transcript_chars"], 24);
            assert_eq!(t["spans"]["first_audio"], 1720);
            assert_eq!(t["spans"]["total"], 4000);
            assert_eq!(t["up_bytes"], 48_000);
            assert_eq!(t["ok"], true);
        }
        assert_eq!(turns[2]["at"], "2026-11-01T10:00:00.000Z");
        // The server turn is also an event, and carries no transcript text.
        let (evs, _) = s
            .events(
                ID,
                &EventQuery {
                    kind: Some("server_turn".into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(evs.len(), 2);
        assert!(evs[0].get("transcript").is_none());
    }

    #[test]
    fn only_the_last_twenty_turns_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let t0 = at("2026-11-01T10:00:00Z");
        for i in 0..30 {
            s.record_turn_at(ID, "S", server_turn(t0.timestamp_millis() + i * 60_000), t0);
        }
        let v = s.status_at(ID, t0).unwrap();
        let turns = v["turns"].as_array().unwrap();
        assert_eq!(turns.len(), KEEP_TURNS);
        assert_eq!(
            turns[0]["at"],
            ms_to_rfc(t0.timestamp_millis() + 29 * 60_000)
        );
    }

    #[test]
    fn events_rotate_at_the_limit_and_two_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path()).with_rotation(2_000);
        let t0 = at("2026-11-01T10:00:00Z");
        for i in 0..60i64 {
            s.ingest_at(
                ID,
                "S",
                "telemetry",
                &frame(1000, vec![json!({"t": 1000, "k": "error", "what": format!("e{i:03}"), "detail": "d".repeat(60)})]),
                t0 + chrono::Duration::seconds(i),
            );
        }
        let d = dir.path().join("voice").join(ID);
        let cur = fs::metadata(d.join("events.jsonl")).unwrap().len();
        let old = fs::metadata(d.join("events.jsonl.1")).unwrap().len();
        assert!(
            cur <= 2_000 && old <= 2_000 && cur > 0 && old > 0,
            "{cur} {old}"
        );
        assert!(!d.join("events.jsonl.2").exists());
        // Both files are read, newest first, and the oldest are gone.
        let (evs, next) = s
            .events(
                ID,
                &EventQuery {
                    limit: 500,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(next.is_none());
        assert_eq!(evs[0]["what"], "e059");
        assert!(evs.len() < 60 && evs.len() > 10, "{}", evs.len());
        let whats: Vec<&str> = evs.iter().map(|e| e["what"].as_str().unwrap()).collect();
        let mut sorted = whats.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(whats, sorted);
        // A restart picks the size up from disk and keeps rotating.
        drop(s);
        let s = store(dir.path()).with_rotation(2_000);
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(1000, vec![json!({"t": 1000, "k": "error", "what": "late"})]),
            t0 + chrono::Duration::seconds(100),
        );
        let (evs, _) = s
            .events(
                ID,
                &EventQuery {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(evs[0]["what"], "late");
    }

    #[test]
    fn events_filter_limit_and_page_with_before() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let t0 = at("2026-11-01T10:00:00Z");
        for i in 0..10i64 {
            let k = if i % 2 == 0 { "wifi" } else { "link" };
            let mut e = json!({"t": 1000, "k": k, "up": true});
            if k == "wifi" {
                e["rssi"] = json!(-50 - i);
            }
            s.ingest_at(
                ID,
                "S",
                "telemetry",
                &frame(1000, vec![e]),
                t0 + chrono::Duration::seconds(i),
            );
        }
        let q = |kind: Option<&str>, since: Option<&str>, before: Option<&str>, limit| EventQuery {
            since: since.and_then(normalize_time),
            before: before.and_then(normalize_time),
            kind: kind.map(str::to_owned),
            limit,
        };
        let (evs, next) = s.events(ID, &q(None, None, None, 4)).unwrap();
        assert_eq!(evs.len(), 4);
        assert_eq!(evs[0]["at"], "2026-11-01T10:00:09.000Z");
        // `next` is the `at` of the oldest returned; it feeds `before`.
        assert_eq!(next.as_deref(), Some("2026-11-01T10:00:06.000Z"));
        let (page2, next2) = s.events(ID, &q(None, None, next.as_deref(), 4)).unwrap();
        assert_eq!(page2[0]["at"], "2026-11-01T10:00:05.000Z");
        assert_eq!(page2.len(), 4);
        assert_eq!(next2.as_deref(), Some("2026-11-01T10:00:02.000Z"));
        let (page3, next3) = s.events(ID, &q(None, None, next2.as_deref(), 4)).unwrap();
        assert_eq!(page3.len(), 2);
        assert!(next3.is_none());
        // kind and since (inclusive).
        let (w, _) = s.events(ID, &q(Some("wifi"), None, None, 50)).unwrap();
        assert_eq!(w.len(), 5);
        assert!(w.iter().all(|e| e["k"] == "wifi"));
        let (w, _) = s
            .events(ID, &q(Some("wifi"), Some("2026-11-01T10:00:04Z"), None, 50))
            .unwrap();
        assert_eq!(w.len(), 3);
        // since given with an offset is the same instant.
        assert_eq!(
            normalize_time("2026-11-01T11:00:04+01:00").unwrap(),
            "2026-11-01T10:00:04.000Z"
        );
        assert!(normalize_time("yesterday").is_none());
        // A limit of 0 still returns one; huge limits are capped.
        assert_eq!(s.events(ID, &q(None, None, None, 0)).unwrap().0.len(), 1);
        assert!(s.events("dev_nobody", &q(None, None, None, 5)).is_none());
        assert!(s.events("../etc", &q(None, None, None, 5)).is_none());
    }

    #[test]
    fn status_survives_a_restart_and_is_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = at("2026-11-01T10:00:00Z");
        {
            let s = store(dir.path());
            s.connect_at(ID, "Speaker", t0);
            s.ingest_at(ID, "Speaker", "hello", &hello(), t0);
            s.ingest_at(
                ID,
                "Speaker",
                "telemetry",
                &frame(1000, vec![stats(700, 900, 0.7, 1000)]),
                t0,
            );
            s.record_turn_at(ID, "Speaker", server_turn(t0.timestamp_millis()), t0);
            s.disconnect_at(ID, t0 + chrono::Duration::seconds(30));
        }
        let path = dir.path().join("voice").join(ID).join("status.json");
        assert!(path.exists());
        assert!(!dir
            .path()
            .join("voice")
            .join(ID)
            .join("status.json.tmp")
            .exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let s = store(dir.path());
        let v = s.status_at(ID, t0 + chrono::Duration::seconds(60)).unwrap();
        assert_eq!(v["online"], false);
        assert!(v.get("connected_since").is_none());
        assert_eq!(v["name"], "Speaker");
        assert_eq!(v["specs"]["chip"], "esp32");
        assert_eq!(v["network"]["total_up_bytes"], 700);
        assert_eq!(v["turns"].as_array().unwrap().len(), 1);
        assert_eq!(v["last_seen"], "2026-11-01T10:00:30.000Z");
        assert!(s.is_speaker(ID));
        let list = s.list(None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["fw"], "0.4.1");
        assert_eq!(list[0]["est_mw"], 410);
    }

    #[test]
    fn status_writes_are_debounced() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let t0 = at("2026-11-01T10:00:00Z");
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(1000, vec![stats(10, 10, 0.1, 1000)]),
            t0,
        );
        let path = dir.path().join("voice").join(ID).join("status.json");
        assert!(path.exists(), "the first change is written at once");
        let first = fs::read_to_string(&path).unwrap();
        s.ingest_at(
            ID,
            "S",
            "telemetry",
            &frame(2000, vec![stats(50, 50, 0.2, 2000)]),
            t0,
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            first,
            "the second waits"
        );
        s.flush();
        assert_ne!(fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn bad_ids_get_no_directory() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let t0 = at("2026-11-01T10:00:00Z");
        for id in ["", "../x", "a/b", &"a".repeat(65)] {
            s.connect_at(id, "S", t0);
            s.record_turn_at(id, "S", server_turn(1), t0);
        }
        assert!(!dir.path().join("voice").exists());
    }
}

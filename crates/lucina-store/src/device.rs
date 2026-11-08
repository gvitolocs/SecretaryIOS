//! Device bridge stores (SecretaryIOS `docs/device-bridge.md`; reference:
//! the Node `src/device-bridge.js` on branch secretary-shopping-v2, which is
//! never merged into Node: the bridge ships in Rust only).
//!
//! - `device-context.jsonl`: events, one per line, kept `retention_days`.
//! - `device-state.json`: the latest state snapshot.
//! - `device-commands.json`: the last 200 commands and their results.
//!
//! Privacy: web visits keep only domain + title (full URLs are dropped at
//! ingest). Never log event contents.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::atomic::{open_append, write_atomic};
use crate::timeutil::{iso_ms, parse_js_date};
use crate::Result;

pub const EVENT_TYPES: [&str; 10] = [
    "web_visit",
    "location_visit",
    "health_summary",
    "workout",
    "focus_change",
    "screen_threshold",
    "calendar_change",
    "reminder_change",
    "alarm_fired",
    "app_open",
];
pub const SOURCES: [&str; 4] = ["ios", "mac", "safari-ext", "chrome-ext"];
pub const MAX_EVENTS_PER_BATCH: usize = 500;
pub const MAX_STATE_ITEMS: usize = 200;
const MAX_EVENT_DATA_BYTES: usize = 4096;
const MAX_TEXT: usize = 200;
const DAY_MS: i64 = 86_400_000;
const COMPACT_EVERY_MS: i64 = 6 * 3_600_000;
const COMMANDS_KEEP: usize = 200;

fn clip_text(v: &Value, max: usize) -> String {
    match v {
        Value::String(s) => {
            let norm = s.split_whitespace().collect::<Vec<_>>().join(" ");
            crate::timeutil::clip_utf16(&norm, max)
        }
        _ => String::new(),
    }
}

fn opt_text(v: &Value, max: usize) -> Value {
    let t = clip_text(v, max);
    if t.is_empty() {
        Value::Null
    } else {
        Value::String(t)
    }
}

fn clean_iso(v: &Value) -> Value {
    v.as_str()
        .and_then(parse_js_date)
        .map(|d| Value::String(iso_ms(d)))
        .unwrap_or(Value::Null)
}

fn clean_id(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(200).collect())
}

/// Hostname of a URL or bare domain, lowercased, without `www.`.
pub fn domain_of(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let rest = match raw.find("://") {
        Some(i)
            if raw[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
                && raw[..i]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic()) =>
        {
            &raw[i + 3..]
        }
        _ => raw,
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(host_port);
    let host = if host.starts_with('[') {
        host
    } else {
        host.split(':').next().unwrap_or("")
    };
    let host = host.to_lowercase();
    if host.is_empty()
        || host.contains(' ')
        || !host
            .chars()
            .all(|c| c.is_alphanumeric() || ".-_[]:".contains(c))
    {
        return String::new();
    }
    host.strip_prefix("www.")
        .unwrap_or(&host)
        .chars()
        .take(253)
        .collect()
}

fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Null => Some(0.0),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

fn sanitize_data(
    kind: &str,
    data: &Map<String, Value>,
) -> std::result::Result<Value, &'static str> {
    if kind == "web_visit" {
        let src = data
            .get("domain")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .or_else(|| data.get("url"));
        let domain = domain_of(src.and_then(Value::as_str).unwrap_or(""));
        if domain.is_empty() {
            return Err("missing_domain");
        }
        let mut out = Map::new();
        out.insert("domain".into(), json!(domain));
        let title = clip_text(data.get("title").unwrap_or(&Value::Null), MAX_TEXT);
        if !title.is_empty() {
            out.insert("title".into(), json!(title));
        }
        if let Some(d) = data.get("durationSec").and_then(js_number) {
            out.insert("durationSec".into(), json!((d.round()).max(0.0) as i64));
        }
        return Ok(Value::Object(out));
    }
    let text = serde_json::to_string(data).map_err(|_| "invalid_data")?;
    if text.len() > MAX_EVENT_DATA_BYTES {
        return Err("data_too_large");
    }
    Ok(Value::Object(data.clone()))
}

/// Validate one incoming event: the stored event or an error code.
pub fn validate_event(
    input: &Value,
    now: DateTime<Utc>,
    retention_days: i64,
) -> std::result::Result<Value, &'static str> {
    let obj = input.as_object().ok_or("not_an_object")?;
    let id = obj.get("id").and_then(Value::as_str).filter(|s| {
        (1..=128).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    });
    let id = id.ok_or("invalid_id")?;
    let ts = obj
        .get("ts")
        .and_then(Value::as_str)
        .and_then(parse_js_date)
        .ok_or("invalid_ts")?;
    let now_ms = now.timestamp_millis();
    if ts.timestamp_millis() > now_ms + DAY_MS {
        return Err("ts_in_future");
    }
    if ts.timestamp_millis() < now_ms - retention_days * DAY_MS {
        return Err("too_old");
    }
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| EVENT_TYPES.contains(t))
        .ok_or("invalid_type")?;
    let source = obj
        .get("source")
        .and_then(Value::as_str)
        .filter(|s| SOURCES.contains(s))
        .ok_or("invalid_source")?;
    let empty = Map::new();
    let data = match obj.get("data") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err("invalid_data"),
    };
    let data = sanitize_data(kind, data)?;
    Ok(
        json!({"id": id, "ts": iso_ms(ts), "type": kind, "source": source, "data": data, "receivedAt": iso_ms(now)}),
    )
}

struct EventsInner {
    loaded: bool,
    events: Vec<Value>,
    ids: HashSet<String>,
    last_compact_ms: i64,
    last_received_ms: i64,
}

/// JSONL event store with retention.
pub struct DeviceEventStore {
    path: PathBuf,
    retention_days: i64,
    inner: Mutex<EventsInner>,
}

fn ts_of(e: &Value) -> &str {
    e["ts"].as_str().unwrap_or("")
}

impl DeviceEventStore {
    pub fn new(path: impl Into<PathBuf>, retention_days: i64) -> Self {
        Self {
            path: path.into(),
            retention_days: retention_days.max(1),
            inner: Mutex::new(EventsInner {
                loaded: false,
                events: Vec::new(),
                ids: HashSet::new(),
                last_compact_ms: 0,
                last_received_ms: 0,
            }),
        }
    }

    fn load(&self, g: &mut EventsInner) -> Result<()> {
        if g.loaded {
            return Ok(());
        }
        g.loaded = true;
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(ev) = serde_json::from_str::<Value>(line) {
                if let Some(id) = ev["id"].as_str().map(str::to_owned) {
                    if g.ids.insert(id) {
                        let r = ev["receivedAt"]
                            .as_str()
                            .and_then(parse_js_date)
                            .map(|d| d.timestamp_millis())
                            .unwrap_or(0);
                        g.last_received_ms = g.last_received_ms.max(r);
                        g.events.push(ev);
                    }
                }
            }
        }
        g.events.sort_by(|a, b| ts_of(a).cmp(ts_of(b)));
        self.compact(g, Utc::now(), true)
    }

    fn compact(&self, g: &mut EventsInner, now: DateTime<Utc>, force: bool) -> Result<()> {
        let now_ms = now.timestamp_millis();
        if !force && now_ms - g.last_compact_ms < COMPACT_EVERY_MS {
            return Ok(());
        }
        g.last_compact_ms = now_ms;
        let cutoff = iso_ms(
            DateTime::from_timestamp_millis(now_ms - self.retention_days * DAY_MS)
                .unwrap_or_default(),
        );
        let before = g.events.len();
        g.events.retain(|e| ts_of(e) >= cutoff.as_str());
        if g.events.len() == before {
            return Ok(());
        }
        g.ids = g
            .events
            .iter()
            .filter_map(|e| e["id"].as_str().map(str::to_owned))
            .collect();
        let text: String = g.events.iter().map(|e| format!("{e}\n")).collect();
        write_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Validate, dedupe and store a batch: `{accepted, duplicates, rejected}`.
    pub fn ingest(&self, inputs: &[Value], now: DateTime<Utc>) -> Result<Value> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.load(&mut g)?;
        let (mut accepted, mut duplicates, mut rejected, mut fresh) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (index, input) in inputs.iter().enumerate() {
            match validate_event(input, now, self.retention_days) {
                Err(error) => {
                    let mut r = json!({"index": index});
                    if let Some(id) = input.get("id").and_then(Value::as_str) {
                        r["id"] = json!(id.chars().take(128).collect::<String>());
                    }
                    r["error"] = json!(error);
                    rejected.push(r);
                }
                Ok(ev) => {
                    let id = ev["id"].as_str().unwrap_or_default().to_owned();
                    if !g.ids.insert(id.clone()) {
                        duplicates.push(id);
                    } else {
                        accepted.push(id);
                        fresh.push(ev);
                    }
                }
            }
        }
        if !fresh.is_empty() {
            let text: String = fresh.iter().map(|e| format!("{e}\n")).collect();
            if let Err(e) = open_append(&self.path).and_then(|mut f| f.write_all(text.as_bytes())) {
                // Not stored: forget the ids, or the client's retry would be a
                // "duplicate" and the events lost (verification E1).
                for id in &accepted {
                    g.ids.remove(id);
                }
                return Err(e.into());
            }
            g.events.extend(fresh);
            g.events.sort_by(|a, b| ts_of(a).cmp(ts_of(b)));
            g.last_received_ms = now.timestamp_millis();
        }
        self.compact(&mut g, now, false)?;
        Ok(json!({"accepted": accepted, "duplicates": duplicates, "rejected": rejected}))
    }

    /// Events with `from <= ts < to`, oldest first.
    pub fn between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Value>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.load(&mut g)?;
        let (f, t) = (iso_ms(from), iso_ms(to));
        Ok(g.events
            .iter()
            .filter(|e| ts_of(e) >= f.as_str() && ts_of(e) < t.as_str())
            .cloned()
            .collect())
    }

    pub fn last_received_ms(&self) -> Result<i64> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.load(&mut g)?;
        Ok(g.last_received_ms)
    }
}

// ---------------------------------------------------------------- state

/// "7:05", "07:05", "7.05" -> "07:05".
pub fn normalize_clock(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (h, rest) = s.split_once([':', '.'])?;
    let m: String = rest.chars().take(2).collect();
    if h.is_empty()
        || h.len() > 2
        || m.len() != 2
        || !h.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 23 && m <= 59).then(|| format!("{h:02}:{m:02}"))
}

fn clean_entry(list: &str, e: &Value) -> Option<Value> {
    let id = clean_id(&e["id"])?;
    match list {
        "alarms" => {
            let time = normalize_clock(&e["time"])?;
            Some(
                json!({"id": id, "time": time, "label": opt_text(&e["label"], MAX_TEXT), "enabled": e["enabled"] != json!(false), "repeat": opt_text(&e["repeat"], 40)}),
            )
        }
        "timers" => Some(json!({
            "id": id, "label": opt_text(&e["label"], MAX_TEXT), "endsAt": clean_iso(&e["endsAt"]),
            "seconds": js_number(&e["seconds"]).filter(|_| !e["seconds"].is_null()).map(|n| json!(n.round() as i64)).unwrap_or(Value::Null),
        })),
        "reminders" => {
            let title = clip_text(&e["title"], MAX_TEXT);
            (!title.is_empty()).then(|| json!({"id": id, "title": title, "due": clean_iso(&e["due"]), "list": opt_text(&e["list"], 80), "completed": e["completed"] == json!(true)}))
        }
        _ => {
            let title = clip_text(&e["title"], MAX_TEXT);
            let start = clean_iso(&e["start"]);
            (!title.is_empty() && !start.is_null()).then(|| json!({
                "id": id, "title": title, "start": start, "end": clean_iso(&e["end"]), "calendar": opt_text(&e["calendar"], 80), "allDay": e["allDay"] == json!(true),
            }))
        }
    }
}

/// Validate a `POST /device/state` body into the stored snapshot.
pub fn validate_state(body: &Value, now: DateTime<Utc>) -> std::result::Result<Value, String> {
    let obj = body.as_object().ok_or("body must be an object")?;
    let mut state = Map::new();
    state.insert("receivedAt".into(), json!(iso_ms(now)));
    let ts = clean_iso(obj.get("ts").unwrap_or(&Value::Null));
    state.insert(
        "ts".into(),
        if ts.is_null() { json!(iso_ms(now)) } else { ts },
    );
    let source = obj
        .get("source")
        .and_then(Value::as_str)
        .filter(|s| SOURCES.contains(s))
        .unwrap_or("ios");
    state.insert("source".into(), json!(source));
    for key in ["alarms", "timers", "reminders", "events"] {
        let list = match obj.get(key) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return Err(format!("{key} must be an array")),
        };
        if list.len() > MAX_STATE_ITEMS {
            return Err(format!("{key} has more than {MAX_STATE_ITEMS} entries"));
        }
        let cleaned: Vec<Value> = list
            .iter()
            .filter(|e| e.is_object())
            .filter_map(|e| clean_entry(key, e))
            .collect();
        state.insert(key.into(), Value::Array(cleaned));
    }
    let focus = obj.get("focus").filter(|f| f.is_object());
    let mode = focus.map(|f| clip_text(&f["mode"], 60)).unwrap_or_default();
    state.insert(
        "focus".into(),
        if mode.is_empty() {
            Value::Null
        } else {
            json!({"mode": mode, "since": clean_iso(&focus.unwrap()["since"])})
        },
    );
    Ok(Value::Object(state))
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn write_pretty(path: &Path, v: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(v)?;
    write_atomic(path, format!("{text}\n").as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------- commands

/// Deletes/cancels and anything outside alarms, timers and reminders need a tap.
pub fn requires_confirmation(kind: &str) -> bool {
    kind.ends_with(".delete")
        || kind.ends_with(".cancel")
        || !(kind.starts_with("alarm.")
            || kind.starts_with("timer.")
            || kind.starts_with("reminder."))
}

pub fn public_command(c: &Value, now: DateTime<Utc>) -> Value {
    let expired = c["status"] == "pending"
        && c["expiresAt"]
            .as_str()
            .and_then(parse_js_date)
            .is_none_or(|e| e <= now);
    let or_null = |k: &str| {
        if c[k].is_null() || c[k] == json!("") {
            Value::Null
        } else {
            c[k].clone()
        }
    };
    json!({
        "id": c["id"], "type": c["type"], "args": c["args"],
        "status": if expired { json!("expired") } else { c["status"].clone() },
        "requiresConfirmation": c["requiresConfirmation"] == json!(true),
        "channel": c["channel"], "createdAt": c["createdAt"], "expiresAt": c["expiresAt"],
        "result": or_null("result"), "resultAt": or_null("resultAt"),
        "timelineItemId": or_null("timelineItemId"), "confirmationItemId": or_null("confirmationItemId"),
    })
}

/// `meta.command` for the timeline item carrying a command.
pub fn timeline_command_meta(c: &Value) -> Value {
    json!({"id": c["id"], "type": c["type"], "args": c["args"], "expiresAt": c["expiresAt"], "requiresConfirmation": c["requiresConfirmation"] == json!(true)})
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResultError {
    NotFound,
    Expired,
    AlreadyResolved,
}

/// State, commands and events of the device bridge.
pub struct DeviceStore {
    pub events: DeviceEventStore,
    state_path: PathBuf,
    commands_path: PathBuf,
    command_ttl_ms: i64,
    reachable_minutes: i64,
    lock: Mutex<()>,
}

impl DeviceStore {
    pub fn new(
        data_dir: &Path,
        retention_days: i64,
        command_ttl_ms: i64,
        reachable_minutes: i64,
    ) -> Self {
        Self::with_paths(
            data_dir.join("device-context.jsonl"),
            data_dir.join("device-state.json"),
            data_dir.join("device-commands.json"),
            retention_days,
            command_ttl_ms,
            reachable_minutes,
        )
    }

    pub fn with_paths(
        events: PathBuf,
        state: PathBuf,
        commands: PathBuf,
        retention_days: i64,
        command_ttl_ms: i64,
        reachable_minutes: i64,
    ) -> Self {
        Self {
            events: DeviceEventStore::new(events, retention_days),
            state_path: state,
            commands_path: commands,
            command_ttl_ms,
            reachable_minutes,
            lock: Mutex::new(()),
        }
    }

    pub fn read_state(&self) -> Option<Value> {
        read_json(&self.state_path)
    }

    pub fn save_state(&self, state: &Value) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        write_pretty(&self.state_path, state)
    }

    pub fn reachable(&self, now: DateTime<Utc>) -> bool {
        let state_ms = self
            .read_state()
            .and_then(|s| s["receivedAt"].as_str().and_then(parse_js_date))
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        let last = state_ms.max(self.events.last_received_ms().unwrap_or(0));
        last > 0 && now.timestamp_millis() - last <= self.reachable_minutes * 60_000
    }

    fn commands(&self) -> Vec<Value> {
        read_json(&self.commands_path)
            .and_then(|v| v["commands"].as_array().cloned())
            .unwrap_or_default()
    }

    fn write_commands(&self, list: &[Value]) -> Result<()> {
        let skip = list.len().saturating_sub(COMMANDS_KEEP);
        write_pretty(
            &self.commands_path,
            &json!({"commands": &list[skip..], "updatedAt": iso_ms(Utc::now())}),
        )
    }

    pub fn create_command(
        &self,
        kind: &str,
        args: Value,
        channel: &str,
        lang: &str,
        now: DateTime<Utc>,
    ) -> Result<Value> {
        let cmd = json!({
            "id": uuid::Uuid::new_v4().to_string(), "type": kind, "args": args, "channel": channel, "lang": lang,
            "requiresConfirmation": requires_confirmation(kind), "status": "pending", "createdAt": iso_ms(now),
            "expiresAt": iso_ms(now + chrono::Duration::milliseconds(self.command_ttl_ms)),
            "timelineItemId": null, "result": null, "resultAt": null, "confirmationItemId": null,
        });
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = self.commands();
        list.push(cmd.clone());
        self.write_commands(&list)?;
        Ok(cmd)
    }

    pub fn update_command(&self, id: &str, patch: Value) -> Result<Option<Value>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = self.commands();
        let Some(c) = list.iter_mut().find(|c| c["id"] == json!(id)) else {
            return Ok(None);
        };
        if let (Some(obj), Some(p)) = (c.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                obj.insert(k.clone(), v.clone());
            }
        }
        let out = c.clone();
        self.write_commands(&list)?;
        Ok(Some(out))
    }

    pub fn find_command(&self, id: &str) -> Option<Value> {
        self.commands().into_iter().find(|c| c["id"] == json!(id))
    }

    pub fn pending_commands(&self, now: DateTime<Utc>) -> Vec<Value> {
        self.commands()
            .into_iter()
            .filter(|c| {
                c["status"] == "pending"
                    && c["expiresAt"]
                        .as_str()
                        .and_then(parse_js_date)
                        .is_some_and(|e| e > now)
            })
            .collect()
    }

    /// Record the phone's outcome: `(command, duplicate)`.
    pub fn record_result(
        &self,
        id: &str,
        ok: bool,
        error: &str,
        device_object_id: &str,
        now: DateTime<Utc>,
    ) -> std::result::Result<(Value, bool), ResultError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = self.commands();
        let c = list
            .iter_mut()
            .find(|c| c["id"] == json!(id))
            .ok_or(ResultError::NotFound)?;
        let err = clip_text(&json!(error), 300);
        let obj = clip_text(&json!(device_object_id), 200);
        let result = json!({
            "ok": ok,
            "error": if ok { Value::Null } else if err.is_empty() { json!("failed") } else { json!(err) },
            "deviceObjectId": if obj.is_empty() { Value::Null } else { json!(obj) },
        });
        if !c["result"].is_null() {
            if c["result"]["ok"] == json!(ok) {
                return Ok((c.clone(), true));
            }
            return Err(ResultError::AlreadyResolved);
        }
        if c["expiresAt"]
            .as_str()
            .and_then(parse_js_date)
            .is_none_or(|e| e <= now)
        {
            return Err(ResultError::Expired);
        }
        c["result"] = result;
        c["status"] = json!(if ok { "done" } else { "failed" });
        c["resultAt"] = json!(iso_ms(now));
        let out = c.clone();
        self.write_commands(&list)
            .map_err(|_| ResultError::NotFound)?;
        Ok((out, false))
    }
}

fn duration_text(seconds: i64, lang: &str) -> String {
    let w = if lang == "da" {
        ["t", "min", "s"]
    } else {
        ["h", "min", "s"]
    };
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    let parts: Vec<String> = [(h, w[0]), (m, w[1]), (s, w[2])]
        .iter()
        .filter(|(n, _)| *n != 0)
        .map(|(n, u)| format!("{n} {u}"))
        .collect();
    if parts.is_empty() {
        format!("0 {}", w[2])
    } else {
        parts.join(" ")
    }
}

/// Lucina's confirmation text once the phone reported a result
/// (`deviceResultText`).
pub fn result_text(c: &Value) -> String {
    // English or Danish, never Italian, whatever the phone says (owner rule 2026-11-02).
    let lang = if c["lang"].as_str() == Some("da") { "da" } else { "en" };
    let a = &c["args"];
    let s = |k: &str| a[k].as_str().unwrap_or("").to_owned();
    if c["result"]["ok"] != json!(true) {
        let e = c["result"]["error"].as_str().unwrap_or("");
        let sep = if e.is_empty() {
            String::new()
        } else {
            format!(": {e}")
        };
        return match lang {
            "it" => format!("Il telefono non ci è riuscito{sep}."),
            "da" => format!("Telefonen kunne ikke gøre det{sep}."),
            _ => format!("Your phone could not do that{sep}."),
        };
    }
    let secs = a["seconds"].as_i64().unwrap_or(0);
    let kind = c["type"].as_str().unwrap_or("");
    match (lang, kind) {
        ("it", "alarm.set") => format!("Fatto: sveglia impostata alle {}.", s("time")),
        ("it", "alarm.delete") => format!("Fatto: sveglia delle {} eliminata.", s("time")),
        ("it", "timer.start") => format!("Fatto: timer di {} avviato.", duration_text(secs, "it")),
        ("it", "timer.cancel") => "Fatto: timer annullato.".into(),
        ("it", "reminder.create") => format!("Fatto: \"{}\" è nei Promemoria.", s("title")),
        ("it", "calendar.create") => format!("Fatto: \"{}\" è nel calendario.", s("title")),
        ("it", _) => "Fatto sul telefono.".into(),
        ("da", "alarm.set") => format!("Færdig: alarm sat til kl. {}.", s("time")),
        ("da", "alarm.delete") => format!("Færdig: alarmen kl. {} er slettet.", s("time")),
        ("da", "timer.start") => format!("Færdig: timer på {} startet.", duration_text(secs, "da")),
        ("da", "timer.cancel") => "Færdig: timeren er annulleret.".into(),
        ("da", "reminder.create") => format!("Færdig: \"{}\" er i Påmindelser.", s("title")),
        ("da", "calendar.create") => format!("Færdig: \"{}\" er i kalenderen.", s("title")),
        ("da", _) => "Færdig på telefonen.".into(),
        (_, "alarm.set") => format!("Done: alarm set for {}.", s("time")),
        (_, "alarm.delete") => format!("Done: your {} alarm is deleted.", s("time")),
        (_, "timer.start") => format!("Done: {} timer started.", duration_text(secs, "en")),
        (_, "timer.cancel") => "Done: timer cancelled.".into(),
        (_, "reminder.create") => format!("Done: \"{}\" is in your Reminders.", s("title")),
        (_, "calendar.create") => format!("Done: \"{}\" is in your calendar.", s("title")),
        _ => "Done on your phone.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_validate_dedupe_and_strip_urls() {
        let dir = tempfile::tempdir().unwrap();
        let s = DeviceStore::new(dir.path(), 90, 600_000, 360);
        let now = Utc::now();
        let ts = iso_ms(now - chrono::Duration::minutes(5));
        let batch = vec![
            json!({"id": "e1", "ts": ts, "type": "web_visit", "source": "safari-ext", "data": {"url": "https://www.GitHub.com/x/y?token=secret", "title": "PR  #12"}}),
            json!({"id": "e1", "ts": ts, "type": "web_visit", "source": "safari-ext", "data": {"url": "https://github.com"}}),
            json!({"id": "bad id", "ts": ts, "type": "workout", "source": "ios"}),
            json!({"id": "e3", "ts": "2001-01-01T00:00:00Z", "type": "workout", "source": "ios"}),
            json!({"id": "e4", "ts": ts, "type": "teleport", "source": "ios"}),
            json!({"id": "e5", "ts": ts, "type": "web_visit", "source": "ios", "data": {}}),
            json!({"id": "e6", "ts": ts, "type": "location_visit", "source": "ios", "data": {"name": "Gym", "lat": 55.6}}),
            json!("nope"),
        ];
        let out = s.events.ingest(&batch, now).unwrap();
        assert_eq!(out["accepted"], json!(["e1", "e6"]));
        assert_eq!(out["duplicates"], json!(["e1"]));
        let errors: Vec<&str> = out["rejected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["error"].as_str().unwrap())
            .collect();
        assert_eq!(
            errors,
            vec![
                "invalid_id",
                "too_old",
                "invalid_type",
                "missing_domain",
                "not_an_object"
            ]
        );
        let text = std::fs::read_to_string(dir.path().join("device-context.jsonl")).unwrap();
        assert!(
            !text.contains("token=secret")
                && text.contains("\"domain\":\"github.com\"")
                && text.contains("\"title\":\"PR #12\"")
        );
        assert!(s.reachable(now));
        // a fresh store reloads and still dedupes
        let s2 = DeviceStore::new(dir.path(), 90, 600_000, 360);
        assert_eq!(
            s2.events.ingest(&[batch[0].clone()], now).unwrap()["duplicates"],
            json!(["e1"])
        );
        assert_eq!(domain_of("news.ycombinator.com"), "news.ycombinator.com");
        assert_eq!(
            domain_of("http://user:pw@WWW.example.org:8080/a"),
            "example.org"
        );
    }

    #[test]
    fn state_and_commands() {
        let dir = tempfile::tempdir().unwrap();
        let s = DeviceStore::new(dir.path(), 90, 600_000, 360);
        let now = Utc::now();
        let state = validate_state(&json!({"source": "ios", "alarms": [{"id": "A1", "time": "7:00", "label": "Gym"}, {"id": "A2", "time": "25:00"}, {"time": "08:00"}],
            "timers": [{"id": "T1", "seconds": "600"}], "events": [{"id": "E1", "title": "Dentist"}], "focus": {"mode": "Work"}}), now).unwrap();
        assert_eq!(
            state["alarms"],
            json!([{"id": "A1", "time": "07:00", "label": "Gym", "enabled": true, "repeat": null}])
        );
        assert_eq!(state["timers"][0]["seconds"], 600);
        assert_eq!(state["events"], json!([]));
        assert_eq!(state["focus"]["mode"], "Work");
        assert!(validate_state(&json!({"alarms": {}}), now).is_err());
        s.save_state(&state).unwrap();
        assert_eq!(s.read_state().unwrap()["alarms"][0]["id"], "A1");

        let c = s
            .create_command(
                "alarm.delete",
                json!({"id": "A1", "time": "07:00"}),
                "ios",
                "it",
                now,
            )
            .unwrap();
        assert_eq!(c["requiresConfirmation"], true);
        assert_eq!(s.pending_commands(now).len(), 1);
        let id = c["id"].as_str().unwrap();
        assert_eq!(
            s.record_result("nope", true, "", "", now),
            Err(ResultError::NotFound)
        );
        let (done, dup) = s.record_result(id, true, "", "A1", now).unwrap();
        assert!(!dup);
        assert_eq!(done["status"], "done");
        assert_eq!(result_text(&done), "Done: your 07:00 alarm is deleted.");
        assert!(s.record_result(id, true, "", "A1", now).unwrap().1);
        assert_eq!(
            s.record_result(id, false, "x", "", now),
            Err(ResultError::AlreadyResolved)
        );
        let late = s
            .create_command(
                "timer.start",
                json!({"seconds": 600}),
                "web",
                "en",
                now - chrono::Duration::minutes(11),
            )
            .unwrap();
        assert_eq!(public_command(&late, now)["status"], "expired");
        assert_eq!(
            s.record_result(late["id"].as_str().unwrap(), true, "", "", now),
            Err(ResultError::Expired)
        );
        assert!(!requires_confirmation("timer.start") && requires_confirmation("calendar.create"));
    }
}

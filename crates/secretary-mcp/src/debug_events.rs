//! `debug_events`: the debug event log of the owner's apps (taps, intents,
//! notifications, Live Activities, Face ID, failed requests), from
//! secretary-server's `GET /v1/debug/events` with the MCP's device token
//! (SecretaryIOS `docs/debug-events.md`). The log never holds secrets or
//! message, note or command text; the server refuses such fields on arrival.

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::backend::Backend;
use crate::tools::{ToolError, ToolResult};

pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;

/// Arguments of `debug_events`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Args {
    /// A device id, or part of a device name, case-insensitive (e.g. "iphone", "macbook"). Default: every device.
    #[serde(default)]
    pub device: Option<String>,
    /// Events at or after: "30m", "2h", "1d", a date YYYY-MM-DD (owner's time zone) or an RFC 3339 time. Default: the newest events, however old.
    #[serde(default)]
    pub since: Option<String>,
    /// Events at or before (same forms as since).
    #[serde(default)]
    pub until: Option<String>,
    /// A prefix of the event type: "net" (requests), "ui.tap", "card", "notification", "activity", "auth" (Face ID), "approval", "unlock_key", "app" (lifecycle), "bg".
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// How many of the newest matching events (1-500, default 100); they are listed oldest first.
    #[serde(default)]
    pub limit: Option<u32>,
}

fn bad(m: impl Into<String>) -> ToolError {
    ToolError(m.into())
}

/// `2026-11-01T09:00:00.250Z` → `10:00:00.250` (or `2026-11-01 10:00:00.250` when `day`).
fn clock(ts: &str, tz: Tz, day: bool) -> String {
    match DateTime::parse_from_rfc3339(ts) {
        Ok(d) => tz
            .from_utc_datetime(&d.with_timezone(&Utc).naive_utc())
            .format(if day {
                "%Y-%m-%d %H:%M:%S%.3f"
            } else {
                "%H:%M:%S%.3f"
            })
            .to_string(),
        Err(_) => ts.to_owned(),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) if s.contains(' ') || s.is_empty() => format!("{s:?}"),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// One line per event, oldest first, times in `tz`; the date is printed when it changes.
pub fn format(v: &Value, a: &Args, tz: Tz) -> String {
    let events = v["events"].as_array().cloned().unwrap_or_default();
    let mut what = vec![];
    if let Some(d) = &a.device {
        what.push(format!("device {d}"));
    }
    if let Some(k) = &a.kind {
        what.push(format!("type {k}*"));
    }
    if let Some(s) = &a.since {
        what.push(format!("since {s}"));
    }
    if let Some(u) = &a.until {
        what.push(format!("until {u}"));
    }
    let filter = if what.is_empty() {
        String::new()
    } else {
        format!(" ({})", what.join(", "))
    };
    if events.is_empty() {
        return format!("No debug events{filter}.");
    }
    let mut out = format!(
        "{} debug event(s){filter}, oldest first, times in {}:\n",
        events.len(),
        tz.name()
    );
    let mut last_day = String::new();
    for e in &events {
        let ts = e["ts"].as_str().unwrap_or("");
        let full = clock(ts, tz, true);
        let day = full.split(' ').next().unwrap_or("").to_owned();
        let time = if day != last_day {
            last_day = day;
            full
        } else {
            clock(ts, tz, false)
        };
        let device = e["device_name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .or(e["device"].as_str())
            .unwrap_or("?");
        let mut line = format!(
            "{time} [{device}/{}] {}",
            e["process"].as_str().unwrap_or("?"),
            e["type"].as_str().unwrap_or("?")
        );
        if let Some(fields) = e["fields"].as_object() {
            for (k, v) in fields {
                line.push_str(&format!(" {k}={}", scalar(v)));
            }
        }
        line.push_str(&format!(
            "  (seq {}, build {})",
            scalar(&e["seq"]),
            e["build"].as_str().unwrap_or("?")
        ));
        out.push_str(&line);
        out.push('\n');
    }
    if v["more"] == true {
        out.push_str("Older matching events exist: narrow with since / type, or raise limit.\n");
    }
    out
}

pub async fn query(backend: &Backend, a: Args, tz: Tz) -> ToolResult {
    let limit = a.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad(format!("`limit` must be 1-{MAX_LIMIT}")));
    }
    let mut q = vec![("limit", limit.to_string())];
    for (name, v) in [
        ("device", &a.device),
        ("since", &a.since),
        ("until", &a.until),
        ("type", &a.kind),
    ] {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            if v.chars().count() > 128 {
                return Err(bad(format!("`{name}` is too long")));
            }
            q.push((name, v.to_owned()));
        }
    }
    let v = backend.get("/v1/debug/events", &q).await?;
    Ok(format(&v, &a, tz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_show_local_time_device_type_and_fields() {
        let v = json!({"count": 2, "more": true, "events": [
            {"ts": "2026-11-01T09:00:00.250Z", "seq": 7, "device": "dev_a", "device_name": "iPhone",
             "build": "adfd5fe", "process": "app", "type": "ui.tap",
             "fields": {"element": "approve", "screen": "approval sheet"}},
            {"ts": "2026-11-01T09:00:01.000Z", "seq": 8, "device": "dev_a", "device_name": "iPhone",
             "build": "adfd5fe", "process": "widgets", "type": "net.request",
             "fields": {"method": "POST", "status": 500}},
        ]});
        let a = Args {
            device: Some("iphone".into()),
            ..Default::default()
        };
        let text = format(&v, &a, chrono_tz::Europe::Copenhagen);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("2 debug event(s) (device iphone)"),
            "{text}"
        );
        assert_eq!(
            lines[1],
            "2026-11-01 10:00:00.250 [iPhone/app] ui.tap element=approve screen=\"approval sheet\"  (seq 7, build adfd5fe)"
        );
        assert_eq!(
            lines[2],
            "10:00:01.000 [iPhone/widgets] net.request method=POST status=500  (seq 8, build adfd5fe)"
        );
        assert!(lines[3].starts_with("Older matching events exist"));
        assert_eq!(
            format(&json!({"events": []}), &Args::default(), chrono_tz::UTC),
            "No debug events."
        );
    }
}

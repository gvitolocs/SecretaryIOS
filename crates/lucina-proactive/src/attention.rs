//! The attention gate: a candidate the score wants to send goes out only
//! when it is a good moment. In order:
//!
//! 1. expired → drop;
//! 2. muted by "Basta così" (this kind, `budget.stop_days`) → drop;
//! 3. quiet hours or Do Not Disturb (secretary-server's settings, else the
//!    `[quiet]` fallback) → hold until they end (Pulse if it expires first);
//! 4. Discord says he is playing a game or is on Do Not Disturb, or the
//!    iPhone reports a focus mode such as sleep or driving → hold, re-check
//!    every tick (the next tick after he is back is the good moment);
//! 5. "Non ora" on this kind → hold until it runs out;
//! 6. per-kind cooldown → Pulse;
//! 7. daily budget (`budget.max_per_day` unsolicited messages) → Pulse;
//! 8. send.

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::config::Config;
use crate::model::Kind;

/// `HH:MM` → minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub fn local_minutes(now: DateTime<Utc>, tz: Tz) -> u32 {
    let t = now.with_timezone(&tz);
    t.hour() * 60 + t.minute()
}

/// The UTC instant of local `minutes` on the local day of `now` plus `days`.
pub fn local_at(now: DateTime<Utc>, tz: Tz, days: i64, minutes: u32) -> DateTime<Utc> {
    let date = now.with_timezone(&tz).date_naive() + Duration::days(days);
    let time = NaiveTime::from_hms_opt(minutes / 60, minutes % 60, 0).unwrap_or_default();
    match tz.from_local_datetime(&date.and_time(time)) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
            t.with_timezone(&Utc)
        }
        // A DST gap: an hour later exists.
        chrono::LocalResult::None => tz
            .from_local_datetime(&(date.and_time(time) + Duration::hours(1)))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(now),
    }
}

/// When the quiet window `start`–`end` (local, may wrap midnight) that
/// contains `now` ends, or `None` when `now` is outside it.
pub fn quiet_until(now: DateTime<Utc>, tz: Tz, start: u32, end: u32) -> Option<DateTime<Utc>> {
    let m = local_minutes(now, tz);
    let inside = match start.cmp(&end) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => m >= start && m < end,
        std::cmp::Ordering::Greater => m >= start || m < end,
    };
    if !inside {
        return None;
    }
    let days = i64::from(m >= end);
    Some(local_at(now, tz, days, end))
}

/// Quiet hours and Do Not Disturb right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quiet {
    pub quiet: bool,
    pub until: Option<DateTime<Utc>>,
    /// `server` or `fallback`.
    pub source: String,
    /// The server's notification language (`it`, `en`, `da`), if known.
    pub language: Option<String>,
    /// `lucina_message` pushes are switched off in the app.
    pub app_messages_off: bool,
}

impl Quiet {
    /// From the `[quiet]` fallback.
    pub fn fallback(cfg: &Config, now: DateTime<Utc>) -> Self {
        let q = &cfg.quiet;
        let until = match (q.enabled, parse_hhmm(&q.start), parse_hhmm(&q.end)) {
            (true, Some(s), Some(e)) => quiet_until(now, cfg.tz(), s, e),
            _ => None,
        };
        Self {
            quiet: until.is_some(),
            until,
            source: "fallback".into(),
            language: None,
            app_messages_off: false,
        }
    }

    /// From secretary-server's notification settings (`GET /v1/notify/settings`
    /// or the device route's `settings` object).
    pub fn from_settings(v: &serde_json::Value, now: DateTime<Utc>) -> Option<Self> {
        let s = v.get("settings").unwrap_or(v);
        let tz: Tz = s.get("timeZone")?.as_str()?.parse().ok()?;
        let mut until = None;
        if let Some(dnd) = s.get("dndUntil").and_then(serde_json::Value::as_i64) {
            if dnd > now.timestamp_millis() {
                until = DateTime::from_timestamp_millis(dnd);
            }
        }
        let qh = s.get("quietHours");
        let enabled = qh
            .and_then(|q| q.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if enabled {
            let start = qh
                .and_then(|q| q.get("start"))
                .and_then(|v| v.as_str())
                .and_then(parse_hhmm);
            let end = qh
                .and_then(|q| q.get("end"))
                .and_then(|v| v.as_str())
                .and_then(parse_hhmm);
            if let (Some(st), Some(en)) = (start, end) {
                if let Some(q) = quiet_until(now, tz, st, en) {
                    until = Some(until.map_or(q, |u: DateTime<Utc>| u.max(q)));
                }
            }
        }
        Some(Self {
            quiet: until.is_some(),
            until,
            source: "server".into(),
            language: s
                .get("language")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            app_messages_off: s
                .get("categories")
                .and_then(|c| c.get("lucina_message"))
                .and_then(serde_json::Value::as_bool)
                == Some(false),
        })
    }
}

/// The owner's Discord presence (from the presence watcher's files).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Presence {
    /// `online`, `idle`, `dnd`, `offline`, or empty when unknown.
    pub status: String,
    /// The game he is playing, if any.
    pub gaming: Option<String>,
    /// He came online at (from the history), if known.
    pub online_since: Option<DateTime<Utc>>,
}

/// What the iPhone reports (`device-context.jsonl`), if anything.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Device {
    pub focus: Option<String>,
}

/// Focus modes that mean "do not interrupt".
pub const HOLD_FOCUS: [&str; 6] = [
    "sleep",
    "driving",
    "dnd",
    "do_not_disturb",
    "donotdisturb",
    "fitness",
];

/// What the engine's store says about this kind.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ledger {
    /// Unsolicited messages sent today (local day).
    pub sent_today: u32,
    pub last_sent_of_kind: Option<DateTime<Utc>>,
    pub not_now_until: Option<DateTime<Utc>>,
    pub muted_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Send,
    Hold {
        until: DateTime<Utc>,
        reason: String,
    },
    Digest {
        reason: String,
    },
    Drop {
        reason: String,
    },
}

/// How long a presence or focus hold waits before the next look.
pub const RECHECK: i64 = 5;

fn hhmm(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%H:%M").to_string()
}

pub struct GateInput<'a> {
    pub kind: Kind,
    pub expires_at: Option<DateTime<Utc>>,
    pub quiet: &'a Quiet,
    pub presence: &'a Presence,
    pub device: &'a Device,
    pub ledger: &'a Ledger,
}

pub fn gate(cfg: &Config, now: DateTime<Utc>, g: &GateInput<'_>) -> Gate {
    let tz = cfg.tz();
    let expired = |t: DateTime<Utc>| g.expires_at.is_some_and(|e| e <= t);
    // Hold until `until`, or to the Pulse when it would expire first.
    let hold = |until: DateTime<Utc>, reason: String| {
        if expired(until) {
            Gate::Digest {
                reason: format!("{reason}; it would be stale by then"),
            }
        } else {
            Gate::Hold { until, reason }
        }
    };
    if expired(now) {
        return Gate::Drop {
            reason: "expired before it could be sent".into(),
        };
    }
    if let Some(m) = g.ledger.muted_until.filter(|m| *m > now) {
        return Gate::Drop {
            reason: format!(
                "muted: \"Basta così\" on {} until {}",
                g.kind,
                m.with_timezone(&tz).format("%Y-%m-%d")
            ),
        };
    }
    if g.quiet.quiet {
        let until = g.quiet.until.unwrap_or(now + Duration::minutes(30));
        return hold(
            until,
            format!(
                "quiet hours / Do Not Disturb ({}) until {}",
                g.quiet.source,
                hhmm(until, tz)
            ),
        );
    }
    if cfg.budget.hold_while_gaming {
        if let Some(game) = &g.presence.gaming {
            return hold(
                now + Duration::minutes(RECHECK),
                format!("he is playing {game} (Discord): not interrupting"),
            );
        }
    }
    if cfg.budget.hold_on_discord_dnd && g.presence.status == "dnd" {
        return hold(
            now + Duration::minutes(RECHECK),
            "Discord status is Do Not Disturb".into(),
        );
    }
    if let Some(f) = &g.device.focus {
        if HOLD_FOCUS.contains(&f.to_ascii_lowercase().as_str()) {
            return hold(
                now + Duration::minutes(RECHECK),
                format!("iPhone focus mode {f}"),
            );
        }
    }
    if let Some(u) = g.ledger.not_now_until.filter(|u| *u > now) {
        return hold(
            u,
            format!("\"Non ora\" on {} until {}", g.kind, hhmm(u, tz)),
        );
    }
    if let Some(last) = g.ledger.last_sent_of_kind {
        let cd = cfg.budget.cooldown(g.kind.as_str());
        if last + cd > now {
            return Gate::Digest {
                reason: format!(
                    "cooldown: a {} message went out at {} ({} min cooldown)",
                    g.kind,
                    hhmm(last, tz),
                    cd.num_minutes()
                ),
            };
        }
    }
    if g.ledger.sent_today >= cfg.budget.max_per_day {
        return Gate::Digest {
            reason: format!(
                "daily budget used: {} of {} unsolicited messages today",
                g.ledger.sent_today, cfg.budget.max_per_day
            ),
        };
    }
    Gate::Send
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn input<'a>(
        quiet: &'a Quiet,
        presence: &'a Presence,
        device: &'a Device,
        ledger: &'a Ledger,
    ) -> GateInput<'a> {
        GateInput {
            kind: Kind::MeetingPrep,
            expires_at: None,
            quiet,
            presence,
            device,
            ledger,
        }
    }

    #[test]
    fn quiet_window_wraps_midnight_and_dst() {
        let tz = chrono_tz::Europe::Copenhagen;
        let (s, e) = (parse_hhmm("22:30").unwrap(), parse_hhmm("07:30").unwrap());
        // 23:00 local (CEST, +2) → ends 07:30 next day.
        let u = quiet_until(at("2026-10-22T21:00:00Z"), tz, s, e).unwrap();
        assert_eq!(u, at("2026-10-23T05:30:00Z"));
        // 06:00 local → ends 07:30 the same day.
        assert_eq!(
            quiet_until(at("2026-10-22T04:00:00Z"), tz, s, e).unwrap(),
            at("2026-10-22T05:30:00Z")
        );
        assert_eq!(quiet_until(at("2026-10-22T10:00:00Z"), tz, s, e), None);
        // Night of the switch to CET (25 Oct): 07:30 CET = 06:30 UTC.
        assert_eq!(
            quiet_until(at("2026-10-24T22:00:00Z"), tz, s, e).unwrap(),
            at("2026-10-25T06:30:00Z")
        );
    }

    #[test]
    fn server_settings_dnd_and_quiet_hours() {
        let now = at("2026-10-22T21:00:00Z");
        let v = json!({"settings": {"timeZone": "Europe/Copenhagen", "quietHours": {"enabled": true, "start": "22:30", "end": "07:30"}, "dndUntil": null, "language": "it", "categories": {"lucina_message": true}}});
        let q = Quiet::from_settings(&v, now).unwrap();
        assert!(q.quiet && q.source == "server");
        assert_eq!(q.until, Some(at("2026-10-23T05:30:00Z")));
        let v = json!({"timeZone": "Europe/Copenhagen", "quietHours": {"enabled": false}, "dndUntil": now.timestamp_millis() + 60_000});
        let q = Quiet::from_settings(&v, now).unwrap();
        assert!(q.quiet);
        let v = json!({"timeZone": "Europe/Copenhagen", "quietHours": {"enabled": false}, "dndUntil": now.timestamp_millis() - 1});
        assert!(!Quiet::from_settings(&v, now).unwrap().quiet);
        assert!(Quiet::from_settings(&json!({}), now).is_none());
    }

    #[test]
    fn gate_order() {
        let cfg = Config::default();
        let now = at("2026-10-22T10:00:00Z");
        let calm = Quiet::default();
        let none = Presence::default();
        let dev = Device::default();
        let fresh = Ledger::default();
        assert_eq!(
            gate(&cfg, now, &input(&calm, &none, &dev, &fresh)),
            Gate::Send
        );

        let quiet = Quiet {
            quiet: true,
            until: Some(now + Duration::hours(2)),
            source: "server".into(),
            ..Quiet::default()
        };
        assert!(
            matches!(gate(&cfg, now, &input(&quiet, &none, &dev, &fresh)), Gate::Hold { until, .. } if until == now + Duration::hours(2))
        );
        // ...but if it expires before quiet hours end, it goes to the Pulse.
        let mut i = input(&quiet, &none, &dev, &fresh);
        i.expires_at = Some(now + Duration::hours(1));
        assert!(matches!(gate(&cfg, now, &i), Gate::Digest { .. }));
        i.expires_at = Some(now);
        assert!(matches!(gate(&cfg, now, &i), Gate::Drop { .. }));

        let gaming = Presence {
            status: "online".into(),
            gaming: Some("League of Legends".into()),
            online_since: None,
        };
        assert!(
            matches!(gate(&cfg, now, &input(&calm, &gaming, &dev, &fresh)), Gate::Hold { reason, .. } if reason.contains("League"))
        );
        let dnd = Presence {
            status: "dnd".into(),
            ..Presence::default()
        };
        assert!(matches!(
            gate(&cfg, now, &input(&calm, &dnd, &dev, &fresh)),
            Gate::Hold { .. }
        ));
        let sleep = Device {
            focus: Some("Sleep".into()),
        };
        assert!(matches!(
            gate(&cfg, now, &input(&calm, &none, &sleep, &fresh)),
            Gate::Hold { .. }
        ));

        let muted = Ledger {
            muted_until: Some(now + Duration::days(3)),
            ..Ledger::default()
        };
        // Muted wins over everything but expiry.
        assert!(matches!(
            gate(&cfg, now, &input(&quiet, &gaming, &dev, &muted)),
            Gate::Drop { .. }
        ));
        let not_now = Ledger {
            not_now_until: Some(now + Duration::hours(6)),
            ..Ledger::default()
        };
        assert!(matches!(
            gate(&cfg, now, &input(&calm, &none, &dev, &not_now)),
            Gate::Hold { .. }
        ));
        let cooling = Ledger {
            last_sent_of_kind: Some(now - Duration::minutes(10)),
            ..Ledger::default()
        };
        assert!(
            matches!(gate(&cfg, now, &input(&calm, &none, &dev, &cooling)), Gate::Digest { reason } if reason.starts_with("cooldown"))
        );
        let spent = Ledger {
            sent_today: 3,
            ..Ledger::default()
        };
        assert!(
            matches!(gate(&cfg, now, &input(&calm, &none, &dev, &spent)), Gate::Digest { reason } if reason.starts_with("daily budget"))
        );
        let two = Ledger {
            sent_today: 2,
            last_sent_of_kind: Some(now - Duration::hours(3)),
            ..Ledger::default()
        };
        assert_eq!(
            gate(&cfg, now, &input(&calm, &none, &dev, &two)),
            Gate::Send
        );
    }
}

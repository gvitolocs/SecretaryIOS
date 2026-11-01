//! Notification settings: per-category switches, quiet hours, Do Not
//! Disturb, the calendar lead time, previews and per-device overrides.
//! Stored as `notification-settings.json` (0600) in the data dir.

use std::collections::BTreeMap;

use chrono::{TimeZone, Timelike};
use serde::{Deserialize, Serialize};

use super::model::Category;

pub const DEFAULT_TIME_ZONE: &str = "Europe/Rome";
pub const MAX_LEAD_MINUTES: u32 = 24 * 60;
/// At most this many per-device entries.
pub const MAX_DEVICES: usize = 64;

fn yes() -> bool {
    true
}
fn no() -> bool {
    false
}

/// One switch per category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Categories {
    #[serde(default = "yes")]
    pub lucina_message: bool,
    #[serde(default = "yes")]
    pub reminder: bool,
    #[serde(default = "yes")]
    pub calendar: bool,
    #[serde(default = "yes")]
    pub device_confirm: bool,
    #[serde(default = "yes")]
    pub agent: bool,
    #[serde(default = "yes")]
    pub shopping: bool,
    #[serde(default = "no")]
    pub presence: bool,
    #[serde(default = "no")]
    pub system: bool,
}

impl Default for Categories {
    fn default() -> Self {
        Self {
            lucina_message: true,
            reminder: true,
            calendar: true,
            device_confirm: true,
            agent: true,
            shopping: true,
            presence: false,
            system: false,
        }
    }
}

impl Categories {
    pub fn get(&self, c: Category) -> bool {
        match c {
            Category::LucinaMessage => self.lucina_message,
            Category::Reminder => self.reminder,
            Category::Calendar => self.calendar,
            Category::DeviceConfirm => self.device_confirm,
            Category::Agent => self.agent,
            Category::Shopping => self.shopping,
            Category::Presence => self.presence,
            Category::System => self.system,
            // No switch: approvals are always on.
            Category::Approval => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietHours {
    #[serde(default)]
    pub enabled: bool,
    /// `HH:MM`, local time in [`Settings::time_zone`].
    #[serde(default = "QuietHours::default_start")]
    pub start: String,
    #[serde(default = "QuietHours::default_end")]
    pub end: String,
}

impl QuietHours {
    fn default_start() -> String {
        "22:30".into()
    }
    fn default_end() -> String {
        "07:30".into()
    }
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: Self::default_start(),
            end: Self::default_end(),
        }
    }
}

/// Overrides for one device: a Web Push subscription (`ps_…`) or a native
/// app (its device id, `dev_…`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePrefs {
    /// `false` mutes the device entirely.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Category name → on/off; missing means "as the global switch".
    #[serde(default)]
    pub categories: BTreeMap<String, bool>,
}

impl Default for DevicePrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            categories: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub categories: Categories,
    #[serde(default)]
    pub quiet_hours: QuietHours,
    /// IANA zone for quiet hours and calendar times.
    #[serde(default = "Settings::default_tz")]
    pub time_zone: String,
    /// Do Not Disturb until this epoch ms (`null`: off).
    #[serde(default)]
    pub dnd_until: Option<i64>,
    /// Calendar notifications this many minutes before the event.
    #[serde(default = "Settings::default_lead")]
    pub calendar_lead_minutes: u32,
    /// Reminders still push during quiet hours and DND.
    #[serde(default = "yes")]
    pub reminders_bypass_quiet: bool,
    /// After quiet hours / DND, one push sums up what was held back.
    #[serde(default = "yes")]
    pub digest: bool,
    /// Show message text in pushes (`false`: "Open Secretary to read it").
    #[serde(default = "yes")]
    pub previews: bool,
    /// Language of the server's own notices (`en`, `it`, `da`); clients
    /// set it to theirs when they save.
    #[serde(default = "Settings::default_language")]
    pub language: String,
    #[serde(default)]
    pub devices: BTreeMap<String, DevicePrefs>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            categories: Categories::default(),
            quiet_hours: QuietHours::default(),
            time_zone: Self::default_tz(),
            dnd_until: None,
            calendar_lead_minutes: Self::default_lead(),
            reminders_bypass_quiet: true,
            digest: true,
            previews: true,
            language: Self::default_language(),
            devices: BTreeMap::new(),
        }
    }
}

/// `HH:MM` → minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl Settings {
    fn default_tz() -> String {
        DEFAULT_TIME_ZONE.into()
    }
    fn default_lead() -> u32 {
        10
    }
    fn default_language() -> String {
        "en".into()
    }

    /// Check what a client sent; the error names the field.
    pub fn validate(&self) -> Result<(), String> {
        if self.time_zone.parse::<chrono_tz::Tz>().is_err() {
            return Err("timeZone: unknown IANA time zone".into());
        }
        if parse_hhmm(&self.quiet_hours.start).is_none() {
            return Err("quietHours.start: expected HH:MM".into());
        }
        if parse_hhmm(&self.quiet_hours.end).is_none() {
            return Err("quietHours.end: expected HH:MM".into());
        }
        if self.calendar_lead_minutes > MAX_LEAD_MINUTES {
            return Err(format!("calendarLeadMinutes: at most {MAX_LEAD_MINUTES}"));
        }
        if !matches!(self.language.as_str(), "en" | "it" | "da") {
            return Err("language: one of en, it, da".into());
        }
        if self.devices.len() > MAX_DEVICES {
            return Err(format!("devices: at most {MAX_DEVICES} entries"));
        }
        for (id, prefs) in &self.devices {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err("devices: keys are device or subscription ids".into());
            }
            if let Some(bad) = prefs
                .categories
                .keys()
                .find(|k| Category::parse(k).is_none())
            {
                return Err(format!("devices.{id}.categories: unknown category {bad}"));
            }
        }
        Ok(())
    }

    pub fn tz(&self) -> chrono_tz::Tz {
        self.time_zone.parse().unwrap_or(chrono_tz::Europe::Rome)
    }

    /// Minutes after local midnight at `now_ms`.
    pub fn local_minutes(&self, now_ms: i64) -> u32 {
        match self.tz().timestamp_millis_opt(now_ms) {
            chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
                t.hour() * 60 + t.minute()
            }
            chrono::LocalResult::None => 0,
        }
    }

    /// `HH:MM` local time of `ms`.
    pub fn local_hhmm(&self, ms: i64) -> String {
        let m = self.local_minutes(ms);
        format!("{:02}:{:02}", m / 60, m % 60)
    }

    /// Inside quiet hours or Do Not Disturb at `now_ms`.
    pub fn quiet_at(&self, now_ms: i64) -> bool {
        if self.dnd_until.is_some_and(|until| until > now_ms) {
            return true;
        }
        if !self.quiet_hours.enabled {
            return false;
        }
        let (Some(start), Some(end)) = (
            parse_hhmm(&self.quiet_hours.start),
            parse_hhmm(&self.quiet_hours.end),
        ) else {
            return false;
        };
        let now = self.local_minutes(now_ms);
        match start.cmp(&end) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => now >= start && now < end,
            // Wraps midnight (22:30 → 07:30).
            std::cmp::Ordering::Greater => now >= start || now < end,
        }
    }

    /// Whether a push of `category` may go to device / subscription `target`.
    pub fn allows(&self, category: Category, target: &str) -> bool {
        if category.always_on() {
            return true;
        }
        if !self.categories.get(category) {
            return false;
        }
        match self.devices.get(target) {
            None => true,
            Some(p) => p.enabled && p.categories.get(category.as_str()).copied().unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-22T21:00:00Z = 23:00 in Rome (CEST, +02:00).
    const T23: i64 = 1_792_702_800_000;

    #[test]
    fn defaults_and_partial_json() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert!(s.categories.lucina_message && !s.categories.presence);
        let s: Settings =
            serde_json::from_str(r#"{"categories":{"shopping":false},"calendarLeadMinutes":30}"#)
                .unwrap();
        assert!(!s.categories.shopping && s.categories.reminder);
        assert_eq!(s.calendar_lead_minutes, 30);
        assert!(s.validate().is_ok());
        let v = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(v["categories"]["lucina_message"], true);
        assert_eq!(v["quietHours"]["start"], "22:30");
    }

    #[test]
    fn validation() {
        let s = Settings {
            time_zone: "Mars/Olympus".into(),
            ..Settings::default()
        };
        assert!(s.validate().unwrap_err().starts_with("timeZone"));
        let mut s = Settings::default();
        s.quiet_hours.start = "25:00".into();
        assert!(s.validate().unwrap_err().starts_with("quietHours.start"));
        let s = Settings {
            calendar_lead_minutes: 5000,
            ..Settings::default()
        };
        assert!(s.validate().is_err());
        let mut s = Settings::default();
        s.devices.insert(
            "ps_x".into(),
            DevicePrefs {
                enabled: true,
                categories: [("weather".to_owned(), true)].into(),
            },
        );
        assert!(s.validate().unwrap_err().contains("weather"));
        let mut s = Settings::default();
        s.devices.insert("../x".into(), DevicePrefs::default());
        assert!(s.validate().is_err());
        assert_eq!(parse_hhmm("07:05"), Some(425));
        assert_eq!(parse_hhmm("7:05"), None);
    }

    #[test]
    fn quiet_hours_wrap_midnight_and_dnd() {
        let mut s = Settings::default();
        assert!(!s.quiet_at(T23));
        s.quiet_hours.enabled = true; // 22:30 → 07:30
        assert_eq!(s.local_hhmm(T23), "23:00");
        assert!(s.quiet_at(T23));
        assert!(s.quiet_at(T23 + 8 * 3_600_000)); // 07:00
        assert!(!s.quiet_at(T23 + 9 * 3_600_000)); // 08:00
        assert!(!s.quiet_at(T23 - 3_600_000)); // 22:00
        s.quiet_hours.start = "09:00".into();
        s.quiet_hours.end = "17:00".into();
        assert!(!s.quiet_at(T23));
        assert!(s.quiet_at(T23 + 12 * 3_600_000)); // 11:00
        s.quiet_hours.enabled = false;
        s.dnd_until = Some(T23 + 60_000);
        assert!(s.quiet_at(T23));
        assert!(!s.quiet_at(T23 + 60_000));
        s.time_zone = "America/New_York".into();
        assert_eq!(s.local_hhmm(T23), "17:00");
    }

    #[test]
    fn per_device_overrides() {
        let mut s = Settings::default();
        assert!(s.allows(Category::Calendar, "ps_a"));
        assert!(!s.allows(Category::System, "ps_a"));
        s.devices.insert(
            "ps_a".into(),
            DevicePrefs {
                enabled: true,
                categories: [("calendar".to_owned(), false)].into(),
            },
        );
        s.devices.insert(
            "ps_b".into(),
            DevicePrefs {
                enabled: false,
                categories: BTreeMap::new(),
            },
        );
        assert!(!s.allows(Category::Calendar, "ps_a"));
        assert!(s.allows(Category::Reminder, "ps_a"));
        assert!(!s.allows(Category::Reminder, "ps_b"));
        s.categories.reminder = false;
        assert!(!s.allows(Category::Reminder, "ps_c"));
        // Approvals ignore every switch, global and per device.
        s.devices.insert(
            "ps_a".into(),
            DevicePrefs {
                enabled: false,
                categories: [("approval".to_owned(), false)].into(),
            },
        );
        assert!(s.allows(Category::Approval, "ps_a"));
    }
}

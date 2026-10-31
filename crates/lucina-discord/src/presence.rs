//! The owner's presence and the mapping from Discord presence payloads.

use std::borrow::Cow;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Longest string kept from an activity field (Discord allows 128).
pub const MAX_FIELD_CHARS: usize = 128;
/// At most this many activities are kept.
pub const MAX_ACTIVITIES: usize = 8;

/// `clientStatus`: the status per platform, `null` when not connected there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStatus {
    pub desktop: Option<String>,
    pub mobile: Option<String>,
    pub web: Option<String>,
}

impl ClientStatus {
    /// Connected platforms, in a fixed order.
    pub fn platforms(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (name, v) in [
            ("desktop", &self.desktop),
            ("mobile", &self.mobile),
            ("web", &self.web),
        ] {
            if v.as_deref().is_some_and(|s| s != "offline") {
                out.push(name);
            }
        }
        out
    }
}

/// One activity: `type` is playing | streaming | listening | watching |
/// custom | competing | other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub details: Option<String>,
    pub state: Option<String>,
    pub started_at: Option<String>,
}

impl Activity {
    /// Identity for start/stop history: a song change inside Spotify or a new
    /// "details" line in a game is the same activity; a different custom
    /// status text is a new one.
    pub fn key(&self) -> (String, String, Option<String>) {
        let extra = (self.kind == "custom").then(|| self.state.clone().unwrap_or_default());
        (self.kind.clone(), self.name.clone(), extra)
    }
}

/// What one presence payload says about the owner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observed {
    pub status: String,
    pub client_status: ClientStatus,
    pub activities: Vec<Activity>,
}

impl Observed {
    pub fn offline() -> Self {
        Self {
            status: "offline".into(),
            ..Self::default()
        }
    }
}

/// `data/presence/discord.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceState {
    /// online | idle | dnd | offline (invisible arrives as offline).
    pub status: String,
    pub client_status: ClientStatus,
    pub activities: Vec<Activity>,
    /// When the current status began (as far as the watcher knows).
    pub since: String,
    /// Last write by the watcher (refreshed every minute while it runs).
    pub updated_at: String,
}

pub fn iso(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

// ---------------------------------------------------------------- raw payloads

/// Just enough of a presence (or member) object to read the user id. Every
/// other field is skipped unparsed.
#[derive(Deserialize)]
pub struct PeekUser<'a> {
    #[serde(borrow)]
    pub user: Option<UserRef<'a>>,
}

#[derive(Deserialize)]
pub struct UserRef<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
}

/// The fields of a presence payload that are kept (owner only).
#[derive(Deserialize, Default)]
pub struct RawPresence {
    pub status: Option<String>,
    #[serde(default)]
    pub activities: Vec<RawActivity>,
    #[serde(default)]
    pub client_status: Option<ClientStatus>,
}

#[derive(Deserialize)]
pub struct RawActivity {
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub timestamps: Option<RawTimestamps>,
    #[serde(default)]
    pub emoji: Option<RawEmoji>,
}

#[derive(Deserialize)]
pub struct RawTimestamps {
    pub start: Option<i64>,
}

#[derive(Deserialize)]
pub struct RawEmoji {
    pub name: Option<String>,
}

fn clip(s: Option<&str>) -> Option<String> {
    let t = s?.trim();
    if t.is_empty() {
        return None;
    }
    let mut out: String = t.chars().take(MAX_FIELD_CHARS).collect();
    if t.chars().count() > MAX_FIELD_CHARS {
        out.push('…');
    }
    Some(out)
}

fn kind_name(kind: u8) -> &'static str {
    match kind {
        0 => "playing",
        1 => "streaming",
        2 => "listening",
        3 => "watching",
        4 => "custom",
        5 => "competing",
        _ => "other",
    }
}

/// Discord activity -> Lucina activity. `None` for an empty custom status.
pub fn map_activity(a: &RawActivity) -> Option<Activity> {
    let kind = kind_name(a.kind);
    let started_at = a
        .timestamps
        .as_ref()
        .and_then(|t| t.start)
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .map(iso);
    if kind == "custom" {
        let emoji = a
            .emoji
            .as_ref()
            .and_then(|e| e.name.as_deref())
            .map(str::trim)
            .filter(|e| !e.is_empty());
        let text = clip(a.state.as_deref());
        let state = match (emoji, text) {
            (Some(e), Some(t)) => Some(format!("{e} {t}")),
            (Some(e), None) => Some(e.to_owned()),
            (None, t) => t,
        };
        state.as_ref()?;
        return Some(Activity {
            kind: kind.into(),
            name: "Custom Status".into(),
            details: None,
            state,
            started_at: None,
        });
    }
    let name = clip(a.name.as_deref()).unwrap_or_else(|| "Unknown".into());
    let mut state = clip(a.state.as_deref());
    if kind == "listening" && name == "Spotify" {
        // Spotify: details = song, state = artists separated by ';'.
        state = state.map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        });
    }
    Some(Activity {
        kind: kind.into(),
        name,
        details: clip(a.details.as_deref()),
        state,
        started_at,
    })
}

/// Discord presence -> what it says about the owner.
pub fn map_presence(p: &RawPresence) -> Observed {
    let status = match p.status.as_deref().map(str::trim) {
        Some(s @ ("online" | "idle" | "dnd" | "offline")) => s.to_owned(),
        // "invisible" is never sent for other users; treat anything odd as offline.
        _ => "offline".into(),
    };
    let mut cs = p.client_status.clone().unwrap_or_default();
    let norm = |v: Option<String>| v.filter(|s| !s.trim().is_empty() && s != "offline");
    cs = ClientStatus {
        desktop: norm(cs.desktop.take()),
        mobile: norm(cs.mobile.take()),
        web: norm(cs.web.take()),
    };
    if status == "offline" {
        return Observed::offline();
    }
    let activities = p
        .activities
        .iter()
        .filter_map(map_activity)
        .take(MAX_ACTIVITIES)
        .collect();
    Observed {
        status,
        client_status: cs,
        activities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(v: serde_json::Value) -> RawPresence {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn spotify_game_custom() {
        let o = map_presence(&raw(json!({
            "user": {"id": "1"}, "status": "dnd",
            "client_status": {"desktop": "dnd", "mobile": "idle"},
            "activities": [
                {"type": 4, "name": "Custom Status", "state": "coding", "emoji": {"name": "🔥"}, "created_at": 1},
                {"type": 2, "name": "Spotify", "details": "Song A", "state": "Artist 1; Artist 2",
                 "timestamps": {"start": 1_761_160_200_000i64, "end": 1_761_160_400_000i64}, "sync_id": "x", "party": {"id": "spotify:1"}},
                {"type": 0, "name": "Elden Ring", "details": "Limgrave", "timestamps": {"start": 1_761_156_600_000i64}, "application_id": "9"}
            ]
        })));
        assert_eq!(o.status, "dnd");
        assert_eq!(o.client_status.platforms(), vec!["desktop", "mobile"]);
        assert_eq!(o.activities.len(), 3);
        assert_eq!(o.activities[0].kind, "custom");
        assert_eq!(o.activities[0].state.as_deref(), Some("🔥 coding"));
        let s = &o.activities[1];
        assert_eq!(
            (
                s.kind.as_str(),
                s.name.as_str(),
                s.details.as_deref(),
                s.state.as_deref()
            ),
            (
                "listening",
                "Spotify",
                Some("Song A"),
                Some("Artist 1, Artist 2")
            )
        );
        assert_eq!(s.started_at.as_deref(), Some("2025-10-22T19:10:00.000Z"));
        assert_eq!(o.activities[2].kind, "playing");
        assert_eq!(o.activities[2].name, "Elden Ring");
    }

    #[test]
    fn offline_drops_everything_and_odd_values() {
        let o = map_presence(&raw(
            json!({"status": "offline", "activities": [{"type": 0, "name": "x"}]}),
        ));
        assert_eq!(o, Observed::offline());
        let o = map_presence(&raw(json!({"status": "invisible"})));
        assert_eq!(o.status, "offline");
        // empty custom status is no activity; long fields are clipped
        let long = "x".repeat(300);
        let o = map_presence(&raw(
            json!({"status": "online", "activities": [{"type": 4, "name": "Custom Status"}, {"type": 3, "name": long}]}),
        ));
        assert_eq!(o.activities.len(), 1);
        assert_eq!(o.activities[0].name.chars().count(), MAX_FIELD_CHARS + 1);
        assert_eq!(o.activities[0].kind, "watching");
    }
}

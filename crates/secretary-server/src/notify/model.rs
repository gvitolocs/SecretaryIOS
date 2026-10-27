//! Notification types shared by the inbox, the routes and the push adapters.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a notification is about. The wire form is snake_case
/// (`lucina_message`, `device_confirm`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// A reply or a proactive message from Lucina, on any channel, while the
    /// owner is not looking at the app.
    LucinaMessage,
    /// A Lucina reminder became due.
    Reminder,
    /// A calendar event starts soon (from the phone's device state).
    Calendar,
    /// A device command waits for the owner's tap.
    DeviceConfirm,
    /// A coding agent finished, needs permission, or something asks the owner's OK
    /// (lucina-agentd; pushed only while no app is in front).
    Agent,
    /// A shopping list changed, by someone or something other than the owner.
    Shopping,
    /// Lucina went offline or came back (off by default).
    Presence,
    /// Security and server notices, e.g. a new device signed in (off by default).
    System,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::LucinaMessage,
        Category::Reminder,
        Category::Calendar,
        Category::DeviceConfirm,
        Category::Agent,
        Category::Shopping,
        Category::Presence,
        Category::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::LucinaMessage => "lucina_message",
            Category::Reminder => "reminder",
            Category::Calendar => "calendar",
            Category::DeviceConfirm => "device_confirm",
            Category::Agent => "agent",
            Category::Shopping => "shopping",
            Category::Presence => "presence",
            Category::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// On unless the owner turns it on (presence, system) or off.
    pub fn default_on(self) -> bool {
        !matches!(self, Category::Presence | Category::System)
    }

    /// The web route a click opens when the notification names none.
    pub fn default_route(self) -> &'static str {
        match self {
            Category::Shopping => "/shopping",
            _ => "/",
        }
    }

    /// Time-sensitive categories get `Urgency: high` (Web Push) and priority 10 (APNs).
    pub fn urgent(self) -> bool {
        matches!(
            self,
            Category::Reminder | Category::Calendar | Category::DeviceConfirm | Category::Agent
        )
    }

    /// A `dedupKey` names a slot instead of a one-off: a newer notification
    /// with the same key replaces the older one (inbox entry and the shown
    /// notification) instead of being dropped as a duplicate. Host alerts
    /// use this: "nezopt: x failed" then "nezopt: x recovered".
    pub fn replaces_by_key(self) -> bool {
        matches!(self, Category::System)
    }

    /// Push "topic" / notification tag: a newer push of the same topic
    /// replaces an undelivered or still-shown older one, so a burst of
    /// Lucina messages ends up as one entry on the lock screen.
    pub fn collapse_key(self) -> Option<&'static str> {
        match self {
            Category::LucinaMessage => Some("lucina"),
            Category::Shopping => Some("shopping"),
            Category::Presence => Some("presence"),
            _ => None,
        }
    }
}

/// One inbox entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// `n_` + 13-digit epoch ms + 8 hex: sortable by time.
    pub id: String,
    /// Creation time, epoch ms.
    pub ts: i64,
    pub category: Category,
    /// A custom title (from `POST /v1/notify`); clients title the rest by category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub body: String,
    /// Web route to open (`/`, `/shopping`, …); the iOS app maps it to a tab.
    pub route: String,
    /// Timeline channel for Lucina items (`telegram`, `web`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// Category-specific extras (`{start}` for calendar, `{commandId}` …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default)]
    pub read: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
    /// Held back by quiet hours / DND, waiting for the digest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}

/// A notification before it gets an id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewNotification {
    pub category: Option<Category>,
    pub title: Option<String>,
    pub body: String,
    pub route: Option<String>,
    pub channel: Option<String>,
    pub timeline_id: Option<String>,
    pub dedup_key: Option<String>,
    pub data: Option<Value>,
}

impl NewNotification {
    pub fn new(category: Category, body: impl Into<String>) -> Self {
        Self {
            category: Some(category),
            body: body.into(),
            ..Self::default()
        }
    }

    /// Every key this notification is known by: the explicit dedup key and
    /// the timeline item (`tl:<id>`), so a reminder that reaches the server
    /// both from the timeline and from Lucina's `/v1/notify` shows once.
    pub fn keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        if let Some(k) = &self.dedup_key {
            keys.push(k.clone());
        }
        if let Some(t) = &self.timeline_id {
            keys.push(format!("tl:{t}"));
        }
        keys
    }
}

/// What happened to a published notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Stored in the inbox; `pushed` targets were sent a push (or queued).
    Created {
        notification: Box<Notification>,
        pushed: usize,
        deferred: bool,
    },
    /// Seen before (same dedup key or timeline item).
    Duplicate,
    /// The category is off.
    Disabled,
    /// A Lucina message while the owner is looking at the app.
    Present,
}

impl Outcome {
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Created { .. } => "created",
            Outcome::Duplicate => "duplicate",
            Outcome::Disabled => "disabled",
            Outcome::Present => "present",
        }
    }
}

/// Trim to at most `max` characters (by `char`), adding `…` when cut.
pub fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `n_<13-digit ms><8 hex>`.
pub fn new_id(now: i64) -> String {
    let mut r = [0u8; 4];
    let _ = getrandom::fill(&mut r);
    format!(
        "n_{:013}{:02x}{:02x}{:02x}{:02x}",
        now.max(0),
        r[0],
        r[1],
        r[2],
        r[3]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_wire_names_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
            let json = serde_json::to_value(c).unwrap();
            assert_eq!(json, c.as_str());
        }
        assert_eq!(Category::parse("nope"), None);
        assert!(!Category::Presence.default_on() && !Category::System.default_on());
        assert!(Category::LucinaMessage.default_on());
    }

    #[test]
    fn clip_and_ids() {
        assert_eq!(clip("  ciao  ", 10), "ciao");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("èèèèè", 5), "èèèèè");
        let a = new_id(1_761_048_000_000);
        assert!(a.starts_with("n_1761048000000") && a.len() == 2 + 13 + 8);
        assert!(new_id(1) < new_id(2));
        let mut n = NewNotification::new(Category::Reminder, "x");
        n.timeline_id = Some("01J".into());
        n.dedup_key = Some("reminder:1".into());
        assert_eq!(n.keys(), vec!["reminder:1".to_owned(), "tl:01J".to_owned()]);
    }
}

//! The stored report (`report.json`) and its index summary.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which client sent the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppName {
    SecretaryCode,
    Ios,
    Android,
    Web,
    Server,
}

impl AppName {
    pub const ALL: [AppName; 5] = [
        AppName::SecretaryCode,
        AppName::Ios,
        AppName::Android,
        AppName::Web,
        AppName::Server,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AppName::SecretaryCode => "secretary-code",
            AppName::Ios => "ios",
            AppName::Android => "android",
            AppName::Web => "web",
            AppName::Server => "server",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == s.trim())
    }

    /// The name the owner knows.
    pub fn label(self) -> &'static str {
        match self {
            AppName::SecretaryCode => "Secretary Code",
            AppName::Ios => "the iPhone app",
            AppName::Android => "the Android app",
            AppName::Web => "the web app",
            AppName::Server => "a Lucina server",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Crash,
    FatalError,
    Error,
    UserReport,
    Hang,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Crash,
        Kind::FatalError,
        Kind::Error,
        Kind::UserReport,
        Kind::Hang,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Crash => "crash",
            Kind::FatalError => "fatal_error",
            Kind::Error => "error",
            Kind::UserReport => "user_report",
            Kind::Hang => "hang",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s.trim())
    }

    /// A new signature of this kind notifies the owner.
    pub fn notifies(self) -> bool {
        matches!(self, Kind::Crash | Kind::FatalError)
    }

    /// The owner describing a problem: never merged with another report.
    pub fn dedupes(self) -> bool {
        self != Kind::UserReport
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Text (logs, JSON): scrubbed, readable by the agents.
    Log,
    /// A Crashpad/Breakpad minidump (binary, stored as sent).
    Minidump,
    /// Any other binary file (stored as sent).
    Binary,
}

impl FileKind {
    pub fn is_text(self) -> bool {
        self == FileKind::Log
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// File name in the report directory (sanitized, `.gz` dropped).
    pub name: String,
    pub kind: FileKind,
    /// Size on disk (after gzip was expanded).
    pub bytes: u64,
    /// The client sent it gzip-compressed.
    pub gzip: bool,
    pub sha256: String,
    /// Secrets replaced in this file.
    #[serde(default)]
    pub redactions: u32,
}

/// One more arrival of a known report (dedupe).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Occurrence {
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub app_version: String,
    pub os: String,
    pub device: String,
}

/// `report.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub app: AppName,
    pub app_version: String,
    pub os: String,
    pub device: String,
    pub kind: Kind,
    pub message: String,
    pub stack: String,
    pub context: Value,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub fingerprint: String,
    /// The normalized frames the fingerprint was computed from.
    pub signature: Vec<String>,
    /// How many times it arrived (1 = only this one).
    pub count: u64,
    pub last_seen_at: DateTime<Utc>,
    pub last_occurred_at: DateTime<Utc>,
    /// The latest repeats (at most [`RECENT_OCCURRENCES`]).
    #[serde(default)]
    pub recent: Vec<Occurrence>,
    pub attachments: Vec<Attachment>,
    /// Secrets replaced in message, stack and context.
    pub redactions: u32,
    /// The client's own id for the report (idempotent retries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

pub const RECENT_OCCURRENCES: usize = 20;

/// A row of the list (`GET /v1/diagnostics/reports`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub app: AppName,
    pub app_version: String,
    pub os: String,
    pub device: String,
    pub kind: Kind,
    /// The first line of the message (at most 300 characters).
    pub message: String,
    pub fingerprint: String,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub count: u64,
    pub attachments: u32,
}

pub fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    clip(line, max)
}

/// At most `max` characters, with `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 23 && id.starts_with("dg_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn new_id() -> String {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).expect("random");
    let mut s = String::from("dg_");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for a in AppName::ALL {
            assert_eq!(AppName::parse(a.as_str()), Some(a));
            assert_eq!(serde_json::to_value(a).unwrap(), a.as_str());
        }
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        assert!(valid_id(&new_id()));
        assert!(!valid_id("dg_../../../etc/passwd"));
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(first_line("\n  boom  \nat x", 10), "boom");
    }
}

//! What a report is: the incoming body, the stored `report.json`, and the
//! row of the index.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The client that sent a report.
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

    /// How the owner calls it.
    pub fn label(self) -> &'static str {
        match self {
            AppName::SecretaryCode => "Secretary Code",
            AppName::Ios => "The iPhone app",
            AppName::Android => "The Android app",
            AppName::Web => "The web app",
            AppName::Server => "The server",
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

    /// A new fingerprint of this kind notifies the owner.
    pub fn notifies(self) -> bool {
        matches!(self, Kind::Crash | Kind::FatalError)
    }

    /// The verb of the notification ("crashed at 19:44").
    pub fn verb(self) -> &'static str {
        match self {
            Kind::Crash => "crashed",
            Kind::FatalError => "hit a fatal error",
            Kind::Error => "reported an error",
            Kind::UserReport => "sent a report",
            Kind::Hang => "hung",
        }
    }
}

/// The JSON a client posts (alone, or as the `report` part of a multipart body).
/// Field names are snake_case; camelCase aliases are accepted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingReport {
    pub app: String,
    #[serde(alias = "appVersion")]
    pub app_version: String,
    #[serde(default)]
    pub os: Option<String>,
    pub device: String,
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub stack: Option<String>,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default, alias = "occurredAt")]
    pub occurred_at: Option<DateTime<Utc>>,
}

/// One stored attachment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub size: u64,
    pub content_type: String,
    /// Text attachments are scrubbed and readable by the agent tools;
    /// binary ones (minidumps) are stored as received.
    pub text: bool,
    /// The client sent it gzip-compressed (it is stored decompressed).
    #[serde(default)]
    pub gzip: bool,
    pub sha256: String,
}

/// `report.json`: everything about the first occurrence of a fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The normalized frames the fingerprint was made of.
    pub top_frames: Vec<String>,
    pub attachments: Vec<Attachment>,
    /// How many secrets the scrubber replaced (text fields and text attachments).
    pub redactions: usize,
    /// Fields cut to their size limit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
}

/// A row of the index: the list view, plus the repeat counter.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub id: String,
    pub app: String,
    pub app_version: String,
    pub os: String,
    pub device: String,
    pub kind: String,
    pub message: String,
    pub top_frame: String,
    pub fingerprint: String,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    /// Occurrences of this fingerprint (1 = only the stored one).
    pub count: i64,
    pub last_seen_at: DateTime<Utc>,
    pub last_device: String,
    pub attachments: i64,
    pub bytes: i64,
}

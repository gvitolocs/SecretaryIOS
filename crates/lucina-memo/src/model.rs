//! Records kept per memo: `meta.json` ([`Memo`]), `transcript.json`
//! ([`Transcript`]) and `summary.json` ([`Summary`]).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where a memo is in its life. The pipeline steps run in this order; a
/// memo that crashed mid-step is picked up again from its last finished output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Created, audio still arriving.
    Uploading,
    /// Audio complete, waiting for the worker.
    Queued,
    Transcribing,
    Summarizing,
    Rendering,
    Publishing,
    Done,
    /// Gave up after the last retry (`error` says why). `reprocess` re-queues it.
    Failed,
}

impl Status {
    pub fn is_pipeline(self) -> bool {
        matches!(
            self,
            Status::Queued
                | Status::Transcribing
                | Status::Summarizing
                | Status::Rendering
                | Status::Publishing
        )
    }
}

/// When each pipeline output was written (None = not yet).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Steps {
    pub transcribed_at: Option<DateTime<Utc>>,
    pub summarized_at: Option<DateTime<Utc>>,
    pub rendered_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub notified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memo {
    /// `mm_` + 16 lowercase hex digits.
    pub id: String,
    /// The device's own id for this recording (idempotent create).
    pub client_id: String,
    pub title: String,
    /// `auto` (placeholder), `lucina` (from the summary), `owner` (sent by the device or renamed).
    pub title_source: String,
    pub recorded_at: DateTime<Utc>,
    /// IANA zone of the recording device, for local dates in titles and notes.
    #[serde(default)]
    pub timezone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Seconds; the device's figure until the server measured the file.
    pub duration_sec: f64,
    pub mime: String,
    /// File extension of the stored audio (`m4a`, `webm`, `wav`, ...).
    pub ext: String,
    pub size_bytes: u64,
    pub received_bytes: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    /// `ios`, `web`, `android`, `import`.
    pub source: String,
    pub status: Status,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub language: Option<String>,
    /// e.g. `whisper.cpp large-v3 (GPU)`.
    #[serde(default)]
    pub engine: Option<String>,
    /// The summary's first lines, for lists and notifications.
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub action_items: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Vault-relative path of the Obsidian note, once published.
    #[serde(default)]
    pub obsidian_note: Option<String>,
    #[serde(default)]
    pub steps: Steps,
    /// Bumped by every re-summary, so a new result notifies and republishes.
    #[serde(default)]
    pub revision: u32,
    /// Where the current transcript came from: `whisper` (the server) or `device` (the
    /// iPhone's, used while the GPU was busy; the worker upgrades it to Whisper later).
    #[serde(default)]
    pub transcript_source: Option<String>,
    #[serde(default)]
    pub upgrade_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Seconds from the start of the recording.
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    /// Whisper's detected (or forced) language, ISO 639-1.
    pub language: String,
    pub engine: String,
    pub duration_sec: f64,
    pub segments: Vec<Segment>,
    /// True when segments carry speaker labels.
    #[serde(default)]
    pub speakers: bool,
    pub created_at: DateTime<Utc>,
}

impl Transcript {
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A point, decision or action, optionally pinned to a moment of the audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub text: String,
    /// Seconds into the recording where it is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// Action items only: who does it ("me" = the owner).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Action items only: a date or time as said (`2026-10-30`, `Friday`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub title: String,
    /// Language the summary is written in.
    pub language: String,
    /// The *resoconto*: a few sentences on what the memo is about.
    pub overview: String,
    #[serde(default)]
    pub key_points: Vec<Item>,
    #[serde(default)]
    pub decisions: Vec<Item>,
    #[serde(default)]
    pub action_items: Vec<Item>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub model: String,
    pub created_at: DateTime<Utc>,
    /// The owner's extra instructions for a re-summary, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// `m:ss` or `h:mm:ss`.
pub fn clock(sec: f64) -> String {
    let s = sec.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(-3.0), "0:00");
    }
}

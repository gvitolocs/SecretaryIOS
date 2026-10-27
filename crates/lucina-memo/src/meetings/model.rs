//! Records kept per meeting, in its folder on the MyBook:
//! `meeting.json` ([`Meeting`]), `transcript.json` ([`MeetingTranscript`]),
//! `speakers.json` ([`Speaker`]s), `brief.json` ([`Brief`]); `transcript.md` and `brief.md`
//! are rendered from them. JSON is snake_case, like the Mac uploader's.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use super::kernel::Status;
use crate::model::{EventLink, Item, Word};

/// The track files of the upload contract (`MEETINGS_API.md`); `mic`, `system` and `video`
/// are accepted as short names.
pub const TRACKS: [&str; 3] = ["mic.m4a", "system.m4a", "video.mp4"];

pub fn track_name(s: &str) -> Option<&'static str> {
    match s {
        "mic" | "mic.m4a" => Some("mic.m4a"),
        "system" | "system.m4a" => Some("system.m4a"),
        "video" | "video.mp4" => Some("video.mp4"),
        _ => None,
    }
}

/// `mic` (the owner), `system` (everyone else) or `video`.
pub fn track_kind(name: &str) -> &'static str {
    match name {
        "mic.m4a" => "mic",
        "system.m4a" => "system",
        _ => "video",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    /// Declared size.
    pub bytes: u64,
    /// Declared SHA-256 (lowercase hex).
    pub sha256: String,
    pub mime: String,
    /// Milliseconds from the shared recording start to the track's first sample.
    #[serde(default)]
    pub start_offset_ms: Option<i64>,
    /// Bytes stored so far.
    #[serde(default)]
    pub received: u64,
    /// Whole and its digest equal to the declared one.
    #[serde(default)]
    pub verified_at: Option<DateTime<Utc>>,
    /// Uploads that ended with another digest (the track was reset each time).
    #[serde(default)]
    pub mismatches: u32,
}

impl Track {
    pub fn whole(&self) -> bool {
        self.verified_at.is_some()
    }
    /// Every track can carry speech: a screen recording's video has the system audio, used
    /// when there is no `system.m4a`.
    pub fn has_audio(&self) -> bool {
        TRACKS.contains(&self.name.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceApp {
    #[serde(default)]
    pub bundle_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// When each output was written (None = not yet).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Steps {
    pub linked_at: Option<DateTime<Utc>>,
    pub transcribed_at: Option<DateTime<Utc>>,
    pub diarized_at: Option<DateTime<Utc>>,
    pub analyzed_at: Option<DateTime<Utc>>,
    pub briefed_at: Option<DateTime<Utc>>,
    /// `transcript.md`, `transcript.json`, `speakers.json`, `brief.md` in the folder.
    pub written_at: Option<DateTime<Utc>>,
    pub noted_at: Option<DateTime<Utc>>,
    pub remembered_at: Option<DateTime<Utc>>,
    pub notified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meeting {
    /// `mtg_` + 16 lowercase hex digits.
    pub id: String,
    /// The client's `Idempotency-Key` (a retried create returns this meeting).
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub title: String,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_app: SourceApp,
    #[serde(default)]
    pub calendar_event_id: Option<String>,
    pub tracks: Vec<Track>,
    /// Relative to the meetings base: `2026/2026-10-26 1430 Acme – Weekly sync`.
    pub folder: String,
    pub status: Status,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// The calendar event (by `calendar_event_id`, else the one the recording falls in).
    #[serde(default)]
    pub event: Option<EventLink>,
    /// From the attendees' e-mail domains.
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub steps: Steps,
    /// Bumped when the outputs are redone (speaker named, reprocess).
    #[serde(default)]
    pub revision: u32,
    /// MK8 markers: the revision whose Telegram message / memory entry was (about to be) sent.
    #[serde(default)]
    pub notify_telegram_rev: Option<u32>,
    #[serde(default)]
    pub memory_rev: Option<u32>,
    /// Vault-relative path of the Obsidian note, recorded before the push.
    #[serde(default)]
    pub obsidian_note: Option<String>,
    /// `llm` or `fallback` (the model's JSON failed validation or the model was unreachable).
    #[serde(default)]
    pub analysis_source: Option<String>,
    #[serde(default)]
    pub brief_source: Option<String>,
}

impl Meeting {
    pub fn track(&self, name: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.name == name)
    }
    pub fn end(&self) -> DateTime<Utc> {
        self.ended_at.unwrap_or_else(|| {
            self.started_at + chrono::Duration::milliseconds((self.duration_sec * 1000.0) as i64)
        })
    }
}

/// What an utterance does in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Question,
    Answer,
    #[default]
    Statement,
    Decision,
    ActionItem,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Question, Kind::Answer, Kind::Statement, Kind::Decision, Kind::ActionItem];
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "question" => Kind::Question,
            "answer" => Kind::Answer,
            "statement" => Kind::Statement,
            "decision" => Kind::Decision,
            "action_item" => Kind::ActionItem,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Answer => "answer",
            Kind::Statement => "statement",
            Kind::Decision => "decision",
            Kind::ActionItem => "action_item",
        }
    }
}

/// Roles the analysis may give a speaker.
pub const ROLES: &[&str] = &[
    "owner",
    "professor",
    "lecturer",
    "teaching_assistant",
    "student",
    "client",
    "colleague",
    "manager",
    "interviewer",
    "interviewee",
    "candidate",
    "presenter",
    "host",
    "participant",
    "guest",
];

/// "teaching_assistant" → "Teaching assistant".
pub fn role_label(role: &str) -> String {
    let s = role.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Utterance {
    /// Seconds from the shared recording start.
    pub start: f64,
    pub end: f64,
    /// `me` (the owner, mic) or `S1`, `S2`, … (clusters of the system track).
    pub speaker: String,
    /// The name or label shown (`Giuseppe`, `Anna`, `Professor`, `Speaker 2`).
    pub speaker_name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub kind: Kind,
    pub text: String,
    /// `mic` or `system`.
    pub track: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingTranscript {
    pub meeting_id: String,
    pub language: String,
    pub engine: String,
    #[serde(default)]
    pub diarization: Option<String>,
    pub duration_sec: f64,
    pub segments: Vec<Utterance>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    pub id: String,
    /// `Speaker 2` (or the owner's name).
    pub label: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `config` (the owner), `owner` (named via the API), `profile` (voice match).
    #[serde(default)]
    pub name_source: Option<String>,
    /// A name the analysis heard in the conversation ("I'm Anna"); never used without the
    /// owner's confirmation.
    #[serde(default)]
    pub suggested_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub track: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// The unknown voice (`vu_…`) this cluster is, until named.
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub similarity: Option<f32>,
    pub talk_sec: f64,
    pub utterances: u32,
}

impl Speaker {
    /// What transcripts show: the name, else the role, else the label.
    pub fn display(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.role.as_deref().filter(|r| *r != "participant").map(role_label))
            .unwrap_or_else(|| self.label.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerPoints {
    pub speaker: String,
    pub points: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Qa {
    pub question: String,
    #[serde(default)]
    pub asked_by: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub answered_by: Option<String>,
    #[serde(default)]
    pub at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    pub title: String,
    pub language: String,
    pub about: String,
    #[serde(default)]
    pub key_points: Vec<SpeakerPoints>,
    #[serde(default)]
    pub qa: Vec<Qa>,
    #[serde(default)]
    pub decisions: Vec<Item>,
    /// `owner`: a speaker id (`me`, `S2`) or a name; `due` as said or `YYYY-MM-DD`.
    #[serde(default)]
    pub action_items: Vec<Item>,
    #[serde(default)]
    pub follow_ups: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `llm` or `fallback`.
    pub source: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
}

/// What the diarization step leaves in `.work/diarization.json` (voice embeddings stay out of
/// the files the owner reads).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Voices {
    /// Speaker id → its centroid embedding.
    #[serde(default)]
    pub centroids: Vec<(String, Vec<f32>)>,
    #[serde(default)]
    pub engine: Option<String>,
}

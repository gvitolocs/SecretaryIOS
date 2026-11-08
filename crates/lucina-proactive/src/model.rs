//! The engine's data model. The Postgres schema (`migrations/`) mirrors it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// What a candidate is about. New sources (MVP 4: email, GitHub, Pokoin
/// servers) add kinds here and a row in the scoring table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A meeting starts within the prep window and there is material to prepare.
    MeetingPrep,
    /// A pending reminder is past due and was not delivered.
    ReminderOverdue,
    /// Something is due soon and nothing in the calendar plans for it.
    DueNoPlan,
    /// A pinned note's topic has a calendar event coming up.
    NoteForEvent,
    /// Something the owner was following looks solved.
    ThreadResolved,
    /// Something the owner was following is still open (Pulse "keep an eye on").
    ThreadOpen,
    /// One of the owner's services is down (sustained, not a blip).
    ServiceDown,
    /// It is back up.
    ServiceRecovered,
    /// The Daily Pulse itself (outreach only, never a candidate).
    Pulse,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::MeetingPrep,
        Kind::ReminderOverdue,
        Kind::DueNoPlan,
        Kind::NoteForEvent,
        Kind::ThreadResolved,
        Kind::ThreadOpen,
        Kind::ServiceDown,
        Kind::ServiceRecovered,
        Kind::Pulse,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::MeetingPrep => "meeting_prep",
            Kind::ReminderOverdue => "reminder_overdue",
            Kind::DueNoPlan => "due_no_plan",
            Kind::NoteForEvent => "note_for_event",
            Kind::ThreadResolved => "thread_resolved",
            Kind::ThreadOpen => "thread_open",
            Kind::ServiceDown => "service_down",
            Kind::ServiceRecovered => "service_recovered",
            Kind::Pulse => "pulse",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Kinds that report something finished (the Pulse's "completed" part).
    pub fn is_completion(self) -> bool {
        matches!(self, Kind::ThreadResolved | Kind::ServiceRecovered)
    }

    /// Kinds that are things to watch (the Pulse's "keep an eye on" part).
    pub fn is_watch(self) -> bool {
        matches!(
            self,
            Kind::ThreadOpen | Kind::DueNoPlan | Kind::NoteForEvent
        )
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
        }
    }
    pub fn parse(s: &str) -> Priority {
        match s {
            "high" => Priority::High,
            "low" => Priority::Low,
            _ => Priority::Normal,
        }
    }
}

/// Where a candidate ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Not worth the owner's attention (low score, expired, muted).
    Drop,
    /// Worth mentioning, not worth an interruption: goes to the Daily Pulse.
    Digest,
    /// Worth sending, but not now (quiet hours, gaming): re-checked every tick.
    Hold,
    /// Passed every gate: sent (or, in dry-run, recorded as would-send).
    Sent,
    /// Surfaced by a Daily Pulse.
    Pulsed,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Drop => "drop",
            Decision::Digest => "digest",
            Decision::Hold => "hold",
            Decision::Sent => "sent",
            Decision::Pulsed => "pulsed",
        }
    }
    pub fn parse(s: &str) -> Decision {
        match s {
            "digest" => Decision::Digest,
            "hold" => Decision::Hold,
            "sent" => Decision::Sent,
            "pulsed" => Decision::Pulsed,
            _ => Decision::Drop,
        }
    }
}

/// The four inputs of the score, each in `[0, 1]` (see [`crate::score`]).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Scores {
    /// How soon it matters.
    pub urgency: f32,
    /// How much it matters to the owner.
    pub relevance: f32,
    /// How verified the signal is (a calendar entry is a fact, a keyword
    /// match is a guess).
    pub confidence: f32,
    /// Whether there is something to do or something prepared.
    pub actionability: f32,
}

impl Scores {
    pub fn new(urgency: f32, relevance: f32, confidence: f32, actionability: f32) -> Self {
        let c = |v: f32| v.clamp(0.0, 1.0);
        Self {
            urgency: c(urgency),
            relevance: c(relevance),
            confidence: c(confidence),
            actionability: c(actionability),
        }
    }
}

/// A signal a source observed. `dedup_key` makes it unique: the same thing
/// seen on the next tick is the same event and is not considered again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub source: String,
    pub kind: Kind,
    pub dedup_key: String,
    pub observed_at: DateTime<Utc>,
    /// After this there is no point in telling the owner.
    pub expires_at: Option<DateTime<Utc>>,
    /// One short line for the decision log and the Pulse (private).
    pub subject: String,
    /// The facts the composer may use (and nothing else).
    pub payload: Value,
    pub scores: Scores,
    /// Why the source thinks this matters (part of the "why" shown on tap).
    pub signal: String,
}

impl Event {
    pub fn new(
        source: &str,
        kind: Kind,
        dedup_key: impl Into<String>,
        observed_at: DateTime<Utc>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: source.to_owned(),
            kind,
            dedup_key: dedup_key.into(),
            observed_at,
            expires_at: None,
            subject: subject.into(),
            payload: Value::Object(Default::default()),
            scores: Scores::default(),
            signal: String::new(),
        }
    }
    pub fn expires(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }
    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
    pub fn scores(mut self, scores: Scores) -> Self {
        self.scores = scores;
        self
    }
    pub fn signal(mut self, signal: impl Into<String>) -> Self {
        self.signal = signal.into();
        self
    }
}

/// An event, scored and decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: Uuid,
    pub event_id: Uuid,
    pub kind: Kind,
    pub scores: Scores,
    pub score: f32,
    pub decision: Decision,
    /// Every decision has a reason (shown on tap as "why").
    pub reason: String,
    pub priority: Priority,
    pub created_at: DateTime<Utc>,
    pub decided_at: DateTime<Utc>,
    pub hold_until: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub pulse_id: Option<String>,
}

/// A button on an outreach. Feedback buttons have `feedback: true`;
/// content buttons may carry an app `route`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub feedback: bool,
}

/// A message Lucina sent (or, dry-run, would have sent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outreach {
    /// `po_` + 26 characters; also in the deep link `/?outreach=<id>`.
    pub id: String,
    pub candidate_id: Option<Uuid>,
    pub kind: Kind,
    /// `app`, `telegram`, `app+telegram` or `none`.
    pub channel: String,
    pub message: String,
    pub priority: Priority,
    pub actions: Vec<Action>,
    pub why: String,
    pub language: String,
    pub dry_run: bool,
    /// `dry_run`, `sent`, `partial`, `failed`.
    pub status: String,
    pub result: Value,
    pub sent_at: DateTime<Utc>,
    pub timeline_id: Option<String>,
    /// `false` for the Daily Pulse (it has its own slot, outside the budget).
    pub unsolicited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackAction {
    /// "Utile" / "Useful" / "Nyttigt".
    Useful,
    /// "Non ora" / "Not now" / "Ikke nu": this kind waits a few hours.
    NotNow,
    /// "Basta così" / "Stop this" / "Stop med det": this kind is muted.
    Stop,
}

impl FeedbackAction {
    pub fn as_str(self) -> &'static str {
        match self {
            FeedbackAction::Useful => "useful",
            FeedbackAction::NotNow => "not_now",
            FeedbackAction::Stop => "stop",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "useful" => Some(FeedbackAction::Useful),
            "not_now" => Some(FeedbackAction::NotNow),
            "stop" => Some(FeedbackAction::Stop),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feedback {
    pub id: Uuid,
    pub outreach_id: String,
    pub action: FeedbackAction,
    pub kind: Kind,
    pub source: String,
    /// The server's id for this tap (idempotent pulls).
    pub external_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A new outreach id: `po_` + a ULID (time-sortable).
pub fn new_outreach_id(now: DateTime<Utc>) -> String {
    format!("po_{}", lucina_store::ulid::new_id(now.timestamp_millis()))
}

/// Is `s` an outreach id we could have made?
pub fn valid_outreach_id(s: &str) -> bool {
    s.len() == 29 && s.starts_with("po_") && lucina_store::ulid::is_timeline_id(&s[3..])
}

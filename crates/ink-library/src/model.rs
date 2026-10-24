//! Canonical records. Ownership, device presence and reading status are independent.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrmState {
    None,
    Protected,
    #[default]
    Unknown,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingStatus {
    #[default]
    Unknown,
    NotStarted,
    Reading,
    Completed,
    Abandoned,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Azw3,
    Azw1,
    Tpz,
    Azw,
    Mobi,
    Kfx,
    Pdf,
    Txt,
    Md,
    Prc,
    Pobi,
    Epub,
}
/// Content classification is separate from ownership and reading status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookKind {
    #[default]
    Book,
    Dictionary,
    Document,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub kind: String,
    pub device: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindlePresence {
    pub path: String,
    pub size_bytes: u64,
    pub last_seen_at: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub percent: Option<f32>,
    pub location: Option<u32>,
    pub page: Option<u32>,
    pub source: String,
    pub at: String,
    /// Reader position (e.g. an EPUB CFI) from a synced device; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRef {
    pub vault: String,
    pub path: String,
    pub title: String,
    pub linked_at: String,
    pub how: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRef {
    pub kind: String,
    pub id: String,
    pub at: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BriefKind {
    Extractive,
    Model,
}
/// Generated text, never attributed to the owner. `based_on` cites highlights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    pub id: String,
    pub kind: BriefKind,
    pub text: String,
    pub generated_at: String,
    pub model: Option<String>,
    pub based_on: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    #[serde(default)]
    pub kind: BookKind,
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub isbn: Option<String>,
    pub doi: Option<String>,
    pub asin: Option<String>,
    pub fingerprints: Vec<String>,
    pub formats: Vec<Format>,
    pub sources: Vec<Source>,
    pub drm: DrmState,
    pub owned: bool,
    pub on_kindle: Option<KindlePresence>,
    pub removed_from_kindle_at: Option<String>,
    pub mentioned_only: bool,
    #[serde(default)]
    pub status: ReadingStatus,
    pub status_source: Option<String>,
    pub progress: Option<Progress>,
    pub last_opened_at: Option<String>,
    pub tags: Vec<String>,
    pub briefs: Vec<Brief>,
    pub related_notes: Vec<NoteRef>,
    pub memory_refs: Vec<MemoryRef>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HighlightKind {
    Highlight,
    Note,
    Bookmark,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocRange {
    pub start: u32,
    pub end: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub file: String,
    pub entry_index: usize,
    pub device: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Highlight {
    pub id: String,
    pub book_id: String,
    pub kind: HighlightKind,
    pub text: String,
    pub note: Option<String>,
    pub location: Option<LocRange>,
    pub page: Option<String>,
    pub added_at: Option<String>,
    pub added_at_raw: String,
    pub source: String,
    pub provenance: Provenance,
    pub dedup_key: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingSession {
    pub id: String,
    pub book_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub start_location: Option<u32>,
    pub end_location: Option<u32>,
    pub source: String,
}
/// Explicit owner status/progress changes, retained for recent-reading reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingChange {
    pub book_id: String,
    pub status: Option<ReadingStatus>,
    pub progress: Option<Progress>,
    pub at: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryState {
    pub books: BTreeMap<String, Book>,
    pub highlights: BTreeMap<String, Vec<Highlight>>,
    pub sessions: Vec<ReadingSession>,
    #[serde(default)]
    pub changes: Vec<ReadingChange>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    /// EXTH 531/532 dictionary language record presence, without interpreting it.
    #[serde(default)]
    pub dictionary: bool,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub cdetype: Option<String>,
    pub language: Option<String>,
    pub drm: DrmState,
}

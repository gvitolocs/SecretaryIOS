//! The note model (docs/keep.md): [`Note`] and its parts, the request
//! bodies, and every validation and normalisation rule (titles, slugs,
//! labels, checklists, colors, reminders).

use std::collections::{HashMap, HashSet};
use std::io;

use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ApiError;

pub const NOTES_DIR: &str = "notes";
pub const TRASH_DIR: &str = "trash";
pub const ATTACHMENTS_DIR: &str = "attachments";
/// Markdown content, in UTF-8 bytes.
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;
/// Request bodies on the JSON notes routes (JSON escaping can double the content).
pub const BODY_LIMIT: usize = 3 * MAX_CONTENT_BYTES;
/// Request bodies on the attachment upload route.
pub const ATTACHMENT_BODY_LIMIT: usize = MAX_ATTACHMENT_BYTES + 64 * 1024;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SLUG_LEN: usize = 64;
pub const MAX_TAGS: usize = 50;
pub const MAX_TAG_CHARS: usize = 50;
pub const MAX_SOURCE_LEN: usize = 32;
pub const MAX_SEPARATOR_CHARS: usize = 8;
/// Live notes at most, archived ones included (the trash does not count).
pub const MAX_NOTES: usize = 20_000;
pub const TRASH_DAYS: i64 = 30;
/// Tombstones of notes deleted forever stay this long for `changes`.
pub const TOMBSTONE_DAYS: i64 = 60;
pub const DEFAULT_LIST_LIMIT: usize = 200;
pub const MAX_LIST_LIMIT: usize = 500;
pub const DEFAULT_CHANGES_LIMIT: usize = 200;
pub const MAX_CHANGES_LIMIT: usize = 500;
pub const MAX_BATCH_IDS: usize = 500;
/// Characters of the excerpt in list answers.
pub const EXCERPT_CHARS: usize = 220;
pub const MAX_ITEMS: usize = 1000;
pub const MAX_ITEM_CHARS: usize = 1000;
pub const MAX_ATTACHMENTS: usize = 50;
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_ATTACHMENT_NAME_CHARS: usize = 200;
pub const MAX_ANNOTATIONS: usize = 50;
pub const MAX_ANNOTATION_CHARS: usize = 2048;
pub const MAX_COLLABORATORS: usize = 50;
/// Path words that cannot be slugs (they are routes).
pub const RESERVED_SLUGS: &[&str] = &[
    "trash",
    "new",
    "search",
    "labels",
    "changes",
    "attachments",
    "import",
    "batch",
];
/// Takeout's color names, lower-cased.
pub const COLORS: &[&str] = &[
    "default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink",
    "brown", "gray",
];
pub const RECURRENCES: &[&str] = &["daily", "weekly", "monthly", "yearly"];

// ---------------------------------------------------------------- note

/// One note, as stored and as `GET /v1/notes/{key}` answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub slug: String,
    /// May be empty when the note has content, items or attachments.
    #[serde(default)]
    pub title: String,
    /// Markdown. For a list note it is derived from `items` on every write.
    #[serde(default)]
    pub content: String,
    /// The Keep labels, normalized (see [`parse_labels`]).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Who wrote it: `owner`, `lucina`, `chatgpt`, `grok`, `claude-code`, …
    pub source: String,
    #[serde(default)]
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Set on notes in the trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub archived: bool,
    /// Present (possibly empty) = a list note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ChecklistItem>>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// Emails; metadata only, never contacted.
    #[serde(default)]
    pub collaborators: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder: Option<Reminder>,
    /// Manual order: higher = earlier in the grid. Defaults to `createdAt` in
    /// epoch milliseconds (a stored 0 means "not set").
    #[serde(default)]
    pub position: f64,
    /// Store revision of the last write to this note (server-set).
    #[serde(default)]
    pub rev: u64,
    #[serde(default, rename = "import", skip_serializing_if = "Option::is_none")]
    pub import: Option<ImportInfo>,
}

fn default_color() -> String {
    "default".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistItem {
    /// `i_` + 12 lowercase hex digits.
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub checked: bool,
    /// 0 or 1.
    #[serde(default)]
    pub indent: u8,
}

/// A file attached to a note; the bytes live in `notes/attachments/<file>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// The SHA-256 of the bytes, hex.
    pub id: String,
    /// `<sha256>.<ext>`.
    pub file: String,
    pub mimetype: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub created_at: String,
}

/// A web link on a note (Takeout `WEBLINK`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    /// RFC 3339.
    pub at: String,
    /// `daily`, `weekly`, `monthly` or `yearly`: metadata for the clients
    /// (Lucina's reminder fires once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// The Lucina reminder id (server-set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lucina_id: Option<String>,
}

/// Importer bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInfo {
    /// `google-keep`.
    pub source: String,
    /// `gkeep:<createdTimestampUsec>[-n]`.
    pub key: String,
    /// The export's `userEditedTimestampUsec`.
    pub edited_usec: i64,
    /// The `updatedAt` the import wrote: a note whose `updatedAt` still equals
    /// it was not edited here since.
    pub imported_at: String,
}

/// A list entry: the note without its content.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    /// The labels' display names, in tag order.
    pub labels: Vec<String>,
    pub source: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Content length in UTF-8 bytes.
    pub size: usize,
    /// The start of the content, or the text around the first match of `q`.
    pub excerpt: String,
    pub color: String,
    pub archived: bool,
    pub is_list: bool,
    pub items_total: usize,
    pub items_done: usize,
    pub attachment_count: usize,
    pub has_image: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder_at: Option<String>,
    pub position: f64,
    pub rev: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<String>,
}

// ---------------------------------------------------------------- errors

pub(crate) fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

pub(crate) fn not_found(key: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "note_not_found",
        format!("no note {key:?}"),
    )
}

pub(crate) fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "note_too_large",
        format!(
            "a note holds at most {} KB of text",
            MAX_CONTENT_BYTES / 1024
        ),
    )
}

pub(crate) fn notes_full() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "notes_full",
        format!("at most {MAX_NOTES} notes; delete some first"),
    )
}

// ---------------------------------------------------------------- time

pub(crate) fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

pub(crate) fn rfc3339(t: time::OffsetDateTime) -> String {
    // Millisecond precision keeps `updatedAt` usable as a version stamp.
    let t = t
        .replace_nanosecond(t.millisecond() as u32 * 1_000_000)
        .unwrap_or(t);
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

pub(crate) fn parse_time(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

pub(crate) fn purge_after(deleted_at: &str) -> Option<String> {
    parse_time(deleted_at).map(|t| rfc3339(t + time::Duration::days(TRASH_DAYS)))
}

/// Epoch milliseconds of an RFC 3339 stamp.
pub(crate) fn epoch_ms(s: &str) -> Option<f64> {
    parse_time(s).map(|t| (t.unix_timestamp_nanos() / 1_000_000) as f64)
}

pub(crate) fn now_ms() -> f64 {
    (now().unix_timestamp_nanos() / 1_000_000) as f64
}

// ---------------------------------------------------------------- ids, slugs

/// Note ids: `n_` + 20 lowercase hex digits.
pub fn valid_id(s: &str) -> bool {
    s.len() == 22
        && s.starts_with("n_")
        && s.as_bytes()[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

fn random_hex(n: usize) -> io::Result<String> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    let mut s = String::with_capacity(n * 2);
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
}

pub(crate) fn new_id() -> io::Result<String> {
    Ok(format!("n_{}", random_hex(10)?))
}

/// Checklist item ids: `i_` + 12 lowercase hex digits.
pub fn valid_item_id(s: &str) -> bool {
    s.len() == 14
        && s.starts_with("i_")
        && s.as_bytes()[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

pub(crate) fn new_item_id() -> String {
    // A failed random source is a broken host; fall back to the clock.
    random_hex(6)
        .map(|h| format!("i_{h}"))
        .unwrap_or_else(|_| format!("i_{:012x}", now().unix_timestamp_nanos() as u64))
}

/// `[a-z0-9]` then `[a-z0-9-]`, no `--`, no trailing `-`, at most
/// [`MAX_SLUG_LEN`] bytes, not a reserved word.
pub fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_SLUG_LEN
        && b[0].is_ascii_alphanumeric()
        && !b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.ends_with('-')
        && !s.contains("--")
        && !RESERVED_SLUGS.contains(&s)
}

fn fold_char(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => "e",
        'ì' | 'í' | 'î' | 'ï' => "i",
        'ñ' | 'ń' => "n",
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => "o",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' => "u",
        'ß' => "ss",
        'ł' => "l",
        'š' | 'ś' => "s",
        'ž' | 'ź' | 'ż' => "z",
        'ý' | 'ÿ' => "y",
        _ => return None,
    })
}

/// A slug from a title: "CRAFT-IT4SD — meeting with Marianne & Jonas" →
/// "craft-it4sd-meeting-with-marianne-jonas". Empty when nothing is left.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.trim().to_lowercase().chars() {
        let piece: String = match fold_char(c) {
            Some(s) => s.to_owned(),
            None if c.is_ascii_alphanumeric() => c.to_string(),
            None => String::new(),
        };
        if piece.is_empty() {
            dash = !out.is_empty();
            continue;
        }
        if dash {
            out.push('-');
            dash = false;
        }
        out.push_str(&piece);
        if out.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let mut s: String = out.chars().take(MAX_SLUG_LEN).collect();
    while s.ends_with('-') {
        s.pop();
    }
    if RESERVED_SLUGS.contains(&s.as_str()) {
        s.push_str("-note");
    }
    s
}

/// The slug a new note gets when none is given: from the title, else from the
/// first six words of the first non-empty line of the content or items, else
/// `note`.
pub(crate) fn slug_base(title: &str, content: &str, items: Option<&[ChecklistItem]>) -> String {
    let from_title = slugify(title);
    if !from_title.is_empty() {
        return from_title;
    }
    let first_line = match items {
        Some(items) => items
            .iter()
            .map(|i| i.text.as_str())
            .find(|t| !t.trim().is_empty()),
        None => content.lines().find(|l| !l.trim().is_empty()),
    };
    let words = first_line
        .map(|l| l.split_whitespace().take(6).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let s = slugify(&words);
    if s.is_empty() {
        "note".to_owned()
    } else {
        s
    }
}

pub(crate) fn check_slug(s: &str) -> Result<String, ApiError> {
    let s = s.trim().to_lowercase();
    if valid_slug(&s) {
        Ok(s)
    } else {
        Err(bad_request(format!(
            "slug: lower-case letters, digits and single '-' (at most {MAX_SLUG_LEN}), not {}",
            RESERVED_SLUGS.join("/")
        )))
    }
}

// ---------------------------------------------------------------- text

/// Title: trimmed, at most [`MAX_TITLE_CHARS`], no control characters. May be
/// empty; the "something must be there" rule is [`check_not_blank`].
pub(crate) fn check_title(t: &str) -> Result<String, ApiError> {
    let t = t.trim();
    if t.chars().count() > MAX_TITLE_CHARS {
        return Err(bad_request(format!(
            "title: at most {MAX_TITLE_CHARS} characters"
        )));
    }
    if t.chars().any(char::is_control) {
        return Err(bad_request("title: no control characters"));
    }
    Ok(t.to_owned())
}

/// An empty title needs content, items or attachments.
pub(crate) fn check_not_blank(n: &Note) -> Result<(), ApiError> {
    let has_items = n.items.as_ref().is_some_and(|i| !i.is_empty());
    if n.title.is_empty() && n.content.trim().is_empty() && !has_items && n.attachments.is_empty() {
        return Err(bad_request("title: must not be empty"));
    }
    Ok(())
}

/// Markdown text: any size up to the limit, no control characters other
/// than newline, carriage return and tab. Line endings become `\n`, except a lone
/// `\r` at the very end: it stays, so that a CRLF document cut between `\r`
/// and `\n` across two appends is still one newline (the next append starts
/// with it; Verus kernel `norm_keep` in verification/verus/notes_kernel.rs).
pub(crate) fn check_content(field: &str, c: &str) -> Result<String, ApiError> {
    if c.chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(bad_request(format!("{field}: no control characters")));
    }
    let trailing_cr = c.ends_with('\r');
    let mut c = c.replace("\r\n", "\n").replace('\r', "\n");
    if trailing_cr {
        c.pop();
        c.push('\r');
    }
    if c.len() > MAX_CONTENT_BYTES {
        return Err(too_large());
    }
    Ok(c)
}

/// Sources: `[a-z0-9][a-z0-9-]*`, at most [`MAX_SOURCE_LEN`] bytes.
pub fn valid_source(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_SOURCE_LEN
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub(crate) fn check_source(s: Option<&str>, default: &str) -> Result<String, ApiError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(default.to_owned()),
        Some(s) => {
            let s = s.to_lowercase();
            if valid_source(&s) {
                Ok(s)
            } else {
                Err(bad_request(
                    "source: must match [a-z0-9][a-z0-9-]* (at most 32 characters)",
                ))
            }
        }
    }
}

// ---------------------------------------------------------------- labels

/// A label: the normalized tag a note stores and the name shown to people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub tag: String,
    pub name: String,
}

/// One label name: whitespace collapsed, a leading `#` dropped. `None` when
/// nothing is left. Any characters except control characters and `,`, at
/// most [`MAX_TAG_CHARS`].
pub fn parse_label(raw: &str) -> Result<Option<Label>, ApiError> {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = flat.trim_start_matches('#').trim_start().to_owned();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_TAG_CHARS || name.chars().any(|c| c.is_control() || c == ',') {
        return Err(bad_request(format!(
            "tags: {raw:?} must not contain control characters or ',' (at most {MAX_TAG_CHARS} characters)"
        )));
    }
    Ok(Some(Label {
        tag: name.to_lowercase(),
        name,
    }))
}

/// Labels in order, unique by tag, at most [`MAX_TAGS`].
pub fn parse_labels(names: &[String]) -> Result<Vec<Label>, ApiError> {
    let mut out: Vec<Label> = Vec::new();
    for raw in names {
        if let Some(l) = parse_label(raw)? {
            if !out.iter().any(|x| x.tag == l.tag) {
                out.push(l);
            }
        }
    }
    if out.len() > MAX_TAGS {
        return Err(bad_request(format!("tags: at most {MAX_TAGS}")));
    }
    Ok(out)
}

/// Tags: trimmed, inner whitespace collapsed, a leading `#` dropped,
/// lower-case; unique; at most [`MAX_TAGS`].
pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>, ApiError> {
    Ok(parse_labels(tags)?.into_iter().map(|l| l.tag).collect())
}

/// `tags` and `labels` of a body, merged (tags first).
pub(crate) fn merge_label_fields(
    tags: Option<&[String]>,
    labels: Option<&[String]>,
) -> Result<Option<Vec<Label>>, ApiError> {
    if tags.is_none() && labels.is_none() {
        return Ok(None);
    }
    let all: Vec<String> = tags
        .unwrap_or_default()
        .iter()
        .chain(labels.unwrap_or_default())
        .cloned()
        .collect();
    parse_labels(&all).map(Some)
}

// ---------------------------------------------------------------- colors, misc fields

pub(crate) fn check_color(c: &str) -> Result<String, ApiError> {
    let c = c.trim().to_lowercase();
    if COLORS.contains(&c.as_str()) {
        Ok(c)
    } else {
        Err(bad_request(format!("color: one of {}", COLORS.join(", "))))
    }
}

pub(crate) fn check_position(p: f64) -> Result<f64, ApiError> {
    if p.is_finite() {
        Ok(p)
    } else {
        Err(bad_request("position: must be a finite number"))
    }
}

fn clean_text(field: &str, s: &str, max: usize) -> Result<String, ApiError> {
    let s = s.trim();
    if s.chars().count() > max {
        return Err(bad_request(format!("{field}: at most {max} characters")));
    }
    if s.chars().any(char::is_control) {
        return Err(bad_request(format!("{field}: no control characters")));
    }
    Ok(s.to_owned())
}

fn opt_text(field: &str, s: Option<&str>, max: usize) -> Result<Option<String>, ApiError> {
    s.map(|s| clean_text(field, s, max))
        .transpose()
        .map(|o| o.filter(|s| !s.is_empty()))
}

pub(crate) fn check_annotations(a: &[Annotation]) -> Result<Vec<Annotation>, ApiError> {
    if a.len() > MAX_ANNOTATIONS {
        return Err(bad_request(format!(
            "annotations: at most {MAX_ANNOTATIONS}"
        )));
    }
    a.iter().map(check_annotation).collect()
}

pub(crate) fn check_annotation(a: &Annotation) -> Result<Annotation, ApiError> {
    let url = clean_text("annotations.url", &a.url, MAX_ANNOTATION_CHARS)?;
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(bad_request("annotations.url: must be an http(s) link"));
    }
    Ok(Annotation {
        url,
        title: opt_text("annotations.title", a.title.as_deref(), 500)?,
        description: opt_text("annotations.description", a.description.as_deref(), 2000)?,
        source: opt_text("annotations.source", a.source.as_deref(), 200)?,
    })
}

pub(crate) fn check_collaborators(c: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for raw in c {
        let e = clean_text("collaborators", raw, 254)?.to_lowercase();
        if e.is_empty() {
            continue;
        }
        if !e.contains('@') || e.chars().any(char::is_whitespace) {
            return Err(bad_request(format!(
                "collaborators: {raw:?} is not an email address"
            )));
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    if out.len() > MAX_COLLABORATORS {
        return Err(bad_request(format!(
            "collaborators: at most {MAX_COLLABORATORS}"
        )));
    }
    Ok(out)
}

// ---------------------------------------------------------------- reminders

/// A reminder as clients send it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderReq {
    pub at: String,
    #[serde(default)]
    pub recurrence: Option<String>,
    /// Server-set; accepted (a client may send a note back as it got it) and ignored.
    #[serde(default)]
    pub lucina_id: Option<String>,
}

/// Validate a requested reminder. `lucina_id` is whatever the server already
/// holds for the same moment, or the id it just got from Lucina.
pub(crate) fn check_reminder(
    r: &ReminderReq,
    lucina_id: Option<String>,
) -> Result<Reminder, ApiError> {
    let at = r.at.trim();
    if parse_time(at).is_none() {
        return Err(bad_request(
            "reminder.at: must be an RFC 3339 timestamp with offset",
        ));
    }
    let recurrence = match r
        .recurrence
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(x) => {
            let x = x.to_lowercase();
            if !RECURRENCES.contains(&x.as_str()) {
                return Err(bad_request(format!(
                    "reminder.recurrence: one of {}",
                    RECURRENCES.join(", ")
                )));
            }
            Some(x)
        }
    };
    Ok(Reminder {
        at: at.to_owned(),
        recurrence,
        lucina_id,
    })
}

/// The text of the Lucina reminder: "Keep: <title or first line>".
pub(crate) fn reminder_text(n: &Note) -> String {
    let line = if n.title.is_empty() {
        n.items
            .as_deref()
            .and_then(|i| {
                i.iter()
                    .map(|i| i.text.as_str())
                    .find(|t| !t.trim().is_empty())
            })
            .or_else(|| n.content.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or("")
    } else {
        n.title.as_str()
    };
    let line = clip(line, 120);
    if line.is_empty() {
        "Keep: note".to_owned()
    } else {
        format!("Keep: {line}")
    }
}

// ---------------------------------------------------------------- checklists

/// Keep a note's text in line with its items: `- [ ] text` / `- [x] text`,
/// two leading spaces for indent 1, one line per item.
pub fn derive_content(items: &[ChecklistItem]) -> String {
    let mut out = String::new();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if it.indent > 0 {
            out.push_str("  ");
        }
        out.push_str(if it.checked { "- [x] " } else { "- [ ] " });
        out.push_str(&it.text);
    }
    out
}

/// Items as plain lines (a list note converted to text).
pub fn items_to_text(items: &[ChecklistItem]) -> String {
    items
        .iter()
        .map(|i| {
            if i.indent > 0 {
                format!("  {}", i.text)
            } else {
                i.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One content line as an item: `- [ ]`, `- [x]`, `* [ ]`, `* [x]` keep their
/// state, a plain `- `/`* ` bullet loses its marker, any other non-empty line
/// is an unchecked item. Two or more leading spaces (or a tab) = indent 1.
fn parse_line(line: &str) -> Option<(String, bool, u8)> {
    let line = line.trim_end();
    let body = line.trim_start();
    if body.is_empty() {
        return None;
    }
    let lead = &line[..line.len() - body.len()];
    let indent = u8::from(lead.contains('\t') || lead.len() >= 2);
    for (prefix, checked) in [
        ("- [ ]", false),
        ("* [ ]", false),
        ("- [x]", true),
        ("- [X]", true),
        ("* [x]", true),
        ("* [X]", true),
    ] {
        if let Some(rest) = body.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(' ') {
                let text = rest.trim().to_owned();
                return (!text.is_empty()).then_some((text, checked, indent));
            }
        }
    }
    let text = body
        .strip_prefix("- ")
        .or_else(|| body.strip_prefix("* "))
        .unwrap_or(body)
        .trim()
        .to_owned();
    (!text.is_empty()).then_some((text, false, indent))
}

/// Parse content lines back into items. An item whose text matches an
/// unused item of `old` keeps that item's id, so ids stay stable when a
/// client saves the derived text again.
pub fn parse_items(content: &str, old: &[ChecklistItem]) -> Vec<ChecklistItem> {
    let mut used: HashSet<usize> = HashSet::new();
    let mut taken: HashSet<String> = HashSet::new();
    content
        .lines()
        .filter_map(parse_line)
        .map(|(text, checked, indent)| {
            let reuse = old
                .iter()
                .enumerate()
                .find(|(i, o)| !used.contains(i) && o.text == text);
            let id = match reuse {
                Some((i, o)) => {
                    used.insert(i);
                    o.id.clone()
                }
                None => new_item_id(),
            };
            taken.insert(id.clone());
            ChecklistItem {
                id,
                text,
                checked,
                indent,
            }
        })
        .collect()
}

/// An item as clients send it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReq {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub indent: u8,
}

/// Validate request items: at most [`MAX_ITEMS`], text without control
/// characters, indent 0 or 1; ids kept when valid and unique, else new.
pub(crate) fn check_items(items: &[ItemReq]) -> Result<Vec<ChecklistItem>, ApiError> {
    if items.len() > MAX_ITEMS {
        return Err(bad_request(format!("items: at most {MAX_ITEMS}")));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let text = clean_text("items.text", &it.text, MAX_ITEM_CHARS)?;
        if it.indent > 1 {
            return Err(bad_request("items.indent: 0 or 1"));
        }
        let id = match it.id.as_deref() {
            Some(id) if valid_item_id(id) && !seen.contains(id) => id.to_owned(),
            _ => {
                let mut id = new_item_id();
                while seen.contains(&id) {
                    id = new_item_id();
                }
                id
            }
        };
        seen.insert(id.clone());
        out.push(ChecklistItem {
            id,
            text,
            checked: it.checked,
            indent: it.indent,
        });
    }
    Ok(out)
}

/// Set a list note's items and the content derived from them.
pub(crate) fn set_items(n: &mut Note, items: Vec<ChecklistItem>) -> Result<(), ApiError> {
    let content = derive_content(&items);
    if content.len() > MAX_CONTENT_BYTES {
        return Err(too_large());
    }
    n.content = content;
    n.items = Some(items);
    Ok(())
}

// ---------------------------------------------------------------- bodies

fn double_option<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// An attachment a body refers to by id (the bytes are already uploaded).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub id: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /v1/notes`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateNote {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    pub slug: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Alias of `tags` (display names).
    #[serde(default)]
    pub labels: Vec<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    pub color: Option<String>,
    #[serde(default)]
    pub archived: bool,
    /// Present = a list note (`content` is then derived and ignored).
    pub items: Option<Vec<ItemReq>>,
    pub attachments: Option<Vec<AttachmentRef>>,
    pub annotations: Option<Vec<Annotation>>,
    pub collaborators: Option<Vec<String>>,
    pub reminder: Option<ReminderReq>,
    pub position: Option<f64>,
    /// Ignored (the importer sets it through [`super::ImportedNote`]).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Server-set; accepted and ignored.
    #[serde(default)]
    pub rev: Option<u64>,
    /// Server-set; accepted and ignored.
    #[serde(default, rename = "import")]
    pub import: Option<serde_json::Value>,
    /// Set by the route after Lucina made the reminder.
    #[serde(skip)]
    pub lucina_id: Option<String>,
}

/// What a PATCH does with the note's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Convert {
    Text,
    List,
}

/// `PATCH /v1/notes/{key}`: absent fields keep their value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateNote {
    pub title: Option<String>,
    pub content: Option<String>,
    pub slug: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Alias of `tags` (display names).
    pub labels: Option<Vec<String>>,
    pub pinned: Option<bool>,
    pub color: Option<String>,
    pub archived: Option<bool>,
    /// `Some(None)` is `items: null`, which is refused (use `convert`).
    #[serde(default, deserialize_with = "double_option")]
    pub items: Option<Option<Vec<ItemReq>>>,
    pub convert: Option<Convert>,
    pub attachments: Option<Vec<AttachmentRef>>,
    pub annotations: Option<Vec<Annotation>>,
    pub collaborators: Option<Vec<String>>,
    /// `Some(None)` = `reminder: null`, clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub reminder: Option<Option<ReminderReq>>,
    pub position: Option<f64>,
    /// Who changed it; only recorded in the log (the note keeps its source).
    pub source: Option<String>,
    /// The `updatedAt` the client edited; a newer note is `409 conflict`.
    pub base_updated_at: Option<String>,
    /// Server-set; accepted and ignored.
    #[serde(default)]
    pub rev: Option<u64>,
    /// Server-set; accepted and ignored.
    #[serde(default, rename = "import")]
    pub import: Option<serde_json::Value>,
    /// Set by the route after Lucina made the reminder.
    #[serde(skip)]
    pub lucina_id: Option<String>,
}

/// `POST /v1/notes/{key}/append`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppendNote {
    pub content: String,
    /// Put between the old content and the new (default: nothing).
    #[serde(default)]
    pub separator: Option<String>,
    pub source: Option<String>,
}

/// `POST /v1/notes/batch`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BatchReq {
    #[serde(default)]
    pub ids: Vec<String>,
    pub set: Option<BatchSet>,
    #[serde(default)]
    pub add_labels: Vec<String>,
    #[serde(default)]
    pub remove_labels: Vec<String>,
    #[serde(default)]
    pub trash: bool,
    #[serde(default)]
    pub restore: bool,
    #[serde(default)]
    pub delete_forever: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BatchSet {
    pub color: Option<String>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
}

fn opt_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

/// `GET /v1/notes` query.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default, deserialize_with = "opt_str")]
    pub q: Option<String>,
    #[serde(default, deserialize_with = "opt_str")]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// `include` (default), `exclude` or `only`.
    #[serde(default, deserialize_with = "opt_str")]
    pub archived: Option<String>,
    #[serde(default, deserialize_with = "opt_str")]
    pub color: Option<String>,
    /// `list`, `text`, `image`, `audio`, `link` or `reminder`.
    #[serde(default, rename = "type", deserialize_with = "opt_str")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArchivedFilter {
    #[default]
    Include,
    Exclude,
    Only,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    List,
    Text,
    Image,
    Audio,
    Link,
    Reminder,
}

/// A parsed list query.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Folded, trimmed search text.
    pub q: Option<String>,
    pub tag: Option<String>,
    pub archived: ArchivedFilter,
    pub color: Option<String>,
    pub kind: Option<Kind>,
}

impl Filter {
    pub fn new(q: Option<&str>, tag: Option<&str>) -> Self {
        let q = q
            .map(fold)
            .map(|q| q.trim().to_owned())
            .filter(|q| !q.is_empty());
        // A tag that cannot exist matches nothing.
        let tag = tag.filter(|t| !t.trim().is_empty()).map(|t| {
            parse_label(t)
                .ok()
                .flatten()
                .map_or_else(String::new, |l| l.tag)
        });
        Self {
            q,
            tag,
            ..Self::default()
        }
    }

    pub fn from_query(q: &ListQuery) -> Result<Self, ApiError> {
        let mut f = Self::new(q.q.as_deref(), q.tag.as_deref());
        f.archived = match q.archived.as_deref().map(str::trim) {
            None | Some("") | Some("include") => ArchivedFilter::Include,
            Some("exclude") => ArchivedFilter::Exclude,
            Some("only") => ArchivedFilter::Only,
            Some(_) => return Err(bad_request("archived: include, exclude or only")),
        };
        f.color = match q.color.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            None => None,
            Some(c) => Some(check_color(c)?),
        };
        f.kind = match q.kind.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            None => None,
            Some("list") => Some(Kind::List),
            Some("text") => Some(Kind::Text),
            Some("image") => Some(Kind::Image),
            Some("audio") => Some(Kind::Audio),
            Some("link") => Some(Kind::Link),
            Some("reminder") => Some(Kind::Reminder),
            Some(_) => {
                return Err(bad_request(
                    "type: list, text, image, audio, link or reminder",
                ))
            }
        };
        Ok(f)
    }
}

// ---------------------------------------------------------------- search

pub(crate) fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        match fold_char(c) {
            Some(f) => out.push_str(f),
            None => out.push(c),
        }
    }
    out
}

/// The first `n` characters of `s`, whitespace collapsed, `…` when cut.
pub(crate) fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        return flat;
    }
    let mut out: String = flat.chars().take(n).collect();
    out.push('…');
    out
}

/// Text around the first match of `needle` (already folded) in `content`.
fn snippet(content: &str, needle: &str) -> Option<String> {
    let chars: Vec<char> = content.chars().collect();
    // Search the folded text char by char so positions map back.
    let folded: Vec<String> = chars.iter().map(|c| fold(&c.to_string())).collect();
    let joined: String = folded.concat();
    let byte = joined.find(needle)?;
    let mut acc = 0;
    let mut at = 0;
    for (i, f) in folded.iter().enumerate() {
        if acc >= byte {
            at = i;
            break;
        }
        acc += f.len();
        at = i;
    }
    let start = at.saturating_sub(60);
    let end = (at + EXCERPT_CHARS - 60).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    Some(s)
}

/// Tag → display name.
pub type LabelNames = HashMap<String, String>;

pub(crate) fn is_image(mimetype: &str) -> bool {
    mimetype.starts_with("image/")
}

impl Note {
    pub fn is_list(&self) -> bool {
        self.items.is_some()
    }

    pub fn matches(&self, f: &Filter) -> bool {
        match f.archived {
            ArchivedFilter::Include => {}
            ArchivedFilter::Exclude if self.archived => return false,
            ArchivedFilter::Only if !self.archived => return false,
            _ => {}
        }
        if let Some(t) = f.tag.as_deref() {
            if !self.tags.iter().any(|x| x == t) {
                return false;
            }
        }
        if f.color.as_deref().is_some_and(|c| c != self.color) {
            return false;
        }
        if let Some(k) = f.kind {
            let ok = match k {
                Kind::List => self.is_list(),
                Kind::Text => !self.is_list(),
                Kind::Image => self.attachments.iter().any(|a| is_image(&a.mimetype)),
                Kind::Audio => self
                    .attachments
                    .iter()
                    .any(|a| a.mimetype.starts_with("audio/")),
                Kind::Link => !self.annotations.is_empty(),
                Kind::Reminder => self.reminder.is_some(),
            };
            if !ok {
                return false;
            }
        }
        match f.q.as_deref() {
            None => true,
            Some(q) => {
                fold(&self.title).contains(q)
                    || self.slug.contains(q)
                    || self.tags.iter().any(|t| fold(t).contains(q))
                    || fold(&self.content).contains(q)
            }
        }
    }

    pub fn summary(&self, q: Option<&str>, names: &LabelNames) -> NoteSummary {
        let excerpt = q
            .and_then(|q| snippet(&self.content, q))
            .unwrap_or_else(|| clip(&self.content, EXCERPT_CHARS));
        let items = self.items.as_deref().unwrap_or_default();
        NoteSummary {
            id: self.id.clone(),
            slug: self.slug.clone(),
            title: self.title.clone(),
            tags: self.tags.clone(),
            labels: self
                .tags
                .iter()
                .map(|t| names.get(t).cloned().unwrap_or_else(|| t.clone()))
                .collect(),
            source: self.source.clone(),
            pinned: self.pinned,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            size: self.content.len(),
            excerpt,
            color: self.color.clone(),
            archived: self.archived,
            is_list: self.is_list(),
            items_total: items.len(),
            items_done: items.iter().filter(|i| i.checked).count(),
            attachment_count: self.attachments.len(),
            has_image: self.attachments.iter().any(|a| is_image(&a.mimetype)),
            reminder_at: self.reminder.as_ref().map(|r| r.at.clone()),
            position: self.position,
            rev: self.rev,
            deleted_at: self.deleted_at.clone(),
            purge_after: self.deleted_at.as_deref().and_then(purge_after),
        }
    }
}

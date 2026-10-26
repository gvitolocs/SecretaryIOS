//! Notes: briefings, notes and documents the owner can read in the app and
//! Lucina can recall by name (docs/server-api.md, "Notes").
//!
//! - `GET /v1/notes?q=&tag=&limit=` → `{notes: [NoteSummary]}`: pinned first,
//!   then the most recently updated. `q` looks in the title, slug, tags and
//!   content (case-insensitive); `tag` keeps one tag.
//! - `POST /v1/notes` `{title, content?, slug?, tags?, source?, pinned?}` →
//!   201 [`Note`]. Without a slug, one is made from the title (`-2`, `-3`, …
//!   when taken); an explicit slug that is taken is `409 slug_taken`.
//! - `GET /v1/notes/{key}`, `PATCH /v1/notes/{key}`, `DELETE /v1/notes/{key}`:
//!   `key` is the id (`n_…`) or the slug, so "craftit-meeting" resolves by
//!   name. PATCH takes any of `{title, content, slug, tags, pinned}` plus an
//!   optional `baseUpdatedAt` (`409 conflict` when the note changed since).
//!   DELETE moves the note to the trash.
//! - `POST /v1/notes/{key}/append` `{content, separator?}`: adds text at the
//!   end, exactly as given (chunked uploads of long documents).
//! - `GET /v1/notes/trash` and `POST /v1/notes/trash/{id}/restore`: deleted
//!   notes stay restorable for [`TRASH_DAYS`] days, then they are purged.
//!
//! Content is markdown, at most [`MAX_CONTENT_BYTES`]. Each note is a JSON
//! file in `$SECRETARY_DATA_DIR/notes/` (trash: `notes/trash/`), written
//! atomically with mode 0600. Who may call: a device (Bearer, or the cookie
//! plus `X-Secretary-CSRF: 1` for writes) or Lucina with the service token.
//! Every write is logged with the caller (never the content).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::extract::{DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::devices::{ensure_dir, save_json_atomic};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

pub const NOTES_DIR: &str = "notes";
pub const TRASH_DIR: &str = "trash";
/// Markdown content, in UTF-8 bytes.
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;
/// Request bodies on the notes routes (JSON escaping can double the content).
pub const BODY_LIMIT: usize = 3 * MAX_CONTENT_BYTES;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SLUG_LEN: usize = 64;
pub const MAX_TAGS: usize = 16;
pub const MAX_TAG_CHARS: usize = 32;
pub const MAX_SOURCE_LEN: usize = 32;
pub const MAX_SEPARATOR_CHARS: usize = 8;
/// Live notes at most (the trash does not count).
pub const MAX_NOTES: usize = 2000;
pub const TRASH_DAYS: i64 = 30;
pub const DEFAULT_LIST_LIMIT: usize = 200;
pub const MAX_LIST_LIMIT: usize = 500;
/// Characters of the excerpt in list answers.
pub const EXCERPT_CHARS: usize = 220;
/// Path words that cannot be slugs (they are routes).
pub const RESERVED_SLUGS: &[&str] = &["trash", "new", "search"];

/// One note, as stored and as `GET /v1/notes/{key}` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub content: String,
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
}

/// A list entry: the note without its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub source: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Content length in UTF-8 bytes.
    pub size: usize,
    /// The start of the content, or the text around the first match of `q`.
    pub excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<String>,
}

// ---------------------------------------------------------------- validation

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn not_found(key: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "note_not_found",
        format!("no note {key:?}"),
    )
}

fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "note_too_large",
        format!(
            "a note holds at most {} KB of text",
            MAX_CONTENT_BYTES / 1024
        ),
    )
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

/// Note ids: `n_` + 20 lowercase hex digits.
pub fn valid_id(s: &str) -> bool {
    s.len() == 22
        && s.starts_with("n_")
        && s.as_bytes()[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

fn new_id() -> io::Result<String> {
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    let mut s = String::from("n_");
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
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

fn check_title(t: &str) -> Result<String, ApiError> {
    let t = t.trim();
    if t.is_empty() {
        return Err(bad_request("title: must not be empty"));
    }
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

/// Markdown text: any size up to the limit, no control characters other than
/// newline, carriage return and tab. Line endings become `\n`, except a lone
/// `\r` at the very end: it stays, so that a CRLF document cut between `\r`
/// and `\n` across two appends is still one newline (the next append starts
/// with it; Verus kernel `norm_keep` in verification/verus/notes_kernel.rs).
fn check_content(field: &str, c: &str) -> Result<String, ApiError> {
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

/// Tags: trimmed, lower-case, inner spaces collapsed, letters, digits,
/// spaces, `-` and `_`; unique; at most [`MAX_TAGS`].
pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for raw in tags {
        let t = raw
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let t = t.trim_start_matches('#').to_owned();
        if t.is_empty() {
            continue;
        }
        if t.chars().count() > MAX_TAG_CHARS
            || !t
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            return Err(bad_request(format!(
                "tags: {raw:?} must be letters, digits, spaces, '-' or '_' (at most {MAX_TAG_CHARS})"
            )));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > MAX_TAGS {
        return Err(bad_request(format!("tags: at most {MAX_TAGS}")));
    }
    Ok(out)
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

fn check_source(s: Option<&str>, default: &str) -> Result<String, ApiError> {
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

// ---------------------------------------------------------------- bodies

/// `POST /v1/notes`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateNote {
    pub title: String,
    #[serde(default)]
    pub content: String,
    pub slug: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

/// `PATCH /v1/notes/{key}`: absent fields keep their value.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateNote {
    pub title: Option<String>,
    pub content: Option<String>,
    pub slug: Option<String>,
    pub tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    /// Who changed it; only recorded in the log (the note keeps its source).
    pub source: Option<String>,
    /// The `updatedAt` the client edited; a newer note is `409 conflict`.
    pub base_updated_at: Option<String>,
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
}

// ---------------------------------------------------------------- search

fn fold(s: &str) -> String {
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
fn clip(s: &str, n: usize) -> String {
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

impl Note {
    fn matches(&self, q: Option<&str>, tag: Option<&str>) -> bool {
        if let Some(t) = tag {
            if !self.tags.iter().any(|x| x == t) {
                return false;
            }
        }
        match q {
            None => true,
            Some(q) => {
                fold(&self.title).contains(q)
                    || self.slug.contains(q)
                    || self.tags.iter().any(|t| fold(t).contains(q))
                    || fold(&self.content).contains(q)
            }
        }
    }

    pub fn summary(&self, q: Option<&str>) -> NoteSummary {
        let excerpt = q
            .and_then(|q| snippet(&self.content, q))
            .unwrap_or_else(|| clip(&self.content, EXCERPT_CHARS));
        NoteSummary {
            id: self.id.clone(),
            slug: self.slug.clone(),
            title: self.title.clone(),
            tags: self.tags.clone(),
            source: self.source.clone(),
            pinned: self.pinned,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            size: self.content.len(),
            excerpt,
            deleted_at: self.deleted_at.clone(),
            purge_after: self.deleted_at.as_deref().and_then(purge_after),
        }
    }
}

// ---------------------------------------------------------------- time

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

fn rfc3339(t: time::OffsetDateTime) -> String {
    // Millisecond precision keeps `updatedAt` usable as a version stamp.
    let t = t
        .replace_nanosecond(t.millisecond() as u32 * 1_000_000)
        .unwrap_or(t);
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn parse_time(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

fn purge_after(deleted_at: &str) -> Option<String> {
    parse_time(deleted_at).map(|t| rfc3339(t + time::Duration::days(TRASH_DAYS)))
}

// ---------------------------------------------------------------- store

/// Who is writing (for `source` defaults and the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller(pub &'static str);

impl Caller {
    /// The default `source` for notes this caller creates.
    fn default_source(self) -> &'static str {
        match self.0 {
            "lucina" => "lucina",
            _ => "owner",
        }
    }
}

struct Cache {
    stamp: Option<SystemTime>,
    notes: BTreeMap<String, Note>,
}

/// `notes/*.json` in the data dir, cached in memory and reloaded when the
/// directory changes (an import from the CLI, a restore by hand).
pub struct NoteStore {
    dir: PathBuf,
    cache: Mutex<Cache>,
}

fn dir_stamp(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

fn read_dir_notes(dir: &Path) -> BTreeMap<String, Note> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        match std::fs::read(&p)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Note>(&b).map_err(|e| e.to_string()))
        {
            Ok(n) if n.id == id => {
                out.insert(n.id.clone(), n);
            }
            Ok(_) => tracing::warn!(path = %p.display(), "note file id mismatch; skipped"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "note unreadable; skipped"),
        }
    }
    out
}

fn io_err(e: io::Error) -> ApiError {
    tracing::error!(error = %e, "notes write failed");
    ApiError::internal("could not save the note")
}

impl NoteStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(NOTES_DIR),
            cache: Mutex::new(Cache {
                stamp: None,
                notes: BTreeMap::new(),
            }),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn trash_dir(&self) -> PathBuf {
        self.dir.join(TRASH_DIR)
    }

    /// Run `f` on the up-to-date cache, one caller at a time.
    fn with<R>(&self, f: impl FnOnce(&Self, &mut BTreeMap<String, Note>) -> R) -> R {
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let stamp = dir_stamp(&self.dir);
        if stamp.is_none() || stamp != c.stamp {
            c.notes = read_dir_notes(&self.dir);
            c.stamp = stamp;
        }
        let r = f(self, &mut c.notes);
        c.stamp = dir_stamp(&self.dir);
        r
    }

    fn write(&self, n: &Note) -> Result<(), ApiError> {
        save_json_atomic(&self.dir, &format!("{}.json", n.id), n).map_err(io_err)
    }

    fn find<'a>(notes: &'a BTreeMap<String, Note>, key: &str) -> Option<&'a Note> {
        let key = key.trim();
        if valid_id(key) {
            return notes.get(key);
        }
        let k = key.to_lowercase();
        notes.values().find(|n| n.slug == k)
    }

    fn slug_taken(notes: &BTreeMap<String, Note>, slug: &str, except: Option<&str>) -> bool {
        notes
            .values()
            .any(|n| n.slug == slug && Some(n.id.as_str()) != except)
    }

    fn free_slug(notes: &BTreeMap<String, Note>, base: &str, except: Option<&str>) -> String {
        if !Self::slug_taken(notes, base, except) {
            return base.to_owned();
        }
        for i in 2.. {
            let suffix = format!("-{i}");
            let mut b: String = base.to_owned();
            b.truncate(MAX_SLUG_LEN - suffix.len());
            while b.ends_with('-') {
                b.pop();
            }
            let s = format!("{b}{suffix}");
            if !Self::slug_taken(notes, &s, except) {
                return s;
            }
        }
        unreachable!()
    }

    /// Every live note matching `q` / `tag`, pinned first, newest first.
    pub fn list(&self, q: Option<&str>, tag: Option<&str>, limit: usize) -> Vec<NoteSummary> {
        let q = q.map(fold).filter(|q| !q.trim().is_empty());
        let q = q.as_deref().map(str::trim);
        let tag = tag
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty());
        self.with(|_, notes| {
            let mut v: Vec<&Note> = notes
                .values()
                .filter(|n| n.matches(q, tag.as_deref()))
                .collect();
            v.sort_by(|a, b| {
                b.pinned
                    .cmp(&a.pinned)
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
            });
            v.into_iter().take(limit).map(|n| n.summary(q)).collect()
        })
    }

    pub fn get(&self, key: &str) -> Result<Note, ApiError> {
        self.with(|_, notes| {
            Self::find(notes, key)
                .cloned()
                .ok_or_else(|| not_found(key))
        })
    }

    pub fn create(&self, req: CreateNote, caller: Caller) -> Result<Note, ApiError> {
        let title = check_title(&req.title)?;
        let content = check_content("content", &req.content)?;
        let tags = normalize_tags(&req.tags)?;
        let source = check_source(req.source.as_deref(), caller.default_source())?;
        let explicit = match req.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => {
                let s = s.to_lowercase();
                if !valid_slug(&s) {
                    return Err(bad_request(format!(
                        "slug: lower-case letters, digits and single '-' (at most {MAX_SLUG_LEN}), not {}",
                        RESERVED_SLUGS.join("/")
                    )));
                }
                Some(s)
            }
            None => None,
        };
        let id = new_id().map_err(io_err)?;
        self.with(|st, notes| {
            if notes.len() >= MAX_NOTES {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "notes_full",
                    format!("at most {MAX_NOTES} notes; delete some first"),
                ));
            }
            let slug = match explicit {
                Some(s) => {
                    if Self::slug_taken(notes, &s, None) {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "slug_taken",
                            format!("a note named {s:?} exists; update it or append to it"),
                        ));
                    }
                    s
                }
                None => {
                    let base = slugify(&title);
                    let base = if base.is_empty() {
                        "note".to_owned()
                    } else {
                        base
                    };
                    Self::free_slug(notes, &base, None)
                }
            };
            let t = rfc3339(now());
            let note = Note {
                id,
                slug,
                title,
                content,
                tags,
                source,
                pinned: req.pinned,
                created_at: t.clone(),
                updated_at: t,
                deleted_at: None,
            };
            st.write(&note)?;
            notes.insert(note.id.clone(), note.clone());
            Ok(note)
        })
    }

    pub fn update(&self, key: &str, req: UpdateNote) -> Result<Note, ApiError> {
        let title = req.title.as_deref().map(check_title).transpose()?;
        let content = req
            .content
            .as_deref()
            .map(|c| check_content("content", c))
            .transpose()?;
        let tags = req.tags.as_deref().map(normalize_tags).transpose()?;
        let slug = match req.slug.as_deref().map(str::trim) {
            Some(s) => {
                let s = s.to_lowercase();
                if !valid_slug(&s) {
                    return Err(bad_request(
                        "slug: lower-case letters, digits and single '-'",
                    ));
                }
                Some(s)
            }
            None => None,
        };
        self.with(|st, notes| {
            let mut note = Self::find(notes, key)
                .cloned()
                .ok_or_else(|| not_found(key))?;
            if let Some(base) = req.base_updated_at.as_deref() {
                if base != note.updated_at {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "conflict",
                        "the note changed since you opened it; reload and try again",
                    ));
                }
            }
            if let Some(s) = slug {
                if Self::slug_taken(notes, &s, Some(&note.id)) {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "slug_taken",
                        format!("a note named {s:?} exists"),
                    ));
                }
                note.slug = s;
            }
            if let Some(t) = title {
                note.title = t;
            }
            if let Some(c) = content {
                note.content = c;
            }
            if let Some(t) = tags {
                note.tags = t;
            }
            if let Some(p) = req.pinned {
                note.pinned = p;
            }
            note.updated_at = rfc3339(now());
            st.write(&note)?;
            notes.insert(note.id.clone(), note.clone());
            Ok(note)
        })
    }

    pub fn append(&self, key: &str, req: AppendNote) -> Result<Note, ApiError> {
        let add = check_content("content", &req.content)?;
        if add.is_empty() {
            return Err(bad_request("content: must not be empty"));
        }
        let sep = match req.separator.as_deref() {
            Some(s) if s.chars().count() > MAX_SEPARATOR_CHARS => {
                return Err(bad_request(format!(
                    "separator: at most {MAX_SEPARATOR_CHARS} characters"
                )))
            }
            Some(s) => {
                check_content("separator", s)?;
                s.to_owned()
            }
            None => String::new(),
        };
        self.with(|st, notes| {
            let mut note = Self::find(notes, key)
                .cloned()
                .ok_or_else(|| not_found(key))?;
            let sep = if note.content.is_empty() {
                ""
            } else {
                sep.as_str()
            };
            // Normalise the raw separator + text together, after a '\r' the
            // previous append left at the end (Verus kernel `join`).
            let mut content = note.content.clone();
            let carry = if content.ends_with('\r') {
                content.pop();
                "\r"
            } else {
                ""
            };
            let tail = check_content("content", &format!("{carry}{sep}{}", req.content))?;
            if content.len() + tail.len() > MAX_CONTENT_BYTES {
                return Err(too_large());
            }
            content.push_str(&tail);
            note.content = content;
            note.updated_at = rfc3339(now());
            st.write(&note)?;
            notes.insert(note.id.clone(), note.clone());
            Ok(note)
        })
    }

    /// Move a note to the trash. Returns it (with `deletedAt`).
    pub fn delete(&self, key: &str) -> Result<Note, ApiError> {
        self.with(|st, notes| {
            let mut note = Self::find(notes, key)
                .cloned()
                .ok_or_else(|| not_found(key))?;
            note.deleted_at = Some(rfc3339(now()));
            save_json_atomic(&st.trash_dir(), &format!("{}.json", note.id), &note)
                .map_err(io_err)?;
            std::fs::remove_file(st.dir.join(format!("{}.json", note.id))).map_err(io_err)?;
            notes.remove(&note.id);
            st.purge_trash();
            Ok(note)
        })
    }

    /// Notes in the trash, newest deletion first (expired ones are purged).
    pub fn trash(&self) -> Vec<NoteSummary> {
        self.with(|st, _| {
            st.purge_trash();
            let mut v: Vec<Note> = read_dir_notes(&st.trash_dir()).into_values().collect();
            v.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
            v.iter().map(|n| n.summary(None)).collect()
        })
    }

    /// Bring a note back from the trash. Its slug gets a `-2` suffix when a
    /// live note took it meanwhile.
    pub fn restore(&self, id: &str) -> Result<Note, ApiError> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        self.with(|st, notes| {
            let path = st.trash_dir().join(format!("{id}.json"));
            let mut note: Note = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| not_found(id))?;
            if notes.len() >= MAX_NOTES {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "notes_full",
                    format!("at most {MAX_NOTES} notes; delete some first"),
                ));
            }
            note.deleted_at = None;
            note.slug = Self::free_slug(notes, &note.slug, None);
            note.updated_at = rfc3339(now());
            st.write(&note)?;
            let _ = std::fs::remove_file(&path);
            notes.insert(note.id.clone(), note.clone());
            Ok(note)
        })
    }

    /// Remove trash entries deleted more than [`TRASH_DAYS`] ago.
    fn purge_trash(&self) {
        let dir = self.trash_dir();
        let cutoff = now() - time::Duration::days(TRASH_DAYS);
        for n in read_dir_notes(&dir).into_values() {
            let old = n
                .deleted_at
                .as_deref()
                .and_then(parse_time)
                .is_none_or(|t| t < cutoff);
            if old {
                let _ = std::fs::remove_file(dir.join(format!("{}.json", n.id)));
                tracing::info!(id = %n.id, slug = %n.slug, "note purged from the trash");
            }
        }
    }

    /// Import (CLI): create the note, or with `replace` update the one with
    /// the same slug. Returns `(note, created)`; `None` = skipped (exists).
    pub fn import(&self, seed: Seed, replace: bool) -> Result<Option<(Note, bool)>, ApiError> {
        if let Ok(existing) = self.get(&seed.slug) {
            if !replace {
                return Ok(None);
            }
            let n = self.update(
                &existing.id,
                UpdateNote {
                    title: Some(seed.title),
                    content: Some(seed.content),
                    tags: Some(seed.tags),
                    pinned: Some(seed.pinned),
                    ..UpdateNote::default()
                },
            )?;
            return Ok(Some((n, false)));
        }
        let n = self.create(
            CreateNote {
                title: seed.title,
                content: seed.content,
                slug: Some(seed.slug),
                tags: seed.tags,
                source: Some(seed.source),
                pinned: seed.pinned,
            },
            Caller("owner"),
        )?;
        Ok(Some((n, true)))
    }
}

// ---------------------------------------------------------------- seed files

/// A note to import: a `.json` file (`{slug, title, content, tags?, source?,
/// pinned?}`) or a `.md` file with a front matter block:
///
/// ```text
/// ---
/// slug: craftit-meeting
/// title: CRAFT-IT4SD — meeting with Marianne & Jonas
/// tags: work, aarhus
/// source: chatgpt
/// pinned: true
/// ---
/// # Markdown content…
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    pub slug: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "owner")]
    pub source: String,
    #[serde(default)]
    pub pinned: bool,
}

fn owner() -> String {
    "owner".into()
}

/// Parse a seed file's text (`md` = front matter + markdown).
pub fn parse_seed(text: &str, md: bool) -> anyhow::Result<Seed> {
    if !md {
        return Ok(serde_json::from_str(text)?);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("a .md seed starts with a '---' front matter block"))?;
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("the front matter block has no closing '---'"))?;
    let (head, body) = (&rest[..end], &rest[end + 5..]);
    let mut v = serde_json::Map::new();
    for line in head.lines().filter(|l| !l.trim().is_empty()) {
        let (k, val) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("front matter line without ':': {line:?}"))?;
        let (k, val) = (k.trim(), val.trim());
        let value = match k {
            "tags" => Value::Array(
                val.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| Value::String(t.to_owned()))
                    .collect(),
            ),
            "pinned" => Value::Bool(val == "true"),
            _ => Value::String(val.to_owned()),
        };
        v.insert(k.to_owned(), value);
    }
    v.insert(
        "content".into(),
        Value::String(body.trim_start_matches('\n').to_owned()),
    );
    Ok(serde_json::from_value(Value::Object(v))?)
}

// ---------------------------------------------------------------- HTTP

fn client_label(headers: &HeaderMap) -> &'static str {
    match headers
        .get(crate::routes::CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        Some("web") => "web",
        Some("ios") => "ios",
        _ => "device",
    }
}

/// A device (Bearer, or cookie + CSRF for writes) or the service token.
async fn device_or_service(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if crate::service::service_bearer(req.headers()).is_some() {
        req.extensions_mut().insert(Caller("lucina"));
        return crate::service::require_service(State(st), req, next).await;
    }
    let label = client_label(req.headers());
    req.extensions_mut().insert(Caller(label));
    crate::auth::require_device(State(st), req, next).await
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("notes task failed"))
}

fn log_write(what: &str, n: &Note, caller: Caller, source: Option<&str>) {
    tracing::info!(
        id = %n.id,
        slug = %n.slug,
        bytes = n.content.len(),
        source = %source.unwrap_or(&n.source),
        by = caller.0,
        "note {what}"
    );
}

async fn list(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    if q.q.as_deref().is_some_and(|s| s.chars().count() > 200) {
        return Err(bad_request("q: at most 200 characters"));
    }
    let store = st.notes.clone();
    let notes = blocking(move || store.list(q.q.as_deref(), q.tag.as_deref(), limit)).await?;
    Ok(Json(json!({ "notes": notes })))
}

async fn create(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(req): JsonBody<CreateNote>,
) -> Result<(StatusCode, Json<Note>), ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.create(req, caller)).await??;
    log_write("created", &n, caller, None);
    Ok((StatusCode::CREATED, Json(n)))
}

async fn get_note(
    State(st): State<AppState>,
    UrlPath(key): UrlPath<String>,
) -> Result<Json<Note>, ApiError> {
    let store = st.notes.clone();
    Ok(Json(blocking(move || store.get(&key)).await??))
}

async fn patch_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
    JsonBody(req): JsonBody<UpdateNote>,
) -> Result<Json<Note>, ApiError> {
    let source = req.source.clone();
    if let Some(s) = source.as_deref() {
        check_source(Some(s), "owner")?;
    }
    let store = st.notes.clone();
    let n = blocking(move || store.update(&key, req)).await??;
    log_write("updated", &n, caller, source.as_deref());
    Ok(Json(n))
}

async fn append_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
    JsonBody(req): JsonBody<AppendNote>,
) -> Result<Json<Note>, ApiError> {
    let source = req.source.clone();
    if let Some(s) = source.as_deref() {
        check_source(Some(s), "owner")?;
    }
    let store = st.notes.clone();
    let n = blocking(move || store.append(&key, req)).await??;
    log_write("appended", &n, caller, source.as_deref());
    Ok(Json(n))
}

async fn delete_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.delete(&key)).await??;
    log_write("moved to the trash", &n, caller, None);
    Ok(Json(json!({
        "deleted": true,
        "id": n.id,
        "slug": n.slug,
        "deletedAt": n.deleted_at,
        "purgeAfter": n.deleted_at.as_deref().and_then(purge_after),
    })))
}

async fn trash(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let notes = blocking(move || store.trash()).await?;
    Ok(Json(json!({ "notes": notes, "days": TRASH_DAYS })))
}

async fn restore(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Note>, ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.restore(&id)).await??;
    log_write("restored", &n, caller, None);
    Ok(Json(n))
}

/// The notes routes with their own auth (device or service token) and a
/// larger body limit than the rest of `/v1`.
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/notes", get(list).post(create))
        .route("/v1/notes/trash", get(trash))
        .route("/v1/notes/trash/{id}/restore", post(restore))
        .route(
            "/v1/notes/{key}",
            get(get_note).patch(patch_note).delete(delete_note),
        )
        .route("/v1/notes/{key}/append", post(append_note))
        .route_layer(middleware::from_fn_with_state(state, device_or_service))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// Shared handle for [`crate::Inner`].
pub fn store(data_dir: &Path) -> Arc<NoteStore> {
    let s = NoteStore::new(data_dir);
    if let Err(e) = ensure_dir(s.dir()) {
        tracing::warn!(dir = %s.dir().display(), error = %e, "notes dir not created yet");
    }
    Arc::new(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(store: &NoteStore, title: &str, slug: Option<&str>, content: &str) -> Note {
        store
            .create(
                CreateNote {
                    title: title.into(),
                    content: content.into(),
                    slug: slug.map(str::to_owned),
                    ..CreateNote::default()
                },
                Caller("web"),
            )
            .unwrap()
    }

    #[test]
    fn slugs_ids_and_tags() {
        assert_eq!(
            slugify("CRAFT-IT4SD — meeting with Marianne & Jonas"),
            "craft-it4sd-meeting-with-marianne-jonas"
        );
        assert_eq!(slugify("  Città già più  "), "citta-gia-piu");
        assert_eq!(slugify("Træning øvelser"), "traening-ovelser");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("Trash"), "trash-note");
        assert!(slugify(&"a".repeat(100)).len() <= MAX_SLUG_LEN);
        for ok in ["craftit-meeting", "a", "x1-2"] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "a--b",
            "A",
            "a_b",
            "a b",
            "trash",
            &"a".repeat(65),
        ] {
            assert!(!valid_slug(bad), "{bad:?}");
        }
        let id = new_id().unwrap();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("n_xyz") && !valid_id("craftit-meeting"));
        assert_eq!(
            normalize_tags(&["  Work ".into(), "#aarhus".into(), "work".into(), "".into()])
                .unwrap(),
            ["work", "aarhus"]
        );
        assert!(normalize_tags(&["a/b".into()]).is_err());
        assert!(normalize_tags(&(0..17).map(|i| format!("t{i}")).collect::<Vec<_>>()).is_err());
        assert!(valid_source("claude-code") && !valid_source("Claude") && !valid_source(""));
    }

    #[test]
    fn create_find_update_append() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::new(dir.path());
        let a = create(
            &store,
            "CRAFT meeting",
            Some("craftit-meeting"),
            "Marianne & Jonas",
        );
        assert_eq!(
            (a.slug.as_str(), a.source.as_str()),
            ("craftit-meeting", "owner")
        );
        assert_eq!(store.get("craftit-meeting").unwrap(), a);
        assert_eq!(store.get("CRAFTIT-MEETING").unwrap().id, a.id);
        assert_eq!(store.get(&a.id).unwrap(), a);
        let e = store
            .create(
                CreateNote {
                    title: "x".into(),
                    slug: Some("craftit-meeting".into()),
                    ..CreateNote::default()
                },
                Caller("web"),
            )
            .unwrap_err();
        assert_eq!(
            (e.status, e.code.as_str()),
            (StatusCode::CONFLICT, "slug_taken")
        );
        let b = create(&store, "CRAFT meeting", None, "");
        assert_eq!(b.slug, "craft-meeting");
        let c = create(&store, "CRAFT meeting", None, "");
        assert_eq!(c.slug, "craft-meeting-2");

        let n = store
            .append(
                "craftit-meeting",
                AppendNote {
                    content: "\npart 2".into(),
                    ..AppendNote::default()
                },
            )
            .unwrap();
        assert_eq!(n.content, "Marianne & Jonas\npart 2");
        let n = store
            .append(
                &a.id,
                AppendNote {
                    content: "part 3".into(),
                    separator: Some("\n\n".into()),
                    ..AppendNote::default()
                },
            )
            .unwrap();
        assert_eq!(n.content, "Marianne & Jonas\npart 2\n\npart 3");

        let stale = UpdateNote {
            title: Some("New".into()),
            base_updated_at: Some("2000-01-01T00:00:00Z".into()),
            ..UpdateNote::default()
        };
        assert_eq!(store.update(&a.id, stale).unwrap_err().code, "conflict");
        let u = store
            .update(
                &a.id,
                UpdateNote {
                    title: Some("New".into()),
                    tags: Some(vec!["Work".into()]),
                    pinned: Some(true),
                    base_updated_at: Some(n.updated_at.clone()),
                    ..UpdateNote::default()
                },
            )
            .unwrap();
        assert_eq!((u.title.as_str(), u.pinned), ("New", true));
        assert_eq!(u.tags, ["work"]);
        assert_eq!(u.content, n.content, "absent fields are kept");
        let e = store
            .update(
                &a.id,
                UpdateNote {
                    slug: Some("craft-meeting".into()),
                    ..UpdateNote::default()
                },
            )
            .unwrap_err();
        assert_eq!(e.code, "slug_taken");

        // Pinned first, then newest; search and tag filters.
        let l = store.list(None, None, 10);
        assert_eq!(l[0].id, a.id);
        assert_eq!(l.len(), 3);
        assert_eq!(store.list(Some("JONAS"), None, 10).len(), 1);
        assert_eq!(store.list(Some("craft"), None, 10).len(), 3);
        assert_eq!(store.list(None, Some("work"), 10).len(), 1);
        assert_eq!(store.list(Some("nothing here"), None, 10).len(), 0);
        let hit = &store.list(Some("part 3"), None, 10)[0];
        assert!(hit.excerpt.contains("part 3"), "{}", hit.excerpt);
    }

    #[test]
    fn limits_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::new(dir.path());
        let big = "x".repeat(MAX_CONTENT_BYTES);
        let n = create(&store, "big", None, &big);
        let e = store
            .append(
                &n.id,
                AppendNote {
                    content: "y".into(),
                    ..AppendNote::default()
                },
            )
            .unwrap_err();
        assert_eq!(
            (e.status, e.code.as_str()),
            (StatusCode::PAYLOAD_TOO_LARGE, "note_too_large")
        );
        assert_eq!(store.get(&n.id).unwrap().content.len(), MAX_CONTENT_BYTES);
        for bad in [
            CreateNote {
                title: " ".into(),
                ..CreateNote::default()
            },
            CreateNote {
                title: "a\u{7}".into(),
                ..CreateNote::default()
            },
            CreateNote {
                title: "x".repeat(201),
                ..CreateNote::default()
            },
            CreateNote {
                title: "t".into(),
                content: "a\u{0}b".into(),
                ..CreateNote::default()
            },
            CreateNote {
                title: "t".into(),
                slug: Some("Bad Slug".into()),
                ..CreateNote::default()
            },
            CreateNote {
                title: "t".into(),
                source: Some("Chat GPT".into()),
                ..CreateNote::default()
            },
        ] {
            assert_eq!(
                store.create(bad, Caller("web")).unwrap_err().code,
                "bad_request"
            );
        }
        let n = create(&store, "crlf", None, "a\r\nb\rc");
        assert_eq!(n.content, "a\nb\nc");
        let lucina = store
            .create(
                CreateNote {
                    title: "from lucina".into(),
                    ..CreateNote::default()
                },
                Caller("lucina"),
            )
            .unwrap();
        assert_eq!(lucina.source, "lucina");
        assert_eq!(store.get("nope").unwrap_err().code, "note_not_found");
    }

    #[test]
    fn trash_restore_purge_and_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::new(dir.path());
        let a = create(&store, "Alpha", Some("alpha"), "one");
        let file = dir.path().join("notes").join(format!("{}.json", a.id));
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let d = store.delete("alpha").unwrap();
        assert!(d.deleted_at.is_some());
        assert!(!file.exists());
        assert!(store.get("alpha").is_err());
        let t = store.trash();
        assert_eq!(t.len(), 1);
        assert!(t[0].purge_after.is_some());
        // The slug is free again; a restore then gets a suffix.
        let b = create(&store, "Alpha again", Some("alpha"), "two");
        let r = store.restore(&a.id).unwrap();
        assert_eq!((r.slug.as_str(), r.content.as_str()), ("alpha-2", "one"));
        assert!(r.deleted_at.is_none());
        assert!(store.trash().is_empty());
        assert_eq!(store.get("alpha").unwrap().id, b.id);

        // Expired trash is purged.
        store.delete(&b.id).unwrap();
        let tfile = dir
            .path()
            .join("notes/trash")
            .join(format!("{}.json", b.id));
        let mut old: Note = serde_json::from_slice(&std::fs::read(&tfile).unwrap()).unwrap();
        old.deleted_at = Some("2020-01-01T00:00:00Z".into());
        std::fs::write(&tfile, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(store.trash().is_empty());
        assert!(!tfile.exists());

        // Another process (the CLI) writing a note is picked up.
        let other = NoteStore::new(dir.path());
        let c = create(&other, "From the CLI", None, "");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(store.get(&c.slug).unwrap().id, c.id);
    }

    #[test]
    fn seeds() {
        let md = "---\nslug: craftit-meeting\ntitle: CRAFT-IT4SD — meeting\ntags: work, aarhus\nsource: chatgpt\npinned: true\n---\n\n# Hello\n\nBody: with colon\n";
        let s = parse_seed(md, true).unwrap();
        assert_eq!(s.slug, "craftit-meeting");
        assert_eq!(s.tags, ["work", "aarhus"]);
        assert_eq!((s.source.as_str(), s.pinned), ("chatgpt", true));
        assert_eq!(s.content, "# Hello\n\nBody: with colon\n");
        assert!(parse_seed("no front matter", true).is_err());
        let j = parse_seed(r#"{"slug":"a","title":"A","content":"x"}"#, false).unwrap();
        assert_eq!(j.source, "owner");

        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::new(dir.path());
        let (n, created) = store.import(s.clone(), false).unwrap().unwrap();
        assert!(created && n.pinned && n.source == "chatgpt");
        assert!(store.import(s.clone(), false).unwrap().is_none(), "kept");
        let mut s2 = s;
        s2.content = "changed".into();
        let (n2, created) = store.import(s2, true).unwrap().unwrap();
        assert!(!created);
        assert_eq!((n2.id, n2.content.as_str()), (n.id, "changed"));
    }
}

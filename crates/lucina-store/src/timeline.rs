//! Owner conversation timeline, byte-compatible with Node's
//! `src/timeline.js` and `data/timeline.jsonl`.
//!
//! Storage: JSONL, one item per line, appended with `O_APPEND` (one write per
//! call, so concurrent writers keep whole lines), rotated before the file
//! would pass `max_bytes` into `<path>.1` and `<path>.2`. The in-memory index
//! holds `{id, generation, offset, length}` per item sorted by id (ULIDs sort
//! by time), plus a cache of recent raw lines. Lines appended by another
//! process (the Node Lucina, the backfill script) are picked up on the next
//! operation by re-scanning the file tail.
//!
//! Items are served as the raw stored line, so a Node-written item reaches a
//! client byte for byte. New items are serialized with the same key order
//! as `JSON.stringify` of Node's `normalizeTimelineItem`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::broadcast;

use crate::atomic::open_append;
use crate::timeutil::{clip_utf16, iso_ms, js_trim, now_ms, parse_js_date};
use crate::ulid::{is_timeline_id, new_id};
use crate::{Result, StoreError};

pub const CHANNELS: [&str; 6] = ["telegram", "discord", "web", "ios", "alexa", "other"];
pub const ROLES: [&str; 2] = ["user", "lucina"];
pub const MAX_TEXT_CHARS: usize = 8000;
pub const MAX_LIST_LIMIT: usize = 200;
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_KEEP_ROTATED: u64 = 2;
const DEFAULT_CACHE_SIZE: usize = 2000;
const MAX_ATTACHMENTS: usize = 10;
const MAX_NAME_CHARS: usize = 200;
const MAX_CLIENT_MESSAGE_ID_CHARS: usize = 128;
const MAX_META_STRING_CHARS: usize = 200;
const MAX_COMMAND_ARGS_CHARS: usize = 2000;
const ATTACHMENT_KINDS: [&str; 3] = ["image", "voice", "file"];

/// `{kind, name?}`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Item `meta`, in Node's key order: the string keys, then `isDm`, `ok`,
/// `command`. `tool` is this implementation's extension (tool items).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// With `kind: "avatar_outfit"`: the outfit Lucina put on (none = back to automatic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outfit: Option<String>,
    /// With `kind: "avatar_outfit"`: the hairstyle Lucina put on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hair: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_dm: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl Meta {
    pub fn kind(kind: &str) -> Self {
        Self {
            kind: Some(kind.to_owned()),
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.chat_title.is_none()
            && self.guild_name.is_none()
            && self.channel_name.is_none()
            && self.kind.is_none()
            && self.job_id.is_none()
            && self.list_id.is_none()
            && self.command_id.is_none()
            && self.outfit.is_none()
            && self.hair.is_none()
            && self.is_dm.is_none()
            && self.ok.is_none()
            && self.command.is_none()
            && self.tool.is_none()
    }

    /// Later keys win (`{...a, ...b}`).
    pub fn merge(mut self, other: Meta) -> Meta {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            chat_title,
            guild_name,
            channel_name,
            kind,
            job_id,
            list_id,
            command_id,
            outfit,
            hair,
            is_dm,
            ok,
            command,
            tool
        );
        self
    }
}

/// One stored item (Node field order).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    pub id: String,
    pub ts: String,
    pub channel: String,
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfilled: Option<bool>,
}

/// Input of [`TimelineStore::append`] (validated by [`normalize`]).
#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub id: Option<String>,
    pub ts: Option<String>,
    pub channel: String,
    pub role: String,
    pub text: String,
    pub client_message_id: Option<String>,
    pub reply_to: Option<String>,
    pub attachments: Vec<Attachment>,
    pub meta: Option<Meta>,
    pub backfilled: bool,
}

fn clean_string(value: Option<&str>, max: usize) -> Option<String> {
    let v = clip_utf16(js_trim(value?), max);
    (!v.is_empty()).then_some(v)
}

fn normalize_command(raw: &RawValue) -> Option<Box<RawValue>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Cmd<'a> {
        id: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        #[serde(borrow)]
        args: Option<&'a RawValue>,
        expires_at: Option<String>,
        requires_confirmation: Option<serde_json::Value>,
    }
    let cmd: Cmd = serde_json::from_str(raw.get()).ok()?;
    let id = clean_string(cmd.id.as_deref(), 64)?;
    let kind = clean_string(cmd.kind.as_deref(), 40)?;
    let (head, tail) = kind.split_once('.')?;
    let valid_type = !head.is_empty()
        && head.bytes().all(|b| b.is_ascii_lowercase())
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if !valid_type {
        return None;
    }
    let args = cmd
        .args
        .filter(|a| {
            a.get().trim_start().starts_with('{') && a.get().len() <= MAX_COMMAND_ARGS_CHARS
        })
        .map(|a| a.get().to_owned())
        .unwrap_or_else(|| "{}".into());
    let expires = cmd
        .expires_at
        .as_deref()
        .and_then(parse_js_date)
        .map(|d| serde_json::Value::String(iso_ms(d)))
        .unwrap_or(serde_json::Value::Null);
    let requires = matches!(
        cmd.requires_confirmation,
        Some(serde_json::Value::Bool(true))
    );
    let text = format!(
        r#"{{"id":{},"type":{},"args":{},"expiresAt":{},"requiresConfirmation":{}}}"#,
        serde_json::to_string(&id).ok()?,
        serde_json::to_string(&kind).ok()?,
        args,
        expires,
        requires
    );
    RawValue::from_string(text).ok()
}

fn normalize_meta(meta: Option<Meta>) -> Option<Meta> {
    let m = meta?;
    let s = |v: Option<String>| clean_string(v.as_deref(), MAX_META_STRING_CHARS);
    let out = Meta {
        chat_title: s(m.chat_title),
        guild_name: s(m.guild_name),
        channel_name: s(m.channel_name),
        kind: s(m.kind),
        job_id: s(m.job_id),
        list_id: s(m.list_id),
        command_id: s(m.command_id),
        outfit: s(m.outfit),
        hair: s(m.hair),
        is_dm: m.is_dm,
        ok: m.ok,
        command: m.command.as_deref().and_then(normalize_command),
        tool: s(m.tool),
    };
    (!out.is_empty()).then_some(out)
}

/// Validate and normalize like Node's `normalizeTimelineItem`: a known
/// channel and role, text or attachments, text clipped to 8000 characters.
pub fn normalize(input: NewItem, now: i64) -> Result<TimelineItem> {
    if !CHANNELS.contains(&input.channel.as_str()) {
        return Err(StoreError::Invalid(format!(
            "timeline: invalid channel {:?}",
            input.channel
        )));
    }
    if !ROLES.contains(&input.role.as_str()) {
        return Err(StoreError::Invalid(format!(
            "timeline: invalid role {:?}",
            input.role
        )));
    }
    let text = clip_utf16(js_trim(&input.text), MAX_TEXT_CHARS);
    let attachments: Vec<Attachment> = input
        .attachments
        .into_iter()
        .filter(|a| ATTACHMENT_KINDS.contains(&a.kind.as_str()))
        .take(MAX_ATTACHMENTS)
        .map(|a| Attachment {
            name: clean_string(a.name.as_deref(), MAX_NAME_CHARS),
            kind: a.kind,
        })
        .collect();
    if text.is_empty() && attachments.is_empty() {
        return Err(StoreError::Invalid(
            "timeline: item has no text or attachments".into(),
        ));
    }
    let ts = input
        .ts
        .as_deref()
        .and_then(parse_js_date)
        .map(iso_ms)
        .unwrap_or_else(|| {
            iso_ms(chrono::DateTime::from_timestamp_millis(now).unwrap_or_default())
        });
    Ok(TimelineItem {
        id: input
            .id
            .filter(|id| is_timeline_id(id))
            .unwrap_or_else(|| new_id(now)),
        ts,
        channel: input.channel,
        role: input.role,
        text,
        client_message_id: clean_string(
            input.client_message_id.as_deref(),
            MAX_CLIENT_MESSAGE_ID_CHARS,
        ),
        reply_to: input.reply_to.filter(|id| is_timeline_id(id)),
        attachments: (!attachments.is_empty()).then_some(attachments),
        meta: normalize_meta(input.meta),
        backfilled: input.backfilled.then_some(true),
    })
}

// ---------------------------------------------------------------- store

/// A stored item: its id and the raw JSON line.
#[derive(Debug, Clone)]
pub struct StoredItem {
    pub id: String,
    pub raw: Arc<str>,
}

impl StoredItem {
    pub fn raw_value(&self) -> Box<RawValue> {
        RawValue::from_string(self.raw.to_string()).expect("stored lines are valid JSON")
    }
}

#[derive(Debug, Clone)]
struct Entry {
    id: String,
    gen: u64,
    offset: u64,
    length: usize,
}

struct Inner {
    file_path: PathBuf,
    max_bytes: u64,
    keep_rotated: u64,
    cache_size: usize,
    entries: Vec<Entry>,
    by_id: HashSet<String>,
    cache: HashMap<String, Arc<str>>,
    cache_order: VecDeque<String>,
    current_gen: u64,
    known_size: u64,
    known_ino: Option<u64>,
    needs_leading_newline: bool,
    loaded: bool,
}

#[cfg(unix)]
fn ino_of(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn ino_of(_meta: &fs::Metadata) -> u64 {
    0
}

/// Read from `start` to the end. Missing file: empty.
fn read_range(path: &Path, start: u64) -> Result<(Vec<u8>, u64, Option<u64>)> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0, None)),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    let size = meta.len();
    let mut buf = Vec::with_capacity(size.saturating_sub(start) as usize);
    if size > start {
        file.seek(SeekFrom::Start(start))?;
        file.take(size - start).read_to_end(&mut buf)?;
    }
    Ok((buf.clone(), start + buf.len() as u64, Some(ino_of(&meta))))
}

#[derive(Deserialize)]
struct IdOnly {
    id: serde_json::Value,
}

impl Inner {
    fn path_for_gen(&self, gen: u64) -> PathBuf {
        let suffix = self.current_gen - gen;
        if suffix == 0 {
            self.file_path.clone()
        } else {
            PathBuf::from(format!("{}.{suffix}", self.file_path.display()))
        }
    }

    fn cache_item(&mut self, id: &str, raw: Arc<str>) {
        if self.cache.insert(id.to_owned(), raw).is_some() {
            self.cache_order.retain(|k| k != id);
        }
        self.cache_order.push_back(id.to_owned());
        while self.cache.len() > self.cache_size {
            if let Some(old) = self.cache_order.pop_front() {
                self.cache.remove(&old);
            } else {
                break;
            }
        }
    }

    fn insert_entry(&mut self, entry: Entry) -> bool {
        if self.by_id.contains(&entry.id) {
            return false;
        }
        self.by_id.insert(entry.id.clone());
        match self.entries.last() {
            Some(last) if last.id >= entry.id => {
                let pos = self.entries.partition_point(|e| e.id < entry.id);
                self.entries.insert(pos, entry);
            }
            _ => self.entries.push(entry),
        }
        true
    }

    /// Index the complete lines of `buffer` (which starts at byte `base`).
    fn index_buffer(&mut self, buffer: &[u8], base: u64, gen: u64) -> (u64, Vec<StoredItem>) {
        let mut line_start = 0usize;
        let mut boundary = 0usize;
        let mut added = Vec::new();
        for (i, b) in buffer.iter().enumerate() {
            if *b != b'\n' {
                continue;
            }
            let line = &buffer[line_start..i];
            if !line.is_empty() {
                if let (
                    Ok(text),
                    Ok(IdOnly {
                        id: serde_json::Value::String(id),
                    }),
                ) = (
                    std::str::from_utf8(line),
                    serde_json::from_slice::<IdOnly>(line),
                ) {
                    if is_timeline_id(&id) {
                        let entry = Entry {
                            id: id.clone(),
                            gen,
                            offset: base + line_start as u64,
                            length: line.len(),
                        };
                        if self.insert_entry(entry) {
                            added.push(StoredItem {
                                id,
                                raw: Arc::from(text),
                            });
                        }
                    }
                }
            }
            line_start = i + 1;
            boundary = i + 1;
        }
        (base + boundary as u64, added)
    }

    fn load_all(&mut self) -> Result<()> {
        self.entries.clear();
        self.by_id.clear();
        self.cache.clear();
        self.cache_order.clear();
        self.known_size = 0;
        self.known_ino = None;
        self.needs_leading_newline = false;
        for suffix in (0..=self.keep_rotated).rev() {
            let gen = self.current_gen - suffix;
            let (buffer, size, ino) = read_range(&self.path_for_gen(gen), 0)?;
            let (boundary, added) = self.index_buffer(&buffer, 0, gen);
            let skip = added.len().saturating_sub(self.cache_size);
            for item in added.into_iter().skip(skip) {
                self.cache_item(&item.id, item.raw);
            }
            if suffix == 0 {
                self.known_size = boundary;
                self.known_ino = ino;
                self.needs_leading_newline = size > boundary;
            }
        }
        self.loaded = true;
        Ok(())
    }

    /// Pick up lines appended by anyone since the last scan.
    fn refresh(&mut self) -> Result<Vec<StoredItem>> {
        if !self.loaded {
            self.load_all()?;
            return Ok(Vec::new());
        }
        let meta = match fs::metadata(&self.file_path) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let Some(meta) = meta else {
            if self.known_size > 0 || self.known_ino.is_some() {
                self.load_all()?;
            }
            return Ok(Vec::new());
        };
        let ino = ino_of(&meta);
        if self.known_ino.is_some_and(|k| k != ino) || meta.len() < self.known_size {
            // Rotated or truncated by another process: rebuild.
            self.load_all()?;
            return Ok(Vec::new());
        }
        if meta.len() == self.known_size {
            self.known_ino = Some(ino);
            return Ok(Vec::new());
        }
        let (buffer, size, ino) = read_range(&self.file_path, self.known_size)?;
        let (boundary, added) = self.index_buffer(&buffer, self.known_size, self.current_gen);
        self.known_size = boundary;
        self.known_ino = ino;
        self.needs_leading_newline = size > boundary;
        for item in &added {
            self.cache_item(&item.id, item.raw.clone());
        }
        Ok(added)
    }

    fn rotate(&mut self) -> Result<()> {
        for suffix in (1..=self.keep_rotated).rev() {
            let from = if suffix == 1 {
                self.file_path.clone()
            } else {
                PathBuf::from(format!("{}.{}", self.file_path.display(), suffix - 1))
            };
            let to = PathBuf::from(format!("{}.{suffix}", self.file_path.display()));
            match fs::rename(&from, &to) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.current_gen += 1;
        let oldest = self.current_gen - self.keep_rotated;
        self.entries.retain(|e| e.gen >= oldest);
        self.by_id = self.entries.iter().map(|e| e.id.clone()).collect();
        let by_id = &self.by_id;
        self.cache.retain(|k, _| by_id.contains(k));
        self.cache_order.retain(|k| by_id.contains(k));
        self.known_size = 0;
        self.known_ino = None;
        self.needs_leading_newline = false;
        Ok(())
    }

    fn write_lines(&mut self, lines: &str) -> Result<()> {
        let bytes = lines.len() as u64;
        if self.known_size > 0 && self.known_size + bytes > self.max_bytes {
            self.rotate()?;
        }
        let mut file = open_append(&self.file_path)?;
        let payload = if self.needs_leading_newline {
            format!("\n{lines}")
        } else {
            lines.to_owned()
        };
        // One write per call: O_APPEND keeps concurrent writers' lines whole.
        file.write_all(payload.as_bytes())?;
        self.needs_leading_newline = false;
        self.refresh()?;
        Ok(())
    }

    fn read_entries(&self, selected: &[Entry]) -> Result<Vec<StoredItem>> {
        let mut out = Vec::with_capacity(selected.len());
        let mut handles: HashMap<u64, Option<fs::File>> = HashMap::new();
        for entry in selected {
            if let Some(raw) = self.cache.get(&entry.id) {
                out.push(StoredItem {
                    id: entry.id.clone(),
                    raw: raw.clone(),
                });
                continue;
            }
            let handle = handles
                .entry(entry.gen)
                .or_insert_with(|| fs::File::open(self.path_for_gen(entry.gen)).ok());
            let Some(file) = handle.as_mut() else {
                continue;
            };
            let mut buf = vec![0u8; entry.length];
            if file.seek(SeekFrom::Start(entry.offset)).is_err()
                || file.read_exact(&mut buf).is_err()
            {
                continue;
            }
            if let Ok(text) = String::from_utf8(buf) {
                if serde_json::from_str::<IdOnly>(&text).is_ok() {
                    out.push(StoredItem {
                        id: entry.id.clone(),
                        raw: Arc::from(text),
                    });
                }
            }
        }
        Ok(out)
    }
}

/// The timeline store. Cheap to share behind an `Arc`.
pub struct TimelineStore {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<StoredItem>,
    recording: bool,
}

impl std::fmt::Debug for TimelineStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineStore")
            .field("recording", &self.recording)
            .finish()
    }
}

impl TimelineStore {
    /// `recording: false` (`LUCINA_TIMELINE_ENABLED=false`) stops new
    /// turn items; the store still serves what is stored.
    pub fn new(file_path: impl Into<PathBuf>, max_bytes: u64, recording: bool) -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            inner: Mutex::new(Inner {
                file_path: file_path.into(),
                max_bytes: max_bytes.max(1),
                keep_rotated: DEFAULT_KEEP_ROTATED,
                cache_size: DEFAULT_CACHE_SIZE,
                entries: Vec::new(),
                by_id: HashSet::new(),
                cache: HashMap::new(),
                cache_order: VecDeque::new(),
                current_gen: DEFAULT_KEEP_ROTATED,
                known_size: 0,
                known_ino: None,
                needs_leading_newline: false,
                loaded: false,
            }),
            tx,
            recording,
        }
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append one item and notify live subscribers. Returns the stored item.
    pub fn append(&self, input: NewItem) -> Result<TimelineItem> {
        let item = normalize(input, now_ms())?;
        let line = serde_json::to_string(&item)?;
        {
            let mut inner = self.lock();
            inner.refresh()?;
            if inner.by_id.contains(&item.id) {
                return Ok(item);
            }
            inner.write_lines(&format!("{line}\n"))?;
            let raw: Arc<str> = Arc::from(line.as_str());
            inner.cache_item(&item.id, raw.clone());
            let _ = self.tx.send(StoredItem {
                id: item.id.clone(),
                raw,
            });
        }
        Ok(item)
    }

    /// Newest first; `before` is an exclusive id cursor. Returns
    /// `(items, nextBefore)`.
    pub fn list(
        &self,
        before: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<StoredItem>, Option<String>)> {
        let size = limit.clamp(1, MAX_LIST_LIMIT);
        let mut inner = self.lock();
        inner.refresh()?;
        let end = match before {
            Some(b) if !b.is_empty() => inner.entries.partition_point(|e| e.id.as_str() < b),
            _ => inner.entries.len(),
        };
        let start = end.saturating_sub(size);
        let selected = inner.entries[start..end].to_vec();
        let mut items = inner.read_entries(&selected)?;
        items.reverse();
        let next = (start > 0 && !items.is_empty()).then(|| items[items.len() - 1].id.clone());
        Ok((items, next))
    }

    /// Items with an id strictly after `after_id`, oldest first (SSE replay).
    pub fn after(&self, after_id: &str, limit: usize) -> Result<Vec<StoredItem>> {
        let mut inner = self.lock();
        inner.refresh()?;
        let start = inner.entries.partition_point(|e| e.id.as_str() <= after_id);
        let end = (start + limit).min(inner.entries.len());
        let selected = inner.entries[start..end].to_vec();
        inner.read_entries(&selected)
    }

    /// Every stored item, oldest first.
    pub fn read_all(&self) -> Result<Vec<StoredItem>> {
        let mut inner = self.lock();
        inner.refresh()?;
        let selected = inner.entries.clone();
        inner.read_entries(&selected)
    }

    pub fn len(&self) -> Result<usize> {
        let mut inner = self.lock();
        inner.refresh()?;
        Ok(inner.entries.len())
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Live items appended through this store.
    pub fn subscribe(&self) -> broadcast::Receiver<StoredItem> {
        self.tx.subscribe()
    }

    /// Append; never fails (a failed write is logged and yields `None`).
    pub fn record(&self, input: NewItem) -> Option<TimelineItem> {
        if !self.recording {
            return None;
        }
        let channel = input.channel.clone();
        let role = input.role.clone();
        match self.append(input) {
            Ok(item) => Some(item),
            Err(e) => {
                tracing::warn!(%channel, %role, error = %e, "timeline append failed");
                None
            }
        }
    }
}

/// One owner turn on one channel: the owner's message and Lucina's replies
/// (`replyTo` = the owner's item). Shared by every surface.
pub struct TimelineTurn {
    store: Arc<TimelineStore>,
    channel: String,
    client_message_id: Option<String>,
    user_item: Mutex<Option<Option<TimelineItem>>>,
}

impl TimelineTurn {
    pub fn new(
        store: Arc<TimelineStore>,
        channel: &str,
        client_message_id: Option<String>,
    ) -> Self {
        Self {
            store,
            channel: channel.to_owned(),
            client_message_id,
            user_item: Mutex::new(None),
        }
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Write the owner's message (once). Returns the item.
    pub fn record_user(&self, text: &str, attachments: Vec<Attachment>) -> Option<TimelineItem> {
        let mut slot = self.user_item.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = slot.as_ref() {
            return existing.clone();
        }
        let item = self.store.record(NewItem {
            channel: self.channel.clone(),
            role: "user".into(),
            text: text.to_owned(),
            client_message_id: self.client_message_id.clone(),
            attachments,
            ..NewItem::default()
        });
        *slot = Some(item.clone());
        item
    }

    pub fn user_item_id(&self) -> Option<String> {
        self.user_item
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|i| i.as_ref().map(|i| i.id.clone()))
    }

    /// A Lucina item replying to the owner's message.
    pub fn record_lucina(
        &self,
        text: &str,
        attachments: Vec<Attachment>,
        meta: Option<Meta>,
    ) -> Option<TimelineItem> {
        self.store.record(NewItem {
            channel: self.channel.clone(),
            role: "lucina".into(),
            text: text.to_owned(),
            attachments,
            meta,
            reply_to: self.user_item_id(),
            ..NewItem::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path, max: u64) -> TimelineStore {
        TimelineStore::new(dir.join("timeline.jsonl"), max, true)
    }

    fn item(text: &str) -> NewItem {
        NewItem {
            channel: "web".into(),
            role: "user".into(),
            text: text.into(),
            ..NewItem::default()
        }
    }

    #[test]
    fn appends_lists_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), DEFAULT_MAX_BYTES);
        let ids: Vec<String> = (0..5)
            .map(|i| s.append(item(&format!("m{i}"))).unwrap().id)
            .collect();
        let (page, next) = s.list(None, 2).unwrap();
        assert_eq!(
            page.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
            vec![ids[4].clone(), ids[3].clone()]
        );
        assert_eq!(next.as_deref(), Some(ids[3].as_str()));
        let (page, next) = s.list(next.as_deref(), 10).unwrap();
        assert_eq!(page.len(), 3);
        assert!(next.is_none());
        let after = s.after(&ids[1], 500).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].id, ids[2]);
    }

    #[test]
    fn serialization_matches_node_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), DEFAULT_MAX_BYTES);
        let item = s
            .append(NewItem {
                channel: "ios".into(),
                role: "lucina".into(),
                text: "  ciao \"x\"\n ".into(),
                client_message_id: Some("c1".into()),
                reply_to: Some("01K83ZK4Q6X1V9W2D3E4F5G6H7".into()),
                attachments: vec![
                    Attachment {
                        kind: "image".into(),
                        name: None,
                    },
                    Attachment {
                        kind: "bogus".into(),
                        name: None,
                    },
                ],
                meta: Some(Meta {
                    is_dm: Some(true),
                    kind: Some("tool".into()),
                    ..Meta::default()
                }),
                ts: Some("2026-10-21T16:30:02.118Z".into()),
                ..NewItem::default()
            })
            .unwrap();
        let line = fs::read_to_string(dir.path().join("timeline.jsonl")).unwrap();
        assert_eq!(
            line,
            format!(
                "{{\"id\":\"{}\",\"ts\":\"2026-10-21T16:30:02.118Z\",\"channel\":\"ios\",\"role\":\"lucina\",\"text\":\"ciao \\\"x\\\"\",\"clientMessageId\":\"c1\",\"replyTo\":\"01K83ZK4Q6X1V9W2D3E4F5G6H7\",\"attachments\":[{{\"kind\":\"image\"}}],\"meta\":{{\"kind\":\"tool\",\"isDm\":true}}}}\n",
                item.id
            )
        );
    }

    #[test]
    fn rejects_bad_items() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), DEFAULT_MAX_BYTES);
        assert!(s
            .append(NewItem {
                channel: "sms".into(),
                ..item("x")
            })
            .is_err());
        assert!(s.append(item("   ")).is_err());
        let long = "é".repeat(9000);
        assert_eq!(s.append(item(&long)).unwrap().text.chars().count(), 8000);
    }

    #[test]
    fn picks_up_foreign_lines_partial_lines_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeline.jsonl");
        let s = store(dir.path(), 600);
        let a = s.append(item("first")).unwrap();
        // another process appends a line, then a partial line
        let foreign = normalize(item("foreign"), now_ms()).unwrap();
        let mut f = open_append(&path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&foreign).unwrap()).unwrap();
        write!(f, "{{\"id\":\"partial").unwrap();
        drop(f);
        assert_eq!(s.len().unwrap(), 2);
        // our next write starts on a fresh line
        let b = s.append(item("second")).unwrap();
        assert_eq!(s.len().unwrap(), 3);
        // a fresh store sees the same items
        let fresh = store(dir.path(), 600);
        let all: Vec<String> = fresh
            .read_all()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(all, vec![a.id.clone(), foreign.id.clone(), b.id.clone()]);
        // rotation keeps two old files and still serves their items
        for i in 0..20 {
            s.append(item(&format!("filler {i} {}", "x".repeat(40))))
                .unwrap();
        }
        assert!(dir.path().join("timeline.jsonl.1").exists());
        let (latest, _) = s.list(None, 3).unwrap();
        assert!(latest[0].raw.contains("filler 19"));
        assert!(fs::metadata(&path).unwrap().len() <= 600);
    }

    #[test]
    fn turn_links_reply_to_user() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(store(dir.path(), DEFAULT_MAX_BYTES));
        let mut rx = s.subscribe();
        let turn = TimelineTurn::new(s.clone(), "web", Some("cm1".into()));
        let u = turn.record_user("hello", vec![]).unwrap();
        assert_eq!(turn.record_user("again", vec![]).unwrap().id, u.id);
        let l = turn.record_lucina("hi", vec![], None).unwrap();
        assert_eq!(l.reply_to.as_deref(), Some(u.id.as_str()));
        assert_eq!(u.client_message_id.as_deref(), Some("cm1"));
        assert_eq!(rx.try_recv().unwrap().id, u.id);
        assert_eq!(rx.try_recv().unwrap().id, l.id);
    }

    #[test]
    fn command_meta_is_normalized() {
        let cmd = RawValue::from_string(
            r#"{"id":"c-1","type":"alarm.set","args":{"time":"07:00","label":"Gym"},"expiresAt":"2026-10-21T08:10:00Z","requiresConfirmation":false,"x":1}"#.into(),
        )
        .unwrap();
        let out = normalize_command(&cmd).unwrap();
        assert_eq!(
            out.get(),
            r#"{"id":"c-1","type":"alarm.set","args":{"time":"07:00","label":"Gym"},"expiresAt":"2026-10-21T08:10:00.000Z","requiresConfirmation":false}"#
        );
        let bad = RawValue::from_string(r#"{"id":"c","type":"Alarm"}"#.into()).unwrap();
        assert!(normalize_command(&bad).is_none());
    }
}

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
/// A voice note's transcript, on its attachment and on the reply's meta.
pub const MAX_TRANSCRIPT_CHARS: usize = 4000;
/// A voice note's waveform: at most 64 levels (0–255, oldest first).
pub const MAX_WAVEFORM_BYTES: usize = 64;
const ATTACHMENT_KINDS: [&str; 11] = [
    "image", "voice", "file", "video", "audio", "pdf", "document", "link", "text", "contact",
    "location",
];

/// `{kind, name?}`, plus `mime`, `mediaId`, `url`, `bytes`, `width`, `height`,
/// `durationMs`, `thumbhash` on shared items, and `waveform`, `transcript` on
/// voice notes (docs/share.md).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// ThumbHash (base64) from lucina-media, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbhash: Option<String>,
    /// Voice notes: base64 of up to 64 levels (0–255, oldest first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waveform: Option<String>,
    /// Voice notes: the transcript, when known at receipt (on-device).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
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
    pub share_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
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
    /// `"mcp"` on Lucina MCP turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The MCP caller ("chatgpt", "claude-code", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    /// On MCP turns: the caller's `ask_lucina` conversation ("coordinator").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// With `kind: "decision"`: the decision's id (`dc-…`, lucina-server `decide`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// With `kind: "avatar_outfit"`: the outfit Lucina put on (none = back to automatic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outfit: Option<String>,
    /// With `kind: "avatar_outfit"`: the hairstyle Lucina put on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hair: Option<String>,
    /// With `kind: "note"`: the note she saved or read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// On Lucina's reply to a voice note: the text she answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_dm: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// On Lucina's reply to a voice note: the client may speak it (`/v1/tts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_reply: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Lucina message format v1 (`crate::rich`): `"v1"` plus the parts,
    /// validated again on write. `text` holds the plain fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bubbles: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<serde_json::Value>>,
}

impl Meta {
    pub fn kind(kind: &str) -> Self {
        Self {
            kind: Some(kind.to_owned()),
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.share_id.is_none()
            && self.intent.is_none()
            && self.chat_title.is_none()
            && self.guild_name.is_none()
            && self.channel_name.is_none()
            && self.kind.is_none()
            && self.job_id.is_none()
            && self.list_id.is_none()
            && self.via.is_none()
            && self.caller.is_none()
            && self.conversation.is_none()
            && self.decision_id.is_none()
            && self.device.is_none()
            && self.device_name.is_none()
            && self.command_id.is_none()
            && self.transcript.is_none()
            && self.voice_reply.is_none()
            && self.outfit.is_none()
            && self.hair.is_none()
            && self.slug.is_none()
            && self.is_dm.is_none()
            && self.ok.is_none()
            && self.command.is_none()
            && self.tool.is_none()
            && self.format.is_none()
    }

    /// The meta of a rich reply (merged onto the turn's own meta).
    pub fn rich(m: &crate::rich::RichMessage) -> Self {
        Self {
            format: Some(crate::rich::FORMAT.into()),
            bubbles: Some(m.bubbles.clone()),
            lang: m.lang.clone(),
            blocks: (!m.blocks.is_empty()).then(|| m.blocks.clone()),
            actions: (!m.actions.is_empty()).then(|| m.actions.clone()),
            ..Self::default()
        }
    }

    /// Later keys win (`{...a, ...b}`).
    pub fn merge(mut self, other: Meta) -> Meta {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            share_id,
            intent,
            chat_title,
            guild_name,
            channel_name,
            kind,
            job_id,
            list_id,
            via,
            caller,
            conversation,
            decision_id,
            device,
            device_name,
            command_id,
            transcript,
            outfit,
            hair,
            slug,
            is_dm,
            ok,
            voice_reply,
            command,
            tool,
            format,
            bubbles,
            lang,
            blocks,
            actions
        );
        self
    }
}

/// Secretary device identifiers are deliberately narrower than timeline ids.
pub fn valid_device(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn conversation_key(channel: &str, meta: Option<&Meta>) -> String {
    if meta.is_some_and(|m| m.via.as_deref() == Some("mcp")) {
        let caller: String = meta
            .and_then(|m| m.caller.as_deref())
            .unwrap_or("")
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
            .collect();
        return format!("agent:{}", if caller.is_empty() { "mcp" } else { &caller });
    }
    match channel {
        "web" | "ios" => match meta
            .and_then(|m| m.device.as_deref())
            .filter(|d| valid_device(d))
        {
            Some(device) => format!("device:{device}"),
            None => format!("legacy:{channel}"),
        },
        "telegram" | "discord" | "alexa" => channel.to_owned(),
        _ => "lucina".into(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub count: usize,
    pub last_id: String,
    pub last_ts: String,
    pub last_role: String,
    pub last_text: String,
}

#[derive(Debug, Clone, Default)]
pub enum ConversationFilter {
    #[default]
    All,
    Key(String),
    Mine {
        device: String,
        channel: String,
    },
}

impl ConversationFilter {
    fn matches(&self, key: &str) -> bool {
        match self {
            Self::All => true,
            Self::Key(k) => k == key,
            Self::Mine { device, channel } => {
                key == "lucina"
                    || key.strip_prefix("device:") == Some(device.as_str())
                    || key.strip_prefix("legacy:") == Some(channel.as_str())
            }
        }
    }
}

pub struct TimelinePage {
    pub items: Vec<StoredItem>,
    pub next_before: Option<String>,
    pub cursor: Option<String>,
    pub has_more: bool,
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

/// A voice note waveform as stored: base64 (padded or not) of 1 to 64 bytes,
/// trimmed. Anything else is dropped.
pub fn valid_waveform(s: &str) -> Option<String> {
    use base64::engine::{general_purpose, DecodePaddingMode, GeneralPurpose};
    use base64::Engine;
    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        general_purpose::PAD.with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let s = s.trim();
    // 64 bytes are 88 characters padded; a longer string cannot fit.
    if s.is_empty() || s.len() > 88 {
        return None;
    }
    let bytes = ENGINE.decode(s).ok()?;
    (!bytes.is_empty() && bytes.len() <= MAX_WAVEFORM_BYTES).then(|| s.to_owned())
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
        share_id: s(m.share_id),
        intent: s(m.intent),
        chat_title: s(m.chat_title),
        guild_name: s(m.guild_name),
        channel_name: s(m.channel_name),
        kind: s(m.kind),
        job_id: s(m.job_id),
        list_id: s(m.list_id),
        via: s(m.via),
        caller: s(m.caller),
        conversation: s(m.conversation),
        decision_id: s(m.decision_id),
        device: m.device.filter(|v| valid_device(v)),
        device_name: m.device_name.and_then(|v| {
            let clean: String = v.chars().filter(|c| !c.is_control()).collect();
            clean_string(Some(&clean), 64)
        }),
        outfit: s(m.outfit),
        hair: s(m.hair),
        slug: s(m.slug),
        command_id: s(m.command_id),
        transcript: clean_string(m.transcript.as_deref(), MAX_TRANSCRIPT_CHARS),
        is_dm: m.is_dm,
        ok: m.ok,
        voice_reply: m.voice_reply,
        command: m.command.as_deref().and_then(normalize_command),
        tool: s(m.tool),
        ..Meta::default()
    };
    // Rich reply parts: validated again here, the timeline is what the apps render.
    let out = if m.format.as_deref() == Some(crate::rich::FORMAT) {
        let input = serde_json::json!({
            "bubbles": m.bubbles,
            "blocks": m.blocks,
            "actions": m.actions,
            "lang": m.lang,
        });
        match crate::rich::normalize(&input) {
            Ok((rich, _)) => out.merge(Meta::rich(&rich)),
            Err(_) => out,
        }
    } else {
        out
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
    let share = input
        .meta
        .as_ref()
        .is_some_and(|m| m.kind.as_deref() == Some("share"));
    let text = clip_utf16(
        js_trim(&input.text),
        if share { 40_000 } else { MAX_TEXT_CHARS },
    );
    let attachments: Vec<Attachment> = input
        .attachments
        .into_iter()
        .filter(|a| ATTACHMENT_KINDS.contains(&a.kind.as_str()))
        .take(if share { 32 } else { MAX_ATTACHMENTS })
        .map(|a| Attachment {
            name: clean_string(a.name.as_deref(), MAX_NAME_CHARS),
            kind: a.kind,
            mime: clean_string(a.mime.as_deref(), 200),
            media_id: clean_string(a.media_id.as_deref(), 200),
            url: clean_string(a.url.as_deref(), 8000),
            bytes: a.bytes,
            width: a.width,
            height: a.height,
            duration_ms: a.duration_ms,
            thumbhash: clean_string(a.thumbhash.as_deref(), 100),
            waveform: a.waveform.as_deref().and_then(valid_waveform),
            transcript: clean_string(a.transcript.as_deref(), MAX_TRANSCRIPT_CHARS),
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
    key: usize,
    gen: u64,
    offset: u64,
    length: usize,
}

struct Inner {
    file_path: PathBuf,
    max_bytes: u64,
    keep_rotated: u64,
    cache_size: usize,
    /// Sorted by id (pages, `before` cursors).
    entries: Vec<Entry>,
    conversations: Vec<Conversation>,
    keys: HashMap<String, usize>,
    /// id → arrival sequence number.
    by_id: HashMap<String, u64>,
    /// `(seq, id)` in arrival order, which is file order: the order the
    /// lines were committed, whoever wrote them. Ids are not commit order (a
    /// slow writer's line can land behind a newer id), so the SSE replay and
    /// follow use this order (docs/verification.md, T1 fix A).
    arrival: VecDeque<(u64, String)>,
    next_seq: u64,
    cache: HashMap<String, Arc<str>>,
    cache_order: VecDeque<String>,
    current_gen: u64,
    known_size: u64,
    known_ino: Option<u64>,
    needs_leading_newline: bool,
    loaded: bool,
    /// The newest id this store appended (kept across reloads: the lines it
    /// was bumped past may be rotated or truncated away).
    last_appended: Option<String>,
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
    fn index_conversation(&mut self, item: &TimelineItem) -> usize {
        let key = conversation_key(&item.channel, item.meta.as_ref());
        let meta = item.meta.as_ref().cloned().unwrap_or_default();
        let kind = key.split(':').next().unwrap_or("lucina");
        let title = match kind {
            "device" => meta.device_name.clone().unwrap_or_else(|| key[7..].into()),
            "legacy" if item.channel == "ios" => "iPhone (before devices)".into(),
            "legacy" => "Web (before devices)".into(),
            "agent" => match &key[6..] {
                "chatgpt" => "ChatGPT",
                "claude" => "Claude",
                "claude-code" => "Claude Code",
                "grok" | "grok-bot" => "Grok Bot",
                "cursor" => "Cursor",
                "codex" => "Codex",
                _ => meta.caller.as_deref().unwrap_or("mcp"),
            }
            .into(),
            "telegram" => "Telegram".into(),
            "discord" => "Discord".into(),
            "alexa" => "Alexa".into(),
            _ => "Lucina".into(),
        };
        let index = *self.keys.entry(key.clone()).or_insert_with(|| {
            let index = self.conversations.len();
            self.conversations.push(Conversation {
                key: key.clone(),
                kind: kind.into(),
                title: title.clone(),
                channel: item.channel.clone(),
                device: meta.device.clone(),
                device_name: None,
                caller: meta.caller.clone(),
                count: 0,
                last_id: String::new(),
                last_ts: String::new(),
                last_role: String::new(),
                last_text: String::new(),
            });
            index
        });
        let summary = &mut self.conversations[index];
        summary.count += 1;
        if let Some(name) = meta.device_name.filter(|_| summary.kind == "device") {
            summary.title = name.clone();
            summary.device_name = Some(name);
        }
        if item.id > summary.last_id {
            summary.last_id.clone_from(&item.id);
            summary.last_ts.clone_from(&item.ts);
            summary.last_role.clone_from(&item.role);
            summary.last_text = clip_utf16(
                &item.text.split_whitespace().collect::<Vec<_>>().join(" "),
                160,
            );
        }
        index
    }

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
        if self.by_id.contains_key(&entry.id) {
            return false;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.by_id.insert(entry.id.clone(), seq);
        self.arrival.push_back((seq, entry.id.clone()));
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
                        if self.by_id.contains_key(&id) {
                            line_start = i + 1;
                            boundary = i + 1;
                            continue;
                        }
                        let item =
                            serde_json::from_slice::<TimelineItem>(line).unwrap_or_else(|_| {
                                TimelineItem {
                                    id: id.clone(),
                                    ts: String::new(),
                                    channel: "other".into(),
                                    role: String::new(),
                                    text: String::new(),
                                    client_message_id: None,
                                    reply_to: None,
                                    attachments: None,
                                    meta: None,
                                    backfilled: None,
                                }
                            });
                        let key = self.index_conversation(&item);
                        let entry = Entry {
                            key,
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
        self.conversations.clear();
        self.keys.clear();
        self.by_id.clear();
        // Rebuilt in file order (oldest generation first): the arrival order.
        self.arrival.clear();
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

    /// Rebuild the index after another process rotated or truncated the
    /// file; returns the lines it had not indexed before, in arrival order,
    /// so they are published like any other new line (a stream following by
    /// arrival must see them before anything that arrives later).
    fn reload(&mut self) -> Result<Vec<StoredItem>> {
        let before: HashSet<String> = self.by_id.keys().cloned().collect();
        self.load_all()?;
        let fresh: Vec<Entry> = self
            .arrival
            .iter()
            .filter(|(_, id)| !before.contains(id))
            .filter_map(|(_, id)| self.entry(id).cloned())
            .collect();
        self.read_entries(&fresh)
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
                return self.reload();
            }
            return Ok(Vec::new());
        };
        let ino = ino_of(&meta);
        if self.known_ino.is_some_and(|k| k != ino) || meta.len() < self.known_size {
            // Rotated or truncated by another process: rebuild.
            return self.reload();
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
        self.load_all()?;
        Ok(())
    }

    fn write_lines(&mut self, lines: &str) -> Result<Vec<StoredItem>> {
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
        self.refresh()
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

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.id.as_str().cmp(id))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Up to `limit` entries that arrived after `after`, in arrival order
    /// (T1 fix A). An id the index no longer holds (rotated away, or from
    /// another store) falls back to "ids after it", still in arrival order;
    /// `None` is everything.
    fn after_arrival(&self, after: Option<&str>, limit: usize) -> Vec<Entry> {
        let ids: Vec<&String> = match after.map(|a| (a, self.by_id.get(a))) {
            None => self.arrival.iter().map(|(_, id)| id).take(limit).collect(),
            Some((_, Some(&seq))) => {
                let start = self.arrival.partition_point(|(s, _)| *s <= seq);
                self.arrival
                    .range(start..)
                    .map(|(_, id)| id)
                    .take(limit)
                    .collect()
            }
            Some((a, None)) => self
                .arrival
                .iter()
                .map(|(_, id)| id)
                .filter(|id| id.as_str() > a)
                .take(limit)
                .collect(),
        };
        ids.into_iter()
            .filter_map(|id| self.entry(id).cloned())
            .collect()
    }
}

/// Exclusive advisory lock on `<path>.lock` (released on drop).
fn lock_sidecar(path: &Path) -> Result<fs::File> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    if let Some(dir) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(&lock_path)?;
    f.lock()?;
    Ok(f)
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
                conversations: Vec::new(),
                keys: HashMap::new(),
                by_id: HashMap::new(),
                arrival: VecDeque::new(),
                next_seq: 0,
                cache: HashMap::new(),
                cache_order: VecDeque::new(),
                current_gen: DEFAULT_KEEP_ROTATED,
                known_size: 0,
                known_ino: None,
                needs_leading_newline: false,
                loaded: false,
                last_appended: None,
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

    /// Send newly indexed lines (this process's or another writer's) to the
    /// live subscribers, once each.
    fn publish(&self, items: Vec<StoredItem>) {
        for it in items {
            let _ = self.tx.send(it);
        }
    }

    /// Index lines other writers appended and publish them (SSE follow).
    pub fn poll(&self) -> Result<()> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        Ok(())
    }

    /// Append one item and notify live subscribers. Returns the stored item.
    ///
    /// Shared-writer rules (the Node Lucina appends to the same file): one
    /// `O_APPEND` write per line; an advisory `flock` on `<path>.lock` around
    /// refresh + append + rotation for every Rust writer; a generated id is
    /// moved past the newest id already in the file, so Rust lines never sort
    /// before lines that were written first.
    pub fn append(&self, input: NewItem) -> Result<TimelineItem> {
        let explicit_id = input.id.as_deref().is_some_and(is_timeline_id);
        let mut item = normalize(input, now_ms())?;
        // The timeline is a sink: no secret reaches the file or the live stream.
        lucina_secrets::redactor().redact_string(&mut item.text);
        let mut inner = self.lock();
        let _flock = lock_sidecar(&inner.file_path)?;
        let found = inner.refresh()?;
        self.publish(found);
        if inner.by_id.contains_key(&item.id) {
            return Ok(item);
        }
        if !explicit_id {
            // Past the newest id in the file and the newest this store wrote
            // (Verus kernel `choose_id` in verification/verus/timeline_ids.rs).
            let floor = match (inner.entries.last(), &inner.last_appended) {
                (Some(e), Some(l)) => Some(e.id.clone().max(l.clone())),
                (Some(e), None) => Some(e.id.clone()),
                (None, l) => l.clone(),
            };
            if let Some(f) = floor {
                if item.id <= f {
                    item.id = crate::ulid::successor(&f).ok_or_else(|| {
                        StoreError::Invalid(format!("timeline: no id sorts after {f}"))
                    })?;
                }
            }
        }
        let line = serde_json::to_string(&item)?;
        let added = inner.write_lines(&format!("{line}\n"))?;
        if inner
            .last_appended
            .as_deref()
            .is_none_or(|l| item.id.as_str() > l)
        {
            inner.last_appended = Some(item.id.clone());
        }
        inner.cache_item(&item.id, Arc::from(line.as_str()));
        self.publish(added);
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
        let found = inner.refresh()?;
        self.publish(found);
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

    /// Filter using interned conversation ids; only returned lines are read.
    pub fn page(
        &self,
        before: Option<&str>,
        after: Option<&str>,
        limit: usize,
        filter: &ConversationFilter,
    ) -> Result<TimelinePage> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let size = limit.clamp(1, MAX_LIST_LIMIT);
        let mut cursor = inner.arrival.back().map(|(_, id)| id.clone());
        let matches = |e: &&Entry| filter.matches(&inner.conversations[e.key].key);
        let (mut selected, next_before, has_more) = if let Some(after) = after {
            let seq = inner.by_id.get(after).ok_or(StoreError::CursorExpired)?;
            let start = inner.arrival.partition_point(|(s, _)| s <= seq);
            let mut selected = Vec::new();
            let mut scanned = start;
            for (_, id) in inner.arrival.range(start..) {
                scanned += 1;
                let entry = inner.entry(id).expect("arrival is indexed");
                if matches(&entry) {
                    selected.push(entry.clone());
                }
                if selected.len() == size {
                    cursor = Some(id.clone());
                    break;
                }
            }
            let more = scanned < inner.arrival.len();
            if !more {
                cursor = inner.arrival.back().map(|(_, id)| id.clone());
            }
            (selected, None, more)
        } else {
            let end = before.map_or(inner.entries.len(), |b| {
                inner.entries.partition_point(|e| e.id.as_str() < b)
            });
            let mut candidates = inner.entries[..end].iter().rev().filter(matches);
            let selected: Vec<Entry> = candidates.by_ref().take(size).cloned().collect();
            let next = candidates
                .next()
                .and_then(|_| selected.last().map(|e| e.id.clone()));
            (selected, next, false)
        };
        selected.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(TimelinePage {
            items: inner.read_entries(&selected)?,
            next_before,
            cursor,
            has_more,
        })
    }

    pub fn conversations(&self) -> Result<Vec<Conversation>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let mut items = inner.conversations.clone();
        items.sort_by(|a, b| {
            b.last_ts
                .cmp(&a.last_ts)
                .then_with(|| b.last_id.cmp(&a.last_id))
        });
        Ok(items)
    }

    /// Items with an id strictly after `after_id`, oldest id first. Not for
    /// resuming a stream: a line can land after a newer id (use
    /// [`Self::after_arrival`]).
    pub fn after(&self, after_id: &str, limit: usize) -> Result<Vec<StoredItem>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let start = inner.entries.partition_point(|e| e.id.as_str() <= after_id);
        let end = (start + limit).min(inner.entries.len());
        let selected = inner.entries[start..end].to_vec();
        inner.read_entries(&selected)
    }

    /// Items that arrived after `after_id` (file order), oldest first: the
    /// SSE replay and follow (T1 fix A). Gap-free across writers: a late
    /// line with a smaller id than the cursor is still after it here. An id
    /// the index does not hold falls back to ids after it; `None` = all.
    pub fn after_arrival(&self, after_id: Option<&str>, limit: usize) -> Result<Vec<StoredItem>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let selected = inner.after_arrival(after_id, limit);
        inner.read_entries(&selected)
    }

    /// The id of the line that arrived last (a fresh stream's cursor).
    pub fn newest_arrival(&self) -> Result<Option<String>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        Ok(inner.arrival.back().map(|(_, id)| id.clone()))
    }

    /// One stored item by id (`None` when the index does not hold it, e.g.
    /// rotated away or never written).
    pub fn get(&self, id: &str) -> Result<Option<StoredItem>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let Some(entry) = inner.entry(id).cloned() else {
            return Ok(None);
        };
        Ok(inner.read_entries(&[entry])?.into_iter().next())
    }

    /// Every stored item, oldest first.
    pub fn read_all(&self) -> Result<Vec<StoredItem>> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
        let selected = inner.entries.clone();
        inner.read_entries(&selected)
    }

    pub fn len(&self) -> Result<usize> {
        let mut inner = self.lock();
        let found = inner.refresh()?;
        self.publish(found);
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
    /// On every item of the turn (Lucina items merge their own on top), as
    /// Node's `createTimelineTurn({meta})`: `{via: "mcp", caller}`.
    meta: Option<Meta>,
    /// The earlier item the owner's message replies to (swipe to reply).
    reply_to: Option<String>,
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
            meta: None,
            reply_to: None,
            user_item: Mutex::new(None),
        }
    }

    /// The owner's message replies to this earlier item (its `replyTo`).
    pub fn with_reply_to(mut self, reply_to: Option<String>) -> Self {
        self.reply_to = reply_to;
        self
    }

    /// Meta for every item of this turn.
    pub fn with_meta(mut self, meta: Option<Meta>) -> Self {
        self.meta = meta;
        self
    }

    /// Resume a turn whose user item was durably written during share ingestion.
    pub fn with_user_item(self, item: TimelineItem) -> Self {
        *self.user_item.lock().unwrap_or_else(|e| e.into_inner()) = Some(Some(item));
        self
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
            reply_to: self.reply_to.clone(),
            attachments,
            meta: self.meta.clone(),
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
            meta: match (self.meta.clone(), meta) {
                (Some(base), Some(extra)) => Some(base.merge(extra)),
                (base, extra) => base.or(extra),
            },
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
                        ..Attachment::default()
                    },
                    Attachment {
                        kind: "bogus".into(),
                        name: None,
                        ..Attachment::default()
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
    fn voice_note_fields_are_validated_and_clipped() {
        // 64 levels: 88 characters padded, 86 unpadded; both are accepted.
        let levels: String = "AAEC".repeat(21) + "Aw==";
        assert_eq!(levels.len(), 88);
        assert_eq!(valid_waveform(&levels).as_deref(), Some(levels.as_str()));
        assert_eq!(valid_waveform(" AAECAw ").as_deref(), Some("AAECAw"));
        // 65 bytes, not base64, empty, or too long: dropped.
        assert_eq!(valid_waveform(&("AAEC".repeat(21) + "AwQ=")), None);
        assert_eq!(valid_waveform("not base64!"), None);
        assert_eq!(valid_waveform(""), None);
        assert_eq!(valid_waveform(&"A".repeat(200)), None);
        let item = normalize(
            NewItem {
                channel: "ios".into(),
                role: "user".into(),
                attachments: vec![
                    Attachment {
                        kind: "voice".into(),
                        mime: Some("audio/mp4".into()),
                        duration_ms: Some(4200),
                        waveform: Some(levels.clone()),
                        transcript: Some(format!("  {}  ", "è".repeat(5000))),
                        ..Attachment::default()
                    },
                    Attachment {
                        kind: "voice".into(),
                        waveform: Some("%%%".into()),
                        transcript: Some("   ".into()),
                        ..Attachment::default()
                    },
                ],
                meta: Some(Meta {
                    transcript: Some("t".repeat(4500)),
                    voice_reply: Some(true),
                    ..Meta::default()
                }),
                ..NewItem::default()
            },
            now_ms(),
        )
        .unwrap();
        let a = item.attachments.unwrap();
        assert_eq!(a[0].waveform.as_deref(), Some(levels.as_str()));
        assert_eq!(
            a[0].transcript.as_ref().unwrap().chars().count(),
            MAX_TRANSCRIPT_CHARS
        );
        assert_eq!(
            (a[1].waveform.clone(), a[1].transcript.clone()),
            (None, None)
        );
        let meta = item.meta.unwrap();
        assert_eq!(meta.transcript.unwrap().len(), MAX_TRANSCRIPT_CHARS);
        assert_eq!(meta.voice_reply, Some(true));
        // A meta with only the voice hint is kept, and merges like the rest.
        let only = normalize_meta(Some(Meta {
            voice_reply: Some(true),
            ..Meta::default()
        }))
        .unwrap();
        let v = serde_json::to_value(only.merge(Meta {
            transcript: Some("ciao".into()),
            ..Meta::default()
        }))
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"transcript": "ciao", "voiceReply": true})
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

    /// T1 (docs/verification.md): a line whose id was taken before a newer
    /// line's but which lands after it in the file. Resuming by id skips it;
    /// resuming by arrival does not.
    #[test]
    fn late_line_is_after_the_cursor_in_arrival_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeline.jsonl");
        let s = store(dir.path(), DEFAULT_MAX_BYTES);
        let old = s.append(item("old")).unwrap();
        // Node takes N2's id, then Rust writes R (a newer id) first.
        let late = normalize(item("late node line"), now_ms()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let newer = s.append(item("rust line")).unwrap();
        assert!(late.id < newer.id);
        // The client resumes from R (the last event it saw): N2 lands now.
        let mut f = open_append(&path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&late).unwrap()).unwrap();
        drop(f);
        assert!(
            s.after(&newer.id, 500).unwrap().is_empty(),
            "by id: skipped"
        );
        let ids = |v: Vec<StoredItem>| v.into_iter().map(|i| i.id).collect::<Vec<_>>();
        assert_eq!(
            ids(s.after_arrival(Some(&newer.id), 500).unwrap()),
            vec![late.id.clone()]
        );
        assert_eq!(s.newest_arrival().unwrap(), Some(late.id.clone()));
        assert_eq!(
            ids(s.after_arrival(Some(&old.id), 500).unwrap()),
            vec![newer.id.clone(), late.id.clone()]
        );
        assert_eq!(
            ids(s.after_arrival(Some(&late.id), 500).unwrap()),
            Vec::<String>::new()
        );
        // An id the index does not know: ids after it, in arrival order.
        assert_eq!(
            ids(s.after_arrival(Some(&old.id[..20]), 500).unwrap()).len(),
            3
        );
        // A fresh store rebuilds the same order from the file.
        let fresh = store(dir.path(), DEFAULT_MAX_BYTES);
        assert_eq!(
            ids(fresh.after_arrival(Some(&newer.id), 500).unwrap()),
            vec![late.id.clone()]
        );
        // limit
        assert_eq!(
            ids(fresh.after_arrival(None, 2).unwrap()),
            vec![old.id, newer.id]
        );
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
    fn owner_reply_records_reply_to_in_node_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(store(dir.path(), DEFAULT_MAX_BYTES));
        let first = TimelineTurn::new(s.clone(), "ios", None);
        first.record_user("hello", vec![]).unwrap();
        let quoted = first.record_lucina("hi there", vec![], None).unwrap();
        let turn = TimelineTurn::new(s.clone(), "ios", Some("cm2".into()))
            .with_reply_to(Some(quoted.id.clone()));
        let u = turn.record_user("what did you mean?", vec![]).unwrap();
        assert_eq!(u.reply_to.as_deref(), Some(quoted.id.as_str()));
        let raw = s.get(&u.id).unwrap().unwrap().raw;
        assert_eq!(
            &*raw,
            format!(
                "{{\"id\":\"{}\",\"ts\":\"{}\",\"channel\":\"ios\",\"role\":\"user\",\"text\":\"what did you mean?\",\"clientMessageId\":\"cm2\",\"replyTo\":\"{}\"}}",
                u.id, u.ts, quoted.id
            )
        );
        // Lucina's answer still replies to the owner's message.
        let l = turn.record_lucina("I meant…", vec![], None).unwrap();
        assert_eq!(l.reply_to.as_deref(), Some(u.id.as_str()));
        // A reply target that is not a timeline id is dropped, not stored.
        let odd = TimelineTurn::new(s.clone(), "ios", None).with_reply_to(Some("tl_x".into()));
        assert_eq!(odd.record_user("x", vec![]).unwrap().reply_to, None);
    }

    #[test]
    fn get_finds_one_item_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), DEFAULT_MAX_BYTES);
        let a = s.append(item("a")).unwrap();
        let b = s.append(item("b")).unwrap();
        assert_eq!(s.get(&a.id).unwrap().unwrap().id, a.id);
        assert!(s
            .get(&b.id)
            .unwrap()
            .unwrap()
            .raw
            .contains("\"text\":\"b\""));
        assert!(s.get("01K83ZK4Q6X1V9W2D3E4F5G6H7").unwrap().is_none());
        // Another store on the same file sees it too.
        let fresh = store(dir.path(), DEFAULT_MAX_BYTES);
        assert_eq!(fresh.get(&b.id).unwrap().unwrap().id, b.id);
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

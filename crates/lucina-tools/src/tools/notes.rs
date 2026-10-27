//! `notes`: the owner's notes, briefings and documents.
//!
//! With secretary-server configured (`LUCINA_SECRETARY_SERVER_URL` and
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`) they live there (`/v1/notes`,
//! SecretaryIOS `docs/server-api.md`, "Notes"), so the owner sees them in the
//! app and ChatGPT, Grok or Claude Code (Lucina MCP) share them. When the
//! server is not configured or not reachable, markdown files under
//! `<data dir>/notes/` (atomic 0600 writes) are the fallback; a server that
//! answers but refuses (4xx) is reported, not bypassed.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::calendar::server_of;
use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

const MAX_NOTE_CHARS: usize = 20_000;
/// Characters of a note's content a `read` returns.
const READ_CHARS: usize = 12_000;
/// Content limit on the server (UTF-8 bytes).
const SERVER_MAX_BYTES: usize = 256 * 1024;
/// Keep's palette as secretary-server stores it.
const COLORS: [&str; 12] = [
    "default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink",
    "brown", "gray",
];

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Read,
    Create,
    Append,
    Search,
    /// Change an existing note without rewriting it (labels, colour, pin, archive, reminder, checklist items).
    Update,
    /// The label names with how many notes each has.
    Labels,
}

/// A checklist item to add: text, or text with its state.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum ItemArg {
    Text(String),
    Item {
        text: String,
        #[serde(default)]
        checked: Option<bool>,
    },
}

/// A checklist item of an existing note: its number as `read` shows it (from 1), or its exact text.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum ItemRefArg {
    Number(u32),
    Text(String),
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Archive {
    Include,
    Exclude,
    Only,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Kind {
    List,
    Text,
    Image,
    Audio,
    Link,
    Reminder,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Repeat {
    Daily,
    Weekly,
    Monthly,
    Yearly,
    None,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Convert {
    /// Each non-empty line becomes an unchecked item.
    List,
    /// The items become plain lines.
    Text,
}

fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// Note title (read, create, append, update: names the note).
    title: Option<String>,
    /// Short name of the note, e.g. "craftit-meeting" (read, append, update; create: optional).
    slug: Option<String>,
    /// Markdown text (create, append; update: replaces the whole content).
    content: Option<String>,
    /// Text to look for (search).
    query: Option<String>,
    /// Tags for a new note (create), e.g. ["work"]. Same thing as `labels`.
    tags: Option<Vec<String>>,
    /// Keep labels for a new note (create), by display name, e.g. ["GodLevel", "Fisica 2"].
    labels: Option<Vec<String>>,
    /// Make a new note a checklist (create): strings, or {text, checked}.
    items: Option<Vec<ItemArg>>,
    /// Keep colour: default, red, orange, yellow, green, teal, blue, cerulean, purple, pink, brown, gray (create, update; or a filter for list and search).
    color: Option<String>,
    /// Pin the note (create, update).
    pinned: Option<bool>,
    /// Put the note in the archive, or bring it back (create, update).
    archived: Option<bool>,
    /// Only notes with this label (list, search).
    label: Option<String>,
    /// Only this kind of note: list (checklist), text, image, audio, link, reminder (list, search).
    #[serde(rename = "type")]
    kind: Option<Kind>,
    /// Archived notes in list and search: include, exclude or only. Default: list excludes them, search includes them.
    archive: Option<Archive>,
    /// New title (update).
    new_title: Option<String>,
    /// Labels to add, by display name (update).
    add_labels: Option<Vec<String>>,
    /// Labels to remove, by name (update).
    remove_labels: Option<Vec<String>>,
    /// Set the note's reminder, RFC 3339 with offset (update); null clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schemars(with = "Option<String>")]
    reminder_at: Option<Option<String>>,
    /// Repeat of the note's reminder (update). Default: keep the current one.
    reminder_repeat: Option<Repeat>,
    /// Check off checklist items: numbers as `read` shows them, or exact texts (update).
    check: Option<Vec<ItemRefArg>>,
    /// Uncheck checklist items: numbers or exact texts (update).
    uncheck: Option<Vec<ItemRefArg>>,
    /// Add items at the end of the checklist: strings, or {text, checked} (update).
    add_items: Option<Vec<ItemArg>>,
    /// Remove checklist items: numbers or exact texts (update).
    remove_items: Option<Vec<ItemRefArg>>,
    /// Turn the note into a checklist or back into text (update).
    convert: Option<Convert>,
}

/// Lowercase ASCII slug, `-` separated, at most 80 characters.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().to_lowercase().chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'æ' => 'a',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(80).collect();
    s.trim_end_matches('-').to_owned()
}

// ---------------------------------------------------------------- server

enum Remote {
    /// The server answered.
    Done(Value),
    /// Not configured or not reachable: use the local files.
    Fallback(String),
}

/// `(status, body)` from the server, or why it could not be asked.
async fn send(req: reqwest::RequestBuilder) -> Result<(u16, Value), String> {
    let resp = req
        .send()
        .await
        .map_err(|e| format!("secretary-server unreachable: {}", e.without_url()))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

fn server_err(status: u16, body: &Value) -> ToolError {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    ToolError::new(format!(
        "secretary-server answered {status} {code}: {message}"
    ))
}

/// Key for `/v1/notes/{key}`: the slug, else one made from the title.
fn key_of(a: &Args) -> Option<String> {
    let s = a
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .or_else(|| a.title.as_deref().map(slug).filter(|s| !s.is_empty()))?;
    let ok = s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    ok.then_some(s)
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---------------------------------------------------------------- Keep helpers

fn keep_color(c: &str) -> Result<String, ToolError> {
    let c = c.trim().to_lowercase();
    let c = if c == "grey" { "gray".to_owned() } else { c };
    if COLORS.contains(&c.as_str()) {
        Ok(c)
    } else {
        Err(ToolError::new(format!(
            "color must be one of {} (got {c:?})",
            COLORS.join(", ")
        )))
    }
}

/// A label name as typed: trimmed, whitespace collapsed, a leading '#' dropped.
fn label_name(s: &str) -> Result<String, ToolError> {
    let t = s.trim().trim_start_matches('#');
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() || t.chars().count() > 50 || t.chars().any(|c| c.is_control() || c == ',') {
        return Err(ToolError::new(format!(
            "label {s:?} must be 1-50 characters, without commas"
        )));
    }
    Ok(t)
}

fn label_names(labels: Option<&[String]>) -> Result<Vec<String>, ToolError> {
    let labels = labels.unwrap_or_default();
    if labels.len() > 50 {
        return Err(ToolError::new("at most 50 labels"));
    }
    labels.iter().map(|l| label_name(l)).collect()
}

/// The stored form of a label (the server's `tags`).
fn label_tag(name: &str) -> String {
    name.trim()
        .trim_start_matches('#')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The labels to send in a PATCH: current tags minus `remove`, plus `add`.
fn merge_labels(tags: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let removed: Vec<String> = remove.iter().map(|r| label_tag(r)).collect();
    let mut out: Vec<String> = tags
        .iter()
        .filter(|t| !removed.contains(&label_tag(t)))
        .cloned()
        .collect();
    for a in add {
        if !out.iter().any(|t| label_tag(t) == label_tag(a)) {
            out.push(a.clone());
        }
    }
    out
}

fn text_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// Checklist items from the arguments: `{text, checked, indent: 0}`.
fn items_json(items: &[ItemArg]) -> Result<Vec<Value>, ToolError> {
    if items.len() > 1000 {
        return Err(ToolError::new("at most 1000 items"));
    }
    items
        .iter()
        .map(|it| {
            let (text, checked) = match it {
                ItemArg::Text(t) => (t.as_str(), false),
                ItemArg::Item { text, checked } => (text.as_str(), checked.unwrap_or(false)),
            };
            let text = text.trim();
            if text.is_empty() || text.chars().count() > 2000 {
                return Err(ToolError::new("an item's text must be 1-2000 characters"));
            }
            Ok(json!({"text": text, "checked": checked, "indent": 0}))
        })
        .collect()
}

/// A reference to an item: its number (from 1) or its exact text.
enum ItemRef {
    Number(usize),
    Text(String),
}

fn item_refs(refs: Option<&[ItemRefArg]>) -> Vec<ItemRef> {
    refs.unwrap_or_default()
        .iter()
        .map(|r| match r {
            ItemRefArg::Number(n) => ItemRef::Number(*n as usize),
            ItemRefArg::Text(t) => ItemRef::Text(t.clone()),
        })
        .collect()
}

fn item_text(it: &Value) -> &str {
    it["text"].as_str().unwrap_or("")
}

/// What identifies an item across a refetch: its id, else its text.
fn item_key(it: &Value) -> String {
    it["id"]
        .as_str()
        .map_or_else(|| item_text(it).to_owned(), str::to_owned)
}

fn numbered(items: &[Value]) -> String {
    items
        .iter()
        .enumerate()
        .take(40)
        .map(|(i, it)| format!("{}. {}", i + 1, item_text(it)))
        .collect::<Vec<_>>()
        .join("; ")
}

fn resolve(items: &[Value], r: &ItemRef) -> Result<usize, ToolError> {
    match r {
        ItemRef::Number(n) if *n >= 1 && *n <= items.len() => Ok(*n - 1),
        ItemRef::Number(n) => Err(ToolError::new(format!(
            "no item number {n}: the list has {} item(s). Items: {}",
            items.len(),
            numbered(items)
        ))),
        ItemRef::Text(t) => {
            let want = t.trim().to_lowercase();
            let hits: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, it)| item_text(it).trim().to_lowercase() == want)
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [i] => Ok(*i),
                [] => Err(ToolError::new(format!(
                    "no item with the text {t:?}. Items: {}",
                    numbered(items)
                ))),
                many => Err(ToolError::new(format!(
                    "{} items have the text {t:?} (numbers {}); use the item number",
                    many.len(),
                    many.iter()
                        .map(|i| (i + 1).to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
            }
        }
    }
}

/// Checklist edits resolved once against the items the model saw, so a retry
/// after a 409 applies them to the same items (by id) in the newer version.
#[derive(Default)]
struct Plan {
    check: Vec<String>,
    uncheck: Vec<String>,
    remove: Vec<String>,
    add: Vec<Value>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.check.is_empty()
            && self.uncheck.is_empty()
            && self.remove.is_empty()
            && self.add.is_empty()
    }

    fn new(
        items: &[Value],
        check: &[ItemRef],
        uncheck: &[ItemRef],
        remove: &[ItemRef],
        add: Vec<Value>,
    ) -> Result<Self, ToolError> {
        let keys = |refs: &[ItemRef]| -> Result<Vec<String>, ToolError> {
            refs.iter()
                .map(|r| resolve(items, r).map(|i| item_key(&items[i])))
                .collect()
        };
        Ok(Self {
            check: keys(check)?,
            uncheck: keys(uncheck)?,
            remove: keys(remove)?,
            add,
        })
    }

    fn apply(&self, items: &[Value]) -> Vec<Value> {
        let mut out = Vec::with_capacity(items.len() + self.add.len());
        for it in items {
            let key = item_key(it);
            if self.remove.contains(&key) {
                continue;
            }
            let mut it = it.clone();
            if self.check.contains(&key) {
                it["checked"] = json!(true);
            }
            if self.uncheck.contains(&key) {
                it["checked"] = json!(false);
            }
            out.push(it);
        }
        out.extend(self.add.iter().cloned());
        out
    }

    fn summary(&self) -> Vec<String> {
        [
            (self.check.len(), "checked"),
            (self.uncheck.len(), "unchecked"),
            (self.remove.len(), "removed"),
            (self.add.len(), "added"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, w)| format!("{w} {n} item{}", if n == 1 { "" } else { "s" }))
        .collect()
    }
}

fn progress(items: &[Value]) -> String {
    let done = items.iter().filter(|i| i["checked"] == true).count();
    format!("{done}/{} done", items.len())
}

/// The items as the model addresses them: `{n, text, checked}` (indent 1 shown).
fn numbered_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .take(300)
        .map(|(i, it)| {
            let mut o =
                json!({"n": i + 1, "text": item_text(it), "checked": it["checked"] == true});
            if it["indent"].as_u64().unwrap_or(0) > 0 {
                o["indent"] = json!(1);
            }
            o
        })
        .collect()
}

/// `[note:<slug>]`: how Lucina's context and memory refer to a note, so an
/// answer can cite it and the apps can link it (`/notes/<slug>`).
fn cite(slug: &str) -> String {
    format!("[note:{slug}]")
}

fn reminder_of(n: &Value) -> Option<String> {
    let at = n["reminder"]["at"]
        .as_str()
        .or_else(|| n["reminderAt"].as_str())?;
    Some(match n["reminder"]["recurrence"].as_str() {
        Some(r) => format!("{at} ({r})"),
        None => at.to_owned(),
    })
}

/// Keep fields shared by a summary (list/search hit) and a full note.
fn keep_fields(n: &Value, out: &mut Value) {
    if let Some(l) = n["labels"].as_array().filter(|l| !l.is_empty()) {
        out["labels"] = json!(l);
    }
    if let Some(c) = n["color"].as_str().filter(|c| *c != "default") {
        out["color"] = json!(c);
    }
    if n["archived"] == true {
        out["archived"] = json!(true);
    }
    if n["deletedAt"].is_string() {
        out["trashed"] = json!(true);
    }
    if let Some(items) = n["items"].as_array() {
        out["checklist"] = json!(progress(items));
    } else if n["isList"] == true {
        out["checklist"] = json!(format!(
            "{}/{} done",
            n["itemsDone"].as_u64().unwrap_or(0),
            n["itemsTotal"].as_u64().unwrap_or(0)
        ));
    }
    if let Some(r) = reminder_of(n) {
        out["reminder"] = json!(r);
    }
    if let Some(slug) = n["slug"].as_str() {
        out["cite"] = json!(cite(slug));
    }
}

/// A list or search hit's Keep fields on top of `base`.
fn hit(n: &Value, mut base: Value) -> Value {
    keep_fields(n, &mut base);
    base
}

/// The filters of list and search as a query-string tail (`&tag=…`).
fn filters(a: &Args, default_archive: Option<&str>) -> Result<String, ToolError> {
    let mut q = String::new();
    if let Some(l) = a.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        q.push_str(&format!("&tag={}", enc(&label_tag(l))));
    }
    if let Some(c) = a.color.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        q.push_str(&format!("&color={}", keep_color(c)?));
    }
    if let Some(k) = a.kind {
        let k = match k {
            Kind::List => "list",
            Kind::Text => "text",
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Link => "link",
            Kind::Reminder => "reminder",
        };
        q.push_str(&format!("&type={k}"));
    }
    let archive = match a.archive {
        Some(Archive::Include) => None,
        Some(Archive::Exclude) => Some("exclude"),
        Some(Archive::Only) => Some("only"),
        None => default_archive,
    };
    if let Some(x) = archive {
        q.push_str(&format!("&archived={x}"));
    }
    Ok(q)
}

fn has_filters(a: &Args) -> bool {
    a.label.is_some() || a.color.is_some() || a.kind.is_some() || a.archive.is_some()
}

/// Keep-only arguments the local files cannot honour: the first one given.
fn keep_only_arg(a: &Args) -> Option<&'static str> {
    match a.action {
        Action::Create | Action::Append => [
            (a.labels.is_some(), "labels"),
            (a.items.is_some(), "items"),
            (a.color.is_some(), "color"),
            (a.pinned.is_some(), "pinned"),
            (a.archived.is_some(), "archived"),
        ]
        .into_iter()
        .find_map(|(b, n)| b.then_some(n)),
        Action::List | Action::Search => [
            (a.label.is_some(), "label"),
            (a.color.is_some(), "color"),
            (a.kind.is_some(), "type"),
            (a.archive.is_some(), "archive"),
        ]
        .into_iter()
        .find_map(|(b, n)| b.then_some(n)),
        _ => None,
    }
}

async fn remote(
    ctx: &Ctx,
    a: &Args,
    req_title: Option<&str>,
    content: Option<&str>,
) -> Result<Remote, ToolError> {
    let Some((base, token)) = server_of(&ctx.env) else {
        return Ok(Remote::Fallback("not configured".into()));
    };
    let http = &ctx.env.http;
    let get = |path: String| http.get(format!("{base}{path}")).bearer_auth(&token);
    let post = |path: String, body: Value| {
        http.post(format!("{base}{path}"))
            .bearer_auth(&token)
            .json(&body)
    };
    // Unreachable → local files; any HTTP answer is final.
    macro_rules! ask {
        ($req:expr) => {
            match send($req).await {
                Ok(r) => r,
                Err(why) => return Ok(Remote::Fallback(why)),
            }
        };
    }
    let out = match a.action {
        Action::Update => return update_remote(ctx, a, req_title).await,
        Action::Labels => return labels_remote(ctx).await,
        Action::List => {
            let f = filters(a, Some("exclude"))?;
            let (s, v) = ask!(get(format!("/v1/notes?limit=100{f}")));
            if s != 200 {
                return Err(server_err(s, &v));
            }
            let notes: Vec<Value> = v["notes"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|n| {
                            hit(n, json!({"title": n["title"], "slug": n["slug"], "modified": n["updatedAt"],
                                   "source": n["source"], "pinned": n["pinned"], "tags": n["tags"]}))
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({"notes": notes, "stored": "secretary"})
        }
        Action::Search => {
            let q = a.query.as_deref().unwrap_or_default().trim().to_owned();
            let words = if q.is_empty() {
                String::new()
            } else {
                format!("&q={}", enc(&q))
            };
            let f = filters(a, None)?;
            let (s, v) = ask!(get(format!("/v1/notes?limit=20{words}{f}")));
            if s != 200 {
                return Err(server_err(s, &v));
            }
            let results: Vec<Value> = v["notes"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|n| hit(n, json!({"title": n["title"], "slug": n["slug"], "lines": [n["excerpt"]]})))
                        .collect()
                })
                .unwrap_or_default();
            json!({"query": q.to_lowercase(), "results": results, "stored": "secretary"})
        }
        Action::Read => {
            let key = key_of(a).ok_or_else(|| ToolError::new("title or slug is required"))?;
            let (mut s, mut v) = ask!(get(format!("/v1/notes/{}", enc(&key))));
            if s == 404 {
                // A title that is not the slug: the best search hit.
                let q = req_title.unwrap_or(&key);
                let (s2, hits) = ask!(get(format!("/v1/notes?limit=5&q={}", enc(q))));
                let first = (s2 == 200)
                    .then(|| hits["notes"].as_array().and_then(|l| l.first().cloned()))
                    .flatten();
                if let Some(h) = first.and_then(|h| h["slug"].as_str().map(str::to_owned)) {
                    (s, v) = ask!(get(format!("/v1/notes/{}", enc(&h))));
                }
            }
            if s == 404 {
                return Err(ToolError::new(format!(
                    "no note titled {:?}",
                    req_title.unwrap_or(&key)
                )));
            }
            if s != 200 {
                return Err(server_err(s, &v));
            }
            // Display names of the labels (the note carries the stored tags).
            if v["tags"].as_array().is_some_and(|t| !t.is_empty()) {
                if let Ok((200, reg)) = send(get("/v1/notes/labels".into())).await {
                    let names: Vec<Value> = v["tags"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|t| {
                            reg["labels"]
                                .as_array()
                                .and_then(|l| l.iter().find(|l| l["tag"] == *t))
                                .map(|l| l["name"].clone())
                                .unwrap_or_else(|| t.clone())
                        })
                        .collect();
                    v["labels"] = json!(names);
                }
            }
            let mut out = json!({"title": v["title"], "slug": v["slug"], "source": v["source"],
                "updated": v["updatedAt"], "tags": v["tags"], "pinned": v["pinned"], "stored": "secretary"});
            keep_fields(&v, &mut out);
            if let Some(items) = v["items"].as_array() {
                out["items"] = json!(numbered_items(items));
            } else {
                let text = v["content"].as_str().unwrap_or_default();
                let total = text.chars().count();
                out["content"] = json!(text.chars().take(READ_CHARS).collect::<String>());
                if total > READ_CHARS {
                    out["truncated"] = json!(format!(
                        "{total} characters in total; the whole note is in the Secretary app"
                    ));
                }
            }
            if let Some(att) = v["attachments"].as_array().filter(|a| !a.is_empty()) {
                out["attachments"] = json!(att
                    .iter()
                    .map(|x| json!({"name": x["name"], "type": x["mimetype"], "size": x["size"]}))
                    .collect::<Vec<_>>());
            }
            if let Some(l) = v["annotations"].as_array().filter(|l| !l.is_empty()) {
                out["links"] = json!(l
                    .iter()
                    .map(|x| json!({"url": x["url"], "title": x["title"]}))
                    .collect::<Vec<_>>());
            }
            if let Some(c) = v["collaborators"].as_array().filter(|c| !c.is_empty()) {
                out["shared_with"] = json!(c);
            }
            out
        }
        Action::Create | Action::Append => {
            let content = content.unwrap_or_default();
            if content.len() > SERVER_MAX_BYTES {
                return Err(ToolError::new("note too long"));
            }
            let title = req_title.unwrap_or_default().to_owned();
            let explicit = a
                .slug
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase);
            let keep = create_keep_fields(a)?;
            let create = |slug: Option<String>| {
                let mut body = json!({"title": title, "source": "lucina"});
                if !content.is_empty() {
                    body["content"] = json!(content);
                }
                if let Some(s) = slug {
                    body["slug"] = json!(s);
                }
                if let Some(t) = &a.tags {
                    body["tags"] = json!(t);
                }
                if let (Some(b), Some(k)) = (body.as_object_mut(), keep.as_object()) {
                    b.extend(k.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                post("/v1/notes".into(), body)
            };
            let (s, v) = if a.action == Action::Create {
                let (s, v) = ask!(create(explicit.clone()));
                if s == 409 && v["error"]["code"] == "slug_taken" {
                    return Err(ToolError::new(format!(
                        "a note titled {title:?} exists; use append"
                    )));
                }
                (s, v)
            } else {
                let key = key_of(a).ok_or_else(|| ToolError::new("title or slug is required"))?;
                let (s, v) = ask!(post(
                    format!("/v1/notes/{}/append", enc(&key)),
                    json!({"content": content, "separator": "\n", "source": "lucina"})
                ));
                if s == 404 {
                    // Appending to a missing note starts it (as with local files).
                    ask!(create(explicit.clone().or(Some(key))))
                } else {
                    (s, v)
                }
            };
            if !(200..300).contains(&s) {
                if v["error"]["code"] == "note_too_large" {
                    return Err(ToolError::new("note too long"));
                }
                return Err(server_err(s, &v));
            }
            let saved_title = v["title"].as_str().unwrap_or(&title).to_owned();
            ctx.push_effect(Effect::Note {
                title: saved_title.clone(),
                slug: v["slug"].as_str().map(str::to_owned),
            });
            tracing::info!(
                slug = v["slug"].as_str().unwrap_or_default(),
                "note saved on secretary-server"
            );
            let mut out = json!({"saved": true, "title": saved_title, "slug": v["slug"],
                   "chars": v["content"].as_str().map_or(0, |c| c.chars().count()), "stored": "secretary"});
            if let Some(items) = v["items"].as_array() {
                out["checklist"] = json!(progress(items));
            }
            if let Some(slug) = v["slug"].as_str() {
                out["cite"] = json!(cite(slug));
            }
            out
        }
    };
    Ok(Remote::Done(out))
}

/// Keep fields of a new note's body.
fn create_keep_fields(a: &Args) -> Result<Value, ToolError> {
    let mut k = json!({});
    if a.labels.is_some() {
        k["labels"] = json!(label_names(a.labels.as_deref())?);
    }
    if let Some(items) = &a.items {
        k["items"] = json!(items_json(items)?);
    }
    if let Some(c) = &a.color {
        k["color"] = json!(keep_color(c)?);
    }
    if let Some(p) = a.pinned {
        k["pinned"] = json!(p);
    }
    if let Some(r) = a.archived {
        k["archived"] = json!(r);
    }
    Ok(k)
}

/// `GET /v1/notes/labels`.
async fn labels_remote(ctx: &Ctx) -> Result<Remote, ToolError> {
    let Some((base, token)) = server_of(&ctx.env) else {
        return Ok(Remote::Fallback("not configured".into()));
    };
    let req = ctx
        .env
        .http
        .get(format!("{base}/v1/notes/labels"))
        .bearer_auth(&token);
    let (s, v) = match send(req).await {
        Ok(r) => r,
        Err(why) => return Ok(Remote::Fallback(why)),
    };
    if s != 200 {
        return Err(server_err(s, &v));
    }
    let labels: Vec<Value> = v["labels"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|x| json!({"name": x["name"], "notes": x["count"]}))
                .collect()
        })
        .unwrap_or_default();
    Ok(Remote::Done(
        json!({"labels": labels, "stored": "secretary"}),
    ))
}

/// What an update step can fail with after the server answered once.
fn down(why: String) -> ToolError {
    ToolError::new(format!("{why}; the note was not changed"))
}

/// The `reminder` object to send for an update.
fn reminder_value(a: &Args, cur: &Value) -> Result<Value, ToolError> {
    if let Some(None) = a.reminder_at {
        return Ok(Value::Null);
    }
    let cur_r = cur["reminder"].is_object().then(|| &cur["reminder"]);
    let at = match &a.reminder_at {
        Some(Some(at)) => at.clone(),
        _ => cur_r
            .and_then(|r| r["at"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| ToolError::new("this note has no reminder yet: give reminder_at"))?,
    };
    let repeat = match a.reminder_repeat {
        Some(Repeat::None) => None,
        Some(Repeat::Daily) => Some("daily".to_owned()),
        Some(Repeat::Weekly) => Some("weekly".to_owned()),
        Some(Repeat::Monthly) => Some("monthly".to_owned()),
        Some(Repeat::Yearly) => Some("yearly".to_owned()),
        None => cur_r
            .and_then(|r| r["recurrence"].as_str())
            .map(str::to_owned),
    };
    let mut r = json!({"at": at});
    if let Some(rec) = repeat {
        r["recurrence"] = json!(rec);
    }
    Ok(r)
}

/// A server address: client, base URL, service token.
type Srv<'a> = (&'a reqwest::Client, &'a str, &'a str);

/// PATCH a note, editing from the version we hold; on `409 conflict`
/// (somebody changed it since) fetch the newer one and edit that, once.
async fn patch_note(
    srv: &Srv<'_>,
    slug: &str,
    mut note: Value,
    build: &(dyn Fn(&Value) -> Result<Value, ToolError> + Sync),
) -> Result<Value, ToolError> {
    let (http, base, token) = *srv;
    let mut retried = false;
    loop {
        let mut body = build(&note)?;
        if let Some(u) = note.get("updatedAt").filter(|u| !u.is_null()) {
            body["baseUpdatedAt"] = u.clone();
        }
        let req = http
            .patch(format!("{base}/v1/notes/{}", enc(slug)))
            .bearer_auth(token)
            .json(&body);
        let (st, v) = send(req).await.map_err(down)?;
        if (200..300).contains(&st) {
            return Ok(v);
        }
        match (st, v["error"]["code"].as_str()) {
            (409, Some("conflict")) if !retried => {
                retried = true;
                let req = http
                    .get(format!("{base}/v1/notes/{}", enc(slug)))
                    .bearer_auth(token);
                let (gs, gv) = send(req).await.map_err(down)?;
                if gs != 200 {
                    return Err(server_err(gs, &gv));
                }
                note = gv;
            }
            (409, Some("conflict")) => {
                return Err(ToolError::new(
                    "the note was changed by someone else while this edit was being made; read it again and repeat the change",
                ))
            }
            (502, _) if body.get("reminder").is_some() => {
                return Err(ToolError::new(
                    "the reminder could not be scheduled (Lucina unreachable); the note is unchanged",
                ))
            }
            _ => return Err(server_err(st, &v)),
        }
    }
}

/// `update`: change a Keep note (labels, colour, pin, archive, reminder,
/// checklist items). The checklist is edited here: GET the note, change
/// `items`, PATCH with `baseUpdatedAt`; one retry when the note changed
/// meanwhile (`409 conflict`), re-applying the edits to the newer items by id.
async fn update_remote(ctx: &Ctx, a: &Args, req_title: Option<&str>) -> Result<Remote, ToolError> {
    let Some((base, token)) = server_of(&ctx.env) else {
        return Ok(Remote::Fallback("not configured".into()));
    };
    let http = &ctx.env.http;
    let get = |path: String| http.get(format!("{base}{path}")).bearer_auth(&token);
    let srv = (http, base.as_str(), token.as_str());
    // Everything that needs no server.
    let mut fields = json!({});
    let mut changes: Vec<String> = Vec::new();
    if let Some(t) = a
        .new_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if t.chars().count() > 200 {
            return Err(ToolError::new("new_title is longer than 200 characters"));
        }
        fields["title"] = json!(t);
        changes.push("title".into());
    }
    if let Some(c) = a.content.as_deref().filter(|c| !c.trim().is_empty()) {
        if c.len() > SERVER_MAX_BYTES {
            return Err(ToolError::new("note too long"));
        }
        fields["content"] = json!(c);
        changes.push("content".into());
    }
    if let Some(c) = &a.color {
        let c = keep_color(c)?;
        changes.push(format!("colour {c}"));
        fields["color"] = json!(c);
    }
    if let Some(p) = a.pinned {
        fields["pinned"] = json!(p);
        changes.push(if p { "pinned" } else { "unpinned" }.into());
    }
    if let Some(r) = a.archived {
        fields["archived"] = json!(r);
        changes.push(if r { "archived" } else { "unarchived" }.into());
    }
    let add_labels = label_names(a.add_labels.as_deref())?;
    let remove_labels = label_names(a.remove_labels.as_deref())?;
    if !add_labels.is_empty() {
        changes.push(format!("label +{}", add_labels.join(", +")));
    }
    if !remove_labels.is_empty() {
        changes.push(format!("label -{}", remove_labels.join(", -")));
    }
    let add_items = items_json(a.add_items.as_deref().unwrap_or_default())?;
    let (check, uncheck, remove) = (
        item_refs(a.check.as_deref()),
        item_refs(a.uncheck.as_deref()),
        item_refs(a.remove_items.as_deref()),
    );
    let has_ops =
        !(check.is_empty() && uncheck.is_empty() && remove.is_empty() && add_items.is_empty());
    if has_ops && fields.get("content").is_some() {
        return Err(ToolError::new(
            "give either content or the checklist operations (check, uncheck, add_items, remove_items), not both",
        ));
    }
    if let Some(Some(at)) = &a.reminder_at {
        if chrono::DateTime::parse_from_rfc3339(at).is_err() {
            return Err(ToolError::new(
                "reminder_at must be RFC 3339 with a UTC offset, e.g. 2026-11-03T09:00:00+01:00 (or null to clear)",
            ));
        }
        changes.push(format!("reminder {at}"));
    } else if a.reminder_at.is_some() {
        changes.push("reminder cleared".into());
    } else if a.reminder_repeat.is_some() {
        changes.push("reminder repeat".into());
    }
    let reminder_change = a.reminder_at.is_some() || a.reminder_repeat.is_some();
    let labels_change = !add_labels.is_empty() || !remove_labels.is_empty();
    if fields.as_object().is_some_and(|f| f.is_empty())
        && !reminder_change
        && !has_ops
        && !labels_change
        && a.convert.is_none()
    {
        return Err(ToolError::new(
            "nothing to change: give at least one of new_title, content, color, pinned, archived, add_labels, remove_labels, reminder_at, check, uncheck, add_items, remove_items, convert",
        ));
    }

    // Find the note: by slug, else by title (best search hit).
    let key = key_of(a).ok_or_else(|| ToolError::new("title or slug is required"))?;
    let (mut s, mut v) = match send(get(format!("/v1/notes/{}", enc(&key)))).await {
        Ok(r) => r,
        Err(why) => return Ok(Remote::Fallback(why)),
    };
    if s == 404 {
        let q = req_title.unwrap_or(&key);
        if let Ok((200, hits)) = send(get(format!("/v1/notes?limit=5&q={}", enc(q)))).await {
            if let Some(h) = hits["notes"][0]["slug"].as_str() {
                (s, v) = send(get(format!("/v1/notes/{}", enc(h))))
                    .await
                    .map_err(down)?;
            }
        }
    }
    if s == 404 {
        return Err(ToolError::new(format!(
            "no note titled {:?}",
            req_title.unwrap_or(&key)
        )));
    }
    if s != 200 {
        return Err(server_err(s, &v));
    }
    let mut note = v;
    let slug = text_of(&note, "slug");

    // Convert first: the item operations then work on the converted note.
    if let Some(c) = a.convert {
        let is_list = note["items"].is_array();
        let want_list = matches!(c, Convert::List);
        if want_list != is_list {
            let how = if want_list { "list" } else { "text" };
            note = patch_note(&srv, &slug, note, &|_| Ok(json!({"convert": how}))).await?;
            changes.insert(0, format!("converted to {how}"));
        }
    }
    // The checklist edits, resolved once against the items as `read` showed them.
    let plan = if has_ops {
        let Some(items) = note["items"].as_array() else {
            return Err(ToolError::new(
                "this is a text note, not a checklist: use convert=\"list\" to make it one, or append to add text",
            ));
        };
        let plan = Plan::new(items, &check, &uncheck, &remove, add_items)?;
        changes.extend(plan.summary());
        plan
    } else {
        Plan::default()
    };
    let needs_patch = fields.as_object().is_some_and(|f| !f.is_empty())
        || reminder_change
        || labels_change
        || !plan.is_empty();
    if needs_patch {
        note = patch_note(&srv, &slug, note, &|cur| {
            let mut body = fields.clone();
            if labels_change {
                let tags: Vec<String> = cur["tags"]
                    .as_array()
                    .map(|t| {
                        t.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                body["labels"] = json!(merge_labels(&tags, &add_labels, &remove_labels));
            }
            if !plan.is_empty() {
                let items = cur["items"].as_array().ok_or_else(|| {
                    ToolError::new("the note is no longer a checklist; read it again")
                })?;
                body["items"] = json!(plan.apply(items));
            }
            if reminder_change {
                body["reminder"] = reminder_value(a, cur)?;
            }
            Ok(body)
        })
        .await?;
    }
    let title = text_of(&note, "title");
    ctx.push_effect(Effect::Note {
        title: title.clone(),
        slug: Some(slug.clone()),
    });
    tracing::info!(slug = %slug, "note updated on secretary-server");
    let mut out = json!({"updated": true, "title": title, "slug": slug, "changes": changes,
                         "cite": cite(&slug), "stored": "secretary"});
    if let Some(items) = note["items"].as_array() {
        out["checklist"] = json!(progress(items));
        out["items"] = json!(numbered_items(items));
    }
    Ok(Remote::Done(out))
}

// ---------------------------------------------------------------- local files

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.env.data_dir.join("notes")
}

fn path_for(ctx: &Ctx, title: &str) -> Result<PathBuf, ToolError> {
    let s = slug(title);
    if s.is_empty() {
        return Err(ToolError::new("title is required"));
    }
    Ok(dir(ctx).join(format!("{s}.md")))
}

fn list(ctx: &Ctx) -> Vec<(String, std::time::SystemTime, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(ctx)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let title = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("# "))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    p.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((title, mtime, text));
        }
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.1));
    out
}

fn local(a: &Args, ctx: &Ctx) -> ToolResult {
    let req = |v: &Option<String>, what: &str| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ToolError::new(format!("{what} is required")))
    };
    let title_or_slug = || req(&a.title, "title").or_else(|_| req(&a.slug, "title"));
    if let Some(arg) = keep_only_arg(a) {
        return Err(ToolError::new(format!(
            "`{arg}` needs the Secretary server (Keep notes), which is not reachable right now; only plain notes work offline"
        )));
    }
    match a.action {
        Action::Update | Action::Labels => Err(ToolError::new(
            "labels, checklists, colours, archive and reminders on notes need the Secretary server, which is not reachable right now; only plain notes work offline",
        )),
        Action::List => Ok(
            json!({"notes": list(ctx).into_iter().take(100).map(|(t, m, _)| json!({
            "title": t,
            "modified": chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        })).collect::<Vec<_>>()}),
        ),
        Action::Read => {
            let title = title_or_slug()?;
            let p = path_for(ctx, &title)?;
            let text = std::fs::read_to_string(&p)
                .map_err(|_| ToolError::new(format!("no note titled {title:?}")))?;
            Ok(json!({"title": title, "content": text.chars().take(8000).collect::<String>()}))
        }
        Action::Create | Action::Append => {
            let title = title_or_slug()?;
            let content = req(&a.content, "content")?;
            let p = path_for(ctx, &title)?;
            let existing = std::fs::read_to_string(&p).ok();
            let text = match (a.action, existing) {
                (Action::Create, Some(_)) => {
                    return Err(ToolError::new(format!(
                        "a note titled {title:?} exists; use append"
                    )))
                }
                (Action::Create, None) => format!("# {title}\n\n{content}\n"),
                (_, Some(old)) => format!("{}\n{content}\n", old.trim_end()),
                (_, None) => format!("# {title}\n\n{content}\n"),
            };
            if text.chars().count() > MAX_NOTE_CHARS {
                return Err(ToolError::new("note too long"));
            }
            lucina_store::write_atomic(&p, text.as_bytes())
                .map_err(|e| ToolError::new(format!("write failed: {e}")))?;
            ctx.push_effect(Effect::Note {
                title: title.clone(),
                slug: None,
            });
            Ok(json!({"saved": true, "title": title, "chars": text.chars().count()}))
        }
        Action::Search => {
            let q = req(&a.query, "query")?.to_lowercase();
            let hits: Vec<Value> = list(ctx)
                .into_iter()
                .filter_map(|(t, _, text)| {
                    let lines: Vec<String> = text
                        .lines()
                        .filter(|l| l.to_lowercase().contains(&q))
                        .take(5)
                        .map(str::to_owned)
                        .collect();
                    (t.to_lowercase().contains(&q) || !lines.is_empty())
                        .then(|| json!({"title": t, "lines": lines}))
                })
                .take(20)
                .collect();
            Ok(json!({"query": q, "results": hits}))
        }
    }
}

pub struct NotesTool;

#[async_trait::async_trait]
impl Tool for NotesTool {
    fn name(&self) -> &'static str {
        "notes"
    }
    fn description(&self) -> &'static str {
        "The owner's notes, briefings, documents and Google-Keep-style notes (labels, colours, pinned, \
         archived, checklists, reminders), shared with the Secretary app (Notes) and with ChatGPT, Grok and \
         Claude Code: list, read (by title or slug, e.g. \"craftit-meeting\"), create, append, search, labels, \
         and update. A checklist note reads as numbered items; update checks them off (check / uncheck by \
         number or exact text), adds or removes items (add_items, remove_items), and changes labels \
         (add_labels, remove_labels), color, pinned, archived, reminder_at and convert (list or text). \
         search and list filter by label, color, type (list, text, image, audio, link, reminder) and \
         archive (list hides archived notes, search includes them). Read a note whenever Giuseppe names one \
         or asks about a briefing, and use update, not create, to tick off or add to an existing list. \
         Every note result has a `cite` such as [note:craftit-meeting]."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "notes {} {}",
            args["action"].as_str().unwrap_or(""),
            args["title"]
                .as_str()
                .or(args["slug"].as_str())
                .or(args["query"].as_str())
                .unwrap_or("")
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let trimmed = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let title = trimmed(&a.title);
        let content = trimmed(&a.content);
        let has_items = a.items.as_ref().is_some_and(|i| !i.is_empty());
        match a.action {
            Action::Read | Action::Append | Action::Update
                if title.is_none() && trimmed(&a.slug).is_none() =>
            {
                return Err(ToolError::new("title is required"))
            }
            Action::Create if title.is_none() => return Err(ToolError::new("title is required")),
            Action::Create if content.is_none() && !has_items => {
                return Err(ToolError::new("content is required"))
            }
            Action::Append if content.is_none() => {
                return Err(ToolError::new("content is required"))
            }
            Action::Search if trimmed(&a.query).is_none() && !has_filters(&a) => {
                return Err(ToolError::new(
                    "query or a filter (label, color, type) is required",
                ))
            }
            _ => {}
        }
        let req_title = title.clone().or_else(|| trimmed(&a.slug));
        match remote(ctx, &a, req_title.as_deref(), content.as_deref()).await? {
            Remote::Done(v) => Ok(v),
            Remote::Fallback(why) => {
                if why != "not configured" {
                    tracing::warn!(reason = %why, "notes: secretary-server unavailable, using local files");
                }
                local(&a, ctx)
            }
        }
    }
}

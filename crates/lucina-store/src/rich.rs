//! Lucina message format v1 (`docs/lucina-message-format.md`; Node
//! `src/lucina-message-format.js`): short bubbles, typed widgets (blocks) and
//! buttons (actions), plus the plain-text fallback for every surface that does
//! not render them. Pure: the timeline validates stored meta with it, the
//! server composes replies with it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const FORMAT: &str = "v1";
pub const MAX_BUBBLES: usize = 4;
pub const BUBBLE_SOFT_CHARS: usize = 280;
const BUBBLE_HARD_CHARS: usize = 900;
pub const MAX_BLOCKS: usize = 8;
pub const MAX_ACTIONS: usize = 4;
const MAX_LIST_ITEMS: usize = 20;
const MAX_CHECKLIST_ITEMS: usize = 12;
const MAX_STATS: usize = 4;
pub const MAX_TABLE_COLUMNS: usize = 4;
pub const MAX_TABLE_ROWS: usize = 6;
const MAX_STEPS: usize = 8;
const MAX_TITLE: usize = 120;
const MAX_TEXT: usize = 400;
const MAX_BODY: usize = 600;
const MAX_LABEL: usize = 40;
const MAX_CELL: usize = 80;
const MAX_PAYLOAD_TEXT: usize = 500;
pub const MAX_PLACES: usize = 3;
/// `image` blocks per message.
pub const MAX_IMAGES: usize = 2;
const MAX_IMAGE_SRC: usize = 300;
const MAX_ALT: usize = 200;
const MAX_IMAGE_SIDE: u64 = 4096;
const MAX_ADDRESS: usize = 200;
const MAX_EVENT_ID: usize = 200;
/// A `route` to the in-app place map, `/map/place?<query>` (iOS: `secretary://open/map/place?…`).
pub const MAX_MAP_ROUTE: usize = 600;
/// A place's travel time from home, in minutes.
const MAX_TRAVEL_MIN: u64 = 1440;
const TRAVEL_MODES: [&str; 4] = ["walk", "bike", "transit", "drive"];

/// A validated v1 message. `blocks` and `actions` are normalized JSON objects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RichMessage {
    pub format: String,
    pub bubbles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Value>,
}

// ---------------------------------------------------------------- helpers

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{}…", head.trim_end())
}

/// Trimmed text without control characters (newline and tab kept), clipped.
fn clean(v: &Value, max: usize) -> String {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return String::new(),
    };
    let text: String = raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    clip(text.trim(), max)
}

fn line(v: &Value, max: usize) -> String {
    clean(v, max)
        .split('\n')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

fn emoji_of(v: &Value) -> Option<String> {
    let t = line(v, 16);
    (!t.is_empty() && !t.chars().any(|c| c.is_ascii_alphanumeric())).then_some(t)
}

/// An `http(s)` URL, else `None`.
pub fn safe_http_url(v: &Value) -> Option<String> {
    let t = line(v, 2000);
    let lower = t.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let host = t.split("://").nth(1)?.split(['/', '?', '#']).next()?;
    (!host.is_empty() && !t.chars().any(char::is_whitespace)).then_some(t)
}

fn str_v(s: &str) -> Value {
    Value::String(s.to_owned())
}

/// Chunks of at most `max` characters, cut at sentence ends, then at spaces.
pub fn split_into_chunks(text: &str, max: usize) -> Vec<String> {
    let source = text.trim();
    if source.is_empty() {
        return Vec::new();
    }
    if source.chars().count() <= max {
        return vec![source.to_owned()];
    }
    // Sentences: up to and including a run of .!?… (plus closing quotes) or a newline.
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            cur.push('\n');
            sentences.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(c);
        if matches!(c, '.' | '!' | '?' | '…') {
            while i + 1 < chars.len()
                && matches!(
                    chars[i + 1],
                    '.' | '!' | '?' | '…' | '"' | '»' | '”' | '\'' | ')' | ']'
                )
            {
                i += 1;
                cur.push(chars[i]);
            }
            while i + 1 < chars.len() && chars[i + 1] == ' ' {
                i += 1;
                cur.push(' ');
            }
            sentences.push(std::mem::take(&mut cur));
        }
        i += 1;
    }
    if !cur.is_empty() {
        sentences.push(cur);
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for sentence in sentences {
        // The line end after a sentence: bubbles keep their lines and paragraphs.
        if sentence.trim().is_empty() {
            current.push_str(&sentence);
            continue;
        }
        let joined = format!("{current}{sentence}");
        if joined.trim().chars().count() <= max {
            current = joined;
            continue;
        }
        if !current.trim().is_empty() {
            out.push(current.trim().to_owned());
        }
        if sentence.trim().chars().count() <= max {
            current = sentence;
            continue;
        }
        let mut rest: Vec<char> = sentence.trim().chars().collect();
        while rest.len() > max {
            let window: String = rest[..max].iter().collect();
            let cut = match window.rfind(' ') {
                Some(b) if window[..b].chars().count() >= max / 2 => window[..b].chars().count(),
                _ => max,
            };
            out.push(rest[..cut].iter().collect::<String>().trim().to_owned());
            rest = rest[cut..]
                .iter()
                .collect::<String>()
                .trim()
                .chars()
                .collect();
        }
        current = rest.into_iter().collect();
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// What stood between each chunk of `split_into_chunks` and the one before it in `source`, whitespace
/// only (the first gap is whatever preceded it). A chunk not found where expected gets a space.
fn chunk_gaps(source: &str, chunks: &[String]) -> Vec<String> {
    let mut at = 0;
    chunks
        .iter()
        .map(|c| match source[at..].find(c.as_str()) {
            Some(found) => {
                let gap = &source[at..at + found];
                at += found + c.len();
                if gap.trim().is_empty() { gap.to_owned() } else { " ".to_owned() }
            }
            None => " ".to_owned(),
        })
        .collect()
}

/// Chunks of `source` back as one text, with the line ends between them.
fn rejoin_chunks(source: &str, chunks: &[String]) -> String {
    let gaps = chunk_gaps(source, chunks);
    let mut out = String::new();
    for (i, c) in chunks.iter().enumerate() {
        if i > 0 {
            out.push_str(&gaps[i]);
        }
        out.push_str(c);
    }
    out
}

fn normalize_bubbles(v: &Value) -> Result<Vec<String>, String> {
    let Value::Array(items) = v else {
        return Err("bubbles must be an array of strings".into());
    };
    // Each chunk with what separated it from the one before, so a tail glued back together keeps
    // its line ends and lists.
    let mut chunks: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    for raw in items {
        if !raw.is_string() {
            continue;
        }
        let text = clean(raw, 4000);
        if !text.is_empty() {
            let split = split_into_chunks(&text, BUBBLE_SOFT_CHARS);
            let mut item_gaps = chunk_gaps(&text, &split);
            if let Some(first) = item_gaps.first_mut() {
                *first = "\n\n".to_owned();
            }
            gaps.extend(item_gaps);
            chunks.extend(split);
        }
    }
    if chunks.len() > MAX_BUBBLES {
        let mut tail = String::new();
        for (i, c) in chunks.iter().enumerate().skip(MAX_BUBBLES - 1) {
            if i > MAX_BUBBLES - 1 {
                tail.push_str(&gaps[i]);
            }
            tail.push_str(c);
        }
        chunks.truncate(MAX_BUBBLES - 1);
        chunks.push(tail);
    }
    Ok(chunks.iter().map(|c| clip(c, BUBBLE_HARD_CHARS)).collect())
}

// ---------------------------------------------------------------- blocks

fn items(raw: &Value) -> &[Value] {
    raw.get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn with_title(mut block: Map<String, Value>, raw: &Value) -> Value {
    let title = line(&raw["title"], MAX_TITLE);
    if !title.is_empty() {
        block.insert("title".into(), str_v(&title));
    }
    Value::Object(block)
}

fn item_text(item: &Value, max: usize) -> String {
    if item.is_string() {
        clean(item, max)
    } else {
        clean(&item["text"], max)
    }
}

/// One block, or `None` when it does not fit the contract.
pub fn normalize_block(raw: &Value) -> Option<Value> {
    let kind = raw.get("type")?.as_str()?;
    match kind {
        "checklist" => {
            let list: Vec<Value> = items(raw)
                .iter()
                .filter_map(|i| {
                    let text = item_text(i, MAX_TEXT);
                    if text.is_empty() {
                        return None;
                    }
                    Some(if i["done"] == Value::Bool(true) {
                        json!({"text": text, "done": true})
                    } else {
                        json!({"text": text})
                    })
                })
                .take(MAX_CHECKLIST_ITEMS)
                .collect();
            if list.is_empty() {
                return None;
            }
            let mut b = Map::new();
            b.insert("type".into(), str_v("checklist"));
            b.insert("items".into(), Value::Array(list));
            Some(with_title(b, raw))
        }
        "card" => {
            let title = line(&raw["title"], MAX_TITLE);
            let body = clean(&raw["body"], MAX_BODY);
            if title.is_empty() && body.is_empty() {
                return None;
            }
            let tone = raw["tone"]
                .as_str()
                .filter(|t| matches!(*t, "info" | "success" | "warning"))
                .unwrap_or("info");
            let mut b = Map::new();
            b.insert("type".into(), str_v("card"));
            b.insert(
                "title".into(),
                str_v(&if title.is_empty() {
                    line(&str_v(&body), 60)
                } else {
                    title.clone()
                }),
            );
            b.insert("tone".into(), str_v(tone));
            if let Some(e) = emoji_of(&raw["emoji"]) {
                b.insert("emoji".into(), str_v(&e));
            }
            let subtitle = line(&raw["subtitle"], MAX_TITLE);
            if !subtitle.is_empty() {
                b.insert("subtitle".into(), str_v(&subtitle));
            }
            if !body.is_empty() && (!title.is_empty() || body.chars().count() > 60) {
                b.insert("body".into(), str_v(&body));
            }
            Some(Value::Object(b))
        }
        "stat" => {
            let source: Vec<Value> = match raw.get("items") {
                Some(Value::Array(a)) => a.clone(),
                _ => vec![raw.clone()],
            };
            let list: Vec<Value> = source
                .iter()
                .filter_map(|i| {
                    let label = line(&i["label"], MAX_LABEL);
                    let value = line(&i["value"], MAX_LABEL);
                    if label.is_empty() || value.is_empty() {
                        return None;
                    }
                    let delta = line(&i["delta"], MAX_LABEL);
                    Some(if delta.is_empty() {
                        json!({"label": label, "value": value})
                    } else {
                        json!({"label": label, "value": value, "delta": delta})
                    })
                })
                .take(MAX_STATS)
                .collect();
            (!list.is_empty()).then(|| json!({"type": "stat", "items": list}))
        }
        "list" => {
            let list: Vec<Value> = items(raw)
                .iter()
                .filter_map(|i| {
                    let text = item_text(i, MAX_TEXT);
                    if text.is_empty() {
                        return None;
                    }
                    let mut e = Map::new();
                    e.insert("text".into(), str_v(&text));
                    if let Some(em) = emoji_of(&i["emoji"]) {
                        e.insert("emoji".into(), str_v(&em));
                    }
                    let meta = line(&i["meta"], MAX_LABEL * 2);
                    if !meta.is_empty() {
                        e.insert("meta".into(), str_v(&meta));
                    }
                    Some(Value::Object(e))
                })
                .take(MAX_LIST_ITEMS)
                .collect();
            if list.is_empty() {
                return None;
            }
            let mut b = Map::new();
            b.insert("type".into(), str_v("list"));
            b.insert("items".into(), Value::Array(list));
            Some(with_title(b, raw))
        }
        "table" => {
            let columns: Vec<String> = raw["columns"]
                .as_array()?
                .iter()
                .map(|c| line(c, MAX_CELL))
                .take(MAX_TABLE_COLUMNS)
                .collect();
            if columns.iter().all(String::is_empty) {
                return None;
            }
            let rows: Vec<Vec<String>> = raw["rows"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(Value::as_array)
                .map(|r| {
                    (0..columns.len())
                        .map(|k| line(r.get(k).unwrap_or(&Value::Null), MAX_CELL))
                        .collect::<Vec<_>>()
                })
                .filter(|r| r.iter().any(|c| !c.is_empty()))
                .take(MAX_TABLE_ROWS)
                .collect();
            if rows.is_empty() {
                return None;
            }
            let mut b = Map::new();
            b.insert("type".into(), str_v("table"));
            b.insert("columns".into(), json!(columns));
            b.insert("rows".into(), json!(rows));
            Some(with_title(b, raw))
        }
        "steps" => {
            let list: Vec<Value> = items(raw)
                .iter()
                .map(|i| item_text(i, MAX_TEXT))
                .filter(|t| !t.is_empty())
                .take(MAX_STEPS)
                .map(|t| str_v(&t))
                .collect();
            if list.is_empty() {
                return None;
            }
            let mut b = Map::new();
            b.insert("type".into(), str_v("steps"));
            b.insert("items".into(), Value::Array(list));
            Some(with_title(b, raw))
        }
        "quote" => {
            let text = clean(&raw["text"], MAX_BODY);
            if text.is_empty() {
                return None;
            }
            let cite = line(&raw["cite"], MAX_TITLE);
            Some(if cite.is_empty() {
                json!({"type": "quote", "text": text})
            } else {
                json!({"type": "quote", "text": text, "cite": cite})
            })
        }
        "link_preview" => {
            let url = safe_http_url(&raw["url"])?;
            let mut title = line(&raw["title"], MAX_TITLE);
            if title.is_empty() {
                title = url
                    .split("://")
                    .nth(1)
                    .unwrap_or("")
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_owned();
            }
            Some(json!({"type": "link_preview", "url": url, "title": title}))
        }
        "model3d" => crate::model3d::normalize(raw),
        "place" => normalize_place(raw),
        "image" => normalize_image(raw),
        _ => None,
    }
}

/// An image `src`: a path of this API (`/v1/…`, no `..`, no `//`) or an
/// `https://` URL on getsecretary.tech; at most 300 characters.
pub fn valid_image_src(src: &str) -> bool {
    if src.is_empty() || src.len() > MAX_IMAGE_SRC || src.contains("..") {
        return false;
    }
    let safe = |p: &str| {
        !p.contains("//")
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-.%?=&~".contains(&b))
    };
    if src.starts_with("/v1/") {
        return safe(src);
    }
    let Some(rest) = src.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    host.eq_ignore_ascii_case("getsecretary.tech") && (path.is_empty() || safe(path))
}

/// An `image` block: a safe `src` (else dropped), `alt`/`caption` clipped,
/// `width`/`height` kept when sensible.
fn normalize_image(raw: &Value) -> Option<Value> {
    let src = raw["src"].as_str()?.trim();
    if !valid_image_src(src) {
        return None;
    }
    let mut b = Map::new();
    b.insert("type".into(), str_v("image"));
    b.insert("src".into(), str_v(src));
    for k in ["alt", "caption"] {
        let t = line(&raw[k], MAX_ALT);
        if !t.is_empty() {
            b.insert(k.into(), str_v(&t));
        }
    }
    for k in ["width", "height"] {
        if let Some(n) = raw[k].as_u64().filter(|n| (1..=MAX_IMAGE_SIDE).contains(n)) {
            b.insert(k.into(), json!(n));
        }
    }
    Some(Value::Object(b))
}

static PUBLIC_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The public base URL of the apps' API (`LUCINA_PUBLIC_URL`): plain-text
/// image lines link to it. Unset: no link.
pub fn set_public_base(url: &str) {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("https://") || url.starts_with("http://") {
        let _ = PUBLIC_BASE.set(url.to_owned());
    }
}

/// `🗺️ <caption>`, plus the image's link when it can be opened from outside.
pub fn image_text(block: &Value) -> String {
    let src = s(block, "src");
    let mut label = s(block, "caption").to_owned();
    if label.is_empty() {
        label = s(block, "alt").to_owned();
    }
    if label.is_empty() {
        label = "Map".into();
    }
    let link = if src.starts_with("https://") {
        Some(src.to_owned())
    } else {
        PUBLIC_BASE.get().map(|b| format!("{b}{src}"))
    };
    match link {
        Some(l) => format!("🗺️ {label} — {l}"),
        None => format!("🗺️ {label}"),
    }
}

/// A finite JSON number in `[-max, max]`.
fn coordinate(v: &Value, max: f64) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite() && x.abs() <= max)
}

/// An RFC 3339 timestamp, as given (trimmed).
fn rfc3339(v: &Value) -> Option<String> {
    let t = v.as_str()?.trim();
    chrono::DateTime::parse_from_rfc3339(t)
        .is_ok()
        .then(|| t.to_owned())
}

/// A `place` block: a title plus an address or a valid coordinate pair (both
/// or neither, finite, in range, not (0,0)); a bad pair is dropped, the block
/// kept by its address.
fn normalize_place(raw: &Value) -> Option<Value> {
    let title = line(&raw["title"], MAX_TITLE);
    let address = line(&raw["address"], MAX_ADDRESS);
    let coords = match (
        coordinate(&raw["lat"], 90.0),
        coordinate(&raw["lng"], 180.0),
    ) {
        (Some(lat), Some(lng)) => {
            let (lat, lng) = ((lat * 1e6).round() / 1e6, (lng * 1e6).round() / 1e6);
            (lat != 0.0 || lng != 0.0).then_some((lat, lng))
        }
        _ => None,
    };
    if title.is_empty() || (address.is_empty() && coords.is_none()) {
        return None;
    }
    let mut b = Map::new();
    b.insert("type".into(), str_v("place"));
    b.insert("title".into(), str_v(&title));
    if !address.is_empty() {
        b.insert("address".into(), str_v(&address));
    }
    if let Some((lat, lng)) = coords {
        b.insert("lat".into(), json!(lat));
        b.insert("lng".into(), json!(lng));
    }
    if let Some(when) = rfc3339(&raw["when"]) {
        b.insert("when".into(), str_v(&when));
    }
    if let Some(end) = rfc3339(&raw["end"]) {
        b.insert("end".into(), str_v(&end));
    }
    let event_id = line(&raw["eventId"], MAX_EVENT_ID + 1);
    if !event_id.is_empty() && event_id.chars().count() <= MAX_EVENT_ID {
        b.insert("eventId".into(), str_v(&event_id));
    }
    let kind = if raw["kind"] == "event" {
        "event"
    } else {
        "place"
    };
    b.insert("kind".into(), str_v(kind));
    let minutes = raw["travelMin"]
        .as_u64()
        .filter(|m| (1..=MAX_TRAVEL_MIN).contains(m));
    let mode = raw["travelMode"]
        .as_str()
        .filter(|m| TRAVEL_MODES.contains(m));
    if let (Some(minutes), Some(mode)) = (minutes, mode) {
        b.insert("travelMin".into(), json!(minutes));
        b.insert("travelMode".into(), str_v(mode));
        if raw["travelApprox"] == Value::Bool(true) {
            b.insert("travelApprox".into(), Value::Bool(true));
        }
    }
    Some(Value::Object(b))
}

// ---------------------------------------------------------------- actions

fn valid_id(s: &str) -> bool {
    (1..=48).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

/// `/map/place?` and a percent-encoded query (no `+`, no raw spaces), at most
/// [`MAX_MAP_ROUTE`] bytes.
pub fn valid_map_route(p: &str) -> bool {
    p.len() <= MAX_MAP_ROUTE
        && p.strip_prefix("/map/place?").is_some_and(|q| {
            !q.is_empty()
                && q.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"%=&_.~-".contains(&b))
        })
}

fn valid_route(p: &str) -> bool {
    p.starts_with('/')
        && !p.starts_with("//")
        && !p.contains("..")
        && p.len() <= 121
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.%".contains(&b))
}

/// One action (button), or `None`.
pub fn normalize_action(raw: &Value, index: usize) -> Option<Value> {
    let kind = raw.get("kind")?.as_str()?;
    if !matches!(kind, "reply" | "route" | "confirm" | "dismiss" | "secret") {
        return None;
    }
    let label = line(&raw["label"], MAX_LABEL);
    if label.is_empty() {
        return None;
    }
    let id = raw["id"]
        .as_str()
        .filter(|s| valid_id(s))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("a{}", index + 1));
    let p = &raw["payload"];
    let mut payload = Map::new();
    match kind {
        "reply" => {
            let text = clean(&p["text"], MAX_PAYLOAD_TEXT);
            if text.is_empty() {
                return None;
            }
            payload.insert("text".into(), str_v(&text));
            if p["prefill"] == Value::Bool(true) {
                payload.insert("prefill".into(), Value::Bool(true));
            }
        }
        "route" => {
            let path = line(&p["path"], MAX_MAP_ROUTE + 1);
            if !valid_route(&path) && !valid_map_route(&path) {
                return None;
            }
            payload.insert("path".into(), str_v(&path));
        }
        "confirm" => {
            let answer = p["answer"]
                .as_str()
                .filter(|a| matches!(*a, "yes" | "no" | "edit"))?;
            payload.insert("answer".into(), str_v(answer));
            let text = clean(&p["text"], MAX_PAYLOAD_TEXT);
            if answer != "edit" && text.is_empty() {
                return None;
            }
            if !text.is_empty() {
                payload.insert("text".into(), str_v(&text));
            }
        }
        "secret" => {
            // Opens the app's "Add secret" sheet: names only, never a value
            // (`held` names a value Lucina holds in memory for the owner).
            let mode = p["mode"]
                .as_str()
                .filter(|m| matches!(*m, "save" | "project"))?;
            payload.insert("mode".into(), str_v(mode));
            let name_like = |s: &str, max: usize| {
                (1..=max).contains(&s.len())
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-' | b'/')
                    })
            };
            for (key, max) in [
                ("name", 256),
                ("project", 128),
                ("environment", 128),
                ("path", 512),
                ("held", 64),
                ("target", 200),
                // A request in the queue: clients draw its inline card from the request's status.
                ("request", 64),
            ] {
                if let Some(v) = p[key].as_str().filter(|v| name_like(v, max)) {
                    payload.insert(key.into(), str_v(v));
                }
            }
            let project_name = line(&p["projectName"], 64);
            if !project_name.is_empty() {
                payload.insert("projectName".into(), str_v(&project_name));
            }
        }
        _ => {}
    }
    let mut a = Map::new();
    a.insert("id".into(), str_v(&id));
    a.insert("label".into(), str_v(&label));
    a.insert("kind".into(), str_v(kind));
    a.insert("payload".into(), Value::Object(payload));
    if let Some(e) = emoji_of(&raw["emoji"]) {
        a.insert("emoji".into(), str_v(&e));
    }
    Some(Value::Object(a))
}

// ---------------------------------------------------------------- message

/// Validate an untrusted message (the model, stored meta). Bad blocks and
/// actions are dropped (listed in the second value); the message is only
/// invalid without a usable bubble.
pub fn normalize(input: &Value) -> Result<(RichMessage, Vec<String>), Vec<String>> {
    let Value::Object(m) = input else {
        return Err(vec!["message must be a JSON object".into()]);
    };
    let mut errors = Vec::new();
    if let Some(f) = m.get("format") {
        if f != FORMAT {
            errors.push(format!("format must be \"{FORMAT}\""));
        }
    }
    let bubbles = match normalize_bubbles(m.get("bubbles").unwrap_or(&Value::Null)) {
        Ok(b) => b,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };
    if bubbles.is_empty() && errors.is_empty() {
        errors.push("bubbles must contain 1-4 non-empty strings".into());
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut dropped = Vec::new();
    let mut blocks: Vec<Value> = Vec::new();
    for raw in m
        .get("blocks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let places = blocks.iter().filter(|b| b["type"] == "place").count();
        let images = blocks.iter().filter(|b| b["type"] == "image").count();
        match normalize_block(raw) {
            Some(b) if b["type"] == "place" && places >= MAX_PLACES => {
                dropped.push("block \"place\" (over 3)".into())
            }
            Some(b) if b["type"] == "image" && images >= MAX_IMAGES => {
                dropped.push("block \"image\" (over 2)".into())
            }
            Some(b) if blocks.len() >= MAX_BLOCKS => {
                dropped.push(format!("block {} (over {MAX_BLOCKS})", b["type"]));
            }
            Some(b) => blocks.push(b),
            None => dropped.push(format!("block {}", raw.get("type").unwrap_or(&Value::Null))),
        }
    }
    let mut actions: Vec<Value> = Vec::new();
    for raw in m
        .get("actions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(mut a) = normalize_action(raw, actions.len()) else {
            dropped.push(format!(
                "action {}",
                raw.get("kind").unwrap_or(&Value::Null)
            ));
            continue;
        };
        let id = a["id"].as_str().unwrap_or_default().to_owned();
        if actions.iter().any(|x| x["id"] == a["id"]) {
            a["id"] = str_v(&format!("{id}-{}", actions.len() + 1));
        }
        actions.push(a);
        if actions.len() >= MAX_ACTIONS {
            break;
        }
    }
    let lang = m
        .get("lang")
        .and_then(Value::as_str)
        .filter(|l| matches!(*l, "it" | "en" | "da"))
        .map(str::to_owned);
    Ok((
        RichMessage {
            format: FORMAT.into(),
            bubbles,
            lang,
            blocks,
            actions,
        },
        dropped,
    ))
}

/// Code-made blocks first (a tool's map card or image), then the composer's:
/// each normalized, within the block limits (8 in all, 3 places, 2 images).
pub fn merge_blocks(primary: &[Value], secondary: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let preset = primary.iter().filter_map(normalize_block);
    for b in preset.chain(secondary) {
        let count = |t: &str| out.iter().filter(|x| x["type"] == t).count();
        let full = out.len() >= MAX_BLOCKS
            || (b["type"] == "place" && count("place") >= MAX_PLACES)
            || (b["type"] == "image" && count("image") >= MAX_IMAGES);
        if !full {
            out.push(b);
        }
    }
    out
}

/// Code-made actions first (confirmations, hints), then the model's: at most
/// 4, no repeated labels, unique ids.
pub fn merge_actions(primary: Vec<Value>, secondary: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for mut a in primary.into_iter().chain(secondary) {
        if out.len() >= MAX_ACTIONS {
            break;
        }
        let label = a["label"].as_str().unwrap_or_default().to_lowercase();
        if labels.contains(&label) {
            continue;
        }
        labels.push(label);
        let mut id = a["id"].as_str().unwrap_or("a").to_owned();
        while out.iter().any(|x| x["id"] == str_v(&id)) {
            id.push('x');
        }
        a["id"] = str_v(&id);
        out.push(a);
    }
    out
}

// ---------------------------------------------------------------- plain text

/// Inline markdown removed (speech, plain surfaces).
pub fn strip_inline_markdown(text: &str) -> String {
    let mut s = text.to_owned();
    for (open, close) in [("**", "**"), ("__", "__"), ("~~", "~~"), ("`", "`")] {
        s = strip_pairs(&s, open, close);
    }
    // [label](url) -> label (url)
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        if let Some(mid) = after.find("](") {
            let label = &after[..mid];
            let tail = &after[mid + 2..];
            if let Some(end) = tail.find(')') {
                let url = &tail[..end];
                if !label.contains('\n') && (url.starts_with("http") || url.starts_with("mailto:"))
                {
                    out.push_str(&rest[..start]);
                    if label.is_empty() || label == url {
                        out.push_str(url);
                    } else {
                        out.push_str(&format!("{label} ({url})"));
                    }
                    rest = &tail[end + 1..];
                    continue;
                }
            }
        }
        out.push_str(&rest[..=start]);
        rest = &rest[start + 1..];
    }
    out.push_str(rest);
    out
}

fn strip_pairs(s: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find(open) {
        let inner_start = a + open.len();
        match rest[inner_start..].find(close) {
            Some(b) if b > 0 && !rest[inner_start..inner_start + b].contains('\n') => {
                out.push_str(&rest[..a]);
                out.push_str(&rest[inner_start..inner_start + b]);
                rest = &rest[inner_start + b + close.len()..];
            }
            _ => {
                out.push_str(&rest[..inner_start]);
                rest = &rest[inner_start..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn more_items(lang: &str, n: usize) -> String {
    match lang {
        "en" => format!("… and {n} more"),
        "da" => format!("… og {n} mere"),
        _ => format!("… e altri {n}"),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// One block as compact text lines (markdown kept).
pub fn block_to_text(block: &Value, lang: &str, list_limit: usize) -> Vec<String> {
    let titled = |lines: Vec<String>| {
        let title = s(block, "title");
        if title.is_empty() {
            lines
        } else {
            std::iter::once(format!("**{title}**"))
                .chain(lines)
                .collect()
        }
    };
    let arr = |k: &str| {
        block
            .get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    match s(block, "type") {
        "checklist" => titled(
            arr("items")
                .iter()
                .map(|i| {
                    format!(
                        "{} {}",
                        if i["done"] == Value::Bool(true) {
                            "✅"
                        } else {
                            "⬜"
                        },
                        s(i, "text")
                    )
                })
                .collect(),
        ),
        "card" => {
            let emoji = s(block, "emoji");
            let sub = s(block, "subtitle");
            let head = format!(
                "{}**{}**{}",
                if emoji.is_empty() {
                    String::new()
                } else {
                    format!("{emoji} ")
                },
                s(block, "title"),
                if sub.is_empty() {
                    String::new()
                } else {
                    format!(" — {sub}")
                }
            );
            let body = s(block, "body");
            if body.is_empty() {
                vec![head]
            } else {
                vec![head, body.to_owned()]
            }
        }
        "stat" => arr("items")
            .iter()
            .map(|i| {
                let d = s(i, "delta");
                format!(
                    "{}: **{}**{}",
                    s(i, "label"),
                    s(i, "value"),
                    if d.is_empty() {
                        String::new()
                    } else {
                        format!(" ({d})")
                    }
                )
            })
            .collect(),
        "list" => {
            let all = arr("items");
            let mut lines: Vec<String> = all
                .iter()
                .take(list_limit)
                .map(|i| {
                    let e = s(i, "emoji");
                    let m = s(i, "meta");
                    format!(
                        "{}{}{}",
                        if e.is_empty() {
                            "• ".to_owned()
                        } else {
                            format!("{e} ")
                        },
                        s(i, "text"),
                        if m.is_empty() {
                            String::new()
                        } else {
                            format!(" — {m}")
                        }
                    )
                })
                .collect();
            if all.len() > list_limit {
                lines.push(more_items(lang, all.len() - list_limit));
            }
            titled(lines)
        }
        "table" => {
            let cols: Vec<String> = arr("columns")
                .iter()
                .map(|c| c.as_str().unwrap_or("").to_owned())
                .collect();
            let mut lines = vec![cols.join(" · ")];
            for r in arr("rows") {
                lines.push(
                    r.as_array()
                        .map(|r| {
                            r.iter()
                                .map(|c| c.as_str().unwrap_or(""))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        })
                        .unwrap_or_default(),
                );
            }
            titled(lines)
        }
        "steps" => titled(
            arr("items")
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{}. {}", i + 1, t.as_str().unwrap_or("")))
                .collect(),
        ),
        "quote" => {
            let mut lines = vec![format!("> {}", s(block, "text").replace('\n', "\n> "))];
            let cite = s(block, "cite");
            if !cite.is_empty() {
                lines.push(format!("— {cite}"));
            }
            lines
        }
        "link_preview" => vec![format!("🔗 [{}]({})", s(block, "title"), s(block, "url"))],
        "model3d" => vec![crate::model3d::plain_text(block)],
        "place" => vec![place_text(block)],
        "image" => vec![image_text(block)],
        _ => Vec::new(),
    }
}

/// `📍 <title> — <address>` (Telegram, the plain fallback).
pub fn place_text(block: &Value) -> String {
    match travel_text(block) {
        Some(t) => format!("📍 {} · {t}", place_speech(block)),
        None => format!("📍 {}", place_speech(block)),
    }
}

/// `🚌 18 min` (`≈ 12 min 🚶` when estimated) for a place with a travel time.
pub fn travel_text(block: &Value) -> Option<String> {
    let minutes = block["travelMin"].as_u64()?;
    let emoji = match block["travelMode"].as_str()? {
        "walk" => "🚶",
        "bike" => "🚲",
        "transit" => "🚌",
        _ => "🚗",
    };
    let time = if minutes < 60 {
        format!("{minutes} min")
    } else if minutes % 60 == 0 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} h {} min", minutes / 60, minutes % 60)
    };
    Some(if block["travelApprox"] == Value::Bool(true) {
        format!("{emoji} ≈ {time}")
    } else {
        format!("{emoji} {time}")
    })
}

/// `<title> — <address>`: what a voice reads of a place.
pub fn place_speech(block: &Value) -> String {
    let address = s(block, "address");
    if address.is_empty() {
        s(block, "title").to_owned()
    } else {
        format!("{} — {address}", s(block, "title"))
    }
}

/// `↪︎ reply: «yes» · «no»` for the actions a text surface can follow by typing.
pub fn action_hint_line(actions: &[Value], lang: &str) -> String {
    let mut typed = Vec::new();
    let mut routes = Vec::new();
    for a in actions {
        match s(a, "kind") {
            "reply" | "confirm" => {
                let p = &a["payload"];
                let text = s(p, "text");
                let said = if text.is_empty() {
                    s(a, "label").to_owned()
                } else if p["prefill"] == Value::Bool(true) || s(p, "answer") == "edit" {
                    format!("{} …", text.trim_end_matches([' ', ':']))
                } else {
                    text.to_owned()
                };
                typed.push(format!("«{said}»"));
            }
            "route" => routes.push(s(a, "label").to_owned()),
            _ => {}
        }
    }
    let lead = match lang {
        "da" => ("svar", "i appen"),
        _ => ("reply", "in the app"),
    };
    let mut parts = Vec::new();
    if !typed.is_empty() {
        parts.push(format!("{}: {}", lead.0, typed.join(" · ")));
    }
    if !routes.is_empty() {
        parts.push(format!("{}: {}", lead.1, routes.join(" · ")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("↪︎ {}", parts.join(" — "))
    }
}

impl RichMessage {
    fn lang(&self) -> &str {
        self.lang.as_deref().filter(|l| *l != "it").unwrap_or("en")
    }

    /// The plain-text fallback: bubbles, each widget as text, the action hints.
    pub fn plain_text(&self, include_actions: bool) -> String {
        let mut parts: Vec<String> = self.bubbles.clone();
        for b in &self.blocks {
            parts.push(block_to_text(b, self.lang(), usize::MAX).join("\n"));
        }
        if include_actions {
            parts.push(action_hint_line(&self.actions, self.lang()));
        }
        parts.retain(|p| !p.is_empty());
        parts.join("\n\n")
    }

    /// What her voice reads: bubbles and place titles/addresses, without markdown.
    pub fn speech_text(&self) -> String {
        let mut parts = self.bubbles.clone();
        parts.extend(
            self.blocks
                .iter()
                .filter(|b| b["type"] == "place")
                .map(place_speech),
        );
        // List marks and headings are for the eye: the web and iOS drop them too.
        let plain: Vec<String> = strip_inline_markdown(&parts.join(" "))
            .lines()
            .map(|l| {
                let t = l.trim_start();
                let t = t.strip_prefix(['-', '*', '+', '•']).filter(|r| r.starts_with(char::is_whitespace)).unwrap_or(t);
                let hashes = t.chars().take_while(|c| *c == '#').count();
                match (1..=6).contains(&hashes) && t[hashes..].starts_with(char::is_whitespace) {
                    true => t[hashes..].to_owned(),
                    false => t.to_owned(),
                }
            })
            .collect();
        plain.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The timeline meta fields (`format`, `bubbles`, `lang`, `blocks`, `actions`).
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ---------------------------------------------------------------- plain text -> rich

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Heading(String),
    Para(String),
    Bullets(Vec<String>),
    Ordered(Vec<String>),
    Tasks(Vec<(String, bool)>),
    Table(Vec<String>, Vec<Vec<String>>),
    Quote(String),
}

fn heading_of(l: &str) -> Option<String> {
    let t = l.trim();
    if t.starts_with('#') && l.len() - l.trim_start().len() <= 3 {
        let rest = t.trim_start_matches('#');
        if rest.starts_with(' ') && t.len() - rest.len() <= 6 {
            let h = rest.trim().trim_end_matches('#').trim();
            return (!h.is_empty()).then(|| h.to_owned());
        }
    }
    // A line that is only **bold** (optionally with a trailing colon).
    let b = t.trim_end_matches(':').trim_end();
    if b.len() > 4 && b.starts_with("**") && b.ends_with("**") {
        let inner = &b[2..b.len() - 2];
        if !inner.contains('*') && inner.chars().count() <= 120 && !inner.trim().is_empty() {
            return Some(inner.trim().trim_end_matches(':').trim().to_owned());
        }
    }
    None
}

fn bullet_of(l: &str) -> Option<(bool, String)> {
    let t = l.trim_start();
    for m in ["- ", "* ", "+ ", "• ", "– "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some((false, rest.trim().to_owned()));
        }
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if (1..=2).contains(&digits.len()) {
        let rest = &t[digits.len()..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((true, r.trim().to_owned()));
        }
    }
    None
}

fn is_rule(l: &str) -> bool {
    let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
    !t.is_empty() && t.chars().all(|c| matches!(c, '-' | '*' | '_' | '—' | '–'))
}

fn is_table_sep(l: &str) -> bool {
    let t = l.trim();
    t.contains('-')
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
        && t.matches('-').count() >= 2
}

fn split_row(l: &str) -> Vec<String> {
    let t = l.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_owned()).collect()
}

fn segment(text: &str) -> Vec<Segment> {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.trim().is_empty() || is_rule(l) {
            i += 1;
            continue;
        }
        if let Some(h) = heading_of(l) {
            out.push(Segment::Heading(h));
            i += 1;
            continue;
        }
        if l.contains('|') && lines.get(i + 1).is_some_and(|n| is_table_sep(n)) {
            let cols = split_row(l);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(split_row(lines[i]));
                i += 1;
            }
            out.push(Segment::Table(cols, rows));
            continue;
        }
        if l.trim_start().starts_with('>') {
            let mut body = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                body.push(lines[i].trim_start()[1..].trim_start().to_owned());
                i += 1;
            }
            out.push(Segment::Quote(body.join("\n").trim().to_owned()));
            continue;
        }
        if let Some((ordered, _)) = bullet_of(l) {
            let mut list: Vec<String> = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                if let Some((_, item)) = bullet_of(lines[i]) {
                    list.push(item);
                } else if !list.is_empty() && lines[i].starts_with("  ") {
                    let last = list.last_mut().unwrap();
                    last.push(' ');
                    last.push_str(lines[i].trim());
                } else {
                    break;
                }
                i += 1;
            }
            let tasks: Vec<Option<(String, bool)>> = list
                .iter()
                .map(|it| {
                    let b = it.as_bytes();
                    (b.len() > 4
                        && b[0] == b'['
                        && b[2] == b']'
                        && b[3] == b' '
                        && matches!(b[1], b' ' | b'x' | b'X'))
                    .then(|| (it[4..].to_owned(), b[1] != b' '))
                })
                .collect();
            if tasks.iter().all(Option::is_some) {
                out.push(Segment::Tasks(tasks.into_iter().flatten().collect()));
            } else if ordered {
                out.push(Segment::Ordered(list));
            } else {
                out.push(Segment::Bullets(list));
            }
            continue;
        }
        let mut body = vec![l.trim().to_owned()];
        i += 1;
        while i < lines.len()
            && !lines[i].trim().is_empty()
            && bullet_of(lines[i]).is_none()
            && heading_of(lines[i]).is_none()
            && !lines[i].trim_start().starts_with('>')
            && !is_rule(lines[i])
        {
            body.push(lines[i].trim().to_owned());
            i += 1;
        }
        out.push(Segment::Para(body.join("\n")));
    }
    out
}

fn list_item(text: &str) -> Value {
    let t = text.trim();
    let first = t.chars().next().unwrap_or(' ');
    // A leading emoji (not a letter, digit or ASCII mark) becomes the item's emoji.
    if !first.is_alphanumeric() && !first.is_ascii() && !matches!(first, '«' | '“' | '‘' | '"')
    {
        let emoji: String = t.chars().take_while(|c| !c.is_whitespace()).collect();
        if emoji.chars().count() <= 3 {
            let rest = t[emoji.len()..].trim();
            if !rest.is_empty() {
                return json!({"emoji": emoji, "text": rest});
            }
        }
    }
    json!({"text": t})
}

fn first_sentence(text: &str, max: usize) -> String {
    let plain: String = strip_inline_markdown(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let end = plain
        .char_indices()
        .find(|(i, c)| *i >= 8 && matches!(c, '.' | '!' | '?' | '…'))
        .map(|(i, c)| i + c.len_utf8());
    let sentence = match end {
        Some(e) if plain[..e].chars().count() <= max => plain[..e].to_owned(),
        _ => plain,
    };
    clip(&sentence, max)
}

/// Deterministic plain text → v1: loose paragraphs become bubbles (at most
/// `max_bubbles`), headings with their content become widgets. The fallback
/// when the model's JSON is unusable, and the formatter for short replies.
pub fn from_plain_text(
    text: &str,
    lang: &str,
    max_bubbles: usize,
    actions: Vec<Value>,
) -> RichMessage {
    let segments = segment(text);
    let mut bubbles: Vec<String> = Vec::new();
    let mut blocks: Vec<Value> = Vec::new();
    let mut title = String::new();
    let last = segments.len().saturating_sub(1);
    for (index, seg) in segments.iter().enumerate() {
        let room = max_bubbles.saturating_sub(bubbles.len());
        match seg {
            Segment::Heading(h) => {
                let next_is_heading =
                    matches!(segments.get(index + 1), None | Some(Segment::Heading(_)));
                if bubbles.is_empty() && blocks.is_empty() && next_is_heading {
                    bubbles.push(h.clone());
                    continue;
                }
                if !title.is_empty() && blocks.is_empty() && bubbles.is_empty() {
                    bubbles.push(std::mem::take(&mut title));
                }
                title = h.clone();
            }
            Segment::Para(p) => {
                if !title.is_empty() {
                    blocks.push(json!({"type": "card", "title": std::mem::take(&mut title), "body": p, "tone": "info"}));
                    continue;
                }
                let chunks = split_into_chunks(p, BUBBLE_SOFT_CHARS);
                let opening = blocks.is_empty();
                let closing = index == last;
                if (opening || closing) && chunks.len() <= room {
                    bubbles.extend(chunks);
                } else if opening && room > 0 {
                    bubbles.push(chunks[0].clone());
                    let rest = rejoin_chunks(p, &chunks[1..]);
                    if !rest.is_empty() {
                        blocks.push(json!({"type": "card", "title": first_sentence(&rest, 70), "body": rest, "tone": "info"}));
                    }
                } else {
                    blocks.push(json!({"type": "card", "emoji": "📝", "title": first_sentence(p, 70), "body": p, "tone": "info"}));
                }
            }
            Segment::Bullets(list) => {
                let t = std::mem::take(&mut title);
                if list.len() == 1 && !t.is_empty() {
                    blocks
                        .push(json!({"type": "card", "title": t, "body": list[0], "tone": "info"}));
                } else {
                    blocks.push(json!({"type": "list", "title": t, "items": list.iter().map(|x| list_item(x)).collect::<Vec<_>>()}));
                }
            }
            Segment::Ordered(list) => {
                blocks.push(
                    json!({"type": "steps", "title": std::mem::take(&mut title), "items": list}),
                );
            }
            Segment::Tasks(list) => {
                let items: Vec<Value> = list
                    .iter()
                    .map(|(t, d)| json!({"text": t, "done": d}))
                    .collect();
                blocks.push(json!({"type": "checklist", "title": std::mem::take(&mut title), "items": items}));
            }
            Segment::Table(cols, rows) => {
                blocks.push(json!({"type": "table", "title": std::mem::take(&mut title), "columns": cols, "rows": rows}));
            }
            Segment::Quote(q) => blocks.push(json!({"type": "quote", "text": q})),
        }
    }
    if !title.is_empty() {
        if bubbles.len() < max_bubbles {
            bubbles.push(title);
        } else {
            blocks.push(json!({"type": "card", "title": title, "tone": "info"}));
        }
    }
    if bubbles.is_empty() {
        let lead = blocks
            .iter()
            .find(|b| matches!(s(b, "type"), "card" | "list" | "checklist"))
            .map(|b| s(b, "title").to_owned())
            .filter(|t| !t.is_empty());
        bubbles.push(lead.unwrap_or_else(|| match lang {
            "da" => "Her 👇".into(),
            _ => "Here you go 👇".into(),
        }));
    }
    bubbles.truncate(max_bubbles);
    let input = json!({"bubbles": bubbles, "blocks": blocks, "actions": actions, "lang": lang});
    match normalize(&input) {
        Ok((m, _)) => m,
        Err(_) => RichMessage {
            format: FORMAT.into(),
            bubbles: split_into_chunks(&strip_inline_markdown(text), BUBBLE_SOFT_CHARS)
                .into_iter()
                .take(max_bubbles.max(1))
                .collect(),
            lang: Some(lang.to_owned()),
            blocks: Vec::new(),
            actions: Vec::new(),
        },
    }
}

fn text_chars(v: &Value) -> usize {
    match v {
        Value::String(s) => s.chars().count(),
        Value::Array(a) => a.iter().map(text_chars).sum(),
        Value::Object(o) => o.values().map(text_chars).sum(),
        _ => 0,
    }
}

/// Normalizing `raw` into `m` cut text: [`clip`] ends it with "…", the item limits drop the rest.
pub fn loses_text(raw: &Value, m: &RichMessage) -> bool {
    let ellipses = |v: &Value| v.to_string().matches('…').count();
    let text = |v: &Value| text_chars(&v["bubbles"]) + text_chars(&v["blocks"]);
    let out = m.to_value();
    ellipses(&out) > ellipses(raw) || text(&out) + 20 < text(raw)
}

/// Splitting `draft` into `m` cut it (markdown marks may go, so a tenth of the length is allowed).
pub fn loses_draft(draft: &str, m: &RichMessage) -> bool {
    let text = m.plain_text(false);
    text.matches('…').count() > draft.matches('…').count()
        || text.chars().count() * 10 < draft.chars().count() * 9
}

/// Already chat-shaped: short, only paragraphs, at most two chunks.
pub fn is_conversational(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty()
        && t.chars().count() <= BUBBLE_SOFT_CHARS * 2
        && segment(t).iter().all(|s| matches!(s, Segment::Para(_)))
        && split_into_chunks(t, BUBBLE_SOFT_CHARS).len() <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_valid_parts_and_drops_junk() {
        let (m, dropped) = normalize(&json!({
            "bubbles": ["Ciao! ✅"],
            "blocks": [
                {"type": "checklist", "title": "Oggi", "items": [{"text": "Spesa", "done": true}, {"text": "Palestra"}]},
                {"type": "stat", "label": "Peso", "value": "82 kg", "delta": "-0.4"},
                {"type": "table", "columns": ["a","b","c","d","e"], "rows": (0..9).map(|i| vec![format!("r{i}"), "x".into(), "y".into(), "z".into(), "w".into()]).collect::<Vec<Vec<String>>>()},
                {"type": "link_preview", "url": "javascript:alert(1)", "title": "bad"},
                {"type": "html", "html": "<script>"}
            ],
            "actions": [
                {"id": "a", "label": "Sì", "kind": "confirm", "payload": {"answer": "yes", "text": "sì"}},
                {"id": "b", "label": "Note", "kind": "route", "payload": {"path": "/notes/spesa"}},
                {"id": "c", "label": "Evil", "kind": "route", "payload": {"path": "https://evil.example"}},
                {"id": "d", "label": "Up", "kind": "route", "payload": {"path": "/../etc"}},
                {"id": "f", "label": "Dettagli", "kind": "reply", "payload": {"text": "Dammi i dettagli"}},
                {"id": "g", "label": "Chiudi", "kind": "dismiss", "payload": {}},
                {"id": "h", "label": "Fifth", "kind": "reply", "payload": {"text": "x"}}
            ]
        }))
        .unwrap();
        let kinds: Vec<&str> = m.blocks.iter().map(|b| s(b, "type")).collect();
        assert_eq!(kinds, ["checklist", "stat", "table"]);
        assert_eq!(m.blocks[2]["columns"].as_array().unwrap().len(), 4);
        assert_eq!(m.blocks[2]["rows"].as_array().unwrap().len(), 6);
        let ids: Vec<&str> = m.actions.iter().map(|a| s(a, "id")).collect();
        assert_eq!(ids, ["a", "b", "f", "g"]);
        assert_eq!(dropped.len(), 4);
        assert!(normalize(&json!({"bubbles": []})).is_err());
        assert!(normalize(&json!({"bubbles": ["x"], "format": "v2"})).is_err());
        assert!(normalize(&json!("x")).is_err());
    }

    #[test]
    fn long_bubbles_split_and_cap_at_four() {
        let long: String = (0..20)
            .map(|i| format!("Frase numero {i} con un po' di testo per riempire. "))
            .collect();
        let (m, _) = normalize(&json!({"bubbles": [long]})).unwrap();
        assert!(m.bubbles.len() <= MAX_BUBBLES);
        for b in &m.bubbles[..m.bubbles.len() - 1] {
            assert!(b.chars().count() <= BUBBLE_SOFT_CHARS, "{}", b.len());
        }
    }

    #[test]
    fn a_long_opening_paragraph_keeps_its_lines_in_the_card() {
        let source = "one\n- two\n\nthree";
        let chunks = vec!["one".to_owned(), "- two".to_owned(), "three".to_owned()];
        assert_eq!(rejoin_chunks(source, &chunks), source);
        assert_eq!(rejoin_chunks(source, &chunks[1..]), "- two\n\nthree");
    }

    #[test]
    fn bubbles_past_the_limit_keep_their_lines_and_lists() {
        let para = |tag: &str| format!("{tag} {}", "word ".repeat(45).trim());
        let list = "Two things:\n- the first\n- the second";
        let text = format!("{}\n\n{}\n\n{}\n\n{}\n\n{list}\n\n{}", para("a"), para("b"), para("c"), para("d"), para("e"));
        let (m, _) = normalize(&json!({"bubbles": [text]})).unwrap();
        assert_eq!(m.bubbles.len(), MAX_BUBBLES);
        let tail = m.bubbles.last().unwrap();
        assert!(tail.contains(&format!("{}\n\n{list}\n\n{}", para("d"), para("e"))), "{tail:?}");
    }

    #[test]
    fn bubbles_past_the_limit_keep_a_sentence_on_its_line() {
        let s = |tag: &str| format!("{tag} {}.", "word ".repeat(50).trim());
        let text = format!("{} {} {} {} {}", s("a"), s("b"), s("c"), s("d"), s("e"));
        let (m, _) = normalize(&json!({"bubbles": [text]})).unwrap();
        assert_eq!(m.bubbles.len(), MAX_BUBBLES);
        assert_eq!(m.bubbles[3], format!("{} {}", s("d"), s("e")));
    }

    #[test]
    fn long_text_keeps_its_lines_when_it_is_split() {
        let filler = "Parole che riempiono la bolla fino a farla dividere. ".repeat(8);
        let text = format!("Fine della frase.\nWhy: la riga dopo.\n\n1. Primo\n2. Secondo\n{filler}");
        let chunks = split_into_chunks(&text, 280);
        assert!(chunks.len() > 1);
        assert!(chunks[0].starts_with("Fine della frase.\nWhy: la riga dopo.\n\n1. Primo\n2. Secondo"), "{}", chunks[0]);
    }

    #[test]
    fn plain_fallback_and_speech() {
        let (m, _) = normalize(&json!({
            "lang": "it",
            "bubbles": ["Ecco la lista 🛒", "Vuoi **altro**?"],
            "blocks": [{"type": "list", "items": [{"emoji": "🥛", "text": "Latte", "meta": "2 L"}, {"text": "Pane"}]}],
            "actions": [
                {"id": "d", "label": "Dettagli", "kind": "reply", "payload": {"text": "Dammi i dettagli"}},
                {"id": "p", "label": "Promemoria", "kind": "reply", "payload": {"text": "Ricordami alle 21:30 di", "prefill": true}}
            ]
        }))
        .unwrap();
        let text = m.plain_text(true);
        assert!(text.contains("🥛 Latte — 2 L"), "{text}");
        assert!(text.contains("• Pane"));
        assert!(
            text.ends_with("↪︎ reply: «Dammi i dettagli» · «Ricordami alle 21:30 di …»"),
            "{text}"
        );
        assert_eq!(m.speech_text(), "Ecco la lista 🛒 Vuoi altro?");
    }

    #[test]
    fn a_text_wall_becomes_few_bubbles_and_widgets() {
        let wall = "**Inventario fattuale — cosa supporto ORA**\n\n**Canali**\n- Telegram (chat + foto)\n- Discord\n\n**Memoria**\n- Honcho, workspace `hermes-peer1`.\n\n**Limiti**\n- Nessuna email.\n\nSe vuoi, il prossimo passo è decidere *quale* buco colmare.";
        let m = from_plain_text(wall, "it", 3, Vec::new());
        assert!(m.bubbles.len() <= 3);
        assert_eq!(m.bubbles[0], "Inventario fattuale — cosa supporto ORA");
        assert!(m.bubbles.last().unwrap().starts_with("Se vuoi"));
        let kinds: Vec<&str> = m.blocks.iter().map(|b| s(b, "type")).collect();
        assert_eq!(kinds, ["list", "card", "card"]);
        assert_eq!(s(&m.blocks[0], "title"), "Canali");
        assert!(is_conversational(
            "Fatto ✅ Ti ricordo la spesa domani alle 9."
        ));
        assert!(!is_conversational(wall));
        let tasks = from_plain_text(
            "Ecco:\n\n- [x] uno\n- [ ] due\n\n1. primo\n2. secondo",
            "it",
            3,
            Vec::new(),
        );
        let kinds: Vec<&str> = tasks.blocks.iter().map(|b| s(b, "type")).collect();
        assert_eq!(kinds, ["checklist", "steps"]);
    }

    fn place(raw: Value) -> Option<Value> {
        normalize_block(&raw)
    }

    #[test]
    fn a_place_with_coordinates_and_times() {
        let b = place(json!({
            "type": "place", "title": " Hack Night: Rust meets AI ", "address": "Folkehuset Møllestien,\nAarhus",
            "lat": 56.161_123_456, "lng": 10.200_6, "when": "2026-10-27T17:00:00.000Z",
            "end": "2026-10-27T20:00:00+01:00", "eventId": "iphone:abc", "kind": "event", "extra": 1
        }))
        .unwrap();
        assert_eq!(
            b,
            json!({
                "type": "place", "title": "Hack Night: Rust meets AI", "address": "Folkehuset Møllestien, Aarhus",
                "lat": 56.161123, "lng": 10.2006, "when": "2026-10-27T17:00:00.000Z",
                "end": "2026-10-27T20:00:00+01:00", "eventId": "iphone:abc", "kind": "event"
            })
        );
    }

    #[test]
    fn a_place_keeps_a_valid_travel_time() {
        let b = place(
            json!({"type": "place", "title": "Folkehuset", "address": "Møllestien",
            "travelMin": 18, "travelMode": "transit", "travelApprox": false}),
        )
        .unwrap();
        assert_eq!(
            (b["travelMin"].clone(), b["travelMode"].clone()),
            (json!(18), json!("transit"))
        );
        assert!(b.get("travelApprox").is_none());
        assert_eq!(place_text(&b), "📍 Folkehuset — Møllestien · 🚌 18 min");
        let b = place(json!({"type": "place", "title": "F", "address": "M",
            "travelMin": 75, "travelMode": "walk", "travelApprox": true}))
        .unwrap();
        assert_eq!(travel_text(&b).as_deref(), Some("🚶 ≈ 1 h 15 min"));
        for (min, mode) in [
            (json!(0), "walk"),
            (json!(1441), "walk"),
            (json!(5), "car"),
            (json!("5"), "walk"),
        ] {
            let b = place(json!({"type": "place", "title": "F", "address": "M", "travelMin": min, "travelMode": mode})).unwrap();
            assert!(
                b.get("travelMin").is_none() && b.get("travelMode").is_none(),
                "{min} {mode}"
            );
        }
    }

    #[test]
    fn map_place_routes() {
        let path = "/map/place?title=Hack%20Night&address=Folkehuset%20M%C3%B8llestien&when=2026-10-27T16%3A00%3A00.000Z";
        let a = normalize_action(
            &json!({"id": "nav-1", "label": "Naviga", "kind": "route", "payload": {"path": path}}),
            0,
        )
        .unwrap();
        assert_eq!(a["payload"]["path"], path);
        for bad in [
            "/map/place?",
            "/map/place?title=a b",
            "/map/place?title=a+b",
            "/map/other?title=a",
            "/map/place?title=<script>",
        ] {
            assert!(!valid_map_route(bad), "{bad}");
        }
        let long = format!("/map/place?title={}", "a".repeat(MAX_MAP_ROUTE));
        assert!(normalize_action(
            &json!({"label": "Naviga", "kind": "route", "payload": {"path": long}}),
            0
        )
        .is_none());
        assert!(normalize_action(
            &json!({"label": "x", "kind": "route", "payload": {"path": "/notes?x=1"}}),
            0
        )
        .is_none());
    }

    #[test]
    fn a_place_by_address_only_or_coordinates_only() {
        let b = place(json!({"type": "place", "title": "Salling", "address": "Søndergade 27", "kind": "shop"})).unwrap();
        assert_eq!(
            b,
            json!({"type": "place", "title": "Salling", "address": "Søndergade 27", "kind": "place"})
        );
        let b = place(json!({"type": "place", "title": "Home", "lat": 56.19633, "lng": 10.2011}))
            .unwrap();
        assert_eq!(b["lat"], 56.19633);
        assert!(b.get("address").is_none());
    }

    #[test]
    fn a_place_needs_a_title_and_somewhere() {
        assert!(place(
            json!({"type": "place", "address": "Søndergade 27", "lat": 56.1, "lng": 10.2})
        )
        .is_none());
        assert!(
            place(json!({"type": "place", "title": "  ", "address": "Søndergade 27"})).is_none()
        );
        assert!(place(json!({"type": "place", "title": "Somewhere"})).is_none());
        assert!(
            place(json!({"type": "place", "title": "Null Island", "lat": 0.0, "lng": 0.0}))
                .is_none()
        );
    }

    #[test]
    fn bad_coordinates_are_dropped_and_the_address_kept() {
        for (lat, lng) in [
            (json!(56.1), Value::Null),
            (json!("NaN"), json!("NaN")),
            (json!("56.1"), json!("10.2")),
            (json!(91.0), json!(10.0)),
            (json!(56.0), json!(-180.5)),
            (json!(0), json!(0)),
            (json!(0.000_000_1), json!(-0.000_000_1)),
            (Value::Null, json!(10.2)),
        ] {
            let b = place(json!({"type": "place", "title": "Dentist", "address": "Via Roma 1", "lat": lat, "lng": lng}))
                .unwrap();
            assert!(b.get("lat").is_none() && b.get("lng").is_none(), "{b}");
            assert_eq!(b["address"], "Via Roma 1");
        }
        assert!(place(json!({"type": "place", "title": "Dentist", "lat": 56.1})).is_none());
    }

    #[test]
    fn place_coordinate_boundaries_are_valid() {
        for (lat, lng) in [(90.0, 180.0), (-90.0, -180.0), (0.0, 10.0), (56.0, 0.0)] {
            let b = place(json!({"type": "place", "title": "Boundary", "lat": lat, "lng": lng}))
                .unwrap();
            assert_eq!(b["lat"], lat);
            assert_eq!(b["lng"], lng);
        }
    }

    #[test]
    fn excess_places_are_reported_even_after_the_eight_block_limit() {
        let mut blocks = vec![json!({"type": "quote", "text": "x"}); 5];
        blocks.extend(vec![
            json!({"type": "place", "title": "t", "address": "a"});
            4
        ]);
        let (m, dropped) = normalize(&json!({"bubbles": ["Ecco"], "blocks": blocks})).unwrap();
        assert_eq!(m.blocks.len(), MAX_BLOCKS);
        assert_eq!(dropped, [r#"block "place" (over 3)"#]);
    }

    #[test]
    fn place_times_and_ids_must_be_valid() {
        let b = place(json!({
            "type": "place", "title": "Padel", "address": "Club", "when": "tomorrow at 9",
            "end": "2026-10-24", "eventId": "x".repeat(201)
        }))
        .unwrap();
        assert!(b.get("when").is_none() && b.get("end").is_none() && b.get("eventId").is_none());
        let invalid = place(json!({
            "type": "place", "title": "Padel", "address": "Club",
            "when": "2026-10-27T17:00:00Z\u{0000}extra", "end": 123
        }))
        .unwrap();
        assert!(invalid.get("when").is_none() && invalid.get("end").is_none());
        let long =
            place(json!({"type": "place", "title": "t".repeat(300), "address": "a".repeat(300)}))
                .unwrap();
        assert_eq!(long["title"].as_str().unwrap().chars().count(), MAX_TITLE);
        assert_eq!(
            long["address"].as_str().unwrap().chars().count(),
            MAX_ADDRESS
        );
    }

    #[test]
    fn at_most_three_places_and_their_plain_text() {
        let places: Vec<Value> = (1..=4)
            .map(|i| json!({"type": "place", "title": format!("Place {i}"), "address": format!("Street {i}")}))
            .collect();
        let mut blocks = vec![json!({"type": "quote", "text": "x"})];
        blocks.extend(places);
        blocks.push(json!({"type": "place", "title": "Only coords", "lat": 56.2, "lng": 10.2}));
        let (m, dropped) = normalize(&json!({"bubbles": ["Ecco"], "blocks": blocks})).unwrap();
        let titles: Vec<&str> = m
            .blocks
            .iter()
            .filter(|b| b["type"] == "place")
            .map(|b| s(b, "title"))
            .collect();
        assert_eq!(titles, ["Place 1", "Place 2", "Place 3"]);
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert!(dropped.iter().all(|d| d.contains("place")));
        let text = m.plain_text(false);
        assert!(
            text.contains("📍 Place 1 — Street 1\n\n📍 Place 2 — Street 2"),
            "{text}"
        );
        let coords_only = place(json!({"type": "place", "title": "Home", "lat": 56.2, "lng": 10.2, "when": "2026-10-27T17:00:00Z"})).unwrap();
        assert_eq!(block_to_text(&coords_only, "it", 5), ["📍 Home"]);
        assert_eq!(place_speech(&m.blocks[1]), "Place 1 — Street 1");
        assert_eq!(
            m.speech_text(),
            "Ecco Place 1 — Street 1 Place 2 — Street 2 Place 3 — Street 3"
        );
    }
}

#[cfg(test)]
mod secret_action_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secret_actions_keep_names_and_drop_values() {
        let a = normalize_action(
            &json!({"id": "save-secret", "label": "Save as secret", "kind": "secret",
                    "payload": {"mode": "save", "held": "held-0123abcd", "name": "OPENAI_API_KEY",
                                "project": "proj-1", "environment": "prod", "path": "/Pokobot",
                                "value": "sk-should-never-pass-0123456789", "secretValue": "x"}}),
            0,
        )
        .unwrap();
        let p = &a["payload"];
        assert_eq!(p["mode"], "save");
        assert_eq!(p["held"], "held-0123abcd");
        assert_eq!(p["path"], "/Pokobot");
        assert!(p.get("value").is_none() && p.get("secretValue").is_none());
        let a = normalize_action(
            &json!({"label": "Create project", "kind": "secret", "payload": {"mode": "project", "projectName": "Poko Shorts"}}),
            1,
        )
        .unwrap();
        assert_eq!(a["payload"]["projectName"], "Poko Shorts");
        // Unknown mode, or a name with spaces (dropped, the action stays).
        assert!(normalize_action(
            &json!({"label": "x", "kind": "secret", "payload": {"mode": "reveal"}}),
            0
        )
        .is_none());
        let a = normalize_action(
            &json!({"label": "x", "kind": "secret", "payload": {"mode": "save", "name": "a b"}}),
            0,
        )
        .unwrap();
        assert!(a["payload"].get("name").is_none());
    }

    #[test]
    fn image_blocks_are_safe_and_few() {
        let img = |src: &str| {
            normalize_block(
                &json!({"type": "image", "src": src, "alt": "Map", "caption": "AU building 1532", "width": 800, "height": 600}),
            )
        };
        let b = img("/v1/au/map.png?building=1532&z=17").unwrap();
        assert_eq!(b["src"], "/v1/au/map.png?building=1532&z=17");
        assert_eq!(b["width"], 800);
        assert_eq!(b["caption"], "AU building 1532");
        assert!(img("https://getsecretary.tech/v1/au/map.png?z=17").is_some());
        for bad in [
            "//evil.example/x.png",
            "/v1//evil.example/x.png",
            "javascript:alert(1)",
            "/v1/../etc/passwd",
            "/v1/au/../../x",
            "https://evil.example/x.png",
            "https://getsecretary.tech.evil.example/x.png",
            "http://getsecretary.tech/x.png",
            "/api/secretary/au/map.png",
            "data:image/png;base64,AAAA",
            "/v1/au/map.png?q=a b",
        ] {
            assert!(img(bad).is_none(), "{bad}");
        }
        assert!(img(&format!("/v1/{}", "a".repeat(300))).is_none());
        let long = normalize_block(
            &json!({"type": "image", "src": "/v1/x.png", "alt": "a".repeat(500), "width": 0}),
        )
        .unwrap();
        assert_eq!(long["alt"].as_str().unwrap().chars().count(), 200);
        assert!(long.get("width").is_none());
        let (m, dropped) = normalize(&json!({"bubbles": ["Ecco"], "blocks": [
            {"type": "image", "src": "/v1/a.png"}, {"type": "image", "src": "/v1/b.png"}, {"type": "image", "src": "/v1/c.png"}
        ]})).unwrap();
        assert_eq!(m.blocks.len(), 2);
        assert_eq!(dropped, [r#"block "image" (over 2)"#]);
        assert_eq!(block_to_text(&m.blocks[0], "it", 5), ["🗺️ Map"]);
        assert_eq!(image_text(&b), "🗺️ AU building 1532");
        let merged = merge_blocks(
            &[
                json!({"type": "image", "src": "/v1/x.png"}),
                json!({"type": "image", "src": "//evil"}),
            ],
            m.blocks.clone(),
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["src"], "/v1/x.png");
    }
}

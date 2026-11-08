//! Reading Takeout zips and parsing Keep notes.
//!
//! The zips stay on disk: only the central directories (entry names) are
//! indexed, note JSON files are read one at a time and attachments are read
//! on demand ([`Takeout::read_attachment`]), so memory stays small on a
//! 1 GB host. Parsing is lenient: unknown fields are ignored, missing ones
//! defaulted, and a bad file becomes a [`ReportError`] while the import
//! continues.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::html::html_to_markdown;
use super::report::{FileCounts, ReportError};

/// Attachment limits (the store's: 25 MB each, 50 per note).
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_NOTE: usize = 50;
const MAX_TITLE_CHARS: usize = 200;
const MAX_LABEL_CHARS: usize = 50;
const MAX_LABELS_PER_NOTE: usize = 50;
/// A note JSON larger than this is an error, not read.
const MAX_NOTE_JSON_BYTES: u64 = 8 * 1024 * 1024;
/// `.json` outside a `Keep/` folder is only looked at when it is small.
const MAX_SNIFF_JSON_BYTES: u64 = 4 * 1024 * 1024;

/// Takeout's colour names, lowercased.
const COLORS: &[&str] = &[
    "default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink",
    "brown", "gray",
];

/// Attachment types the store accepts.
pub const ALLOWED_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/heic",
    "image/heif",
    "audio/3gpp",
    "audio/mp4",
    "audio/aac",
    "audio/mpeg",
    "audio/ogg",
    "audio/webm",
    "audio/x-m4a",
    "application/pdf",
];

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];

// ------------------------------------------------------------- note model

/// A web link (`annotations` with `source: WEBLINK`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAnnotation {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub source: Option<String>,
}

/// An attachment a note names, resolved (or not) inside the zips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// `filePath` as written in the JSON.
    pub file_path: String,
    /// `mimetype` as written in the JSON (may be empty or wrong).
    pub declared_mime: String,
    /// Index into the archive's entries when the file was found.
    pub entry: Option<usize>,
    /// Name of the found entry (`Takeout/Keep/x.jpg`), for messages.
    pub entry_name: Option<String>,
}

/// One Keep note, ready to map onto the store's `ImportedNote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNote {
    /// Entry name of the JSON file.
    pub file: String,
    /// `gkeep:<createdTimestampUsec>[-n]`.
    pub key: String,
    /// `userEditedTimestampUsec`.
    pub edited_usec: i64,
    /// At most 200 chars.
    pub title: String,
    /// Markdown; empty for a list note (the store derives it from the items).
    pub content: String,
    /// `(text, checked)`; `Some` = list note.
    pub items: Option<Vec<(String, bool)>>,
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    pub trashed: bool,
    /// Display names, de-duplicated case-insensitively.
    pub labels: Vec<String>,
    pub attachments: Vec<AttachmentRef>,
    pub annotations: Vec<KeepAnnotation>,
    /// Non-owner sharees' emails.
    pub collaborators: Vec<String>,
    /// RFC 3339, millisecond precision.
    pub created_at: String,
    pub updated_at: String,
    /// Things dropped on the way (too many labels or attachments); the scan
    /// turns them into report errors.
    pub warnings: Vec<String>,
}

impl ParsedNote {
    /// `list`, `image`, `audio`, `link` or `text` (the report's `kind`).
    pub fn kind(&self) -> &'static str {
        let has = |p: &str, exts: &[&str]| {
            self.attachments.iter().any(|a| {
                let m = a.declared_mime.to_ascii_lowercase();
                let f = a.file_path.to_ascii_lowercase();
                m.starts_with(p) || exts.iter().any(|e| f.ends_with(e))
            })
        };
        if self.items.is_some() {
            "list"
        } else if has(
            "image/",
            &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".heic"],
        ) {
            "image"
        } else if has("audio/", &[".3gp", ".m4a", ".aac", ".mp3", ".ogg"]) {
            "audio"
        } else if !self.annotations.is_empty() {
            "link"
        } else {
            "text"
        }
    }
}

/// What [`Takeout::scan`] found.
#[derive(Debug, Default)]
pub struct Scan {
    pub notes: Vec<ParsedNote>,
    /// `Labels.txt` lines (display names).
    pub label_names: Vec<String>,
    pub files: FileCounts,
    pub errors: Vec<ReportError>,
}

// ---------------------------------------------------------------- archive

struct Part {
    path: PathBuf,
    zip: ZipArchive<File>,
}

struct Entry {
    part: usize,
    idx: usize,
    /// `/`-separated, no leading slash.
    name: String,
    size: u64,
}

/// One or more Takeout zips, opened for reading.
pub struct Takeout {
    parts: Vec<Part>,
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    by_lower: HashMap<String, usize>,
    /// Lowercased base name → entries (only used when unique).
    by_base: HashMap<String, Vec<usize>>,
}

fn norm_name(raw: &str) -> String {
    let s = raw.replace('\\', "/");
    let mut s = s.as_str();
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.trim_start_matches('/').to_owned()
}

fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn parent_dir(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(d, _)| d)
}

fn ext_of(name: &str) -> Option<&str> {
    let b = base_name(name);
    b.rsplit_once('.').map(|(_, e)| e).filter(|e| !e.is_empty())
}

fn in_keep_dir(name: &str) -> bool {
    let dir = parent_dir(name);
    dir.split('/').any(|c| c.eq_ignore_ascii_case("keep"))
}

impl Takeout {
    /// Open the zips and index their entry names (no entry is read yet).
    pub fn open(paths: &[PathBuf]) -> Result<Self, String> {
        let mut t = Takeout {
            parts: Vec::new(),
            entries: Vec::new(),
            by_name: HashMap::new(),
            by_lower: HashMap::new(),
            by_base: HashMap::new(),
        };
        for p in paths {
            let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut zip = ZipArchive::new(f)
                .map_err(|e| format!("{}: not a readable zip ({e})", p.display()))?;
            let part = t.parts.len();
            for idx in 0..zip.len() {
                let Ok(e) = zip.by_index_raw(idx) else {
                    continue;
                };
                if e.is_dir() {
                    continue;
                }
                let name = norm_name(e.name());
                if name.is_empty() || name.ends_with('/') || t.by_name.contains_key(&name) {
                    continue;
                }
                let i = t.entries.len();
                t.by_name.insert(name.clone(), i);
                t.by_lower.entry(name.to_lowercase()).or_insert(i);
                t.by_base
                    .entry(base_name(&name).to_lowercase())
                    .or_default()
                    .push(i);
                t.entries.push(Entry {
                    part,
                    idx,
                    size: e.size(),
                    name,
                });
            }
            t.parts.push(Part {
                path: p.clone(),
                zip,
            });
        }
        Ok(t)
    }

    /// Read one entry, at most `max` bytes (bigger is an error).
    fn read_entry(&mut self, e: usize, max: u64) -> Result<Vec<u8>, String> {
        let (part, idx, size) = {
            let en = &self.entries[e];
            (en.part, en.idx, en.size)
        };
        if size > max {
            return Err(format!("too large ({size} bytes, limit {max})"));
        }
        let p = &mut self.parts[part];
        let mut f = p
            .zip
            .by_index(idx)
            .map_err(|er| format!("cannot read from {}: {er}", p.path.display()))?;
        let mut buf = Vec::with_capacity(size.min(max) as usize);
        (&mut f)
            .take(max + 1)
            .read_to_end(&mut buf)
            .map_err(|er| format!("cannot read: {er}"))?;
        if buf.len() as u64 > max {
            return Err(format!("too large (over {max} bytes)"));
        }
        Ok(buf)
    }

    /// Bytes of a resolved attachment (25 MB cap). One at a time: callers
    /// drop the buffer before asking for the next.
    pub fn read_attachment(&mut self, a: &AttachmentRef) -> Result<Vec<u8>, String> {
        let e = a
            .entry
            .ok_or_else(|| format!("{}: not in the export", a.file_path))?;
        self.read_entry(e, MAX_ATTACHMENT_BYTES)
            .map_err(|er| format!("{}: {er}", a.file_path))
    }

    /// Entries that name a Keep note, in file-name order, plus the other
    /// bookkeeping (Labels.txt, html).
    fn classify(&self) -> (Vec<usize>, Vec<usize>, Vec<usize>, usize) {
        let (mut sure, mut maybe, mut labels, mut html) = (vec![], vec![], vec![], 0);
        for (i, e) in self.entries.iter().enumerate() {
            let lower = e.name.to_lowercase();
            let base = base_name(&lower);
            if base == "labels.txt" {
                labels.push(i);
            } else if lower.ends_with(".html") || lower.ends_with(".htm") {
                html += 1;
            } else if lower.ends_with(".json") {
                if in_keep_dir(&e.name) {
                    sure.push(i);
                } else if e.size <= MAX_SNIFF_JSON_BYTES {
                    maybe.push(i);
                }
            }
        }
        (sure, maybe, labels, html)
    }

    /// Find every note, parse it, resolve its attachments.
    pub fn scan(&mut self) -> Scan {
        let mut scan = Scan::default();
        let (sure, maybe, labels, html) = self.classify();
        scan.files.html = html;

        for l in labels {
            match self.read_entry(l, 1024 * 1024) {
                Ok(b) => {
                    for line in String::from_utf8_lossy(&b).lines() {
                        let line = line.trim_start_matches('\u{feff}');
                        if let Some(n) = normalize_label(line) {
                            scan.label_names.push(n);
                        }
                    }
                }
                Err(e) => scan.errors.push(ReportError {
                    file: self.entries[l].name.clone(),
                    error: e,
                }),
            }
        }

        let sure_set: HashSet<usize> = sure.iter().copied().collect();
        let mut candidates: Vec<usize> = sure.into_iter().chain(maybe).collect();
        candidates.sort_by(|a, b| self.entries[*a].name.cmp(&self.entries[*b].name));

        let mut key_counts: HashMap<String, usize> = HashMap::new();
        let mut json_files = HashSet::new();
        let mut used_attachments: BTreeSet<usize> = BTreeSet::new();
        for c in candidates {
            let name = self.entries[c].name.clone();
            let max = if sure_set.contains(&c) {
                MAX_NOTE_JSON_BYTES
            } else {
                MAX_SNIFF_JSON_BYTES
            };
            let bytes = match self.read_entry(c, max) {
                Ok(b) => b,
                Err(e) => {
                    if sure_set.contains(&c) {
                        json_files.insert(c);
                        scan.errors.push(ReportError {
                            file: name,
                            error: e,
                        });
                    }
                    continue;
                }
            };
            if !sure_set.contains(&c)
                && !bytes
                    .windows(b"\"createdTimestampUsec\"".len())
                    .any(|w| w == b"\"createdTimestampUsec\"")
            {
                continue; // some other product's JSON
            }
            json_files.insert(c);
            match parse_note_json(&name, &bytes) {
                Ok(mut n) => {
                    for w in n.warnings.drain(..) {
                        scan.errors.push(ReportError {
                            file: name.clone(),
                            error: w,
                        });
                    }
                    let n_seen = key_counts.entry(n.key.clone()).or_insert(0);
                    *n_seen += 1;
                    if *n_seen > 1 {
                        n.key = format!("{}-{}", n.key, n_seen);
                    }
                    for a in &mut n.attachments {
                        if let Some(e) = self.resolve(&name, &a.file_path) {
                            a.entry = Some(e);
                            a.entry_name = Some(self.entries[e].name.clone());
                            used_attachments.insert(e);
                        }
                    }
                    scan.notes.push(n);
                }
                Err(e) => scan.errors.push(ReportError {
                    file: name,
                    error: e,
                }),
            }
        }

        scan.files.json = json_files.len();
        scan.files.attachments = used_attachments.len();
        let labels_files = self
            .entries
            .iter()
            .filter(|e| base_name(&e.name).eq_ignore_ascii_case("labels.txt"))
            .count();
        let counted = scan.files.json + scan.files.html + labels_files + used_attachments.len();
        scan.files.ignored = self.entries.len().saturating_sub(counted);
        scan
    }

    /// Find an attachment next to the note's JSON: exact name, then
    /// case-insensitive, then the other image extensions (Takeout writes
    /// `x.jpeg` in the JSON for an `x.jpg` file and vice versa), then a
    /// unique file with that base name anywhere in the export.
    fn resolve(&self, json_name: &str, file_path: &str) -> Option<usize> {
        let fp = norm_name(file_path);
        if fp.is_empty() {
            return None;
        }
        let dir = parent_dir(json_name);
        let mut bases = Vec::new();
        if !dir.is_empty() {
            bases.push(format!("{dir}/{fp}"));
        }
        bases.push(fp.clone());
        let exact = |s: &str| {
            self.by_name
                .get(s)
                .or_else(|| self.by_lower.get(&s.to_lowercase()))
                .copied()
        };
        for b in &bases {
            if let Some(i) = exact(b) {
                return Some(i);
            }
        }
        let ext = ext_of(&fp).map(str::to_ascii_lowercase);
        if ext.as_deref().is_some_and(|e| IMAGE_EXTS.contains(&e)) {
            for b in &bases {
                let stem = &b[..b.len() - ext.as_ref().map_or(0, |e| e.len() + 1)];
                for alt in IMAGE_EXTS {
                    if Some(*alt) == ext.as_deref() {
                        continue;
                    }
                    if let Some(i) = exact(&format!("{stem}.{alt}")) {
                        return Some(i);
                    }
                }
            }
        }
        let base = base_name(&fp).to_lowercase();
        match self.by_base.get(&base) {
            Some(v) if v.len() == 1 => Some(v[0]),
            _ => None,
        }
    }
}

// ----------------------------------------------------------------- labels

/// A label as the store will keep its display name: control characters and
/// commas become spaces, inner whitespace collapses, a leading `#` goes, at
/// most 50 characters. `None` when nothing is left.
pub fn normalize_label(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() || c == ',' { ' ' } else { c })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let joined = joined.trim_start_matches('#').trim();
    if joined.is_empty() {
        return None;
    }
    Some(
        joined
            .chars()
            .take(MAX_LABEL_CHARS)
            .collect::<String>()
            .trim_end()
            .to_owned(),
    )
}

// --------------------------------------------------------------- mimetypes

/// A type recognised from the first bytes. `generic`: the container
/// (ISO base media, WebM) does not say audio or video by itself, so it only
/// stands when the JSON declared an audio type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed {
    pub mime: &'static str,
    pub generic: bool,
}

/// Recognise an allowed attachment type from its bytes.
pub fn sniff_mime(b: &[u8]) -> Option<Sniffed> {
    let some = |mime| {
        Some(Sniffed {
            mime,
            generic: false,
        })
    };
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return some("image/png");
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return some("image/jpeg");
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return some("image/gif");
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return some("image/webp");
    }
    if b.starts_with(b"%PDF-") {
        return some("application/pdf");
    }
    if b.starts_with(b"OggS") {
        return some("audio/ogg");
    }
    if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Sniffed {
            mime: "audio/webm",
            generic: true,
        });
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let brand = &b[8..12];
        return match brand {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" => some("image/heic"),
            b"mif1" | b"msf1" | b"heif" => some("image/heif"),
            b"M4A " | b"M4B " | b"M4P " => some("audio/x-m4a"),
            x if x.starts_with(b"3g") => some("audio/3gpp"),
            _ => Some(Sniffed {
                mime: "audio/mp4",
                generic: true,
            }),
        };
    }
    if b.len() >= 2 && b[0] == 0xFF {
        if b[1] & 0xF6 == 0xF0 {
            return some("audio/aac");
        }
        if b[1] & 0xE0 == 0xE0 && (b[1] >> 1) & 3 != 0 {
            return some("audio/mpeg");
        }
    }
    if b.starts_with(b"ID3") {
        return some("audio/mpeg");
    }
    None
}

/// Lowercase, no parameters, common aliases mapped.
fn normalize_declared(m: &str) -> String {
    let m = m
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match m.as_str() {
        "image/jpg" | "image/pjpeg" => "image/jpeg".into(),
        "audio/mp3" | "audio/x-mp3" | "audio/mpeg3" => "audio/mpeg".into(),
        "audio/3gp" | "audio/x-3gpp" | "video/3gpp" => "audio/3gpp".into(),
        "audio/m4a" | "audio/x-mp4" | "audio/mp4a-latm" => "audio/x-m4a".into(),
        "audio/x-aac" => "audio/aac".into(),
        _ => m,
    }
}

fn same_family(a: &str, b: &str) -> bool {
    const ISO_AUDIO: &[&str] = &["audio/3gpp", "audio/mp4", "audio/x-m4a"];
    const HEIF: &[&str] = &["image/heic", "image/heif"];
    a == b
        || (ISO_AUDIO.contains(&a) && ISO_AUDIO.contains(&b))
        || (HEIF.contains(&a) && HEIF.contains(&b))
}

fn allowed(m: &str) -> Option<&'static str> {
    ALLOWED_MIMES.iter().copied().find(|a| *a == m)
}

/// The mimetype to store: the bytes win over a missing or wrong declaration;
/// a type the store does not take is an error message for the report.
pub fn decide_mime(declared: &str, bytes: &[u8]) -> Result<&'static str, String> {
    let decl = normalize_declared(declared);
    let decl_ok = allowed(&decl);
    match sniff_mime(bytes) {
        Some(s) if !s.generic => match decl_ok {
            Some(d) if same_family(d, s.mime) => Ok(d),
            _ => Ok(s.mime),
        },
        Some(s) => match decl_ok {
            Some(d) if d.starts_with("audio/") && same_family(d, s.mime) => Ok(d),
            _ => Err(format!(
                "unsupported attachment type ({} container, declared {:?})",
                s.mime, declared
            )),
        },
        None => decl_ok.ok_or_else(|| {
            format!(
                "unsupported attachment type (declared {:?})",
                if declared.is_empty() {
                    "none"
                } else {
                    declared
                }
            )
        }),
    }
}

// ---------------------------------------------------------------- parsing

fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

fn bool_of(o: &Map<String, Value>, k: &str) -> bool {
    match o.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn i64_of(o: &Map<String, Value>, k: &str) -> Option<i64> {
    match o.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn rfc3339_usec(usec: i64) -> Option<String> {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(usec) * 1000).ok()?;
    let t = t
        .replace_nanosecond(t.millisecond() as u32 * 1_000_000)
        .unwrap_or(t);
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn clip_title(title: &str) -> (String, Option<String>) {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return (title.to_owned(), None);
    }
    let mut short: String = title.chars().take(MAX_TITLE_CHARS - 1).collect();
    short = short.trim_end().to_owned();
    short.push('…');
    (short, Some(title.to_owned()))
}

/// Rich text of one text field: the HTML converted to markdown, else the
/// plain text.
fn rich(plain: &str, html: &str) -> String {
    if !html.trim().is_empty() {
        let md = html_to_markdown(html);
        if !md.trim().is_empty() {
            return md;
        }
    }
    let plain = plain.trim_matches(['\n', '\r']);
    if plain.trim().is_empty() {
        String::new()
    } else {
        plain.to_owned()
    }
}

/// Parse one Takeout Keep JSON file. `file` is the entry name (kept for
/// messages and for the key order); the key gets no collision suffix here.
pub fn parse_note_json(file: &str, bytes: &[u8]) -> Result<ParsedNote, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))?;
    let o = v.as_object().ok_or("not a JSON object")?;

    let created = i64_of(o, "createdTimestampUsec");
    let edited = i64_of(o, "userEditedTimestampUsec");
    let (created_at, updated_at, key, edited_usec) = {
        let c = created.or(edited);
        let e = edited.or(created);
        let key = match c {
            Some(c) => format!("gkeep:{c}"),
            None => {
                let h = Sha256::digest(bytes);
                let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
                format!("gkeep:h{hex}")
            }
        };
        let now = || {
            rfc3339_usec((time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1000) as i64)
                .unwrap_or_default()
        };
        let ca = c.and_then(rfc3339_usec).unwrap_or_else(now);
        let ua = e.and_then(rfc3339_usec).unwrap_or_else(|| ca.clone());
        (ca, ua, key, e.unwrap_or(0))
    };

    let title_raw = str_of(o, "title").trim();
    let (title, full_title) = clip_title(title_raw);

    // Items or text.
    let list = o.get("listContent").and_then(Value::as_array);
    let text = rich(str_of(o, "textContent"), str_of(o, "textContentHtml"));
    let is_list = list.is_some_and(|l| !l.is_empty() || text.is_empty());
    let (mut content, mut items) = (text, None);
    if is_list {
        let mut v: Vec<(String, bool)> = Vec::new();
        for it in list.into_iter().flatten() {
            let Some(it) = it.as_object() else { continue };
            let t = {
                let t = str_of(it, "text").trim();
                if t.is_empty() {
                    rich("", str_of(it, "textHtml"))
                } else {
                    t.to_owned()
                }
            };
            if !t.is_empty() {
                v.push((t, bool_of(it, "isChecked")));
            }
        }
        content = String::new();
        items = Some(v);
    }
    if let Some(full) = full_title {
        match items.as_mut() {
            Some(v) => v.insert(0, (full, false)),
            None if content.is_empty() => content = full,
            None => content = format!("{full}\n\n{content}"),
        }
    }

    // Colour.
    let color_raw = str_of(o, "color").trim().to_ascii_lowercase();
    let color = COLORS
        .iter()
        .find(|c| **c == color_raw)
        .copied()
        .unwrap_or("default")
        .to_owned();

    let mut warnings: Vec<String> = Vec::new();
    // Labels.
    let mut labels: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for l in o
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = match l {
            Value::Object(m) => str_of(m, "name"),
            Value::String(s) => s.as_str(),
            _ => "",
        };
        if let Some(n) = normalize_label(name) {
            if seen.insert(n.to_lowercase()) {
                if labels.len() < MAX_LABELS_PER_NOTE {
                    labels.push(n);
                } else {
                    warnings.push(format!(
                        "label {n:?} dropped (over {MAX_LABELS_PER_NOTE} labels)"
                    ));
                }
            }
        }
    }

    // Attachments.
    let mut attachments = Vec::new();
    for a in o
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(a) = a.as_object() else { continue };
        let fp = str_of(a, "filePath").trim();
        if fp.is_empty() {
            continue;
        }
        if attachments.len() >= MAX_ATTACHMENTS_PER_NOTE {
            warnings.push(format!(
                "attachment {fp} dropped (over {MAX_ATTACHMENTS_PER_NOTE} per note)"
            ));
            continue;
        }
        attachments.push(AttachmentRef {
            file_path: fp.to_owned(),
            declared_mime: str_of(a, "mimetype").trim().to_owned(),
            entry: None,
            entry_name: None,
        });
    }

    // Web links.
    let mut annotations = Vec::new();
    for a in o
        .get("annotations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(a) = a.as_object() else { continue };
        let url = str_of(a, "url").trim();
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            continue;
        }
        let opt = |k: &str| Some(str_of(a, k).trim().to_owned()).filter(|s| !s.is_empty());
        annotations.push(KeepAnnotation {
            url: url.to_owned(),
            title: opt("title"),
            description: opt("description"),
            source: opt("source"),
        });
    }

    // Sharees (non-owners).
    let mut collaborators: Vec<String> = Vec::new();
    for s in o
        .get("sharees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(s) = s.as_object() else { continue };
        let email = str_of(s, "email").trim();
        if !bool_of(s, "isOwner") && !email.is_empty() && !collaborators.iter().any(|c| c == email)
        {
            collaborators.push(email.to_owned());
        }
    }

    let empty = title.is_empty()
        && content.trim().is_empty()
        && items.as_ref().is_none_or(Vec::is_empty)
        && attachments.is_empty();
    if empty {
        return Err("empty note (no title, text, items or attachments), skipped".into());
    }

    Ok(ParsedNote {
        file: file.to_owned(),
        key,
        edited_usec,
        title,
        content,
        items,
        color,
        pinned: bool_of(o, "isPinned"),
        archived: bool_of(o, "isArchived"),
        trashed: bool_of(o, "isTrashed"),
        labels,
        attachments,
        annotations,
        collaborators,
        created_at,
        updated_at,
        warnings,
    })
}

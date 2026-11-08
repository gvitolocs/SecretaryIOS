//! Voice Learn vocabulary (docs/lucinavoice-protocol.md section 8).
//!
//! One shared word list for every speech-to-text path. This crate is pure (serde
//! and std only, no async, no network):
//!
//! - [`Vocab`]: the entries and their JSON file ([`Vocab::load`], atomic
//!   [`Vocab::save`], [`default_path`]), validation ([`validate_entry`]);
//! - [`Vocab::prompt`]: the Whisper initial prompt (words, newest first, bounded);
//! - [`Vocab::correct`]: whole-word / multi-word-span replacement of close
//!   mishearings of an entry by its canonical word, conservative, with
//!   borderline cases reported in [`Corrected::ambiguous`] instead of replaced;
//! - [`Vocab::meanings_for`]: the `meaning` of every entry the text mentions;
//! - [`Watched`]: a shared handle that re-reads the file when it changes.
mod phonetic;
mod stop;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub use phonetic::{distance, key as phonetic_key, ratio};

/// At most this many entries.
pub const MAX_ENTRIES: usize = 500;
/// At most this many characters in any text field.
pub const MAX_FIELD_CHARS: usize = 100;
/// At most this many `soundsLike` phrases per entry.
pub const MAX_SOUNDS_LIKE: usize = 20;
/// Languages an entry may name.
pub const LANGS: [&str; 3] = ["it", "en", "da"];
/// Values of [`Entry::source`].
pub const SOURCES: [&str; 2] = ["app", "correction"];

fn default_source() -> String {
    "app".into()
}

/// One taught word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    /// The canonical spelling.
    pub word: String,
    /// Phrases it is known to be misheard as (also matched exactly).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sounds_like: Vec<String>,
    /// What it is, given to Lucina as context ("the owner's home street").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// `it`, `en` or `da`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    /// `app` or `correction`.
    #[serde(default = "default_source")]
    pub source: String,
}

impl Entry {
    /// A bare entry (no id, no timestamps, source `app`).
    pub fn new(word: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            word: word.into(),
            sounds_like: Vec::new(),
            meaning: None,
            lang: None,
            created_at: String::new(),
            updated_at: String::new(),
            source: default_source(),
        }
    }

    /// Every spelling that should map to this entry: the word, then `soundsLike`.
    fn aliases(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.word.as_str()).chain(self.sounds_like.iter().map(String::as_str))
    }
}

fn check_text(name: &str, v: &str) -> Result<(), String> {
    if v != v.trim() {
        return Err(format!("{name}: leading or trailing whitespace"));
    }
    if v.chars().count() > MAX_FIELD_CHARS {
        return Err(format!("{name}: more than {MAX_FIELD_CHARS} characters"));
    }
    if v.chars().any(char::is_control) {
        return Err(format!("{name}: control character"));
    }
    Ok(())
}

/// Check one entry on its own: trimmed fields of at most 100 characters, a
/// non-empty `word`, non-empty `soundsLike` phrases (at most 20), `lang` in
/// `it|en|da`, `source` in `app|correction`. Uniqueness and the 500-entry cap
/// are checked by [`Vocab::validate`], [`Vocab::add`] and [`Vocab::replace`].
pub fn validate_entry(e: &Entry) -> Result<(), String> {
    check_text("id", &e.id)?;
    check_text("word", &e.word)?;
    if e.word.is_empty() {
        return Err("word: empty".into());
    }
    if e.sounds_like.len() > MAX_SOUNDS_LIKE {
        return Err(format!("soundsLike: more than {MAX_SOUNDS_LIKE} phrases"));
    }
    for s in &e.sounds_like {
        check_text("soundsLike", s)?;
        if s.is_empty() {
            return Err("soundsLike: empty phrase".into());
        }
    }
    if let Some(m) = &e.meaning {
        check_text("meaning", m)?;
    }
    if let Some(l) = &e.lang {
        if !LANGS.contains(&l.as_str()) {
            return Err("lang: it, en or da".into());
        }
    }
    if !SOURCES.contains(&e.source.as_str()) {
        return Err("source: app or correction".into());
    }
    check_text("createdAt", &e.created_at)?;
    check_text("updatedAt", &e.updated_at)?;
    Ok(())
}

fn word_key(w: &str) -> String {
    w.to_lowercase()
}

/// The vocabulary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vocab {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// `LUCINA_VOICE_LEARN_FILE`, else `$HOME/Projects/Lucina/data/voice-learn.json`.
pub fn default_path() -> PathBuf {
    path_from_lookup(|k| std::env::var(k).ok())
}

/// [`default_path`] with the variables read through `get` (a config lookup).
pub fn path_from_lookup(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(p) = get("LUCINA_VOICE_LEARN_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    match get("HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h).join("Projects/Lucina/data/voice-learn.json"),
        None => PathBuf::from("voice-learn.json"),
    }
}

impl Vocab {
    /// Read the file. A missing file is an empty vocabulary; unreadable JSON is
    /// `InvalidData`. Entries are not validated here (a hand-edited file still loads).
    pub fn load(path: &Path) -> io::Result<Vocab> {
        match std::fs::read(path) {
            Ok(b) if b.iter().all(u8::is_ascii_whitespace) => Ok(Vocab::default()),
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vocab::default()),
            Err(e) => Err(e),
        }
    }

    /// Validate, then write atomically (temp file in the same directory, mode
    /// 0600, fsync, rename). Missing parent directories are created.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
        if let Some(d) = dir {
            std::fs::create_dir_all(d)?;
        }
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?
            .to_string_lossy();
        let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let write = || -> io::Result<()> {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(&body)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        let r = write();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else if let Some(d) = dir {
            if let Ok(d) = std::fs::File::open(d) {
                let _ = d.sync_all();
            }
        }
        r
    }

    /// Every entry valid, at most [`MAX_ENTRIES`], ids unique (when set), words
    /// unique case-insensitively.
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.len() > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
        let mut words = HashMap::new();
        let mut ids = HashMap::new();
        for e in &self.entries {
            validate_entry(e)?;
            if words.insert(word_key(&e.word), ()).is_some() {
                return Err(format!("duplicate word: {}", e.word));
            }
            if !e.id.is_empty() && ids.insert(e.id.as_str(), ()).is_some() {
                return Err(format!("duplicate id: {}", e.id));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Append an entry after validating it against the cap and the existing words and ids.
    pub fn add(&mut self, e: Entry) -> Result<(), String> {
        validate_entry(&e)?;
        if self.entries.len() >= MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
        self.check_unique(&e, None)?;
        self.entries.push(e);
        Ok(())
    }

    /// Replace the entry with id `id` by `e` (same uniqueness rules, ignoring itself).
    pub fn replace(&mut self, id: &str, e: Entry) -> Result<(), String> {
        validate_entry(&e)?;
        let at = self
            .entries
            .iter()
            .position(|x| x.id == id)
            .ok_or_else(|| "no such entry".to_string())?;
        self.check_unique(&e, Some(at))?;
        self.entries[at] = e;
        Ok(())
    }

    /// Remove the entry with id `id`; false if there was none.
    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != n
    }

    fn check_unique(&self, e: &Entry, skip: Option<usize>) -> Result<(), String> {
        let wk = word_key(&e.word);
        for (i, o) in self.entries.iter().enumerate() {
            if Some(i) == skip {
                continue;
            }
            if word_key(&o.word) == wk {
                return Err(format!("duplicate word: {}", e.word));
            }
            if !e.id.is_empty() && o.id == e.id {
                return Err(format!("duplicate id: {}", e.id));
            }
        }
        Ok(())
    }

    /// The Whisper initial prompt: the entries' words, most recently updated
    /// first (ties: later in the file first), comma separated, at most
    /// `max_chars` characters (an entry that does not fit is skipped, shorter
    /// ones after it may still fit). About 600 characters is 200 tokens.
    ///
    /// `soundsLike` is deliberately not listed: a prompt only steers spelling,
    /// and a mishearing in it would make Whisper write the mishearing. They are
    /// used by [`Vocab::correct`].
    pub fn prompt(&self, max_chars: usize) -> String {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        order.sort_by(|&a, &b| {
            self.entries[b]
                .updated_at
                .cmp(&self.entries[a].updated_at)
                .then(b.cmp(&a))
        });
        let mut out = String::new();
        let mut len = 0usize;
        for i in order {
            let w = self.entries[i].word.as_str();
            let n = w.chars().count();
            let add = if out.is_empty() { n } else { n + 2 };
            if len + add > max_chars {
                continue;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(w);
            len += add;
        }
        out
    }

    /// Replace close mishearings in `text` by the canonical word; see [`Corrected`].
    pub fn correct(&self, text: &str) -> Corrected {
        correct::run(self, text).0
    }

    /// [`Vocab::correct`] plus where each replacement happened, as byte ranges of
    /// the input `text` in increasing order (to map a correction back onto word
    /// timings).
    pub fn correct_with_spans(&self, text: &str) -> (Corrected, Vec<Replacement>) {
        correct::run(self, text)
    }

    /// `(word, meaning)` of every entry that has a meaning and whose word occurs
    /// in `text` (case and diacritics ignored, whole words). Call it on the
    /// corrected text.
    pub fn meanings_for(&self, text: &str) -> Vec<(String, String)> {
        let hay = phonetic::fold_spaced(text);
        self.entries
            .iter()
            .filter_map(|e| {
                let m = e.meaning.as_deref().filter(|m| !m.is_empty())?;
                let needle = phonetic::fold_spaced(&e.word);
                (!needle.is_empty() && contains_words(&hay, &needle))
                    .then(|| (e.word.clone(), m.to_owned()))
            })
            .collect()
    }
}

/// Whole-word containment on `fold_spaced` strings (single spaces between words).
fn contains_words(hay: &str, needle: &str) -> bool {
    let hay = format!(" {hay} ");
    let needle = format!(" {needle} ");
    hay.contains(&needle)
}

/// The result of [`Vocab::correct`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Corrected {
    /// The text with the corrections applied (the input if none).
    pub text: String,
    /// `(heard, canonical word)` for every replacement made, in order.
    pub corrections: Vec<(String, String)>,
    /// Spans that are close to more than one entry, or only borderline close to
    /// one: left unchanged. The caller may ask Jev "which did they mean".
    pub ambiguous: Vec<Ambiguity>,
}

/// One replacement made by [`Vocab::correct_with_spans`]: `text[start..end]` of the
/// input became `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub start: usize,
    pub end: usize,
    pub to: String,
}

/// A span of the text that might be one of several entries' words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ambiguity {
    /// The span as it appears in the text.
    pub heard: String,
    /// The canonical words it might be, closest first.
    pub candidates: Vec<String>,
}

mod correct;

/// A vocabulary file that is re-read when it changes (modification time or size).
/// A missing file is an empty vocabulary; a file that cannot be parsed keeps the
/// last good one.
#[derive(Debug)]
pub struct Watched {
    path: PathBuf,
    cache: Mutex<Cache>,
}

#[derive(Debug)]
struct Cache {
    stamp: Option<(SystemTime, u64)>,
    vocab: Arc<Vocab>,
}

impl Watched {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(Cache {
                stamp: None,
                vocab: Arc::new(Vocab::default()),
            }),
        }
    }

    /// Watch [`default_path`].
    pub fn from_default() -> Self {
        Self::new(default_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current vocabulary (one `stat` per call, a re-read when it changed).
    pub fn get(&self) -> Arc<Vocab> {
        let stamp = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if stamp == c.stamp {
            return c.vocab.clone();
        }
        match (stamp, Vocab::load(&self.path)) {
            (None, _) => c.vocab = Arc::new(Vocab::default()),
            (Some(_), Ok(v)) => c.vocab = Arc::new(v),
            (Some(_), Err(_)) => return c.vocab.clone(),
        }
        c.stamp = stamp;
        c.vocab.clone()
    }
}

#[cfg(test)]
mod tests;

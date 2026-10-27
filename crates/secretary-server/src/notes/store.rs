//! The note store: every note, live and in the trash, cached in memory and
//! backed by files in `$SECRETARY_DATA_DIR/notes/`.
//!
//! Layout (all files 0600, directories 0700):
//!
//! ```text
//! notes/<id>.json           live notes (archived ones too)
//! notes/trash/<id>.json     notes in the trash
//! notes/state.json          {rev, floor, tombstones}: the sync state
//! notes/labels.json         the label registry
//! notes/import-deleted.json import keys of notes deleted forever
//! notes/attachments/<sha256>.<ext>
//! notes/.lock               flock: one writer at a time across processes
//! ```
//!
//! The cache is reloaded when the mtime of `notes/` or `notes/trash/`
//! changes (a CLI import, a restore by hand). Writes go through [`NoteStore::with`],
//! which holds the lock file and refreshes the stamp afterwards, so a write
//! of our own never triggers a reload.
//!
//! Revisions: every write to a note takes the next store revision and
//! stamps `note.rev`; deleting forever leaves a tombstone with its own
//! revision. The counter is recovered at load as the highest revision in any
//! note or tombstone (and `state.json`), so it never runs backwards.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::model::*;
use crate::devices::{lock_file, save_json_atomic};
use crate::error::ApiError;

const STATE_FILE: &str = "state.json";
const LABELS_FILE: &str = "labels.json";
const IMPORT_DELETED_FILE: &str = "import-deleted.json";
const LOCK_FILE: &str = ".lock";
/// How often the trash and the tombstones are looked at for expiry.
const PURGE_EVERY: Duration = Duration::from_secs(300);

/// Who is writing (for `source` defaults and the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller(pub &'static str);

impl Caller {
    /// The default `source` for notes this caller creates.
    pub(super) fn default_source(self) -> &'static str {
        match self.0 {
            "lucina" => "lucina",
            _ => "owner",
        }
    }
}

/// A note deleted forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Tombstone {
    pub id: String,
    pub rev: u64,
    pub at: String,
}

/// `notes/state.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct State {
    /// The last revision handed out.
    pub rev: u64,
    /// The highest revision of a tombstone that was pruned: a `since` below
    /// it cannot be answered any more (`410 cursor_expired`).
    pub floor: u64,
    pub tombstones: Vec<Tombstone>,
}

/// One entry of `notes/labels.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LabelEntry {
    pub tag: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Default)]
pub(super) struct Index {
    /// Live notes (archived ones included).
    pub notes: BTreeMap<String, Note>,
    pub trash: BTreeMap<String, Note>,
    pub state: State,
    pub labels: Vec<LabelEntry>,
    pub deleted_keys: BTreeSet<String>,
    state_dirty: bool,
    labels_dirty: bool,
    deleted_dirty: bool,
}

type Stamp = (Option<SystemTime>, Option<SystemTime>);

struct Cache {
    stamp: Option<Stamp>,
    idx: Index,
    purged: Option<Instant>,
}

/// The notes in the data dir, cached in memory.
pub struct NoteStore {
    dir: PathBuf,
    cache: Mutex<Cache>,
}

pub(super) fn io_err(e: io::Error) -> ApiError {
    tracing::error!(error = %e, "notes write failed");
    ApiError::internal("could not save the note")
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn read_json<T: DeserializeOwned>(p: &Path) -> Option<T> {
    let bytes = std::fs::read(p).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(path = %p.display(), error = %e, "notes file unreadable; ignored");
            None
        }
    }
}

fn remove(p: &Path) {
    if let Err(e) = std::fs::remove_file(p) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(path = %p.display(), error = %e, "could not remove a notes file");
        }
    }
}

/// Position 0 / NaN means "not set": the creation time in epoch ms.
fn fix_note(n: &mut Note) {
    if !n.position.is_finite() || n.position == 0.0 {
        n.position = epoch_ms(&n.created_at).unwrap_or_else(now_ms);
    }
}

/// Like `save_json_atomic` without the fsyncs: for the one-time revision
/// stamp of notes written before revisions existed (thousands of files in one
/// go; the stamp is derived again at the next load if a crash loses it).
fn save_json_unsynced<T: Serialize + ?Sized>(dir: &Path, name: &str, value: &T) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    f.write_all(&json)?;
    drop(f);
    std::fs::rename(&tmp, dir.join(name))
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
            Ok(mut n) if n.id == id => {
                fix_note(&mut n);
                out.insert(n.id.clone(), n);
            }
            Ok(_) => tracing::warn!(path = %p.display(), "note file id mismatch; skipped"),
            Err(e) => tracing::warn!(path = %p.display(), error = %e, "note unreadable; skipped"),
        }
    }
    out
}

// ---------------------------------------------------------------- index

impl Index {
    pub fn bump(&mut self) -> u64 {
        self.state.rev += 1;
        self.state.rev
    }

    pub fn find_live(&self, key: &str) -> Option<&Note> {
        let key = key.trim();
        if valid_id(key) {
            return self.notes.get(key);
        }
        let k = key.to_lowercase();
        self.notes.values().find(|n| n.slug == k)
    }

    pub fn slug_taken(&self, slug: &str, except: Option<&str>) -> bool {
        self.notes
            .values()
            .any(|n| n.slug == slug && Some(n.id.as_str()) != except)
    }

    pub fn free_slug(&self, base: &str, except: Option<&str>) -> String {
        if !self.slug_taken(base, except) {
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
            if !self.slug_taken(&s, except) {
                return s;
            }
        }
        unreachable!()
    }

    /// Tag → display name from the registry.
    pub fn label_names(&self) -> LabelNames {
        self.labels
            .iter()
            .map(|l| (l.tag.clone(), l.name.clone()))
            .collect()
    }

    /// Add the labels the registry does not know yet. True when it changed.
    pub fn register(&mut self, labels: &[Label]) -> bool {
        let mut changed = false;
        for l in labels {
            if !self.labels.iter().any(|e| e.tag == l.tag) {
                self.labels.push(LabelEntry {
                    tag: l.tag.clone(),
                    name: l.name.clone(),
                    created_at: rfc3339(now()),
                });
                changed = true;
            }
        }
        self.labels_dirty |= changed;
        changed
    }

    pub fn mark_labels_dirty(&mut self) {
        self.labels_dirty = true;
    }

    /// Does any note (live or in the trash) refer to this attachment file?
    pub fn references(&self, file: &str) -> bool {
        self.notes
            .values()
            .chain(self.trash.values())
            .any(|n| n.attachments.iter().any(|a| a.file == file))
    }
}

// ---------------------------------------------------------------- store

impl NoteStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(NOTES_DIR),
            cache: Mutex::new(Cache {
                stamp: None,
                idx: Index::default(),
                purged: None,
            }),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn trash_dir(&self) -> PathBuf {
        self.dir.join(TRASH_DIR)
    }

    pub(super) fn attachments_dir(&self) -> PathBuf {
        self.dir.join(ATTACHMENTS_DIR)
    }

    fn stamp(&self) -> Stamp {
        (mtime(&self.dir), mtime(&self.trash_dir()))
    }

    /// Run `f` on the up-to-date index, one caller at a time (in this
    /// process through the mutex, across processes through the lock file).
    pub(super) fn with<R>(&self, f: impl FnOnce(&Self, &mut Index) -> R) -> R {
        let mut guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let c = &mut *guard;
        let _flock = if self.dir.is_dir() {
            lock_file(&self.dir, LOCK_FILE).ok()
        } else {
            None
        };
        let stamp = self.stamp();
        if stamp.0.is_none() || c.stamp != Some(stamp) {
            c.idx = self.load();
            c.purged = None;
        }
        if c.purged.is_none_or(|t| t.elapsed() > PURGE_EVERY) {
            self.purge(&mut c.idx);
            c.purged = Some(Instant::now());
        }
        let r = f(self, &mut c.idx);
        if let Err(e) = self.flush(&mut c.idx) {
            tracing::error!(error = %e.message, "notes state could not be saved");
        }
        c.stamp = Some(self.stamp());
        r
    }

    /// Read everything from disk.
    fn load(&self) -> Index {
        let mut notes = read_dir_notes(&self.dir);
        let trash = read_dir_notes(&self.trash_dir());
        // A crash between "written to the trash" and "removed from the live
        // dir" leaves both: the trash copy is the later one.
        for id in trash.keys() {
            if notes.remove(id).is_some() {
                remove(&self.dir.join(format!("{id}.json")));
            }
        }
        let state: State = read_json(&self.dir.join(STATE_FILE)).unwrap_or_default();
        let labels: Vec<LabelEntry> = read_json(&self.dir.join(LABELS_FILE)).unwrap_or_default();
        let deleted_keys: BTreeSet<String> =
            read_json::<Vec<String>>(&self.dir.join(IMPORT_DELETED_FILE))
                .unwrap_or_default()
                .into_iter()
                .collect();
        let mut idx = Index {
            notes,
            trash,
            state,
            labels,
            deleted_keys,
            ..Index::default()
        };
        // The counter is at least as high as anything written.
        let max = idx
            .notes
            .values()
            .chain(idx.trash.values())
            .map(|n| n.rev)
            .chain(idx.state.tombstones.iter().map(|t| t.rev))
            .max()
            .unwrap_or(0);
        idx.state.rev = idx.state.rev.max(max);
        // Notes written before revisions existed get one now, oldest first.
        let mut legacy: Vec<(String, String, bool)> = idx
            .notes
            .values()
            .map(|n| (n, false))
            .chain(idx.trash.values().map(|n| (n, true)))
            .filter(|(n, _)| n.rev == 0)
            .map(|(n, t)| (n.updated_at.clone(), n.id.clone(), t))
            .collect();
        legacy.sort();
        for (_, id, trashed) in legacy {
            let rev = idx.bump();
            let (map, dir) = if trashed {
                (&mut idx.trash, self.trash_dir())
            } else {
                (&mut idx.notes, self.dir.clone())
            };
            if let Some(n) = map.get_mut(&id) {
                n.rev = rev;
                if let Err(e) = save_json_unsynced(&dir, &format!("{id}.json"), n) {
                    tracing::warn!(id = %id, error = %e, "could not stamp a note with its revision");
                }
            }
            idx.state_dirty = true;
        }
        idx
    }

    /// Write the state, labels and import-deleted files when they changed.
    fn flush(&self, idx: &mut Index) -> Result<(), ApiError> {
        if idx.state_dirty {
            save_json_atomic(&self.dir, STATE_FILE, &idx.state).map_err(io_err)?;
            idx.state_dirty = false;
        }
        if idx.labels_dirty {
            save_json_atomic(&self.dir, LABELS_FILE, &idx.labels).map_err(io_err)?;
            idx.labels_dirty = false;
        }
        if idx.deleted_dirty {
            let keys: Vec<&String> = idx.deleted_keys.iter().collect();
            save_json_atomic(&self.dir, IMPORT_DELETED_FILE, &keys).map_err(io_err)?;
            idx.deleted_dirty = false;
        }
        Ok(())
    }

    /// Delete forever what has been in the trash more than [`TRASH_DAYS`]
    /// days, and forget tombstones older than [`TOMBSTONE_DAYS`].
    fn purge(&self, idx: &mut Index) {
        let cutoff = now() - time::Duration::days(TRASH_DAYS);
        let expired: Vec<String> = idx
            .trash
            .values()
            .filter(|n| {
                n.deleted_at
                    .as_deref()
                    .and_then(parse_time)
                    .is_none_or(|t| t < cutoff)
            })
            .map(|n| n.id.clone())
            .collect();
        for id in expired {
            if let Some(n) = self.forget(idx, &id) {
                tracing::info!(id = %n.id, slug = %n.slug, "note purged from the trash");
            }
        }
        let horizon = now() - time::Duration::days(TOMBSTONE_DAYS);
        let before = idx.state.tombstones.len();
        let mut floor = idx.state.floor;
        idx.state.tombstones.retain(|t| {
            let keep = parse_time(&t.at).is_none_or(|at| at >= horizon);
            if !keep {
                floor = floor.max(t.rev);
            }
            keep
        });
        if idx.state.tombstones.len() != before {
            idx.state.floor = floor;
            idx.state_dirty = true;
        }
    }

    /// Write a note to its file (live or trash), stamping the next revision,
    /// and update the index. Returns the stored note.
    pub(super) fn put(&self, idx: &mut Index, mut note: Note) -> Result<Note, ApiError> {
        note.rev = idx.bump();
        let name = format!("{}.json", note.id);
        if note.deleted_at.is_some() {
            save_json_atomic(&self.trash_dir(), &name, &note).map_err(io_err)?;
            remove(&self.dir.join(&name));
            idx.notes.remove(&note.id);
            idx.trash.insert(note.id.clone(), note.clone());
        } else {
            save_json_atomic(&self.dir, &name, &note).map_err(io_err)?;
            remove(&self.trash_dir().join(&name));
            idx.trash.remove(&note.id);
            idx.notes.insert(note.id.clone(), note.clone());
        }
        Ok(note)
    }

    /// Delete a note forever: files, index, a tombstone, the import key and
    /// the attachment files nobody else uses. The caller flushes (done by `with`).
    pub(super) fn forget(&self, idx: &mut Index, id: &str) -> Option<Note> {
        let note = idx.trash.remove(id).or_else(|| idx.notes.remove(id))?;
        let name = format!("{id}.json");
        remove(&self.trash_dir().join(&name));
        remove(&self.dir.join(&name));
        let rev = idx.bump();
        idx.state.tombstones.push(Tombstone {
            id: id.to_owned(),
            rev,
            at: rfc3339(now()),
        });
        idx.state_dirty = true;
        if let Some(imp) = &note.import {
            if idx.deleted_keys.insert(imp.key.clone()) {
                idx.deleted_dirty = true;
            }
        }
        let files: Vec<String> = note.attachments.iter().map(|a| a.file.clone()).collect();
        self.gc(idx, &files);
        Some(note)
    }

    // ------------------------------------------------------------ reads

    /// Every live note matching `q` / `tag`, pinned first, newest first.
    pub fn list(&self, q: Option<&str>, tag: Option<&str>, limit: usize) -> Vec<NoteSummary> {
        self.list_filtered(&Filter::new(q, tag), limit)
    }

    pub fn list_filtered(&self, f: &Filter, limit: usize) -> Vec<NoteSummary> {
        self.with(|_, idx| {
            let names = idx.label_names();
            let mut v: Vec<&Note> = idx.notes.values().filter(|n| n.matches(f)).collect();
            v.sort_by(|a, b| {
                b.pinned
                    .cmp(&a.pinned)
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
            });
            v.into_iter()
                .take(limit)
                .map(|n| n.summary(f.q.as_deref(), &names))
                .collect()
        })
    }

    pub fn get(&self, key: &str) -> Result<Note, ApiError> {
        self.with(|_, idx| idx.find_live(key).cloned().ok_or_else(|| not_found(key)))
    }

    /// The Lucina reminder ids of these live notes (id or slug each).
    pub fn lucina_ids(&self, keys: &[String]) -> Vec<String> {
        self.with(|_, idx| {
            keys.iter()
                .filter_map(|k| idx.find_live(k))
                .filter_map(|n| n.reminder.as_ref()?.lucina_id.clone())
                .collect()
        })
    }

    /// The store revision (the cursor of an empty `changes` answer).
    pub fn rev(&self) -> u64 {
        self.with(|_, idx| idx.state.rev)
    }

    // ------------------------------------------------------------ create

    pub fn create(&self, req: CreateNote, caller: Caller) -> Result<Note, ApiError> {
        self.create_inner(req, caller, false)
    }

    /// Everything [`Self::create`] checks, and the note it would store, but
    /// nothing written.
    pub fn create_dry(&self, req: CreateNote, caller: Caller) -> Result<Note, ApiError> {
        self.create_inner(req, caller, true)
    }

    fn create_inner(&self, req: CreateNote, caller: Caller, dry: bool) -> Result<Note, ApiError> {
        let title = check_title(&req.title)?;
        let items = req.items.as_deref().map(check_items).transpose()?;
        let content = if items.is_some() {
            String::new()
        } else {
            check_content("content", &req.content)?
        };
        let labels = merge_label_fields(Some(&req.tags), Some(&req.labels))?.unwrap_or_default();
        let source = check_source(req.source.as_deref(), caller.default_source())?;
        let explicit = req.slug.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let explicit = explicit.map(check_slug).transpose()?;
        let color = match req.color.as_deref() {
            Some(c) => check_color(c)?,
            None => "default".to_owned(),
        };
        let annotations = check_annotations(req.annotations.as_deref().unwrap_or_default())?;
        let collaborators = check_collaborators(req.collaborators.as_deref().unwrap_or_default())?;
        let position = req.position.map(check_position).transpose()?;
        let id = new_id().map_err(io_err)?;
        self.with(|st, idx| {
            if idx.notes.len() >= MAX_NOTES {
                return Err(notes_full());
            }
            let attachments = match req.attachments.as_deref() {
                Some(r) => st.resolve_refs(r)?,
                None => Vec::new(),
            };
            let slug = match explicit {
                Some(s) => {
                    if idx.slug_taken(&s, None) {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "slug_taken",
                            format!("a note named {s:?} exists; update it or append to it"),
                        ));
                    }
                    s
                }
                None => idx.free_slug(&slug_base(&title, &content, None), None),
            };
            let t = rfc3339(now());
            let mut note = Note {
                id,
                slug,
                title,
                content,
                tags: labels.iter().map(|l| l.tag.clone()).collect(),
                source,
                pinned: req.pinned,
                created_at: t.clone(),
                updated_at: t,
                deleted_at: None,
                color,
                archived: req.archived,
                items: None,
                attachments,
                annotations,
                collaborators,
                reminder: req
                    .reminder
                    .as_ref()
                    .map(|r| check_reminder(r, req.lucina_id.clone()))
                    .transpose()?,
                position: position.unwrap_or_else(now_ms),
                rev: 0,
                import: None,
            };
            if let Some(items) = items {
                if req.slug.is_none() {
                    // The slug of an untitled list note comes from its items.
                    note.slug = idx.free_slug(&slug_base(&note.title, "", Some(&items)), None);
                }
                set_items(&mut note, items)?;
            }
            check_not_blank(&note)?;
            if dry {
                return Ok(note);
            }
            let note = st.put(idx, note)?;
            idx.register(&labels);
            Ok(note)
        })
    }

    // ------------------------------------------------------------ update

    pub fn update(&self, key: &str, req: UpdateNote) -> Result<Note, ApiError> {
        self.update_inner(key, req, false)
    }

    /// Everything [`Self::update`] checks, and the note it would store, but
    /// nothing written.
    pub fn update_dry(&self, key: &str, req: UpdateNote) -> Result<Note, ApiError> {
        self.update_inner(key, req, true)
    }

    fn update_inner(&self, key: &str, req: UpdateNote, dry: bool) -> Result<Note, ApiError> {
        let title = req.title.as_deref().map(check_title).transpose()?;
        let items = match &req.items {
            Some(None) => {
                return Err(bad_request(
                    "items: null is not supported; convert with {\"convert\": \"text\"}",
                ))
            }
            Some(Some(v)) => Some(check_items(v)?),
            None => None,
        };
        if req.convert.is_some() && (items.is_some() || req.content.is_some()) {
            return Err(bad_request(
                "convert: cannot be combined with items or content",
            ));
        }
        let content = req
            .content
            .as_deref()
            .map(|c| check_content("content", c))
            .transpose()?;
        let labels = merge_label_fields(req.tags.as_deref(), req.labels.as_deref())?;
        let slug = req.slug.as_deref().map(check_slug).transpose()?;
        let color = req.color.as_deref().map(check_color).transpose()?;
        let annotations = req
            .annotations
            .as_deref()
            .map(check_annotations)
            .transpose()?;
        let collaborators = req
            .collaborators
            .as_deref()
            .map(check_collaborators)
            .transpose()?;
        let position = req.position.map(check_position).transpose()?;
        self.with(|st, idx| {
            let mut note = idx.find_live(key).cloned().ok_or_else(|| not_found(key))?;
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
                if idx.slug_taken(&s, Some(&note.id)) {
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
            let was_list = note.items.is_some();
            match (items, content, req.convert) {
                (Some(items), _, _) => set_items(&mut note, items)?,
                (None, Some(c), _) if was_list => {
                    let old = note.items.take().unwrap_or_default();
                    set_items(&mut note, parse_items(&c, &old))?;
                }
                (None, Some(c), _) => note.content = c,
                (None, None, Some(Convert::Text)) => {
                    if let Some(items) = note.items.take() {
                        note.content = items_to_text(&items);
                    }
                }
                (None, None, Some(Convert::List)) => {
                    if !was_list {
                        let items = parse_items(&note.content, &[]);
                        set_items(&mut note, items)?;
                    }
                }
                (None, None, None) => {}
            }
            if let Some(l) = &labels {
                note.tags = l.iter().map(|l| l.tag.clone()).collect();
            }
            if let Some(p) = req.pinned {
                note.pinned = p;
            }
            if let Some(c) = color {
                note.color = c;
            }
            if let Some(a) = req.archived {
                note.archived = a;
            }
            if let Some(a) = annotations {
                note.annotations = a;
            }
            if let Some(c) = collaborators {
                note.collaborators = c;
            }
            if let Some(p) = position {
                note.position = p;
            }
            let mut dropped: Vec<String> = Vec::new();
            if let Some(refs) = req.attachments.as_deref() {
                let new = st.resolve_refs(refs)?;
                dropped = note
                    .attachments
                    .iter()
                    .filter(|a| !new.iter().any(|b| b.file == a.file))
                    .map(|a| a.file.clone())
                    .collect();
                note.attachments = new;
            }
            match &req.reminder {
                None => {}
                Some(None) => note.reminder = None,
                Some(Some(r)) => {
                    // The same moment keeps the Lucina reminder it has.
                    let kept = note
                        .reminder
                        .as_ref()
                        .filter(|old| same_instant(&old.at, &r.at))
                        .and_then(|old| old.lucina_id.clone());
                    note.reminder = Some(check_reminder(r, req.lucina_id.clone().or(kept))?);
                }
            }
            check_not_blank(&note)?;
            note.updated_at = rfc3339(now());
            if dry {
                return Ok(note);
            }
            let note = st.put(idx, note)?;
            if let Some(l) = &labels {
                idx.register(l);
            }
            st.gc(idx, &dropped);
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
        self.with(|st, idx| {
            let mut note = idx.find_live(key).cloned().ok_or_else(|| not_found(key))?;
            if let Some(mut items) = note.items.take() {
                // A list note: the appended lines become items at the end.
                let new = parse_items(&add, &[]);
                if new.is_empty() {
                    return Err(bad_request("content: must not be empty"));
                }
                items.extend(new);
                if items.len() > MAX_ITEMS {
                    return Err(bad_request(format!("items: at most {MAX_ITEMS}")));
                }
                set_items(&mut note, items)?;
            } else {
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
            }
            note.updated_at = rfc3339(now());
            st.put(idx, note)
        })
    }

    // ------------------------------------------------------------ trash

    /// Move a live note to the trash (its Lucina reminder id is dropped: the
    /// route cancels the reminder). Returns it (with `deletedAt`).
    fn trash_note(&self, idx: &mut Index, mut note: Note) -> Result<Note, ApiError> {
        note.deleted_at = Some(rfc3339(now()));
        if let Some(r) = note.reminder.as_mut() {
            r.lucina_id = None;
        }
        self.put(idx, note)
    }

    /// Move a note to the trash. Returns it (with `deletedAt`).
    pub fn delete(&self, key: &str) -> Result<Note, ApiError> {
        self.with(|st, idx| {
            let note = idx.find_live(key).cloned().ok_or_else(|| not_found(key))?;
            st.trash_note(idx, note)
        })
    }

    /// Notes in the trash, newest deletion first.
    pub fn trash(&self) -> Vec<NoteSummary> {
        self.with(|_, idx| {
            let names = idx.label_names();
            let mut v: Vec<&Note> = idx.trash.values().collect();
            v.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
            v.into_iter().map(|n| n.summary(None, &names)).collect()
        })
    }

    fn restore_note(&self, idx: &mut Index, id: &str) -> Result<Note, ApiError> {
        let mut note = idx.trash.get(id).cloned().ok_or_else(|| not_found(id))?;
        if idx.notes.len() >= MAX_NOTES {
            return Err(notes_full());
        }
        note.deleted_at = None;
        note.slug = idx.free_slug(&note.slug, None);
        note.updated_at = rfc3339(now());
        self.put(idx, note)
    }

    /// Bring a note back from the trash. Its slug gets a `-2` suffix when a
    /// live note took it meanwhile.
    pub fn restore(&self, id: &str) -> Result<Note, ApiError> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        self.with(|st, idx| st.restore_note(idx, id))
    }

    /// Delete a trashed note forever.
    pub fn delete_forever(&self, id: &str) -> Result<(), ApiError> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        self.with(|st, idx| {
            if !idx.trash.contains_key(id) {
                return Err(not_found(id));
            }
            st.forget(idx, id);
            Ok(())
        })
    }

    /// Delete everything in the trash forever. Returns how many.
    pub fn empty_trash(&self) -> usize {
        self.with(|st, idx| {
            let ids: Vec<String> = idx.trash.keys().cloned().collect();
            for id in &ids {
                st.forget(idx, id);
            }
            ids.len()
        })
    }

    // ------------------------------------------------------------ batch

    pub fn batch(&self, req: &BatchReq) -> Result<BatchOutcome, ApiError> {
        if req.ids.len() > MAX_BATCH_IDS {
            return Err(bad_request(format!("ids: at most {MAX_BATCH_IDS}")));
        }
        let structural = [req.trash, req.restore, req.delete_forever]
            .iter()
            .filter(|b| **b)
            .count();
        let edits =
            req.set.is_some() || !req.add_labels.is_empty() || !req.remove_labels.is_empty();
        if structural > 1 || (structural == 1 && edits) {
            return Err(bad_request(
                "trash, restore and deleteForever are exclusive with each other and with set/labels",
            ));
        }
        let color = req
            .set
            .as_ref()
            .and_then(|s| s.color.as_deref())
            .map(check_color)
            .transpose()?;
        let add = parse_labels(&req.add_labels)?;
        let remove = parse_labels(&req.remove_labels)?;
        let mut ids: Vec<&str> = Vec::new();
        for id in &req.ids {
            let id = id.trim();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        self.with(|st, idx| {
            let mut out = BatchOutcome::default();
            if req.trash {
                for key in ids {
                    if let Some(n) = idx.find_live(key).cloned() {
                        out.notes.push(st.trash_note(idx, n)?);
                    }
                }
            } else if req.restore {
                for id in ids {
                    if valid_id(id) && idx.trash.contains_key(id) {
                        out.notes.push(st.restore_note(idx, id)?);
                    }
                }
            } else if req.delete_forever {
                for id in ids {
                    if valid_id(id) && idx.trash.contains_key(id) {
                        st.forget(idx, id);
                        out.removed.push(id.to_owned());
                    }
                }
            } else {
                // Work out every change first, then write.
                let mut changed: Vec<Note> = Vec::new();
                for key in ids {
                    let Some(old) = idx.find_live(key) else {
                        continue;
                    };
                    let mut n = old.clone();
                    if let Some(set) = &req.set {
                        if let Some(c) = &color {
                            n.color = c.clone();
                        }
                        if let Some(p) = set.pinned {
                            n.pinned = p;
                        }
                        if let Some(a) = set.archived {
                            n.archived = a;
                        }
                    }
                    for l in &add {
                        if !n.tags.contains(&l.tag) {
                            n.tags.push(l.tag.clone());
                        }
                    }
                    n.tags.retain(|t| !remove.iter().any(|l| &l.tag == t));
                    if n.tags.len() > MAX_TAGS {
                        return Err(bad_request(format!("tags: at most {MAX_TAGS} per note")));
                    }
                    if &n != old {
                        n.updated_at = rfc3339(now());
                        changed.push(n);
                    }
                }
                for n in changed {
                    out.notes.push(st.put(idx, n)?);
                }
                if !add.is_empty() {
                    idx.register(&add);
                }
            }
            Ok(out)
        })
    }

    // ------------------------------------------------------------ sync

    /// What changed after revision `since`, oldest first, at most `limit`
    /// entries (notes and removals together).
    pub fn changes(&self, since: u64, limit: usize) -> Result<Changes, ApiError> {
        self.with(|_, idx| {
            if since > 0 && (since < idx.state.floor || since > idx.state.rev) {
                return Err(ApiError::new(
                    StatusCode::GONE,
                    "cursor_expired",
                    "the cursor is too old (or not from this store); load the notes again from the start",
                ));
            }
            enum Ev<'a> {
                Note(&'a Note),
                Gone(&'a str),
            }
            let mut evs: Vec<(u64, Ev)> = idx
                .notes
                .values()
                .chain(idx.trash.values())
                .filter(|n| n.rev > since)
                .map(|n| (n.rev, Ev::Note(n)))
                .chain(
                    idx.state
                        .tombstones
                        .iter()
                        .filter(|t| t.rev > since)
                        .map(|t| (t.rev, Ev::Gone(&t.id))),
                )
                .collect();
            evs.sort_by_key(|e| e.0);
            let has_more = evs.len() > limit;
            evs.truncate(limit);
            let cursor = evs.last().map_or(idx.state.rev, |e| e.0);
            let mut out = Changes {
                notes: Vec::new(),
                removed: Vec::new(),
                cursor: cursor.to_string(),
                has_more,
            };
            for (_, e) in evs {
                match e {
                    Ev::Note(n) => out.notes.push(n.clone()),
                    Ev::Gone(id) => out.removed.push(id.to_owned()),
                }
            }
            Ok(out)
        })
    }
}

/// The answer of [`NoteStore::batch`].
#[derive(Debug, Default)]
pub struct BatchOutcome {
    /// The notes that changed (trashed ones with `deletedAt`).
    pub notes: Vec<Note>,
    /// Ids deleted forever.
    pub removed: Vec<String>,
}

/// The answer of [`NoteStore::changes`].
#[derive(Debug)]
pub struct Changes {
    pub notes: Vec<Note>,
    pub removed: Vec<String>,
    pub cursor: String,
    pub has_more: bool,
}

/// Do two RFC 3339 stamps name the same instant?
pub fn same_instant(a: &str, b: &str) -> bool {
    match (parse_time(a.trim()), parse_time(b.trim())) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

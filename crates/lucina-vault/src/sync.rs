//! Vault sync for devices without Syncthing (Juno, later the phones): a plain folder here kept in step with the owner's
//! canonical vault on nezopt (`/home/nez/mnt/mybook/Obsidian`) through Lucina's `/v1/vault` routes. The server side uses
//! the same [`Folder`] on the canonical folder, so both ends write the same way. Design: `docs/lucina-vault.md`, "Sync".
//!
//! One run: read both manifests, decide each file with [`crate::sync_rules::decide`] against the base (the revision both
//! sides had at the last sync, kept in a state file outside the vault), and carry out the acts. Every write on either side
//! is compare-and-swap against the revision the manifest showed, so a file that changed after the manifests were read is
//! left alone and decided again next run. A conflict keeps both texts: this side's goes to
//! `Note (conflict <date> <device>).md`, which the next round sends to the server like any new note.
//!
//! The vault lives on nezopt only (the owner keeps no copy on the Mac since 2026-11-08). The server writes the canonical
//! folder with atomic renames from `.lucina-vault-*.tmp` names, which a Syncthing folder there would have to ignore.
use crate::sync_rules::{classify, decide, Act, Facts, PathClass};
use crate::vault::revision_of;
use crate::write::{atomic_replace, no_symlink_below, sync_dir};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// Files above this size are neither sent nor fetched by default (`Options::max_size`).
pub const DEFAULT_MAX_SIZE: u64 = 64 * 1024 * 1024;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// One file as a manifest lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    /// sha256 of the bytes, lowercase hex.
    pub rev: String,
}

/// Every file of a folder that takes part, by path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub files: Vec<Entry>,
}

impl Manifest {
    fn by_path(&self) -> BTreeMap<&str, &Entry> {
        self.files.iter().map(|e| (e.path.as_str(), e)).collect()
    }
}

/// What a stat said about a file when its revision was computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub mtime_ns: i128,
    pub ino: u64,
    pub rev: String,
}

/// A vault folder, read and written by sync. Holds no index of notes, only a cache of revisions by stat.
pub struct Folder {
    root: PathBuf,
    cache: Mutex<BTreeMap<String, Seen>>,
}

fn class_of(rel: &str) -> PathClass {
    classify(rel.as_bytes())
}

fn mtime_ns(m: &fs::Metadata) -> i128 {
    m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128
}

impl Folder {
    /// The folder must exist: an unmounted disk shows an empty mount point, and must not look like an empty vault.
    pub fn open(root: impl AsRef<Path>) -> Result<Folder> {
        let root = root.as_ref().to_path_buf();
        let m = fs::metadata(&root)?;
        if !m.is_dir() {
            return Err(Error::Invalid(format!("not a folder: {}", root.display())));
        }
        Ok(Folder { root, cache: Mutex::new(BTreeMap::new()) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The revision cache, to keep between runs (`State::seen`).
    pub fn seen(&self) -> BTreeMap<String, Seen> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_seen(&self, seen: BTreeMap<String, Seen>) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = seen;
    }

    /// Every file that takes part (see [`classify`]), sorted by path. Symlinks are never followed or listed. A file whose
    /// size, modification time and inode match the cache keeps its cached revision; any other is hashed.
    pub fn manifest(&self) -> Result<Manifest> {
        let mut files = Vec::new();
        let mut stack = vec![(self.root.clone(), String::new())];
        let mut fresh = BTreeMap::new();
        let old = self.seen();
        while let Some((dir, prefix)) = stack.pop() {
            for de in fs::read_dir(&dir)? {
                let de = de?;
                let Some(name) = de.file_name().to_str().map(str::to_owned) else { continue };
                let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                let ft = de.file_type()?;
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    // A folder is walked when a file below it could take part: `.obsidian` itself, and no other dot name.
                    if (!name.starts_with('.') || rel == ".obsidian") && prefix != ".obsidian" && !rel.contains('\\') {
                        stack.push((de.path(), rel));
                    }
                    continue;
                }
                if !ft.is_file() || class_of(&rel) == PathClass::Skip {
                    continue;
                }
                let m = de.metadata()?;
                let (size, mt, ino) = (m.len(), mtime_ns(&m), m.ino());
                let rev = match old.get(&rel) {
                    Some(s) if s.size == size && s.mtime_ns == mt && s.ino == ino => s.rev.clone(),
                    _ => match fs::read(de.path()) {
                        Ok(bytes) => revision_of(&bytes),
                        // Gone or unreadable between the listing and the read: not listed this time.
                        Err(_) => continue,
                    },
                };
                fresh.insert(rel.clone(), Seen { size, mtime_ns: mt, ino, rev: rev.clone() });
                files.push(Entry { path: rel, size, rev });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        self.set_seen(fresh);
        Ok(Manifest { files })
    }

    fn path_for(&self, rel: &str, allow_settings: bool) -> Result<PathBuf> {
        match class_of(rel) {
            PathClass::Sync => {}
            PathClass::PullOnly if allow_settings => {}
            _ => return Err(Error::Invalid(format!("not a path that syncs: {rel}"))),
        }
        no_symlink_below(&self.root, rel)
    }

    /// A file's bytes and their revision.
    pub fn read(&self, rel: &str) -> Result<(Vec<u8>, String)> {
        let path = self.path_for(rel, true)?;
        match fs::read(&path) {
            Ok(b) => {
                let rev = revision_of(&b);
                Ok((b, rev))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(rel.to_owned())),
            Err(e) => Err(e.into()),
        }
    }

    /// The revision on disk now; `None` when there is no file.
    fn current(path: &Path) -> Result<Option<String>> {
        match fs::read(path) {
            Ok(b) => Ok(Some(revision_of(&b))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn conflict(rel: &str, current: Option<String>) -> Error {
        Error::Conflict { path: rel.to_owned(), current: current.unwrap_or_default() }
    }

    /// Writes `bytes` at `rel` if the file is still `expect` (`None`: if there is no file). Returns the new revision.
    /// Otherwise nothing is written and the answer is `Conflict` with the revision found (empty for none).
    /// `allow_settings` lets a device take `.obsidian/<name>.json` from the server; a server never passes it.
    pub fn put(&self, rel: &str, bytes: &[u8], expect: Option<&str>, allow_settings: bool) -> Result<String> {
        let path = self.path_for(rel, allow_settings)?;
        let now = Self::current(&path)?;
        if now.as_deref() != expect {
            return Err(Self::conflict(rel, now));
        }
        let rev = revision_of(bytes);
        match expect {
            Some(want) => {
                let want = want.to_owned();
                atomic_replace(&path, bytes, || match Self::current(&path)? {
                    Some(r) if r == want => Ok(()),
                    other => Err(Self::conflict(rel, other)),
                })?;
            }
            None => self.create_new(&path, rel, bytes)?,
        }
        Ok(rev)
    }

    /// A new file that must not exist: written beside it, then hard-linked into place, which fails rather than replace a
    /// file that appeared meanwhile.
    fn create_new(&self, path: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
        let dir = path.parent().ok_or_else(|| Error::Invalid("no folder".into()))?;
        fs::create_dir_all(dir)?;
        // The folders just made must not be symlinks either.
        no_symlink_below(&self.root, rel)?;
        let tmp = dir.join(format!(".lucina-vault-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let result: Result<()> = (|| {
            let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            drop(f);
            match fs::hard_link(&tmp, path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    Err(Self::conflict(rel, Self::current(path)?))
                }
                Err(e) => Err(e.into()),
            }
        })();
        let _ = fs::remove_file(&tmp);
        result?;
        sync_dir(dir)
    }

    /// Moves `rel` to the vault's `.trash` (where Obsidian looks) if it is still `expect`. Returns where it went.
    pub fn trash(&self, rel: &str, expect: &str) -> Result<String> {
        let from = self.path_for(rel, true)?;
        let now = Self::current(&from)?;
        if now.as_deref() != Some(expect) {
            return Err(Self::conflict(rel, now));
        }
        let mut dest = format!(".trash/{rel}");
        let mut i = 1;
        while self.root.join(&dest).exists() {
            dest = match rel.rsplit_once('.') {
                Some((stem, ext)) if !ext.contains('/') => format!(".trash/{stem} {i}.{ext}"),
                _ => format!(".trash/{rel} {i}"),
            };
            i += 1;
        }
        let to = self.root.join(&dest);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        if let Some(dir) = from.parent() {
            sync_dir(dir)?;
        }
        Ok(dest)
    }

    /// Keeps `bytes` (this side's text that lost to the other side's edit) beside `rel` as
    /// `<stem> (conflict <date> <device>).<ext>`, the name `Vault::save_conflict_copy` uses. Returns the copy's path.
    pub fn conflict_copy(&self, rel: &str, bytes: &[u8], date: &str, device: &str) -> Result<String> {
        let safe: String = device.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
        let (stem, ext) = match rel.rsplit_once('.') {
            Some((s, e)) if !e.contains('/') && !s.is_empty() && !s.ends_with('/') => (s, Some(e)),
            _ => (rel, None),
        };
        let name = |n: Option<u32>| {
            let extra = n.map(|i| format!(" {i}")).unwrap_or_default();
            match ext {
                Some(e) => format!("{stem} (conflict {date} {safe}{extra}).{e}"),
                None => format!("{stem} (conflict {date} {safe}{extra})"),
            }
        };
        let mut n = None;
        loop {
            let p = name(n);
            match self.put(&p, bytes, None, false) {
                Ok(_) => return Ok(p),
                Err(Error::Conflict { .. }) => n = Some(n.map_or(2, |i| i + 1)),
                Err(e) => return Err(e),
            }
        }
    }
}

/// The other side of a sync (Lucina's `/v1/vault` routes for a device).
pub trait Remote {
    fn manifest(&mut self) -> std::result::Result<Manifest, RemoteError>;
    /// The bytes and their revision.
    fn get(&mut self, rel: &str) -> std::result::Result<(Vec<u8>, String), RemoteError>;
    /// Compare-and-swap like [`Folder::put`]; the new revision.
    fn put(&mut self, rel: &str, bytes: &[u8], expect: Option<&str>) -> std::result::Result<String, RemoteError>;
    /// Like [`Folder::trash`].
    fn trash(&mut self, rel: &str, expect: &str) -> std::result::Result<(), RemoteError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// The server's file is not what was expected; nothing was written there.
    Conflict,
    /// Anything else: no connection, signed out, refused. Says what, for the screen.
    Other(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::Conflict => f.write_str("changed on the server meanwhile"),
            RemoteError::Other(m) => f.write_str(m),
        }
    }
}

/// What sync keeps between runs, outside the vault.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Path → the revision both sides had at the last sync.
    pub base: BTreeMap<String, String>,
    /// The local revision cache (see [`Folder::manifest`]).
    #[serde(default)]
    pub seen: BTreeMap<String, Seen>,
}

impl State {
    /// A missing file is a first sync. A damaged one is an error: starting over would turn every difference into a
    /// conflict copy, so it is kept for a person to look at.
    pub fn load(path: &Path) -> Result<State> {
        match fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| Error::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().ok_or_else(|| Error::Invalid("no folder".into()))?;
        fs::create_dir_all(dir)?;
        let bytes = serde_json::to_vec(self).map_err(|e| Error::Invalid(e.to_string()))?;
        let tmp = dir.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("state")));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        sync_dir(dir)
    }
}

/// One file's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    pub local: Option<String>,
    pub remote: Option<String>,
    pub base: Option<String>,
    pub act: Act,
}

fn facts(base: Option<&str>, local: Option<&str>, remote: Option<&str>) -> Facts {
    let eq = |a: Option<&str>, b: Option<&str>| a.is_some() && a == b;
    Facts {
        base: base.is_some(),
        local: local.is_some(),
        remote: remote.is_some(),
        local_is_base: eq(local, base),
        remote_is_base: eq(remote, base),
        local_is_remote: eq(local, remote),
    }
}

/// The act for every path in either manifest or the base. Settings (`.obsidian/*.json`) only ever come from the server.
/// Files over `max_size` on either side are left out and counted in the second value.
pub fn plan(local: &Manifest, remote: &Manifest, base: &BTreeMap<String, String>, max_size: u64) -> (Vec<Item>, usize) {
    let (l, r) = (local.by_path(), remote.by_path());
    let paths: BTreeSet<&str> = l.keys().chain(r.keys()).copied().chain(base.keys().map(String::as_str)).collect();
    let mut items = Vec::new();
    let mut large = 0;
    for p in paths {
        let class = class_of(p);
        if class == PathClass::Skip {
            continue;
        }
        let (le, re) = (l.get(p), r.get(p));
        if le.is_some_and(|e| e.size > max_size) || re.is_some_and(|e| e.size > max_size) {
            large += 1;
            continue;
        }
        let (lr, rr, br) = (le.map(|e| e.rev.as_str()), re.map(|e| e.rev.as_str()), base.get(p).map(String::as_str));
        let act = match class {
            PathClass::PullOnly => match (lr, rr) {
                (_, Some(r)) if lr == Some(r) => Act::Record,
                (_, Some(_)) => Act::Pull,
                // Settings removed on the server, or never there: this side's copy stays, untracked.
                _ => Act::Nothing,
            },
            _ => decide(facts(br, lr, rr)),
        };
        items.push(Item {
            path: p.to_owned(),
            local: lr.map(str::to_owned),
            remote: rr.map(str::to_owned),
            base: br.map(str::to_owned),
            act,
        });
    }
    (items, large)
}

/// Settings for [`run`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Names this device in conflict copies: `juno`, `phone`.
    pub device: String,
    /// `YYYY-MM-DD` for conflict copies.
    pub date: String,
    pub max_size: u64,
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub pulled: usize,
    pub pushed: usize,
    pub trashed_here: usize,
    pub trashed_there: usize,
    /// Conflict copies made here (sent to the server too, in the same run when it allows).
    pub conflicts: Vec<String>,
    /// Files that changed while the run went on; the next run takes them.
    pub deferred: usize,
    /// Left out for size.
    pub large: usize,
    /// Files that failed for another reason, with why.
    pub errors: Vec<String>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.pulled + self.pushed + self.trashed_here + self.trashed_there + self.conflicts.len() > 0
    }
}

/// Brings `folder` and `remote` into step: up to three rounds, so that the conflict copies made in one go to the server
/// in the next. `state` is updated as files settle; the caller saves it (also after an error), and `progress` hears the
/// path being worked on. A failed connection stops the run with what was done so far in `state`.
pub fn run(
    folder: &Folder,
    remote: &mut dyn Remote,
    state: &mut State,
    opts: &Options,
    progress: &mut dyn FnMut(&str),
) -> std::result::Result<Report, RemoteError> {
    folder.set_seen(std::mem::take(&mut state.seen));
    let mut report = Report::default();
    for _round in 0..3 {
        let local = folder.manifest().map_err(|e| RemoteError::Other(format!("this vault: {e}")))?;
        state.seen = folder.seen();
        let remote_m = remote.manifest()?;
        let (items, large) = plan(&local, &remote_m, &state.base, opts.max_size);
        report.large = large;
        let mut acted = false;
        for it in items {
            if !matches!(it.act, Act::Nothing | Act::Record) {
                progress(&it.path);
                acted = true;
            }
            match step(folder, remote, state, opts, &it, &mut report) {
                Ok(()) => {}
                Err(StepError::Deferred) => report.deferred += 1,
                Err(StepError::Local(e)) => report.errors.push(format!("{}: {e}", it.path)),
                Err(StepError::Remote(RemoteError::Conflict)) => report.deferred += 1,
                Err(StepError::Remote(e)) => {
                    state.seen = folder.seen();
                    return Err(e);
                }
            }
        }
        state.seen = folder.seen();
        if !acted {
            break;
        }
    }
    Ok(report)
}

enum StepError {
    /// A side changed after the manifests: decided again next round.
    Deferred,
    Local(Error),
    Remote(RemoteError),
}

impl From<RemoteError> for StepError {
    fn from(e: RemoteError) -> Self {
        StepError::Remote(e)
    }
}

fn local(e: Error) -> StepError {
    match e {
        Error::Conflict { .. } | Error::NotFound(_) => StepError::Deferred,
        e => StepError::Local(e),
    }
}

fn step(
    folder: &Folder,
    remote: &mut dyn Remote,
    state: &mut State,
    opts: &Options,
    it: &Item,
    report: &mut Report,
) -> std::result::Result<(), StepError> {
    let settings = class_of(&it.path) == PathClass::PullOnly;
    match it.act {
        Act::Nothing => {
            state.base.remove(&it.path);
        }
        Act::Record => {
            if let Some(r) = &it.local {
                state.base.insert(it.path.clone(), r.clone());
            }
        }
        Act::Pull => {
            let (bytes, rev) = remote.get(&it.path)?;
            if Some(&rev) != it.remote.as_ref() {
                return Err(StepError::Deferred);
            }
            folder.put(&it.path, &bytes, it.local.as_deref(), settings).map_err(local)?;
            state.base.insert(it.path.clone(), rev);
            report.pulled += 1;
        }
        Act::Push => {
            let (bytes, rev) = folder.read(&it.path).map_err(local)?;
            if Some(&rev) != it.local.as_ref() {
                return Err(StepError::Deferred);
            }
            let new = remote.put(&it.path, &bytes, it.remote.as_deref())?;
            state.base.insert(it.path.clone(), new);
            report.pushed += 1;
        }
        Act::TrashLocal => {
            let expect = it.local.as_deref().ok_or(StepError::Deferred)?;
            folder.trash(&it.path, expect).map_err(local)?;
            state.base.remove(&it.path);
            report.trashed_here += 1;
        }
        Act::TrashRemote => {
            let expect = it.remote.as_deref().ok_or(StepError::Deferred)?;
            remote.trash(&it.path, expect)?;
            state.base.remove(&it.path);
            report.trashed_there += 1;
        }
        Act::Conflict => {
            // This side's text first, so that a stop between the two steps loses nothing.
            let (mine, rev) = folder.read(&it.path).map_err(local)?;
            if Some(&rev) != it.local.as_ref() {
                return Err(StepError::Deferred);
            }
            let (theirs, their_rev) = remote.get(&it.path)?;
            if Some(&their_rev) != it.remote.as_ref() {
                return Err(StepError::Deferred);
            }
            let copy = folder.conflict_copy(&it.path, &mine, &opts.date, &opts.device).map_err(local)?;
            report.conflicts.push(copy);
            folder.put(&it.path, &theirs, Some(&rev), false).map_err(local)?;
            state.base.insert(it.path.clone(), their_rev);
            report.pulled += 1;
        }
    }
    Ok(())
}

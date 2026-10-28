//! Bulk imports: a folder (e.g. a Nextcloud user's files, read through the data dir) or a
//! Google Takeout export (folder or `.zip`). Sources are copied, never moved or changed.
//!
//! Takeout strips EXIF dates and places from many files and puts them in JSON sidecars
//! (`IMG_1.jpg.json`, `IMG_1.jpg.supplemental-metadata.json`, or a name truncated to 51
//! characters). Those values win over the file's own.
//!
//! A folder import can keep a journal (JSON Lines, one line per file): a rerun skips every file
//! it lists as done and retries the failures, so a big Takeout survives restarts. Takeout Live
//! Photos (`IMG_1.HEIC` + `IMG_1.MP4` in one folder) are paired after the walk, from the journal,
//! by item id. No device or asset id is set on Takeout files, so a later iPhone upload of the same
//! photo still records its asset on the item (`ingest_file`'s duplicate branch).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::gallery;
use crate::ingest::{self, Library};
use crate::meta::ClientMeta;

#[derive(Clone, Debug)]
pub struct ImportOpts {
    /// `items.source`, e.g. `nextcloud`, `takeout`, `import`.
    pub source: String,
    /// Put everything in this album (Takeout album folders add their own).
    pub album: Option<String>,
    /// Folder imports only: the JSON Lines journal that makes a rerun resume.
    pub journal: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportStats {
    pub seen: usize,
    pub added: usize,
    pub duplicates: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Files whose outcome came from the journal of an earlier run (counted in the totals too).
    pub resumed: usize,
    /// Takeout Live Photos paired by this run.
    pub live_pairs: usize,
    /// Takeout motion halves left hidden because their photo already has one (e.g. from the iPhone).
    pub live_redundant: usize,
}

impl ImportStats {
    fn count(&mut self, o: Outcome) {
        match o {
            Outcome::Added => self.added += 1,
            Outcome::Duplicate => self.duplicates += 1,
            Outcome::Failed => self.failed += 1,
            Outcome::Skipped => self.skipped += 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Added,
    Duplicate,
    Failed,
    Skipped,
}

/// One journal line: what happened to one file, by its path relative to the import root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalLine {
    pub path: String,
    pub outcome: Outcome,
    pub item_id: Option<String>,
    pub error: Option<String>,
    pub album: Option<String>,
    pub at: String,
}

/// The newest line per path, plus the open file new lines go to (none without `--journal`).
#[derive(Default)]
struct Journal {
    file: Option<fs::File>,
    lines: HashMap<String, JournalLine>,
}

impl Journal {
    fn open(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let mut lines = HashMap::new();
        let mut ends_clean = true;
        if path.exists() {
            let text = fs::read(path)?;
            ends_clean = text.is_empty() || text.ends_with(b"\n");
            lines = parse_journal(&text);
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        if !ends_clean {
            // A crash mid-line: the next line starts on its own.
            file.write_all(b"\n")?;
        }
        Ok(Self {
            file: Some(file),
            lines,
        })
    }

    /// The outcome of `path` from an earlier run, unless it failed (failures are retried).
    fn finished(&self, path: &str) -> Option<&JournalLine> {
        self.lines
            .get(path)
            .filter(|l| l.outcome != Outcome::Failed)
    }

    fn record(&mut self, line: JournalLine) -> io::Result<()> {
        if let Some(f) = &mut self.file {
            let mut s = serde_json::to_string(&line).map_err(io::Error::other)?;
            s.push('\n');
            f.write_all(s.as_bytes())?;
            f.flush()?;
        }
        self.lines.insert(line.path.clone(), line);
        Ok(())
    }
}

/// Journal lines by path, the newest winning; unreadable lines (a truncated last one) are skipped.
fn parse_journal(text: &[u8]) -> HashMap<String, JournalLine> {
    let mut out = HashMap::new();
    for line in text.split(|b| *b == b'\n') {
        if let Ok(l) = serde_json::from_slice::<JournalLine>(line) {
            out.insert(l.path.clone(), l);
        }
    }
    out
}

fn journal_line(
    path: &str,
    outcome: Outcome,
    item_id: Option<String>,
    error: Option<String>,
    album: Option<String>,
) -> JournalLine {
    JournalLine {
        path: path.to_string(),
        outcome,
        item_id,
        error,
        album,
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Sidecar {
    title: Option<String>,
    description: Option<String>,
    photo_taken_time: Option<Ts>,
    geo_data: Option<Geo>,
    geo_data_exif: Option<Geo>,
    favorited: Option<bool>,
}

#[derive(Deserialize)]
struct Ts {
    timestamp: String,
}

#[derive(Deserialize)]
struct Geo {
    latitude: f64,
    longitude: f64,
}

/// An album folder's `metadata.json`.
#[derive(Deserialize)]
struct AlbumMeta {
    title: Option<String>,
}

fn media_like(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !lower.ends_with(".json")
        && !lower.starts_with('.')
        && !lower.ends_with(".html")
        && lower != "thumbs.db"
}

fn sidecar_meta(sc: &Sidecar, client: &mut ClientMeta) {
    if let Some(t) = sc
        .photo_taken_time
        .as_ref()
        .and_then(|t| t.timestamp.parse::<i64>().ok())
    {
        if t > 0 {
            client.taken_at = chrono::DateTime::from_timestamp(t, 0)
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
    }
    for g in [&sc.geo_data, &sc.geo_data_exif].into_iter().flatten() {
        if g.latitude != 0.0 || g.longitude != 0.0 {
            client.lat = Some(g.latitude);
            client.lon = Some(g.longitude);
            break;
        }
    }
    client.caption = sc.description.clone().filter(|d| !d.trim().is_empty());
    if sc.favorited == Some(true) {
        client.favorite = Some(true);
    }
    if let Some(t) = &sc.title {
        client.filename = Some(t.clone());
    }
}

/// Candidate sidecar names for a media file, most specific first.
pub fn sidecar_names(file: &str) -> Vec<String> {
    let mut v = vec![
        format!("{file}.supplemental-metadata.json"),
        format!("{file}.json"),
    ];
    // Takeout truncates "<name>.<suffix>.json" to 51 characters before ".json".
    for suffix in [".supplemental-metadata", ""] {
        let full = format!("{file}{suffix}");
        if full.len() > 46 {
            let cut: String = full.chars().take(46).collect();
            v.push(format!("{cut}.json"));
        }
    }
    // "IMG_1(1).jpg" → "IMG_1.jpg(1).json"
    if let Some((stem, rest)) = file.split_once('(') {
        if let Some((n, ext)) = rest.split_once(')') {
            v.push(format!("{stem}{ext}({n}).json"));
        }
    }
    v
}

/// The Google Photos folder names of a Takeout (localized), under `Takeout/`.
const TAKEOUT_ROOTS: &[&str] = &["google photos", "google fotos", "google foto"];
/// Date buckets: "<prefix> <year>", e.g. "Photos from 2019", "Fotos fra 2019".
const DATE_BUCKETS: &[&str] = &[
    "photos from",
    "fotos fra",
    "billeder fra",
    "foto dal",
    "foto del",
    "fotos de",
    "fotos von",
];
/// Archived photos: imported, but the folder is not an album.
const ARCHIVE_DIRS: &[&str] = &["archive", "arkiv", "archivio"];
/// Deleted photos: not imported.
const TRASH_DIRS: &[&str] = &["trash", "bin", "papirkurv", "cestino", "papierkorb"];
/// An album folder's own metadata file (localized).
const ALBUM_METADATA: &[&str] = &[
    "metadata.json",
    "metadati.json",
    "metadaten.json",
    "métadonnées.json",
];
const STILL_EXTS: &[&str] = &["heic", "heif", "jpg", "jpeg"];
const MOTION_EXTS: &[&str] = &["mov", "mp4", "m4v"];
/// A "motion half" longer than this is a real video, never paired.
const MAX_MOTION_MS: i64 = 6000;

fn is_date_bucket(dir_name: &str) -> bool {
    let lower = dir_name.trim().to_lowercase();
    let Some((prefix, year)) = lower.rsplit_once(' ') else {
        return false;
    };
    year.len() == 4
        && year.bytes().all(|b| b.is_ascii_digit())
        && DATE_BUCKETS.contains(&prefix.trim())
}

/// Album name implied by a Takeout folder ("Photos from 2019" is a date bucket, not an album).
fn takeout_album(dir_name: &str) -> Option<String> {
    let d = dir_name.trim();
    let lower = d.to_lowercase();
    if d.is_empty()
        || lower == "takeout"
        || TAKEOUT_ROOTS.contains(&lower.as_str())
        || ARCHIVE_DIRS.contains(&lower.as_str())
        || is_date_bucket(d)
    {
        None
    } else {
        Some(d.to_string())
    }
}

fn dir_name_lower(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().trim().to_lowercase())
        .unwrap_or_default()
}

/// A Takeout trash folder: a trash name directly under the Google Photos folder.
fn is_trash(dir: &Path) -> bool {
    TRASH_DIRS.contains(&dir_name_lower(dir).as_str())
        && dir
            .parent()
            .is_some_and(|p| TAKEOUT_ROOTS.contains(&dir_name_lower(p).as_str()))
}

/// "IMG_1(1).HEIC" → ("img_1(1)", "heic").
fn stem_ext(name: &str) -> Option<(String, String)> {
    let (stem, ext) = name.rsplit_once('.')?;
    Some((stem.to_lowercase(), ext.to_ascii_lowercase()))
}

/// What a folder import needs to know about one directory, read once.
#[derive(Default)]
struct DirInfo {
    /// Lower-case stems of its stills, to spot Live Photo motion halves.
    still_stems: HashSet<String>,
    /// Its `.json` file names.
    json: Vec<String>,
    /// Sidecar titles → json names, read on first need.
    titles: Option<HashMap<String, Vec<String>>>,
    /// The album this folder stands for (Takeout), from its metadata.json or its name.
    album: Option<String>,
    trash: bool,
}

impl DirInfo {
    fn read(dir: &Path, takeout: bool) -> Self {
        let mut info = DirInfo::default();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.filter_map(Result::ok) {
                if !e.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                if name.to_ascii_lowercase().ends_with(".json") {
                    info.json.push(name);
                } else if let Some((stem, ext)) = stem_ext(&name) {
                    if STILL_EXTS.contains(&ext.as_str()) {
                        info.still_stems.insert(stem);
                    }
                }
            }
        }
        if takeout {
            info.trash = is_trash(dir);
            let folder = dir
                .file_name()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default();
            info.album = takeout_album(&folder).map(|name| {
                info.json
                    .iter()
                    .filter(|j| ALBUM_METADATA.contains(&j.to_lowercase().as_str()))
                    .filter_map(|j| fs::read(dir.join(j)).ok())
                    .filter_map(|b| serde_json::from_slice::<AlbumMeta>(&b).ok())
                    .filter_map(|m| m.title)
                    .map(|t| t.trim().to_string())
                    .find(|t| !t.is_empty())
                    .unwrap_or(name)
            });
        }
        info
    }

    /// A video whose stem matches a still in the same folder: the still's Live Photo motion.
    fn is_motion_half(&self, name: &str) -> bool {
        stem_ext(name).is_some_and(|(stem, ext)| {
            MOTION_EXTS.contains(&ext.as_str()) && self.still_stems.contains(&stem)
        })
    }

    /// The sidecar of `name` in `dir`: Google's names first, then localized suffixes
    /// ("IMG_1.jpg.supplerende-metadata.json"), truncated names, then a unique `title` match.
    fn find_sidecar(&mut self, dir: &Path, name: &str) -> Option<PathBuf> {
        let has = |n: &str| self.json.iter().any(|j| j == n);
        if let Some(n) = sidecar_names(name).into_iter().find(|n| has(n)) {
            return Some(dir.join(n));
        }
        let prefix = format!("{name}.");
        if let Some(j) = self
            .json
            .iter()
            .filter(|j| j.starts_with(&prefix))
            .min_by_key(|j| j.len())
        {
            return Some(dir.join(j));
        }
        // "IMG_1(1).jpg" → "IMG_1.jpg.<suffix>(1).json"
        if let Some((stem, rest)) = name.split_once('(') {
            if let Some((n, ext)) = rest.split_once(')') {
                let head = format!("{stem}{ext}.");
                let tail = format!("({n}).json");
                if let Some(j) = self
                    .json
                    .iter()
                    .filter(|j| j.starts_with(&head) && j.ends_with(&tail))
                    .min_by_key(|j| j.len())
                {
                    return Some(dir.join(j));
                }
            }
        }
        // The name itself truncated ("<first 46 characters>.json").
        if let Some(j) = self.json.iter().find(|j| {
            j.strip_suffix(".json")
                .is_some_and(|b| b.chars().count() >= 40 && name.starts_with(b))
        }) {
            return Some(dir.join(j));
        }
        let titles = self.titles.get_or_insert_with(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for j in &self.json {
                if let Some(t) = fs::read(dir.join(j))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok())
                    .and_then(|s| s.title)
                {
                    m.entry(t).or_default().push(j.clone());
                }
            }
            m
        });
        match titles.get(name).map(Vec::as_slice) {
            Some([only]) => Some(dir.join(only)),
            _ => None,
        }
    }
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn import_path(lib: &Library, root: &Path, opts: &ImportOpts) -> anyhow::Result<ImportStats> {
    let mut stats = ImportStats::default();
    if root.is_file()
        && root
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        import_zip(lib, root, opts, &mut stats)?;
        return Ok(stats);
    }
    let takeout = opts.source == "takeout";
    let mut journal = Journal::open(opts.journal.as_deref())?;
    let mut dirs: HashMap<PathBuf, DirInfo> = HashMap::new();
    // Takeout puts a photo in its date bucket and again in each album folder. Buckets first, so
    // the copy that is added (not a duplicate) is the one most likely to have its sidecar.
    let walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by(move |a, b| {
            let album = |e: &walkdir::DirEntry| {
                takeout
                    && e.file_type().is_dir()
                    && takeout_album(&e.file_name().to_string_lossy()).is_some()
            };
            album(a)
                .cmp(&album(b))
                .then_with(|| a.file_name().cmp(b.file_name()))
        });
    for entry in walk.into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.to_ascii_lowercase().ends_with(".zip") {
            import_zip(lib, path, opts, &mut stats)?;
            continue;
        }
        if !media_like(&name) {
            continue;
        }
        stats.seen += 1;
        let rel = rel_path(root, path);
        if let Some(done) = journal.finished(&rel) {
            stats.resumed += 1;
            stats.count(done.outcome);
            continue;
        }
        let dir = path.parent().unwrap_or(root);
        let info = dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| DirInfo::read(dir, takeout));
        if takeout && info.trash {
            stats.skipped += 1;
            journal.record(journal_line(
                &rel,
                Outcome::Skipped,
                None,
                Some("trash".into()),
                None,
            ))?;
            continue;
        }
        let (origin_folder, origin_root) = gallery::import_origin(root, path);
        let mut client = ClientMeta {
            filename: Some(name.clone()),
            source: Some(opts.source.clone()),
            origin_folder,
            origin_root,
            ..Default::default()
        };
        client.album = opts.album.clone();
        if takeout {
            if let Some(sc) = info.find_sidecar(dir, &name) {
                match fs::read(&sc)
                    .map_err(anyhow::Error::from)
                    .and_then(|b| Ok(serde_json::from_slice::<Sidecar>(&b)?))
                {
                    Ok(sc) => {
                        sidecar_meta(&sc, &mut client);
                        client.filename = Some(name.clone());
                    }
                    Err(e) => {
                        tracing::warn!(sidecar = %sc.display(), error = %e, "unreadable sidecar")
                    }
                }
            }
            if client.album.is_none() {
                client.album = info.album.clone();
            }
            if info.is_motion_half(&name) {
                // Hidden like an iPhone motion half; paired (or shown again) after the walk.
                client.live_motion = Some(true);
            }
        }
        let line = match copy_in(lib, path) {
            Err(e) => Err(e),
            Ok(tmp) => ingest::ingest_file(lib, &tmp, None, &client, None).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            }),
        };
        let line = match line {
            Ok(r) if r.duplicate => {
                stats.duplicates += 1;
                if let Some(a) = &client.album {
                    add_dup_to_album(lib, a, &r.item_id)?;
                }
                journal_line(
                    &rel,
                    Outcome::Duplicate,
                    Some(r.item_id),
                    None,
                    client.album,
                )
            }
            Ok(r) => {
                stats.added += 1;
                journal_line(&rel, Outcome::Added, Some(r.item_id), None, client.album)
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "import failed");
                stats.failed += 1;
                journal_line(
                    &rel,
                    Outcome::Failed,
                    None,
                    Some(format!("{e:#}")),
                    client.album,
                )
            }
        };
        journal.record(line)?;
        if stats.seen % 200 == 0 {
            tracing::info!(?stats, "import progress");
        }
    }
    if takeout {
        pair_live(lib, &journal, &mut stats)?;
    }
    Ok(stats)
}

#[derive(Debug, PartialEq, Eq)]
enum PairResult {
    Paired,
    AlreadyPaired,
    /// The photo already has another motion half: this one stays hidden.
    Redundant,
    /// Not a motion half after all: shown again as a plain video.
    Shown,
    Nothing,
}

/// Pair the Takeout Live Photos of this import (and earlier runs of its journal): per folder and
/// stem, one still and its motion halves.
fn pair_live(lib: &Library, journal: &Journal, stats: &mut ImportStats) -> anyhow::Result<()> {
    #[derive(Default)]
    struct Group {
        stills: Vec<String>,
        motions: Vec<(String, Outcome)>,
    }
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    for l in journal.lines.values() {
        if !matches!(l.outcome, Outcome::Added | Outcome::Duplicate) {
            continue;
        }
        let Some(id) = &l.item_id else { continue };
        let (dir, name) = l.path.rsplit_once('/').unwrap_or(("", l.path.as_str()));
        let Some((stem, ext)) = stem_ext(name) else {
            continue;
        };
        let g = groups.entry((dir.to_string(), stem)).or_default();
        if STILL_EXTS.contains(&ext.as_str()) {
            g.stills.push(id.clone());
        } else if MOTION_EXTS.contains(&ext.as_str()) {
            g.motions.push((id.clone(), l.outcome));
        }
    }
    for g in groups.values() {
        // Only an unambiguous folder pair; anything else is shown as a plain video.
        let still = match (g.stills.as_slice(), g.motions.len()) {
            ([s], 1) => Some(s.as_str()),
            _ => None,
        };
        for (motion, outcome) in &g.motions {
            let r = lib.db.with(|c| {
                let tx = c.transaction()?;
                let r = pair_one(&tx, still, motion, *outcome)?;
                tx.commit()?;
                Ok(r)
            })?;
            match r {
                PairResult::Paired => stats.live_pairs += 1,
                PairResult::Redundant => stats.live_redundant += 1,
                _ => {}
            }
        }
    }
    Ok(())
}

fn pair_one(
    c: &Connection,
    still: Option<&str>,
    motion: &str,
    outcome: Outcome,
) -> rusqlite::Result<PairResult> {
    let now = db::now_str();
    if let Some(still) = still.filter(|s| *s != motion) {
        let s_pair: Option<Option<String>> = c
            .query_row("SELECT live_pair FROM items WHERE id = ?1", [still], |r| {
                r.get(0)
            })
            .optional()?;
        let m: Option<(Option<String>, Option<i64>)> = c
            .query_row(
                "SELECT live_pair, duration_ms FROM items WHERE id = ?1",
                [motion],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let (Some(sp), Some((mp, duration))) = (s_pair, m) {
            if sp.as_deref() == Some(motion) && mp.as_deref() == Some(still) {
                return Ok(PairResult::AlreadyPaired);
            }
            let short = match duration {
                Some(d) => d <= MAX_MOTION_MS,
                None => true,
            };
            if short && mp.is_none() {
                if sp.is_none() {
                    c.execute(
                        "UPDATE items SET live_pair = ?2, updated_at = ?3 WHERE id = ?1",
                        params![still, motion, now],
                    )?;
                    c.execute(
                        "UPDATE items SET live_pair = ?2, hidden = 1, updated_at = ?3 WHERE id = ?1",
                        params![motion, still, now],
                    )?;
                    c.execute(
                        "DELETE FROM album_items WHERE album_id = 'sys-videos' AND item_id = ?1",
                        [motion],
                    )?;
                    db::reindex(c, still)?;
                    db::reindex(c, motion)?;
                    return Ok(PairResult::Paired);
                }
                if outcome == Outcome::Added {
                    return Ok(PairResult::Redundant);
                }
            }
        }
    }
    // Hidden by this import as a motion half but not paired: a plain video after all.
    if outcome == Outcome::Added
        && c.execute(
            "UPDATE items SET hidden = 0, updated_at = ?2 WHERE id = ?1 AND hidden = 1 AND live_pair IS NULL",
            params![motion, now],
        )? > 0
    {
        db::add_to_album(c, "sys-videos", motion)?;
        db::reindex(c, motion)?;
        return Ok(PairResult::Shown);
    }
    Ok(PairResult::Nothing)
}

fn add_dup_to_album(lib: &Library, album: &str, item_id: &str) -> anyhow::Result<()> {
    lib.db.with(|c| {
        let aid = ingest::find_or_create_album(c, album)?;
        if db::add_to_album(c, &aid, item_id)? {
            db::reindex(c, item_id)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Copy a source file into the uploads area (same disk as the spool) for ingest to consume.
fn copy_in(lib: &Library, src: &Path) -> anyhow::Result<PathBuf> {
    let tmp = lib.cfg.uploads_dir().join(db::new_id("imp"));
    fs::copy(src, &tmp)?;
    Ok(tmp)
}

fn import_zip(
    lib: &Library,
    zip_path: &Path,
    opts: &ImportOpts,
    stats: &mut ImportStats,
) -> anyhow::Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file))?;
    // Pass 1: the sidecars (small) by full entry name.
    let mut sidecars: HashMap<String, Sidecar> = HashMap::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        if name.ends_with(".json") && e.size() < 1 << 20 {
            let mut buf = Vec::new();
            io::copy(&mut e, &mut buf)?;
            if let Ok(sc) = serde_json::from_slice::<Sidecar>(&buf) {
                sidecars.insert(name, sc);
            }
        }
    }
    // Pass 2: media entries.
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let Some(entry_path) = e.enclosed_name() else {
            stats.skipped += 1;
            continue;
        };
        let name = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !media_like(&name) {
            continue;
        }
        stats.seen += 1;
        let dir = entry_path
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let mut client = ClientMeta {
            filename: Some(name.clone()),
            source: Some(opts.source.clone()),
            // In a zip the "root" is the archive and the folder is the entry's folder in it.
            origin_folder: gallery::clean_folder(&dir),
            origin_root: zip_path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(gallery::clean_name),
            ..Default::default()
        };
        client.album = opts.album.clone();
        for cand in sidecar_names(&name) {
            let key = if dir.is_empty() {
                cand
            } else {
                format!("{dir}/{cand}")
            };
            if let Some(sc) = sidecars.get(&key) {
                sidecar_meta(sc, &mut client);
                client.filename = Some(name.clone());
                break;
            }
        }
        if client.album.is_none() {
            client.album = entry_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|d| takeout_album(&d.to_string_lossy()));
        }
        let tmp = lib.cfg.uploads_dir().join(db::new_id("imp"));
        let res = (|| -> anyhow::Result<ingest::IngestResult> {
            let mut out = fs::File::create(&tmp)?;
            io::copy(&mut e, &mut out)?;
            ingest::ingest_file(lib, &tmp, None, &client, None)
        })();
        match res {
            Ok(r) if r.duplicate => {
                stats.duplicates += 1;
                if let Some(a) = &client.album {
                    add_dup_to_album(lib, a, &r.item_id)?;
                }
            }
            Ok(_) => stats.added += 1,
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                tracing::warn!(entry = %entry_path.display(), error = %err, "zip import failed");
                stats.failed += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store;

    #[test]
    fn sidecar_name_variants() {
        let v = sidecar_names("IMG_1234.JPG");
        assert_eq!(v[0], "IMG_1234.JPG.supplemental-metadata.json");
        assert_eq!(v[1], "IMG_1234.JPG.json");
        let v = sidecar_names("IMG_1(1).jpg");
        assert!(v.contains(&"IMG_1.jpg(1).json".to_string()));
        let long = "PXL_20230612_183015123.NIGHT.RAW-01.MP.COVER.jpg";
        let v = sidecar_names(long);
        assert!(v.iter().any(|n| n.len() == 51));
        assert_eq!(takeout_album("Photos from 2019"), None);
        assert_eq!(
            takeout_album("Copenhagen trip"),
            Some("Copenhagen trip".into())
        );
    }

    #[test]
    fn localized_takeout_folders() {
        for d in [
            "Fotos fra 2019",
            "Billeder fra 2008",
            "Foto dal 2015",
            "photos FROM 2001",
            "Google Fotos",
            "Takeout",
            "Arkiv",
            "Archive",
        ] {
            assert_eq!(takeout_album(d), None, "{d}");
        }
        assert_eq!(takeout_album("Fotos fra Rom"), Some("Fotos fra Rom".into()));
        assert_eq!(takeout_album("Sommer 2019"), Some("Sommer 2019".into()));
        assert!(is_trash(Path::new("x/Takeout/Google Fotos/Papirkurv")));
        assert!(is_trash(Path::new("Takeout/Google Photos/Trash")));
        assert!(!is_trash(Path::new("Takeout/Google Fotos/Bin trip")));
        assert!(!is_trash(Path::new("Takeout/Google Fotos/Rejse/Bin")));
    }

    fn touch(dir: &Path, name: &str, body: &[u8]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn album_title_from_metadata_and_motion_halves() {
        let t = tempfile::tempdir().unwrap();
        let album = t.path().join("Takeout/Google Fotos/Rom_ 2019");
        touch(
            &album,
            "metadata.json",
            br#"{"title":"Rom: 2019","description":""}"#,
        );
        for n in [
            "IMG_1.HEIC",
            "IMG_1.MP4",
            "IMG_2(1).jpg",
            "IMG_2(1).mp4",
            "img_3.JPG",
            "IMG_3.mov",
            "VID_4.mp4",
        ] {
            touch(&album, n, b"x");
        }
        let info = DirInfo::read(&album, true);
        assert_eq!(info.album.as_deref(), Some("Rom: 2019"));
        assert!(!info.trash);
        for n in ["IMG_1.MP4", "IMG_2(1).mp4", "IMG_3.mov"] {
            assert!(info.is_motion_half(n), "{n}");
        }
        for n in ["VID_4.mp4", "IMG_1.HEIC", "IMG_2.mp4"] {
            assert!(!info.is_motion_half(n), "{n}");
        }
        // A date bucket has no album, even with a metadata file.
        let bucket = t.path().join("Takeout/Google Fotos/Fotos fra 2019");
        touch(&bucket, "metadata.json", br#"{"title":"2019"}"#);
        assert_eq!(DirInfo::read(&bucket, true).album, None);
    }

    #[test]
    fn localized_and_truncated_sidecars() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Fotos fra 2014");
        let long = "PXL_20230612_183015123.NIGHT.RAW-01.MP.COVER.jpg";
        let cut: String = long.chars().take(46).collect();
        touch(&d, "IMG_1.jpg.supplerende-metadata.json", b"{}");
        touch(&d, "IMG_5.jpg.supplerende-metadata(1).json", b"{}");
        touch(&d, &format!("{cut}.json"), b"{}");
        touch(&d, "abc.json", br#"{"title":"Strange name.jpg"}"#);
        let mut info = DirInfo::read(&d, true);
        let found = |info: &mut DirInfo, n: &str| {
            info.find_sidecar(&d, n)
                .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        };
        assert_eq!(
            found(&mut info, "IMG_1.jpg").as_deref(),
            Some("IMG_1.jpg.supplerende-metadata.json")
        );
        assert_eq!(
            found(&mut info, "IMG_5(1).jpg").as_deref(),
            Some("IMG_5.jpg.supplerende-metadata(1).json")
        );
        assert_eq!(found(&mut info, long), Some(format!("{cut}.json")));
        assert_eq!(
            found(&mut info, "Strange name.jpg").as_deref(),
            Some("abc.json")
        );
        assert_eq!(found(&mut info, "IMG_9.jpg"), None);
    }

    #[test]
    fn journal_newest_wins_failures_retried_truncation_survives() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("j.jsonl");
        let a1 = journal_line("a.jpg", Outcome::Failed, None, Some("boom".into()), None);
        let a2 = journal_line("a.jpg", Outcome::Added, Some("itm_a".into()), None, None);
        let b = journal_line("b.jpg", Outcome::Failed, None, Some("bad".into()), None);
        let mut text = String::new();
        for l in [&a1, &a2, &b] {
            text.push_str(&serde_json::to_string(l).unwrap());
            text.push('\n');
        }
        text.push_str(r#"{"path":"c.jpg","outco"#);
        fs::write(&p, &text).unwrap();
        let mut j = Journal::open(Some(&p)).unwrap();
        assert_eq!(
            j.finished("a.jpg").unwrap().item_id.as_deref(),
            Some("itm_a")
        );
        assert!(j.finished("b.jpg").is_none());
        assert!(j.finished("c.jpg").is_none());
        j.record(journal_line(
            "c.jpg",
            Outcome::Skipped,
            None,
            Some("trash".into()),
            None,
        ))
        .unwrap();
        drop(j);
        let j = Journal::open(Some(&p)).unwrap();
        assert_eq!(j.finished("c.jpg").unwrap().outcome, Outcome::Skipped);
        assert_eq!(j.lines.len(), 3);
    }

    fn library() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        cfg.quiet_start = 0;
        cfg.quiet_end = 0;
        store::init_store(&cfg).unwrap();
        (tmp, Library::open(cfg).unwrap())
    }

    fn png(shade: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([shade, 90, 160]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn takeout_folder_import_resumes_from_its_journal() {
        let (tmp, lib) = library();
        let root = tmp.path().join("export");
        let photos = root.join("Takeout/Google Fotos");
        let bucket = photos.join("Fotos fra 2014");
        touch(&bucket, "a.png", &png(1));
        touch(
            &bucket,
            "a.png.supplemental-metadata.json",
            br#"{"title":"a.png","description":"Nyhavn","photoTakenTime":{"timestamp":"1400000000"},
                "geoData":{"latitude":55.68,"longitude":12.59}}"#,
        );
        let trip = photos.join("Rejse_ Rom");
        touch(&trip, "metadata.json", br#"{"title":"Rejse: Rom"}"#);
        touch(&trip, "a.png", &png(1));
        touch(&trip, "b.png", &png(2));
        touch(&photos.join("Papirkurv"), "c.png", &png(3));
        let journal = tmp.path().join("journal.jsonl");
        let opts = ImportOpts {
            source: "takeout".into(),
            album: None,
            journal: Some(journal.clone()),
        };
        let s = import_path(&lib, &root, &opts).unwrap();
        assert_eq!(
            (
                s.seen,
                s.added,
                s.duplicates,
                s.skipped,
                s.failed,
                s.resumed
            ),
            (4, 2, 1, 1, 0, 0)
        );
        let (taken, lat, caption, items, in_album): (String, f64, String, i64, i64) = lib
            .db
            .with(|c| {
                let (t, l, cap) = c.query_row(
                    "SELECT taken_at, lat, caption FROM items WHERE origin_name = 'a.png'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                let n = c.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
                let a = c.query_row(
                    "SELECT COUNT(*) FROM album_items ai JOIN albums al ON al.id = ai.album_id
                     WHERE al.title = 'Rejse: Rom'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((t, l, cap, n, a))
            })
            .unwrap();
        assert!(taken.starts_with("2014-05-13"), "{taken}");
        assert!((lat - 55.68).abs() < 1e-6);
        assert_eq!(caption, "Nyhavn");
        assert_eq!((items, in_album), (2, 2));

        // A rerun takes everything from the journal and ingests nothing.
        let s = import_path(&lib, &root, &opts).unwrap();
        assert_eq!(
            (s.seen, s.added, s.duplicates, s.skipped, s.resumed),
            (4, 2, 1, 1, 4)
        );
        let n: i64 = lib
            .db
            .with(|c| c.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 2);
    }

    fn put(tmp: &tempfile::TempDir, lib: &Library, shade: u8, motion: bool) -> String {
        let src = tmp.path().join(format!("in-{shade}"));
        fs::write(&src, png(shade)).unwrap();
        let client = ClientMeta {
            filename: Some(format!("p{shade}.png")),
            source: Some("takeout".into()),
            live_motion: motion.then_some(true),
            ..Default::default()
        };
        ingest::ingest_file(lib, &src, None, &client, None)
            .unwrap()
            .item_id
    }

    fn pair_state(lib: &Library, id: &str) -> (Option<String>, i64) {
        lib.db
            .with(|c| {
                c.query_row(
                    "SELECT live_pair, hidden FROM items WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap()
    }

    fn pair(lib: &Library, still: Option<&str>, motion: &str, o: Outcome) -> PairResult {
        lib.db.with(|c| pair_one(c, still, motion, o)).unwrap()
    }

    #[test]
    fn live_pairs_by_item_id() {
        let (tmp, lib) = library();
        let still = put(&tmp, &lib, 10, false);
        let motion = put(&tmp, &lib, 11, true);
        assert_eq!(pair_state(&lib, &motion).1, 1);
        assert_eq!(
            pair(&lib, Some(&still), &motion, Outcome::Added),
            PairResult::Paired
        );
        assert_eq!(pair_state(&lib, &still), (Some(motion.clone()), 0));
        assert_eq!(pair_state(&lib, &motion), (Some(still.clone()), 1));
        assert_eq!(
            pair(&lib, Some(&still), &motion, Outcome::Duplicate),
            PairResult::AlreadyPaired
        );
        // A second motion half for the same photo stays hidden.
        let other = put(&tmp, &lib, 12, true);
        assert_eq!(
            pair(&lib, Some(&still), &other, Outcome::Added),
            PairResult::Redundant
        );
        assert_eq!(pair_state(&lib, &other), (None, 1));
        // A motion half without its photo is shown as a plain video.
        let lone = put(&tmp, &lib, 13, true);
        assert_eq!(pair(&lib, None, &lone, Outcome::Added), PairResult::Shown);
        assert_eq!(pair_state(&lib, &lone), (None, 0));
        // An older item that was already visible is never touched without a pair.
        let old = put(&tmp, &lib, 14, false);
        assert_eq!(
            pair(&lib, None, &old, Outcome::Duplicate),
            PairResult::Nothing
        );
    }
}

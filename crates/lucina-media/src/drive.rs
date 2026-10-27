//! The Drive: every regular file in the Drive folders (`LUCINA_MEDIA_FOLDERS` minus the film
//! folders), indexed in place with its folder hierarchy, type, size, mtime and blake3, so the apps
//! can browse it like a disk and Lucina can search it.
//!
//! - **Index** ([`index_drive_folder`]): a walk that skips junk (`.git`, `node_modules`, `target`,
//!   `__pycache__`, `.DS_Store`, hidden files; `LUCINA_MEDIA_DRIVE_IGNORE`) and symlinks. A file
//!   unchanged since the last scan (same size and mtime) costs one catalog lookup. A new or changed
//!   file is hashed and becomes an item (`register_external`, `source = folder`, the folder's album),
//!   plus a `drive_files` row with its path. Nothing is copied: the bytes stay where they are.
//! - **Prune**: files the walk no longer finds are trashed in the catalog (never on disk), but only
//!   after a clean, complete walk of a folder that exists and had files (kernel DK4), so an
//!   unmounted disk never empties the Drive.
//! - **Work** ([`drive_worker`]): text for the search index (PDF, Office, text, code), PDF first
//!   pages for thumbnails and Office → PDF, one file at a time, capped and paused at Tctl ≥ 90 °C.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{self, now_str, Item};
use crate::doctype::{self, DocType};
use crate::drive_kernel::{self, ignored, join_under, rel_of, split_rel};
use crate::extract;
use crate::ingest::{self, ExternalReq, Library};
use crate::meta;
use crate::thumbs;

/// `drive` in the item JSON.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DriveInfo {
    pub folder: String,
    pub path: String,
    pub dir: String,
    pub name: String,
    pub ext: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub preview: String,
    pub language: Option<String>,
    pub modified: String,
    pub text: String,
    /// `drive_files.id`, the row of the content index.
    #[serde(skip)]
    pub row: i64,
}

pub const INFO_COLS: &str =
    "df.folder, df.path, df.dir, df.name, df.ext, df.doc_type, df.mtime, df.text_state, df.id";

/// A `DriveInfo` from [`INFO_COLS`] starting at column `at`.
pub fn info_from_row(r: &rusqlite::Row, at: usize) -> rusqlite::Result<DriveInfo> {
    let name: String = r.get(at + 3)?;
    let doc_type: String = r.get(at + 5)?;
    let text: String = r.get(at + 7)?;
    let t = DocType::parse(&doc_type).unwrap_or(DocType::Other);
    // A file of unknown type whose bytes turned out to be text previews as text.
    let preview = if t == DocType::Other && text == "ready" {
        "text"
    } else {
        t.preview()
    };
    Ok(DriveInfo {
        folder: r.get(at)?,
        path: r.get(at + 1)?,
        dir: r.get(at + 2)?,
        language: matches!(t, DocType::Text | DocType::Code)
            .then(|| doctype::language_for(&name).map(str::to_string))
            .flatten(),
        ext: r.get(at + 4)?,
        name,
        doc_type,
        preview: preview.into(),
        modified: r.get(at + 6)?,
        text,
        row: r.get(at + 8)?,
    })
}

pub fn info_for(c: &Connection, item_id: &str) -> rusqlite::Result<Option<DriveInfo>> {
    c.prepare_cached(&format!(
        "SELECT {INFO_COLS} FROM drive_files df WHERE df.item_id = ?1"
    ))?
    .query_row([item_id], |r| info_from_row(r, 0))
    .optional()
}

/// The Drive folders: the configured folders that are not film folders.
pub fn drive_folders(lib: &Library) -> Vec<(String, PathBuf)> {
    lib.cfg
        .folders
        .iter()
        .filter(|(label, _)| !lib.cfg.film_folders.iter().any(|f| f == label))
        .cloned()
        .collect()
}

/// Where a Drive file is on disk: its folder's root joined with its (safe) path.
pub fn file_path(lib: &Library, info: &DriveInfo) -> Option<PathBuf> {
    let root = lib
        .cfg
        .folders
        .iter()
        .find(|(l, _)| *l == info.folder)
        .map(|(_, p)| p.clone())?;
    join_under(&root, &info.path)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ScanStats {
    pub added: usize,
    pub kept: usize,
    pub pruned: usize,
    pub skipped: usize,
    pub errors: usize,
}

fn skip_name(lib: &Library, name: &str) -> bool {
    ignored(name, &lib.cfg.drive_ignore) || (!lib.cfg.drive_hidden && name.starts_with('.'))
}

fn mtime_of(meta: &std::fs::Metadata) -> (i64, String) {
    let t = meta.modified().unwrap_or(UNIX_EPOCH);
    let ns = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    (ns, dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Derived files of an item that describe its old bytes.
fn drop_derived(lib: &Library, item_id: &str) {
    let dir = thumbs::dir_for(&lib.cfg, item_id);
    for f in ["text.txt", "page1.jpg", "doc.pdf"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// Wait while the box is too hot or loaded for background Drive work (DK5). Logs a pause once.
pub fn wait_until_cool(lib: &Library) {
    let gate = secretary_media_stream::gate::Gate::new(
        0,
        0,
        lib.cfg.drive_max_temp_c,
        lib.cfg.prepare_max_load,
    );
    let mut logged = false;
    loop {
        let r = gate.reading();
        let ok = drive_kernel::drive_may_work(
            r.temp_c.is_some(),
            r.temp_c.map(|t| t.max(0.0) as u32).unwrap_or(0),
            lib.cfg.drive_max_temp_c,
            r.load.is_none_or(|l| l < lib.cfg.prepare_max_load as f32),
        );
        if ok {
            return;
        }
        if !logged {
            tracing::info!(temp = ?r.temp_c, load = ?r.load, "drive: paused (hot or loaded)");
            logged = true;
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Index every regular file under `root` as the Drive folder `label`. Blocking.
pub fn index_drive_folder(lib: &Library, label: &str, root: &Path) -> anyhow::Result<ScanStats> {
    let mut st = ScanStats::default();
    let root_ok = root.is_dir();
    // The filesystem the folder's files were indexed from (DK4: an unmounted disk shows the
    // mountpoint's own filesystem instead).
    let dev = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(root).map(|m| m.dev() as i64).ok()
    };
    let recorded: Option<i64> = lib.db.with(|c| {
        c.query_row(
            "SELECT dev FROM drive_folders WHERE folder = ?1",
            [label],
            |r| r.get(0),
        )
        .optional()
    })?;
    let same_fs = recorded.is_none() || recorded == dev;
    let mut seen: HashSet<String> = HashSet::new();
    if root_ok {
        // Symlinks are not followed: a link inside a project (to `/`, to a home) must not pull the
        // rest of the disk into the Drive. The root itself may be a link.
        let walk = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !skip_name(lib, &e.file_name().to_string_lossy()));
        for entry in walk {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(folder = label, error = %e, "drive walk error");
                    st.errors += 1;
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(rel) = rel_of(root, path) else {
                st.skipped += 1;
                continue;
            };
            let Ok(md) = entry.metadata() else {
                st.errors += 1;
                continue;
            };
            seen.insert(rel.clone());
            match index_one(lib, label, &rel, path, &md) {
                Ok(true) => st.added += 1,
                Ok(false) => st.kept += 1,
                Err(e) => {
                    // The file is seen (it is not pruned); it is tried again next scan.
                    tracing::warn!(path = %path.display(), error = %e, "drive: index failed");
                    st.skipped += 1;
                }
            }
        }
    }
    let known: Vec<(i64, String, String)> = lib.db.with(|c| {
        let mut s = c.prepare("SELECT id, item_id, path FROM drive_files WHERE folder = ?1")?;
        let rows = s.query_map([label], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })?;
    let missing = known.iter().filter(|(_, _, p)| !seen.contains(p)).count();
    if let Some(dev) = dev.filter(|d| {
        recorded != Some(*d)
            && drive_kernel::may_adopt_fs(
                root_ok,
                st.errors as u64,
                seen.len() as u64,
                missing as u64,
            )
    }) {
        lib.db.with(|c| {
            c.execute(
                "INSERT INTO drive_folders (folder, dev) VALUES (?1, ?2) ON CONFLICT(folder) DO UPDATE SET dev = excluded.dev",
                params![label, dev],
            )
        })?;
        if recorded.is_some() {
            tracing::info!(
                folder = label,
                "drive: the folder's files are all on a new filesystem; adopted it"
            );
        }
    }
    if drive_kernel::may_prune(
        root_ok,
        same_fs,
        st.errors as u64,
        seen.len() as u64,
        known.len() as u64,
    ) {
        for (row, item_id, path) in known.into_iter().filter(|(_, _, p)| !seen.contains(p)) {
            prune(lib, row, &item_id)?;
            tracing::info!(folder = label, %path, "drive: file gone, trashed in the catalog");
            st.pruned += 1;
        }
    } else if !known.is_empty() {
        tracing::warn!(
            folder = label,
            root_ok,
            same_fs,
            errors = st.errors,
            seen = seen.len(),
            "drive: not pruning after an incomplete scan"
        );
    }
    Ok(st)
}

fn prune(lib: &Library, row: i64, item_id: &str) -> anyhow::Result<()> {
    lib.db.with(|c| {
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE items SET trashed_at = ?2, updated_at = ?2 WHERE id = ?1 AND trashed_at IS NULL",
            params![item_id, now_str()],
        )?;
        tx.execute("DELETE FROM drive_fts WHERE rowid = ?1", [row])?;
        tx.execute("DELETE FROM drive_files WHERE id = ?1", [row])?;
        tx.commit()
    })?;
    drop_derived(lib, item_id);
    lib.emit("item.trashed", item_id);
    Ok(())
}

/// Index one file; `Ok(false)` when it is unchanged since the last scan.
fn index_one(
    lib: &Library,
    label: &str,
    rel: &str,
    path: &Path,
    md: &std::fs::Metadata,
) -> anyhow::Result<bool> {
    let size = md.len() as i64;
    let (mtime_ns, mtime) = mtime_of(md);
    let unchanged = lib.db.with(|c| {
        c.query_row(
            "SELECT 1 FROM drive_files WHERE folder = ?1 AND path = ?2 AND bytes = ?3 AND mtime_ns = ?4",
            params![label, rel, size, mtime_ns],
            |_| Ok(()),
        )
        .optional()
    })?;
    if unchanged.is_some() {
        return Ok(false);
    }
    // Hashing reads the whole file: background work, paused when hot.
    wait_until_cool(lib);
    let (dir, name) = split_rel(rel);
    let mime = meta::mime_for(path, None);
    let kind = meta::kind_for_mime(&mime);
    let doc = doctype::classify(name, kind);
    // The same id the media-only folder index used, so files indexed before keep their items.
    let ext_id = blake3::hash(path.to_string_lossy().as_bytes()).to_hex()[..32].to_string();
    let req = ExternalReq {
        kind: Some(kind.to_string()),
        title: Some(name.to_string()),
        caption: None,
        created_at: (kind == "file").then(|| mtime.clone()),
        duration_ms: None,
        mime: Some(mime),
        path: path.to_string_lossy().to_string(),
        links: serde_json::Value::Null,
        album: Some(label.to_string()),
    };
    let res = ingest::register_external(lib, "folder", &ext_id, &req)?;
    let text_state = if doc.has_text() || (doc == DocType::Other && size <= 1 << 20) {
        "pending"
    } else {
        "none"
    };
    drop_derived(lib, &res.item_id);
    let (dir, name, ext) = (dir.to_string(), name.to_string(), doctype::ext_of(name));
    lib.db.with(|c| {
        let tx = c.transaction()?;
        // The item may have had another path (a folder renamed and back): one row per item.
        for (row,) in tx
            .prepare("SELECT id FROM drive_files WHERE item_id = ?1 OR (folder = ?2 AND path = ?3)")?
            .query_map(params![res.item_id, label, rel], |r| Ok((r.get::<_, i64>(0)?,)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            tx.execute("DELETE FROM drive_fts WHERE rowid = ?1", [row])?;
            tx.execute("DELETE FROM drive_files WHERE id = ?1", [row])?;
        }
        tx.execute(
            "INSERT INTO drive_files (item_id, folder, path, dir, name, ext, doc_type, bytes, mtime, mtime_ns, text_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![res.item_id, label, rel, dir, name, ext, doc.as_str(), size, mtime, mtime_ns, text_state],
        )?;
        if kind == "file" {
            // Documents sort by their modification time ("the last lecture").
            tx.execute(
                "UPDATE items SET taken_at = ?2, sort_at = ?2 WHERE id = ?1",
                params![res.item_id, mtime],
            )?;
        }
        db::reindex(&tx, &res.item_id)?;
        tx.commit()
    })?;
    Ok(true)
}

/// One pending file for the worker.
struct Pending {
    row: i64,
    item_id: String,
    info: DriveInfo,
}

fn next_pending(c: &Connection) -> rusqlite::Result<Option<Pending>> {
    c.query_row(
        &format!(
            "SELECT df.item_id, {INFO_COLS} FROM drive_files df JOIN items i ON i.id = df.item_id
             WHERE df.text_state = 'pending' AND i.trashed_at IS NULL ORDER BY df.mtime DESC LIMIT 1"
        ),
        [],
        |r| {
            let info = info_from_row(r, 1)?;
            Ok(Pending {
                row: info.row,
                item_id: r.get(0)?,
                info,
            })
        },
    )
    .optional()
}

/// Text, first page and Office PDF of one file. Returns the new `text_state` and whether a page
/// picture was made. Blocking.
fn work_one(lib: &Library, p: &Pending) -> (String, bool) {
    let Some(path) = file_path(lib, &p.info).filter(|p| p.is_file()) else {
        return ("failed".into(), false);
    };
    let doc = DocType::parse(&p.info.doc_type).unwrap_or(DocType::Other);
    let dir = thumbs::dir_for(&lib.cfg, &p.item_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return ("failed".into(), false);
    }
    // Err: the extractor failed (state `failed`); Ok(None): the file has no text (`none`).
    let text: Result<Option<String>, ()> = match doc {
        DocType::Pdf | DocType::Document | DocType::Spreadsheet | DocType::Presentation => {
            let out = dir.join("text.txt");
            match extract::extract_text(&lib.cfg, &path, &p.info.name, doc, &out) {
                Ok(true) => Ok(std::fs::read_to_string(&out).ok()),
                Ok(false) => Ok(None),
                Err(e) => {
                    tracing::warn!(item = %p.item_id, error = %e, "drive: text extraction failed");
                    Err(())
                }
            }
        }
        // Text and code are indexed straight from the file (no copy of the text is kept).
        _ => Ok(extract::read_text_prefix(&path, extract::MAX_TEXT_BYTES)
            .ok()
            .flatten()
            .map(|(t, _)| t)
            .filter(|t| t.chars().any(|c| c.is_alphanumeric()))),
    };
    let mut page = false;
    let page1 = dir.join("page1.jpg");
    match doc {
        DocType::Pdf => page = extract::pdf_page(&lib.cfg, &path, &page1).unwrap_or(false),
        DocType::Document | DocType::Spreadsheet | DocType::Presentation
            if lib.cfg.soffice.is_some() =>
        {
            let pdf = dir.join("doc.pdf");
            match extract::office_pdf(&lib.cfg, &path, &p.info.name, &pdf) {
                Ok(true) => page = extract::pdf_page(&lib.cfg, &pdf, &page1).unwrap_or(false),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(item = %p.item_id, error = %e, "drive: office conversion failed")
                }
            }
        }
        _ => {}
    }
    let state = match text {
        Ok(Some(t)) => {
            let body = extract::truncate_text(t);
            let row = p.row;
            let stored = lib.db.with(|c| {
                c.execute("DELETE FROM drive_fts WHERE rowid = ?1", [row])?;
                c.execute(
                    "INSERT INTO drive_fts (rowid, body) VALUES (?1, ?2)",
                    params![row, body],
                )
            });
            if stored.is_ok() {
                "ready"
            } else {
                "failed"
            }
        }
        Ok(None) => "none",
        Err(()) => "failed",
    };
    (state.into(), page)
}

/// The background Drive worker: one file at a time, forever.
pub async fn drive_worker(lib: Library) {
    loop {
        let next = lib.db.call(|c| next_pending(c)).await.ok().flatten();
        let Some(p) = next else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let l = lib.clone();
        let permit = lib.jobs.clone().acquire_owned().await;
        let res = tokio::task::spawn_blocking(move || {
            wait_until_cool(&l);
            let r = work_one(&l, &p);
            (p, r)
        })
        .await;
        drop(permit);
        let Ok((p, (state, page))) = res else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let item = p.item_id.clone();
        let _ = lib
            .db
            .call(move |c| {
                c.execute(
                    "UPDATE drive_files SET text_state = ?2 WHERE id = ?1 AND text_state = 'pending'",
                    params![p.row, state],
                )?;
                if page {
                    // The thumbnail worker turns page1.jpg into the usual renditions.
                    c.execute("UPDATE items SET thumb_state = 'pending' WHERE id = ?1", [&p.item_id])?;
                }
                Ok(())
            })
            .await;
        lib.emit("item.updated", &item);
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FolderOut {
    pub name: String,
    pub path: String,
    pub files: i64,
    pub bytes: i64,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RootOut {
    pub folder: String,
    pub files: i64,
    pub bytes: i64,
    pub modified: Option<String>,
}

pub fn roots(c: &Connection, folders: &[String]) -> rusqlite::Result<Vec<RootOut>> {
    let mut st = c.prepare_cached(
        "SELECT COUNT(*), COALESCE(SUM(df.bytes), 0), MAX(df.mtime) FROM drive_files df JOIN items i ON i.id = df.item_id
         WHERE df.folder = ?1 AND i.trashed_at IS NULL AND i.hidden = 0",
    )?;
    folders
        .iter()
        .map(|f| {
            st.query_row([f], |r| {
                Ok(RootOut {
                    folder: f.clone(),
                    files: r.get(0)?,
                    bytes: r.get(1)?,
                    modified: r.get(2)?,
                })
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct BrowseQuery {
    pub folder: String,
    #[serde(default)]
    pub path: String,
    pub sort: Option<String>,
    pub order: Option<String>,
    #[serde(rename = "type")]
    pub doc_type: Option<String>,
    pub q: Option<String>,
    pub recursive: Option<String>,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error("unsafe path")]
    BadPath,
    #[error("unknown sort or type")]
    BadQuery,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

pub struct Browse {
    pub breadcrumbs: Vec<(String, String)>,
    pub folders: Vec<FolderOut>,
    pub files: Vec<(Item, DriveInfo)>,
    pub total: i64,
    pub next_offset: Option<u32>,
    pub words: Vec<String>,
}

/// One page of a Drive folder. `q.folder` must be a Drive folder (the caller checks).
pub fn browse(c: &Connection, q: &BrowseQuery) -> Result<Browse, BrowseError> {
    if !drive_kernel::safe_rel(&q.path) {
        return Err(BrowseError::BadPath);
    }
    let search = q.q.as_deref().and_then(db::fts_query);
    let recursive = search.is_some() || matches!(q.recursive.as_deref(), Some("1" | "true"));
    let sort = q
        .sort
        .as_deref()
        .unwrap_or(if search.is_some() { "modified" } else { "name" });
    let desc = match q.order.as_deref() {
        Some("asc") => false,
        Some("desc") => true,
        None => matches!(sort, "modified" | "size"),
        Some(_) => return Err(BrowseError::BadQuery),
    };
    let ord = if desc { "DESC" } else { "ASC" };
    let order_by = match sort {
        "name" => format!("df.name COLLATE NOCASE {ord}, df.path {ord}"),
        "modified" => format!("df.mtime {ord}, df.name COLLATE NOCASE"),
        "size" => format!("df.bytes {ord}, df.name COLLATE NOCASE"),
        "type" => format!("df.doc_type {ord}, df.name COLLATE NOCASE"),
        _ => return Err(BrowseError::BadQuery),
    };
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0);

    let mut args: Vec<rusqlite::types::Value> =
        vec![q.folder.clone().into(), q.path.clone().into()];
    let mut wh = vec![
        "df.folder = ?1".to_string(),
        "i.trashed_at IS NULL".into(),
        "i.hidden = 0".into(),
    ];
    if recursive {
        wh.push("(?2 = '' OR df.dir = ?2 OR substr(df.dir, 1, length(?2) + 1) = ?2 || '/')".into());
    } else {
        wh.push("df.dir = ?2".into());
    }
    if let Some(t) = &q.doc_type {
        if DocType::parse(t).is_none() {
            return Err(BrowseError::BadQuery);
        }
        args.push(t.clone().into());
        wh.push(format!("df.doc_type = ?{}", args.len()));
    }
    if let Some(fq) = &search {
        args.push(fq.clone().into());
        let n = args.len();
        wh.push(format!(
            "(i.id IN (SELECT item_id FROM items_fts WHERE items_fts MATCH ?{n}) \
             OR df.id IN (SELECT rowid FROM drive_fts WHERE drive_fts MATCH ?{n}))"
        ));
    }
    let from = format!(
        "FROM drive_files df JOIN items i ON i.id = df.item_id WHERE {}",
        wh.join(" AND ")
    );
    let total: i64 = c.query_row(
        &format!("SELECT COUNT(*) {from}"),
        rusqlite::params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    let cols: String = db::ITEM_COLS
        .split(", ")
        .map(|c| format!("i.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut st = c.prepare(&format!(
        "SELECT {cols}, {INFO_COLS} {from} ORDER BY {order_by} LIMIT {limit} OFFSET {offset}"
    ))?;
    let files: Vec<(Item, DriveInfo)> = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((db::item_from_row(r)?, info_from_row(r, db::ITEM_COL_COUNT)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let next = offset as i64 + files.len() as i64;
    let next_offset = (next < total).then_some(next as u32);

    let folders = if recursive || offset > 0 {
        Vec::new()
    } else {
        subfolders(c, &q.folder, &q.path)?
    };
    let mut breadcrumbs = vec![(q.folder.clone(), String::new())];
    let mut acc = String::new();
    for seg in q.path.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        breadcrumbs.push((seg.to_string(), acc.clone()));
    }
    let words =
        q.q.as_deref()
            .map(|s| {
                s.split(|ch: char| !ch.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
    Ok(Browse {
        breadcrumbs,
        folders,
        files,
        total,
        next_offset,
        words,
    })
}

/// The folders directly in `dir`, each with the files under it (recursively), by name.
pub fn subfolders(c: &Connection, folder: &str, dir: &str) -> rusqlite::Result<Vec<FolderOut>> {
    let mut st = c.prepare_cached(
        "SELECT df.dir, COUNT(*), COALESCE(SUM(df.bytes), 0), MAX(df.mtime) FROM drive_files df JOIN items i ON i.id = df.item_id
         WHERE df.folder = ?1 AND i.trashed_at IS NULL AND i.hidden = 0 AND df.dir != ?2
           AND (?2 = '' OR substr(df.dir, 1, length(?2) + 1) = ?2 || '/')
         GROUP BY df.dir",
    )?;
    let rows = st.query_map(params![folder, dir], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut out: BTreeMap<String, FolderOut> = BTreeMap::new();
    for row in rows {
        let (d, n, bytes, modified) = row?;
        let rest = if dir.is_empty() {
            d.as_str()
        } else {
            &d[dir.len() + 1..]
        };
        let child = rest.split('/').next().unwrap_or(rest).to_string();
        let path = if dir.is_empty() {
            child.clone()
        } else {
            format!("{dir}/{child}")
        };
        let e = out
            .entry(child.to_lowercase() + "\u{0}" + &child)
            .or_insert(FolderOut {
                name: child,
                path,
                files: 0,
                bytes: 0,
                modified: None,
            });
        e.files += n;
        e.bytes += bytes;
        if modified > e.modified {
            e.modified = modified;
        }
    }
    Ok(out.into_values().collect())
}

/// A snippet of a Drive file's text around the first query word, when its text is at hand.
pub fn snippet_for(
    lib: &Library,
    item_id: &str,
    info: &DriveInfo,
    words: &[String],
) -> Option<String> {
    if words.is_empty() || info.text != "ready" {
        return None;
    }
    let doc = DocType::parse(&info.doc_type)?;
    let text = match doc {
        DocType::Pdf | DocType::Document | DocType::Spreadsheet | DocType::Presentation => {
            let p = thumbs::dir_for(&lib.cfg, item_id).join("text.txt");
            extract::read_text_prefix(&p, 2 << 20).ok()??.0
        }
        _ => {
            extract::read_text_prefix(&file_path(lib, info)?, 2 << 20)
                .ok()??
                .0
        }
    };
    extract::snippet(&text, words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn lib_with(folders: &[(&str, &Path)]) -> (tempfile::TempDir, Library) {
        let t = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(t.path().join("data"), t.path().join("store"));
        cfg.folders = folders
            .iter()
            .map(|(l, p)| (l.to_string(), p.to_path_buf()))
            .collect();
        (t, Library::open(cfg).unwrap())
    }

    fn write(p: &Path, body: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn count(lib: &Library, sql: &str) -> i64 {
        lib.db.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    #[test]
    fn indexes_every_file_keeps_the_hierarchy_and_skips_junk() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Tracer");
        write(&root.join("README.md"), b"# Tracer\nA tracing project");
        write(
            &root.join("src/main.rs"),
            b"fn main() { println!(\"trace\"); }",
        );
        write(&root.join("data/run1.csv"), b"t,x\n0,1\n");
        write(&root.join("data/blob.bin"), &[0u8, 1, 2, 3]);
        write(&root.join("docs/paper.pdf"), b"%PDF-1.4 fake");
        write(&root.join(".git/HEAD"), b"ref: refs/heads/main");
        write(&root.join("node_modules/x/index.js"), b"module.exports = 1");
        write(&root.join("target/debug/tracer"), b"\x7fELF");
        write(&root.join("src/__pycache__/m.cpython-312.pyc"), b"\0");
        write(&root.join(".DS_Store"), b"\0");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", root.join("etc-link")).unwrap();
        let (_t, lib) = lib_with(&[("Tracer", &root)]);
        let s = index_drive_folder(&lib, "Tracer", &root).unwrap();
        assert_eq!((s.added, s.kept, s.pruned, s.errors), (5, 0, 0, 0), "{s:?}");
        let paths: Vec<String> = lib
            .db
            .with(|c| {
                let mut st = c.prepare("SELECT path || ' ' || doc_type || ' ' || text_state FROM drive_files ORDER BY path")?;
                let r = st.query_map([], |r| r.get(0))?.collect();
                r
            })
            .unwrap();
        assert_eq!(
            paths,
            vec![
                "README.md text pending",
                "data/blob.bin other pending",
                "data/run1.csv text pending",
                "docs/paper.pdf pdf pending",
                "src/main.rs code pending",
            ]
        );
        // Every file is an item in the Drive scope, with its own blake3.
        let mut q = db::ListQuery {
            scope: Some("drive".into()),
            film_folders: lib.cfg.film_folders.clone(),
            ..Default::default()
        };
        assert_eq!(
            lib.db.with(|c| db::list_items(c, &q)).unwrap().items.len(),
            5
        );
        q.scope = Some("gallery".into());
        assert_eq!(
            lib.db.with(|c| db::list_items(c, &q)).unwrap().items.len(),
            0
        );
        let hash: String = lib
            .db
            .with(|c| c.query_row("SELECT i.hash FROM items i JOIN drive_files df ON df.item_id = i.id WHERE df.path = 'src/main.rs'", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(
            hash,
            blake3::hash(b"fn main() { println!(\"trace\"); }")
                .to_hex()
                .to_string()
        );

        // A second scan only looks.
        let s = index_drive_folder(&lib, "Tracer", &root).unwrap();
        assert_eq!((s.added, s.kept), (0, 5));

        // Browse the root: folders with recursive counts, then files.
        let b = lib
            .db
            .with(|c| {
                Ok(browse(
                    c,
                    &BrowseQuery {
                        folder: "Tracer".into(),
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .unwrap();
        let names: Vec<_> = b
            .folders
            .iter()
            .map(|f| (f.name.as_str(), f.files))
            .collect();
        assert_eq!(names, vec![("data", 2), ("docs", 1), ("src", 1)]);
        assert_eq!(b.files.len(), 1);
        assert_eq!(b.files[0].1.name, "README.md");
        assert_eq!(b.files[0].1.language.as_deref(), Some("Markdown"));
        let b = lib
            .db
            .with(|c| {
                Ok(browse(
                    c,
                    &BrowseQuery {
                        folder: "Tracer".into(),
                        path: "data".into(),
                        sort: Some("size".into()),
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(
            b.breadcrumbs,
            vec![("Tracer".into(), "".into()), ("data".into(), "data".into())]
        );
        assert_eq!(
            b.files
                .iter()
                .map(|f| f.1.name.as_str())
                .collect::<Vec<_>>(),
            vec!["run1.csv", "blob.bin"]
        );
        for bad in ["..", "../x", "/etc", "data/../.."] {
            assert!(matches!(
                lib.db
                    .with(|c| Ok(browse(
                        c,
                        &BrowseQuery {
                            folder: "Tracer".into(),
                            path: bad.into(),
                            ..Default::default()
                        }
                    )))
                    .unwrap(),
                Err(BrowseError::BadPath)
            ));
        }
        // Search by folder name, recursive.
        let b = lib
            .db
            .with(|c| {
                Ok(browse(
                    c,
                    &BrowseQuery {
                        folder: "Tracer".into(),
                        q: Some("docs".into()),
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(
            b.files
                .iter()
                .map(|f| f.1.path.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/paper.pdf"]
        );
        let b = lib
            .db
            .with(|c| {
                Ok(browse(
                    c,
                    &BrowseQuery {
                        folder: "Tracer".into(),
                        recursive: Some("1".into()),
                        doc_type: Some("code".into()),
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(b.files.len(), 1);
    }

    #[test]
    fn worker_indexes_content_for_search() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Lezioni");
        write(
            &root.join("Digital Forensics/notes.md"),
            b"Lecture 5: file carving with scalpel",
        );
        write(
            &root.join("Digital Forensics/blob.dat"),
            b"plain words in an unknown file",
        );
        write(&root.join("Other/x.bin"), &[0, 159, 146, 150]);
        let (_t, lib) = lib_with(&[("Lezioni", &root)]);
        index_drive_folder(&lib, "Lezioni", &root).unwrap();
        while let Some(p) = lib.db.with(|c| next_pending(c)).unwrap() {
            let (state, page) = work_one(&lib, &p);
            assert!(!page);
            lib.db
                .with(|c| {
                    c.execute(
                        "UPDATE drive_files SET text_state = ?2 WHERE id = ?1",
                        params![p.row, state],
                    )
                })
                .unwrap();
        }
        assert_eq!(
            count(
                &lib,
                "SELECT COUNT(*) FROM drive_files WHERE text_state = 'ready'"
            ),
            2
        );
        assert_eq!(
            count(
                &lib,
                "SELECT COUNT(*) FROM drive_files WHERE text_state = 'none'"
            ),
            1
        );
        let q = db::ListQuery {
            scope: Some("drive".into()),
            q: Some("scalpel".into()),
            film_folders: lib.cfg.film_folders.clone(),
            ..Default::default()
        };
        let page = lib.db.with(|c| db::list_items(c, &q)).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "notes.md");
        let info = lib
            .db
            .with(|c| info_for(c, &page.items[0].id))
            .unwrap()
            .unwrap();
        assert_eq!(
            snippet_for(&lib, &page.items[0].id, &info, &["scalpel".into()]).as_deref(),
            Some("Lecture 5: file carving with scalpel")
        );
        // The unknown file whose bytes are text previews as text.
        let b = lib
            .db
            .with(|c| {
                Ok(browse(
                    c,
                    &BrowseQuery {
                        folder: "Lezioni".into(),
                        path: "Digital Forensics".into(),
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .unwrap();
        let blob = b.files.iter().find(|f| f.1.name == "blob.dat").unwrap();
        assert_eq!(
            (blob.1.doc_type.as_str(), blob.1.preview.as_str()),
            ("other", "text")
        );
    }

    #[test]
    fn prune_only_after_a_clean_full_scan() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Drive");
        write(&root.join("a.txt"), b"a");
        write(&root.join("sub/b.txt"), b"b");
        let (_t, lib) = lib_with(&[("Drive", &root)]);
        assert_eq!(index_drive_folder(&lib, "Drive", &root).unwrap().added, 2);
        let live =
            |lib: &Library| count(lib, "SELECT COUNT(*) FROM items WHERE trashed_at IS NULL");

        // The disk is unmounted: the folder is missing. Nothing is trashed.
        std::fs::rename(&root, d.path().join("away")).unwrap();
        let s = index_drive_folder(&lib, "Drive", &root).unwrap();
        assert_eq!(s.pruned, 0);
        assert_eq!(live(&lib), 2);
        // An empty mountpoint: the folder exists but holds nothing. Nothing is trashed.
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(index_drive_folder(&lib, "Drive", &root).unwrap().pruned, 0);
        assert_eq!(live(&lib), 2);
        std::fs::remove_dir(&root).unwrap();
        std::fs::rename(d.path().join("away"), &root).unwrap();

        // The disk is unmounted and something wrote a stray file into the empty mountpoint (another
        // filesystem): the scan sees one file, cleanly, and still trashes nothing.
        lib.db
            .with(|c| c.execute("UPDATE drive_folders SET dev = dev + 1", []))
            .unwrap();
        std::fs::rename(&root, d.path().join("away")).unwrap();
        write(&root.join("stray.txt"), b"written while unmounted");
        let s = index_drive_folder(&lib, "Drive", &root).unwrap();
        assert_eq!((s.added, s.pruned), (1, 0));
        assert_eq!(live(&lib), 3);
        // The disk is back with its own filesystem: the stray row goes, the real files stay.
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::rename(d.path().join("away"), &root).unwrap();
        lib.db
            .with(|c| c.execute("UPDATE drive_folders SET dev = dev - 1", []))
            .unwrap();
        let s = index_drive_folder(&lib, "Drive", &root).unwrap();
        assert_eq!((s.pruned, s.kept), (1, 2), "{s:?}");
        assert_eq!(live(&lib), 2);
        // The Drive copied to a new disk: every known file is seen again on another filesystem,
        // which is adopted; only then can a later scan prune.
        lib.db
            .with(|c| c.execute("UPDATE drive_folders SET dev = dev + 7", []))
            .unwrap();
        assert_eq!(index_drive_folder(&lib, "Drive", &root).unwrap().pruned, 0);
        let recorded: i64 = lib
            .db
            .with(|c| c.query_row("SELECT dev FROM drive_folders", [], |r| r.get(0)))
            .unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(recorded, std::fs::metadata(&root).unwrap().dev() as i64);

        // A file really goes: its row is trashed, the other stays; the file system is untouched.
        std::fs::remove_file(root.join("sub/b.txt")).unwrap();
        let s = index_drive_folder(&lib, "Drive", &root).unwrap();
        assert_eq!((s.pruned, s.kept), (1, 1));
        assert_eq!(live(&lib), 1);
        assert!(root.join("a.txt").is_file());
        // It comes back: the same item returns.
        write(&root.join("sub/b.txt"), b"b");
        assert_eq!(index_drive_folder(&lib, "Drive", &root).unwrap().added, 1);
        assert_eq!(live(&lib), 2);
        // a, b and the stray file (trashed); b came back as the same item.
        assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 3);
    }

    #[test]
    fn a_changed_file_is_rehashed_and_reindexed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Drive");
        write(&root.join("n.txt"), b"first version");
        let (_t, lib) = lib_with(&[("Drive", &root)]);
        index_drive_folder(&lib, "Drive", &root).unwrap();
        let id: String = lib
            .db
            .with(|c| c.query_row("SELECT item_id FROM drive_files", [], |r| r.get(0)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write(&root.join("n.txt"), b"second version, longer");
        assert_eq!(index_drive_folder(&lib, "Drive", &root).unwrap().added, 1);
        let (id2, hash, state): (String, String, String) = lib
            .db
            .with(|c| c.query_row("SELECT df.item_id, i.hash, df.text_state FROM drive_files df JOIN items i ON i.id = df.item_id", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))
            .unwrap();
        assert_eq!(id, id2, "same path, same item");
        assert_eq!(
            hash,
            blake3::hash(b"second version, longer").to_hex().to_string()
        );
        assert_eq!(state, "pending");
    }
}

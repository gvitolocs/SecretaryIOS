//! The archive manifest: one JSON line per converted file, kept on the big disk next to the store.
//!
//! `archive-jxl --apply --manifest <path>` appends a line for every file it processes (converted,
//! deleted, or failed); `archive-jxl --export-manifest <path>` writes the complete manifest from
//! the catalog (`archive_blobs` and `items`), read-only. The export is the authority: the lines an
//! interrupted run did not get to write are all in it.
//!
//! A line:
//!
//! ```json
//! {"item_id":"it_…","device":"dev1","asset_id":"dev1/IMG_1","kind":"jpeg",
//!  "original_rel_path":"objects/ab/cd/<hash>.jpg","original_bytes":123,
//!  "original_blake3":"<hash>","original_sha256":"<hex>",
//!  "jxl_rel_path":"archive-jxl/ab/cd/<hash>.jxl","jxl_bytes":100,"jxl_blake3":"<hex>",
//!  "verified":true,"verified_at":"2026-…Z","original_deleted_at":null}
//! ```
//!
//! Paths are relative to the store root, never absolute. A failure has `"verified":false` and an
//! `"error"` (a reason code and a message, no paths); the original is untouched. `kind` is `jpeg`
//! or `png`; a PNG line also has `pixel_sha256`, the digest of the original's pixels, layout and
//! profile. A blob shared by several items gets one line per item.

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::archive_jxl::{archive_row, jxl_rel, ArchiveRow, KIND_PNG};
use crate::config::Config;
use crate::ingest::Library;
use crate::store;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub item_id: Option<String>,
    pub device: Option<String>,
    pub asset_id: Option<String>,
    pub kind: String,
    pub original_rel_path: String,
    pub original_bytes: u64,
    pub original_blake3: String,
    pub original_sha256: Option<String>,
    pub jxl_rel_path: Option<String>,
    pub jxl_bytes: Option<u64>,
    pub jxl_blake3: Option<String>,
    pub verified: bool,
    pub verified_at: Option<String>,
    pub original_deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_sha256: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ItemRef {
    item_id: Option<String>,
    device: Option<String>,
    asset_id: Option<String>,
}

/// The items that use a blob (current items first, then items that only hold it as an older
/// version); one empty reference when the catalog has none.
fn item_refs(c: &Connection, hash: &str) -> rusqlite::Result<Vec<ItemRef>> {
    let q = |sql: &str| -> rusqlite::Result<Vec<ItemRef>> {
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([hash], |r| {
            Ok(ItemRef {
                item_id: r.get(0)?,
                device: r.get(1)?,
                asset_id: r.get(2)?,
            })
        })?;
        rows.collect()
    };
    let mut out =
        q("SELECT id, device, asset_id FROM items WHERE hash = ?1 ORDER BY added_at, id")?;
    if out.is_empty() {
        out = q(
            "SELECT DISTINCT i.id, i.device, i.asset_id FROM item_versions v
             JOIN items i ON i.id = v.item_id WHERE v.hash = ?1 ORDER BY i.added_at, i.id",
        )?;
    }
    if out.is_empty() {
        out.push(ItemRef::default());
    }
    Ok(out)
}

fn rel(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

fn kind_name(row_kind: &str) -> &'static str {
    if row_kind == KIND_PNG {
        "png"
    } else {
        "jpeg"
    }
}

fn entry(item: ItemRef, row: &ArchiveRow, verified: bool, error: Option<String>) -> ManifestEntry {
    ManifestEntry {
        item_id: item.item_id,
        device: item.device,
        asset_id: item.asset_id,
        kind: kind_name(&row.kind).to_string(),
        original_rel_path: rel(store::object_rel(&row.hash, &row.original_ext)),
        original_bytes: row.original_bytes as u64,
        original_blake3: row.hash.clone(),
        original_sha256: row.original_sha256.clone(),
        jxl_rel_path: Some(rel(jxl_rel(&row.hash))),
        jxl_bytes: Some(row.jxl_bytes as u64),
        jxl_blake3: Some(row.jxl_hash.clone()),
        verified,
        verified_at: verified.then(|| row.verified_at.clone()),
        original_deleted_at: row.original_deleted_at.clone(),
        error,
        pixel_sha256: row.pixel_digest.clone(),
    }
}

/// The manifest a run appends to, and its failures file.
pub struct Manifest {
    file: std::fs::File,
    failures_path: PathBuf,
    failures: Option<std::fs::File>,
    unsynced: usize,
}

/// Lines between two fsyncs (the run also syncs when it ends).
const SYNC_EVERY: usize = 50;

fn append(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

impl Manifest {
    pub fn open(path: &Path) -> std::io::Result<Manifest> {
        let mut failures = path.as_os_str().to_owned();
        failures.push(".failures.jsonl");
        Ok(Manifest {
            file: append(path)?,
            failures_path: PathBuf::from(failures),
            failures: None,
            unsynced: 0,
        })
    }

    fn write(&mut self, e: &ManifestEntry) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(e)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        if e.error.is_some() {
            if self.failures.is_none() {
                self.failures = Some(append(&self.failures_path)?);
            }
            if let Some(f) = self.failures.as_mut() {
                f.write_all(line.as_bytes())?;
            }
        }
        self.unsynced += 1;
        if self.unsynced >= SYNC_EVERY {
            self.sync()?;
        }
        Ok(())
    }

    /// A converted (or deleted) file: one line per item that uses the blob.
    pub fn record_success(&mut self, lib: &Library, row: &ArchiveRow) -> anyhow::Result<usize> {
        let items = lib.db.with(|c| item_refs(c, &row.hash))?;
        for item in &items {
            self.write(&entry(item.clone(), row, true, None))?;
        }
        Ok(items.len())
    }

    /// A file that failed verification or conversion; its original stays. `row` is the catalog row
    /// when an earlier run had converted it.
    pub fn record_failure(
        &mut self,
        lib: &Library,
        hash: &str,
        ext: &str,
        bytes: i64,
        row: Option<&ArchiveRow>,
        error: &str,
    ) -> anyhow::Result<usize> {
        let items = lib.db.with(|c| item_refs(c, hash))?;
        for item in &items {
            let e = match row {
                Some(r) => entry(item.clone(), r, false, Some(error.to_string())),
                None => ManifestEntry {
                    item_id: item.item_id.clone(),
                    device: item.device.clone(),
                    asset_id: item.asset_id.clone(),
                    kind: if ext.eq_ignore_ascii_case("png") {
                        "png"
                    } else {
                        "jpeg"
                    }
                    .to_string(),
                    original_rel_path: rel(store::object_rel(hash, ext)),
                    original_bytes: bytes as u64,
                    original_blake3: hash.to_string(),
                    original_sha256: None,
                    jxl_rel_path: None,
                    jxl_bytes: None,
                    jxl_blake3: None,
                    verified: false,
                    verified_at: None,
                    original_deleted_at: None,
                    error: Some(error.to_string()),
                    pixel_sha256: None,
                },
            };
            self.write(&e)?;
        }
        Ok(items.len())
    }

    pub fn sync(&mut self) -> anyhow::Result<()> {
        self.file.flush()?;
        self.file.sync_all()?;
        if let Some(f) = self.failures.as_mut() {
            f.flush()?;
            f.sync_all()?;
        }
        self.unsynced = 0;
        Ok(())
    }
}

impl Drop for Manifest {
    fn drop(&mut self) {
        let _ = self.sync();
    }
}

/// The complete manifest from the catalog, written to `out` (replacing it) from a read-only
/// connection. Returns the number of lines.
pub fn export(cfg: &Config, out: &Path) -> anyhow::Result<usize> {
    let c = crate::archive_jxl_measure::open_readonly(&cfg.db_path())?;
    let have: bool = c.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'archive_blobs'",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        have,
        "the catalog has no archive_blobs table yet (nothing was ever archived)"
    );
    let hashes: Vec<String> = {
        let mut st = c.prepare("SELECT hash FROM archive_blobs ORDER BY hash")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut text = String::new();
    let mut lines = 0;
    for hash in hashes {
        let Some(row) = archive_row(&c, &hash)? else {
            continue;
        };
        for item in item_refs(&c, &hash)? {
            text.push_str(&serde_json::to_string(&entry(item, &row, true, None))?);
            text.push('\n');
            lines += 1;
        }
    }
    crate::archive_jxl::write_durable(out, text.as_bytes())?;
    Ok(lines)
}

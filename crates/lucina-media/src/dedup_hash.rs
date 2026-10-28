//! Perceptual hashes in the background, so the duplicate review (`dedup_review.rs`) and the stack
//! suggestions (`stacks.rs`) see new photos without anyone running `dedup-hash`.
//!
//! A step of the single background thread (`smart.rs`, `embed_loop`), first in its round because a
//! hash costs a few milliseconds: up to [`BATCH`] photos without a current hash (none, or one of
//! older bytes), read from the 360 px grid JPEG (`s.jpg`, never a store original), written in one
//! small transaction to `item_hashes` (`dedup::HASH_SCHEMA`), the table `dedup-hash` fills. The
//! catalog lock is held only for the query and the write, not while hashing.

use std::collections::HashSet;

use rusqlite::{params, Connection};

use crate::ingest::Library;
use crate::{dedup, phash};

pub const BATCH: usize = 32;

/// Photos `dedup::hash_missing` would hash: the Gallery's photos (no Drive files), not trashed or
/// hidden, with a ready rendition and no hash of their current bytes. `skip` (JSON array of ids):
/// pictures that failed in this process, so they do not come back in every batch.
const PENDING_WHERE: &str = "i.kind = 'photo' AND i.trashed_at IS NULL AND i.hidden = 0 \
     AND i.thumb_state = 'ready' \
     AND NOT EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = i.id) \
     AND NOT EXISTS (SELECT 1 FROM item_hashes h WHERE h.item_id = i.id AND h.content_hash = i.hash) \
     AND i.id NOT IN (SELECT value FROM json_each(?1))";

fn skip_json(skip: &HashSet<String>) -> String {
    serde_json::to_string(&skip.iter().collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
}

fn pending_batch(
    c: &Connection,
    skip: &HashSet<String>,
    n: usize,
) -> rusqlite::Result<Vec<(String, String)>> {
    c.execute_batch(dedup::HASH_SCHEMA)?;
    let mut st = c.prepare(&format!(
        "SELECT i.id, i.hash FROM items i WHERE {PENDING_WHERE} ORDER BY i.sort_at DESC, i.id DESC LIMIT ?2"
    ))?;
    let rows = st.query_map(params![skip_json(skip), n as i64], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    rows.collect()
}

/// Photos still waiting for a hash (for status lines and tests).
pub fn pending_count(c: &Connection) -> rusqlite::Result<i64> {
    c.execute_batch(dedup::HASH_SCHEMA)?;
    c.query_row(
        &format!("SELECT COUNT(*) FROM items i WHERE {PENDING_WHERE}"),
        ["[]"],
        |r| r.get(0),
    )
}

/// Hashes up to [`BATCH`] pending photos; how many were looked at (0 = nothing pending). A
/// picture that cannot be read goes into `skip` and is left alone until the service restarts.
pub fn hash_pending(lib: &Library, skip: &mut HashSet<String>) -> anyhow::Result<usize> {
    let s2 = skip.clone();
    let batch = lib.db.with(|c| pending_batch(c, &s2, BATCH))?;
    if batch.is_empty() {
        return Ok(0);
    }
    let mut done = Vec::new();
    for (id, content_hash) in &batch {
        let path = crate::thumbs::dir_for(&lib.cfg, id).join("s.jpg");
        match phash::hash_file(&path) {
            Ok(h) => done.push((id.clone(), content_hash.clone(), h)),
            Err(e) => {
                tracing::debug!(item = %id, error = %e, "perceptual hash failed");
                skip.insert(id.clone());
            }
        }
    }
    let now = crate::db::now_str();
    lib.db.with(|c| {
        let tx = c.transaction()?;
        for (id, content_hash, h) in &done {
            tx.execute(
                "INSERT OR REPLACE INTO item_hashes (item_id, content_hash, phash, dhash, thumb_w, thumb_h, flat, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    content_hash,
                    h.phash as i64,
                    h.dhash as i64,
                    h.width,
                    h.height,
                    h.flat as i64,
                    now
                ],
            )?;
        }
        tx.commit()
    })?;
    Ok(batch.len())
}

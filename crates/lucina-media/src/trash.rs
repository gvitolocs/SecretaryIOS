//! The Trash: how long an item stays, and the one function that deletes for good.
//!
//! Deleting in Lucina is a soft delete (`trashed_at`); the original and the catalog row stay for
//! `LUCINA_MEDIA_TRASH_DAYS` days (30; 0 keeps them forever). Purging is the only permanent
//! deletion and it only ever acts on items that are already in the trash: the batch op `purge` and
//! the hourly retention task both go through [`purge_in`]. An original's file is removed only when
//! no item and no version row still references its hash, and only when the library owns the file
//! (store or spool): files another service owns (`external`) are never touched.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::config::Config;
use crate::db::BlobLoc;
use crate::ingest::Library;
use crate::{store, thumbs};

/// How often the retention task looks for expired items.
pub const RETENTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// When an item trashed at `trashed_at` is purged by the retention task: `None` when the Trash
/// is kept forever (`days == 0`) or the date does not parse.
pub fn trash_expiry(trashed_at: &str, days: u32) -> Option<String> {
    if days == 0 {
        return None;
    }
    let t = DateTime::parse_from_rfc3339(trashed_at).ok()?;
    let at = t.with_timezone(&Utc) + Duration::days(days as i64);
    Some(at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Items trashed before this instant are expired. `None` when the Trash is kept forever. In the
/// catalog's own date format, so it compares as a string with `trashed_at`.
pub fn retention_cutoff(now: DateTime<Utc>, days: u32) -> Option<String> {
    (days > 0)
        .then(|| (now - Duration::days(days as i64)).to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// The ids trashed before `cutoff`, oldest first.
pub fn expired_ids(c: &Connection, cutoff: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(
        "SELECT id FROM items WHERE trashed_at IS NOT NULL AND trashed_at < ?1 ORDER BY trashed_at, id",
    )?;
    let rows = st.query_map([cutoff], |r| r.get(0))?;
    rows.collect()
}

/// What a purge did. The originals in `orphans` are not referenced any more; the caller removes
/// their files after the catalog change is committed ([`finish_purge`]).
#[derive(Debug, Default)]
pub struct PurgeReport {
    pub done: Vec<String>,
    pub failed: Vec<(String, &'static str)>,
    pub orphans: Vec<BlobLoc>,
}

/// Delete trashed items for good, inside the caller's transaction. An id that is not an item
/// fails with `not_found`, one that is not in the trash with `not_trashed` (nothing of it is
/// touched). Removes the item row and its versions, album links, externals, Drive row, search
/// text and uploads record; a Live Photo partner keeps its pair link cleared. Device deletion
/// requests stay queued: they are for the device, not for the catalog.
pub fn purge_in(c: &Connection, ids: &[String]) -> rusqlite::Result<PurgeReport> {
    let mut report = PurgeReport::default();
    let mut hashes: BTreeSet<String> = BTreeSet::new();
    for id in ids {
        let row: Option<(String, Option<String>)> = c
            .query_row(
                "SELECT hash, trashed_at FROM items WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((hash, trashed_at)) = row else {
            report.failed.push((id.clone(), "not_found"));
            continue;
        };
        if trashed_at.is_none() {
            report.failed.push((id.clone(), "not_trashed"));
            continue;
        }
        hashes.insert(hash);
        {
            let mut st = c.prepare_cached("SELECT hash FROM item_versions WHERE item_id = ?1")?;
            let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
            for h in rows {
                hashes.insert(h?);
            }
        }
        // The Drive's content index is keyed by the drive_files row.
        if let Some(row) = c
            .query_row("SELECT id FROM drive_files WHERE item_id = ?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            c.execute("DELETE FROM drive_fts WHERE rowid = ?1", [row])?;
            c.execute("DELETE FROM drive_files WHERE id = ?1", [row])?;
        }
        c.execute("DELETE FROM items_fts WHERE item_id = ?1", [id])?;
        c.execute("DELETE FROM album_items WHERE item_id = ?1", [id])?;
        c.execute(
            "UPDATE albums SET cover_item = NULL WHERE cover_item = ?1",
            [id],
        )?;
        c.execute("DELETE FROM item_versions WHERE item_id = ?1", [id])?;
        c.execute("DELETE FROM item_edits WHERE item_id = ?1", [id])?;
        c.execute("DELETE FROM externals WHERE item_id = ?1", [id])?;
        c.execute("DELETE FROM uploads WHERE item_id = ?1", [id])?;
        c.execute(
            "UPDATE items SET live_pair = NULL WHERE live_pair = ?1",
            [id],
        )?;
        c.execute("DELETE FROM items WHERE id = ?1", [id])?;
        report.done.push(id.clone());
    }
    // A blob goes when nothing references its hash any more (the same bytes may be another item,
    // or an older version of one).
    for hash in hashes {
        let used: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM items WHERE hash = ?1) OR EXISTS (SELECT 1 FROM item_versions WHERE hash = ?1)",
            [&hash],
            |r| r.get(0),
        )?;
        if used {
            continue;
        }
        if let Some(b) = crate::db::blob(c, &hash)? {
            c.execute("DELETE FROM blobs WHERE hash = ?1", params![hash])?;
            if matches!(b.location.as_str(), "store" | "spool") {
                report.orphans.push(b);
            }
        }
    }
    Ok(report)
}

/// Purge in one transaction of its own.
pub fn purge_trashed(c: &mut Connection, ids: &[String]) -> rusqlite::Result<PurgeReport> {
    let tx = c.transaction()?;
    let report = purge_in(&tx, ids)?;
    tx.commit()?;
    Ok(report)
}

/// After the catalog change is committed: remove the originals nothing references and the purged
/// items' thumbnails. A file that cannot be removed is logged and left (an orphan costs disk, never
/// data). Returns how many original files went.
pub fn finish_purge(cfg: &Config, report: &PurgeReport) -> usize {
    let mut removed = 0;
    for b in &report.orphans {
        let path = store::path_of(cfg, b);
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "purge: an original could not be removed"),
        }
    }
    for id in &report.done {
        let _ = std::fs::remove_dir_all(thumbs::dir_for(cfg, id));
    }
    removed
}

/// Purge everything trashed longer ago than the retention allows at `now`. Returns how many items
/// went. Blocking.
pub fn purge_expired(lib: &Library, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let Some(cutoff) = retention_cutoff(now, lib.cfg.trash_days) else {
        return Ok(0);
    };
    let report = lib.db.with(|c| {
        let ids = expired_ids(c, &cutoff)?;
        purge_trashed(c, &ids)
    })?;
    finish_purge(&lib.cfg, &report);
    for id in &report.done {
        lib.emit("item.purged", id);
    }
    Ok(report.done.len())
}

/// Once an hour: purge what has been in the Trash too long. Logs a count, never names.
pub async fn retention_worker(lib: Library) {
    loop {
        let l = lib.clone();
        match tokio::task::spawn_blocking(move || purge_expired(&l, Utc::now())).await {
            Ok(Ok(n)) if n > 0 => tracing::info!(purged = n, "trash retention"),
            Ok(Err(e)) => tracing::warn!(error = %e, "trash retention failed"),
            _ => {}
        }
        tokio::time::sleep(RETENTION_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn an_item_expires_trash_days_after_it_was_trashed() {
        assert_eq!(
            trash_expiry("2026-10-01T10:00:00.000Z", 30).as_deref(),
            Some("2026-10-31T10:00:00.000Z")
        );
        assert_eq!(
            trash_expiry("2026-12-15T23:59:59.500Z", 30).as_deref(),
            Some("2027-01-14T23:59:59.500Z")
        );
        // 0 keeps the Trash forever; a date that does not parse has no expiry.
        assert_eq!(trash_expiry("2026-10-01T10:00:00.000Z", 0), None);
        assert_eq!(trash_expiry("yesterday", 30), None);
    }

    #[test]
    fn the_retention_cutoff_is_days_before_now() {
        let now = at("2026-10-28T12:00:00Z");
        assert_eq!(
            retention_cutoff(now, 30).as_deref(),
            Some("2026-09-28T12:00:00.000Z")
        );
        assert_eq!(
            retention_cutoff(now, 1).as_deref(),
            Some("2026-10-27T12:00:00.000Z")
        );
        assert_eq!(retention_cutoff(now, 0), None);
    }

    #[test]
    fn the_cutoff_compares_as_a_string_with_trashed_at() {
        let cutoff = retention_cutoff(at("2026-10-28T12:00:00Z"), 1).unwrap();
        // Exactly one day old is not yet expired; a millisecond older is.
        assert!("2026-10-27T12:00:00.000Z" >= cutoff.as_str());
        assert!("2026-10-27T11:59:59.999Z" < cutoff.as_str());
    }
}

//! Device deletion requests: the only way Lucina ever asks a device to delete something.
//!
//! Deleting in Lucina never deletes the original on the device. When the owner ticks "also delete
//! from <device>", the item's asset is queued here (`device_deletions`); the device app fetches the
//! queue, asks the system to delete those assets (PhotoKit shows its own confirmation) and acks
//! the ones that are gone. Nothing here touches a device by itself.

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::db::now_str;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Pending {
    pub asset_id: String,
    pub item_id: String,
    pub requested_at: String,
}

/// Queue the deletion of one device asset. A request that was already done is queued again (the
/// item came back and went away again); one still pending just refreshes.
pub fn queue(c: &Connection, device: &str, asset_id: &str, item_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO device_deletions (device, asset_id, item_id, requested_at, done_at) VALUES (?1, ?2, ?3, ?4, NULL) \
         ON CONFLICT(device, asset_id) DO UPDATE SET item_id = excluded.item_id, requested_at = excluded.requested_at, done_at = NULL",
        params![device, asset_id, item_id, now_str()],
    )?;
    Ok(())
}

/// What the device still has to delete, oldest request first.
pub fn pending(c: &Connection, device: &str) -> rusqlite::Result<Vec<Pending>> {
    let mut st = c.prepare(
        "SELECT asset_id, item_id, requested_at FROM device_deletions \
         WHERE device = ?1 AND done_at IS NULL ORDER BY requested_at, asset_id",
    )?;
    let rows = st.query_map([device], |r| {
        Ok(Pending {
            asset_id: r.get(0)?,
            item_id: r.get(1)?,
            requested_at: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// Mark pending requests done; returns how many were. Assets that were not pending are ignored.
pub fn ack(c: &mut Connection, device: &str, asset_ids: &[String]) -> rusqlite::Result<usize> {
    let tx = c.transaction()?;
    let now = now_str();
    let mut n = 0;
    for a in asset_ids {
        n += tx.execute(
            "UPDATE device_deletions SET done_at = ?3 WHERE device = ?1 AND asset_id = ?2 AND done_at IS NULL",
            params![device, a, now],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

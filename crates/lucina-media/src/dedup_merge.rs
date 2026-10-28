//! Merging duplicates: hiding one item under another without losing anything.
//!
//! A merge hides the duplicate (`items.hidden = 2`, so every listing, count, share and search that
//! already skips hidden items skips it) and records `item_links(item_id = duplicate, duplicate_of =
//! survivor)`. Nothing is deleted:
//!
//! - the duplicate's row and bytes stay, as an alternate rendition of the survivor (`GET
//!   items/{id}/duplicates`, `GET items/{dup}/original` still work);
//! - its album memberships, favourite flag and cover roles move to the survivor;
//! - its device asset id (`device`, `asset_id`) becomes an alias row pointing at the survivor, so the
//!   phone uploading that asset again (same bytes: a duplicate; edited bytes: a new version) lands on
//!   the survivor, and `ingest` resolves a content hash that matches a hidden duplicate to the survivor
//!   (`resolve`, `asset_lookup`);
//! - `undo` puts all of that back.
//!
//! `GET items/{id}` of a hidden duplicate answers with the survivor (`merged_from` names the id asked).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{self, now_str};
use crate::dedup::{Class, Report};
use crate::http::{ApiError, ApiResult};
use crate::ingest::Library;

/// `items.hidden` of a merged duplicate (1 is a Live Photo's motion part).
pub const HIDDEN_DUPLICATE: i64 = 2;

/// The schema, idempotent: called from `db::migrate`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        -- A duplicate hidden under its survivor. Rows stay after an undo (`undone_at`), so a pair the owner
        -- split is not proposed again.
        CREATE TABLE IF NOT EXISTS item_links (
            item_id TEXT PRIMARY KEY,
            duplicate_of TEXT NOT NULL,
            kind TEXT NOT NULL,
            score REAL NOT NULL,
            decided_by TEXT NOT NULL,
            created_at TEXT NOT NULL,
            undone_at TEXT,
            undo TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS item_links_survivor ON item_links(duplicate_of);
        -- A device asset that now belongs to a survivor: `hash` is what the device last sent for it.
        CREATE TABLE IF NOT EXISTS item_aliases (
            device TEXT NOT NULL,
            asset_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            from_item TEXT NOT NULL,
            hash TEXT NOT NULL,
            PRIMARY KEY (device, asset_id)
        );
        CREATE INDEX IF NOT EXISTS item_aliases_item ON item_aliases(item_id);
        ",
    )?;
    conn.execute_batch(crate::dedup::HASH_SCHEMA)
}

/// The item an id stands for now: the survivor when `id` is a hidden duplicate, else `id`.
pub fn resolve(c: &Connection, id: &str) -> rusqlite::Result<String> {
    Ok(c.query_row(
        "SELECT duplicate_of FROM item_links WHERE item_id = ?1 AND undone_at IS NULL",
        [id],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or_else(|| id.to_string()))
}

/// The survivor a device asset was merged into: `(item id, the hash to compare an upload with)`.
/// An upload of the bytes the device sent before (or of the survivor's current bytes) is unchanged
/// (the answer is `incoming`, so ingest calls it a duplicate); anything else is an edit.
pub fn asset_lookup(
    c: &Connection,
    device: &str,
    asset_id: &str,
    incoming: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    c.query_row(
        "SELECT a.item_id, a.hash, s.hash FROM item_aliases a JOIN items s ON s.id = a.item_id
         WHERE a.device = ?1 AND a.asset_id = ?2",
        params![device, asset_id],
        |r| {
            let (item, sent, current): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let seen = if incoming == sent || incoming == current {
                incoming.to_string()
            } else {
                sent
            };
            Ok((item, seen))
        },
    )
    .optional()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Missing,
    SameItem,
    NotPhoto,
    Trashed,
    /// Already hidden (a duplicate, or a Live Photo's motion part).
    Hidden,
    /// The owner undid this merge before.
    UndoneBefore,
    /// A file of a Drive folder: it stays where the owner put it.
    DriveFile,
}

impl Skip {
    pub fn as_str(self) -> &'static str {
        match self {
            Skip::Missing => "missing",
            Skip::SameItem => "same_item",
            Skip::NotPhoto => "not_photo",
            Skip::Trashed => "trashed",
            Skip::Hidden => "hidden",
            Skip::UndoneBefore => "undone_before",
            Skip::DriveFile => "drive_file",
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct AlbumMove {
    album: String,
    added_at: String,
    /// The survivor did not have it: undo takes it away again.
    moved: bool,
}

/// What a merge changed besides the link row, for `undo`.
#[derive(Default, Serialize, Deserialize)]
struct UndoInfo {
    albums: Vec<AlbumMove>,
    cover_albums: Vec<String>,
    was_favorite: bool,
    fav_added: bool,
    asset: Option<(String, String)>,
    survivor_device_set: Option<String>,
    live_pair: Option<String>,
    repointed_links: Vec<String>,
    repointed_aliases: Vec<(String, String)>,
}

struct Row {
    kind: String,
    trashed: bool,
    hidden: i64,
    favorite: bool,
    device: Option<String>,
    asset_id: Option<String>,
    hash: String,
    live_pair: Option<String>,
    drive: bool,
}

fn load(c: &Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        "SELECT kind, trashed_at IS NOT NULL, hidden, favorite, device, asset_id, hash, live_pair,
                EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = items.id)
         FROM items WHERE id = ?1",
        [id],
        |r| {
            Ok(Row {
                kind: r.get(0)?,
                trashed: r.get(1)?,
                hidden: r.get(2)?,
                favorite: r.get::<_, i64>(3)? != 0,
                device: r.get(4)?,
                asset_id: r.get(5)?,
                hash: r.get(6)?,
                live_pair: r.get(7)?,
                drive: r.get(8)?,
            })
        },
    )
    .optional()
}

/// Hide `dup` under `survivor`. `redo` allows a pair the owner undid. One transaction.
pub fn merge(
    c: &mut Connection,
    survivor: &str,
    dup: &str,
    kind: &str,
    score: f64,
    decided_by: &str,
    redo: bool,
) -> rusqlite::Result<Result<(), Skip>> {
    if survivor == dup {
        return Ok(Err(Skip::SameItem));
    }
    let tx = c.transaction()?;
    let (Some(s), Some(d)) = (load(&tx, survivor)?, load(&tx, dup)?) else {
        return Ok(Err(Skip::Missing));
    };
    if s.kind != "photo" || d.kind != "photo" {
        return Ok(Err(Skip::NotPhoto));
    }
    if s.trashed || d.trashed {
        return Ok(Err(Skip::Trashed));
    }
    if s.drive || d.drive {
        return Ok(Err(Skip::DriveFile));
    }
    if s.hidden != 0 || d.hidden != 0 {
        return Ok(Err(Skip::Hidden));
    }
    if !redo {
        let undone: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM item_links WHERE item_id = ?1 AND undone_at IS NOT NULL)",
            [dup],
            |r| r.get(0),
        )?;
        if undone {
            return Ok(Err(Skip::UndoneBefore));
        }
    }
    let now = now_str();
    let mut undo = UndoInfo::default();

    // Photos already hidden under the duplicate now hide under the survivor.
    {
        let mut st = tx.prepare(
            "SELECT item_id FROM item_links WHERE duplicate_of = ?1 AND undone_at IS NULL",
        )?;
        undo.repointed_links = st
            .query_map([dup], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut st = tx.prepare("SELECT device, asset_id FROM item_aliases WHERE item_id = ?1")?;
        undo.repointed_aliases = st
            .query_map([dup], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
    }
    tx.execute(
        "UPDATE item_links SET duplicate_of = ?2 WHERE duplicate_of = ?1 AND undone_at IS NULL",
        params![dup, survivor],
    )?;
    tx.execute(
        "UPDATE item_aliases SET item_id = ?2 WHERE item_id = ?1",
        params![dup, survivor],
    )?;

    // Albums (the system albums too: Camera roll, Favourites), and the albums the duplicate covers.
    let memberships: Vec<(String, String)> = {
        let mut st = tx.prepare("SELECT album_id, added_at FROM album_items WHERE item_id = ?1")?;
        let rows = st
            .query_map([dup], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
        rows
    };
    for (album, added_at) in memberships {
        let moved = tx.execute(
            "INSERT OR IGNORE INTO album_items (album_id, item_id, added_at) VALUES (?1, ?2, ?3)",
            params![album, survivor, added_at],
        )? > 0;
        undo.albums.push(AlbumMove {
            album,
            added_at,
            moved,
        });
    }
    tx.execute("DELETE FROM album_items WHERE item_id = ?1", [dup])?;
    {
        let mut st = tx.prepare("SELECT id FROM albums WHERE cover_item = ?1")?;
        undo.cover_albums = st
            .query_map([dup], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    tx.execute(
        "UPDATE albums SET cover_item = ?2 WHERE cover_item = ?1",
        params![dup, survivor],
    )?;

    // Favourite.
    undo.was_favorite = d.favorite;
    if d.favorite && !s.favorite {
        tx.execute("UPDATE items SET favorite = 1 WHERE id = ?1", [survivor])?;
        db::add_to_album(&tx, "sys-favorites", survivor)?;
        undo.fav_added = true;
    }

    // The device asset.
    if let (Some(dev), Some(asset)) = (&d.device, &d.asset_id) {
        tx.execute(
            "INSERT OR REPLACE INTO item_aliases (device, asset_id, item_id, from_item, hash) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![dev, asset, survivor, dup, d.hash],
        )?;
        tx.execute("UPDATE items SET asset_id = NULL WHERE id = ?1", [dup])?;
        undo.asset = Some((dev.clone(), asset.clone()));
    }
    // A survivor outside the Gallery (an old backup) takes the device of the duplicate, so a photo
    // the Gallery showed does not vanish from it.
    if s.device.is_none() {
        if let Some(dev) = &d.device {
            tx.execute(
                "UPDATE items SET device = ?2 WHERE id = ?1",
                params![survivor, dev],
            )?;
            undo.survivor_device_set = Some(dev.clone());
        }
    }
    // A Live Photo's motion part follows the picture, when the survivor has none of its own.
    if let (Some(motion), None) = (&d.live_pair, &s.live_pair) {
        tx.execute(
            "UPDATE items SET live_pair = ?2 WHERE id = ?1",
            params![motion, survivor],
        )?;
        tx.execute(
            "UPDATE items SET live_pair = ?2 WHERE id = ?1",
            params![survivor, motion],
        )?;
        tx.execute("UPDATE items SET live_pair = NULL WHERE id = ?1", [dup])?;
        undo.live_pair = Some(motion.clone());
    }

    tx.execute(
        "UPDATE items SET hidden = ?2, favorite = 0, updated_at = ?3 WHERE id = ?1",
        params![dup, HIDDEN_DUPLICATE, now],
    )?;
    tx.execute(
        "UPDATE items SET updated_at = ?2 WHERE id = ?1",
        params![survivor, now],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO item_links (item_id, duplicate_of, kind, score, decided_by, created_at, undone_at, undo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
        params![
            dup,
            survivor,
            kind,
            score,
            decided_by,
            now,
            serde_json::to_string(&undo).unwrap_or_else(|_| "{}".into())
        ],
    )?;
    db::reindex(&tx, survivor)?;
    tx.commit()?;
    Ok(Ok(()))
}

/// Put a merged duplicate back as its own item. `false`: it is not merged.
pub fn undo(c: &mut Connection, dup: &str) -> rusqlite::Result<bool> {
    let tx = c.transaction()?;
    let link: Option<(String, String)> = tx
        .query_row(
            "SELECT duplicate_of, undo FROM item_links WHERE item_id = ?1 AND undone_at IS NULL",
            [dup],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((survivor, raw)) = link else {
        return Ok(false);
    };
    let info: UndoInfo = serde_json::from_str(&raw).unwrap_or_default();
    let now = now_str();

    for id in &info.repointed_links {
        tx.execute(
            "UPDATE item_links SET duplicate_of = ?1 WHERE item_id = ?2 AND duplicate_of = ?3 AND undone_at IS NULL",
            params![dup, id, survivor],
        )?;
    }
    for (dev, asset) in &info.repointed_aliases {
        tx.execute(
            "UPDATE item_aliases SET item_id = ?1 WHERE device = ?2 AND asset_id = ?3",
            params![dup, dev, asset],
        )?;
    }
    for m in &info.albums {
        tx.execute(
            "INSERT OR IGNORE INTO album_items (album_id, item_id, added_at) VALUES (?1, ?2, ?3)",
            params![m.album, dup, m.added_at],
        )?;
        if m.moved {
            tx.execute(
                "DELETE FROM album_items WHERE album_id = ?1 AND item_id = ?2",
                params![m.album, survivor],
            )?;
        }
    }
    for album in &info.cover_albums {
        tx.execute(
            "UPDATE albums SET cover_item = ?2 WHERE id = ?1 AND cover_item = ?3",
            params![album, dup, survivor],
        )?;
    }
    if info.was_favorite {
        tx.execute("UPDATE items SET favorite = 1 WHERE id = ?1", [dup])?;
    }
    if info.fav_added {
        tx.execute("UPDATE items SET favorite = 0 WHERE id = ?1", [&survivor])?;
        tx.execute(
            "DELETE FROM album_items WHERE album_id = 'sys-favorites' AND item_id = ?1",
            [&survivor],
        )?;
    }
    if let Some((dev, asset)) = &info.asset {
        tx.execute(
            "DELETE FROM item_aliases WHERE device = ?1 AND asset_id = ?2 AND from_item = ?3",
            params![dev, asset, dup],
        )?;
        // The survivor may have recorded the asset itself meanwhile (a repeated upload).
        tx.execute(
            "UPDATE items SET asset_id = NULL WHERE id = ?1 AND device = ?2 AND asset_id = ?3",
            params![survivor, dev, asset],
        )?;
        tx.execute(
            "UPDATE items SET asset_id = ?2 WHERE id = ?1",
            params![dup, asset],
        )?;
    }
    if let Some(dev) = &info.survivor_device_set {
        tx.execute(
            "UPDATE items SET device = NULL WHERE id = ?1 AND device = ?2 AND asset_id IS NULL",
            params![survivor, dev],
        )?;
    }
    if let Some(motion) = &info.live_pair {
        tx.execute(
            "UPDATE items SET live_pair = ?2 WHERE id = ?1",
            params![motion, dup],
        )?;
        tx.execute(
            "UPDATE items SET live_pair = ?2 WHERE id = ?1",
            params![dup, motion],
        )?;
        tx.execute(
            "UPDATE items SET live_pair = NULL WHERE id = ?1 AND live_pair = ?2",
            params![survivor, motion],
        )?;
    }
    tx.execute(
        "UPDATE items SET hidden = 0, updated_at = ?2 WHERE id = ?1 AND hidden = ?3",
        params![dup, now, HIDDEN_DUPLICATE],
    )?;
    tx.execute(
        "UPDATE items SET updated_at = ?2 WHERE id = ?1",
        params![survivor, now],
    )?;
    tx.execute(
        "UPDATE item_links SET undone_at = ?2 WHERE item_id = ?1",
        params![dup, now],
    )?;
    db::reindex(&tx, &survivor)?;
    db::reindex(&tx, dup)?;
    tx.commit()?;
    Ok(true)
}

// ---- purge ---------------------------------------------------------------------------------

/// A purged photo takes the copies hidden under it along: they are the same photo. Returns `ids`
/// plus those copies, marked trashed with their survivor so the purge accepts them.
pub fn expand_purge(c: &Connection, ids: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut all: Vec<String> = ids.to_vec();
    for id in ids {
        let trashed_at: Option<String> = c
            .query_row("SELECT trashed_at FROM items WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let Some(trashed_at) = trashed_at else {
            continue;
        };
        let dups: Vec<String> = {
            let mut st = c.prepare_cached(
                "SELECT item_id FROM item_links WHERE duplicate_of = ?1 AND undone_at IS NULL",
            )?;
            let rows = st
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        for dup in dups {
            c.execute(
                "UPDATE items SET trashed_at = COALESCE(trashed_at, ?2) WHERE id = ?1",
                params![dup, trashed_at],
            )?;
            if !all.contains(&dup) {
                all.push(dup);
            }
        }
    }
    Ok(all)
}

/// A purged item leaves no link, alias or hash behind.
pub fn forget(c: &Connection, id: &str) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM item_links WHERE item_id = ?1 OR duplicate_of = ?1",
        [id],
    )?;
    c.execute(
        "DELETE FROM item_aliases WHERE item_id = ?1 OR from_item = ?1",
        [id],
    )?;
    c.execute("DELETE FROM item_hashes WHERE item_id = ?1", [id])?;
    Ok(())
}

// ---- applying a report ---------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ApplyOpts {
    pub min_score: f32,
    pub classes: Vec<Class>,
    pub limit: Option<usize>,
    pub redo: bool,
    pub dry_run: bool,
}

impl Default for ApplyOpts {
    fn default() -> Self {
        Self {
            min_score: 0.7,
            classes: vec![Class::Exact, Class::SameReencoded, Class::ResizedCopy],
            limit: None,
            redo: false,
            dry_run: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ApplyStats {
    pub planned: usize,
    pub applied: usize,
    pub below_score: usize,
    pub other_class: usize,
    pub skipped: std::collections::BTreeMap<String, usize>,
    pub duplicate_bytes: i64,
}

/// Apply the merges of a report that pass `opts`, in the report's order (deepest first). Only
/// hides; never deletes a file. The caller has checked the owner has a backup.
pub fn apply_report(
    c: &mut Connection,
    report: &Report,
    opts: &ApplyOpts,
) -> rusqlite::Result<ApplyStats> {
    let mut stats = ApplyStats::default();
    let decided_by = format!("dedup-apply:{}", report.generated_at);
    for m in &report.merges {
        stats.planned += 1;
        if !opts.classes.contains(&m.kind) {
            stats.other_class += 1;
            continue;
        }
        if m.score < opts.min_score {
            stats.below_score += 1;
            continue;
        }
        if opts.limit.is_some_and(|l| stats.applied >= l) {
            break;
        }
        if opts.dry_run {
            stats.applied += 1;
            stats.duplicate_bytes += m.bytes;
            continue;
        }
        match merge(
            c,
            &m.survivor,
            &m.duplicate,
            m.kind.as_str(),
            m.score as f64,
            &decided_by,
            opts.redo,
        )? {
            Ok(()) => {
                stats.applied += 1;
                stats.duplicate_bytes += m.bytes;
            }
            Err(skip) => *stats.skipped.entry(skip.as_str().into()).or_default() += 1,
        }
    }
    Ok(stats)
}

// ---- the API -------------------------------------------------------------------------------

pub fn routes() -> Router<Library> {
    Router::new().route("/v1/media/items/{id}/duplicates", get(duplicates))
}

/// The copies hidden under a photo, as the compare view and the details panel show them.
pub fn duplicates_of(c: &Connection, survivor: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT l.item_id, l.kind, l.score, l.decided_by, l.created_at, i.bytes, i.mime, i.width, i.height,
                i.source, i.device, d.name, i.hash
         FROM item_links l JOIN items i ON i.id = l.item_id LEFT JOIN devices d ON d.id = i.device
         WHERE l.duplicate_of = ?1 AND l.undone_at IS NULL ORDER BY l.created_at, l.item_id",
    )?;
    let rows = st.query_map([survivor], |r| {
        let id: String = r.get(0)?;
        Ok(json!({
            "id": id,
            "kind": r.get::<_, String>(1)?,
            "score": r.get::<_, f64>(2)?,
            "decided_by": r.get::<_, String>(3)?,
            "merged_at": r.get::<_, String>(4)?,
            "size": r.get::<_, i64>(5)?,
            "mime": r.get::<_, String>(6)?,
            "width": r.get::<_, Option<i64>>(7)?,
            "height": r.get::<_, Option<i64>>(8)?,
            "source": r.get::<_, String>(9)?,
            "device": r.get::<_, Option<String>>(10)?,
            "device_name": r.get::<_, Option<String>>(11)?,
            "hash": r.get::<_, String>(12)?,
            "urls": {
                "original": format!("/v1/media/items/{id}/original"),
                "thumb": format!("/v1/media/items/{id}/thumb?size=256"),
            },
        }))
    })?;
    rows.collect()
}

async fn duplicates(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !crate::batch::id_ok(&id) {
        return Err(ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "not found".into(),
        ));
    }
    let asked = id.clone();
    let found = lib
        .db
        .call(move |c| {
            let exists: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1)",
                [&id],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let survivor = resolve(c, &id)?;
            let dups = duplicates_of(c, &survivor)?;
            Ok(Some((survivor, dups)))
        })
        .await?;
    let Some((survivor, dups)) = found else {
        return Err(ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "not found".into(),
        ));
    };
    Ok(Json(json!({
        "item_id": survivor,
        "merged_from": (survivor != asked).then_some(asked),
        "duplicates": dups,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::meta::ClientMeta;
    use crate::{ingest, store};

    fn library() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        cfg.quiet_start = 0;
        cfg.quiet_end = 0;
        store::init_store(&cfg).unwrap();
        (tmp, Library::open(cfg).unwrap())
    }

    /// A distinct PNG per shade (the bytes differ, so the catalog sees different files).
    fn png(shade: u8, side: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(side, side, image::Rgb([shade, 90, 160]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn put(
        tmp: &tempfile::TempDir,
        lib: &Library,
        shade: u8,
        side: u32,
        source: &str,
        device: Option<(&str, &str)>,
    ) -> ingest::IngestResult {
        let src = tmp.path().join(format!(
            "in-{shade}-{side}-{source}-{}",
            device.map_or("", |d| d.1)
        ));
        std::fs::write(&src, png(shade, side)).unwrap();
        let client = ClientMeta {
            filename: Some(format!("{source}-{shade}.png")),
            source: Some(source.into()),
            device: device.map(|d| d.0.into()),
            asset_id: device.map(|d| d.1.into()),
            ..Default::default()
        };
        ingest::ingest_file(lib, &src, None, &client, None).unwrap()
    }

    fn q<T: rusqlite::types::FromSql>(
        lib: &Library,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> T {
        lib.db
            .with(|c| c.query_row(sql, args, |r| r.get(0)))
            .unwrap()
    }

    fn merge_ids(lib: &Library, survivor: &str, dup: &str) -> Result<(), Skip> {
        lib.db
            .with(|c| merge(c, survivor, dup, "same_photo_reencoded", 0.9, "test", false))
            .unwrap()
    }

    struct Pair {
        phone: String,
        backup: String,
    }

    /// The iPhone's copy (device asset A1) and an old backup's copy of the same photo.
    fn pair(tmp: &tempfile::TempDir, lib: &Library) -> Pair {
        let phone = put(tmp, lib, 10, 8, "phone-ios", Some(("iphone", "A1"))).item_id;
        let backup = put(tmp, lib, 11, 16, "archive-nespc", None).item_id;
        Pair { phone, backup }
    }

    fn visible(lib: &Library) -> Vec<String> {
        lib.db
            .with(|c| {
                let mut st = c.prepare(
                    "SELECT id FROM items WHERE hidden = 0 AND trashed_at IS NULL ORDER BY id",
                )?;
                let rows = st
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(rows)
            })
            .unwrap()
    }

    #[test]
    fn a_merge_hides_the_duplicate_and_keeps_everything() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        let before: i64 = q(&lib, "SELECT COUNT(*) FROM blobs", &[]);
        merge_ids(&lib, &p.backup, &p.phone).unwrap();
        // Hidden from every listing, the row and its bytes stay.
        assert_eq!(visible(&lib), vec![p.backup.clone()]);
        assert_eq!(q::<i64>(&lib, "SELECT COUNT(*) FROM blobs", &[]), before);
        assert_eq!(
            q::<i64>(&lib, "SELECT hidden FROM items WHERE id = ?1", &[&p.phone]),
            HIDDEN_DUPLICATE
        );
        assert_eq!(lib.db.with(|c| resolve(c, &p.phone)).unwrap(), p.backup);
        assert_eq!(lib.db.with(|c| resolve(c, &p.backup)).unwrap(), p.backup);
        let dups = lib.db.with(|c| duplicates_of(c, &p.backup)).unwrap();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0]["id"], p.phone.as_str());
        // The survivor outside the Gallery took the phone's device, so it stays in the Gallery.
        assert_eq!(
            q::<String>(&lib, "SELECT device FROM items WHERE id = ?1", &[&p.backup]),
            "iphone"
        );
        let gallery = crate::db::ListQuery {
            scope: Some("gallery".into()),
            ..Default::default()
        };
        let page = lib.db.with(|c| db::list_items(c, &gallery)).unwrap();
        assert_eq!(
            page.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec![p.backup.as_str()]
        );
    }

    #[test]
    fn albums_and_favourites_move_to_the_survivor() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        lib.db
            .with(|c| {
                c.execute("INSERT INTO albums (id, title, created_at, updated_at) VALUES ('alb_trip', 'Trip', 'x', 'x')", [])?;
                db::add_to_album(c, "alb_trip", &p.phone)?;
                db::add_to_album(c, "sys-camera", &p.backup)?;
                db::set_favorite(c, &p.phone, true)?;
                c.execute("UPDATE albums SET cover_item = ?1 WHERE id = 'alb_trip'", [&p.phone])
            })
            .unwrap();
        merge_ids(&lib, &p.backup, &p.phone).unwrap();
        let albums: Vec<String> = lib.db.with(|c| db::item_albums(c, &p.backup)).unwrap();
        assert!(albums.contains(&"alb_trip".to_string()), "{albums:?}");
        assert!(albums.contains(&"sys-favorites".to_string()));
        assert!(albums.contains(&"sys-camera".to_string()));
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT favorite FROM items WHERE id = ?1",
                &[&p.backup]
            ),
            1
        );
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT favorite FROM items WHERE id = ?1",
                &[&p.phone]
            ),
            0
        );
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT COUNT(*) FROM album_items WHERE item_id = ?1",
                &[&p.phone]
            ),
            0
        );
        assert_eq!(
            q::<String>(
                &lib,
                "SELECT cover_item FROM albums WHERE id = 'alb_trip'",
                &[]
            ),
            p.backup
        );
        // The album lists one photo, not two.
        let page = lib
            .db
            .with(|c| {
                db::list_items(
                    c,
                    &crate::db::ListQuery {
                        album: Some("alb_trip".into()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, p.backup);
    }

    #[test]
    fn undo_puts_everything_back() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        lib.db
            .with(|c| {
                c.execute("INSERT INTO albums (id, title, created_at, updated_at) VALUES ('alb_trip', 'Trip', 'x', 'x')", [])?;
                db::add_to_album(c, "alb_trip", &p.phone)?;
                db::add_to_album(c, "alb_trip", &p.backup)?;
                db::set_favorite(c, &p.phone, true)
            })
            .unwrap();
        let albums_phone = lib.db.with(|c| db::item_albums(c, &p.phone)).unwrap();
        let albums_backup = lib.db.with(|c| db::item_albums(c, &p.backup)).unwrap();
        merge_ids(&lib, &p.backup, &p.phone).unwrap();
        assert!(lib.db.with(|c| undo(c, &p.phone)).unwrap());
        assert_eq!(visible(&lib).len(), 2);
        let mut a = lib.db.with(|c| db::item_albums(c, &p.phone)).unwrap();
        let mut b = lib.db.with(|c| db::item_albums(c, &p.backup)).unwrap();
        let (mut ea, mut eb) = (albums_phone, albums_backup);
        a.sort();
        b.sort();
        ea.sort();
        eb.sort();
        assert_eq!(a, ea);
        assert_eq!(b, eb);
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT favorite FROM items WHERE id = ?1",
                &[&p.phone]
            ),
            1
        );
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT favorite FROM items WHERE id = ?1",
                &[&p.backup]
            ),
            0
        );
        // The asset is the phone item's again; the backup is outside the Gallery again.
        assert_eq!(
            q::<String>(
                &lib,
                "SELECT asset_id FROM items WHERE id = ?1",
                &[&p.phone]
            ),
            "A1"
        );
        assert_eq!(
            q::<Option<String>>(&lib, "SELECT device FROM items WHERE id = ?1", &[&p.backup]),
            None
        );
        assert_eq!(q::<i64>(&lib, "SELECT COUNT(*) FROM item_aliases", &[]), 0);
        assert_eq!(lib.db.with(|c| resolve(c, &p.phone)).unwrap(), p.phone);
        // Undoing twice does nothing, and the owner's split is remembered.
        assert!(!lib.db.with(|c| undo(c, &p.phone)).unwrap());
        assert_eq!(
            merge_ids(&lib, &p.backup, &p.phone),
            Err(Skip::UndoneBefore)
        );
        let again = lib
            .db
            .with(|c| {
                merge(
                    c,
                    &p.backup,
                    &p.phone,
                    "same_photo_reencoded",
                    0.9,
                    "owner",
                    true,
                )
            })
            .unwrap();
        assert_eq!(again, Ok(()));
    }

    #[test]
    fn the_same_asset_uploaded_again_resolves_to_the_survivor() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        merge_ids(&lib, &p.backup, &p.phone).unwrap();
        // The phone sends asset A1 again with the same bytes (a retry after a reinstall): a duplicate.
        let again = put(&tmp, &lib, 10, 8, "phone-ios", Some(("iphone", "A1")));
        assert!(again.duplicate);
        assert_eq!(again.item_id, p.backup);
        // The same bytes from another device resolve to the survivor too.
        let other = put(&tmp, &lib, 10, 8, "phone-ios", Some(("ipad", "Z9")));
        assert!(other.duplicate);
        assert_eq!(other.item_id, p.backup);
        // An edit of the asset becomes a new version of the survivor, not a new item and not of the hidden copy.
        let edit = put(&tmp, &lib, 12, 8, "phone-ios", Some(("iphone", "A1")));
        assert!(!edit.duplicate);
        assert_eq!(edit.item_id, p.backup);
        assert_eq!(edit.version, 2);
        // Sending that edit again changes nothing.
        let edit_again = put(&tmp, &lib, 12, 8, "phone-ios", Some(("iphone", "A1")));
        assert!(edit_again.duplicate);
        assert_eq!(edit_again.item_id, p.backup);
        assert_eq!(visible(&lib), vec![p.backup.clone()]);
        // The device listing still reports the asset as present, now under the survivor.
        assert_eq!(
            q::<i64>(
                &lib,
                "SELECT COUNT(*) FROM item_aliases WHERE device = 'iphone' AND asset_id = 'A1'",
                &[]
            ),
            1
        );
    }

    #[test]
    fn merging_a_survivor_flattens_earlier_merges() {
        let (tmp, lib) = library();
        let a = put(&tmp, &lib, 20, 8, "phone-ios", Some(("iphone", "A"))).item_id;
        let b = put(&tmp, &lib, 21, 16, "archive-nespc", None).item_id;
        let c = put(&tmp, &lib, 22, 32, "folder", None).item_id;
        merge_ids(&lib, &b, &a).unwrap();
        merge_ids(&lib, &c, &b).unwrap();
        assert_eq!(lib.db.with(|x| resolve(x, &a)).unwrap(), c);
        assert_eq!(lib.db.with(|x| resolve(x, &b)).unwrap(), c);
        assert_eq!(visible(&lib), vec![c.clone()]);
        let again = put(&tmp, &lib, 20, 8, "phone-ios", Some(("iphone", "A")));
        assert_eq!(again.item_id, c);
        // Undoing the outer merge hands the inner one back to its own survivor.
        assert!(lib.db.with(|x| undo(x, &b)).unwrap());
        assert_eq!(lib.db.with(|x| resolve(x, &a)).unwrap(), b);
        assert_eq!(visible(&lib).len(), 2);
    }

    #[test]
    fn only_two_visible_untrashed_photos_merge() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        assert_eq!(merge_ids(&lib, &p.phone, &p.phone), Err(Skip::SameItem));
        assert_eq!(merge_ids(&lib, &p.phone, "itm_nope"), Err(Skip::Missing));
        lib.db
            .with(|c| {
                c.execute(
                    "UPDATE items SET trashed_at = 'x' WHERE id = ?1",
                    [&p.backup],
                )
            })
            .unwrap();
        assert_eq!(merge_ids(&lib, &p.phone, &p.backup), Err(Skip::Trashed));
        lib.db
            .with(|c| {
                c.execute(
                    "UPDATE items SET trashed_at = NULL, kind = 'video' WHERE id = ?1",
                    [&p.backup],
                )
            })
            .unwrap();
        assert_eq!(merge_ids(&lib, &p.phone, &p.backup), Err(Skip::NotPhoto));
        lib.db
            .with(|c| {
                c.execute(
                    "UPDATE items SET kind = 'photo', hidden = 1 WHERE id = ?1",
                    [&p.backup],
                )
            })
            .unwrap();
        assert_eq!(merge_ids(&lib, &p.phone, &p.backup), Err(Skip::Hidden));
        // A Drive file stays where the owner put it.
        lib.db
            .with(|c| {
                c.execute("UPDATE items SET hidden = 0 WHERE id = ?1", [&p.backup])?;
                c.execute(
                    "INSERT INTO drive_files (item_id, folder, path, dir, name, ext, doc_type, bytes, mtime, mtime_ns)
                     VALUES (?1, 'Docs', 'a.png', '', 'a.png', 'png', 'image', 1, 'x', 0)",
                    [&p.backup],
                )
            })
            .unwrap();
        assert_eq!(merge_ids(&lib, &p.phone, &p.backup), Err(Skip::DriveFile));
        assert_eq!(merge_ids(&lib, &p.backup, &p.phone), Err(Skip::DriveFile));
    }

    #[test]
    fn purging_a_survivor_takes_its_copies_along_and_leaves_no_links() {
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        merge_ids(&lib, &p.backup, &p.phone).unwrap();
        lib.db
            .with(|c| {
                c.execute(
                    "UPDATE items SET trashed_at = '2026-01-01T00:00:00.000Z' WHERE id = ?1",
                    [&p.backup],
                )
            })
            .unwrap();
        let report = lib
            .db
            .with(|c| crate::trash::purge_trashed(c, std::slice::from_ref(&p.backup)))
            .unwrap();
        assert_eq!(report.done.len(), 2, "{:?}", report.failed);
        assert_eq!(q::<i64>(&lib, "SELECT COUNT(*) FROM items", &[]), 0);
        for t in ["item_links", "item_aliases", "item_hashes"] {
            assert_eq!(
                q::<i64>(&lib, &format!("SELECT COUNT(*) FROM {t}"), &[]),
                0,
                "{t}"
            );
        }
        // Both originals are unreferenced now and go.
        assert_eq!(report.orphans.len(), 2);
    }

    #[test]
    fn a_report_applies_in_order_with_its_filters() {
        use crate::dedup::{MergePlan, Report, Thresholds};
        let (tmp, lib) = library();
        let p = pair(&tmp, &lib);
        let third = put(&tmp, &lib, 40, 24, "folder", None).item_id;
        let plan = |dup: &str, surv: &str, kind: Class, score: f32| MergePlan {
            duplicate: dup.into(),
            survivor: surv.into(),
            root: surv.into(),
            kind,
            score,
            depth: 1,
            bytes: 10,
        };
        let report = Report {
            version: 1,
            generated_at: "t".into(),
            thresholds: Thresholds::default(),
            library: crate::dedup::LibraryStats {
                photos: 3,
                hashed: 3,
                flat: 0,
                without_hash: 0,
            },
            classes: Default::default(),
            by_source_pair: vec![],
            merge_total: 3,
            merge_duplicate_bytes: 0,
            merge_scores: vec![],
            calibration: Default::default(),
            samples: vec![],
            merges: vec![
                plan(&p.phone, &p.backup, Class::SameReencoded, 0.9),
                plan(&third, &p.backup, Class::ResizedCopy, 0.5),
                plan("itm_gone", &p.backup, Class::SameReencoded, 0.95),
            ],
            pairs: vec![],
        };
        let dry = lib
            .db
            .with(|c| {
                apply_report(
                    c,
                    &report,
                    &ApplyOpts {
                        dry_run: true,
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert_eq!((dry.applied, dry.below_score), (2, 1));
        assert_eq!(visible(&lib).len(), 3, "a dry run changes nothing");
        let real = lib
            .db
            .with(|c| apply_report(c, &report, &ApplyOpts::default()))
            .unwrap();
        assert_eq!(real.applied, 1);
        assert_eq!(real.below_score, 1);
        assert_eq!(real.skipped.get("missing"), Some(&1));
        assert_eq!(visible(&lib).len(), 2);
        assert_eq!(
            q::<String>(
                &lib,
                "SELECT decided_by FROM item_links WHERE item_id = ?1",
                &[&p.phone]
            ),
            "dedup-apply:t"
        );
    }
}

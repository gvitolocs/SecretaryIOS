//! Turning a received file into a catalog item: hash, dedupe or version, metadata, place,
//! albums, search text. Also the thumbnail worker and the spool mover.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::db::{self, now_str, Db};
use crate::meta::{self, ClientMeta, Extracted};
use crate::protocol::{ingest_decision, IngestDecision, Known};
use crate::{geo, store, thumbs};

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub item_id: String,
}

/// Shared handle to everything a request needs.
#[derive(Clone)]
pub struct Library {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub events: broadcast::Sender<Event>,
    /// Mints the player's tokenised URLs for listings (poster_url, thumb_url).
    pub signer: Option<Arc<secretary_media_stream::Signer>>,
    /// One heavy Drive job at a time (text extraction, PDF pages, Office conversion), whether the
    /// background worker or a viewer asked for it.
    pub jobs: Arc<tokio::sync::Semaphore>,
}

impl Library {
    pub fn open(cfg: Config) -> anyhow::Result<Self> {
        for d in [
            cfg.data_dir.clone(),
            cfg.spool_dir(),
            cfg.uploads_dir(),
            cfg.thumbs_dir(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        let db = Db::open(&cfg.db_path())?;
        let (events, _) = broadcast::channel(256);
        let signer = cfg.play_secret.as_deref().map(|s| {
            Arc::new(secretary_media_stream::Signer::new(
                s,
                Duration::from_secs(12 * 3600),
            ))
        });
        Ok(Self {
            cfg: Arc::new(cfg),
            db,
            events,
            signer,
            jobs: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    pub fn emit(&self, kind: &'static str, item_id: &str) {
        let _ = self.events.send(Event {
            kind,
            item_id: item_id.to_string(),
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IngestResult {
    pub item_id: String,
    pub duplicate: bool,
    pub version: i64,
}

/// Ingest a file the library now owns (`src` is consumed: moved into the store or removed).
/// `preview` is an optional JPEG rendering from the phone. Blocking; call from the blocking pool.
pub fn ingest_file(
    lib: &Library,
    src: &Path,
    hash: Option<String>,
    client: &ClientMeta,
    preview: Option<&Path>,
) -> anyhow::Result<IngestResult> {
    let cfg = &lib.cfg;
    let hash = match hash {
        Some(h) => h.to_ascii_lowercase(),
        None => store::hash_file(src)?,
    };
    let name_path = PathBuf::from(client.filename.clone().unwrap_or_else(|| {
        src.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string()
    }));
    let mime = meta::mime_for(&name_path, client.mime.as_deref());
    let ext = meta::ext_for(&name_path, &mime);
    let bytes = std::fs::metadata(src)?.len() as i64;
    let x = meta::extract(src, &mime, client, cfg.tz, &cfg.ffprobe);

    let known = lib.db.with(|c| known_for(c, &hash, client))?;
    let decision = ingest_decision(&known, &hash);
    if let IngestDecision::Duplicate { item_id } = &decision {
        keep_owned_bytes(lib, src, &hash, &ext, bytes)?;
        let version = lib.db.with(|c| {
            // A file imported earlier (e.g. from Takeout) now arrives from the phone: remember the
            // asset so later edits version this item instead of creating a new one.
            if let (Some(dev), Some(asset)) = (&client.device, &client.asset_id) {
                c.execute(
                    "UPDATE items SET device = ?2, asset_id = ?3 WHERE id = ?1 AND asset_id IS NULL",
                    params![item_id, dev, asset],
                )
                .ok();
            }
            c.query_row("SELECT version FROM items WHERE id = ?1", [item_id], |r| r.get(0))
        })?;
        return Ok(IngestResult {
            item_id: item_id.clone(),
            duplicate: true,
            version,
        });
    }

    keep_owned_bytes(lib, src, &hash, &ext, bytes)?;

    let place = match (x.lat, x.lon) {
        (Some(lat), Some(lon)) => geo::place_for(lat, lon),
        _ => None,
    };
    let (item_id, version, event) = match decision {
        IngestDecision::New => {
            let id = db::new_id("itm");
            let raced = lib.db.with(|c| {
                insert_new_or_duplicate(
                    c,
                    &id,
                    &hash,
                    bytes,
                    &name_path,
                    &x,
                    place.as_ref(),
                    client,
                )
            })?;
            if let Some(item_id) = raced {
                let version = lib.db.with(|c| {
                    c.query_row("SELECT version FROM items WHERE id = ?1", [&item_id], |r| {
                        r.get(0)
                    })
                })?;
                return Ok(IngestResult {
                    item_id,
                    duplicate: true,
                    version,
                });
            }
            (id, 1, "item.added")
        }
        IngestDecision::NewVersion { item_id } => {
            let v = lib
                .db
                .with(|c| new_version(c, &item_id, &hash, bytes, &x, place.as_ref()))?;
            let dir = thumbs::dir_for(cfg, &item_id);
            for f in ["s.jpg", "l.jpg", "poster.jpg", "preview.jpg"] {
                let _ = std::fs::remove_file(dir.join(f));
            }
            (item_id, v, "item.updated")
        }
        IngestDecision::Duplicate { .. } => unreachable!(),
    };
    if let Some(p) = preview {
        let dir = thumbs::dir_for(cfg, &item_id);
        std::fs::create_dir_all(&dir)?;
        store::durable_move(p, &dir.join("preview.jpg"))?;
    }
    lib.db.with(|c| db::reindex(c, &item_id))?;
    lib.emit(event, &item_id);
    Ok(IngestResult {
        item_id,
        duplicate: false,
        version,
    })
}

/// The blob row for bytes the library now owns (`src`, consumed), by kernel DK3: an owned copy that
/// is already there wins and `src` goes; a row that only references a file in place (a Drive file,
/// a memo) is upgraded to the owned copy, so a phone's original never depends on a file the owner
/// may move or delete in the Drive.
fn keep_owned_bytes(
    lib: &Library,
    src: &Path,
    hash: &str,
    ext: &str,
    bytes: i64,
) -> anyhow::Result<()> {
    use crate::drive_kernel::{blob_decision, BlobAction, BlobLoc};
    let existing = lib.db.with(|c| db::blob(c, hash))?;
    match blob_decision(existing.map(|b| BlobLoc::from_location(&b.location)), true) {
        BlobAction::DropIncoming => std::fs::remove_file(src)?,
        BlobAction::Place => {
            let loc = store::place_new(&lib.cfg, src, hash, ext)?;
            lib.db.with(|c| {
                c.execute(
                    "INSERT OR IGNORE INTO blobs (hash, bytes, ext, location, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![hash, bytes, ext, loc, now_str()],
                )
            })?;
        }
        BlobAction::Upgrade => {
            let loc = store::place_new(&lib.cfg, src, hash, ext)?;
            lib.db.with(|c| {
                c.execute(
                    "UPDATE blobs SET location = ?2, ext = ?3, path = NULL WHERE hash = ?1 AND location = 'external'",
                    params![hash, loc, ext],
                )
            })?;
        }
        // An owned incoming copy never maps to these (DK3).
        BlobAction::RecordExternal | BlobAction::Keep => std::fs::remove_file(src)?,
    }
    Ok(())
}

fn known_for(c: &Connection, hash: &str, client: &ClientMeta) -> rusqlite::Result<Known> {
    let by_hash = c
        .query_row(
            "SELECT id FROM items WHERE hash = ?1 ORDER BY added_at LIMIT 1",
            [hash],
            |r| r.get(0),
        )
        .optional()?;
    let by_asset = match (&client.device, &client.asset_id) {
        (Some(d), Some(a)) => c
            .query_row(
                "SELECT id, hash FROM items WHERE device = ?1 AND asset_id = ?2",
                params![d, a],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        _ => None,
    };
    Ok(Known { by_hash, by_asset })
}

/// The insert of a New item, deciding again under the catalog lock (bug M3 in docs/verification.md):
/// `known_for` ran before `store::place_new`, so a concurrent finish of the same bytes may have
/// added an item since. Then nothing is inserted and that item is returned, with this phone asset
/// recorded on it as the Duplicate branch does. The check and the insert run in one `Db::with`,
/// so no other ingest can come between them.
#[allow(clippy::too_many_arguments)]
fn insert_new_or_duplicate(
    c: &mut Connection,
    id: &str,
    hash: &str,
    bytes: i64,
    name: &Path,
    x: &Extracted,
    place: Option<&db::Place>,
    client: &ClientMeta,
) -> rusqlite::Result<Option<String>> {
    let existing: Option<String> = c
        .query_row(
            "SELECT id FROM items WHERE hash = ?1 ORDER BY added_at LIMIT 1",
            [hash],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if let (Some(dev), Some(asset)) = (&client.device, &client.asset_id) {
            c.execute(
                "UPDATE items SET device = ?2, asset_id = ?3 WHERE id = ?1 AND asset_id IS NULL",
                params![existing, dev, asset],
            )?;
        }
        return Ok(Some(existing));
    }
    insert_item(c, id, hash, bytes, name, x, place, client)?;
    Ok(None)
}

fn sort_at(taken: Option<DateTime<Utc>>) -> String {
    taken
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(now_str)
}

#[allow(clippy::too_many_arguments)]
fn insert_item(
    c: &mut Connection,
    id: &str,
    hash: &str,
    bytes: i64,
    name: &Path,
    x: &Extracted,
    place: Option<&db::Place>,
    client: &ClientMeta,
) -> rusqlite::Result<()> {
    let tx = c.transaction()?;
    let now = now_str();
    let title = client
        .title
        .clone()
        .or_else(|| {
            name.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Untitled".into());
    let taken = x
        .taken_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let source = client.source.clone().unwrap_or_else(|| "upload".into());
    tx.execute(
        "INSERT INTO items (id, kind, title, caption, hash, mime, bytes, taken_at, sort_at, width, height, duration_ms,
            video_codec, audio_codec, lat, lon, place_name, admin1, admin2, cc, camera, source, device, asset_id,
            hidden, favorite, added_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22,
            ?23, ?24, ?25, ?26, ?27, ?27)",
        params![
            id,
            x.kind,
            title,
            client.caption,
            hash,
            x.mime,
            bytes,
            taken,
            sort_at(x.taken_at),
            x.width,
            x.height,
            x.duration_ms,
            x.video_codec,
            x.audio_codec,
            place.map(|p| p.lat),
            place.map(|p| p.lon),
            place.map(|p| p.name.clone()),
            place.and_then(|p| p.admin1.clone()),
            place.and_then(|p| p.admin2.clone()),
            place.and_then(|p| p.country.clone()),
            x.camera,
            source,
            client.device,
            client.asset_id,
            client.live_motion.unwrap_or(false) as i64,
            client.favorite.unwrap_or(false) as i64,
            now,
        ],
    )?;
    if let (Some(dev), Some(pair)) = (&client.device, &client.live_pair_asset) {
        let other: Option<String> = tx
            .query_row(
                "SELECT id FROM items WHERE device = ?1 AND asset_id = ?2",
                params![dev, pair],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = other {
            tx.execute(
                "UPDATE items SET live_pair = ?2 WHERE id = ?1",
                params![id, other],
            )?;
            tx.execute(
                "UPDATE items SET live_pair = ?2 WHERE id = ?1",
                params![other, id],
            )?;
        }
    }
    for key in system_albums_for(&x.kind, &source, client) {
        db::add_to_album(&tx, &format!("sys-{key}"), id)?;
    }
    if let Some(album) = client.album.as_deref().filter(|a| !a.trim().is_empty()) {
        let aid = find_or_create_album(&tx, album.trim())?;
        db::add_to_album(&tx, &aid, id)?;
    }
    tx.commit()
}

fn system_albums_for(kind: &str, source: &str, client: &ClientMeta) -> Vec<&'static str> {
    let mut v = Vec::new();
    if client.live_motion == Some(true) {
        return v;
    }
    if source == "voice-memo" {
        v.push("voice-memos");
        return v;
    }
    if source.starts_with("phone") && matches!(kind, "photo" | "video") {
        v.push("camera");
    }
    if kind == "video" {
        v.push("videos");
    }
    if client.screenshot == Some(true) {
        v.push("screenshots");
    }
    if client.favorite == Some(true) {
        v.push("favorites");
    }
    v
}

pub fn find_or_create_album(c: &Connection, title: &str) -> rusqlite::Result<String> {
    if let Some(id) = c
        .query_row(
            "SELECT id FROM albums WHERE title = ?1 COLLATE NOCASE ORDER BY created_at LIMIT 1",
            [title],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = db::new_id("alb");
    let now = now_str();
    c.execute(
        "INSERT INTO albums (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, title, now],
    )?;
    Ok(id)
}

fn new_version(
    c: &mut Connection,
    item_id: &str,
    hash: &str,
    bytes: i64,
    x: &Extracted,
    place: Option<&db::Place>,
) -> rusqlite::Result<i64> {
    let tx = c.transaction()?;
    let (old_hash, version): (String, i64) = tx.query_row(
        "SELECT hash, version FROM items WHERE id = ?1",
        [item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO item_versions (item_id, version, hash, replaced_at) VALUES (?1, ?2, ?3, ?4)",
        params![item_id, version, old_hash, now_str()],
    )?;
    let taken = x
        .taken_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    tx.execute(
        "UPDATE items SET hash = ?2, mime = ?3, bytes = ?4, version = ?5, width = ?6, height = ?7, duration_ms = ?8,
            video_codec = ?9, audio_codec = ?10, taken_at = COALESCE(?11, taken_at), sort_at = COALESCE(?11, sort_at),
            lat = COALESCE(?12, lat), lon = COALESCE(?13, lon), place_name = COALESCE(?14, place_name),
            admin1 = COALESCE(?15, admin1), admin2 = COALESCE(?16, admin2), cc = COALESCE(?17, cc),
            thumb_state = 'pending', updated_at = ?18
         WHERE id = ?1",
        params![
            item_id,
            hash,
            x.mime,
            bytes,
            version + 1,
            x.width,
            x.height,
            x.duration_ms,
            x.video_codec,
            x.audio_codec,
            taken,
            place.map(|p| p.lat),
            place.map(|p| p.lon),
            place.map(|p| p.name.clone()),
            place.and_then(|p| p.admin1.clone()),
            place.and_then(|p| p.admin2.clone()),
            place.and_then(|p| p.country.clone()),
            now_str(),
        ],
    )?;
    tx.commit()?;
    Ok(version + 1)
}

/// A file another service owns (voice memos). Indexed in place, never copied.
#[derive(Clone, Debug, Deserialize)]
pub struct ExternalReq {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub created_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub mime: Option<String>,
    pub path: String,
    #[serde(default)]
    pub links: serde_json::Value,
    /// Album title to put the item in (created if missing).
    pub album: Option<String>,
}

pub fn register_external(
    lib: &Library,
    source: &str,
    ext_id: &str,
    req: &ExternalReq,
) -> anyhow::Result<IngestResult> {
    let path = PathBuf::from(&req.path);
    anyhow::ensure!(
        path.is_absolute() && path.is_file(),
        "path must be an existing absolute file"
    );
    let hash = store::hash_file(&path)?;
    let mime = meta::mime_for(&path, req.mime.as_deref());
    let ext = meta::ext_for(&path, &mime);
    let bytes = std::fs::metadata(&path)?.len() as i64;
    let client = ClientMeta {
        taken_at: req.created_at.clone(),
        duration_ms: req.duration_ms,
        source: Some(source.to_string()),
        title: req.title.clone(),
        caption: req.caption.clone(),
        album: req.album.clone(),
        ..Default::default()
    };
    let mut x = meta::extract(&path, &mime, &client, lib.cfg.tz, &lib.cfg.ffprobe);
    if let Some(k) = &req.kind {
        x.kind = k.clone();
    }
    let links = (!req.links.is_null()).then(|| req.links.to_string());
    let existing: Option<String> = lib.db.with(|c| {
        c.query_row(
            "SELECT item_id FROM externals WHERE source = ?1 AND ext_id = ?2",
            params![source, ext_id],
            |r| r.get(0),
        )
        .optional()
    })?;
    lib.db.with(|c| {
        c.execute(
            "INSERT INTO blobs (hash, bytes, ext, location, path, created_at) VALUES (?1, ?2, ?3, 'external', ?4, ?5)
             ON CONFLICT(hash) DO UPDATE SET path = excluded.path WHERE blobs.location = 'external'",
            params![hash, bytes, ext, req.path, now_str()],
        )
    })?;
    let (item_id, event) = match existing {
        Some(id) => {
            lib.db.with(|c| {
                c.execute(
                    "UPDATE items SET hash = ?2, mime = ?3, bytes = ?4, title = COALESCE(?5, title), caption = COALESCE(?6, caption),
                        duration_ms = COALESCE(?7, duration_ms), links = COALESCE(?8, links), trashed_at = NULL, updated_at = ?9
                     WHERE id = ?1",
                    params![id, hash, mime, bytes, req.title, req.caption, x.duration_ms, links, now_str()],
                )
            })?;
            (id, "item.updated")
        }
        None => {
            let id = db::new_id("itm");
            let name = PathBuf::from(req.title.clone().unwrap_or_else(|| ext_id.to_string()));
            lib.db.with(|c| {
                insert_item(c, &id, &hash, bytes, &name, &x, None, &client)?;
                c.execute(
                    "UPDATE items SET links = ?2 WHERE id = ?1",
                    params![id, links],
                )?;
                c.execute(
                    "INSERT INTO externals (source, ext_id, item_id) VALUES (?1, ?2, ?3)",
                    params![source, ext_id, id],
                )?;
                Ok(())
            })?;
            (id, "item.added")
        }
    };
    lib.db.with(|c| {
        c.execute(
            "UPDATE items SET thumb_state = 'pending' WHERE id = ?1",
            [&item_id],
        )?;
        db::reindex(c, &item_id)
    })?;
    lib.emit(event, &item_id);
    Ok(IngestResult {
        item_id,
        duplicate: false,
        version: 1,
    })
}

pub fn unregister_external(
    lib: &Library,
    source: &str,
    ext_id: &str,
) -> anyhow::Result<Option<String>> {
    let id: Option<String> = lib.db.with(|c| {
        let id = c
            .query_row(
                "SELECT item_id FROM externals WHERE source = ?1 AND ext_id = ?2",
                params![source, ext_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = &id {
            c.execute(
                "UPDATE items SET trashed_at = ?2 WHERE id = ?1",
                params![id, now_str()],
            )?;
            c.execute(
                "DELETE FROM externals WHERE source = ?1 AND ext_id = ?2",
                params![source, ext_id],
            )?;
        }
        Ok(id)
    })?;
    if let Some(id) = &id {
        lib.emit("item.trashed", id);
    }
    Ok(id)
}

/// Where an item's current original is, if the item exists. A Drive file is its own file (two
/// identical files share one blob row, which names only one of their paths).
pub fn original_path(lib: &Library, item_id: &str) -> anyhow::Result<Option<(PathBuf, db::Item)>> {
    let Some(item) = lib.db.with(|c| db::get_item(c, item_id))? else {
        return Ok(None);
    };
    if let Some(info) = lib.db.with(|c| crate::drive::info_for(c, item_id))? {
        if let Some(p) = crate::drive::file_path(lib, &info).filter(|p| p.is_file()) {
            return Ok(Some((p, item)));
        }
    }
    let Some(b) = lib.db.with(|c| db::blob(c, &item.hash))? else {
        return Ok(None);
    };
    Ok(Some((store::path_of(&lib.cfg, &b), item)))
}

/// Make thumbnails for items that need them, a few at a time, forever.
pub async fn thumb_worker(lib: Library) {
    loop {
        let batch: Vec<(String, String)> = lib
            .db
            .call(|c| {
                let mut st = c.prepare(
                    "SELECT id, kind FROM items WHERE thumb_state = 'pending' AND trashed_at IS NULL ORDER BY added_at DESC LIMIT 8",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect()
            })
            .await
            .unwrap_or_default();
        if batch.is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        for (id, kind) in batch {
            let l = lib.clone();
            let tid = id.clone();
            let res = tokio::task::spawn_blocking(
                move || -> anyhow::Result<(&'static str, Option<Vec<u8>>)> {
                    let Some((path, item)) = original_path(&l, &tid)? else {
                        return Ok(("none", None));
                    };
                    let state = thumbs::generate(&l.cfg, &tid, &kind, &path, item.duration_ms)?;
                    let hash = match &state {
                        thumbs::ThumbState::Ready(h) => Some(h.clone()),
                        thumbs::ThumbState::None => None,
                    };
                    Ok((state.as_str(), hash))
                },
            )
            .await;
            let (state, hash) = match res {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(item = %id, error = %e, "thumbnail failed");
                    ("failed", None)
                }
                Err(e) => {
                    tracing::warn!(item = %id, error = %e, "thumbnail task panicked");
                    ("failed", None)
                }
            };
            let sid = id.clone();
            let _ = lib
                .db
                .call(move |c| {
                    c.execute(
                        "UPDATE items SET thumb_state = ?2, thumbhash = COALESCE(?3, thumbhash) WHERE id = ?1",
                        params![sid, state, hash],
                    )
                })
                .await;
            if state == "ready" {
                lib.emit("item.updated", &id);
            }
        }
    }
}

/// Move spooled originals to the big disk whenever the day window allows. Every 5 minutes.
pub async fn spool_mover(lib: Library) {
    loop {
        let l = lib.clone();
        match tokio::task::spawn_blocking(move || store::flush_spool(&l.cfg, &l.db)).await {
            Ok(Ok(n)) if n > 0 => tracing::info!(moved = n, "spool flushed to store"),
            Ok(Err(e)) => tracing::warn!(error = %e, "spool flush failed"),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

/// Delete upload temp files that were abandoned for more than a week.
pub fn sweep_uploads(lib: &Library) -> anyhow::Result<usize> {
    let cutoff = (Utc::now() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let stale: Vec<String> = lib.db.with(|c| {
        let mut st = c.prepare("SELECT id FROM uploads WHERE finished = 0 AND updated_at < ?1")?;
        let rows = st.query_map([&cutoff], |r| r.get(0))?;
        rows.collect()
    })?;
    for id in &stale {
        let _ = std::fs::remove_file(lib.cfg.uploads_dir().join(id));
        let _ = std::fs::remove_file(lib.cfg.uploads_dir().join(format!("{id}.preview")));
        lib.db
            .with(|c| c.execute("DELETE FROM uploads WHERE id = ?1", [id]))?;
    }
    Ok(stale.len())
}

/// Index every media file under `root` in place (no copy), in the album `label`. Files already
/// indexed with the same size are skipped, so rescans are cheap.
pub fn index_folder(lib: &Library, label: &str, root: &Path) -> anyhow::Result<(usize, usize)> {
    let (mut added, mut kept) = (0, 0);
    // Symlinks are followed: the player's library folders are links to files elsewhere.
    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        // DVD rips: the .VOB pieces are not separate videos.
        if name.starts_with('.') || [".vob", ".ifo", ".bup"].iter().any(|e| lower.ends_with(e)) {
            continue;
        }
        let mime = meta::mime_for(path, None);
        let kind = meta::kind_for_mime(&mime);
        if kind == "file" {
            continue;
        }
        let ext_id = blake3::hash(path.to_string_lossy().as_bytes()).to_hex()[..32].to_string();
        let size = entry.metadata().map(|m| m.len() as i64).unwrap_or(-1);
        let same: bool = lib.db.with(|c| {
            c.query_row(
                "SELECT 1 FROM externals e JOIN items i ON i.id = e.item_id JOIN blobs b ON b.hash = i.hash
                 WHERE e.source = 'folder' AND e.ext_id = ?1 AND b.path = ?2 AND i.bytes = ?3 AND i.trashed_at IS NULL",
                params![ext_id, path.to_string_lossy(), size],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
        })?;
        if same {
            kept += 1;
            continue;
        }
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&name)
            .replace(['.', '_'], " ");
        let req = ExternalReq {
            kind: Some(kind.to_string()),
            title: Some(title),
            caption: None,
            created_at: None,
            duration_ms: None,
            mime: Some(mime),
            path: path.to_string_lossy().to_string(),
            links: serde_json::Value::Null,
            album: Some(label.to_string()),
        };
        match register_external(lib, "folder", &ext_id, &req) {
            Ok(_) => added += 1,
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "index failed"),
        }
    }
    Ok((added, kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        cfg.quiet_start = 0;
        cfg.quiet_end = 0;
        store::init_store(&cfg).unwrap();
        (tmp, Library::open(cfg).unwrap())
    }

    /// A phone's asset `n` (a new asset per round: the same asset with other bytes is an edit).
    fn client(device: &str, n: usize) -> ClientMeta {
        ClientMeta {
            filename: Some("same.txt".into()),
            device: Some(device.into()),
            asset_id: Some(format!("{device}-asset-{n}")),
            ..Default::default()
        }
    }

    fn items_with_hash(lib: &Library, hash: &str) -> i64 {
        lib.db
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM items WHERE hash = ?1", [hash], |r| {
                    r.get(0)
                })
            })
            .unwrap()
    }

    /// M3: the decision said New, then a concurrent finish of the same bytes inserted its item.
    #[test]
    fn insert_after_a_concurrent_new_is_a_duplicate() {
        let (tmp, lib) = library();
        let src = tmp.path().join("same.txt");
        std::fs::write(&src, b"the same bytes").unwrap();
        let x = meta::extract(
            &src,
            "text/plain",
            &client("phone-a", 0),
            lib.cfg.tz,
            &lib.cfg.ffprobe,
        );
        let name = PathBuf::from("same.txt");
        lib.db
            .with(|c| c.execute("INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h1', 14, 'txt', 'store', 'now')", []))
            .unwrap();
        // Both finishes decided with an empty catalog.
        let known = lib
            .db
            .with(|c| known_for(c, "h1", &client("phone-b", 0)))
            .unwrap();
        assert_eq!(ingest_decision(&known, "h1"), IngestDecision::New);
        // The other one inserts first.
        assert_eq!(
            lib.db
                .with(|c| insert_new_or_duplicate(
                    c,
                    "itm_first",
                    "h1",
                    14,
                    &name,
                    &x,
                    None,
                    &client("phone-a", 0)
                ))
                .unwrap(),
            None
        );
        // This one must not add a second item made from the same bytes.
        let got = lib
            .db
            .with(|c| {
                insert_new_or_duplicate(
                    c,
                    "itm_second",
                    "h1",
                    14,
                    &name,
                    &x,
                    None,
                    &client("phone-b", 0),
                )
            })
            .unwrap();
        assert_eq!(got.as_deref(), Some("itm_first"));
        assert_eq!(items_with_hash(&lib, "h1"), 1);
    }

    /// Two devices finish the same new bytes at the same moment, 20 times.
    #[test]
    fn concurrent_ingests_of_the_same_bytes_make_one_item() {
        let (tmp, lib) = library();
        for round in 0..20 {
            let bytes = format!("round {round}: the same picture from two phones").into_bytes();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let results: Vec<IngestResult> = ["phone-a", "phone-b"]
                .into_iter()
                .map(|device| {
                    let src = tmp.path().join(format!("{round}-{device}.txt"));
                    std::fs::write(&src, &bytes).unwrap();
                    let (lib, barrier) = (lib.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        ingest_file(&lib, &src, None, &client(device, round), None).unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect();
            let hash = lib
                .db
                .with(|c| {
                    c.query_row(
                        "SELECT hash FROM items WHERE id = ?1",
                        [&results[0].item_id],
                        |r| r.get::<_, String>(0),
                    )
                })
                .unwrap();
            let rows: Vec<String> = lib
                .db
                .with(|c| {
                    let mut st = c.prepare("SELECT id || ' ' || IFNULL(device,'-') || ' ' || IFNULL(asset_id,'-') || ' ' || source || ' v' || version FROM items WHERE hash = ?1")?;
                    let r = st.query_map([&hash], |r| r.get(0))?.collect();
                    r
                })
                .unwrap();
            assert_eq!(
                items_with_hash(&lib, &hash),
                1,
                "round {round}: two items from the same bytes: {rows:?} {:?}",
                results
                    .iter()
                    .map(|r| (&r.item_id, r.duplicate))
                    .collect::<Vec<_>>()
            );
            assert_eq!(results[0].item_id, results[1].item_id, "round {round}");
            assert_eq!(
                results.iter().filter(|r| r.duplicate).count(),
                1,
                "round {round}"
            );
        }
    }
}

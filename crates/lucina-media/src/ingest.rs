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
}

impl Library {
    pub fn open(cfg: Config) -> anyhow::Result<Self> {
        for d in [cfg.data_dir.clone(), cfg.spool_dir(), cfg.uploads_dir(), cfg.thumbs_dir()] {
            std::fs::create_dir_all(d)?;
        }
        let db = Db::open(&cfg.db_path())?;
        let (events, _) = broadcast::channel(256);
        let signer = cfg
            .play_secret
            .as_deref()
            .map(|s| Arc::new(secretary_media_stream::Signer::new(s, Duration::from_secs(12 * 3600))));
        Ok(Self { cfg: Arc::new(cfg), db, events, signer })
    }

    pub fn emit(&self, kind: &'static str, item_id: &str) {
        let _ = self.events.send(Event { kind, item_id: item_id.to_string() });
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
        src.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
    }));
    let mime = meta::mime_for(&name_path, client.mime.as_deref());
    let ext = meta::ext_for(&name_path, &mime);
    let bytes = std::fs::metadata(src)?.len() as i64;
    let x = meta::extract(src, &mime, client, cfg.tz, &cfg.ffprobe);

    let known = lib.db.with(|c| known_for(c, &hash, client))?;
    let decision = ingest_decision(&known, &hash);
    if let IngestDecision::Duplicate { item_id } = &decision {
        std::fs::remove_file(src)?;
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
        return Ok(IngestResult { item_id: item_id.clone(), duplicate: true, version });
    }

    let have_blob = lib.db.with(|c| db::blob(c, &hash))?.is_some();
    if have_blob {
        std::fs::remove_file(src)?;
    } else {
        let loc = store::place_new(cfg, src, &hash, &ext)?;
        lib.db.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO blobs (hash, bytes, ext, location, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![hash, bytes, ext, loc, now_str()],
            )
        })?;
    }

    let place = match (x.lat, x.lon) {
        (Some(lat), Some(lon)) => geo::place_for(lat, lon),
        _ => None,
    };
    let (item_id, version, event) = match decision {
        IngestDecision::New => {
            let id = db::new_id("itm");
            lib.db.with(|c| insert_item(c, &id, &hash, bytes, &name_path, &x, place.as_ref(), client))?;
            (id, 1, "item.added")
        }
        IngestDecision::NewVersion { item_id } => {
            let v = lib.db.with(|c| new_version(c, &item_id, &hash, bytes, &x, place.as_ref()))?;
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
    Ok(IngestResult { item_id, duplicate: false, version })
}

fn known_for(c: &Connection, hash: &str, client: &ClientMeta) -> rusqlite::Result<Known> {
    let by_hash = c
        .query_row("SELECT id FROM items WHERE hash = ?1 ORDER BY added_at LIMIT 1", [hash], |r| r.get(0))
        .optional()?;
    let by_asset = match (&client.device, &client.asset_id) {
        (Some(d), Some(a)) => c
            .query_row("SELECT id, hash FROM items WHERE device = ?1 AND asset_id = ?2", params![d, a], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?,
        _ => None,
    };
    Ok(Known { by_hash, by_asset })
}

fn sort_at(taken: Option<DateTime<Utc>>) -> String {
    taken.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_else(now_str)
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
        .or_else(|| name.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .unwrap_or_else(|| "Untitled".into());
    let taken = x.taken_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
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
            .query_row("SELECT id FROM items WHERE device = ?1 AND asset_id = ?2", params![dev, pair], |r| r.get(0))
            .optional()?;
        if let Some(other) = other {
            tx.execute("UPDATE items SET live_pair = ?2 WHERE id = ?1", params![id, other])?;
            tx.execute("UPDATE items SET live_pair = ?2 WHERE id = ?1", params![other, id])?;
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
        .query_row("SELECT id FROM albums WHERE title = ?1 COLLATE NOCASE ORDER BY created_at LIMIT 1", [title], |r| {
            r.get(0)
        })
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
    let (old_hash, version): (String, i64) =
        tx.query_row("SELECT hash, version FROM items WHERE id = ?1", [item_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    tx.execute(
        "INSERT OR IGNORE INTO item_versions (item_id, version, hash, replaced_at) VALUES (?1, ?2, ?3, ?4)",
        params![item_id, version, old_hash, now_str()],
    )?;
    let taken = x.taken_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
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

pub fn register_external(lib: &Library, source: &str, ext_id: &str, req: &ExternalReq) -> anyhow::Result<IngestResult> {
    let path = PathBuf::from(&req.path);
    anyhow::ensure!(path.is_absolute() && path.is_file(), "path must be an existing absolute file");
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
        c.query_row("SELECT item_id FROM externals WHERE source = ?1 AND ext_id = ?2", params![source, ext_id], |r| {
            r.get(0)
        })
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
                c.execute("UPDATE items SET links = ?2 WHERE id = ?1", params![id, links])?;
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
        c.execute("UPDATE items SET thumb_state = 'pending' WHERE id = ?1", [&item_id])?;
        db::reindex(c, &item_id)
    })?;
    lib.emit(event, &item_id);
    Ok(IngestResult { item_id, duplicate: false, version: 1 })
}

pub fn unregister_external(lib: &Library, source: &str, ext_id: &str) -> anyhow::Result<Option<String>> {
    let id: Option<String> = lib.db.with(|c| {
        let id = c
            .query_row("SELECT item_id FROM externals WHERE source = ?1 AND ext_id = ?2", params![source, ext_id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(id) = &id {
            c.execute("UPDATE items SET trashed_at = ?2 WHERE id = ?1", params![id, now_str()])?;
            c.execute("DELETE FROM externals WHERE source = ?1 AND ext_id = ?2", params![source, ext_id])?;
        }
        Ok(id)
    })?;
    if let Some(id) = &id {
        lib.emit("item.trashed", id);
    }
    Ok(id)
}

/// Where an item's current original is, if the item exists.
pub fn original_path(lib: &Library, item_id: &str) -> anyhow::Result<Option<(PathBuf, db::Item)>> {
    let Some(item) = lib.db.with(|c| db::get_item(c, item_id))? else { return Ok(None) };
    let Some(b) = lib.db.with(|c| db::blob(c, &item.hash))? else { return Ok(None) };
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
            let res = tokio::task::spawn_blocking(move || -> anyhow::Result<&'static str> {
                let Some((path, _)) = original_path(&l, &tid)? else { return Ok("none") };
                Ok(thumbs::generate(&l.cfg, &tid, &kind, &path)?.as_str())
            })
            .await;
            let state = match res {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(item = %id, error = %e, "thumbnail failed");
                    "failed"
                }
                Err(e) => {
                    tracing::warn!(item = %id, error = %e, "thumbnail task panicked");
                    "failed"
                }
            };
            let sid = id.clone();
            let _ = lib
                .db
                .call(move |c| c.execute("UPDATE items SET thumb_state = ?2 WHERE id = ?1", params![sid, state]))
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
    let cutoff = (Utc::now() - chrono::Duration::days(7)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let stale: Vec<String> = lib.db.with(|c| {
        let mut st = c.prepare("SELECT id FROM uploads WHERE finished = 0 AND updated_at < ?1")?;
        let rows = st.query_map([&cutoff], |r| r.get(0))?;
        rows.collect()
    })?;
    for id in &stale {
        let _ = std::fs::remove_file(lib.cfg.uploads_dir().join(id));
        let _ = std::fs::remove_file(lib.cfg.uploads_dir().join(format!("{id}.preview")));
        lib.db.with(|c| c.execute("DELETE FROM uploads WHERE id = ?1", [id]))?;
    }
    Ok(stale.len())
}

/// Index every media file under `root` in place (no copy), in the album `label`. Files already
/// indexed with the same size are skipped, so rescans are cheap.
pub fn index_folder(lib: &Library, label: &str, root: &Path) -> anyhow::Result<(usize, usize)> {
    let (mut added, mut kept) = (0, 0);
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
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
        let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name).replace(['.', '_'], " ");
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

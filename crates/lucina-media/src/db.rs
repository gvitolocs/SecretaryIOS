//! The catalog: SQLite (WAL) on NVMe. One connection behind a mutex; callers run queries on
//! the blocking pool via [`Db::call`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS blobs (
    hash TEXT PRIMARY KEY,
    bytes INTEGER NOT NULL,
    ext TEXT NOT NULL,
    location TEXT NOT NULL CHECK (location IN ('spool', 'store', 'external')),
    path TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('photo', 'video', 'audio', 'file')),
    title TEXT NOT NULL,
    caption TEXT,
    hash TEXT NOT NULL REFERENCES blobs(hash),
    mime TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    taken_at TEXT,
    sort_at TEXT NOT NULL,
    width INTEGER,
    height INTEGER,
    duration_ms INTEGER,
    video_codec TEXT,
    audio_codec TEXT,
    lat REAL,
    lon REAL,
    place_name TEXT,
    admin1 TEXT,
    admin2 TEXT,
    cc TEXT,
    camera TEXT,
    source TEXT NOT NULL,
    device TEXT,
    asset_id TEXT,
    live_pair TEXT,
    hidden INTEGER NOT NULL DEFAULT 0,
    favorite INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 1,
    links TEXT,
    thumb_state TEXT NOT NULL DEFAULT 'pending',
    trashed_at TEXT,
    added_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS items_asset ON items(device, asset_id) WHERE asset_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS items_hash ON items(hash);
CREATE INDEX IF NOT EXISTS items_sort ON items(sort_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS items_thumb ON items(thumb_state);
CREATE TABLE IF NOT EXISTS item_versions (
    item_id TEXT NOT NULL REFERENCES items(id),
    version INTEGER NOT NULL,
    hash TEXT NOT NULL REFERENCES blobs(hash),
    replaced_at TEXT NOT NULL,
    PRIMARY KEY (item_id, version)
);
CREATE TABLE IF NOT EXISTS albums (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    system_key TEXT UNIQUE,
    cover_item TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS album_items (
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES items(id),
    added_at TEXT NOT NULL,
    PRIMARY KEY (album_id, item_id)
);
CREATE INDEX IF NOT EXISTS album_items_item ON album_items(item_id);
-- Album share links (`share.rs`): a random token per link; rows stay after a revoke.
CREATE TABLE IF NOT EXISTS album_shares (
    token TEXT PRIMARY KEY,
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    revoked_at TEXT,
    allow_original INTEGER NOT NULL DEFAULT 0,
    views INTEGER NOT NULL DEFAULT 0,
    last_view_at TEXT
);
CREATE INDEX IF NOT EXISTS album_shares_album ON album_shares(album_id);
CREATE TABLE IF NOT EXISTS uploads (
    id TEXT PRIMARY KEY,
    filename TEXT NOT NULL,
    declared INTEGER NOT NULL,
    received INTEGER NOT NULL DEFAULT 0,
    expected_hash TEXT,
    meta TEXT NOT NULL,
    finished INTEGER NOT NULL DEFAULT 0,
    item_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS externals (
    source TEXT NOT NULL,
    ext_id TEXT NOT NULL,
    item_id TEXT NOT NULL REFERENCES items(id),
    PRIMARY KEY (source, ext_id)
);
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(item_id UNINDEXED, body, tokenize = 'unicode61 remove_diacritics 2');
-- The Drive (drive.rs): one row per regular file in a Drive folder, with its place in the folder.
-- `path` is relative to the folder (`a/b/c.pdf`), `dir` its parent (`a/b`, '' at the top).
CREATE TABLE IF NOT EXISTS drive_files (
    id INTEGER PRIMARY KEY,
    item_id TEXT NOT NULL UNIQUE REFERENCES items(id),
    folder TEXT NOT NULL,
    path TEXT NOT NULL,
    dir TEXT NOT NULL,
    name TEXT NOT NULL,
    ext TEXT NOT NULL,
    doc_type TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    mtime TEXT NOT NULL,
    mtime_ns INTEGER NOT NULL,
    text_state TEXT NOT NULL DEFAULT 'pending' CHECK (text_state IN ('pending', 'ready', 'none', 'failed')),
    UNIQUE (folder, path)
);
CREATE INDEX IF NOT EXISTS drive_files_dir ON drive_files(folder, dir);
-- The filesystem (st_dev) each Drive folder's files were indexed from (drive.rs, kernel DK4).
CREATE TABLE IF NOT EXISTS drive_folders (
    folder TEXT PRIMARY KEY,
    dev INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS drive_files_text ON drive_files(text_state);
-- The text of Drive documents (PDF, Office, text, code), rowid = drive_files.id. Contentless: the
-- text itself stays in the files (and thumbs/<id>/text.txt for extracted text).
CREATE VIRTUAL TABLE IF NOT EXISTS drive_fts USING fts5(body, content = '', contentless_delete = 1, tokenize = 'unicode61 remove_diacritics 2');
"#;

/// System albums every library has. Keys are stable; titles are what the apps show.
pub const SYSTEM_ALBUMS: &[(&str, &str)] = &[
    ("camera", "Camera roll"),
    ("videos", "Videos"),
    ("screenshots", "Screenshots"),
    ("voice-memos", "Voice memos"),
    ("favorites", "Favourites"),
    // What Lucina last found for the owner in chat (replaced on every search she shows).
    ("lucina", "Lucina's picks"),
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("database worker failed: {0}")]
    Join(String),
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let now = now_str();
        for (key, title) in SYSTEM_ALBUMS {
            conn.execute(
                "INSERT OR IGNORE INTO albums (id, title, system_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![format!("sys-{key}"), title, key, now],
            )?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` with the connection on the blocking pool.
    pub async fn call<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|e| DbError::Join(e.to_string()))?
        .map_err(DbError::from)
    }

    /// Synchronous access, for the CLI and tests.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, rusqlite::Error>,
    ) -> Result<T, DbError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard).map_err(DbError::from)
    }
}

/// Columns added after the first deploy.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has = |col: &str| -> rusqlite::Result<bool> {
        let mut st = conn.prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = ?1")?;
        st.exists([col])
    };
    if !has("thumbhash")? {
        conn.execute("ALTER TABLE items ADD COLUMN thumbhash BLOB", [])?;
    }
    Ok(())
}

pub fn now_str() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Place {
    pub name: String,
    pub admin1: Option<String>,
    pub admin2: Option<String>,
    pub country: Option<String>,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Codecs {
    pub video: Option<String>,
    pub audio: Option<String>,
}

/// An item as the API returns it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub caption: Option<String>,
    pub mime: String,
    pub size: i64,
    pub hash: String,
    pub taken_at: Option<String>,
    pub created_at: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub codecs: Codecs,
    pub place: Option<Place>,
    pub camera: Option<String>,
    pub source: String,
    pub live_pair: Option<String>,
    pub favorite: bool,
    pub version: i64,
    pub albums: Vec<String>,
    pub links: serde_json::Value,
    pub trashed_at: Option<String>,
    pub thumb_state: String,
    /// ThumbHash (base64, about 24 characters): the placeholder shown before any bytes arrive.
    pub thumbhash: Option<String>,
}

pub const ITEM_COLS: &str = "id, kind, title, caption, mime, bytes, hash, taken_at, added_at, width, height, duration_ms, \
    video_codec, audio_codec, lat, lon, place_name, admin1, admin2, cc, camera, source, live_pair, favorite, version, links, \
    trashed_at, thumb_state, thumbhash";

pub fn item_from_row(r: &Row) -> rusqlite::Result<Item> {
    let lat: Option<f64> = r.get(14)?;
    let lon: Option<f64> = r.get(15)?;
    let place_name: Option<String> = r.get(16)?;
    let place = match (lat, lon) {
        (Some(lat), Some(lon)) => Some(Place {
            name: place_name.unwrap_or_default(),
            admin1: r.get(17)?,
            admin2: r.get(18)?,
            country: r.get(19)?,
            lat,
            lon,
        }),
        _ => None,
    };
    let links: Option<String> = r.get(25)?;
    Ok(Item {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        caption: r.get(3)?,
        mime: r.get(4)?,
        size: r.get(5)?,
        hash: r.get(6)?,
        taken_at: r.get(7)?,
        created_at: r.get(8)?,
        width: r.get(9)?,
        height: r.get(10)?,
        duration_ms: r.get(11)?,
        codecs: Codecs {
            video: r.get(12)?,
            audio: r.get(13)?,
        },
        place,
        camera: r.get(20)?,
        source: r.get(21)?,
        live_pair: r.get(22)?,
        favorite: r.get::<_, i64>(23)? != 0,
        version: r.get(24)?,
        albums: Vec::new(),
        links: links
            .and_then(|l| serde_json::from_str(&l).ok())
            .unwrap_or(serde_json::Value::Null),
        trashed_at: r.get(26)?,
        thumb_state: r.get(27)?,
        thumbhash: r.get::<_, Option<Vec<u8>>>(28)?.map(|h| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(h)
        }),
    })
}

pub fn get_item(c: &Connection, id: &str) -> rusqlite::Result<Option<Item>> {
    let mut item = c
        .query_row(
            &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
            [id],
            item_from_row,
        )
        .optional()?;
    if let Some(it) = item.as_mut() {
        it.albums = item_albums(c, id)?;
    }
    Ok(item)
}

/// The videos a viewer can open, newest added first: `(id, added_at)`.
pub fn video_ids(c: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = c.prepare(
        "SELECT id, added_at FROM items WHERE kind = 'video' AND hidden = 0 AND trashed_at IS NULL ORDER BY added_at DESC, id DESC",
    )?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The videos in the film folders (`scope=films`), newest added first: `(id, added_at)`.
/// The background preparation only prepares these; phone videos are left alone.
pub fn film_video_ids(c: &Connection, films: &[String]) -> rusqlite::Result<Vec<(String, String)>> {
    let mut args = Vec::new();
    let cond =
        scope_condition(Some("films"), films, &mut args).expect("films always has a condition");
    let mut st = c.prepare(&format!(
        "SELECT i.id, i.added_at FROM items i WHERE i.kind = 'video' AND i.hidden = 0 AND i.trashed_at IS NULL AND {cond} ORDER BY i.added_at DESC, i.id DESC"
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(args), |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    rows.collect()
}

pub fn item_albums(c: &Connection, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut st =
        c.prepare_cached("SELECT album_id FROM album_items WHERE item_id = ?1 ORDER BY added_at")?;
    let rows = st.query_map([id], |r| r.get(0))?;
    rows.collect()
}

/// Where an item's current original is on disk.
#[derive(Clone, Debug)]
pub struct BlobLoc {
    pub hash: String,
    pub ext: String,
    pub location: String,
    pub path: Option<String>,
    pub bytes: i64,
}

pub fn blob(c: &Connection, hash: &str) -> rusqlite::Result<Option<BlobLoc>> {
    c.query_row(
        "SELECT hash, ext, location, path, bytes FROM blobs WHERE hash = ?1",
        [hash],
        |r| {
            Ok(BlobLoc {
                hash: r.get(0)?,
                ext: r.get(1)?,
                location: r.get(2)?,
                path: r.get(3)?,
                bytes: r.get(4)?,
            })
        },
    )
    .optional()
}

/// Rebuild the search text of one item: title, caption, place, camera, albums, date words.
pub fn reindex(c: &Connection, id: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM items_fts WHERE item_id = ?1", [id])?;
    let Some(it) = get_item(c, id)? else {
        return Ok(());
    };
    let mut body = vec![it.title.clone()];
    body.extend(it.caption.clone());
    if let Some(p) = &it.place {
        body.push(p.name.clone());
        body.extend(p.admin1.clone());
        body.extend(p.admin2.clone());
        if let Some(cc) = &p.country {
            body.push(cc.clone());
            body.extend(crate::geo::country_name(cc).map(str::to_string));
        }
    }
    body.extend(it.camera.clone());
    body.push(it.kind.clone());
    // A Drive file is also found by its folders' names and its type ("slides", "pdf").
    if let Some(d) = crate::drive::info_for(c, id)? {
        body.push(d.folder.clone());
        body.push(d.path.replace('/', " "));
        body.push(d.ext.clone());
        if let Some(t) = crate::doctype::DocType::parse(&d.doc_type) {
            body.push(t.search_words().to_string());
        }
    }
    if let Some(t) = it
        .taken_at
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
    {
        body.push(t.format("%B %Y").to_string());
    }
    for a in &it.albums {
        if let Some(title) = c
            .query_row("SELECT title FROM albums WHERE id = ?1", [a], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            body.push(title);
        }
    }
    c.execute(
        "INSERT INTO items_fts (item_id, body) VALUES (?1, ?2)",
        params![id, body.join(" ")],
    )?;
    Ok(())
}

pub fn add_to_album(c: &Connection, album_id: &str, item_id: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "INSERT OR IGNORE INTO album_items (album_id, item_id, added_at) VALUES (?1, ?2, ?3)",
        params![album_id, item_id, now_str()],
    )?;
    if n > 0 {
        c.execute(
            "UPDATE albums SET updated_at = ?2, cover_item = COALESCE(cover_item, ?3) WHERE id = ?1",
            params![album_id, now_str(), item_id],
        )?;
    }
    Ok(n > 0)
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListQuery {
    pub kind: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub album: Option<String>,
    pub q: Option<String>,
    /// `lat,lon,km`
    pub near: Option<String>,
    pub source: Option<String>,
    /// `gallery`: what the phones backed up (`source` phone-…), the Gallery. `films`: the film
    /// folders (`film_folders`), the Films plugin. `drive`: everything else (old Nextcloud files,
    /// computer photo backups, audio, other folders, shared files, voice memos), the Drive.
    pub scope: Option<String>,
    /// Set by the server from its config (not a query parameter): the film folder labels.
    #[serde(skip)]
    pub film_folders: Vec<String>,
    /// A Drive document type (`pdf`, `presentation`, …; `doctype::DocType`).
    #[serde(rename = "type")]
    pub doc_type: Option<String>,
    pub favorite: Option<bool>,
    pub trashed: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemPage {
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
    pub total: Option<i64>,
}

/// Turn free text into an FTS5 query: every word must match as a prefix.
pub fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

pub fn parse_near(s: &str) -> Option<(f64, f64, f64)> {
    let mut it = s.split(',').map(|p| p.trim().parse::<f64>());
    let lat = it.next()?.ok()?;
    let lon = it.next()?.ok()?;
    let km = it.next().map(|k| k.ok()).unwrap_or(Some(25.0))?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && km > 0.0)
        .then_some((lat, lon, km))
}

/// Accepts `YYYY`, `YYYY-MM`, `YYYY-MM-DD` or RFC 3339; `end` rounds partial dates up.
pub fn normalize_bound(s: &str, end: bool) -> Option<String> {
    use chrono::NaiveDate;
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(
            t.with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
    }
    let date = match s.len() {
        4 => {
            let y: i32 = s.parse().ok()?;
            if end {
                NaiveDate::from_ymd_opt(y + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(y, 1, 1)?
            }
        }
        7 => {
            let d = NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").ok()?;
            if end {
                d.checked_add_months(chrono::Months::new(1))?
            } else {
                d
            }
        }
        10 => {
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
            if end {
                d.succ_opt()?
            } else {
                d
            }
        }
        _ => return None,
    };
    Some(
        date.and_hms_opt(0, 0, 0)?
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

fn add(
    wh: &mut Vec<String>,
    args: &mut Vec<rusqlite::types::Value>,
    cond: &str,
    v: rusqlite::types::Value,
) {
    args.push(v);
    wh.push(cond.replace('?', &format!("?{}", args.len())));
}

pub fn list_items(c: &Connection, q: &ListQuery) -> rusqlite::Result<ItemPage> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500) as usize;
    let mut wh: Vec<String> = Vec::new();
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if q.trashed.unwrap_or(false) {
        wh.push("i.trashed_at IS NOT NULL".into());
    } else {
        wh.push("i.trashed_at IS NULL".into());
        wh.push("i.hidden = 0".into());
    }
    if let Some(k) = &q.kind {
        add(&mut wh, &mut args, "i.kind = ?", k.clone().into());
    }
    if let Some(s) = &q.source {
        add(&mut wh, &mut args, "i.source = ?", s.clone().into());
    }
    if let Some(cond) = scope_condition(q.scope.as_deref(), &q.film_folders, &mut args) {
        wh.push(cond);
    }
    if q.favorite == Some(true) {
        wh.push("i.favorite = 1".into());
    }
    if let Some(t) = &q.doc_type {
        add(
            &mut wh,
            &mut args,
            "i.id IN (SELECT item_id FROM drive_files WHERE doc_type = ?)",
            t.clone().into(),
        );
    }
    if let Some(f) = q.from.as_deref().and_then(|f| normalize_bound(f, false)) {
        add(&mut wh, &mut args, "i.sort_at >= ?", f.into());
    }
    if let Some(t) = q.to.as_deref().and_then(|t| normalize_bound(t, true)) {
        add(&mut wh, &mut args, "i.sort_at < ?", t.into());
    }
    if let Some(a) = &q.album {
        // An album id, or a title (the player apps filter by title).
        args.push(a.clone().into());
        let n = args.len();
        wh.push(format!(
            "i.id IN (SELECT ai.item_id FROM album_items ai JOIN albums al ON al.id = ai.album_id \
             WHERE al.id = ?{n} OR al.title = ?{n} COLLATE NOCASE)"
        ));
    }
    if let Some(fq) = q.q.as_deref().and_then(fts_query) {
        // Titles, places, albums… and the content of Drive documents.
        args.push(fq.into());
        let n = args.len();
        wh.push(format!(
            "(i.id IN (SELECT item_id FROM items_fts WHERE items_fts MATCH ?{n}) \
             OR i.id IN (SELECT df.item_id FROM drive_files df WHERE df.id IN \
               (SELECT rowid FROM drive_fts WHERE drive_fts MATCH ?{n})))"
        ));
    }
    let near = q.near.as_deref().and_then(parse_near);
    if let Some((lat, lon, km)) = near {
        // Bounding box in SQL, exact distance below.
        let dlat = km / 111.0;
        let dlon = km / (111.0 * lat.to_radians().cos().abs().max(0.01));
        add(&mut wh, &mut args, "i.lat >= ?", (lat - dlat).into());
        add(&mut wh, &mut args, "i.lat <= ?", (lat + dlat).into());
        add(&mut wh, &mut args, "i.lon >= ?", (lon - dlon).into());
        add(&mut wh, &mut args, "i.lon <= ?", (lon + dlon).into());
    }
    let total = if near.is_none() && q.cursor.is_none() {
        let sql = format!("SELECT COUNT(*) FROM items i WHERE {}", wh.join(" AND "));
        Some(c.query_row(&sql, rusqlite::params_from_iter(args.iter()), |r| r.get(0))?)
    } else {
        None
    };
    if let Some((sort_at, id)) = q.cursor.as_deref().and_then(|c| c.split_once('|')) {
        args.push(sort_at.to_string().into());
        let a = args.len();
        args.push(id.to_string().into());
        let b = args.len();
        wh.push(format!(
            "(i.sort_at < ?{a} OR (i.sort_at = ?{a} AND i.id < ?{b}))"
        ));
    }
    let fetch = if near.is_some() { limit * 5 } else { limit } + 1;
    let cols: String = ITEM_COLS
        .split(", ")
        .map(|c| format!("i.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {cols}, i.sort_at FROM items i WHERE {} ORDER BY i.sort_at DESC, i.id DESC LIMIT {fetch}",
        wh.join(" AND ")
    );
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query(rusqlite::params_from_iter(args.iter()))?;
    let mut items = Vec::new();
    let mut last_key = None;
    let mut scanned = 0;
    let mut more = false;
    while let Some(r) = rows.next()? {
        scanned += 1;
        if items.len() == limit {
            more = true;
            break;
        }
        let it = item_from_row(r)?;
        let sort_at: String = r.get(29)?;
        last_key = Some(format!("{sort_at}|{}", it.id));
        if let Some((lat, lon, km)) = near {
            match &it.place {
                Some(p) if crate::geo::haversine_km(lat, lon, p.lat, p.lon) <= km => {}
                _ => continue,
            }
        }
        items.push(it);
    }
    if scanned == fetch {
        more = true;
    }
    drop(rows);
    drop(st);
    for it in items.iter_mut() {
        it.albums = item_albums(c, &it.id)?;
    }
    Ok(ItemPage {
        items,
        next_cursor: if more { last_key } else { None },
        total,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub system_key: Option<String>,
    pub cover_item: Option<String>,
    pub count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `ListQuery::scope` as SQL on `items i`; the film labels go into `args` (numbered `?N`).
/// The three conditions are `drive_kernel::in_scope` (DK2): every item is in exactly one scope; a
/// phone backup filed in a film album stays in the Gallery.
pub fn scope_condition(
    scope: Option<&str>,
    films: &[String],
    args: &mut Vec<rusqlite::types::Value>,
) -> Option<String> {
    let mut in_films = || {
        let marks: Vec<String> = if films.is_empty() {
            // `IN (NULL)` matches nothing: no film folders configured.
            args.push(rusqlite::types::Value::Null);
            vec![format!("?{}", args.len())]
        } else {
            films
                .iter()
                .map(|f| {
                    args.push(f.clone().into());
                    format!("?{}", args.len())
                })
                .collect()
        };
        format!(
            "i.id IN (SELECT fi.item_id FROM album_items fi JOIN albums fa ON fa.id = fi.album_id \
             WHERE fa.system_key IS NULL AND fa.title IN ({}))",
            marks.join(", ")
        )
    };
    match scope {
        Some("gallery") => Some("i.source LIKE 'phone%'".into()),
        Some("films") => Some(format!("i.source NOT LIKE 'phone%' AND {}", in_films())),
        Some("drive") => Some(format!("i.source NOT LIKE 'phone%' AND NOT {}", in_films())),
        _ => None,
    }
}

pub fn list_albums(c: &Connection) -> rusqlite::Result<Vec<Album>> {
    list_albums_in(c, None, &[])
}

/// Albums with their counts in one scope (`gallery`, `films`, `drive`); the ones with nothing there
/// are left out.
pub fn list_albums_in(
    c: &Connection,
    scope: Option<&str>,
    films: &[String],
) -> rusqlite::Result<Vec<Album>> {
    let mut args = Vec::new();
    let extra = scope_condition(scope, films, &mut args)
        .map(|s| format!(" AND {s}"))
        .unwrap_or_default();
    let mut st = c.prepare(&format!(
        "SELECT a.id, a.title, a.system_key, a.cover_item, \
           (SELECT COUNT(*) FROM album_items ai JOIN items i ON i.id = ai.item_id \
             WHERE ai.album_id = a.id AND i.trashed_at IS NULL AND i.hidden = 0{extra}), a.created_at, a.updated_at \
         FROM albums a ORDER BY a.system_key IS NULL, a.updated_at DESC"
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(args), album_from_row)?;
    let albums: Vec<Album> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(if scope.is_some() {
        albums.into_iter().filter(|a| a.count > 0).collect()
    } else {
        albums
    })
}

const SHARE_COLS: &str =
    "token, album_id, created_at, expires_at, revoked_at, allow_original, views, last_view_at";

fn share_from_row(r: &Row) -> rusqlite::Result<crate::share::ShareRow> {
    Ok(crate::share::ShareRow {
        token: r.get(0)?,
        album_id: r.get(1)?,
        created_at: r.get(2)?,
        expires_at: r.get(3)?,
        revoked_at: r.get(4)?,
        allow_original: r.get::<_, i64>(5)? != 0,
        views: r.get(6)?,
        last_view_at: r.get(7)?,
    })
}

pub fn insert_share(c: &Connection, s: &crate::share::ShareRow) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO album_shares (token, album_id, created_at, expires_at, allow_original) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![s.token, s.album_id, s.created_at, s.expires_at, s.allow_original as i64],
    )?;
    Ok(())
}

pub fn get_share(c: &Connection, token: &str) -> rusqlite::Result<Option<crate::share::ShareRow>> {
    c.query_row(
        &format!("SELECT {SHARE_COLS} FROM album_shares WHERE token = ?1"),
        [token],
        share_from_row,
    )
    .optional()
}

/// The shares of one album, or of every album; newest first.
pub fn list_shares(
    c: &Connection,
    album_id: Option<&str>,
) -> rusqlite::Result<Vec<crate::share::ShareRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {SHARE_COLS} FROM album_shares WHERE ?1 IS NULL OR album_id = ?1 ORDER BY created_at DESC"
    ))?;
    let rows = st.query_map([album_id], share_from_row)?;
    rows.collect()
}

/// Sets `revoked_at` once; false for an unknown token.
pub fn revoke_share(c: &Connection, token: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE album_shares SET revoked_at = COALESCE(revoked_at, ?2) WHERE token = ?1",
        params![token, now_str()],
    )?;
    Ok(n > 0)
}

pub fn count_share_view(c: &Connection, token: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE album_shares SET views = views + 1, last_view_at = ?2 WHERE token = ?1",
        params![token, now_str()],
    )?;
    Ok(())
}

/// Whether the item is in the album now and visible (not trashed, not hidden): a share serves
/// nothing else.
pub fn item_visible_in_album(
    c: &Connection,
    album_id: &str,
    item_id: &str,
) -> rusqlite::Result<bool> {
    c.prepare_cached(
        "SELECT 1 FROM album_items ai JOIN items i ON i.id = ai.item_id \
         WHERE ai.album_id = ?1 AND ai.item_id = ?2 AND i.trashed_at IS NULL AND i.hidden = 0",
    )?
    .exists(params![album_id, item_id])
}

/// The visible items of an album, newest first, from `offset` (a share's page and its item list).
pub fn album_visible_items(
    c: &Connection,
    album_id: &str,
    offset: usize,
    limit: usize,
) -> rusqlite::Result<(Vec<Item>, i64)> {
    let total: i64 = c.query_row(
        "SELECT COUNT(*) FROM album_items ai JOIN items i ON i.id = ai.item_id \
         WHERE ai.album_id = ?1 AND i.trashed_at IS NULL AND i.hidden = 0",
        [album_id],
        |r| r.get(0),
    )?;
    let cols = ITEM_COLS
        .split(", ")
        .map(|c| format!("i.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut st = c.prepare(&format!(
        "SELECT {cols} FROM album_items ai JOIN items i ON i.id = ai.item_id \
         WHERE ai.album_id = ?1 AND i.trashed_at IS NULL AND i.hidden = 0 \
         ORDER BY i.sort_at DESC, i.id DESC LIMIT ?2 OFFSET ?3"
    ))?;
    let rows = st.query_map(
        params![album_id, limit as i64, offset as i64],
        item_from_row,
    )?;
    Ok((rows.collect::<rusqlite::Result<_>>()?, total))
}

pub fn get_album(c: &Connection, id: &str) -> rusqlite::Result<Option<Album>> {
    c.query_row(
        "SELECT a.id, a.title, a.system_key, a.cover_item, \
           (SELECT COUNT(*) FROM album_items ai JOIN items i ON i.id = ai.item_id \
             WHERE ai.album_id = a.id AND i.trashed_at IS NULL AND i.hidden = 0), a.created_at, a.updated_at \
         FROM albums a WHERE a.id = ?1",
        [id],
        album_from_row,
    )
    .optional()
}

fn album_from_row(r: &Row) -> rusqlite::Result<Album> {
    Ok(Album {
        id: r.get(0)?,
        title: r.get(1)?,
        system_key: r.get(2)?,
        cover_item: r.get(3)?,
        count: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_round_partial_dates() {
        assert_eq!(
            normalize_bound("2025", false).unwrap(),
            "2025-01-01T00:00:00.000Z"
        );
        assert_eq!(
            normalize_bound("2025", true).unwrap(),
            "2026-01-01T00:00:00.000Z"
        );
        assert_eq!(
            normalize_bound("2025-12", true).unwrap(),
            "2026-01-01T00:00:00.000Z"
        );
        assert_eq!(
            normalize_bound("2025-06-30", true).unwrap(),
            "2025-07-01T00:00:00.000Z"
        );
        assert_eq!(
            normalize_bound("2025-06-30T12:00:00+02:00", false).unwrap(),
            "2025-06-30T10:00:00.000Z"
        );
        assert!(normalize_bound("June", false).is_none());
    }

    #[test]
    fn fts_query_quotes_words() {
        assert_eq!(
            fts_query("Copenhagen, summer").unwrap(),
            "\"Copenhagen\"* \"summer\"*"
        );
        assert!(fts_query(" ,; ").is_none());
    }

    #[test]
    fn near_parses_and_validates() {
        assert_eq!(parse_near("55.67,12.56,10"), Some((55.67, 12.56, 10.0)));
        assert_eq!(parse_near("55.67,12.56"), Some((55.67, 12.56, 25.0)));
        assert_eq!(parse_near("95,12,1"), None);
        assert_eq!(parse_near("55,12,-1"), None);
    }
}

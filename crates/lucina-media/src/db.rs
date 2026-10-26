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
    /// `gallery`: what the phones backed up (`source` phone-…), the Gallery. `drive`: everything
    /// else (imported folders such as the films, shared files, voice memos), the Drive.
    pub scope: Option<String>,
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
    if let Some(cond) = scope_condition(q.scope.as_deref()) {
        wh.push(cond.into());
    }
    if q.favorite == Some(true) {
        wh.push("i.favorite = 1".into());
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
        add(
            &mut wh,
            &mut args,
            "i.id IN (SELECT item_id FROM items_fts WHERE items_fts MATCH ?)",
            fq.into(),
        );
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

/// `ListQuery::scope` as SQL on `items i`.
fn scope_condition(scope: Option<&str>) -> Option<&'static str> {
    match scope {
        Some("gallery") => Some("i.source LIKE 'phone%'"),
        Some("drive") => Some("(i.source IS NULL OR i.source NOT LIKE 'phone%')"),
        _ => None,
    }
}

pub fn list_albums(c: &Connection) -> rusqlite::Result<Vec<Album>> {
    list_albums_in(c, None)
}

/// Albums with their counts in one scope (`gallery`, `drive`); the ones with nothing there are left out.
pub fn list_albums_in(c: &Connection, scope: Option<&str>) -> rusqlite::Result<Vec<Album>> {
    let extra = scope_condition(scope)
        .map(|s| format!(" AND {s}"))
        .unwrap_or_default();
    let mut st = c.prepare(&format!(
        "SELECT a.id, a.title, a.system_key, a.cover_item, \
           (SELECT COUNT(*) FROM album_items ai JOIN items i ON i.id = ai.item_id \
             WHERE ai.album_id = a.id AND i.trashed_at IS NULL AND i.hidden = 0{extra}), a.created_at, a.updated_at \
         FROM albums a ORDER BY a.system_key IS NULL, a.updated_at DESC"
    ))?;
    let rows = st.query_map([], album_from_row)?;
    let albums: Vec<Album> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(if scope.is_some() {
        albums.into_iter().filter(|a| a.count > 0).collect()
    } else {
        albums
    })
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

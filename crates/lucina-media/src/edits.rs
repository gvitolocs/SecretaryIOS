//! The Gallery's photo editor, server side: non-destructive edits.
//!
//! An edit is a versioned JSON [`EditList`] (`secretary_image::edit`, the same Rust the web editor
//! previews with) stored per item in `item_edits`, plus a rendered derivative:
//!
//! - `PUT items/{id}/edits` validates the list, renders it from the original at full resolution
//!   (one job at a time, on the blocking pool, single-threaded), writes `thumbs/<id>/edited.jpg`
//!   (JPEG, quality 92), and makes the grid, preview and thumbnail renditions again from the edited
//!   picture. An identity list is a revert.
//! - `DELETE items/{id}/edits` is the revert: the list and the derivative go, the renditions are
//!   made again from the original, the item's size is what it was.
//! - `GET items/{id}/edits` returns the list; `GET items/{id}/edits/base` the *unedited* picture at
//!   1600 px (the editor's working copy; the item's own renditions show the edit once it has one);
//!   `GET items/{id}/edits/download` the derivative as a download.
//!
//! The original is never touched: `urls.original` and `urls.download` stay the original file, an
//! edited item adds `edited: true`, `urls.edited` (the derivative) and `urls.download_edited`.
//! `edited` means "the derivative file exists"; renditions of an edited item carry the file's
//! modification time in their URL version, so caches never mix the two looks, and a revert goes
//! back to the URLs (and cached bytes) of the original look.
//!
//! HEIC and other formats the server can't decode are edited from the same display source the
//! renditions use (the phone's own rendering, `preview.jpg`), so their derivative has that
//! resolution.

use std::path::{Path as FsPath, PathBuf};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use image::DynamicImage;
use rusqlite::{params, Connection, OptionalExtension};
use secretary_image::edit::{self, EditList};
use secretary_image::Frame;
use serde_json::{json, Value};

use crate::config::Config;
use crate::db::{self, now_str};
use crate::http::{content_disposition, item_out, serve_file, ApiError, ApiResult};
use crate::ingest::{self, Library};
use crate::thumbs::{self, EDITED_FILE};

const BASE_FILE: &str = "base.jpg";
/// JPEG quality of the derivative.
pub const EDITED_QUALITY: u8 = 92;
/// Long side of the editor's working copy.
pub const BASE_PX: u32 = 1600;

/// The edit lists, one row per edited item. `orig_width`/`orig_height` remember the item's size
/// before its first edit (the derivative has another size when cropped or turned).
pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS item_edits (
            item_id TEXT PRIMARY KEY,
            edits TEXT NOT NULL,
            orig_width INTEGER,
            orig_height INTEGER,
            updated_at TEXT NOT NULL
        );",
    )
}

pub fn routes() -> Router<Library> {
    Router::new()
        .route(
            "/v1/media/items/{id}/edits",
            get(get_edits).put(put_edits).delete(delete_edits),
        )
        .route("/v1/media/items/{id}/edits/base", get(get_base))
        .route("/v1/media/items/{id}/edits/download", get(download_edited))
}

// ---- what the rest of the crate asks ----------------------------------------------------------

fn dir(cfg: &Config, id: &str) -> PathBuf {
    thumbs::dir_for(cfg, id)
}

/// The derivative's modification time in milliseconds, when the item is edited.
pub fn stamp(cfg: &Config, id: &str) -> Option<u64> {
    let modified = std::fs::metadata(dir(cfg, id).join(EDITED_FILE))
        .ok()?
        .modified()
        .ok()?;
    let ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(ms as u64)
}

/// The `v` of an item's rendition URLs: its version, and the edit's stamp while it has one.
pub fn url_version(version: i64, stamp: Option<u64>) -> String {
    match stamp {
        Some(s) => format!("{version}e{s}"),
        None => version.to_string(),
    }
}

/// An ETag part for files that change with an edit.
pub fn etag_suffix(stamp: Option<u64>) -> String {
    stamp.map(|s| format!("-e{s}")).unwrap_or_default()
}

/// A new original replaces the edited look: drop the list and the derivative (the caller makes the
/// renditions again).
pub fn forget(lib: &Library, id: &str) {
    let _ = lib
        .db
        .with(|c| c.execute("DELETE FROM item_edits WHERE item_id = ?1", [id]));
    let d = dir(&lib.cfg, id);
    for f in [EDITED_FILE, BASE_FILE] {
        let _ = std::fs::remove_file(d.join(f));
    }
}

// ---- errors -----------------------------------------------------------------------------------

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

enum Failure {
    /// Nothing the server can decode to edit (HEIC without the phone's rendering, a damaged file).
    NoSource,
    /// The original is not reachable right now (the big disk is asleep or unmounted).
    Unreachable,
    Other(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(e: E) -> Self {
        Failure::Other(e.into())
    }
}

impl From<Failure> for ApiError {
    fn from(f: Failure) -> Self {
        match f {
            Failure::NoSource => ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "this picture can't be decoded for editing".into(),
            ),
            Failure::Unreachable => ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "original is not reachable right now".into(),
            ),
            Failure::Other(e) => ApiError::from(e),
        }
    }
}

// ---- the item ---------------------------------------------------------------------------------

async fn load_photo(lib: &Library, id: &str) -> ApiResult<db::Item> {
    let iid = id.to_owned();
    lib.db
        .call(move |c| db::get_item(c, &iid))
        .await?
        .ok_or_else(not_found)
}

fn check_editable(item: &db::Item) -> ApiResult<()> {
    if item.kind != "photo" {
        return Err(bad("only photos can be edited"));
    }
    if item.trashed_at.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the item is in the Trash".into(),
        ));
    }
    Ok(())
}

struct Stored {
    edits: EditList,
    updated_at: String,
}

fn stored(c: &Connection, id: &str) -> rusqlite::Result<Option<Stored>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT edits, updated_at FROM item_edits WHERE item_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // A row written by a newer build, or damaged, is ignored: the item shows as unedited.
    Ok(row.and_then(|(json, updated_at)| {
        EditList::from_json(&json)
            .ok()
            .map(|edits| Stored { edits, updated_at })
    }))
}

async fn state_json(lib: &Library, id: &str) -> ApiResult<Json<Value>> {
    let item = load_photo(lib, id).await?;
    let iid = id.to_owned();
    let row = lib.db.call(move |c| stored(c, &iid)).await?;
    let edited = stamp(&lib.cfg, id).is_some();
    let (edits, updated_at) = match row {
        Some(s) => (Some(s.edits), Some(s.updated_at)),
        None => (None, None),
    };
    Ok(Json(json!({
        "edited": edited,
        "edits": edits,
        "updated_at": updated_at,
        "base_url": format!("/v1/media/items/{id}/edits/base"),
        "item": item_out(lib, item, None),
    })))
}

// ---- routes -----------------------------------------------------------------------------------

async fn get_edits(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state_json(&lib, &id).await
}

async fn put_edits(
    State(lib): State<Library>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let text = std::str::from_utf8(&body).map_err(|_| bad("the body must be UTF-8 JSON"))?;
    let list = EditList::from_json(text).map_err(|e| bad(e.0))?;
    let item = load_photo(&lib, &id).await?;
    check_editable(&item)?;
    if list.is_identity() {
        return revert(&lib, &id).await;
    }
    let permit = lib.jobs.clone().acquire_owned().await?;
    let l = lib.clone();
    let iid = id.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        apply(&l, &iid, &list)
    })
    .await?
    .map_err(ApiError::from)?;
    lib.emit("item.updated", &id);
    state_json(&lib, &id).await
}

async fn delete_edits(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    load_photo(&lib, &id).await?;
    revert(&lib, &id).await
}

async fn revert(lib: &Library, id: &str) -> ApiResult<Json<Value>> {
    let permit = lib.jobs.clone().acquire_owned().await?;
    let l = lib.clone();
    let iid = id.to_owned();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        undo(&l, &iid)
    })
    .await?
    .map_err(ApiError::from)?;
    lib.emit("item.updated", id);
    state_json(lib, id).await
}

/// The unedited picture at 1600 px: the item's own large rendition while it is not edited, the
/// editor's working copy once it is.
async fn get_base(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let item = load_photo(&lib, &id).await?;
    if item.kind != "photo" {
        return Err(bad("only photos can be edited"));
    }
    let d = dir(&lib.cfg, &id);
    // The item's own large rendition is the unedited picture while it has no edit (and exists); otherwise the
    // editor's working copy, made on first use.
    let (file, path) = if stamp(&lib.cfg, &id).is_none() && d.join("l.jpg").is_file() {
        ("l.jpg", d.join("l.jpg"))
    } else {
        let path = d.join(BASE_FILE);
        if !path.is_file() {
            let permit = lib.jobs.clone().acquire_owned().await?;
            let l = lib.clone();
            let iid = id.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                make_base(&l, &iid)
            })
            .await?
            .map_err(ApiError::from)?;
        }
        (BASE_FILE, path)
    };
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(
        headers,
        path,
        "image/jpeg",
        &format!("{}-{}-{file}", item.hash, item.version),
        "private, max-age=3600",
    )
    .await
}

async fn download_edited(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let item = load_photo(&lib, &id).await?;
    let stamp = stamp(&lib.cfg, &id).ok_or_else(not_found)?;
    let path = dir(&lib.cfg, &id).join(EDITED_FILE);
    let mut res = serve_file(
        headers,
        path,
        "image/jpeg",
        &format!("{}-{}-edited-e{stamp}", item.hash, item.version),
        "private, no-cache",
    )
    .await?;
    let stem = FsPath::new(&item.title)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("photo");
    if let Some(v) = content_disposition("attachment", &format!("{stem}-edited.jpg")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(res)
}

// ---- the work (blocking) ----------------------------------------------------------------------

fn frame_of(img: DynamicImage) -> Frame {
    let rgba = img.into_rgba8();
    Frame {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        complete: true,
    }
}

fn write_jpeg(dst: &FsPath, f: &Frame) -> anyhow::Result<()> {
    let rgb: Vec<u8> = f
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let tmp = dst.with_extension("jpg.part");
    {
        let file = std::fs::File::create(&tmp)?;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::io::BufWriter::new(file),
            EDITED_QUALITY,
        );
        enc.encode(&rgb, f.width, f.height, image::ExtendedColorType::Rgb8)?;
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// The original and the picture the server decodes for it (EXIF orientation applied).
fn source_of(lib: &Library, id: &str) -> Result<(PathBuf, db::Item, PathBuf), Failure> {
    let (original, item) = ingest::original_path(lib, id)?.ok_or(Failure::NoSource)?;
    let d = dir(&lib.cfg, id);
    // The phone's rendering stands in for formats we can't decode; otherwise the original must be there.
    let source = thumbs::display_source(&d, &original).ok_or_else(|| {
        if original.is_file() {
            Failure::NoSource
        } else {
            Failure::Unreachable
        }
    })?;
    Ok((original, item, source))
}

fn write_base(d: &FsPath, img: &DynamicImage) -> anyhow::Result<()> {
    let small = thumbs::resize_long(img, BASE_PX).to_rgb8();
    let frame = Frame {
        width: small.width(),
        height: small.height(),
        rgba: small
            .pixels()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        complete: true,
    };
    write_jpeg(&d.join(BASE_FILE), &frame)
}

fn make_base(lib: &Library, id: &str) -> Result<(), Failure> {
    let (_, _, source) = source_of(lib, id)?;
    let img = thumbs::load_oriented(&source).map_err(|_| Failure::NoSource)?;
    write_base(&dir(&lib.cfg, id), &img)?;
    Ok(())
}

/// Renditions of the item from whatever its photo is now (`thumbs::generate` prefers the
/// derivative), and the catalog row that says so.
fn remake_renditions(lib: &Library, id: &str, original: &FsPath) -> anyhow::Result<()> {
    match thumbs::generate(&lib.cfg, id, "photo", original, None) {
        Ok(state) => {
            let hash = match &state {
                thumbs::ThumbState::Ready(h) => Some(h.clone()),
                thumbs::ThumbState::None => None,
            };
            lib.db.with(|c| {
                c.execute(
                    "UPDATE items SET thumb_state = ?2, thumbhash = COALESCE(?3, thumbhash) WHERE id = ?1",
                    params![id, state.as_str(), hash],
                )
            })?;
        }
        Err(e) => {
            // The thumbnail worker tries again.
            tracing::warn!(item = %id, error = %e, "renditions after an edit failed");
            lib.db.with(|c| {
                c.execute(
                    "UPDATE items SET thumb_state = 'pending' WHERE id = ?1",
                    [id],
                )
            })?;
        }
    }
    Ok(())
}

fn apply(lib: &Library, id: &str, list: &EditList) -> Result<(), Failure> {
    let (original, item, source) = source_of(lib, id)?;
    let d = dir(&lib.cfg, id);
    std::fs::create_dir_all(&d)?;
    let img = thumbs::load_oriented(&source).map_err(|_| Failure::NoSource)?;
    if !d.join(BASE_FILE).is_file() {
        write_base(&d, &img)?;
    }
    let frame = frame_of(img);
    let out = edit::render(&frame, list).map_err(|e| anyhow::anyhow!("render: {}", e.0))?;
    drop(frame);
    write_jpeg(&d.join(EDITED_FILE), &out)?;
    let json = serde_json::to_string(list)?;
    let (w, h) = (out.width as i64, out.height as i64);
    lib.db.with(|c| {
        let tx = c.transaction()?;
        // The size before the first edit is kept across later ones.
        tx.execute(
            "INSERT INTO item_edits (item_id, edits, orig_width, orig_height, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id) DO UPDATE SET edits = excluded.edits, updated_at = excluded.updated_at",
            params![id, json, item.width, item.height, now_str()],
        )?;
        tx.execute("UPDATE items SET width = ?2, height = ?3, updated_at = ?4 WHERE id = ?1", params![id, w, h, now_str()])?;
        tx.commit()
    })?;
    remake_renditions(lib, id, &original)?;
    Ok(())
}

fn undo(lib: &Library, id: &str) -> Result<(), Failure> {
    let d = dir(&lib.cfg, id);
    let had_file = d.join(EDITED_FILE).is_file();
    let dims: Option<(Option<i64>, Option<i64>)> = lib.db.with(|c| {
        c.query_row(
            "SELECT orig_width, orig_height FROM item_edits WHERE item_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    })?;
    if dims.is_none() && !had_file {
        return Ok(());
    }
    for f in [EDITED_FILE, BASE_FILE] {
        let _ = std::fs::remove_file(d.join(f));
    }
    lib.db.with(|c| {
        let tx = c.transaction()?;
        tx.execute("DELETE FROM item_edits WHERE item_id = ?1", [id])?;
        if let Some((w, h)) = dims {
            tx.execute(
                "UPDATE items SET width = ?2, height = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, w, h, now_str()],
            )?;
        }
        tx.commit()
    })?;
    if let Some((original, _)) = ingest::original_path(lib, id)? {
        remake_renditions(lib, id, &original)?;
    }
    Ok(())
}

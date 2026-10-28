//! Every stored form of one item, for the Gallery's "Server copy / Device copy" view and its compare mode.
//!
//! - `GET items/{id}/renditions`: the original (format, type, size, blake3, size in pixels, where it is
//!   kept), older versions, the display renditions (grid, preview, thumb, large, poster, and the rendering
//!   the phone sent with the upload) and, when the item came from a device, what that device reported at
//!   upload. A host path is never in the answer.
//! - `GET items/{id}/device-preview`: the JPEG the phone rendered for the upload (HEIC and RAW originals
//!   cannot be decoded here, so this is what the server shows for them).
//! - `original?version=N` (in `http.rs`) serves an older version through [`version_file`].
//!
//! What the device reported is what the upload carried (`uploads` rows: the file name, the declared size,
//! the blake3 the app computed, the client metadata); a phone that sent no hash has none here. Nothing in
//! this module reads the phone itself.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::db::{self, Item};
use crate::http::{rendition_url, serve_file, valid_item_id, ApiError, ApiResult};
use crate::ingest::Library;
use crate::meta::{self, ClientMeta};
use crate::thumbs;

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/items/{id}/renditions", get(renditions))
        .route("/v1/media/items/{id}/device-preview", get(device_preview))
}

/// The upload-side index: finished uploads are looked up by the item they produced.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE INDEX IF NOT EXISTS uploads_item ON uploads(item_id);")
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

/// A short format name for a stored file: its extension, with the usual spellings merged.
pub fn format_of(ext: &str, mime: &str) -> String {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        "jpg" | "jpeg" | "jpe" => "jpeg".into(),
        "heic" | "heif" => "heic".into(),
        "tif" | "tiff" => "tiff".into(),
        "" | "bin" => mime.rsplit('/').next().unwrap_or("bin").to_string(),
        _ => e,
    }
}

/// Width and height from the header of a bare JPEG XL codestream (what `thumbs::encode_jxl` writes);
/// `None` for anything else, including the container form.
pub fn jxl_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    struct Bits<'a> {
        b: &'a [u8],
        pos: usize,
    }
    impl Bits<'_> {
        fn bits(&mut self, n: usize) -> Option<u32> {
            let mut v = 0u64;
            for i in 0..n {
                let byte = *self.b.get(self.pos / 8)?;
                v |= u64::from((byte >> (self.pos % 8)) & 1) << i;
                self.pos += 1;
            }
            Some(v as u32)
        }
        /// `U32(Bits(9), Bits(13), Bits(18), Bits(30))` with an offset of 1, as the size header uses.
        fn size(&mut self) -> Option<u32> {
            let sel = self.bits(2)?;
            let n = [9, 13, 18, 30][sel as usize];
            self.bits(n)?.checked_add(1)
        }
    }
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0x0A {
        return None;
    }
    let mut r = Bits { b: &b[2..], pos: 0 };
    let small = r.bits(1)? == 1;
    let y = if small {
        (r.bits(5)? + 1) * 8
    } else {
        r.size()?
    };
    let ratio = r.bits(3)?;
    let x = match ratio {
        0 if small => (r.bits(5)? + 1) * 8,
        0 => r.size()?,
        1 => y,
        2 => (u64::from(y) * 12 / 10) as u32,
        3 => (u64::from(y) * 4 / 3) as u32,
        4 => (u64::from(y) * 3 / 2) as u32,
        5 => (u64::from(y) * 16 / 9) as u32,
        6 => (u64::from(y) * 5 / 4) as u32,
        _ => y.checked_mul(2)?,
    };
    (x > 0 && y > 0).then_some((x, y))
}

/// Pixel size of a rendition file: JPEG XL by its header, the rest through the `image` crate.
fn file_dimensions(path: &FsPath) -> Option<(u32, u32)> {
    if path.extension().is_some_and(|e| e == "jxl") {
        use std::io::Read;
        let mut head = [0u8; 64];
        let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
        return jxl_dimensions(&head[..n]);
    }
    image::image_dimensions(path).ok()
}

/// An older version's blob: `(path, mime, hash)`; `None` when the item has no such version or its
/// bytes are not in the catalog. The type is the one the blob's extension names.
pub async fn version_file(
    lib: Library,
    item_id: String,
    version: i64,
) -> anyhow::Result<Option<(PathBuf, String, String)>> {
    tokio::task::spawn_blocking(move || {
        let found = lib.db.with(|c| {
            let hash: Option<String> = c
                .query_row(
                    "SELECT hash FROM item_versions WHERE item_id = ?1 AND version = ?2",
                    params![item_id, version],
                    |r| r.get(0),
                )
                .optional()?;
            match hash {
                Some(h) => Ok(db::blob(c, &h)?),
                None => Ok(None),
            }
        })?;
        Ok(found.map(|b| {
            let path = crate::store::path_of(&lib.cfg, &b);
            let mime = meta::mime_for(FsPath::new(&format!("x.{}", b.ext)), None);
            (path, mime, b.hash)
        }))
    })
    .await?
}

struct Upload {
    filename: String,
    declared: i64,
    expected_hash: Option<String>,
    meta: ClientMeta,
    created_at: String,
}

fn uploads_of(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<Upload>> {
    let mut st = c.prepare_cached(
        "SELECT filename, declared, expected_hash, meta, created_at FROM uploads \
         WHERE item_id = ?1 AND finished = 1 ORDER BY created_at, id",
    )?;
    let rows = st.query_map([item_id], |r| {
        let meta: String = r.get(3)?;
        Ok(Upload {
            filename: r.get(0)?,
            declared: r.get(1)?,
            expected_hash: r.get(2)?,
            meta: serde_json::from_str(&meta).unwrap_or_default(),
            created_at: r.get(4)?,
        })
    })?;
    rows.collect()
}

struct Version {
    version: i64,
    hash: String,
    replaced_at: String,
    blob: Option<db::BlobLoc>,
}

fn versions_of(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<Version>> {
    let rows: Vec<(i64, String, String)> = {
        let mut st = c.prepare_cached(
            "SELECT version, hash, replaced_at FROM item_versions WHERE item_id = ?1 ORDER BY version",
        )?;
        let it = st.query_map([item_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        it.collect::<rusqlite::Result<_>>()?
    };
    rows.into_iter()
        .map(|(version, hash, replaced_at)| {
            Ok(Version {
                blob: db::blob(c, &hash)?,
                version,
                hash,
                replaced_at,
            })
        })
        .collect()
}

fn size_of(path: &FsPath) -> Option<u64> {
    std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// The display renditions that exist for an item, in the order a viewer would want them.
fn rendition_list(lib: &Library, it: &Item) -> Vec<Value> {
    let dir = thumbs::dir_for(&lib.cfg, &it.id);
    let id = &it.id;
    let v = it.version;
    let table: [(&str, &str, &str, String); 6] = [
        ("grid", "g.jxl", "image/jxl", rendition_url(lib, id, "grid", v)),
        ("preview", "p.jxl", "image/jxl", rendition_url(lib, id, "preview", v)),
        ("thumb", "s.jpg", "image/jpeg", format!("/v1/media/items/{id}/thumb?size=256&v={v}")),
        ("large", "l.jpg", "image/jpeg", format!("/v1/media/items/{id}/thumb?size=1024&v={v}")),
        ("poster", "poster.jpg", "image/jpeg", format!("/v1/media/items/{id}/poster?v={v}")),
        (
            "device_preview",
            "preview.jpg",
            "image/jpeg",
            format!("/v1/media/items/{id}/device-preview?v={v}"),
        ),
    ];
    table
        .into_iter()
        .filter_map(|(name, file, mime, url)| {
            let path = dir.join(file);
            let bytes = size_of(&path)?;
            let dims = file_dimensions(&path);
            Some(json!({
                "name": name,
                "format": format_of(file.rsplit('.').next().unwrap_or(""), mime),
                "mime": mime,
                "bytes": bytes,
                "width": dims.map(|d| d.0),
                "height": dims.map(|d| d.1),
                "url": url,
            }))
        })
        .collect()
}

fn device_out(
    c: &Connection,
    it: &Item,
    asset_id: Option<String>,
    uploads: &[Upload],
    versions: &[Version],
) -> rusqlite::Result<Option<Value>> {
    let upload_device = uploads.iter().find_map(|u| u.meta.device.clone());
    let Some(device_id) = it.device.clone().or(upload_device) else {
        return Ok(None);
    };
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT name, kind, last_seen_at FROM devices WHERE id = ?1",
            [&device_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    // Which stored version a reported hash names: the current one, or an older one.
    let version_of = |h: &str| -> Option<i64> {
        if h.eq_ignore_ascii_case(&it.hash) {
            return Some(it.version);
        }
        versions
            .iter()
            .find(|v| v.hash.eq_ignore_ascii_case(h))
            .map(|v| v.version)
    };
    let reported: Vec<Value> = uploads
        .iter()
        .map(|u| {
            let m = &u.meta;
            json!({
                "filename": m.filename.clone().unwrap_or_else(|| u.filename.clone()),
                "mime": m.mime,
                "uti": m.uti,
                "bytes": u.declared,
                "blake3": u.expected_hash,
                "width": m.width,
                "height": m.height,
                "taken_at": m.taken_at,
                "uploaded_at": u.created_at,
                "matches_version": u.expected_hash.as_deref().and_then(version_of),
            })
        })
        .collect();
    Ok(Some(json!({
        "id": device_id,
        "name": row.as_ref().map(|r| r.0.clone()),
        "kind": row.as_ref().map(|r| r.1.clone()),
        "last_seen_at": row.as_ref().map(|r| r.2.clone()),
        "asset_id": asset_id,
        "uploads": reported,
        "latest": reported.last(),
    })))
}

/// The whole answer, or `None` for an unknown item.
fn build(lib: &Library, id: &str) -> anyhow::Result<Option<Value>> {
    let found = lib.db.with(|c| {
        let Some(it) = db::get_item(c, id)? else {
            return Ok(None);
        };
        let asset: Option<String> = c.query_row("SELECT asset_id FROM items WHERE id = ?1", [id], |r| r.get(0))?;
        let blob = db::blob(c, &it.hash)?;
        let versions = versions_of(c, id)?;
        let uploads = uploads_of(c, id)?;
        let device = device_out(c, &it, asset, &uploads, &versions)?;
        Ok(Some((it, blob, versions, device)))
    })?;
    let Some((it, blob, versions, device)) = found else {
        return Ok(None);
    };
    let ext = blob.as_ref().map_or("", |b| b.ext.as_str());
    let reachable = crate::ingest::original_path(lib, id)?
        .and_then(|(p, _)| size_of(&p))
        .is_some();
    let original = json!({
        "version": it.version,
        "format": format_of(ext, &it.mime),
        "mime": it.mime,
        "bytes": it.size,
        "blake3": it.hash,
        "width": it.width,
        "height": it.height,
        // Where the bytes live, never the path: store (the big disk), spool (waiting to move) or
        // external (a file indexed in place).
        "stored": blob.as_ref().map(|b| b.location.clone()),
        "reachable": reachable,
        "url": format!("/v1/media/items/{id}/original"),
    });
    let versions: Vec<Value> = versions
        .iter()
        .map(|v| {
            let ext = v.blob.as_ref().map_or("", |b| b.ext.as_str());
            let mime = meta::mime_for(FsPath::new(&format!("x.{ext}")), None);
            let path = v.blob.as_ref().map(|b| crate::store::path_of(&lib.cfg, b));
            json!({
                "version": v.version,
                "format": format_of(ext, &mime),
                "mime": mime,
                "bytes": v.blob.as_ref().map(|b| b.bytes),
                "blake3": v.hash,
                "replaced_at": v.replaced_at,
                "stored": v.blob.as_ref().map(|b| b.location.clone()),
                "reachable": path.as_deref().and_then(size_of).is_some(),
                "url": format!("/v1/media/items/{id}/original?version={}", v.version),
            })
        })
        .collect();
    Ok(Some(json!({
        "item_id": it.id,
        "kind": it.kind,
        "original": original,
        "versions": versions,
        "renditions": rendition_list(lib, &it),
        "device": device,
    })))
}

async fn renditions(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !valid_item_id(&id) {
        return Err(not_found());
    }
    let out = tokio::task::spawn_blocking(move || build(&lib, &id)).await??;
    out.map(Json).ok_or_else(not_found)
}

async fn device_preview(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if !valid_item_id(&id) {
        return Err(not_found());
    }
    let iid = id.clone();
    let it = lib
        .db
        .call(move |c| db::get_item(c, &iid))
        .await?
        .ok_or_else(not_found)?;
    let path = thumbs::dir_for(&lib.cfg, &id).join("preview.jpg");
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(
        headers,
        path,
        "image/jpeg",
        &format!("{}-{}-device-preview", it.hash, it.version),
        "private, max-age=604800",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([90, 120, 30])))
    }

    #[test]
    fn jxl_header_sizes_round_trip() {
        // Small (multiples of 8, 256 at most), ratio-coded (4:3, 3:2, 16:9, 2:1) and free sizes, square or not.
        for (w, h) in [(256, 256), (40, 30), (48, 32), (160, 90), (200, 100), (64, 8), (7, 5), (300, 17)] {
            let bytes = thumbs::encode_jxl(&rgb(w, h), false).unwrap();
            assert_eq!(jxl_dimensions(&bytes), Some((w, h)), "{w}x{h}");
        }
        // The progressive preview, a size above the 256 px "small" form.
        let progressive = thumbs::encode_jxl(&rgb(600, 400), true).unwrap();
        assert_eq!(jxl_dimensions(&progressive), Some((600, 400)));
    }

    #[test]
    fn jxl_header_refuses_what_is_not_a_codestream() {
        assert_eq!(jxl_dimensions(b""), None);
        assert_eq!(jxl_dimensions(b"\xFF\xD8\xFF\xE0"), None);
        assert_eq!(jxl_dimensions(&[0xFF, 0x0A]), None);
        // The ISO container starts with a box, not the codestream signature.
        assert_eq!(jxl_dimensions(b"\0\0\0\x0cJXL \r\n\x87\n"), None);
    }

    #[test]
    fn formats_merge_spellings() {
        assert_eq!(format_of("JPG", "image/jpeg"), "jpeg");
        assert_eq!(format_of(".jpeg", "image/jpeg"), "jpeg");
        assert_eq!(format_of("heif", "image/heic"), "heic");
        assert_eq!(format_of("tif", "image/tiff"), "tiff");
        assert_eq!(format_of("bin", "video/quicktime"), "quicktime");
        assert_eq!(format_of("dng", "image/x-adobe-dng"), "dng");
    }
}

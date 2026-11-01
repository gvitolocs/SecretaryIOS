//! Spreadsheets: open, show and edit `.xlsx` workbooks of the Drive and the library
//! (docs/xlsx.md). The workbook logic is `secretary-xlsx`; this module finds the file, checks the
//! owner is editing the bytes he saw, keeps the previous bytes as a version and puts the new ones
//! in place.
//!
//! - `GET items/{id}/sheet`: the workbook view (sheets, cells, styles, inputs, the AU timesheet
//!   form when the file is one), plus `hash` (blake3 of the bytes read), `version`, `writable`.
//! - `POST items/{id}/sheet {base_hash, edits: [{sheet, cell, value: {t, v}}]}`: apply the edits.
//!   `409 conflict` when the file is no longer `base_hash`; `400` with `code` (`formula_cell`,
//!   `locked_cell`, `invalid_value`, …), `sheet`, `cell` when an edit is refused; else the new view
//!   with `changed` and the new `version`.
//! - `GET items/{id}/sheet/versions`: the earlier versions (each downloads as
//!   `items/{id}/original?version=N&download=1`).
//! - `POST items/{id}/sheet/restore {version, base_hash}`: make an earlier version current (the
//!   current bytes become a version too, so a restore can be undone).
//!
//! A save never loses bytes: the old file goes into the store as a blob of the item's previous
//! version before anything is replaced; a Drive file is then replaced atomically (a hidden temp
//! file next to it, fsync, rename) and indexed again at once; a library file gets a new blob.

use std::io::Write;
use std::path::Path as FsPath;
use std::sync::OnceLock;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use secretary_xlsx::{Edit, Options};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{now_str, Item};
use crate::doctype::DocType;
use crate::drive::{self, DriveInfo};
use crate::http::{doc_item, ApiError};
use crate::ingest::Library;
use crate::store;

/// Workbooks larger than this are not opened as sheets (the timesheet is 1.2 MB).
pub const MAX_SHEET_BYTES: u64 = 64 << 20;
/// Edits in one save.
pub const MAX_EDITS: usize = 5000;

pub fn routes() -> Router<Library> {
    Router::new()
        .route(
            "/v1/media/items/{id}/sheet",
            get(get_sheet).post(save_sheet),
        )
        .route("/v1/media/items/{id}/sheet/versions", get(list_versions))
        .route("/v1/media/items/{id}/sheet/restore", post(restore_version))
}

/// The extensions that open as sheets (`.xls`, `.ods`, `.numbers` keep the PDF preview).
pub fn sheet_ext(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "xlsx" | "xlsm" | "xltx" | "xltm")
}

/// One save at a time: a save reads, keeps, replaces and indexes a file.
fn save_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// An error answer: its status and JSON body (`error` is the code clients match on).
struct SheetError(StatusCode, Value);

impl IntoResponse for SheetError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

type SheetResult<T> = Result<T, SheetError>;

fn err(status: StatusCode, code: &str, message: impl Into<String>) -> SheetError {
    SheetError(status, json!({ "error": code, "message": message.into() }))
}

fn api(e: ApiError) -> SheetError {
    SheetError(e.0, json!({ "error": e.1 }))
}

fn xlsx_err(e: secretary_xlsx::Error) -> SheetError {
    let status = if e.is_request_error() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut body = json!({ "error": e.code(), "message": e.to_string() });
    match &e {
        secretary_xlsx::Error::FormulaCell { sheet, cell }
        | secretary_xlsx::Error::LockedCell { sheet, cell }
        | secretary_xlsx::Error::Invalid { sheet, cell, .. } => {
            body["sheet"] = json!(sheet);
            body["cell"] = json!(cell);
        }
        secretary_xlsx::Error::UnknownSheet(s) => body["sheet"] = json!(s),
        secretary_xlsx::Error::BadCell(c) => body["cell"] = json!(c),
        _ => {}
    }
    SheetError(status, body)
}

fn internal(e: impl std::fmt::Display) -> SheetError {
    tracing::warn!(error = %e, "sheet request failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
}

struct Opened {
    item: Item,
    info: Option<DriveInfo>,
    name: String,
    path: std::path::PathBuf,
    bytes: Vec<u8>,
    hash: String,
}

async fn open(lib: &Library, id: &str) -> SheetResult<Opened> {
    let (item, info, doc, name, path) = doc_item(lib, id).await.map_err(api)?;
    if doc != DocType::Spreadsheet || !sheet_ext(&name) {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported",
            "only .xlsx and .xlsm workbooks open as sheets",
        ));
    }
    let md = tokio::fs::metadata(&path).await.map_err(internal)?;
    if md.len() > MAX_SHEET_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the workbook is larger than 64 MB",
        ));
    }
    let bytes = tokio::fs::read(&path).await.map_err(internal)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    Ok(Opened {
        item,
        info,
        name,
        path,
        bytes,
        hash,
    })
}

/// Whether a save can replace the file: a Drive file in a directory this service may write, or
/// a library file (the store, or the spool while it is away).
fn writable(o: &Opened) -> bool {
    match &o.info {
        Some(_) => {
            let dir = o.path.parent().unwrap_or(FsPath::new("/"));
            let ok = |p: &FsPath| {
                use std::os::unix::ffi::OsStrExt;
                let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
                    return false;
                };
                // SAFETY: a valid NUL-terminated path; access() only reads it.
                unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
            };
            ok(dir) && ok(&o.path)
        }
        None => true,
    }
}

async fn view(o: Opened) -> SheetResult<Value> {
    let writable = writable(&o);
    let Opened {
        item,
        info,
        name,
        bytes,
        hash,
        ..
    } = o;
    let mut v = tokio::task::spawn_blocking(move || {
        secretary_xlsx::read(&bytes).map(|wb| secretary_xlsx::view::workbook_json(&wb))
    })
    .await
    .map_err(internal)?
    .map_err(xlsx_err)?;
    v["item_id"] = json!(item.id);
    v["name"] = json!(name);
    v["hash"] = json!(hash);
    v["version"] = json!(item.version);
    v["writable"] = json!(writable);
    v["location"] = match &info {
        Some(d) => json!({ "kind": "drive", "folder": d.folder, "path": d.path }),
        None => json!({ "kind": "library" }),
    };
    v["download_url"] = json!(format!("/v1/media/items/{}/original?download=1", item.id));
    Ok(v)
}

async fn get_sheet(State(lib): State<Library>, Path(id): Path<String>) -> SheetResult<Json<Value>> {
    let o = open(&lib, &id).await?;
    Ok(Json(view(o).await?))
}

#[derive(Deserialize)]
struct SaveReq {
    base_hash: String,
    edits: Vec<Edit>,
}

async fn save_sheet(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<SaveReq>,
) -> SheetResult<Json<Value>> {
    if req.edits.len() > MAX_EDITS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "too_many_edits",
            format!("at most {MAX_EDITS} edits per save"),
        ));
    }
    let _guard = save_lock().lock().await;
    let o = open(&lib, &id).await?;
    if o.hash != req.base_hash {
        return Err(SheetError(
            StatusCode::CONFLICT,
            json!({
                "error": "conflict",
                "message": "the file changed since it was opened; reopen it and apply the edits again",
                "hash": o.hash,
            }),
        ));
    }
    if !writable(&o) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "read_only",
            "the file's folder is read-only",
        ));
    }
    let bytes = o.bytes.clone();
    let edits = req.edits;
    let out = tokio::task::spawn_blocking(move || {
        secretary_xlsx::apply(&bytes, &edits, &Options::default())
    })
    .await
    .map_err(internal)?
    .map_err(xlsx_err)?;
    if out.parts.is_empty() {
        let mut v = view(o).await?;
        v["changed"] = json!([]);
        return Ok(Json(v));
    }
    let changed = out.changed.clone();
    commit(&lib, o, out.bytes).await?;
    tracing::info!(item = %id, cells = changed.len(), parts = ?out.parts, "sheet saved");
    let o = open(&lib, &id).await?;
    let mut v = view(o).await?;
    v["changed"] = json!(changed);
    Ok(Json(v))
}

/// Keep the current bytes as the item's previous version, then make `new` current.
async fn commit(lib: &Library, o: Opened, new: Vec<u8>) -> SheetResult<()> {
    let l = lib.clone();
    let id = o.item.id.clone();
    tokio::task::spawn_blocking(move || commit_blocking(&l, &o, &new))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    lib.emit("item.updated", &id);
    Ok(())
}

fn ext_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_else(|| "xlsx".into())
}

/// Write `bytes` to a hidden temp file in `dir` and fsync it.
fn temp_with(dir: &FsPath, tag: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".sheet-{tag}-{}.part",
        uuid::Uuid::new_v4().simple()
    ));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(tmp)
}

/// Make sure the library holds these bytes as a blob in the store (or the spool): the previous
/// version of a Drive file had them only on disk in the folder.
fn keep_blob(lib: &Library, hash: &str, ext: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let have: Option<String> = lib.db.with(|c| {
        c.query_row("SELECT location FROM blobs WHERE hash = ?1", [hash], |r| {
            r.get(0)
        })
        .optional()
    })?;
    if let Some(loc) = have.as_deref().filter(|l| *l == "store" || *l == "spool") {
        let b = crate::db::BlobLoc {
            hash: hash.into(),
            ext: ext.into(),
            location: loc.into(),
            path: None,
            bytes: 0,
        };
        if store::path_of(&lib.cfg, &b).is_file() {
            return Ok(());
        }
    }
    let tmp = temp_with(&lib.cfg.uploads_dir(), "keep", bytes)?;
    let loc = store::place_new(&lib.cfg, &tmp, hash, ext)?;
    lib.db.with(|c| {
        c.execute(
            "INSERT INTO blobs (hash, bytes, ext, location, path, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT(hash) DO UPDATE SET location = excluded.location, path = NULL, ext = excluded.ext",
            params![hash, bytes.len() as i64, ext, loc, now_str()],
        )
    })?;
    Ok(())
}

/// Record the current hash as version N and make the item version N + 1 (with `new` as its
/// hash and size, for a library file; a Drive file gets them from the index).
fn bump_version(
    lib: &Library,
    item_id: &str,
    old_hash: &str,
    new: Option<(&str, i64)>,
) -> anyhow::Result<i64> {
    let v = lib.db.with(|c| {
        let tx = c.transaction()?;
        let version: i64 = tx.query_row("SELECT version FROM items WHERE id = ?1", [item_id], |r| r.get(0))?;
        tx.execute(
            "INSERT OR REPLACE INTO item_versions (item_id, version, hash, replaced_at) VALUES (?1, ?2, ?3, ?4)",
            params![item_id, version, old_hash, now_str()],
        )?;
        match new {
            Some((hash, bytes)) => tx.execute(
                "UPDATE items SET version = ?2, hash = ?3, bytes = ?4, updated_at = ?5 WHERE id = ?1",
                params![item_id, version + 1, hash, bytes, now_str()],
            )?,
            None => tx.execute(
                "UPDATE items SET version = ?2, updated_at = ?3 WHERE id = ?1",
                params![item_id, version + 1, now_str()],
            )?,
        };
        tx.commit()?;
        Ok::<_, rusqlite::Error>(version + 1)
    })?;
    Ok(v)
}

/// Replace a file atomically, keeping its permissions: a hidden temp file next to it (the Drive
/// scan skips hidden files), fsync, rename, fsync the directory.
fn replace_file(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    let perm = std::fs::metadata(path)?.permissions();
    let tmp = temp_with(dir, "save", bytes)?;
    let done = std::fs::set_permissions(&tmp, perm).and_then(|_| std::fs::rename(&tmp, path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done?;
    std::fs::File::open(dir)?.sync_all()
}

fn commit_blocking(lib: &Library, o: &Opened, new: &[u8]) -> anyhow::Result<()> {
    let ext = ext_of(&o.name);
    // 1. The bytes the owner saw stay in the library as the previous version.
    keep_blob(lib, &o.hash, &ext, &o.bytes)?;
    match &o.info {
        Some(info) => {
            bump_version(lib, &o.item.id, &o.hash, None)?;
            replace_file(&o.path, new)?;
            drive::reindex_file(lib, &info.folder, &info.path, &o.path)?;
        }
        None => {
            let hash = blake3::hash(new).to_hex().to_string();
            let tmp = temp_with(&lib.cfg.uploads_dir(), "new", new)?;
            let loc = store::place_new(&lib.cfg, &tmp, &hash, &ext)?;
            lib.db.with(|c| {
                c.execute(
                    "INSERT INTO blobs (hash, bytes, ext, location, path, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5)
                     ON CONFLICT(hash) DO NOTHING",
                    params![hash, new.len() as i64, ext, loc, now_str()],
                )
            })?;
            bump_version(lib, &o.item.id, &o.hash, Some((&hash, new.len() as i64)))?;
            drive::drop_derived(lib, &o.item.id);
        }
    }
    Ok(())
}

async fn list_versions(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> SheetResult<Json<Value>> {
    let o = open(&lib, &id).await?;
    let iid = o.item.id.clone();
    let rows: Vec<(i64, String, String, Option<i64>)> = lib
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT v.version, v.hash, v.replaced_at, b.bytes FROM item_versions v
                 LEFT JOIN blobs b ON b.hash = v.hash WHERE v.item_id = ?1 ORDER BY v.version DESC",
            )?;
            let it = st.query_map([iid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.collect()
        })
        .await
        .map_err(internal)?;
    let versions: Vec<Value> = rows
        .into_iter()
        .map(|(version, hash, replaced_at, bytes)| {
            json!({
                "version": version,
                "hash": hash,
                "replaced_at": replaced_at,
                "bytes": bytes,
                "download_url": format!("/v1/media/items/{}/original?version={version}&download=1", o.item.id),
            })
        })
        .collect();
    Ok(Json(json!({
        "item_id": o.item.id,
        "current": { "version": o.item.version, "hash": o.hash, "bytes": o.bytes.len() },
        "versions": versions,
    })))
}

#[derive(Deserialize)]
struct RestoreReq {
    version: i64,
    base_hash: String,
}

async fn restore_version(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<RestoreReq>,
) -> SheetResult<Json<Value>> {
    let _guard = save_lock().lock().await;
    let o = open(&lib, &id).await?;
    if o.hash != req.base_hash {
        return Err(err(
            StatusCode::CONFLICT,
            "conflict",
            "the file changed since it was opened",
        ));
    }
    if !writable(&o) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "read_only",
            "the file's folder is read-only",
        ));
    }
    let (path, _mime, hash) =
        crate::renditions::version_file(lib.clone(), o.item.id.clone(), req.version)
            .await
            .map_err(internal)?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "not_found", "no such version"))?;
    let old = tokio::fs::read(&path).await.map_err(internal)?;
    if blake3::hash(&old).to_hex().as_str() != hash {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "damaged",
            "the stored version does not match its hash",
        ));
    }
    let check = old.clone();
    tokio::task::spawn_blocking(move || secretary_xlsx::read(&check).map(|_| ()))
        .await
        .map_err(internal)?
        .map_err(xlsx_err)?;
    if hash != o.hash {
        commit(&lib, o, old).await?;
    }
    let o = open(&lib, &id).await?;
    Ok(Json(view(o).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_extensions() {
        assert!(sheet_ext("Giuseppe_Vitolo_worktime_2026_CraftIT.xlsx"));
        assert!(sheet_ext("a.XLSM"));
        assert!(!sheet_ext("old.xls"));
        assert!(!sheet_ext("x.ods"));
        assert!(!sheet_ext("xlsx"));
    }
}

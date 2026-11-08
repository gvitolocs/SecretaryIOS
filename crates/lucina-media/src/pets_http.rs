//! The pets routes (`pets.rs`), all behind the upstream key like the rest of `/v1/media` and never reachable
//! from a share link. They mirror the people routes (`faces_http.rs`):
//!
//! - `GET|PUT pet-crops/settings`, `DELETE pet-crops`: on or off, progress, and "delete pet data".
//! - `GET pets`, `GET|PATCH pets/{id}` (name, hidden), `POST pets/{id}/merge`, `GET pets/{id}/items`,
//!   `GET pets/{id}/thumb`: the Pets page.
//! - `GET items/{id}/pets`, `PUT pet-crops/{id}`, `GET pet-crops/{id}/thumb`, `GET pet-crops/unassigned`: the
//!   animals of a photo and the owner's corrections.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use lucina_media_ml::pets_store::{self as fs, CropRow};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::db::{self, ListQuery};
use crate::http::{bad, ApiError, ApiResult};
use crate::ingest::Library;
use crate::pets as crops;

/// Pets with at least this many crops are listed unless the request says otherwise.
pub const DEFAULT_MIN_CROPS: i64 = 3;
const NAME_MAX: usize = 80;
const DEFAULT_LIMIT: u32 = 60;
const MAX_LIMIT: u32 = 200;
const MAX_MERGE: usize = 100;

pub fn routes() -> Router<Library> {
    Router::new()
        .route(
            "/v1/media/pet-crops/settings",
            get(get_settings).put(put_settings),
        )
        .route("/v1/media/pet-crops", delete(delete_crops))
        .route("/v1/media/pet-crops/unassigned", get(unassigned))
        .route("/v1/media/pet-crops/{id}", put(put_crop))
        .route("/v1/media/pet-crops/{id}/thumb", get(crop_thumb))
        .route("/v1/media/pets", get(list_pets))
        .route("/v1/media/pets/{id}", get(get_pet).patch(patch_pet))
        .route("/v1/media/pets/{id}/merge", post(merge_pet))
        .route("/v1/media/pets/{id}/items", get(pet_items))
        .route("/v1/media/pets/{id}/thumb", get(pet_thumb))
        .route("/v1/media/items/{id}/pets", get(item_crops))
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

/// Ids come from the URL and may end up in file names: only the characters ids are made of.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `"field": null` (clear it) apart from an absent field (leave it).
fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn round5(v: f64) -> f64 {
    (v * 1e5).round() / 1e5
}

// ---- JSON

const PERSON_SELECT: &str = "SELECT p.id, p.name, p.hidden, p.birth_date, p.cover_crop_id, \
     COUNT(i.id), COUNT(DISTINCT i.id) FROM pets p \
     LEFT JOIN pet_crops f ON f.pet_id = p.id \
     LEFT JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0";

fn pet_from_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = r.get(0)?;
    let cover: Option<String> = r.get(4)?;
    Ok(json!({
        "thumb_url": cover.as_ref().map(|c| format!("/v1/media/pets/{id}/thumb?v={c}")),
        "id": id,
        "name": r.get::<_, Option<String>>(1)?,
        "hidden": r.get::<_, i64>(2)? != 0,
        "birth_date": r.get::<_, Option<String>>(3)?,
        "crop_count": r.get::<_, i64>(5)?,
        "item_count": r.get::<_, i64>(6)?,
        "cover_crop_id": cover,
    }))
}

fn pet_json(c: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    c.query_row(
        &format!("{PERSON_SELECT} WHERE p.id = ?1 GROUP BY p.id"),
        [id],
        pet_from_row,
    )
    .optional()
}

fn crop_json(c: &Connection, f: &CropRow) -> rusqlite::Result<Value> {
    let pet = match &f.pet_id {
        Some(pid) => {
            fs::get_pet(c, pid)?.map(|p| json!({ "id": p.id, "name": p.name, "hidden": p.hidden }))
        }
        None => None,
    };
    Ok(json!({
        "id": f.id,
        "item_id": f.item_id,
        "box": { "x": round5(f.x), "y": round5(f.y), "w": round5(f.w), "h": round5(f.h) },
        "score": round5(f.score),
        "source": f.source,
        "pet": pet,
    }))
}

/// The pets shown for a photo: not hidden, and named or with at least the default number of crops.
pub fn pets_of_item(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT DISTINCT p.id, p.name FROM pet_crops f JOIN pets p ON p.id = f.pet_id \
         WHERE f.item_id = ?1 AND p.hidden = 0 AND (p.name IS NOT NULL \
             OR (SELECT COUNT(*) FROM pet_crops g WHERE g.pet_id = p.id) >= ?2) \
         ORDER BY p.name IS NULL, p.name COLLATE NOCASE, p.id",
    )?;
    let rows = st.query_map(params![item_id, DEFAULT_MIN_CROPS], |r| {
        Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)? }))
    })?;
    rows.collect()
}

fn status_json(c: &Connection, lib: &Library) -> rusqlite::Result<Value> {
    let st = crops::state(lib);
    let model = st
        .as_ref()
        .map(|f| f.model.model().to_string())
        .unwrap_or_else(|| lucina_media_ml::pets::MODEL.to_string());
    let (paused_hot, paused_busy) = crate::smart::pauses(lib);
    let pets: i64 = c.query_row(
        "SELECT COUNT(*) FROM (SELECT p.id FROM pets p JOIN pet_crops f ON f.pet_id = p.id \
         JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0 \
         WHERE p.hidden = 0 GROUP BY p.id HAVING COUNT(i.id) >= ?1 OR p.name IS NOT NULL)",
        [DEFAULT_MIN_CROPS],
        |r| r.get(0),
    )?;
    Ok(json!({
        "enabled": fs::enabled(c)?,
        "available": st.is_some(),
        "model": st.is_some().then_some(model.clone()),
        "scanned": fs::count_scans(c, &model)?,
        "pending": if st.is_some() { crops::pending_count(c, &model)? } else { 0 },
        "crops": fs::count_crops(c)?,
        "pets": pets,
        "running": crops::running(lib),
        "paused_hot": paused_hot,
        "paused_busy": paused_busy,
    }))
}

// ---- settings

async fn get_settings(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let l = lib.clone();
    Ok(Json(lib.db.call(move |c| status_json(c, &l)).await?))
}

#[derive(Deserialize)]
struct SettingsReq {
    enabled: bool,
}

async fn put_settings(
    State(lib): State<Library>,
    Json(req): Json<SettingsReq>,
) -> ApiResult<Json<Value>> {
    if req.enabled && crops::state(&lib).is_none() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "pet grouping is not available on this server".into(),
        ));
    }
    let l = lib.clone();
    Ok(Json(
        lib.db
            .call(move |c| {
                fs::set_enabled(c, req.enabled)?;
                status_json(c, &l)
            })
            .await?,
    ))
}

async fn delete_crops(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let l = lib.clone();
    tokio::task::spawn_blocking(move || crops::delete_everything(&l))
        .await?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- pets

#[derive(Deserialize)]
struct PetsQuery {
    min_crops: Option<i64>,
    include_hidden: Option<bool>,
}

async fn list_pets(
    State(lib): State<Library>,
    Query(q): Query<PetsQuery>,
) -> ApiResult<Json<Value>> {
    let min = q.min_crops.unwrap_or(DEFAULT_MIN_CROPS).max(1);
    let hidden = q.include_hidden.unwrap_or(false);
    let pets = lib
        .db
        .call(move |c| {
            let mut st = c.prepare(&format!(
                "{PERSON_SELECT} WHERE (?1 = 1 OR p.hidden = 0) GROUP BY p.id \
                 HAVING COUNT(i.id) > 0 AND (COUNT(i.id) >= ?2 OR p.name IS NOT NULL) \
                 ORDER BY p.name IS NULL, COUNT(i.id) DESC, p.name COLLATE NOCASE, p.id"
            ))?;
            let rows = st.query_map(params![hidden as i64, min], pet_from_row)?;
            rows.collect::<rusqlite::Result<Vec<Value>>>()
        })
        .await?;
    Ok(Json(json!({ "pets": pets })))
}

async fn get_pet(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("pet"));
    }
    lib.db
        .call(move |c| pet_json(c, &id))
        .await?
        .map(Json)
        .ok_or_else(|| not_found("pet"))
}

#[derive(Deserialize)]
struct PatchReq {
    #[serde(default, deserialize_with = "double_option")]
    name: Option<Option<String>>,
    hidden: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    birth_date: Option<Option<String>>,
    cover_crop_id: Option<String>,
}

/// A trimmed name of at most [`NAME_MAX`] characters; blank = no name.
fn clean_name(raw: &str) -> ApiResult<Option<String>> {
    let n = raw.trim();
    if n.chars().count() > NAME_MAX {
        return Err(bad(format!("a name is at most {NAME_MAX} characters")));
    }
    Ok((!n.is_empty()).then(|| n.to_string()))
}

fn clean_birth_date(raw: &str) -> ApiResult<String> {
    let d = chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .ok()
        .filter(|d| d.format("%Y-%m-%d").to_string() == raw.trim())
        .filter(|d| *d <= chrono::Utc::now().date_naive())
        .ok_or_else(|| bad("birth_date must be a past date as YYYY-MM-DD"))?;
    Ok(d.format("%Y-%m-%d").to_string())
}

enum Patched {
    NoPet,
    BadCover,
    Done(Value),
}

async fn patch_pet(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<PatchReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("pet"));
    }
    let name = match &req.name {
        Some(Some(n)) => Some(clean_name(n)?),
        Some(None) => Some(None),
        None => None,
    };
    let birth = match &req.birth_date {
        Some(Some(b)) => Some(Some(clean_birth_date(b)?)),
        Some(None) => Some(None),
        None => None,
    };
    let (hidden, cover) = (req.hidden, req.cover_crop_id);
    let now = db::now_str();
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_pet(&tx, &id)?.is_none() {
                return Ok(Patched::NoPet);
            }
            if let Some(crop) = &cover {
                let ok = fs::get_crop(&tx, crop)?
                    .is_some_and(|f| f.pet_id.as_deref() == Some(id.as_str()));
                if !ok {
                    return Ok(Patched::BadCover);
                }
                tx.execute(
                    "UPDATE pets SET cover_crop_id = ?2 WHERE id = ?1",
                    params![id, crop],
                )?;
            }
            if let Some(n) = &name {
                tx.execute("UPDATE pets SET name = ?2 WHERE id = ?1", params![id, n])?;
            }
            if let Some(h) = hidden {
                tx.execute(
                    "UPDATE pets SET hidden = ?2 WHERE id = ?1",
                    params![id, h as i64],
                )?;
            }
            if let Some(b) = &birth {
                tx.execute(
                    "UPDATE pets SET birth_date = ?2 WHERE id = ?1",
                    params![id, b],
                )?;
            }
            tx.execute(
                "UPDATE pets SET updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            let v = pet_json(&tx, &id)?.expect("checked above");
            tx.commit()?;
            Ok(Patched::Done(v))
        })
        .await?;
    match out {
        Patched::NoPet => Err(not_found("pet")),
        Patched::BadCover => Err(bad("cover_crop_id must be one of the pet's crops")),
        Patched::Done(v) => Ok(Json(v)),
    }
}

#[derive(Deserialize)]
struct MergeReq {
    from: Vec<String>,
}

async fn merge_pet(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<MergeReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("pet"));
    }
    if req.from.is_empty() || req.from.len() > MAX_MERGE {
        return Err(bad(format!("from must list 1 to {MAX_MERGE} pets")));
    }
    let now = db::now_str();
    let st = crops::state(&lib);
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_pet(&tx, &id)?.is_none() {
                return Ok(None);
            }
            let merged = fs::merge_pets(&tx, &id, &req.from, &now)?;
            let v = pet_json(&tx, &id)?.expect("checked above");
            tx.commit()?;
            if let Some(f) = &st {
                f.graph
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .move_people(&merged, &id);
            }
            Ok(Some(v))
        })
        .await?;
    out.map(Json).ok_or_else(|| not_found("pet"))
}

/// `GET pets/{id}/items`: the listing of `GET items` (same filters, cursor and page), only
/// photos with a crop of this pet.
async fn pet_items(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(mut q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("pet"));
    }
    let pid = id.clone();
    let exists = lib
        .db
        .call(move |c| Ok(fs::get_pet(c, &pid)?.is_some()))
        .await?;
    if !exists {
        return Err(not_found("pet"));
    }
    q.pet = Some(id);
    crate::http::list_items(State(lib), Query(q)).await
}

// ---- crops

fn jpeg_response(bytes: Vec<u8>, cache: &'static str) -> Response {
    let mut res = (StatusCode::OK, bytes).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

async fn crop_bytes(lib: &Library, crop: CropRow) -> ApiResult<Vec<u8>> {
    let cfg = lib.cfg.clone();
    let path = tokio::task::spawn_blocking(move || crops::ensure_crop(&cfg, &crop))
        .await?
        .map_err(|e| {
            tracing::warn!(error = %e, "crop crop failed");
            not_found("crop picture")
        })?;
    Ok(tokio::fs::read(path).await?)
}

async fn crop_thumb(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(not_found("crop"));
    }
    let crop = lib
        .db
        .call(move |c| fs::get_crop(c, &id))
        .await?
        .ok_or_else(|| not_found("crop"))?;
    Ok(jpeg_response(
        crop_bytes(&lib, crop).await?,
        "private, max-age=3600",
    ))
}

#[derive(Deserialize)]
struct ThumbQuery {
    v: Option<String>,
}

async fn pet_thumb(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<ThumbQuery>,
) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(not_found("pet"));
    }
    let (pet, crop) = lib
        .db
        .call(move |c| {
            let Some(p) = fs::get_pet(c, &id)? else {
                return Ok((None, None));
            };
            let crop = match &p.cover_crop_id {
                Some(f) => fs::get_crop(c, f)?,
                None => None,
            };
            Ok((Some(p), crop))
        })
        .await?;
    let pet = pet.ok_or_else(|| not_found("pet"))?;
    let crop = crop.ok_or_else(|| not_found("crop"))?;
    // The URL carries the cover's id, so a matching one never changes.
    let cache = if q.v.as_deref() == pet.cover_crop_id.as_deref() {
        "private, max-age=31536000, immutable"
    } else {
        "private, max-age=60"
    };
    Ok(jpeg_response(crop_bytes(&lib, crop).await?, cache))
}

// ---- crops

async fn item_crops(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !crate::http::valid_item_id(&id) {
        return Err(not_found("item"));
    }
    let out = lib
        .db
        .call(move |c| {
            let id = crate::dedup_merge::resolve(c, &id)?;
            let exists: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1)",
                [&id],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let crops = fs::crops_of_item(c, &id)?
                .iter()
                .map(|f| crop_json(c, f))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(crops))
        })
        .await?;
    out.map(|crops| Json(json!({ "crops": crops })))
        .ok_or_else(|| not_found("item"))
}

#[derive(Deserialize)]
struct PutCropReq {
    #[serde(default, deserialize_with = "double_option")]
    pet_id: Option<Option<String>>,
    new_pet: Option<String>,
}

enum Assigned {
    NoCrop,
    NoPet,
    Done(Value),
}

async fn put_crop(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<PutCropReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("crop"));
    }
    // What to do: assign to an existing pet (Some(Some)), unassign (Some(None)) or make one.
    enum Target {
        Pet(String),
        Nobody,
        New(String),
    }
    let target = match (req.pet_id, req.new_pet) {
        (Some(_), Some(_)) => return Err(bad("give pet_id or new_pet, not both")),
        (None, None) => return Err(bad("pet_id or new_pet is required")),
        (Some(Some(p)), None) => Target::Pet(p),
        (Some(None), None) => Target::Nobody,
        (None, Some(n)) => match clean_name(&n)? {
            Some(n) => Target::New(n),
            None => return Err(bad("new_pet needs a name")),
        },
    };
    let now = db::now_str();
    let st = crops::state(&lib);
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_crop(&tx, &id)?.is_none() {
                return Ok(Assigned::NoCrop);
            }
            let pet: Option<String> = match target {
                Target::Nobody => None,
                Target::Pet(p) => {
                    if fs::get_pet(&tx, &p)?.is_none() {
                        return Ok(Assigned::NoPet);
                    }
                    Some(p)
                }
                Target::New(name) => {
                    let pid = fs::new_pet_id();
                    tx.execute(
                        "INSERT INTO pets (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                        params![pid, name, now],
                    )?;
                    Some(pid)
                }
            };
            fs::assign_crop(&tx, &id, pet.as_deref())?;
            let crop = fs::get_crop(&tx, &id)?.expect("checked above");
            let v = crop_json(&tx, &crop)?;
            tx.commit()?;
            if let Some(f) = &st {
                f.graph
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .set_manual(&id, pet.as_deref());
            }
            Ok(Assigned::Done(v))
        })
        .await?;
    match out {
        Assigned::NoCrop => Err(not_found("crop")),
        Assigned::NoPet => Err(not_found("pet")),
        Assigned::Done(v) => Ok(Json(v)),
    }
}

#[derive(Deserialize)]
struct UnassignedQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

/// `GET crops/unassigned`: crops with no pet, newest photo first.
async fn unassigned(
    State(lib): State<Library>,
    Query(q): Query<UnassignedQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let after = match q.cursor.as_deref() {
        None => None,
        Some(c) => Some(
            c.split_once('|')
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .ok_or_else(|| bad("bad cursor"))?,
        ),
    };
    let (rows, next) = lib
        .db
        .call(move |c| {
            let mut sql = String::from(
                "SELECT f.id, f.item_id, f.score, i.sort_at FROM pet_crops f \
                 JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0 \
                 WHERE f.pet_id IS NULL",
            );
            let mut args: Vec<rusqlite::types::Value> = Vec::new();
            if let Some((sort_at, id)) = &after {
                args.push(sort_at.clone().into());
                args.push(id.clone().into());
                sql.push_str(" AND (i.sort_at < ?1 OR (i.sort_at = ?1 AND f.id < ?2))");
            }
            sql.push_str(&format!(
                " ORDER BY i.sort_at DESC, f.id DESC LIMIT {}",
                limit + 1
            ));
            let mut st = c.prepare(&sql)?;
            let mut rows: Vec<(String, String, f64, String)> = st
                .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let next = if rows.len() > limit {
                rows.truncate(limit);
                rows.last()
                    .map(|(id, _, _, sort_at)| format!("{sort_at}|{id}"))
            } else {
                None
            };
            Ok((rows, next))
        })
        .await?;
    Ok(Json(json!({
        "crops": rows.into_iter().map(|(id, item_id, score, _)| json!({
            "thumb_url": format!("/v1/media/pet-crops/{id}/thumb"),
            "id": id,
            "item_id": item_id,
            "score": round5(score),
        })).collect::<Vec<_>>(),
        "next_cursor": next,
    })))
}

//! The faces and people routes (`docs/gallery-vs-immich.md`, gap 2), all behind the upstream key
//! like the rest of `/v1/media` and never reachable from a share link:
//!
//! - `GET|PUT faces/settings`, `DELETE faces`: the opt-in, progress, and "delete face data".
//! - `GET people`, `GET|PATCH people/{id}`, `POST people/{id}/merge`, `GET people/{id}/items`,
//!   `GET people/{id}/thumb`: the People page.
//! - `GET items/{id}/faces`, `PUT faces/{id}`, `GET faces/{id}/thumb`, `GET faces/unassigned`:
//!   the faces of a photo and the owner's corrections.
//!
//! `GET items/{id}/details` lists the photo's visible people ([`people_of_item`]) and smart search
//! takes `person=<id>` (`smart.rs`); both read the same tables.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use lucina_media_ml::faces_store::{self as fs, FaceRow};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::db::{self, ListQuery};
use crate::faces;
use crate::http::{bad, ApiError, ApiResult};
use crate::ingest::Library;

/// People with at least this many faces are listed unless the request says otherwise.
pub const DEFAULT_MIN_FACES: i64 = 3;
const NAME_MAX: usize = 80;
const DEFAULT_LIMIT: u32 = 60;
const MAX_LIMIT: u32 = 200;
const MAX_MERGE: usize = 100;

pub fn routes() -> Router<Library> {
    Router::new()
        .route(
            "/v1/media/faces/settings",
            get(get_settings).put(put_settings),
        )
        .route("/v1/media/faces", delete(delete_faces))
        .route("/v1/media/faces/unassigned", get(unassigned))
        .route("/v1/media/faces/{id}", put(put_face))
        .route("/v1/media/faces/{id}/thumb", get(face_thumb))
        .route("/v1/media/people", get(list_people))
        .route("/v1/media/people/{id}", get(get_person).patch(patch_person))
        .route("/v1/media/people/{id}/merge", post(merge_person))
        .route("/v1/media/people/{id}/items", get(person_items))
        .route("/v1/media/people/{id}/thumb", get(person_thumb))
        .route("/v1/media/items/{id}/faces", get(item_faces))
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

const PERSON_SELECT: &str = "SELECT p.id, p.name, p.hidden, p.birth_date, p.cover_face_id, \
     COUNT(i.id), COUNT(DISTINCT i.id) FROM people p \
     LEFT JOIN faces f ON f.person_id = p.id \
     LEFT JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0";

fn person_from_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = r.get(0)?;
    let cover: Option<String> = r.get(4)?;
    Ok(json!({
        "thumb_url": cover.as_ref().map(|c| format!("/v1/media/people/{id}/thumb?v={c}")),
        "id": id,
        "name": r.get::<_, Option<String>>(1)?,
        "hidden": r.get::<_, i64>(2)? != 0,
        "birth_date": r.get::<_, Option<String>>(3)?,
        "face_count": r.get::<_, i64>(5)?,
        "item_count": r.get::<_, i64>(6)?,
        "cover_face_id": cover,
    }))
}

fn person_json(c: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    c.query_row(
        &format!("{PERSON_SELECT} WHERE p.id = ?1 GROUP BY p.id"),
        [id],
        person_from_row,
    )
    .optional()
}

fn face_json(c: &Connection, f: &FaceRow) -> rusqlite::Result<Value> {
    let person = match &f.person_id {
        Some(pid) => fs::get_person(c, pid)?
            .map(|p| json!({ "id": p.id, "name": p.name, "hidden": p.hidden })),
        None => None,
    };
    Ok(json!({
        "id": f.id,
        "item_id": f.item_id,
        "box": { "x": round5(f.x), "y": round5(f.y), "w": round5(f.w), "h": round5(f.h) },
        "score": round5(f.score),
        "source": f.source,
        "person": person,
    }))
}

/// The people shown for a photo: not hidden, and named or with at least the default number of faces.
pub fn people_of_item(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT DISTINCT p.id, p.name FROM faces f JOIN people p ON p.id = f.person_id \
         WHERE f.item_id = ?1 AND p.hidden = 0 AND (p.name IS NOT NULL \
             OR (SELECT COUNT(*) FROM faces g WHERE g.person_id = p.id) >= ?2) \
         ORDER BY p.name IS NULL, p.name COLLATE NOCASE, p.id",
    )?;
    let rows = st.query_map(params![item_id, DEFAULT_MIN_FACES], |r| {
        Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)? }))
    })?;
    rows.collect()
}

fn status_json(c: &Connection, lib: &Library) -> rusqlite::Result<Value> {
    let st = faces::state(lib);
    let model = st
        .as_ref()
        .map(|f| f.model.model().to_string())
        .unwrap_or_else(|| lucina_media_ml::faces::MODEL.to_string());
    let (paused_hot, paused_busy) = crate::smart::pauses(lib);
    let people: i64 = c.query_row(
        "SELECT COUNT(*) FROM (SELECT p.id FROM people p JOIN faces f ON f.person_id = p.id \
         JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0 \
         WHERE p.hidden = 0 GROUP BY p.id HAVING COUNT(i.id) >= ?1 OR p.name IS NOT NULL)",
        [DEFAULT_MIN_FACES],
        |r| r.get(0),
    )?;
    Ok(json!({
        "enabled": fs::enabled(c)?,
        "available": st.is_some(),
        "model": st.is_some().then_some(model.clone()),
        "scanned": fs::count_scans(c, &model)?,
        "pending": if st.is_some() { faces::pending_count(c, &model)? } else { 0 },
        "faces": fs::count_faces(c)?,
        "people": people,
        "running": faces::running(lib),
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
    if req.enabled && faces::state(&lib).is_none() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "face recognition is not available on this server".into(),
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

async fn delete_faces(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let l = lib.clone();
    tokio::task::spawn_blocking(move || faces::delete_everything(&l))
        .await?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- people

#[derive(Deserialize)]
struct PeopleQuery {
    min_faces: Option<i64>,
    include_hidden: Option<bool>,
}

async fn list_people(
    State(lib): State<Library>,
    Query(q): Query<PeopleQuery>,
) -> ApiResult<Json<Value>> {
    let min = q.min_faces.unwrap_or(DEFAULT_MIN_FACES).max(1);
    let hidden = q.include_hidden.unwrap_or(false);
    let people = lib
        .db
        .call(move |c| {
            let mut st = c.prepare(&format!(
                "{PERSON_SELECT} WHERE (?1 = 1 OR p.hidden = 0) GROUP BY p.id \
                 HAVING COUNT(i.id) > 0 AND (COUNT(i.id) >= ?2 OR p.name IS NOT NULL) \
                 ORDER BY p.name IS NULL, COUNT(i.id) DESC, p.name COLLATE NOCASE, p.id"
            ))?;
            let rows = st.query_map(params![hidden as i64, min], person_from_row)?;
            rows.collect::<rusqlite::Result<Vec<Value>>>()
        })
        .await?;
    Ok(Json(json!({ "people": people })))
}

async fn get_person(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("person"));
    }
    lib.db
        .call(move |c| person_json(c, &id))
        .await?
        .map(Json)
        .ok_or_else(|| not_found("person"))
}

#[derive(Deserialize)]
struct PatchReq {
    #[serde(default, deserialize_with = "double_option")]
    name: Option<Option<String>>,
    hidden: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    birth_date: Option<Option<String>>,
    cover_face_id: Option<String>,
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
    NoPerson,
    BadCover,
    Done(Value),
}

async fn patch_person(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<PatchReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("person"));
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
    let (hidden, cover) = (req.hidden, req.cover_face_id);
    let now = db::now_str();
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_person(&tx, &id)?.is_none() {
                return Ok(Patched::NoPerson);
            }
            if let Some(face) = &cover {
                let ok = fs::get_face(&tx, face)?
                    .is_some_and(|f| f.person_id.as_deref() == Some(id.as_str()));
                if !ok {
                    return Ok(Patched::BadCover);
                }
                tx.execute(
                    "UPDATE people SET cover_face_id = ?2 WHERE id = ?1",
                    params![id, face],
                )?;
            }
            if let Some(n) = &name {
                tx.execute("UPDATE people SET name = ?2 WHERE id = ?1", params![id, n])?;
            }
            if let Some(h) = hidden {
                tx.execute(
                    "UPDATE people SET hidden = ?2 WHERE id = ?1",
                    params![id, h as i64],
                )?;
            }
            if let Some(b) = &birth {
                tx.execute(
                    "UPDATE people SET birth_date = ?2 WHERE id = ?1",
                    params![id, b],
                )?;
            }
            tx.execute(
                "UPDATE people SET updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            let v = person_json(&tx, &id)?.expect("checked above");
            tx.commit()?;
            Ok(Patched::Done(v))
        })
        .await?;
    match out {
        Patched::NoPerson => Err(not_found("person")),
        Patched::BadCover => Err(bad("cover_face_id must be one of the person's faces")),
        Patched::Done(v) => Ok(Json(v)),
    }
}

#[derive(Deserialize)]
struct MergeReq {
    from: Vec<String>,
}

async fn merge_person(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<MergeReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("person"));
    }
    if req.from.is_empty() || req.from.len() > MAX_MERGE {
        return Err(bad(format!("from must list 1 to {MAX_MERGE} people")));
    }
    let now = db::now_str();
    let st = faces::state(&lib);
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_person(&tx, &id)?.is_none() {
                return Ok(None);
            }
            let merged = fs::merge_people(&tx, &id, &req.from, &now)?;
            let v = person_json(&tx, &id)?.expect("checked above");
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
    out.map(Json).ok_or_else(|| not_found("person"))
}

/// `GET people/{id}/items`: the listing of `GET items` (same filters, cursor and page), only
/// photos with a face of this person.
async fn person_items(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(mut q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("person"));
    }
    let pid = id.clone();
    let exists = lib
        .db
        .call(move |c| Ok(fs::get_person(c, &pid)?.is_some()))
        .await?;
    if !exists {
        return Err(not_found("person"));
    }
    q.person = Some(id);
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

async fn crop_bytes(lib: &Library, face: FaceRow) -> ApiResult<Vec<u8>> {
    let cfg = lib.cfg.clone();
    let path = tokio::task::spawn_blocking(move || faces::ensure_crop(&cfg, &face))
        .await?
        .map_err(|e| {
            tracing::warn!(error = %e, "face crop failed");
            not_found("face picture")
        })?;
    Ok(tokio::fs::read(path).await?)
}

async fn face_thumb(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(not_found("face"));
    }
    let face = lib
        .db
        .call(move |c| fs::get_face(c, &id))
        .await?
        .ok_or_else(|| not_found("face"))?;
    Ok(jpeg_response(
        crop_bytes(&lib, face).await?,
        "private, max-age=3600",
    ))
}

#[derive(Deserialize)]
struct ThumbQuery {
    v: Option<String>,
}

async fn person_thumb(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<ThumbQuery>,
) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(not_found("person"));
    }
    let (person, face) = lib
        .db
        .call(move |c| {
            let Some(p) = fs::get_person(c, &id)? else {
                return Ok((None, None));
            };
            let face = match &p.cover_face_id {
                Some(f) => fs::get_face(c, f)?,
                None => None,
            };
            Ok((Some(p), face))
        })
        .await?;
    let person = person.ok_or_else(|| not_found("person"))?;
    let face = face.ok_or_else(|| not_found("face"))?;
    // The URL carries the cover's id, so a matching one never changes.
    let cache = if q.v.as_deref() == person.cover_face_id.as_deref() {
        "private, max-age=31536000, immutable"
    } else {
        "private, max-age=60"
    };
    Ok(jpeg_response(crop_bytes(&lib, face).await?, cache))
}

// ---- faces

async fn item_faces(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
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
            let faces = fs::faces_of_item(c, &id)?
                .iter()
                .map(|f| face_json(c, f))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(faces))
        })
        .await?;
    out.map(|faces| Json(json!({ "faces": faces })))
        .ok_or_else(|| not_found("item"))
}

#[derive(Deserialize)]
struct PutFaceReq {
    #[serde(default, deserialize_with = "double_option")]
    person_id: Option<Option<String>>,
    new_person: Option<String>,
}

enum Assigned {
    NoFace,
    NoPerson,
    Done(Value),
}

async fn put_face(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<PutFaceReq>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found("face"));
    }
    // What to do: assign to an existing person (Some(Some)), unassign (Some(None)) or make one.
    enum Target {
        Person(String),
        Nobody,
        New(String),
    }
    let target = match (req.person_id, req.new_person) {
        (Some(_), Some(_)) => return Err(bad("give person_id or new_person, not both")),
        (None, None) => return Err(bad("person_id or new_person is required")),
        (Some(Some(p)), None) => Target::Person(p),
        (Some(None), None) => Target::Nobody,
        (None, Some(n)) => match clean_name(&n)? {
            Some(n) => Target::New(n),
            None => return Err(bad("new_person needs a name")),
        },
    };
    let now = db::now_str();
    let st = faces::state(&lib);
    let out = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if fs::get_face(&tx, &id)?.is_none() {
                return Ok(Assigned::NoFace);
            }
            let person: Option<String> = match target {
                Target::Nobody => None,
                Target::Person(p) => {
                    if fs::get_person(&tx, &p)?.is_none() {
                        return Ok(Assigned::NoPerson);
                    }
                    Some(p)
                }
                Target::New(name) => {
                    let pid = fs::new_person_id();
                    tx.execute(
                        "INSERT INTO people (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                        params![pid, name, now],
                    )?;
                    Some(pid)
                }
            };
            fs::assign_face(&tx, &id, person.as_deref())?;
            let face = fs::get_face(&tx, &id)?.expect("checked above");
            let v = face_json(&tx, &face)?;
            tx.commit()?;
            if let Some(f) = &st {
                f.graph
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .set_manual(&id, person.as_deref());
            }
            Ok(Assigned::Done(v))
        })
        .await?;
    match out {
        Assigned::NoFace => Err(not_found("face")),
        Assigned::NoPerson => Err(not_found("person")),
        Assigned::Done(v) => Ok(Json(v)),
    }
}

#[derive(Deserialize)]
struct UnassignedQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

/// `GET faces/unassigned`: faces with no person, newest photo first.
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
                "SELECT f.id, f.item_id, f.score, i.sort_at FROM faces f \
                 JOIN items i ON i.id = f.item_id AND i.trashed_at IS NULL AND i.hidden = 0 \
                 WHERE f.person_id IS NULL",
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
        "faces": rows.into_iter().map(|(id, item_id, score, _)| json!({
            "thumb_url": format!("/v1/media/faces/{id}/thumb"),
            "id": id,
            "item_id": item_id,
            "score": round5(score),
        })).collect::<Vec<_>>(),
        "next_cursor": next,
    })))
}

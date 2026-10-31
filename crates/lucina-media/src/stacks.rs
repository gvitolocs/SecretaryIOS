//! Stacks: a burst, a RAW+JPEG pair or any set of photos that the Gallery shows as one tile with a count
//! (`docs/gallery-vs-immich.md`, gap 4).
//!
//! A stack is a row in `stacks` (its primary photo and kind) plus one row per photo in `stack_items`
//! (a photo is in at most one stack). The primary stays in every listing and its item JSON carries
//! `stack: {id, count, primary}`; every other member is hidden with `items.hidden = 3`
//! ([`HIDDEN_STACK`]), so each listing, count, month, source total and search that already skips hidden
//! items skips it, and `GET items/{id}` still reads it.
//!
//! Why a table and not `item_links`: that table means "this photo is a copy of that one"
//! (`dedup_merge::resolve` makes `GET items/{copy}` answer with the survivor). A stack member must keep
//! its own identity so the viewer can open it.
//!
//! A SQL trigger keeps stacks whole whichever code trashes a photo (the batch op, `DELETE items/{id}`,
//! ingest, the Drive): trashing the primary promotes the next member, a trashed member leaves, and a
//! stack left with one photo dissolves. A restored photo does not rejoin.
//!
//! - `GET stacks/{id}`, `POST stacks {ids, primary?, kind?}`, `PUT stacks/{id} {primary}`,
//!   `DELETE stacks/{id}`.
//! - `GET stacks/suggestions`: bursts (same device, shots within [`BURST_GAP_MS`] of the next, alike
//!   when both have a perceptual hash) and RAW+JPEG pairs (same device, source and file name). Nothing
//!   is ever stacked without a `POST`. `POST stacks/suggestions/dismiss {ids}` hides that exact set.
//!
//! Burst metadata: the iPhone upload carries `taken_at` (milliseconds) but not PhotoKit's
//! `burstIdentifier`, and no EXIF reader here parses Apple's BurstUUID, so bursts are found by time
//! and look.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{self, now_str, Item};
use crate::dedup::{self, Cand, DimsRel, Thresholds};
use crate::http::{bad, item_out, ApiError, ApiResult};
use crate::ingest::Library;

/// `items.hidden` of a stack member that is not its primary (1 is a Live Photo's motion part, 2 a
/// merged duplicate).
pub const HIDDEN_STACK: i64 = 3;
/// Two shots of one device this close in time (milliseconds) belong to one burst.
pub const BURST_GAP_MS: i64 = 1_500;
/// The most photos in one stack.
pub const MAX_STACK: usize = 100;
/// The longest time (milliseconds) between the halves of a RAW+JPEG pair.
const PAIR_GAP_MS: i64 = 2_000;
const KINDS: [&str; 3] = ["manual", "burst", "raw_jpeg"];
const RAW_EXT: [&str; 8] = ["dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2"];
const PICTURE_EXT: [&str; 5] = ["jpg", "jpeg", "heic", "heif", "png"];

/// The schema, idempotent: called from `db::migrate`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS stacks (
            id TEXT PRIMARY KEY,
            primary_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS stack_items (
            item_id TEXT PRIMARY KEY,
            stack_id TEXT NOT NULL,
            added_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS stack_items_stack ON stack_items(stack_id);
        -- A suggestion the owner turned down: the key is a hash of the exact set of photo ids.
        CREATE TABLE IF NOT EXISTS stack_dismissed (
            key TEXT PRIMARY KEY,
            created_at TEXT NOT NULL
        );
        -- Trashing a stacked photo, by any code path: the next member takes over as primary, the photo
        -- leaves the stack (visible again, so the Trash can list it), and a stack of one dissolves.
        CREATE TRIGGER IF NOT EXISTS stack_item_trashed
        AFTER UPDATE OF trashed_at ON items
        WHEN OLD.trashed_at IS NULL AND NEW.trashed_at IS NOT NULL
             AND EXISTS (SELECT 1 FROM stack_items WHERE item_id = NEW.id)
        BEGIN
            UPDATE stacks SET primary_id = COALESCE(
                (SELECT si.item_id FROM stack_items si JOIN items i ON i.id = si.item_id
                  WHERE si.stack_id = stacks.id AND si.item_id <> NEW.id AND i.trashed_at IS NULL
                  ORDER BY i.sort_at, si.item_id LIMIT 1),
                primary_id)
             WHERE primary_id = NEW.id;
            UPDATE items SET hidden = 0 WHERE hidden = 3 AND id IN (SELECT primary_id FROM stacks);
            UPDATE items SET hidden = 0 WHERE id = NEW.id AND hidden = 3;
            DELETE FROM stack_items WHERE item_id = NEW.id;
            UPDATE items SET hidden = 0
             WHERE hidden = 3 AND id IN (
                SELECT item_id FROM stack_items WHERE stack_id IN (
                    SELECT stack_id FROM stack_items GROUP BY stack_id HAVING COUNT(*) < 2));
            DELETE FROM stacks
             WHERE id NOT IN (SELECT stack_id FROM stack_items GROUP BY stack_id HAVING COUNT(*) >= 2);
            DELETE FROM stack_items WHERE stack_id NOT IN (SELECT id FROM stacks);
        END;
        ",
    )
}

/// What an item's JSON says about its stack.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackRef {
    pub id: String,
    /// Photos in the stack, the primary included.
    pub count: i64,
    /// This item is the stack's primary (the one tile the listings show).
    pub primary: bool,
}

/// Set `stack` on items that belong to one.
pub fn fill(c: &Connection, items: &mut [Item]) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached(
        "SELECT s.id, s.primary_id, (SELECT COUNT(*) FROM stack_items x WHERE x.stack_id = s.id)
         FROM stack_items si JOIN stacks s ON s.id = si.stack_id WHERE si.item_id = ?1",
    )?;
    for it in items.iter_mut() {
        let id = it.id.clone();
        it.stack = st
            .query_row([&id], |r| {
                let primary: String = r.get(1)?;
                Ok(StackRef {
                    id: r.get(0)?,
                    count: r.get(2)?,
                    primary: primary == id,
                })
            })
            .optional()?;
    }
    Ok(())
}

// ---- the data ------------------------------------------------------------------------------

/// A stack and its members, the primary first, then by capture time.
pub struct StackData {
    pub id: String,
    pub kind: String,
    pub primary: String,
    pub created_at: String,
    pub items: Vec<Item>,
}

pub fn load(c: &Connection, id: &str) -> rusqlite::Result<Option<StackData>> {
    let head: Option<(String, String, String)> = c
        .query_row(
            "SELECT primary_id, kind, created_at FROM stacks WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((primary, kind, created_at)) = head else {
        return Ok(None);
    };
    let ids: Vec<String> = {
        let mut st = c.prepare(
            "SELECT si.item_id FROM stack_items si JOIN items i ON i.id = si.item_id
             WHERE si.stack_id = ?1 ORDER BY (si.item_id = ?2) DESC, i.sort_at, si.item_id",
        )?;
        let rows = st
            .query_map(params![id, primary], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let mut items = Vec::with_capacity(ids.len());
    for member in &ids {
        if let Some(it) = db::get_item(c, member)? {
            items.push(it);
        }
    }
    Ok(Some(StackData {
        id: id.to_string(),
        kind,
        primary,
        created_at,
        items,
    }))
}

/// Why a photo cannot join a stack, or `None` when it can.
fn blocker(c: &Connection, id: &str) -> rusqlite::Result<Option<&'static str>> {
    let row: Option<(String, bool, i64, bool, bool)> = c
        .query_row(
            "SELECT kind, trashed_at IS NOT NULL, hidden,
                    EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = items.id),
                    EXISTS (SELECT 1 FROM stack_items si WHERE si.item_id = items.id)
             FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(match row {
        None => Some("missing"),
        Some((kind, ..)) if kind != "photo" => Some("not_photo"),
        Some((_, true, ..)) => Some("trashed"),
        Some((_, _, _, true, _)) => Some("drive_file"),
        Some((.., true)) => Some("already_stacked"),
        Some((_, _, hidden, ..)) if hidden != 0 => Some("hidden"),
        _ => None,
    })
}

/// Stack `ids` (already checked for count and shape) around `primary`. `Err` lists the photos that
/// cannot join and nothing is changed.
pub fn create(
    c: &mut Connection,
    ids: &[String],
    primary: &str,
    kind: &str,
) -> rusqlite::Result<Result<String, Vec<(String, &'static str)>>> {
    let tx = c.transaction()?;
    let mut failed = Vec::new();
    for id in ids {
        if let Some(reason) = blocker(&tx, id)? {
            failed.push((id.clone(), reason));
        }
    }
    if !failed.is_empty() {
        return Ok(Err(failed));
    }
    let now = now_str();
    let sid = db::new_id("stk");
    tx.execute(
        "INSERT INTO stacks (id, primary_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![sid, primary, kind, now],
    )?;
    for id in ids {
        tx.execute(
            "INSERT INTO stack_items (item_id, stack_id, added_at) VALUES (?1, ?2, ?3)",
            params![id, sid, now],
        )?;
        let hidden = if id == primary { 0 } else { HIDDEN_STACK };
        tx.execute(
            "UPDATE items SET hidden = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, hidden, now],
        )?;
    }
    tx.commit()?;
    Ok(Ok(sid))
}

/// Make `new_primary` the stack's tile. `false`: it is not a member (or the stack does not exist).
pub fn set_primary(c: &mut Connection, stack: &str, new_primary: &str) -> rusqlite::Result<bool> {
    let tx = c.transaction()?;
    let member: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM stack_items WHERE stack_id = ?1 AND item_id = ?2)",
        params![stack, new_primary],
        |r| r.get(0),
    )?;
    if !member {
        return Ok(false);
    }
    let old: String = tx.query_row(
        "SELECT primary_id FROM stacks WHERE id = ?1",
        [stack],
        |r| r.get(0),
    )?;
    if old != new_primary {
        let now = now_str();
        tx.execute(
            "UPDATE items SET hidden = ?2, updated_at = ?3 WHERE id = ?1 AND hidden = 0",
            params![old, HIDDEN_STACK, now],
        )?;
        tx.execute(
            "UPDATE items SET hidden = 0, updated_at = ?2 WHERE id = ?1",
            params![new_primary, now],
        )?;
        tx.execute(
            "UPDATE stacks SET primary_id = ?2 WHERE id = ?1",
            params![stack, new_primary],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// Dissolve a stack: every member is a plain photo again. `None`: no such stack.
pub fn dissolve(c: &mut Connection, stack: &str) -> rusqlite::Result<Option<Vec<String>>> {
    let tx = c.transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM stacks WHERE id = ?1)",
        [stack],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let ids: Vec<String> = {
        let mut st =
            tx.prepare("SELECT item_id FROM stack_items WHERE stack_id = ?1 ORDER BY item_id")?;
        let rows = st
            .query_map([stack], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let now = now_str();
    for id in &ids {
        tx.execute(
            "UPDATE items SET hidden = 0, updated_at = ?2 WHERE id = ?1 AND hidden = ?3",
            params![id, now, HIDDEN_STACK],
        )?;
    }
    tx.execute("DELETE FROM stack_items WHERE stack_id = ?1", [stack])?;
    tx.execute("DELETE FROM stacks WHERE id = ?1", [stack])?;
    tx.commit()?;
    Ok(Some(ids))
}

// ---- suggestions ---------------------------------------------------------------------------

/// A set of photos that look like one stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub kind: &'static str,
    /// `time_and_look` (shots close in time and alike by perceptual hash), `time` (close in time, a
    /// hash is still missing) or `name` (a RAW and a picture with one file name).
    pub evidence: &'static str,
    pub primary: String,
    pub ids: Vec<String>,
    pub span_ms: i64,
}

#[derive(Debug, Default)]
pub struct Suggestions {
    pub items: Vec<Suggestion>,
    /// Photos of devices whose perceptual hash is not computed yet: their bursts are judged by time alone.
    pub pending: usize,
}

/// The key of a suggestion (and of its dismissal): a hash of the sorted ids.
pub fn set_key(ids: &[String]) -> String {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let h = blake3::hash(sorted.join("\n").as_bytes());
    format!("sg_{}", &h.to_hex()[..16])
}

fn ext_and_stem(name: &str) -> Option<(&str, &str)> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then_some((stem, ext))
}

/// Pure part of the burst search: runs of shots of one device, each within `gap_ms` of the one before
/// and, when both have a hash, alike. Returns runs of two or more, as indices into `cands`.
pub fn burst_runs(cands: &[Cand], th: &Thresholds, gap_ms: i64) -> Vec<(Vec<usize>, bool)> {
    let mut order: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].device.is_some() && cands[i].taken_ms.is_some())
        .collect();
    order.sort_by(|&a, &b| {
        (&cands[a].device, cands[a].taken_ms, &cands[a].id).cmp(&(
            &cands[b].device,
            cands[b].taken_ms,
            &cands[b].id,
        ))
    });
    let mut out = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut looked = true;
    let flush = |run: &mut Vec<usize>, looked: &mut bool, out: &mut Vec<(Vec<usize>, bool)>| {
        for chunk in run.chunks(MAX_STACK) {
            if chunk.len() >= 2 {
                out.push((chunk.to_vec(), *looked));
            }
        }
        run.clear();
        *looked = true;
    };
    for &i in &order {
        if let Some(&prev) = run.last() {
            let (a, b) = (&cands[prev], &cands[i]);
            let gap = b.taken_ms.unwrap_or(0) - a.taken_ms.unwrap_or(0);
            let shape_differs = a.width.is_some()
                && a.height.is_some()
                && b.width.is_some()
                && b.height.is_some()
                && dedup::dims_rel(a, b) == DimsRel::Different;
            let alike = dedup::distance(a, b);
            let close = a.device == b.device
                && gap <= gap_ms
                && !shape_differs
                && alike.is_none_or(|d| d <= th.similar);
            if close {
                looked &= alike.is_some();
                run.push(i);
                continue;
            }
            flush(&mut run, &mut looked, &mut out);
        }
        run.push(i);
    }
    flush(&mut run, &mut looked, &mut out);
    out
}

/// Pure part of the RAW+JPEG search: photos of one device and source that share a file name and
/// differ in extension, one a RAW and one a picture, taken together when both have a time.
pub fn raw_pairs(cands: &[Cand]) -> Vec<Vec<usize>> {
    let mut by_name: HashMap<(Option<&str>, &str, &str), Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        let Some((stem, _)) = c.name.as_deref().and_then(ext_and_stem) else {
            continue;
        };
        by_name
            .entry((c.device.as_deref(), c.source.as_str(), stem))
            .or_default()
            .push(i);
    }
    let ext = |i: usize| {
        cands[i]
            .name
            .as_deref()
            .and_then(ext_and_stem)
            .map_or("", |(_, e)| e)
    };
    let mut out: Vec<Vec<usize>> = Vec::new();
    for mut group in by_name.into_values() {
        let raws = group.iter().filter(|&&i| RAW_EXT.contains(&ext(i))).count();
        let pics = group
            .iter()
            .filter(|&&i| PICTURE_EXT.contains(&ext(i)))
            .count();
        if raws == 0 || pics == 0 || group.len() > MAX_STACK {
            continue;
        }
        group.retain(|&i| RAW_EXT.contains(&ext(i)) || PICTURE_EXT.contains(&ext(i)));
        let times: Vec<i64> = group.iter().filter_map(|&i| cands[i].taken_ms).collect();
        if times.len() == group.len() {
            let (lo, hi) = (
                times.iter().min().copied().unwrap_or(0),
                times.iter().max().copied().unwrap_or(0),
            );
            if hi - lo > PAIR_GAP_MS {
                continue;
            }
        }
        group.sort_by(|&a, &b| cands[a].id.cmp(&cands[b].id));
        out.push(group);
    }
    out.sort();
    out
}

/// Bursts and RAW+JPEG pairs among the visible photos that are in no stack and whose exact set the
/// owner has not turned down. Reads only.
pub fn suggest(c: &Connection) -> rusqlite::Result<Suggestions> {
    let stacked: HashSet<String> = {
        let mut st = c.prepare("SELECT item_id FROM stack_items")?;
        let rows = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let dismissed: HashSet<String> = {
        let mut st = c.prepare("SELECT key FROM stack_dismissed")?;
        let rows = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let cands: Vec<Cand> = dedup::load_cands(c, c)?
        .into_iter()
        .filter(|x| !stacked.contains(&x.id))
        .collect();
    let th = Thresholds::default();
    let mut found: Vec<(&'static str, &'static str, Vec<usize>)> = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();
    for group in raw_pairs(&cands) {
        used.extend(group.iter().copied());
        found.push(("raw_jpeg", "name", group));
    }
    // A burst is judged on the photos no RAW+JPEG pair took.
    let rest: Vec<usize> = (0..cands.len()).filter(|i| !used.contains(i)).collect();
    let rest_cands: Vec<Cand> = rest.iter().map(|&i| cands[i].clone()).collect();
    for (run, looked) in burst_runs(&rest_cands, &th, BURST_GAP_MS) {
        let group: Vec<usize> = run.into_iter().map(|j| rest[j]).collect();
        found.push((
            "burst",
            if looked { "time_and_look" } else { "time" },
            group,
        ));
    }
    let pending = cands
        .iter()
        .filter(|x| x.device.is_some() && x.hashes.is_none())
        .count();
    let mut out = Suggestions {
        items: Vec::new(),
        pending,
    };
    for (kind, evidence, group) in found {
        let ids: Vec<String> = group.iter().map(|&i| cands[i].id.clone()).collect();
        let key = set_key(&ids);
        if dismissed.contains(&key) {
            continue;
        }
        let times: Vec<i64> = group.iter().filter_map(|&i| cands[i].taken_ms).collect();
        let span_ms = times.iter().max().unwrap_or(&0) - times.iter().min().unwrap_or(&0);
        // The tile is the best-looking copy; for a RAW+JPEG pair it is a picture, never the RAW.
        let candidates: Vec<String> = if kind == "raw_jpeg" {
            group
                .iter()
                .filter(|&&i| {
                    cands[i]
                        .name
                        .as_deref()
                        .and_then(ext_and_stem)
                        .is_some_and(|(_, e)| PICTURE_EXT.contains(&e))
                })
                .map(|&i| cands[i].id.clone())
                .collect()
        } else {
            ids.clone()
        };
        let primary = crate::dedup_review::best_of(c, &candidates)?
            .or_else(|| ids.first().cloned())
            .unwrap_or_default();
        out.items.push(Suggestion {
            id: key,
            kind,
            evidence,
            primary,
            ids,
            span_ms,
        });
    }
    out.items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

// ---- the API -------------------------------------------------------------------------------

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/stacks", post(create_stack))
        .route("/v1/media/stacks/suggestions", get(list_suggestions))
        .route(
            "/v1/media/stacks/suggestions/dismiss",
            post(dismiss_suggestion),
        )
        .route(
            "/v1/media/stacks/{id}",
            get(get_stack).put(put_stack).delete(delete_stack),
        )
}

fn stack_json(lib: &Library, s: StackData) -> Value {
    json!({
        "id": s.id,
        "kind": s.kind,
        "primary": s.primary,
        "count": s.items.len(),
        "created_at": s.created_at,
        "items": s.items.into_iter().map(|it| item_out(lib, it, None)).collect::<Vec<_>>(),
    })
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

fn id_param(id: &str) -> ApiResult<()> {
    if crate::batch::id_ok(id) {
        Ok(())
    } else {
        Err(not_found())
    }
}

async fn get_stack(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    id_param(&id)?;
    let data = lib
        .db
        .call(move |c| load(c, &id))
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "ok": true, "stack": stack_json(&lib, data) })))
}

#[derive(Debug, Deserialize)]
struct CreateReq {
    #[serde(default)]
    ids: Vec<String>,
    primary: Option<String>,
    kind: Option<String>,
}

async fn create_stack(
    State(lib): State<Library>,
    Json(req): Json<CreateReq>,
) -> ApiResult<Response> {
    let mut seen = HashSet::new();
    let ids: Vec<String> = req
        .ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if ids.len() < 2 {
        return Err(bad("a stack needs at least two different photos"));
    }
    if ids.len() > MAX_STACK {
        return Err(bad(format!("at most {MAX_STACK} photos in a stack")));
    }
    if ids.iter().any(|id| !crate::batch::id_ok(id)) {
        return Err(bad("an id is not valid"));
    }
    let primary = req.primary.unwrap_or_else(|| ids[0].clone());
    if !ids.contains(&primary) {
        return Err(bad("primary must be one of ids"));
    }
    let kind = req.kind.unwrap_or_else(|| "manual".into());
    if !KINDS.contains(&kind.as_str()) {
        return Err(bad("kind must be manual, burst or raw_jpeg"));
    }
    let (ids2, primary2) = (ids.clone(), primary.clone());
    let made = lib
        .db
        .call(move |c| match create(c, &ids2, &primary2, &kind)? {
            Ok(sid) => Ok(Ok(load(c, &sid)?)),
            Err(failed) => Ok(Err(failed)),
        })
        .await?;
    match made {
        Ok(Some(data)) => Ok((
            StatusCode::CREATED,
            Json(json!({ "ok": true, "stack": stack_json(&lib, data) })),
        )
            .into_response()),
        Ok(None) => Err(not_found()),
        Err(failed) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "ok": false,
                "error": "some photos cannot be stacked",
                "failed": failed.into_iter().map(|(id, reason)| json!({ "id": id, "reason": reason })).collect::<Vec<_>>(),
            })),
        )
            .into_response()),
    }
}

#[derive(Debug, Deserialize)]
struct PutReq {
    primary: Option<String>,
}

async fn put_stack(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<PutReq>,
) -> ApiResult<Json<Value>> {
    id_param(&id)?;
    let primary = req
        .primary
        .filter(|p| crate::batch::id_ok(p))
        .ok_or_else(|| bad("primary is required"))?;
    let sid = id.clone();
    let (exists, ok) = lib
        .db
        .call(move |c| {
            let exists: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM stacks WHERE id = ?1)",
                [&sid],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok((false, false));
            }
            Ok((true, set_primary(c, &sid, &primary)?))
        })
        .await?;
    if !exists {
        return Err(not_found());
    }
    if !ok {
        return Err(bad("primary must be a member of the stack"));
    }
    let data = lib
        .db
        .call(move |c| load(c, &id))
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "ok": true, "stack": stack_json(&lib, data) })))
}

async fn delete_stack(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    id_param(&id)?;
    let ids = lib
        .db
        .call(move |c| dissolve(c, &id))
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "ok": true, "unstacked": ids })))
}

#[derive(Debug, Deserialize)]
struct SuggestQuery {
    limit: Option<usize>,
}

async fn list_suggestions(
    State(lib): State<Library>,
    Query(q): Query<SuggestQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let (found, items) = lib
        .db
        .call(move |c| {
            let found = suggest(c)?;
            let mut items: Vec<Vec<Item>> = Vec::new();
            for s in found.items.iter().take(limit) {
                let mut group = Vec::new();
                for id in &s.ids {
                    if let Some(it) = db::get_item(c, id)? {
                        group.push(it);
                    }
                }
                group.sort_by(|a, b| {
                    (a.taken_at.as_deref(), &a.id).cmp(&(b.taken_at.as_deref(), &b.id))
                });
                items.push(group);
            }
            Ok((found, items))
        })
        .await?;
    let total = found.items.len();
    let suggestions: Vec<Value> = found
        .items
        .into_iter()
        .zip(items)
        .map(|(s, group)| {
            json!({
                "id": s.id,
                "kind": s.kind,
                "evidence": s.evidence,
                "primary": s.primary,
                "span_ms": s.span_ms,
                "items": group.into_iter().map(|it| item_out(&lib, it, None)).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "hashes_pending": found.pending,
        "total": total,
        "suggestions": suggestions,
    })))
}

#[derive(Debug, Deserialize)]
struct DismissReq {
    #[serde(default)]
    ids: Vec<String>,
}

async fn dismiss_suggestion(
    State(lib): State<Library>,
    Json(req): Json<DismissReq>,
) -> ApiResult<Json<Value>> {
    if req.ids.len() < 2
        || req.ids.len() > MAX_STACK
        || req.ids.iter().any(|i| !crate::batch::id_ok(i))
    {
        return Err(bad("ids must be two to a hundred photo ids"));
    }
    let key = set_key(&req.ids);
    lib.db
        .call(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO stack_dismissed (key, created_at) VALUES (?1, ?2)",
                params![key, now_str()],
            )
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, device: &str, taken_ms: i64, hash: Option<(u64, u64)>) -> Cand {
        Cand {
            id: id.into(),
            hash: format!("h-{id}"),
            bytes: 1000,
            width: Some(4000),
            height: Some(3000),
            taken_ms: Some(taken_ms),
            name: Some(format!("{id}.jpg")),
            source: "phone-ios".into(),
            device: Some(device.into()),
            asset_id: Some(format!("a-{id}")),
            label: "iphone".into(),
            added_at: "2026-01-01T00:00:00.000Z".into(),
            hashes: hash.map(|(p, d)| crate::phash::Hashes {
                phash: p,
                dhash: d,
                width: 32,
                height: 24,
                flat: false,
            }),
        }
    }

    #[test]
    fn shots_within_the_gap_chain_into_one_burst() {
        let th = Thresholds::default();
        let cands = vec![
            cand("a", "d1", 10_000, Some((0, 0))),
            cand("b", "d1", 10_900, Some((1, 1))),
            cand("c", "d1", 11_800, Some((3, 3))),
            // 4 s later: another moment.
            cand("d", "d1", 15_800, Some((3, 3))),
            cand("e", "d1", 16_300, Some((3, 3))),
        ];
        let runs = burst_runs(&cands, &th, BURST_GAP_MS);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, vec![0, 1, 2]);
        assert_eq!(runs[1].0, vec![3, 4]);
        assert!(runs.iter().all(|(_, looked)| *looked));
    }

    #[test]
    fn another_device_or_another_scene_breaks_the_burst() {
        let th = Thresholds::default();
        let far = u64::MAX;
        let cands = vec![
            cand("a", "d1", 1_000, Some((0, 0))),
            // Same moment, different phone.
            cand("b", "d2", 1_100, Some((0, 0))),
            // Same phone, 0.5 s later, but a different picture (all 64 bits differ in both hashes).
            cand("c", "d1", 1_500, Some((far, far))),
        ];
        assert!(burst_runs(&cands, &th, BURST_GAP_MS).is_empty());
    }

    #[test]
    fn an_unhashed_shot_is_judged_by_time_and_says_so() {
        let th = Thresholds::default();
        let cands = vec![
            cand("a", "d1", 1_000, Some((0, 0))),
            cand("b", "d1", 1_400, None),
        ];
        let runs = burst_runs(&cands, &th, BURST_GAP_MS);
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].1);
    }

    #[test]
    fn a_portrait_next_to_a_landscape_is_not_one_burst_but_a_turned_phone_is() {
        let th = Thresholds::default();
        let mut square = cand("b", "d1", 1_300, None);
        square.width = Some(3000);
        square.height = Some(3000);
        let turned = {
            let mut c = cand("c", "d1", 1_600, None);
            c.width = Some(3000);
            c.height = Some(4000);
            c
        };
        let cands = vec![cand("a", "d1", 1_000, None), square, turned.clone()];
        assert!(burst_runs(&cands, &th, BURST_GAP_MS).is_empty());
        let runs = burst_runs(&[cand("a", "d1", 1_000, None), turned], &th, BURST_GAP_MS);
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn a_long_burst_is_cut_into_stacks_of_at_most_a_hundred() {
        let th = Thresholds::default();
        let cands: Vec<Cand> = (0..230)
            .map(|i| cand(&format!("p{i:03}"), "d1", i * 100, None))
            .collect();
        let runs = burst_runs(&cands, &th, BURST_GAP_MS);
        let sizes: Vec<usize> = runs.iter().map(|(r, _)| r.len()).collect();
        assert_eq!(sizes, vec![100, 100, 30]);
    }

    #[test]
    fn a_raw_and_its_picture_pair_up_by_name() {
        let mut raw = cand("r", "d1", 5_000, None);
        raw.name = Some("img_0001.dng".into());
        let mut jpg = cand("j", "d1", 5_000, None);
        jpg.name = Some("img_0001.jpg".into());
        let mut other = cand("o", "d1", 5_000, None);
        other.name = Some("img_0002.jpg".into());
        let mut later = cand("l", "d1", 90_000, None);
        later.name = Some("img_0003.jpg".into());
        let mut later_raw = cand("m", "d1", 5_000, None);
        later_raw.name = Some("img_0003.dng".into());
        let pairs = raw_pairs(&[raw, jpg, other, later, later_raw]);
        // IMG_0003's halves are 85 s apart: not one shot.
        assert_eq!(pairs, vec![vec![1, 0]]);
    }

    #[test]
    fn the_suggestion_key_does_not_depend_on_order() {
        let a = ["x".to_string(), "y".to_string()];
        let b = ["y".to_string(), "x".to_string()];
        assert_eq!(set_key(&a), set_key(&b));
        assert_ne!(set_key(&a), set_key(&["x".to_string(), "z".to_string()]));
    }
}

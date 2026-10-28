//! Automatic photo categories in the catalog: the background step, the decider the service uses,
//! and the SQL the other Gallery queries share. The pure parts (categories, the feature record,
//! zero-shot scoring, the rules) are in `lucina-media-ml`'s `categories`; the routes are in
//! `categories_http.rs`.
//!
//! - **The step** ([`step`]) is the next job of smart search's single background thread
//!   (`smart.rs`, `embed_loop`): when nothing is left to embed, before the face scan, it decides
//!   the category of up to [`BATCH`] photos that have a current vector, newest first. It reads the
//!   catalog and the stored vectors only (no picture, no original), so it costs milliseconds.
//!   Pauses, nice and the single thread are the loop's.
//! - **What is stored** is the decision, who made it, the embedding model, the features version,
//!   the decider's id and the feature record (no file names; `ml_categories`). A photo is decided
//!   again when the model, the features version or the decider changes, or when the photo does.
//! - **The owner's corrections** (`decided_by = 'owner'`) are never overwritten by any decider
//!   (the SQL refuses it) and are kept as labelled examples ([`examples`]).
//! - **Automatic albums are views.** There are no rows in `albums` or `album_items`: a category
//!   page is the Gallery's listing with one more condition ([`filter_sql`]), read-only, never
//!   shareable, absent from the album picker, and the photo stays in the Gallery.
//! - **Sensitive categories** (`document`, `receipt`, `screenshot`, `ticket`) stay out of Places,
//!   the memories strips and any share suggestion: [`NOT_SENSITIVE_SQL`] is the one clause the
//!   queries in `browse.rs` and `db::list_filters` add. "On this day" keeps real photos only
//!   ([`REAL_PHOTOS_SQL`]).
//! - **Which decider.** The service decides with [`RulesDecider`] until a decider is installed
//!   with [`install_decider`]; a decider with another id decides every photo again.
//! - Nothing leaves nezopt: the record has no file names and no pixels, and no decider here
//!   makes a network call.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use lucina_media_ml::categories::{
    Category, Decider, Facts, Features, PromptVectors, RulesDecider, SourceKind, FEATURES_VERSION,
};
use lucina_media_ml::categories_store::{self as cs, DecidedBy, NewDecision};
use lucina_media_ml::{store, Embedder};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::db;
use crate::gallery::{self, SourceGroup};
use crate::ingest::Library;
use crate::smart::Smart;

/// Photos decided per step.
pub const BATCH: usize = 64;
/// Rows per write transaction.
const WRITE_CHUNK: usize = 200;
/// After the text tower fails, wait this long before trying to embed the prompts again.
const PROMPT_RETRY: Duration = Duration::from_secs(300);

/// The tables (one line in `db::migrate`).
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    cs::migrate(c)
}

// ---- SQL the other queries share ---------------------------------------------------------------

/// "Not a sensitive category", on `items i`: Places, the memories strips and any share suggestion
/// add this one clause. Photos not decided yet pass (they have no row).
pub const NOT_SENSITIVE_SQL: &str = "NOT EXISTS (SELECT 1 FROM ml_categories mc WHERE mc.item_id = i.id AND mc.category IN ('screenshot', 'receipt', 'document', 'ticket'))";

/// "A real photo" (photo, food, pet, or not decided yet), on `items i`: "on this day" adds this.
pub const REAL_PHOTOS_SQL: &str = "NOT EXISTS (SELECT 1 FROM ml_categories mc WHERE mc.item_id = i.id AND mc.category NOT IN ('photo', 'food', 'pet'))";

/// The listing condition of an automatic album, on `items i`; `args` gets the category if it
/// needs one. `photo` is everything that no other category claims.
pub fn filter_sql(cat: Category, args: &mut Vec<rusqlite::types::Value>) -> String {
    if cat == Category::Photo {
        return "NOT EXISTS (SELECT 1 FROM ml_categories mc WHERE mc.item_id = i.id AND mc.category != 'photo')"
            .to_string();
    }
    args.push(cat.as_str().to_string().into());
    format!(
        "EXISTS (SELECT 1 FROM ml_categories mc WHERE mc.item_id = i.id AND mc.category = ?{})",
        args.len()
    )
}

// ---- the decider and the cached prompts --------------------------------------------------------

/// Categories for one catalog: the decider and the prompt vectors of the current model.
pub struct Categories {
    decider: RwLock<Arc<dyn Decider>>,
    prompts: Mutex<Option<Arc<PromptVectors>>>,
    prompts_failed: Mutex<Option<Instant>>,
    running: AtomicBool,
}

impl Categories {
    fn new() -> Categories {
        Categories {
            decider: RwLock::new(Arc::new(RulesDecider::default())),
            prompts: Mutex::new(None),
            prompts_failed: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    pub fn decider(&self) -> Arc<dyn Decider> {
        self.decider
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The prompt vectors of `embedder`'s model: cached per model, built on first use. `None`
    /// while the text tower fails (it is tried again after [`PROMPT_RETRY`]).
    fn prompt_vectors(&self, embedder: &dyn Embedder) -> Option<Arc<PromptVectors>> {
        let mut cache = self.prompts.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(p) = cache.as_ref().filter(|p| p.model() == embedder.model()) {
            return Some(p.clone());
        }
        let mut failed = self
            .prompts_failed
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if failed.is_some_and(|at| at.elapsed() < PROMPT_RETRY) {
            return None;
        }
        match PromptVectors::build(embedder) {
            Ok(p) => {
                *failed = None;
                let p = Arc::new(p);
                *cache = Some(p.clone());
                Some(p)
            }
            Err(e) => {
                tracing::warn!(error = %e, "categories: could not embed the category prompts");
                *failed = Some(Instant::now());
                None
            }
        }
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Categories>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<Categories>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The categories state of `lib`'s catalog (created with the rules decider on first use).
pub fn state(lib: &Library) -> Arc<Categories> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(lib.db.path().to_path_buf())
        .or_insert_with(|| Arc::new(Categories::new()))
        .clone()
}

/// Chooses the decider of `lib`'s catalog. Photos decided by another decider id are decided
/// again by this one, one batch at a time; the owner's choices stay.
pub fn install_decider(lib: &Library, decider: Arc<dyn Decider>) {
    *state(lib)
        .decider
        .write()
        .unwrap_or_else(|p| p.into_inner()) = decider;
}

/// The decider in use.
pub fn decider(lib: &Library) -> Arc<dyn Decider> {
    state(lib).decider()
}

// ---- facts from the catalog --------------------------------------------------------------------

/// What the catalog knows about one photo, as the step reads it.
#[derive(Clone, Debug)]
struct ItemFacts {
    id: String,
    kind: String,
    width: Option<i64>,
    height: Option<i64>,
    mime: String,
    source: String,
    device: Option<String>,
    origin_name: Option<String>,
    title: String,
    camera: Option<String>,
    has_gps: bool,
}

const FACT_COLS: &str =
    "i.id, i.kind, i.width, i.height, i.mime, i.source, i.device, i.origin_name, \
     i.title, i.camera, (i.lat IS NOT NULL AND i.lon IS NOT NULL)";

fn facts_from_row(r: &rusqlite::Row) -> rusqlite::Result<ItemFacts> {
    Ok(ItemFacts {
        id: r.get(0)?,
        kind: r.get(1)?,
        width: r.get(2)?,
        height: r.get(3)?,
        mime: r.get(4)?,
        source: r.get(5)?,
        device: r.get(6)?,
        origin_name: r.get(7)?,
        title: r.get(8)?,
        camera: r.get(9)?,
        has_gps: r.get::<_, i64>(10)? != 0,
    })
}

/// Where an item came from, as the record's [`SourceKind`].
pub fn source_kind(source: &str, device: Option<&str>) -> SourceKind {
    match gallery::source_group(source) {
        Some(SourceGroup::Chat) => SourceKind::Chat,
        Some(SourceGroup::Lucina) => SourceKind::Lucina,
        None if source.to_ascii_lowercase().starts_with("phone") || device.is_some() => {
            SourceKind::PhoneBackup
        }
        None if source.trim().is_empty() => SourceKind::Other,
        None => SourceKind::DriveFolder,
    }
}

impl ItemFacts {
    fn features(&self, zs: &lucina_media_ml::categories::ZeroShot) -> Features {
        Features::build(
            &Facts {
                kind: &self.kind,
                width: self.width,
                height: self.height,
                mime: &self.mime,
                source_kind: source_kind(&self.source, self.device.as_deref()),
                // Only a hint is kept; the original name is preferred, the title is the fallback.
                name: self.origin_name.as_deref().or(Some(self.title.as_str())),
                camera: self.camera.as_deref(),
                has_gps: self.has_gps,
            },
            zs,
        )
    }
}

fn facts_of(c: &Connection, id: &str) -> rusqlite::Result<Option<ItemFacts>> {
    use rusqlite::OptionalExtension;
    c.query_row(
        &format!("SELECT {FACT_COLS} FROM items i WHERE i.id = ?1"),
        [id],
        facts_from_row,
    )
    .optional()
}

// ---- the step ----------------------------------------------------------------------------------

/// Photos with a ready rendition and a current vector of the model that have no current decision:
/// none, or one by another model, features version or decider, or one older than the photo's last
/// change. The owner's choices are never pending. `?4`: ids to leave out (JSON array), the ones
/// that failed in this process.
const PENDING_WHERE: &str = "i.kind = 'photo' AND i.thumb_state = 'ready' \
     AND i.trashed_at IS NULL AND i.hidden = 0 \
     AND EXISTS (SELECT 1 FROM ml_embeddings e WHERE e.item_id = i.id AND e.model = ?1 \
                 AND e.created_at >= i.updated_at) \
     AND NOT EXISTS (SELECT 1 FROM ml_categories c WHERE c.item_id = i.id AND \
                     (c.decided_by = 'owner' OR (c.model = ?1 AND c.features_version = ?2 \
                      AND c.decider_id = ?3 AND c.decided_at >= i.updated_at))) \
     AND i.id NOT IN (SELECT value FROM json_each(?4))";

fn skip_json(skip: &HashSet<String>) -> String {
    serde_json::to_string(&skip.iter().collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
}

fn pending_batch(
    c: &Connection,
    model: &str,
    decider_id: &str,
    skip: &HashSet<String>,
    n: usize,
) -> rusqlite::Result<Vec<ItemFacts>> {
    let mut st = c.prepare(&format!(
        "SELECT {FACT_COLS} FROM items i WHERE {PENDING_WHERE} ORDER BY i.sort_at DESC, i.id DESC LIMIT ?5"
    ))?;
    let rows = st.query_map(
        params![
            model,
            FEATURES_VERSION as i64,
            decider_id,
            skip_json(skip),
            n as i64
        ],
        facts_from_row,
    )?;
    rows.collect()
}

fn pending_count(c: &Connection, model: &str, decider_id: &str) -> rusqlite::Result<i64> {
    c.query_row(
        &format!("SELECT COUNT(*) FROM items i WHERE {PENDING_WHERE}"),
        params![model, FEATURES_VERSION as i64, decider_id, "[]"],
        |r| r.get(0),
    )
}

/// Decides `batch` and stores the decisions. Photos that cannot be decided (no vector, the
/// decider failed) go into `skip` so the step does not return to them in this process.
fn decide_batch(
    lib: &Library,
    smart: &Smart,
    batch: &[ItemFacts],
    skip: &mut HashSet<String>,
) -> anyhow::Result<usize> {
    let st = state(lib);
    let embedder = smart.embedder().clone();
    let Some(prompts) = st.prompt_vectors(&*embedder) else {
        return Ok(0);
    };
    let model = embedder.model().to_string();
    let decider = st.decider();
    let decider_id = decider.id();
    let vectors: Vec<Option<Vec<f32>>> = lib.db.with(|c| {
        batch
            .iter()
            .map(|f| store::get(c, &f.id, &model))
            .collect::<rusqlite::Result<_>>()
    })?;
    let mut decided = Vec::new();
    for (facts, vector) in batch.iter().zip(vectors) {
        let Some(zs) = vector.as_deref().and_then(|v| prompts.score(v)) else {
            skip.insert(facts.id.clone());
            continue;
        };
        let features = facts.features(&zs);
        match decider.decide(&features) {
            Ok(d) => decided.push((facts.id.clone(), d, features)),
            Err(e) => {
                tracing::warn!(error = %e, decider = %decider_id, "categories: the decider failed");
                skip.insert(facts.id.clone());
            }
        }
    }
    let now = db::now_str();
    for chunk in decided.chunks(WRITE_CHUNK) {
        lib.db.with(|c| {
            let tx = c.transaction()?;
            for (id, d, features) in chunk {
                cs::put_auto(
                    &tx,
                    &NewDecision {
                        item_id: id,
                        category: d.category,
                        confidence: d.confidence,
                        kind: decider.kind(),
                        decider_id: &decider_id,
                        model: &model,
                        features,
                        reason: &d.reason,
                    },
                    &now,
                )?;
            }
            tx.commit()
        })?;
    }
    Ok(batch.len())
}

/// Decides the category of the next pending photos; how many were looked at (0 = nothing to do).
/// A step of smart search's background thread, after the embedding and before the face scan.
pub fn step(lib: &Library, smart: &Smart, skip: &mut HashSet<String>) -> anyhow::Result<usize> {
    let st = state(lib);
    let model = smart.embedder().model().to_string();
    let decider_id = st.decider().id();
    let batch = lib
        .db
        .with(|c| pending_batch(c, &model, &decider_id, skip, BATCH))?;
    if batch.is_empty() {
        st.running.store(false, Ordering::Relaxed);
        return Ok(0);
    }
    let n = decide_batch(lib, smart, &batch, skip)?;
    st.running.store(n > 0, Ordering::Relaxed);
    Ok(n)
}

/// Decides one photo now, best effort (smart search installed, a vector stored): after the owner
/// takes a choice back. The stored row, if any.
pub fn decide_now(lib: &Library, item_id: &str) -> anyhow::Result<Option<cs::Row>> {
    let Some(smart) = crate::smart::state(lib) else {
        return Ok(None);
    };
    let Some(facts) = lib.db.with(|c| facts_of(c, item_id))? else {
        return Ok(None);
    };
    if facts.kind != "photo" {
        return Ok(None);
    }
    decide_batch(lib, &smart, &[facts], &mut HashSet::new())?;
    Ok(lib.db.with(|c| cs::get(c, item_id))?)
}

/// The record of a photo as it is now, when smart search has its vector: what an owner's label
/// keeps as a labelled example if the machine had not decided yet.
pub fn features_now(lib: &Library, item_id: &str) -> anyhow::Result<Option<Features>> {
    let Some(smart) = crate::smart::state(lib) else {
        return Ok(None);
    };
    let st = state(lib);
    let embedder = smart.embedder().clone();
    let Some(prompts) = st.prompt_vectors(&*embedder) else {
        return Ok(None);
    };
    let model = embedder.model().to_string();
    let (facts, vector) = lib
        .db
        .with(|c| Ok((facts_of(c, item_id)?, store::get(c, item_id, &model)?)))?;
    Ok(facts
        .zip(vector)
        .and_then(|(f, v)| prompts.score(&v).map(|zs| f.features(&zs))))
}

// ---- reading -----------------------------------------------------------------------------------

/// A stored decision as the API shows it.
pub fn row_json(r: &cs::Row) -> Value {
    json!({
        "category": r.category.as_str(),
        "confidence": r.confidence,
        "decided_by": r.decided_by.as_str(),
        "auto_category": r.auto_category.map(Category::as_str),
    })
}

/// The `category` of `GET items/{id}/details`: the decision, or `null` while the photo has none.
pub fn of_item(c: &Connection, item_id: &str) -> rusqlite::Result<Value> {
    Ok(cs::get(c, item_id)?.map_or(Value::Null, |r| row_json(&r)))
}

/// Visible photos per category under `q`'s filters (`photo` is not counted), in the apps' order,
/// each with the newest photo as its cover.
pub fn counts(
    c: &Connection,
    q: &db::ListQuery,
) -> rusqlite::Result<Vec<(Category, i64, Option<String>)>> {
    let (wh, args) = db::list_filters(q);
    let cond = wh.join(" AND ");
    let mut st = c.prepare(&format!(
        "SELECT mc.category, COUNT(*) FROM items i JOIN ml_categories mc ON mc.item_id = i.id \
         WHERE {cond} AND mc.category != 'photo' GROUP BY mc.category"
    ))?;
    let found: HashMap<String, i64> = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for cat in Category::ALL {
        let Some(&n) = found.get(cat.as_str()).filter(|n| **n > 0) else {
            continue;
        };
        let mut a = args.clone();
        a.push(cat.as_str().to_string().into());
        let cover: Option<String> = c
            .query_row(
                &format!(
                    "SELECT i.id FROM items i JOIN ml_categories mc ON mc.item_id = i.id \
                     WHERE {cond} AND mc.category = ?{} ORDER BY i.sort_at DESC, i.id DESC LIMIT 1",
                    a.len()
                ),
                rusqlite::params_from_iter(a.iter()),
                |r| r.get(0),
            )
            .ok();
        out.push((cat, n, cover));
    }
    Ok(out)
}

/// The numbers behind `GET categories/status`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    /// Photos with a current decision (made by the current model, features version and decider, or the owner's).
    pub decided: i64,
    pub pending: i64,
    /// Photos with a decision from another model, version or decider: shown until decided again.
    pub stale: i64,
    pub rules: i64,
    pub jev: i64,
    pub owner: i64,
}

pub fn status_counts(c: &Connection, model: &str, decider_id: &str) -> rusqlite::Result<Counts> {
    let mut out = Counts {
        pending: pending_count(c, model, decider_id)?,
        ..Default::default()
    };
    let mut st = c.prepare(
        "SELECT c.decided_by, \
                (c.decided_by = 'owner' OR (c.model = ?1 AND c.features_version = ?2 AND c.decider_id = ?3)), \
                COUNT(*) \
         FROM ml_categories c JOIN items i ON i.id = c.item_id WHERE i.trashed_at IS NULL \
         GROUP BY 1, 2",
    )?;
    let rows = st.query_map(params![model, FEATURES_VERSION as i64, decider_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? != 0,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (by, current, n) = row?;
        if !current {
            out.stale += n;
            continue;
        }
        out.decided += n;
        match DecidedBy::parse(&by) {
            Some(DecidedBy::Rules) => out.rules += n,
            Some(DecidedBy::Jev) => out.jev += n,
            Some(DecidedBy::Owner) => out.owner += n,
            None => {}
        }
    }
    Ok(out)
}

/// `GET categories/status`.
pub fn status_json(lib: &Library) -> anyhow::Result<Value> {
    let st = state(lib);
    let decider = st.decider();
    let smart = crate::smart::state(lib);
    let model = smart.as_ref().map(|s| s.embedder().model().to_string());
    let n = lib
        .db
        .with(|c| status_counts(c, model.as_deref().unwrap_or(""), &decider.id()))?;
    let (paused_hot, paused_busy) = crate::smart::pauses(lib);
    Ok(json!({
        "ok": true,
        "enabled": smart.is_some(),
        "model": model,
        "features_version": FEATURES_VERSION,
        "decider": { "kind": decider.kind().as_str(), "id": decider.id() },
        "decided": n.decided,
        "pending": if smart.is_some() { n.pending } else { 0 },
        "stale": n.stale,
        "by_decider": { "rules": n.rules, "jev": n.jev, "owner": n.owner },
        "running": st.running.load(Ordering::Relaxed),
        "paused_hot": paused_hot,
        "paused_busy": paused_busy,
    }))
}

/// The owner's corrections as labelled examples for a better decider, oldest first.
pub fn examples(
    c: &Connection,
    after: Option<(&str, &str)>,
    limit: usize,
) -> rusqlite::Result<Vec<Value>> {
    Ok(cs::examples(c, after, limit)?
        .into_iter()
        .map(|e| {
            json!({
                "item_id": e.item_id,
                "category": e.category.as_str(),
                "auto_category": e.auto_category.map(Category::as_str),
                "features": e.features,
                "model": e.model,
                "labelled_at": e.labelled_at,
            })
        })
        .collect())
}

/// Drops decisions of items that no longer exist (the hourly prune of smart search).
pub fn prune(lib: &Library) -> anyhow::Result<usize> {
    Ok(lib.db.with(|c| cs::prune(c, "SELECT id FROM items"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_sql_matches_the_category_sets() {
        // The literal lists in the SQL name the same categories as the enum's sets.
        let in_sql = |sql: &str| -> Vec<Category> {
            let inner = sql.split('(').next_back().unwrap_or("");
            let mut v: Vec<Category> = inner.split('\'').filter_map(Category::parse).collect();
            v.sort();
            v
        };
        let sorted = |s: &[Category]| {
            let mut v = s.to_vec();
            v.sort();
            v
        };
        assert_eq!(in_sql(NOT_SENSITIVE_SQL), sorted(&Category::SENSITIVE));
        assert_eq!(in_sql(REAL_PHOTOS_SQL), sorted(&Category::REAL_PHOTOS));
    }

    #[test]
    fn the_listing_condition() {
        let mut args = Vec::new();
        let sql = filter_sql(Category::Receipt, &mut args);
        assert!(sql.contains("mc.category = ?1"));
        assert_eq!(args.len(), 1);
        let mut none = Vec::new();
        let photo = filter_sql(Category::Photo, &mut none);
        assert!(photo.starts_with("NOT EXISTS") && none.is_empty());
    }

    #[test]
    fn sources_become_kinds() {
        assert_eq!(
            source_kind("phone-ios", Some("ios-1")),
            SourceKind::PhoneBackup
        );
        assert_eq!(source_kind("phone-android", None), SourceKind::PhoneBackup);
        assert_eq!(source_kind("ios-share", Some("ios-1")), SourceKind::Chat);
        assert_eq!(source_kind("telegram", None), SourceKind::Chat);
        assert_eq!(source_kind("lucina-lab", None), SourceKind::Lucina);
        assert_eq!(source_kind("takeout", None), SourceKind::DriveFolder);
        assert_eq!(source_kind("nextcloud", None), SourceKind::DriveFolder);
        assert_eq!(
            source_kind("mac-backup", Some("mac-1")),
            SourceKind::PhoneBackup
        );
        assert_eq!(source_kind("", None), SourceKind::Other);
    }
}

//! Smart search: find photos by what they show ("beach at sunset", "ricevuta", "whiteboard"),
//! and photos that look like a given one. The vectors come from `lucina-media-ml` (SigLIP2, CPU);
//! this module feeds it the catalog's renditions and answers the HTTP routes
//! (`docs/gallery-vs-immich.md`, gap 1).
//!
//! - `GET embeddings/status`: `{enabled, model, indexed, pending, failed, running, paused_hot}`.
//! - `GET items/search?q=…` with the listing's filters (`kind`, `origin`, `scope`, `from`, `to`,
//!   `album`, `bbox`, `favorite`, `archived`…), `limit` (1..200, 60) and `cursor`: items by
//!   relevance, with `scores` (0..1) in the same order. 503 when smart search is off.
//! - `GET items/{id}/similar`: the same, from a photo's own vector (the photo left out).
//! - Both take `person=<id>` to keep only photos with a face of that person (`faces.rs`).
//!
//! The worker is off unless `LUCINA_MEDIA_ML=1`. It runs on its own thread at nice 19 with 3
//! ONNX threads, embeds the grid rendition (`s.jpg`; a video's `poster.jpg`; never a store
//! original) of photos and videos newest first, 8 at a time in one small transaction, and pauses
//! while the CPU is at or above `LUCINA_MEDIA_ML_TEMP_LIMIT` (90 °C) or the load is above 16. A
//! photo whose item changed after it was embedded (an edit) is embedded again. Nothing in the
//! library is changed: the vectors go to their own tables (`lucina_media_ml::store`).
//!
//! The same thread runs the face scan (`faces::step`) whenever there is nothing left to embed, so
//! one background worker, with these pauses, does all the machine learning.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lucina_media_ml::index::{Hit, VectorIndex};
use lucina_media_ml::siglip::{Siglip, SiglipConfig};
use lucina_media_ml::{store, thermal, Embedder};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, ListQuery};
use crate::http::{item_out, ApiError, ApiResult};
use crate::ingest::Library;

/// Candidates a query ranks before the filters apply.
const MAX_CANDIDATES: usize = 2000;
const DEFAULT_LIMIT: u32 = 60;
const MAX_LIMIT: u32 = 200;
/// Pictures per model run.
const BATCH: usize = 8;
/// Text-to-picture cosine below which a photo is not a match (`relevance` ≈ 0.002), calibrated
/// on synthetic pictures and the library's score spread (`docs/gallery-vs-immich.md`).
pub const DEFAULT_TEXT_MIN: f32 = 0.09;
/// Picture-to-picture cosine below which a photo is not "similar".
pub const DEFAULT_SIMILAR_MIN: f32 = 0.6;
/// Query vectors kept for paging and repeated searches.
const QUERY_CACHE: usize = 32;

#[derive(Clone, Debug)]
pub struct SmartConfig {
    /// `LUCINA_MEDIA_ML=1` turns the worker and the routes on.
    pub enabled: bool,
    pub siglip: SiglipConfig,
    pub temp_limit: f32,
    /// The 1-minute load average above which it waits: `LUCINA_MEDIA_ML_MAX_LOAD` (16).
    pub max_load: f32,
    pub text_min: f32,
    pub similar_min: f32,
}

impl SmartConfig {
    pub fn from_env() -> Self {
        let f = |key: &str, default: f32| {
            std::env::var(key)
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        Self {
            enabled: matches!(
                std::env::var("LUCINA_MEDIA_ML")
                    .ok()
                    .as_deref()
                    .map(str::trim),
                Some("1" | "on" | "true")
            ),
            siglip: SiglipConfig::from_env(),
            temp_limit: f("LUCINA_MEDIA_ML_TEMP_LIMIT", thermal::DEFAULT_LIMIT_C),
            max_load: f("LUCINA_MEDIA_ML_MAX_LOAD", thermal::DEFAULT_MAX_LOAD),
            text_min: f("LUCINA_MEDIA_ML_TEXT_MIN", DEFAULT_TEXT_MIN),
            similar_min: f("LUCINA_MEDIA_ML_SIMILAR_MIN", DEFAULT_SIMILAR_MIN),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Progress {
    running: bool,
    paused_hot: bool,
    paused_busy: bool,
    pending: i64,
    failed: i64,
}

/// Smart search for one catalog: the model, every vector in memory, the worker's progress.
pub struct Smart {
    embedder: Arc<dyn Embedder>,
    index: RwLock<VectorIndex>,
    progress: Mutex<Progress>,
    queries: Mutex<Vec<(String, Vec<f32>)>>,
    text_min: f32,
    similar_min: f32,
}

/// One per catalog (the service has one; tests open many).
fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Smart>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<Smart>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Whether the worker is paused for heat or load (shown by the faces status too).
pub fn pauses(lib: &Library) -> (bool, bool) {
    state(lib).map_or((false, false), |s| {
        let p = s.progress.lock().unwrap_or_else(|p| p.into_inner());
        (p.paused_hot, p.paused_busy)
    })
}

pub fn state(lib: &Library) -> Option<Arc<Smart>> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(lib.db.path())
        .cloned()
}

/// Loads `embedder`'s vectors from the catalog and turns the routes on for `lib`.
pub fn install(
    lib: &Library,
    embedder: Arc<dyn Embedder>,
    text_min: f32,
    similar_min: f32,
) -> anyhow::Result<Arc<Smart>> {
    let (model, dim) = (embedder.model().to_string(), embedder.dim());
    let index = lib.db.with(|c| store::load(c, &model, dim))?;
    let smart = Arc::new(Smart {
        embedder,
        index: RwLock::new(index),
        progress: Mutex::new(Progress::default()),
        queries: Mutex::new(Vec::new()),
        text_min,
        similar_min,
    });
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(lib.db.path().to_path_buf(), smart.clone());
    Ok(smart)
}

/// The tables (one line in `db::migrate`).
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    store::migrate(c)
}

/// Photos and videos with a rendition and no current vector of `model`, newest first. A vector
/// older than the item's last change (an edit, a new version) no longer counts.
const PENDING_WHERE: &str = "i.kind IN ('photo', 'video') AND i.thumb_state = 'ready' \
     AND i.trashed_at IS NULL AND i.hidden = 0 \
     AND NOT EXISTS (SELECT 1 FROM ml_embed_failures f WHERE f.item_id = i.id AND f.model = ?1) \
     AND NOT EXISTS (SELECT 1 FROM ml_embeddings e WHERE e.item_id = i.id AND e.model = ?1 \
                     AND e.created_at >= i.updated_at)";

fn pending_batch(c: &Connection, model: &str, n: usize) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = c.prepare(&format!(
        "SELECT i.id, i.kind FROM items i WHERE {PENDING_WHERE} ORDER BY i.sort_at DESC, i.id DESC LIMIT ?2"
    ))?;
    let rows = st.query_map(params![model, n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn pending_count(c: &Connection, model: &str) -> rusqlite::Result<i64> {
    c.query_row(
        &format!("SELECT COUNT(*) FROM items i WHERE {PENDING_WHERE}"),
        [model],
        |r| r.get(0),
    )
}

/// The rendition a picture is embedded from: the grid JPEG, a video's poster, the large JPEG.
fn rendition_for(thumbs_dir: &FsPath, kind: &str) -> Option<PathBuf> {
    let names: &[&str] = if kind == "video" {
        &["poster.jpg", "s.jpg", "l.jpg"]
    } else {
        &["s.jpg", "l.jpg"]
    };
    names
        .iter()
        .map(|n| thumbs_dir.join(n))
        .find(|p| p.is_file())
}

/// Embeds up to [`BATCH`] pending pictures; how many were handled (0 = nothing pending).
pub fn embed_pending(lib: &Library, smart: &Smart) -> anyhow::Result<usize> {
    let model = smart.embedder.model().to_string();
    let batch = lib.db.with(|c| pending_batch(c, &model, BATCH))?;
    if batch.is_empty() {
        return Ok(0);
    }
    let mut ids = Vec::new();
    let mut images = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    for (id, kind) in &batch {
        let dir = crate::thumbs::dir_for(&lib.cfg, id);
        match rendition_for(&dir, kind).map(|p| image::open(&p).map(|i| i.to_rgb8())) {
            Some(Ok(img)) => {
                ids.push(id.clone());
                images.push(img);
            }
            Some(Err(e)) => failures.push((id.clone(), format!("unreadable rendition: {e}"))),
            None => failures.push((id.clone(), "no rendition".into())),
        }
    }
    let vectors = match smart.embedder.embed_images(&images) {
        Ok(v) if v.len() == ids.len() => v,
        Ok(_) => anyhow::bail!("the model answered a different number of vectors"),
        // One bad picture must not block the batch: try them one by one.
        Err(e) if images.len() > 1 => {
            tracing::warn!(error = %e, "smart search: batch failed, embedding one by one");
            let mut out = Vec::new();
            let mut kept = Vec::new();
            for (id, img) in ids.iter().zip(images) {
                match smart.embedder.embed_images(std::slice::from_ref(&img)) {
                    Ok(mut v) if v.len() == 1 => {
                        kept.push(id.clone());
                        out.push(v.remove(0));
                    }
                    Ok(_) => failures.push((id.clone(), "no vector".into())),
                    Err(e) => failures.push((id.clone(), e.to_string())),
                }
            }
            ids = kept;
            out
        }
        Err(e) => {
            failures.extend(ids.drain(..).map(|id| (id, e.to_string())));
            Vec::new()
        }
    };
    let now = db::now_str();
    lib.db.with(|c| {
        let tx = c.transaction()?;
        for (id, v) in ids.iter().zip(&vectors) {
            store::put(&tx, id, &model, v, &now)?;
        }
        for (id, e) in &failures {
            store::put_failure(&tx, id, &model, e, &now)?;
        }
        tx.commit()
    })?;
    let mut ix = smart.index.write().unwrap_or_else(|p| p.into_inner());
    for (id, v) in ids.iter().zip(&vectors) {
        ix.upsert(id, v);
    }
    Ok(batch.len())
}

fn set_progress(smart: &Smart, f: impl FnOnce(&mut Progress)) {
    f(&mut smart.progress.lock().unwrap_or_else(|p| p.into_inner()));
}

fn refresh_counts(lib: &Library, smart: &Smart) {
    let model = smart.embedder.model().to_string();
    if let Ok((pending, failed)) = lib
        .db
        .with(|c| Ok((pending_count(c, &model)?, store::failures(c, &model)?)))
    {
        set_progress(smart, |p| {
            p.pending = pending;
            p.failed = failed;
        });
    }
}

/// Lowers the calling thread's CPU priority (Linux: nice is per thread; threads it starts, like
/// ONNX Runtime's pool, inherit it).
fn nice_this_thread(n: i32) {
    // SAFETY: setpriority with who = 0 only changes the calling thread's own nice value.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, n);
    }
}

/// The embedding loop, on its own thread. Returns only if the model cannot be loaded.
fn embed_loop(
    lib: Library,
    smart: Arc<Smart>,
    siglip: Option<Arc<Siglip>>,
    temp_limit: f32,
    max_load: f32,
) {
    nice_this_thread(19);
    let mut paused = false;
    let mut last_prune = Instant::now() - Duration::from_secs(3600);
    loop {
        if let Some(s) = &siglip {
            s.release_idle(Duration::from_secs(600), false);
        }
        paused = thermal::should_pause(thermal::cpu_temp_c(), temp_limit, paused);
        let busy = thermal::load_avg().is_some_and(|l| l > max_load);
        set_progress(&smart, |p| {
            p.paused_hot = paused;
            p.paused_busy = busy;
        });
        if paused || busy {
            std::thread::sleep(Duration::from_secs(60));
            continue;
        }
        match embed_pending(&lib, &smart) {
            Ok(0) => {
                // Nothing to embed: the face scan, when the owner turned it on.
                match crate::faces::step(&lib) {
                    Ok(n) if n > 0 => {
                        set_progress(&smart, |p| p.running = true);
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "face scan step failed");
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                }
                set_progress(&smart, |p| p.running = false);
                if let Some(s) = &siglip {
                    s.release_idle(Duration::from_secs(600), true);
                }
                if last_prune.elapsed() > Duration::from_secs(3600) {
                    match lib.db.with(|c| store::prune(c, "SELECT id FROM items")) {
                        Ok(n) if n > 0 => tracing::info!(
                            rows = n,
                            "smart search: vectors of purged items dropped"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "smart search: prune failed"),
                    }
                    if let Err(e) = crate::faces::prune(&lib) {
                        tracing::warn!(error = %e, "faces: prune failed");
                    }
                    last_prune = Instant::now();
                }
                refresh_counts(&lib, &smart);
                std::thread::sleep(Duration::from_secs(60));
            }
            Ok(_) => {
                set_progress(&smart, |p| p.running = true);
                refresh_counts(&lib, &smart);
                // Room for the rest of the machine between batches.
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!(error = %e, "smart search: embedding step failed");
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    }
}

/// Starts smart search when `LUCINA_MEDIA_ML=1`: loads the model, the vectors, then embeds in
/// the background. A missing model or runtime leaves it off (logged), nothing else changes.
pub async fn worker(lib: Library, cfg: SmartConfig) {
    if !cfg.enabled {
        return;
    }
    let siglip_cfg = cfg.siglip.clone();
    let loaded = tokio::task::spawn_blocking(move || Siglip::load(siglip_cfg)).await;
    let siglip = match loaded {
        Ok(Ok(s)) => Arc::new(s),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "smart search is off");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "smart search is off");
            return;
        }
    };
    let l2 = lib.clone();
    let embedder: Arc<dyn Embedder> = siglip.clone();
    let smart = match tokio::task::spawn_blocking(move || {
        install(&l2, embedder, cfg.text_min, cfg.similar_min)
    })
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "smart search: loading the vectors failed");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "smart search: loading the vectors failed");
            return;
        }
    };
    tracing::info!(
        model = smart.embedder.model(),
        vectors = smart.index.read().unwrap_or_else(|p| p.into_inner()).len(),
        "smart search on"
    );
    // Face recognition rides on this thread; it stays off until the owner turns it on.
    crate::faces::install_real(lib.clone()).await;
    let (limit, max_load) = (cfg.temp_limit, cfg.max_load);
    if let Err(e) = std::thread::Builder::new()
        .name("smart-embed".into())
        .spawn(move || embed_loop(lib, smart, Some(siglip), limit, max_load))
    {
        tracing::warn!(error = %e, "smart search: could not start the embedding thread");
    }
}

// ---- HTTP

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/embeddings/status", get(status))
        .route("/v1/media/items/search", get(search))
        .route("/v1/media/items/{id}/similar", get(similar))
}

fn off() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "smart search is off".into(),
    )
}

fn status_json(lib: &Library) -> Value {
    match state(lib) {
        None => json!({
            "enabled": false, "model": null, "indexed": 0, "pending": 0, "failed": 0,
            "running": false, "paused_hot": false, "paused_busy": false,
        }),
        Some(s) => {
            let p = s.progress.lock().unwrap_or_else(|p| p.into_inner()).clone();
            json!({
                "enabled": true,
                "model": s.embedder.model(),
                "indexed": s.index.read().unwrap_or_else(|p| p.into_inner()).len(),
                "pending": p.pending,
                "failed": p.failed,
                "running": p.running,
                "paused_hot": p.paused_hot,
                "paused_busy": p.paused_busy,
            })
        }
    }
}

async fn status(State(lib): State<Library>) -> Json<Value> {
    if let Some(s) = state(&lib) {
        let l2 = lib.clone();
        let _ = tokio::task::spawn_blocking(move || refresh_counts(&l2, &s)).await;
    }
    Json(status_json(&lib))
}

/// The listing's filters, checked the way `GET items` checks them; `q` and `cursor` are taken out.
fn filters(lib: &Library, mut q: ListQuery) -> ApiResult<(ListQuery, usize, usize)> {
    q.film_folders = lib.cfg.film_folders.clone();
    q.origin = q.origin.take().filter(|o| !o.is_empty());
    if q.origin
        .as_deref()
        .is_some_and(|o| crate::gallery::Origin::parse(o).is_none())
    {
        return Err(crate::http::bad(
            "origin must be all, device:<id>, chat or lucina",
        ));
    }
    if q.bbox
        .as_deref()
        .is_some_and(|b| crate::browse::parse_bbox(b).is_none())
    {
        return Err(crate::http::bad(
            "bbox must be west,south,east,north in degrees",
        ));
    }
    let offset = match q.cursor.take() {
        None => 0,
        Some(c) => c
            .strip_prefix('o')
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| crate::http::bad("bad cursor"))?,
    };
    let limit = q.limit.take().unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    q.q = None;
    Ok((q, offset, limit))
}

/// The ranked hits that pass the filters.
fn filter_hits(c: &Connection, q: &ListQuery, hits: Vec<Hit>) -> rusqlite::Result<Vec<Hit>> {
    if hits.is_empty() {
        return Ok(hits);
    }
    let (mut wh, mut args) = db::list_filters(q);
    let ids = serde_json::to_string(&hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".into());
    args.push(ids.into());
    wh.push(format!(
        "i.id IN (SELECT value FROM json_each(?{}))",
        args.len()
    ));
    let mut st = c.prepare(&format!(
        "SELECT i.id FROM items i WHERE {}",
        wh.join(" AND ")
    ))?;
    let ok: std::collections::HashSet<String> = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(hits.into_iter().filter(|h| ok.contains(&h.id)).collect())
}

/// One page of ranked hits as the listing's item JSON, with scores.
async fn page(
    lib: &Library,
    hits: Vec<Hit>,
    q: ListQuery,
    offset: usize,
    limit: usize,
    score: impl Fn(f32) -> f32,
) -> ApiResult<Json<Value>> {
    let (total, items, scores) = lib
        .db
        .call(move |c| {
            let ranked = filter_hits(c, &q, hits)?;
            let total = ranked.len();
            let mut items = Vec::new();
            let mut scores = Vec::new();
            for h in ranked.into_iter().skip(offset).take(limit) {
                if let Some(it) = db::get_item(c, &h.id)? {
                    items.push(it);
                    scores.push(h.score);
                }
            }
            Ok((total, items, scores))
        })
        .await?;
    let next = offset + limit;
    Ok(Json(json!({
        "ok": true,
        "items": items.into_iter().map(|it| item_out(lib, it, None)).collect::<Vec<_>>(),
        "scores": scores.into_iter().map(|s| (score(s) * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
        "next_cursor": (next < total).then(|| format!("o{next}")),
        "total": total,
        "status": status_json(lib),
    })))
}

impl Smart {
    /// A query's vector, from the cache when it was asked lately.
    fn query_vector(&self, text: &str) -> Result<Vec<f32>, lucina_media_ml::MlError> {
        let key = text.trim().to_lowercase();
        {
            let mut cache = self.queries.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
                let hit = cache.remove(i);
                let v = hit.1.clone();
                cache.push(hit);
                return Ok(v);
            }
        }
        let v = self.embedder.embed_text(&key)?;
        let mut cache = self.queries.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= QUERY_CACHE {
            cache.remove(0);
        }
        cache.push((key, v.clone()));
        Ok(v)
    }
}

/// `person=<id>`: only photos with a face of that person.
#[derive(Deserialize, Default)]
struct PersonParam {
    person: Option<String>,
}

async fn search(
    State(lib): State<Library>,
    Query(q): Query<ListQuery>,
    Query(p): Query<PersonParam>,
) -> ApiResult<Json<Value>> {
    let smart = state(&lib).ok_or_else(off)?;
    let text = q.q.clone().unwrap_or_default();
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(crate::http::bad("q is required"));
    }
    if text.chars().count() > 300 {
        return Err(crate::http::bad("q is too long"));
    }
    let (mut q, offset, limit) = filters(&lib, q)?;
    q.person = p.person.filter(|s| !s.is_empty());
    let s2 = smart.clone();
    let hits =
        tokio::task::spawn_blocking(move || -> Result<Vec<Hit>, lucina_media_ml::MlError> {
            let v = s2.query_vector(&text)?;
            let ix = s2.index.read().unwrap_or_else(|p| p.into_inner());
            Ok(ix.search(&v, MAX_CANDIDATES, s2.text_min, None))
        })
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            tracing::warn!(error = %e, "smart search: query failed");
            off()
        })?;
    let rel = smart.clone();
    page(&lib, hits, q, offset, limit, move |c| {
        rel.embedder.relevance(c)
    })
    .await
}

async fn similar(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
    Query(p): Query<PersonParam>,
) -> ApiResult<Json<Value>> {
    let smart = state(&lib).ok_or_else(off)?;
    let (mut q, offset, limit) = filters(&lib, q)?;
    q.person = p.person.filter(|s| !s.is_empty());
    let id = lib
        .db
        .call(move |c| crate::dedup_merge::resolve(c, &id))
        .await?;
    let s2 = smart.clone();
    let hits = tokio::task::spawn_blocking(move || {
        let ix = s2.index.read().unwrap_or_else(|p| p.into_inner());
        ix.get(&id)
            .map(|v| v.to_vec())
            .map(|v| ix.search(&v, MAX_CANDIDATES, s2.similar_min, Some(&id)))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "this photo has not been looked at yet".into(),
        )
    })?;
    page(&lib, hits, q, offset, limit, |c| c.clamp(0.0, 1.0)).await
}

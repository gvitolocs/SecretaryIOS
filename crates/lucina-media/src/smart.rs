//! Smart search: find photos by what they show ("beach at sunset", "ricevuta", "whiteboard"),
//! and photos that look like a given one. The vectors come from `lucina-media-ml` (EmbeddingGemma 2
//! by default, SigLIP2 on request, CPU); this module feeds it the catalog's renditions and answers
//! the HTTP routes (`docs/gallery-vs-immich.md`, gap 1).
//!
//! - `GET embeddings/status`: `{enabled, model, serving_model, catching_up, catch_up, indexed,
//!   indexed_by_model, pending, failed, running, paused_hot, paused_busy}`.
//! - `GET items/search?q=…` with the listing's filters (`kind`, `origin`, `scope`, `from`, `to`,
//!   `album`, `bbox`, `favorite`, `archived`…), `limit` (1..200, 60) and `cursor`: items by
//!   relevance, with `scores` (0..1) in the same order. 503 when smart search is off.
//! - `GET items/{id}/similar`: the same, from a photo's own vector (the photo left out).
//! - Both take `person=<id>` to keep only photos with a face of that person (`faces.rs`).
//!
//! The model is `LUCINA_MEDIA_ML_MODEL`: `embeddinggemma2` (default) or `siglip2`. Vectors of
//! different models (and of different Gemma picture budgets) are different models and never mix.
//! While the configured model has not yet embedded every photo that the previous model (the other
//! model with the most stored vectors) had, search and similar answer from the previous model's
//! index and embedder, so a model change never empties the search; the status says which model
//! serves and how far the catch-up is. When it is done the previous index and embedder are
//! dropped from memory (its stored vectors stay, so setting the model back is a rollback).
//!
//! The worker is off unless `LUCINA_MEDIA_ML=1`. It runs on its own thread at nice 19 with 3
//! ONNX threads, embeds the grid rendition (`s.jpg`; a video's `poster.jpg`; never a store
//! original) of photos and videos newest first, 8 at a time in one small transaction, and pauses
//! while the CPU is at or above `LUCINA_MEDIA_ML_TEMP_LIMIT` (90 °C) or the load is above 16. A
//! photo whose item changed after it was embedded (an edit) is embedded again. Nothing in the
//! library is changed: the vectors go to their own tables (`lucina_media_ml::store`).
//!
//! The same thread also keeps the perceptual hashes of new photos current (`dedup_hash.rs`, first
//! in each round) and, when the owner turned it on, scans faces (`faces.rs`, last).
//!
//! When there is nothing left to embed the same thread decides the category of new photos from the
//! stored vectors and the catalog (`categories::step`: screenshot, receipt, document...), and then
//! runs the face scan (`faces::step`), so one background worker, with these pauses, does all the
//! machine learning.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lucina_media_ml::categories::Decider;
use lucina_media_ml::gemma::{self, Gemma, GemmaConfig};
use lucina_media_ml::index::{Hit, VectorIndex};
use lucina_media_ml::siglip::{self, Siglip, SiglipConfig};
use lucina_media_ml::{store, thermal, Embedder, MlError};
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
/// SigLIP2's text-to-picture cosine below which a photo is not a match (`relevance` ≈ 0.002),
/// calibrated on synthetic pictures and the library's score spread (`docs/gallery-vs-immich.md`).
/// Each embedder has its own default (`Embedder::default_text_min`); these are SigLIP2's.
pub const DEFAULT_TEXT_MIN: f32 = 0.09;
/// SigLIP2's picture-to-picture cosine below which a photo is not "similar".
pub const DEFAULT_SIMILAR_MIN: f32 = 0.6;
/// Query vectors kept for paging and repeated searches.
const QUERY_CACHE: usize = 32;

/// Which model embeds the library: `LUCINA_MEDIA_ML_MODEL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// EmbeddingGemma 2 (the default).
    Gemma,
    Siglip,
}

impl ModelChoice {
    pub const DEFAULT: ModelChoice = ModelChoice::Gemma;

    /// `embeddinggemma2` | `siglip2` (a few spellings each); `None` for anything else.
    pub fn parse(s: &str) -> Option<ModelChoice> {
        match s
            .trim()
            .to_lowercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "embeddinggemma2" | "embeddinggemma" | "gemma2" | "gemma" => Some(ModelChoice::Gemma),
            "siglip2" | "siglip" => Some(ModelChoice::Siglip),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ModelChoice::Gemma => "embeddinggemma2",
            ModelChoice::Siglip => "siglip2",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SmartConfig {
    /// `LUCINA_MEDIA_ML=1` turns the worker and the routes on.
    pub enabled: bool,
    /// `LUCINA_MEDIA_ML_MODEL`: `embeddinggemma2` (default) or `siglip2`.
    pub model: ModelChoice,
    pub siglip: SiglipConfig,
    pub gemma: GemmaConfig,
    pub temp_limit: f32,
    /// The 1-minute load average above which it waits: `LUCINA_MEDIA_ML_MAX_LOAD` (16).
    pub max_load: f32,
    /// `LUCINA_MEDIA_ML_TEXT_MIN`: replaces the configured model's own default
    /// (`Embedder::default_text_min`); the previous model during a catch-up keeps its own.
    pub text_min: Option<f32>,
    /// `LUCINA_MEDIA_ML_SIMILAR_MIN`, the same for "similar".
    pub similar_min: Option<f32>,
}

impl SmartConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(&|key| std::env::var(key).ok())
    }

    /// The configuration from any source of settings (the environment, or a map in tests).
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Self {
        let opt = |key: &str| {
            get(key)
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
        };
        let f = |key: &str, default: f32| opt(key).unwrap_or(default);
        let model = match get("LUCINA_MEDIA_ML_MODEL").filter(|s| !s.trim().is_empty()) {
            None => ModelChoice::DEFAULT,
            Some(s) => ModelChoice::parse(&s).unwrap_or_else(|| {
                tracing::warn!(
                    value = %s,
                    "LUCINA_MEDIA_ML_MODEL is not embeddinggemma2 or siglip2, using {}",
                    ModelChoice::DEFAULT.as_str()
                );
                ModelChoice::DEFAULT
            }),
        };
        Self {
            enabled: matches!(
                get("LUCINA_MEDIA_ML").as_deref().map(str::trim),
                Some("1" | "on" | "true")
            ),
            model,
            siglip: SiglipConfig::from_env(),
            gemma: GemmaConfig::from_env(),
            temp_limit: f("LUCINA_MEDIA_ML_TEMP_LIMIT", thermal::DEFAULT_LIMIT_C),
            max_load: f("LUCINA_MEDIA_ML_MAX_LOAD", thermal::DEFAULT_MAX_LOAD),
            text_min: opt("LUCINA_MEDIA_ML_TEXT_MIN"),
            similar_min: opt("LUCINA_MEDIA_ML_SIMILAR_MIN"),
        }
    }

    /// The (text, similar) cosine minimums for `embedder`: the environment's override, else the
    /// model's own default.
    pub fn minimums_for(&self, embedder: &dyn Embedder) -> (f32, f32) {
        (
            self.text_min.unwrap_or_else(|| embedder.default_text_min()),
            self.similar_min
                .unwrap_or_else(|| embedder.default_similar_min()),
        )
    }
}

#[derive(Clone, Debug, Default)]
struct Progress {
    running: bool,
    paused_hot: bool,
    paused_busy: bool,
    pending: i64,
    failed: i64,
    /// Vectors stored per model id.
    by_model: BTreeMap<String, i64>,
    /// Photos the previous model had that the configured model has still to embed.
    catch_remaining: i64,
    /// The most that was remaining since the catch-up began (progress = 1 − remaining / total).
    catch_total: i64,
}

/// The model that answered searches before the configured one and still does while the
/// configured one catches up: `embedder` and the thresholds that fit it.
pub struct PreviousModel {
    pub embedder: Arc<dyn Embedder>,
    pub text_min: f32,
    pub similar_min: f32,
}

/// A [`PreviousModel`] with its vectors in memory.
struct Previous {
    embedder: Arc<dyn Embedder>,
    index: VectorIndex,
    text_min: f32,
    similar_min: f32,
}

/// Query vectors, newest last, by (model, normalised query).
type QueryCache = Vec<((String, String), Vec<f32>)>;

/// Smart search for one catalog: the model, every vector in memory, the worker's progress.
pub struct Smart {
    /// The configured model: the worker embeds with it and categories use it.
    embedder: Arc<dyn Embedder>,
    index: RwLock<VectorIndex>,
    /// The model that serves search until `embedder` has caught up; `None` once it has.
    previous: RwLock<Option<Arc<Previous>>>,
    progress: Mutex<Progress>,
    /// Query vectors by (model, normalised query).
    queries: Mutex<QueryCache>,
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
    install_with(lib, embedder, text_min, similar_min, None)
}

/// [`install`] with the model that serves search until `embedder` has embedded every photo it
/// had. A `previous` that has nothing left to hand over (every photo it had is covered, or it is
/// the same model) is dropped at once.
pub fn install_with(
    lib: &Library,
    embedder: Arc<dyn Embedder>,
    text_min: f32,
    similar_min: f32,
    previous: Option<PreviousModel>,
) -> anyhow::Result<Arc<Smart>> {
    let (model, dim) = (embedder.model().to_string(), embedder.dim());
    let index = lib.db.with(|c| store::load(c, &model, dim))?;
    let mut serving = None;
    let mut remaining = 0;
    if let Some(p) = previous.filter(|p| p.embedder.model() != model) {
        let prev = p.embedder.model().to_string();
        remaining = lib.db.with(|c| catchup_remaining(c, &model, &prev))?;
        if remaining > 0 {
            let pix = lib.db.with(|c| store::load(c, &prev, p.embedder.dim()))?;
            serving = Some(Arc::new(Previous {
                embedder: p.embedder,
                index: pix,
                text_min: p.text_min,
                similar_min: p.similar_min,
            }));
        }
    }
    let smart = Arc::new(Smart {
        embedder,
        index: RwLock::new(index),
        previous: RwLock::new(serving),
        progress: Mutex::new(Progress {
            catch_remaining: remaining,
            catch_total: remaining,
            ..Progress::default()
        }),
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

/// Photos that `previous` has a vector of and `model` still has to embed: what a catch-up waits
/// for. Photos `model` cannot embed (failed, no rendition, trashed, hidden) do not hold it up.
fn catchup_remaining(c: &Connection, model: &str, previous: &str) -> rusqlite::Result<i64> {
    c.query_row(
        &format!(
            "SELECT COUNT(*) FROM items i WHERE {PENDING_WHERE} \
             AND EXISTS (SELECT 1 FROM ml_embeddings p WHERE p.item_id = i.id AND p.model = ?2)"
        ),
        params![model, previous],
        |r| r.get(0),
    )
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

/// Re-reads the counts the status shows and, while a previous model serves, checks whether the
/// configured one has caught up: if so, serving switches to it and the previous index and
/// embedder are dropped.
pub fn refresh_counts(lib: &Library, smart: &Smart) {
    let model = smart.embedder.model().to_string();
    if let Ok((pending, failed, by_model)) = lib.db.with(|c| {
        Ok((
            pending_count(c, &model)?,
            store::failures(c, &model)?,
            store::counts_by_model(c)?,
        ))
    }) {
        set_progress(smart, |p| {
            p.pending = pending;
            p.failed = failed;
            p.by_model = by_model;
        });
    }
    let previous = smart
        .previous
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let Some(previous) = previous else { return };
    let prev = previous.embedder.model().to_string();
    match lib.db.with(|c| catchup_remaining(c, &model, &prev)) {
        Ok(0) => {
            *smart.previous.write().unwrap_or_else(|p| p.into_inner()) = None;
            smart
                .queries
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            set_progress(smart, |p| p.catch_remaining = 0);
            tracing::info!(from = %prev, to = %model, "smart search caught up, now served by the configured model");
        }
        Ok(n) => set_progress(smart, |p| {
            p.catch_remaining = n;
            p.catch_total = p.catch_total.max(n);
        }),
        Err(e) => tracing::warn!(error = %e, "smart search: catch-up check failed"),
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
fn embed_loop(lib: Library, smart: Arc<Smart>, temp_limit: f32, max_load: f32) {
    nice_this_thread(19);
    let mut paused = false;
    let mut last_prune = Instant::now() - Duration::from_secs(3600);
    let mut hash_skip = std::collections::HashSet::new();
    let mut category_skip = std::collections::HashSet::new();
    loop {
        smart.release_idle(Duration::from_secs(600), false);
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
        // Perceptual hashes first: milliseconds each, and the duplicate review waits for them.
        match crate::dedup_hash::hash_pending(&lib, &mut hash_skip) {
            Ok(0) => {}
            Ok(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => tracing::warn!(error = %e, "perceptual hash step failed"),
        }
        match embed_pending(&lib, &smart) {
            Ok(0) => {
                // Nothing to embed: decide the category of new photos (`categories.rs`, catalog and
                // vectors only), then the face scan, when the owner turned it on.
                match crate::categories::step(&lib, &smart, &mut category_skip) {
                    Ok(n) if n > 0 => {
                        set_progress(&smart, |p| p.running = true);
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "categories step failed");
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                }
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
                smart.release_idle(Duration::from_secs(600), true);
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
                    if let Err(e) = crate::categories::prune(&lib) {
                        tracing::warn!(error = %e, "categories: prune failed");
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

/// The embedder of a stored model id (`siglip2-base-patch16-256`, `embeddinggemma-2-768-b<N>`),
/// or why it cannot load. Loading checks the files and reads the tokenizer; the heavy sessions
/// load on first use.
pub fn load_embedder(
    model: &str,
    siglip_cfg: &SiglipConfig,
    gemma_cfg: &GemmaConfig,
) -> Result<Arc<dyn Embedder>, MlError> {
    if model == siglip::MODEL {
        return Ok(Arc::new(Siglip::load(siglip_cfg.clone())?));
    }
    if let Some(budget) = gemma::budget_of(model) {
        return Ok(Arc::new(Gemma::load(
            gemma_cfg.clone().with_budget(budget),
        )?));
    }
    Err(MlError::Unavailable(format!(
        "no embedder for the stored model {model}"
    )))
}

/// The configured model's embedder; a Gemma that cannot load (files missing) falls back to
/// SigLIP2 with a warning, so smart search keeps working. The model id stored with the vectors
/// tells them apart, nothing is mixed.
fn load_configured(cfg: &SmartConfig) -> Result<Arc<dyn Embedder>, MlError> {
    match cfg.model {
        ModelChoice::Siglip => load_embedder(siglip::MODEL, &cfg.siglip, &cfg.gemma),
        ModelChoice::Gemma => {
            load_embedder(&gemma::model_id(cfg.gemma.budget), &cfg.siglip, &cfg.gemma).or_else(
                |e| {
                    tracing::warn!(error = %e, "EmbeddingGemma 2 is not available, using SigLIP2");
                    load_embedder(siglip::MODEL, &cfg.siglip, &cfg.gemma)
                },
            )
        }
    }
}

/// The model that served search before `current`: the other model with the most stored vectors,
/// when its files are there. `None` on a fresh catalog or when it cannot be loaded.
fn previous_for(lib: &Library, cfg: &SmartConfig, current: &str) -> Option<PreviousModel> {
    let prev = lib
        .db
        .with(|c| store::previous_model(c, current))
        .ok()
        .flatten()?;
    match load_embedder(&prev, &cfg.siglip, &cfg.gemma) {
        Ok(e) => Some(PreviousModel {
            text_min: e.default_text_min(),
            similar_min: e.default_similar_min(),
            embedder: e,
        }),
        Err(e) => {
            tracing::info!(model = %prev, error = %e, "smart search: the previous model cannot be loaded, no catch-up serving");
            None
        }
    }
}

/// Starts smart search when `LUCINA_MEDIA_ML=1`: loads the model, the vectors, then embeds in
/// the background. A missing model or runtime leaves it off (logged), nothing else changes.
pub async fn worker(lib: Library, cfg: SmartConfig) {
    if !cfg.enabled {
        return;
    }
    let c2 = cfg.clone();
    let loaded = tokio::task::spawn_blocking(move || load_configured(&c2)).await;
    let embedder = match loaded {
        Ok(Ok(e)) => e,
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
    let c2 = cfg.clone();
    let smart = match tokio::task::spawn_blocking(move || {
        let previous = previous_for(&l2, &c2, embedder.model());
        let (text_min, similar_min) = c2.minimums_for(&*embedder);
        install_with(&l2, embedder, text_min, similar_min, previous)
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
        serving = %smart.serving_model(),
        vectors = smart.index.read().unwrap_or_else(|p| p.into_inner()).len(),
        "smart search on"
    );
    // Face recognition rides on this thread; it stays off until the owner turns it on.
    crate::faces::install_real(lib.clone()).await;
    // Photo categories: JEV decides when the service has a key (`JEV_API_KEY`, `LUCINA_JEV`).
    match lucina_media_ml::categories_jev::JevDecider::from_env() {
        Some(d) => {
            tracing::info!(decider = %d.id(), "categories: JEV decides");
            crate::categories::install_decider(&lib, Arc::new(d));
        }
        None => tracing::info!("categories: no JEV key, the rules decide"),
    }
    let (limit, max_load) = (cfg.temp_limit, cfg.max_load);
    if let Err(e) = std::thread::Builder::new()
        .name("smart-embed".into())
        .spawn(move || embed_loop(lib, smart, limit, max_load))
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
            "enabled": false, "model": null, "serving_model": null, "catching_up": false,
            "catch_up": null, "indexed": 0, "indexed_by_model": {}, "pending": 0, "failed": 0,
            "running": false, "paused_hot": false, "paused_busy": false,
        }),
        Some(s) => {
            let p = s.progress.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let previous = s.previous.read().unwrap_or_else(|p| p.into_inner()).clone();
            // `catch_up`: while the configured model is still embedding what the previous one
            // had, the previous one serves search.
            let catch_up = previous.as_ref().map(|prev| {
                let total = p.catch_total.max(p.catch_remaining).max(1);
                json!({
                    "from": prev.embedder.model(),
                    "to": s.embedder.model(),
                    "remaining": p.catch_remaining,
                    "total": total,
                    "progress": ((1.0 - p.catch_remaining as f64 / total as f64) * 1000.0).round() / 1000.0,
                    "serving_indexed": prev.index.len(),
                })
            });
            json!({
                "enabled": true,
                "model": s.embedder.model(),
                "serving_model": s.serving_model(),
                "catching_up": previous.is_some(),
                "catch_up": catch_up,
                "indexed": s.index.read().unwrap_or_else(|p| p.into_inner()).len(),
                "indexed_by_model": p.by_model,
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
    /// The model that embeds pictures and text for this catalog (the configured one; it serves
    /// search once it has caught up with the previous model).
    pub fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    /// The model whose index answers search and similar right now.
    pub fn serving_model(&self) -> String {
        match &*self.previous.read().unwrap_or_else(|p| p.into_inner()) {
            Some(p) => p.embedder.model().to_string(),
            None => self.embedder.model().to_string(),
        }
    }

    /// Whether a previous model still serves search while the configured one catches up.
    pub fn catching_up(&self) -> bool {
        self.previous
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Lets the models give back memory (see `Embedder::release_idle`).
    pub fn release_idle(&self, idle: Duration, vision: bool) {
        self.embedder.release_idle(idle, vision);
        let previous = self
            .previous
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(p) = previous {
            p.embedder.release_idle(idle, vision);
        }
    }

    /// A query's vector for `embedder`, from the cache when it was asked lately.
    fn query_vector(
        &self,
        embedder: &dyn Embedder,
        text: &str,
    ) -> Result<Vec<f32>, lucina_media_ml::MlError> {
        let key = (embedder.model().to_string(), embedder.normalize_query(text));
        {
            let mut cache = self.queries.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
                let hit = cache.remove(i);
                let v = hit.1.clone();
                cache.push(hit);
                return Ok(v);
            }
        }
        let v = embedder.embed_text(&key.1)?;
        let mut cache = self.queries.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= QUERY_CACHE {
            cache.remove(0);
        }
        cache.push((key, v.clone()));
        Ok(v)
    }

    /// The best matches for a query from the serving model's index, and that model's embedder
    /// (it maps the cosines to scores).
    fn search_hits(&self, text: &str) -> Result<(Vec<Hit>, Arc<dyn Embedder>), MlError> {
        let previous = self
            .previous
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match previous {
            Some(p) => {
                let v = self.query_vector(&*p.embedder, text)?;
                let hits = p.index.search(&v, MAX_CANDIDATES, p.text_min, None);
                Ok((hits, p.embedder.clone()))
            }
            None => {
                let v = self.query_vector(&*self.embedder, text)?;
                let ix = self.index.read().unwrap_or_else(|p| p.into_inner());
                Ok((
                    ix.search(&v, MAX_CANDIDATES, self.text_min, None),
                    self.embedder.clone(),
                ))
            }
        }
    }

    /// The photos that look like `id`, from the serving model's index; `None` when that index
    /// has no vector of `id`.
    fn similar_hits(&self, id: &str) -> Option<Vec<Hit>> {
        let previous = self
            .previous
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match previous {
            Some(p) => p
                .index
                .get(id)
                .map(|v| p.index.search(v, MAX_CANDIDATES, p.similar_min, Some(id))),
            None => {
                let ix = self.index.read().unwrap_or_else(|p| p.into_inner());
                ix.get(id)
                    .map(|v| ix.search(v, MAX_CANDIDATES, self.similar_min, Some(id)))
            }
        }
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
    let (hits, embedder) = tokio::task::spawn_blocking(move || s2.search_hits(&text))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            tracing::warn!(error = %e, "smart search: query failed");
            off()
        })?;
    page(&lib, hits, q, offset, limit, move |c| embedder.relevance(c)).await
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
    let hits = tokio::task::spawn_blocking(move || s2.similar_hits(&id))
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

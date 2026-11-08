//! Pets: finds the cats and dogs in the owner's photos, groups the same animal together, keeps the crops. The
//! networks (YOLOX-s and DINOv2-small, `lucina_media_ml::pets`), the clustering and the tables (`pets_store`) are in
//! `lucina-media-ml`; this module feeds them the catalog's renditions and runs the work. It mirrors `faces.rs`.
//!
//! - **On by default** (`pet_settings`, off from the Pets page): animals are not people, so there is no opt-in.
//! - **The scan** ([`step`]) runs in smart search's embedding thread after the face scan, with the same pauses (CPU
//!   at or above 90 °C, load above 16, nice 19, 3 ONNX threads). It looks only at photos categorised `pet` or `photo`
//!   (`ml_categories`), newest first, 4 at a time, from the large rendition (never an original). Crops found again
//!   (box overlap above 0.5) keep their id, pet and the owner's choice. New crops are clustered after each batch.
//! - **Crop pictures** are made when first asked for, 256 px square, under `<data_dir>/pets/<shard>/<crop_id>.jpg`.
//! - **Never public.** Behind the upstream key like the rest of `/v1/media`; share links know nothing about them.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use image::imageops::FilterType;
use lucina_media_ml::cluster::Graph;
use lucina_media_ml::pets::{self as ml, Found, PetModel};
use lucina_media_ml::pets_store::{self as fs, CropRow, NewCrop, PetAssignment};
use rusqlite::Connection;

use crate::config::Config;
use crate::db;
use crate::ingest::Library;

/// Photos per scan step.
pub const SCAN_BATCH: usize = 4;
/// Rows per write transaction.
const WRITE_CHUNK: usize = 200;
/// Side of a crop crop, in pixels, and the margin around the box (a box side times this).
pub const CROP_PX: u32 = 256;
const CROP_MARGIN: f32 = 1.5;

#[derive(Clone, Debug, Default)]
struct Progress {
    running: bool,
    /// Pets were added since the last full pass.
    dirty: bool,
}

/// Crop recognition for one catalog: the model, every crop vector in memory, the worker's state.
pub struct Pets {
    pub(crate) model: Arc<dyn PetModel>,
    pub(crate) graph: Mutex<Graph>,
    /// Held while a scan step or a delete runs, so they never overlap.
    work: Mutex<()>,
    progress: Mutex<Progress>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Pets>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<Pets>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn state(lib: &Library) -> Option<Arc<Pets>> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(lib.db.path())
        .cloned()
}

/// Loads `model`'s crops from the catalog and makes crop recognition available for `lib`. It
/// still scans nothing until the owner turns it on.
pub fn install(lib: &Library, model: Arc<dyn PetModel>) -> anyhow::Result<Arc<Pets>> {
    let (name, dim) = (model.model().to_string(), model.dim());
    let graph = lib.db.with(|c| fs::load_graph(c, &name, dim))?;
    let crops = Arc::new(Pets {
        model,
        graph: Mutex::new(graph),
        work: Mutex::new(()),
        progress: Mutex::new(Progress::default()),
    });
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(lib.db.path().to_path_buf(), crops.clone());
    Ok(crops)
}

/// Loads YuNet and SCrop and installs them; a missing model or runtime leaves crop recognition
/// unavailable (logged), nothing else changes.
pub async fn install_real(lib: Library) {
    let cfg = ml::PetsConfig::from_env();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let model = ml::YoloxDino::load(cfg)?;
        install(&lib, Arc::new(model))?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            tracing::info!("pet grouping available")
        }
        Ok(Err(e)) => tracing::warn!(error = %e, "pet grouping is unavailable"),
        Err(e) => tracing::warn!(error = %e, "pet grouping is unavailable"),
    }
}

/// The tables (one line in `db::migrate`).
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    fs::migrate(c)
}

// ---- crops

pub fn crop_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("pets")
}

/// `<data_dir>/crops/<shard>/<crop_id>.jpg`, the shard being the id's first two characters after
/// the prefix.
pub fn crop_path(cfg: &Config, crop_id: &str) -> PathBuf {
    let tail = crop_id.rsplit('_').next().unwrap_or(crop_id);
    crop_dir(cfg)
        .join(&tail[..2.min(tail.len())])
        .join(format!("{crop_id}.jpg"))
}

fn remove_crop(cfg: &Config, crop_id: &str) {
    let _ = std::fs::remove_file(crop_path(cfg, crop_id));
}

/// The rendition crops are read from: the large JPEG, else the grid one.
fn rendition(thumbs_dir: &FsPath) -> Option<PathBuf> {
    ["l.jpg", "s.jpg"]
        .iter()
        .map(|n| thumbs_dir.join(n))
        .find(|p| p.is_file())
}

/// The square, in pixels of a `w`×`h` picture, that a crop box (fractions) is cropped to: the
/// box's longer side times [`CROP_MARGIN`], centred on the box and moved inside the picture.
pub fn crop_rect(crop: &CropRow, w: u32, h: u32) -> (u32, u32, u32) {
    let (wf, hf) = (w as f32, h as f32);
    let (bw, bh) = (crop.w as f32 * wf, crop.h as f32 * hf);
    let (cx, cy) = (
        (crop.x as f32 + crop.w as f32 / 2.0) * wf,
        (crop.y as f32 + crop.h as f32 / 2.0) * hf,
    );
    let side = (bw.max(bh) * CROP_MARGIN).clamp(8.0, wf.min(hf)).round();
    let x0 = (cx - side / 2.0).clamp(0.0, wf - side).round();
    let y0 = (cy - side / 2.0).clamp(0.0, hf - side).round();
    (x0 as u32, y0 as u32, side as u32)
}

/// The crop's crop file, made from the item's rendition if it is not there yet.
pub fn ensure_crop(cfg: &Config, crop: &CropRow) -> anyhow::Result<PathBuf> {
    let path = crop_path(cfg, &crop.id);
    if path.is_file() {
        return Ok(path);
    }
    let dir = crate::thumbs::dir_for(cfg, &crop.item_id);
    let src = rendition(&dir).ok_or_else(|| anyhow::anyhow!("the item has no rendition"))?;
    let img = image::open(&src)?.to_rgb8();
    let (x, y, side) = crop_rect(crop, img.width(), img.height());
    let crop = image::imageops::crop_imm(&img, x, y, side, side).to_image();
    let out = image::imageops::resize(&crop, CROP_PX, CROP_PX, FilterType::Lanczos3);
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&out)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jpg.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

// ---- the scan

/// Photos with a rendition and no scan newer than their last change, newest first.
const PENDING_WHERE: &str = "i.kind = 'photo' AND i.thumb_state = 'ready' \
     AND i.trashed_at IS NULL AND i.hidden = 0 \
     AND NOT EXISTS (SELECT 1 FROM pet_scans s WHERE s.item_id = i.id AND s.model = ?1 \
                     AND s.scanned_at >= i.updated_at) \
     AND EXISTS (SELECT 1 FROM ml_categories k WHERE k.item_id = i.id AND k.category IN ('pet', 'photo'))";

fn pending_batch(c: &Connection, model: &str, n: usize) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(&format!(
        "SELECT i.id FROM items i WHERE {PENDING_WHERE} ORDER BY i.sort_at DESC, i.id DESC LIMIT ?2"
    ))?;
    let rows = st.query_map(rusqlite::params![model, n as i64], |r| r.get(0))?;
    rows.collect()
}

pub fn pending_count(c: &Connection, model: &str) -> rusqlite::Result<i64> {
    c.query_row(
        &format!("SELECT COUNT(*) FROM items i WHERE {PENDING_WHERE}"),
        [model],
        |r| r.get(0),
    )
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Reloads the graph when the catalog has more or fewer crops than it knows (a purge removed
/// some behind its back).
fn sync_graph(lib: &Library, f: &Pets) -> anyhow::Result<()> {
    let (name, dim) = (f.model.model().to_string(), f.model.dim());
    let rows = lib.db.with(|c| fs::count_model_crops(c, &name, dim))?;
    if rows as usize != lock(&f.graph).len() {
        let g = lib.db.with(|c| fs::load_graph(c, &name, dim))?;
        *lock(&f.graph) = g;
    }
    Ok(())
}

/// Stores clustering's assignments, a few rows per transaction. When the catalog cannot take
/// them the graph is reloaded so it matches what is stored.
fn store_assignments(
    lib: &Library,
    f: &Pets,
    assigned: Vec<lucina_media_ml::cluster::Assignment>,
) -> anyhow::Result<usize> {
    let n = assigned.len();
    let list: Vec<PetAssignment> = assigned
        .into_iter()
        .map(|a| PetAssignment {
            crop_id: a.face_id,
            pet_id: a.person_id,
            created: a.created,
        })
        .collect();
    let now = db::now_str();
    for chunk in list.chunks(WRITE_CHUNK) {
        let res = lib.db.with(|c| {
            let tx = c.transaction()?;
            fs::apply_assignments(&tx, chunk, &now)?;
            tx.commit()
        });
        if let Err(e) = res {
            let (name, dim) = (f.model.model().to_string(), f.model.dim());
            if let Ok(g) = lib.db.with(|c| fs::load_graph(c, &name, dim)) {
                *lock(&f.graph) = g;
            }
            return Err(e.into());
        }
    }
    Ok(n)
}

fn cluster_from(lib: &Library, f: &Pets, seeds: &[String]) -> anyhow::Result<usize> {
    if seeds.is_empty() {
        return Ok(0);
    }
    let assigned = lock(&f.graph).cluster(seeds, &mut fs::new_pet_id);
    store_assignments(lib, f, assigned)
}

/// Looks at the next pending photos and clusters their crops. How many photos were handled
/// (0 = nothing to do, or crop recognition is off).
pub fn scan_step(lib: &Library, f: &Pets) -> anyhow::Result<usize> {
    let _work = lock(&f.work);
    if !lib.db.with(|c| fs::enabled(c))? {
        return Ok(0);
    }
    sync_graph(lib, f)?;
    let model = f.model.model().to_string();
    let batch = lib.db.with(|c| pending_batch(c, &model, SCAN_BATCH))?;
    if batch.is_empty() {
        return Ok(0);
    }
    let mut seeds: Vec<String> = Vec::new();
    for id in &batch {
        let dir = crate::thumbs::dir_for(&lib.cfg, id);
        let looked: Result<Vec<Found>, String> = match rendition(&dir) {
            None => Err("no rendition".into()),
            Some(p) => match image::open(&p) {
                Err(e) => Err(format!("unreadable rendition: {e}")),
                Ok(img) => match ml::find_pets(&*f.model, &img.to_rgb8()) {
                    Ok(found) => Ok(found),
                    // The runtime or a network is gone: try again later, record nothing.
                    Err(lucina_media_ml::MlError::Unavailable(e)) => {
                        anyhow::bail!("crop models unavailable: {e}")
                    }
                    Err(e) => Err(e.to_string()),
                },
            },
        };
        let now = db::now_str();
        match looked {
            Err(e) => {
                lib.db
                    .with(|c| fs::put_scan_failure(c, id, &model, &e, &now))?;
            }
            Ok(found) => {
                let new: Vec<NewCrop> = found
                    .iter()
                    .map(|x| NewCrop {
                        x: x.det.x,
                        y: x.det.y,
                        w: x.det.w,
                        h: x.det.h,
                        score: x.det.score,
                        vec: x.vec.clone(),
                    })
                    .collect();
                let rep = lib.db.with(|c| {
                    let tx = c.transaction()?;
                    let r = fs::replace_item(&tx, id, &model, &new, &now)?;
                    tx.commit()?;
                    Ok(r)
                })?;
                let mut g = lock(&f.graph);
                for gone in &rep.removed {
                    g.remove(gone);
                    remove_crop(&lib.cfg, gone);
                }
                for (fid, n) in &rep.crops {
                    let v = &found[*n].vec;
                    if rep.added.contains(fid) {
                        if g.insert(fid, v, None, false) {
                            seeds.push(fid.clone());
                        }
                    } else {
                        g.update_vector(fid, v);
                        // Its box may have moved: the crop is made again when asked for.
                        remove_crop(&lib.cfg, fid);
                        if g.person_of(fid).is_none() && !g.is_manual(fid) {
                            seeds.push(fid.clone());
                        }
                    }
                }
            }
        }
    }
    cluster_from(lib, f, &seeds)?;
    lock(&f.progress).dirty = true;
    Ok(batch.len())
}

/// One more pass over every crop still without a pet (a crop may have become claimable).
pub fn recluster(lib: &Library, f: &Pets) -> anyhow::Result<usize> {
    let _work = lock(&f.work);
    sync_graph(lib, f)?;
    let seeds = lock(&f.graph).unassigned();
    cluster_from(lib, f, &seeds)
}

/// The worker's step, called from smart search's thread when it has nothing to embed: scan a
/// batch; when nothing is left, one full pass if anything was scanned since the last. How many
/// photos were scanned.
pub fn step(lib: &Library) -> anyhow::Result<usize> {
    let Some(f) = state(lib) else {
        return Ok(0);
    };
    let n = scan_step(lib, &f)?;
    let mut p = lock(&f.progress);
    p.running = n > 0;
    if n == 0 && p.dirty {
        p.dirty = false;
        drop(p);
        recluster(lib, &f)?;
    }
    Ok(n)
}

pub fn running(lib: &Library) -> bool {
    state(lib).is_some_and(|f| lock(&f.progress).running)
}

/// Removes every crop, pet, scan record and crop, and turns crop recognition off.
pub fn delete_everything(lib: &Library) -> anyhow::Result<()> {
    let st = state(lib);
    let _work = st.as_ref().map(|s| lock(&s.work));
    lib.db.with(|c| fs::set_enabled(c, false))?;
    // A few rows at a time, so other requests get the catalog in between.
    while lib.db.with(|c| fs::delete_some(c))? > 0 {}
    if let Some(s) = &st {
        *lock(&s.graph) = Graph::with_rule(s.model.dim(), ml::MATCH, ml::MIN_CROPS);
        let mut p = lock(&s.progress);
        p.running = false;
        p.dirty = false;
    }
    match std::fs::remove_dir_all(crop_dir(&lib.cfg)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Hourly: drops crops of items that are gone (a purge), forgets them in the graph, and removes
/// crop files whose crop no longer exists. Returns how many crops went.
pub fn prune(lib: &Library) -> anyhow::Result<usize> {
    let st = state(lib);
    let _work = st.as_ref().map(|s| lock(&s.work));
    let removed = lib.db.with(|c| {
        let tx = c.transaction()?;
        let r = fs::prune(&tx, "SELECT id FROM items")?;
        tx.commit()?;
        Ok(r)
    })?;
    if let Some(s) = &st {
        let mut g = lock(&s.graph);
        for id in &removed {
            g.remove(id);
        }
    }
    for id in &removed {
        remove_crop(&lib.cfg, id);
    }
    // Pets nothing refers to.
    let root = crop_dir(&lib.cfg);
    if let Ok(shards) = std::fs::read_dir(&root) {
        for shard in shards.flatten() {
            let Ok(files) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().to_string();
                let Some(id) = name.strip_suffix(".jpg") else {
                    continue;
                };
                let known = lib
                    .db
                    .with(|c| Ok(fs::get_crop(c, id)?.is_some()))
                    .unwrap_or(true);
                if !known {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
    Ok(removed.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(x: f64, y: f64, w: f64, h: f64) -> CropRow {
        CropRow {
            id: "pc_ab".into(),
            item_id: "it_1".into(),
            x,
            y,
            w,
            h,
            score: 0.9,
            pet_id: None,
            source: "ml".into(),
        }
    }

    #[test]
    fn the_crop_is_a_square_with_margin_inside_the_picture() {
        // A 100×100 px crop in the middle of 1000×800: a 150 px square around it.
        assert_eq!(
            crop_rect(&crop(0.45, 0.4375, 0.1, 0.125), 1000, 800),
            (425, 325, 150)
        );
        // At the corner it is moved inside, not cut.
        assert_eq!(
            crop_rect(&crop(0.0, 0.0, 0.1, 0.125), 1000, 800),
            (0, 0, 150)
        );
        let (x, y, s) = crop_rect(&crop(0.95, 0.9, 0.05, 0.1), 1000, 800);
        assert!(x + s <= 1000 && y + s <= 800 && s >= 80, "{x} {y} {s}");
        // A crop bigger than the picture's short side is limited to it.
        let (_, _, s) = crop_rect(&crop(0.0, 0.0, 1.0, 1.0), 1000, 800);
        assert_eq!(s, 800);
    }

    #[test]
    fn crop_paths_are_sharded_under_the_crops_folder() {
        let cfg = Config::for_dirs("/data".into(), "/store".into());
        assert_eq!(
            crop_path(&cfg, "pc_ab12cd"),
            PathBuf::from("/data/crops/ab/fc_ab12cd.jpg")
        );
    }
}

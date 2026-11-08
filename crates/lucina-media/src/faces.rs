//! Faces and people: finds the faces in the owner's photos, groups them into people, keeps the
//! crops (`docs/gallery-vs-immich.md`, gap 2). The networks, the clustering and the tables are in
//! `lucina-media-ml`; this module feeds them the catalog's renditions and runs the work.
//!
//! - **Opt-in.** Nothing is scanned until the owner turns it on (`face_settings`, flipped from the
//!   People page). Turning it off stops scanning and keeps what was found; "Delete face data"
//!   ([`delete_everything`]) removes every face, person, scan record and crop.
//! - **The scan** ([`step`]) runs inside smart search's embedding thread (`smart.rs`), only when
//!   that has nothing pending, with the same pauses (CPU at or above 90 °C, load above 16, nice 19,
//!   3 ONNX threads). It looks at photos newest first, 4 at a time, from the large rendition
//!   (`l.jpg`, else `s.jpg`; never an original), and writes a few rows per transaction. A photo
//!   changed after it was scanned is scanned again; faces found again (box overlap above 0.5) keep
//!   their id, person and the owner's choice. After each batch the new faces are clustered; once
//!   nothing is pending, one more pass retries every face still without a person.
//! - **Crops** are made when first asked for, 256 px square with a margin, under
//!   `<data_dir>/faces/<shard>/<face_id>.jpg`, and removed with their face.
//! - **Never public.** Faces and people appear only behind the upstream key; the share routes
//!   (`share_http.rs`) know nothing about them.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use image::imageops::FilterType;
use lucina_media_ml::cluster::Graph;
use lucina_media_ml::faces::{self as ml, FaceModel, Found};
use lucina_media_ml::faces_store::{self as fs, FaceRow, NewFace, PersonAssignment};
use rusqlite::Connection;

use crate::config::Config;
use crate::db;
use crate::ingest::Library;

/// Photos per scan step.
pub const SCAN_BATCH: usize = 4;
/// Rows per write transaction.
const WRITE_CHUNK: usize = 200;
/// Side of a face crop, in pixels, and the margin around the box (a box side times this).
pub const CROP_PX: u32 = 256;
const CROP_MARGIN: f32 = 1.5;

#[derive(Clone, Debug, Default)]
struct Progress {
    running: bool,
    /// Faces were added since the last full pass.
    dirty: bool,
}

/// Face recognition for one catalog: the model, every face vector in memory, the worker's state.
pub struct Faces {
    pub(crate) model: Arc<dyn FaceModel>,
    pub(crate) graph: Mutex<Graph>,
    /// Held while a scan step or a delete runs, so they never overlap.
    work: Mutex<()>,
    progress: Mutex<Progress>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Faces>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<Faces>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn state(lib: &Library) -> Option<Arc<Faces>> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(lib.db.path())
        .cloned()
}

/// Loads `model`'s faces from the catalog and makes face recognition available for `lib`. It
/// still scans nothing until the owner turns it on.
pub fn install(lib: &Library, model: Arc<dyn FaceModel>) -> anyhow::Result<Arc<Faces>> {
    let (name, dim) = (model.model().to_string(), model.dim());
    let graph = lib.db.with(|c| fs::load_graph(c, &name, dim))?;
    let faces = Arc::new(Faces {
        model,
        graph: Mutex::new(graph),
        work: Mutex::new(()),
        progress: Mutex::new(Progress::default()),
    });
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(lib.db.path().to_path_buf(), faces.clone());
    Ok(faces)
}

/// Loads YuNet and SFace and installs them; a missing model or runtime leaves face recognition
/// unavailable (logged), nothing else changes.
pub async fn install_real(lib: Library) {
    let cfg = ml::FacesConfig::from_env();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let model = ml::YuNetSface::load(cfg)?;
        install(&lib, Arc::new(model))?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            tracing::info!("face recognition available (off until the owner turns it on)")
        }
        Ok(Err(e)) => tracing::warn!(error = %e, "face recognition is unavailable"),
        Err(e) => tracing::warn!(error = %e, "face recognition is unavailable"),
    }
}

/// The tables (one line in `db::migrate`).
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    fs::migrate(c)
}

// ---- crops

pub fn crop_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("faces")
}

/// `<data_dir>/faces/<shard>/<face_id>.jpg`, the shard being the id's first two characters after
/// the prefix.
pub fn crop_path(cfg: &Config, face_id: &str) -> PathBuf {
    let tail = face_id.rsplit('_').next().unwrap_or(face_id);
    crop_dir(cfg)
        .join(&tail[..2.min(tail.len())])
        .join(format!("{face_id}.jpg"))
}

fn remove_crop(cfg: &Config, face_id: &str) {
    let _ = std::fs::remove_file(crop_path(cfg, face_id));
}

/// The rendition faces are read from: the large JPEG, else the grid one.
fn rendition(thumbs_dir: &FsPath) -> Option<PathBuf> {
    ["l.jpg", "s.jpg"]
        .iter()
        .map(|n| thumbs_dir.join(n))
        .find(|p| p.is_file())
}

/// The square, in pixels of a `w`×`h` picture, that a face box (fractions) is cropped to: the
/// box's longer side times [`CROP_MARGIN`], centred on the box and moved inside the picture.
pub fn crop_rect(face: &FaceRow, w: u32, h: u32) -> (u32, u32, u32) {
    let (wf, hf) = (w as f32, h as f32);
    let (bw, bh) = (face.w as f32 * wf, face.h as f32 * hf);
    let (cx, cy) = (
        (face.x as f32 + face.w as f32 / 2.0) * wf,
        (face.y as f32 + face.h as f32 / 2.0) * hf,
    );
    let side = (bw.max(bh) * CROP_MARGIN).clamp(8.0, wf.min(hf)).round();
    let x0 = (cx - side / 2.0).clamp(0.0, wf - side).round();
    let y0 = (cy - side / 2.0).clamp(0.0, hf - side).round();
    (x0 as u32, y0 as u32, side as u32)
}

/// The face's crop file, made from the item's rendition if it is not there yet.
pub fn ensure_crop(cfg: &Config, face: &FaceRow) -> anyhow::Result<PathBuf> {
    let path = crop_path(cfg, &face.id);
    if path.is_file() {
        return Ok(path);
    }
    let dir = crate::thumbs::dir_for(cfg, &face.item_id);
    let src = rendition(&dir).ok_or_else(|| anyhow::anyhow!("the item has no rendition"))?;
    let img = image::open(&src)?.to_rgb8();
    let (x, y, side) = crop_rect(face, img.width(), img.height());
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
     AND NOT EXISTS (SELECT 1 FROM face_scans s WHERE s.item_id = i.id AND s.model = ?1 \
                     AND s.scanned_at >= i.updated_at)";

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

/// Reloads the graph when the catalog has more or fewer faces than it knows (a purge removed
/// some behind its back).
fn sync_graph(lib: &Library, f: &Faces) -> anyhow::Result<()> {
    let (name, dim) = (f.model.model().to_string(), f.model.dim());
    let rows = lib.db.with(|c| fs::count_model_faces(c, &name, dim))?;
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
    f: &Faces,
    assigned: Vec<lucina_media_ml::cluster::Assignment>,
) -> anyhow::Result<usize> {
    let n = assigned.len();
    let list: Vec<PersonAssignment> = assigned
        .into_iter()
        .map(|a| PersonAssignment {
            face_id: a.face_id,
            person_id: a.person_id,
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

fn cluster_from(lib: &Library, f: &Faces, seeds: &[String]) -> anyhow::Result<usize> {
    if seeds.is_empty() {
        return Ok(0);
    }
    let assigned = lock(&f.graph).cluster(seeds, &mut fs::new_person_id);
    store_assignments(lib, f, assigned)
}

/// Looks at the next pending photos and clusters their faces. How many photos were handled
/// (0 = nothing to do, or face recognition is off).
pub fn scan_step(lib: &Library, f: &Faces) -> anyhow::Result<usize> {
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
                Ok(img) => match ml::find_faces(&*f.model, &img.to_rgb8()) {
                    Ok(found) => Ok(found),
                    // The runtime or a network is gone: try again later, record nothing.
                    Err(lucina_media_ml::MlError::Unavailable(e)) => {
                        anyhow::bail!("face models unavailable: {e}")
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
                let new: Vec<NewFace> = found
                    .iter()
                    .map(|x| NewFace {
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
                for (fid, n) in &rep.faces {
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

/// One more pass over every face still without a person (a face may have become claimable).
pub fn recluster(lib: &Library, f: &Faces) -> anyhow::Result<usize> {
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

/// Removes every face, person, scan record and crop, and turns face recognition off.
pub fn delete_everything(lib: &Library) -> anyhow::Result<()> {
    let st = state(lib);
    let _work = st.as_ref().map(|s| lock(&s.work));
    lib.db.with(|c| fs::set_enabled(c, false))?;
    // A few rows at a time, so other requests get the catalog in between.
    while lib.db.with(|c| fs::delete_some(c))? > 0 {}
    if let Some(s) = &st {
        *lock(&s.graph) = Graph::new(s.model.dim());
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

/// Hourly: drops faces of items that are gone (a purge), forgets them in the graph, and removes
/// crop files whose face no longer exists. Returns how many faces went.
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
    // Crops nothing refers to.
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
                    .with(|c| Ok(fs::get_face(c, id)?.is_some()))
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

    fn face(x: f64, y: f64, w: f64, h: f64) -> FaceRow {
        FaceRow {
            id: "fc_ab".into(),
            item_id: "it_1".into(),
            x,
            y,
            w,
            h,
            score: 0.9,
            person_id: None,
            source: "ml".into(),
        }
    }

    #[test]
    fn the_crop_is_a_square_with_margin_inside_the_picture() {
        // A 100×100 px face in the middle of 1000×800: a 150 px square around it.
        assert_eq!(
            crop_rect(&face(0.45, 0.4375, 0.1, 0.125), 1000, 800),
            (425, 325, 150)
        );
        // At the corner it is moved inside, not cut.
        assert_eq!(
            crop_rect(&face(0.0, 0.0, 0.1, 0.125), 1000, 800),
            (0, 0, 150)
        );
        let (x, y, s) = crop_rect(&face(0.95, 0.9, 0.05, 0.1), 1000, 800);
        assert!(x + s <= 1000 && y + s <= 800 && s >= 80, "{x} {y} {s}");
        // A face bigger than the picture's short side is limited to it.
        let (_, _, s) = crop_rect(&face(0.0, 0.0, 1.0, 1.0), 1000, 800);
        assert_eq!(s, 800);
    }

    #[test]
    fn crop_paths_are_sharded_under_the_faces_folder() {
        let cfg = Config::for_dirs("/data".into(), "/store".into());
        assert_eq!(
            crop_path(&cfg, "fc_ab12cd"),
            PathBuf::from("/data/faces/ab/fc_ab12cd.jpg")
        );
    }
}

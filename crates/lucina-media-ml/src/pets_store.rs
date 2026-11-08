//! Pets in the catalog, mirroring `faces_store` (people) table for table.
//!
//! - `pet_settings`: `enabled` is the owner's switch (absent = on).
//! - `pet_crops`: one row per animal found: the item, the box (fractions of the picture), YOLOX's score, the
//!   DINOv2 vector (f16), the pet it belongs to (or none) and whose call that was (`ml` or `manual`).
//! - `pets`: a name, a hidden flag, a birth date and the crop shown as the cover. Unnamed pets with no crops left
//!   are dropped, named ones are kept.
//! - `pet_scans`: one row per (item, model): when the picture was looked at, how many animals, or why not.
//!
//! No foreign key to `items`; the caller drops rows of purged items ([`prune`], [`forget`]).

use rusqlite::{params, Connection, OptionalExtension};

use crate::cluster::Graph;
use crate::vector::{from_f16_bytes, to_f16_bytes};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pet_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS pets (
    id            TEXT PRIMARY KEY,
    name          TEXT,
    hidden        INTEGER NOT NULL DEFAULT 0,
    birth_date    TEXT,
    cover_crop_id TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pet_crops (
    id         TEXT PRIMARY KEY,
    item_id    TEXT NOT NULL,
    model      TEXT NOT NULL,
    x          REAL NOT NULL,
    y          REAL NOT NULL,
    w          REAL NOT NULL,
    h          REAL NOT NULL,
    score      REAL NOT NULL,
    dim        INTEGER NOT NULL,
    vec        BLOB NOT NULL,
    pet_id  TEXT,
    source     TEXT NOT NULL DEFAULT 'ml' CHECK (source IN ('ml', 'manual')),
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS pet_crops_item ON pet_crops (item_id);
CREATE INDEX IF NOT EXISTS pet_crops_pet ON pet_crops (pet_id);
CREATE TABLE IF NOT EXISTS pet_scans (
    item_id    TEXT NOT NULL,
    model      TEXT NOT NULL,
    scanned_at TEXT NOT NULL,
    crops      INTEGER NOT NULL,
    error      TEXT,
    PRIMARY KEY (item_id, model)
);
";

/// Idempotent: safe on every start.
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(SCHEMA)
}

pub fn new_crop_id() -> String {
    format!("pc_{}", uuid::Uuid::new_v4().simple())
}

pub fn new_pet_id() -> String {
    format!("pt_{}", uuid::Uuid::new_v4().simple())
}

// ---- settings

/// Pet grouping is on unless the owner turns it off (it knows animals, not people).
pub fn enabled(c: &Connection) -> rusqlite::Result<bool> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM pet_settings WHERE key = 'enabled'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.as_deref() != Some("0"))
}

pub fn set_enabled(c: &Connection, on: bool) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO pet_settings (key, value) VALUES ('enabled', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [if on { "1" } else { "0" }],
    )?;
    Ok(())
}

// ---- rows

#[derive(Clone, Debug, PartialEq)]
pub struct CropRow {
    pub id: String,
    pub item_id: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub score: f64,
    pub pet_id: Option<String>,
    /// `ml` or `manual`.
    pub source: String,
}

const CROP_COLS: &str = "id, item_id, x, y, w, h, score, pet_id, source";

fn crop_from_row(r: &rusqlite::Row) -> rusqlite::Result<CropRow> {
    Ok(CropRow {
        id: r.get(0)?,
        item_id: r.get(1)?,
        x: r.get(2)?,
        y: r.get(3)?,
        w: r.get(4)?,
        h: r.get(5)?,
        score: r.get(6)?,
        pet_id: r.get(7)?,
        source: r.get(8)?,
    })
}

pub fn get_crop(c: &Connection, id: &str) -> rusqlite::Result<Option<CropRow>> {
    c.query_row(
        &format!("SELECT {CROP_COLS} FROM pet_crops WHERE id = ?1"),
        [id],
        crop_from_row,
    )
    .optional()
}

/// An item's crops, left to right.
pub fn crops_of_item(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<CropRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {CROP_COLS} FROM pet_crops WHERE item_id = ?1 ORDER BY x, id"
    ))?;
    let rows = st.query_map([item_id], crop_from_row)?;
    rows.collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct PetRow {
    pub id: String,
    pub name: Option<String>,
    pub hidden: bool,
    pub birth_date: Option<String>,
    pub cover_crop_id: Option<String>,
}

pub fn get_pet(c: &Connection, id: &str) -> rusqlite::Result<Option<PetRow>> {
    c.query_row(
        "SELECT id, name, hidden, birth_date, cover_crop_id FROM pets WHERE id = ?1",
        [id],
        |r| {
            Ok(PetRow {
                id: r.get(0)?,
                name: r.get(1)?,
                hidden: r.get::<_, i64>(2)? != 0,
                birth_date: r.get(3)?,
                cover_crop_id: r.get(4)?,
            })
        },
    )
    .optional()
}

// ---- a scan of one picture

/// A crop found in a picture, ready to store.
#[derive(Clone, Debug)]
pub struct NewCrop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub vec: Vec<f32>,
}

/// What [`replace_item`] did.
#[derive(Clone, Debug, Default)]
pub struct Replaced {
    /// `(crop id, index into the new crops)` for every crop of the picture now: new ones and
    /// old ones that kept their id.
    pub crops: Vec<(String, usize)>,
    /// The ids among `crops` that are new.
    pub added: Vec<String>,
    /// Old crops that are gone.
    pub removed: Vec<String>,
}

fn iou(a: [f64; 4], b: [f64; 4]) -> f64 {
    let iw = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let ih = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// For each new box (x, y, w, h) the old box it continues, if any: pairs are taken best
/// overlap first, each old box once, and only above `min_iou`.
pub fn match_boxes(old: &[[f64; 4]], new: &[[f64; 4]], min_iou: f64) -> Vec<Option<usize>> {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (n, nb) in new.iter().enumerate() {
        for (o, ob) in old.iter().enumerate() {
            let v = iou(*nb, *ob);
            if v > min_iou {
                pairs.push((v, n, o));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut out = vec![None; new.len()];
    let mut used = vec![false; old.len()];
    for (_, n, o) in pairs {
        if out[n].is_none() && !used[o] {
            out[n] = Some(o);
            used[o] = true;
        }
    }
    out
}

/// IoU above which a crop found again is the same crop (and keeps its id, pet and source).
pub const SAME_CROP_IOU: f64 = 0.5;

/// Stores the result of looking at an item: a new crop that overlaps an old one by more than
/// [`SAME_CROP_IOU`] updates it in place (so its pet and the owner's choice survive); other new
/// crops are added; old `ml` crops nothing matches are removed. An old crop the owner set by hand
/// is kept even when nothing matches it. Records the scan.
pub fn replace_item(
    c: &Connection,
    item_id: &str,
    model: &str,
    found: &[NewCrop],
    now: &str,
) -> rusqlite::Result<Replaced> {
    let old = crops_of_item(c, item_id)?;
    let old_boxes: Vec<[f64; 4]> = old.iter().map(|f| [f.x, f.y, f.w, f.h]).collect();
    let new_boxes: Vec<[f64; 4]> = found
        .iter()
        .map(|f| [f.x as f64, f.y as f64, f.w as f64, f.h as f64])
        .collect();
    let matches = match_boxes(&old_boxes, &new_boxes, SAME_CROP_IOU);
    let mut res = Replaced::default();
    let mut kept = vec![false; old.len()];
    for (n, f) in found.iter().enumerate() {
        let blob = to_f16_bytes(&f.vec);
        match matches[n] {
            Some(o) => {
                kept[o] = true;
                c.execute(
                    "UPDATE pet_crops SET model = ?2, x = ?3, y = ?4, w = ?5, h = ?6, score = ?7, dim = ?8, vec = ?9 \
                     WHERE id = ?1",
                    params![old[o].id, model, f.x, f.y, f.w, f.h, f.score, f.vec.len() as i64, blob],
                )?;
                res.crops.push((old[o].id.clone(), n));
            }
            None => {
                let id = new_crop_id();
                c.execute(
                    "INSERT INTO pet_crops (id, item_id, model, x, y, w, h, score, dim, vec, pet_id, source, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, 'ml', ?11)",
                    params![id, item_id, model, f.x, f.y, f.w, f.h, f.score, f.vec.len() as i64, blob, now],
                )?;
                res.added.push(id.clone());
                res.crops.push((id, n));
            }
        }
    }
    let mut affected = Vec::new();
    for (o, crop) in old.iter().enumerate() {
        if kept[o] || crop.source == "manual" {
            continue;
        }
        c.execute("DELETE FROM pet_crops WHERE id = ?1", [&crop.id])?;
        res.removed.push(crop.id.clone());
        affected.extend(crop.pet_id.clone());
    }
    cleanup_pets(c, &affected)?;
    let total: i64 = c.query_row(
        "SELECT COUNT(*) FROM pet_crops WHERE item_id = ?1",
        [item_id],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO pet_scans (item_id, model, scanned_at, crops, error) VALUES (?1, ?2, ?3, ?4, NULL) \
         ON CONFLICT(item_id, model) DO UPDATE SET scanned_at = excluded.scanned_at, \
         crops = excluded.crops, error = NULL",
        params![item_id, model, now, total],
    )?;
    Ok(res)
}

/// Records that an item's picture could not be read, so it is not retried until it changes.
pub fn put_scan_failure(
    c: &Connection,
    item_id: &str,
    model: &str,
    error: &str,
    now: &str,
) -> rusqlite::Result<()> {
    let error: String = error.chars().take(300).collect();
    c.execute(
        "INSERT INTO pet_scans (item_id, model, scanned_at, crops, error) VALUES (?1, ?2, ?3, 0, ?4) \
         ON CONFLICT(item_id, model) DO UPDATE SET scanned_at = excluded.scanned_at, \
         crops = 0, error = excluded.error",
        params![item_id, model, now, error],
    )?;
    Ok(())
}

// ---- pets

/// What clustering decided for one crop.
#[derive(Clone, Debug, PartialEq)]
pub struct PetAssignment {
    pub crop_id: String,
    pub pet_id: String,
    pub created: bool,
}

/// Stores clustering's assignments: creates the pets it made and sets the crops' pet. A crop
/// the owner set by hand, or a pet that no longer exists, is left alone.
pub fn apply_assignments(
    c: &Connection,
    list: &[PetAssignment],
    now: &str,
) -> rusqlite::Result<()> {
    let mut touched: Vec<&str> = Vec::new();
    for a in list {
        if a.created {
            c.execute(
                "INSERT OR IGNORE INTO pets (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
                params![a.pet_id, now],
            )?;
        }
        c.execute(
            "UPDATE pet_crops SET pet_id = ?2 WHERE id = ?1 AND source = 'ml' \
             AND EXISTS (SELECT 1 FROM pets WHERE id = ?2)",
            params![a.crop_id, a.pet_id],
        )?;
        if !touched.contains(&a.pet_id.as_str()) {
            touched.push(&a.pet_id);
        }
    }
    for p in touched {
        ensure_cover(c, p)?;
    }
    Ok(())
}

/// Gives the pet a cover when it has none or its cover is no longer one of its crops: the
/// crop with the biggest, surest box.
pub fn ensure_cover(c: &Connection, pet_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE pets SET cover_crop_id = (SELECT id FROM pet_crops WHERE pet_id = pets.id \
             ORDER BY score * w * h DESC, id LIMIT 1) \
         WHERE id = ?1 AND (cover_crop_id IS NULL \
             OR cover_crop_id NOT IN (SELECT id FROM pet_crops WHERE pet_id = ?1))",
        [pet_id],
    )?;
    Ok(())
}

/// After crops left these pets: drops the unnamed ones with no crop, fixes the others' covers.
pub fn cleanup_pets(c: &Connection, ids: &[String]) -> rusqlite::Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for id in ids {
        if seen.contains(&id.as_str()) {
            continue;
        }
        seen.push(id);
        c.execute(
            "DELETE FROM pets WHERE id = ?1 AND name IS NULL \
             AND NOT EXISTS (SELECT 1 FROM pet_crops WHERE pet_id = ?1)",
            [id],
        )?;
        ensure_cover(c, id)?;
        // A named pet with no crop keeps no cover.
        c.execute(
            "UPDATE pets SET cover_crop_id = NULL WHERE id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM pet_crops WHERE pet_id = ?1)",
            [id],
        )?;
    }
    Ok(())
}

/// The owner's call on one crop: to `pet` (which must exist), or to nobody. Marks it `manual`
/// so clustering leaves it alone. Fixes the covers and drops pets left empty. `false` when the
/// crop or the pet is unknown.
pub fn assign_crop(c: &Connection, crop_id: &str, pet: Option<&str>) -> rusqlite::Result<bool> {
    let Some(crop) = get_crop(c, crop_id)? else {
        return Ok(false);
    };
    if let Some(p) = pet {
        if get_pet(c, p)?.is_none() {
            return Ok(false);
        }
    }
    c.execute(
        "UPDATE pet_crops SET pet_id = ?2, source = 'manual' WHERE id = ?1",
        params![crop_id, pet],
    )?;
    let mut affected: Vec<String> = crop.pet_id.into_iter().collect();
    affected.extend(pet.map(str::to_string));
    cleanup_pets(c, &affected)?;
    Ok(true)
}

/// Moves the crops of the `from` pets to `into` and deletes those pets. An unnamed `into`
/// takes the name (and birth date) of the first named one. Unknown or repeated ids are skipped;
/// returns the ids that were merged.
pub fn merge_pets(
    c: &Connection,
    into: &str,
    from: &[String],
    now: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut merged: Vec<String> = Vec::new();
    for f in from {
        if f != into && !merged.contains(f) && get_pet(c, f)?.is_some() {
            merged.push(f.clone());
        }
    }
    let Some(target) = get_pet(c, into)? else {
        return Ok(Vec::new());
    };
    let (mut name, mut birth) = (target.name.clone(), target.birth_date.clone());
    for f in &merged {
        let p = get_pet(c, f)?.expect("checked above");
        if name.is_none() && p.name.is_some() {
            name = p.name;
            birth = birth.or(p.birth_date);
        }
        c.execute(
            "UPDATE pet_crops SET pet_id = ?1 WHERE pet_id = ?2",
            params![into, f],
        )?;
        c.execute("DELETE FROM pets WHERE id = ?1", [f])?;
    }
    c.execute(
        "UPDATE pets SET name = ?2, birth_date = ?3, updated_at = ?4 WHERE id = ?1",
        params![into, name, birth, now],
    )?;
    ensure_cover(c, into)?;
    Ok(merged)
}

// ---- forgetting

/// Drops everything about an item's crops (its picture changed for good or the item was purged):
/// the crops, the scan records, and the pets left with no crop (unnamed ones). Returns the
/// removed crop ids (their crops are the caller's to delete).
pub fn forget(c: &Connection, item_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare("SELECT id, pet_id FROM pet_crops WHERE item_id = ?1")?;
    let rows: Vec<(String, Option<String>)> = st
        .query_map([item_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    c.execute("DELETE FROM pet_crops WHERE item_id = ?1", [item_id])?;
    c.execute("DELETE FROM pet_scans WHERE item_id = ?1", [item_id])?;
    let pets: Vec<String> = rows.iter().filter_map(|(_, p)| p.clone()).collect();
    cleanup_pets(c, &pets)?;
    Ok(rows.into_iter().map(|(id, _)| id).collect())
}

/// Drops crops and scans of items that are gone, given the SQL of the live item ids. Returns the
/// removed crop ids.
pub fn prune(c: &Connection, live_ids_sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(&format!(
        "SELECT DISTINCT item_id FROM pet_crops WHERE item_id NOT IN ({live_ids_sql}) \
         UNION SELECT item_id FROM pet_scans WHERE item_id NOT IN ({live_ids_sql})"
    ))?;
    let gone: Vec<String> = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    let mut removed = Vec::new();
    for id in gone {
        removed.extend(forget(c, &id)?);
    }
    Ok(removed)
}

/// Removes up to 200 rows (crops first, then scan records, then pets); how many went. Call it
/// until it returns 0 to clear everything without holding the catalog for long.
pub fn delete_some(c: &Connection) -> rusqlite::Result<usize> {
    for table in ["pet_crops", "pet_scans", "pets"] {
        let n = c.execute(
            &format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} LIMIT 200)"),
            [],
        )?;
        if n > 0 {
            return Ok(n);
        }
    }
    Ok(0)
}

/// Removes every crop, pet and scan record. The setting is left as it is.
pub fn delete_all(c: &Connection) -> rusqlite::Result<()> {
    while delete_some(c)? > 0 {}
    Ok(())
}

// ---- reading

/// Every crop of `model` with `dim` values in a clustering graph.
pub fn load_graph(c: &Connection, model: &str, dim: usize) -> rusqlite::Result<Graph> {
    let mut g = Graph::with_rule(dim, crate::pets::MATCH, crate::pets::MIN_CROPS);
    let mut st =
        c.prepare("SELECT id, pet_id, source, vec FROM pet_crops WHERE model = ?1 AND dim = ?2")?;
    let mut rows = st.query(params![model, dim as i64])?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let pet: Option<String> = r.get(1)?;
        let source: String = r.get(2)?;
        let b: Vec<u8> = r.get(3)?;
        if let Some(v) = from_f16_bytes(&b) {
            g.insert(&id, &v, pet.as_deref(), source == "manual");
        }
    }
    Ok(g)
}

pub fn count_crops(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT COUNT(*) FROM pet_crops", [], |r| r.get(0))
}

/// The crops [`load_graph`] would load.
pub fn count_model_crops(c: &Connection, model: &str, dim: usize) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM pet_crops WHERE model = ?1 AND dim = ?2",
        params![model, dim as i64],
        |r| r.get(0),
    )
}

pub fn count_scans(c: &Connection, model: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM pet_scans WHERE model = ?1",
        [model],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn nf(x: f32, y: f32, w: f32, h: f32, v: [f32; 2]) -> NewCrop {
        NewCrop {
            x,
            y,
            w,
            h,
            score: 0.9,
            vec: v.to_vec(),
        }
    }

    fn pet(c: &Connection, id: &str, name: Option<&str>) {
        c.execute(
            "INSERT INTO pets (id, name, created_at, updated_at) VALUES (?1, ?2, 't', 't')",
            params![id, name],
        )
        .unwrap();
    }

    #[test]
    fn the_setting_is_on_until_turned_off() {
        let c = db();
        assert!(enabled(&c).unwrap(), "pets need no opt-in");
        set_enabled(&c, false).unwrap();
        assert!(!enabled(&c).unwrap());
        set_enabled(&c, true).unwrap();
        assert!(enabled(&c).unwrap());
    }

    #[test]
    fn boxes_match_best_overlap_first_and_once() {
        let old = [
            [0.1, 0.1, 0.2, 0.2],
            [0.14, 0.1, 0.2, 0.2],
            [0.7, 0.7, 0.1, 0.1],
        ];
        let new = [
            [0.11, 0.1, 0.2, 0.2],
            [0.72, 0.7, 0.1, 0.1],
            [0.4, 0.4, 0.1, 0.1],
        ];
        let m = match_boxes(&old, &new, 0.5);
        assert_eq!(m[0], Some(0)); // old 0 overlaps it more than old 1 does
        assert_eq!(m[1], Some(2));
        assert_eq!(m[2], None);
        // A single old box is taken once.
        let m = match_boxes(
            &[[0.1, 0.1, 0.2, 0.2]],
            &[[0.1, 0.1, 0.2, 0.2], [0.11, 0.1, 0.2, 0.2]],
            0.5,
        );
        assert_eq!(m, vec![Some(0), None]);
    }

    #[test]
    fn a_rescan_keeps_ids_pets_and_manual_choices() {
        let c = db();
        let first = [
            nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0]),
            nf(0.6, 0.1, 0.2, 0.2, [0.0, 1.0]),
            nf(0.4, 0.6, 0.1, 0.1, [1.0, 1.0]),
        ];
        let r = replace_item(&c, "it_1", "m", &first, "t0").unwrap();
        assert_eq!(r.added.len(), 3);
        assert!(r.removed.is_empty());
        let ids: Vec<String> = r.crops.iter().map(|(id, _)| id.clone()).collect();
        pet(&c, "pe_a", None);
        apply_assignments(
            &c,
            &[PetAssignment {
                crop_id: ids[0].clone(),
                pet_id: "pe_a".into(),
                created: false,
            }],
            "t0",
        )
        .unwrap();
        // The owner sets the second crop (to nobody) and the third (to the pet).
        assert!(assign_crop(&c, &ids[1], None).unwrap());
        assert!(assign_crop(&c, &ids[2], Some("pe_a")).unwrap());

        // Scanned again: the first box moved a little, the second is gone, the third is gone too,
        // and a new one appeared.
        let second = [
            nf(0.12, 0.1, 0.2, 0.2, [0.9, 0.1]),
            nf(0.8, 0.8, 0.1, 0.1, [0.5, 0.5]),
        ];
        let r = replace_item(&c, "it_1", "m", &second, "t1").unwrap();
        assert_eq!(r.crops[0].0, ids[0], "same crop keeps its id");
        assert_eq!(r.added.len(), 1);
        // The first (ml) matched; the owner's two stay even though nothing matches them.
        assert!(r.removed.is_empty());
        let rows = crops_of_item(&c, "it_1").unwrap();
        assert_eq!(rows.len(), 4);
        let first_row = rows.iter().find(|f| f.id == ids[0]).unwrap();
        assert_eq!(first_row.pet_id.as_deref(), Some("pe_a"));
        assert!((first_row.x - 0.12).abs() < 1e-6);
        let third = rows.iter().find(|f| f.id == ids[2]).unwrap();
        assert_eq!(
            (third.source.as_str(), third.pet_id.as_deref()),
            ("manual", Some("pe_a"))
        );
        let scans: (i64, String) = c
            .query_row("SELECT crops, scanned_at FROM pet_scans", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(scans, (4, "t1".to_string()));

        // An ml crop nothing matches any more is removed, and its empty pet with it.
        let r = replace_item(
            &c,
            "it_1",
            "m",
            &[nf(0.9, 0.9, 0.05, 0.05, [0.0, 1.0])],
            "t2",
        )
        .unwrap();
        assert_eq!(
            r.removed.len(),
            2,
            "the first ml crop and the one added second go"
        );
        assert_eq!(r.added.len(), 1);
        assert!(
            get_pet(&c, "pe_a").unwrap().is_some(),
            "the owner's crop still belongs to it"
        );
    }

    #[test]
    fn assignments_make_pets_and_skip_manual_crops() {
        let c = db();
        let r = replace_item(
            &c,
            "it_1",
            "m",
            &[
                nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0]),
                nf(0.6, 0.1, 0.4, 0.4, [1.0, 0.1]),
                nf(0.3, 0.7, 0.1, 0.1, [1.0, 0.2]),
            ],
            "t",
        )
        .unwrap();
        let ids: Vec<String> = r.crops.iter().map(|(i, _)| i.clone()).collect();
        assert!(assign_crop(&c, &ids[2], None).unwrap()); // by hand: unassigned
        apply_assignments(
            &c,
            &ids.iter()
                .enumerate()
                .map(|(i, f)| PetAssignment {
                    crop_id: f.clone(),
                    pet_id: "pe_new".into(),
                    created: i == 0,
                })
                .collect::<Vec<_>>(),
            "t",
        )
        .unwrap();
        let rows = crops_of_item(&c, "it_1").unwrap();
        assert_eq!(rows.iter().filter(|f| f.pet_id.is_some()).count(), 2);
        assert!(rows
            .iter()
            .find(|f| f.id == ids[2])
            .unwrap()
            .pet_id
            .is_none());
        let p = get_pet(&c, "pe_new").unwrap().unwrap();
        // The cover is the biggest, surest box: the second crop.
        assert_eq!(p.cover_crop_id.as_deref(), Some(ids[1].as_str()));
        // A pet that was deleted meanwhile is not assigned to.
        apply_assignments(
            &c,
            &[PetAssignment {
                crop_id: ids[0].clone(),
                pet_id: "pe_gone".into(),
                created: false,
            }],
            "t",
        )
        .unwrap();
        assert_eq!(
            crops_of_item(&c, "it_1").unwrap()[0].pet_id.as_deref(),
            Some("pe_new")
        );
    }

    #[test]
    fn assigning_by_hand_cleans_up_pets() {
        let c = db();
        let r = replace_item(
            &c,
            "it_1",
            "m",
            &[
                nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0]),
                nf(0.6, 0.1, 0.2, 0.2, [0.0, 1.0]),
            ],
            "t",
        )
        .unwrap();
        let (a, b) = (r.crops[0].0.clone(), r.crops[1].0.clone());
        pet(&c, "pe_1", None);
        pet(&c, "pe_named", Some("Ada"));
        assert!(assign_crop(&c, &a, Some("pe_1")).unwrap());
        assert!(!assign_crop(&c, &a, Some("pe_nope")).unwrap());
        assert!(!assign_crop(&c, "pc_nope", None).unwrap());
        assert_eq!(
            get_pet(&c, "pe_1")
                .unwrap()
                .unwrap()
                .cover_crop_id
                .as_deref(),
            Some(a.as_str())
        );
        // Moving the only crop away drops the unnamed pet; a named one is kept, coverless.
        assert!(assign_crop(&c, &a, Some("pe_named")).unwrap());
        assert!(get_pet(&c, "pe_1").unwrap().is_none());
        assert!(!assign_crop(&c, &a, Some("pe_1")).unwrap());
        assert!(assign_crop(&c, &a, None).unwrap());
        let named = get_pet(&c, "pe_named").unwrap().unwrap();
        assert_eq!(named.cover_crop_id, None);
        assert_eq!(get_crop(&c, &b).unwrap().unwrap().source, "ml");
    }

    #[test]
    fn merging_moves_crops_and_adopts_a_name() {
        let c = db();
        let r = replace_item(
            &c,
            "it_1",
            "m",
            &[
                nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0]),
                nf(0.6, 0.1, 0.2, 0.2, [0.0, 1.0]),
                nf(0.3, 0.6, 0.1, 0.1, [1.0, 1.0]),
            ],
            "t",
        )
        .unwrap();
        let ids: Vec<String> = r.crops.iter().map(|(i, _)| i.clone()).collect();
        pet(&c, "pe_a", None);
        pet(&c, "pe_b", Some("Bea"));
        pet(&c, "pe_c", None);
        for (f, p) in ids.iter().zip(["pe_a", "pe_b", "pe_c"]) {
            c.execute(
                "UPDATE pet_crops SET pet_id = ?2 WHERE id = ?1",
                params![f, p],
            )
            .unwrap();
        }
        c.execute(
            "UPDATE pets SET birth_date = '1990-05-06' WHERE id = 'pe_b'",
            [],
        )
        .unwrap();
        let merged = merge_pets(
            &c,
            "pe_a",
            &[
                "pe_b".into(),
                "pe_nope".into(),
                "pe_a".into(),
                "pe_b".into(),
                "pe_c".into(),
            ],
            "t2",
        )
        .unwrap();
        assert_eq!(merged, vec!["pe_b".to_string(), "pe_c".to_string()]);
        let a = get_pet(&c, "pe_a").unwrap().unwrap();
        assert_eq!(a.name.as_deref(), Some("Bea"));
        assert_eq!(a.birth_date.as_deref(), Some("1990-05-06"));
        assert!(a.cover_crop_id.is_some());
        assert!(get_pet(&c, "pe_b").unwrap().is_none() && get_pet(&c, "pe_c").unwrap().is_none());
        assert!(crops_of_item(&c, "it_1")
            .unwrap()
            .iter()
            .all(|f| f.pet_id.as_deref() == Some("pe_a")));
        assert!(merge_pets(&c, "pe_missing", &["pe_a".into()], "t")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn forgetting_an_item_fixes_pets_and_covers() {
        let c = db();
        let r1 = replace_item(&c, "it_1", "m", &[nf(0.1, 0.1, 0.4, 0.4, [1.0, 0.0])], "t").unwrap();
        let r2 = replace_item(&c, "it_2", "m", &[nf(0.1, 0.1, 0.1, 0.1, [1.0, 0.1])], "t").unwrap();
        let r3 = replace_item(&c, "it_3", "m", &[nf(0.1, 0.1, 0.3, 0.3, [0.0, 1.0])], "t").unwrap();
        pet(&c, "pe_a", None);
        pet(&c, "pe_named", Some("Cy"));
        c.execute("UPDATE pet_crops SET pet_id = 'pe_a'", [])
            .unwrap();
        c.execute(
            "UPDATE pet_crops SET pet_id = 'pe_named' WHERE item_id = 'it_3'",
            [],
        )
        .unwrap();
        ensure_cover(&c, "pe_a").unwrap();
        ensure_cover(&c, "pe_named").unwrap();
        assert_eq!(
            get_pet(&c, "pe_a").unwrap().unwrap().cover_crop_id,
            Some(r1.crops[0].0.clone())
        );
        // The cover's item goes: the other crop becomes the cover.
        assert_eq!(forget(&c, "it_1").unwrap(), vec![r1.crops[0].0.clone()]);
        assert_eq!(
            get_pet(&c, "pe_a").unwrap().unwrap().cover_crop_id,
            Some(r2.crops[0].0.clone())
        );
        // The last crop goes: the unnamed pet is dropped, the named one stays without a cover.
        forget(&c, "it_2").unwrap();
        forget(&c, "it_3").unwrap();
        assert!(get_pet(&c, "pe_a").unwrap().is_none());
        let named = get_pet(&c, "pe_named").unwrap().unwrap();
        assert_eq!(named.cover_crop_id, None);
        assert_eq!(count_crops(&c).unwrap(), 0);
        assert_eq!(count_scans(&c, "m").unwrap(), 0);
        assert!(r3.removed.is_empty());
    }

    #[test]
    fn prune_and_delete_all() {
        let c = db();
        c.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY); INSERT INTO items VALUES ('it_1');",
        )
        .unwrap();
        replace_item(&c, "it_1", "m", &[nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0])], "t").unwrap();
        let gone = replace_item(
            &c,
            "it_gone",
            "m",
            &[nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0])],
            "t",
        )
        .unwrap();
        put_scan_failure(&c, "it_gone2", "m", &"x".repeat(1000), "t").unwrap();
        assert_eq!(
            prune(&c, "SELECT id FROM items").unwrap(),
            vec![gone.crops[0].0.clone()]
        );
        assert_eq!(count_crops(&c).unwrap(), 1);
        assert_eq!(count_scans(&c, "m").unwrap(), 1);
        for i in 0..450 {
            c.execute(
                "INSERT INTO pet_crops (id, item_id, model, x, y, w, h, score, dim, vec, created_at) \
                 VALUES (?1, 'it_1', 'm', 0, 0, 1, 1, 1, 1, x'0000', 't')",
                [format!("pc_{i}")],
            )
            .unwrap();
        }
        pet(&c, "pe_x", Some("X"));
        set_enabled(&c, true).unwrap();
        delete_all(&c).unwrap();
        assert_eq!(count_crops(&c).unwrap(), 0);
        let pets: i64 = c
            .query_row("SELECT COUNT(*) FROM pets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pets, 0);
        assert!(enabled(&c).unwrap(), "the setting is not touched");
    }

    #[test]
    fn the_graph_loads_vectors_and_states() {
        let c = db();
        let r = replace_item(
            &c,
            "it_1",
            "m",
            &[
                nf(0.1, 0.1, 0.2, 0.2, [1.0, 0.0]),
                nf(0.6, 0.1, 0.2, 0.2, [0.0, 1.0]),
            ],
            "t",
        )
        .unwrap();
        pet(&c, "pe_a", None);
        assert!(assign_crop(&c, &r.crops[0].0, Some("pe_a")).unwrap());
        let g = load_graph(&c, "m", 2).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g.person_of(&r.crops[0].0), Some("pe_a"));
        assert!(g.is_manual(&r.crops[0].0) && !g.is_manual(&r.crops[1].0));
        assert!(load_graph(&c, "m", 3).unwrap().is_empty());
        assert!(load_graph(&c, "other", 2).unwrap().is_empty());
    }
}

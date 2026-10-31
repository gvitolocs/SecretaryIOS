//! The duplicate review screen's server side (`docs/gallery-vs-immich.md`, gap 4): groups of photos the
//! detector (`dedup.rs`) calls the same picture, a suggested keeper for each, and the choices the owner
//! makes, all on the existing undoable merge (`dedup_merge.rs`).
//!
//! - `GET duplicates?status=open`: the groups, from the perceptual hashes already stored in
//!   `item_hashes`. A request never hashes pictures; photos without a hash are counted as `pending`
//!   (the backfill is `lucina-media dedup-report`/the hasher, not a request).
//! - `POST duplicates/resolve {keep, drop}`: the `drop` photos are hidden under `keep[0]`; the other
//!   `keep` photos stay, and each pair of them is recorded as "not duplicates".
//! - `POST duplicates/dismiss {ids, undo?}`: "not duplicates", remembered (`dedup_dismissed`), so the
//!   group does not come back. A merge the owner undid counts as the same decision.
//! - `POST duplicates/undo {ids}`: put merged copies back (the metadata too).
//! - `POST duplicates/resolve-all {min_score?}`: the suggested keeper of every group scoring at least
//!   `ApplyOpts::default().min_score` (0.7; a larger `min_score` is allowed).
//!
//! The suggested keeper: clearly more pixels (5 %), else the larger file, else more metadata, else the
//! device's own asset, else the earlier arrival. The first rule is ours: a small PNG re-save can be
//! bigger than the full-size JPEG it came from, and "largest file" alone would keep it.
//!
//! Keeping one merges what the merge itself does not carry (albums, the favourite flag, the cover and
//! the device asset it does): captions are joined, a location is copied only when the keeper has none
//! and every copy that has one agrees (within 100 m), and a capture time is copied the same way (copies
//! within 2 s). The merge's undo reverts all of it ([`revert_metadata`], called from
//! `dedup_merge::undo`), unless the owner has edited that field since.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, now_str};
use crate::dedup::{self, Cand, Class, Thresholds};
use crate::dedup_merge::{self, ApplyOpts};
use crate::http::{bad, item_out, ApiError, ApiResult};
use crate::ingest::Library;

/// The most photos one resolve or dismiss names.
pub const MAX_IDS: usize = 200;
const MAX_DISMISS: usize = 50;
/// Two locations closer than this (kilometres) are the same place.
const SAME_PLACE_KM: f64 = 0.1;
/// Two capture times this close (milliseconds) are the same moment.
const SAME_MOMENT_MS: i64 = 2_000;

/// The schema, idempotent: called from `db::migrate`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        -- 'These are not duplicates': one row per pair, `a` < `b`.
        CREATE TABLE IF NOT EXISTS dedup_dismissed (
            a TEXT NOT NULL,
            b TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (a, b)
        );
        -- What keeping a photo copied onto it from a hidden copy, for the merge's undo.
        CREATE TABLE IF NOT EXISTS dedup_meta_undo (
            item_id TEXT PRIMARY KEY,
            survivor TEXT NOT NULL,
            caption_added TEXT,
            loc_lat REAL,
            loc_lon REAL,
            taken_at_set TEXT,
            sort_at_before TEXT,
            created_at TEXT NOT NULL
        );
        ",
    )
}

/// A purged photo leaves no decision behind.
pub fn forget(c: &Connection, id: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM dedup_dismissed WHERE a = ?1 OR b = ?1", [id])?;
    c.execute(
        "DELETE FROM dedup_meta_undo WHERE item_id = ?1 OR survivor = ?1",
        [id],
    )?;
    Ok(())
}

// ---- the keep suggestion -------------------------------------------------------------------

/// Why one photo is the suggested keeper over the next best.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    HighestResolution,
    LargestFile,
    MostMetadata,
    DeviceOriginal,
    FirstAdded,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::HighestResolution => "highest_resolution",
            Reason::LargestFile => "largest_file",
            Reason::MostMetadata => "most_metadata",
            Reason::DeviceOriginal => "device_original",
            Reason::FirstAdded => "first_added",
        }
    }
}

/// What the keep suggestion looks at in one photo.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub id: String,
    pub bytes: i64,
    pub pixels: i64,
    /// How many of capture time, place, camera, caption, size, favourite, album and original name it has.
    pub metadata: u32,
    pub device_original: bool,
    pub added_at: String,
}

/// Facts for `ids`, in the order given; ids that are not items are left out.
pub fn facts(c: &Connection, ids: &[String]) -> rusqlite::Result<Vec<Facts>> {
    let mut st = c.prepare_cached(
        "SELECT i.bytes, i.width, i.height, i.taken_at, i.lat, i.place_name, i.camera, i.caption, i.favorite,
                i.origin_name, i.device, i.asset_id, i.source, i.added_at,
                (SELECT COUNT(*) FROM album_items ai JOIN albums a ON a.id = ai.album_id
                  WHERE ai.item_id = i.id AND a.system_key IS NULL)
         FROM items i WHERE i.id = ?1",
    )?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let row = st
            .query_row([id], |r| {
                let (w, h): (Option<i64>, Option<i64>) = (r.get(1)?, r.get(2)?);
                let text = |i: usize| -> rusqlite::Result<bool> {
                    Ok(r.get::<_, Option<String>>(i)?
                        .is_some_and(|s| !s.trim().is_empty()))
                };
                let metadata = [
                    text(3)?,
                    r.get::<_, Option<f64>>(4)?.is_some(),
                    text(5)?,
                    text(6)?,
                    text(7)?,
                    w.is_some() && h.is_some(),
                    r.get::<_, i64>(8)? != 0,
                    r.get::<_, i64>(14)? > 0,
                    text(9)?,
                ]
                .iter()
                .filter(|b| **b)
                .count() as u32;
                let source: String = r.get(12)?;
                Ok(Facts {
                    id: id.clone(),
                    bytes: r.get(0)?,
                    pixels: w.zip(h).map_or(0, |(w, h)| w.max(0) * h.max(0)),
                    metadata,
                    device_original: r.get::<_, Option<String>>(10)?.is_some()
                        && r.get::<_, Option<String>>(11)?.is_some()
                        && crate::gallery::source_group(&source).is_none(),
                    added_at: r.get(13)?,
                })
            })
            .optional()?;
        out.extend(row);
    }
    Ok(out)
}

/// The deciding rule if `a` is the better keeper than `b`, or `None` when `b` is at least as good.
fn beats(a: &Facts, b: &Facts) -> Option<Reason> {
    let (pa, pb) = (a.pixels as f64, b.pixels as f64);
    if pa > pb * 1.05 {
        return Some(Reason::HighestResolution);
    }
    if pb > pa * 1.05 {
        return None;
    }
    if a.bytes != b.bytes {
        return (a.bytes > b.bytes).then_some(Reason::LargestFile);
    }
    if a.metadata != b.metadata {
        return (a.metadata > b.metadata).then_some(Reason::MostMetadata);
    }
    if a.device_original != b.device_original {
        return a.device_original.then_some(Reason::DeviceOriginal);
    }
    if a.added_at != b.added_at {
        return (a.added_at < b.added_at).then_some(Reason::FirstAdded);
    }
    (a.id < b.id).then_some(Reason::FirstAdded)
}

fn best_index(f: &[Facts]) -> usize {
    let mut best = 0;
    for i in 1..f.len() {
        if beats(&f[i], &f[best]).is_some() {
            best = i;
        }
    }
    best
}

/// The photos best first (the keeper, then the rest by size) and the reason the keeper beats the
/// runner-up.
pub fn order(mut f: Vec<Facts>) -> (Vec<Facts>, Reason) {
    if f.is_empty() {
        return (f, Reason::FirstAdded);
    }
    let keeper = f.remove(best_index(&f));
    f.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
    let runner = f.iter().fold(None::<&Facts>, |best, x| match best {
        Some(b) if beats(x, b).is_none() => Some(b),
        _ => Some(x),
    });
    let reason = runner
        .and_then(|r| beats(&keeper, r))
        .unwrap_or(Reason::FirstAdded);
    f.insert(0, keeper);
    (f, reason)
}

/// The id of the best keeper among `ids`.
pub fn best_of(c: &Connection, ids: &[String]) -> rusqlite::Result<Option<String>> {
    let f = facts(c, ids)?;
    if f.is_empty() {
        return Ok(None);
    }
    Ok(Some(f[best_index(&f)].id.clone()))
}

// ---- groups --------------------------------------------------------------------------------

/// A group before the keeper is chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct RawGroup {
    pub members: Vec<String>,
    pub class: Class,
    /// The weakest merge in the group.
    pub score: f32,
}

/// A group of the same picture with its suggested keeper.
#[derive(Clone, Debug)]
pub struct Group {
    pub id: String,
    pub class: Class,
    pub score: f32,
    pub applicable: bool,
    pub keep: String,
    pub reason: Reason,
    /// The keeper first, then the others by size.
    pub members: Vec<String>,
    pub metadata: HashMap<String, u32>,
    /// Bytes of the other copies that are not the same stored file as the keeper.
    pub reclaimable: i64,
}

pub fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn group_id(members: &[String]) -> String {
    let mut sorted: Vec<&str> = members.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    format!(
        "dg_{}",
        &blake3::hash(sorted.join("\n").as_bytes()).to_hex()[..16]
    )
}

/// The duplicate groups of `cands`: the detector's pairs without the ones in `skip`, joined into
/// photos the way `dedup::plan_merges` does it (never joining two pictures that are known to be
/// different, or burst neighbours).
pub fn plan_groups(
    cands: &[Cand],
    skip: &HashSet<(String, String)>,
    th: &Thresholds,
) -> Vec<RawGroup> {
    let (mut pairs, _) = dedup::find_pairs(cands, th);
    pairs.retain(|p| !skip.contains(&pair_key(&cands[p.a].id, &cands[p.b].id)));
    let plans = dedup::plan_merges(cands, &pairs);
    let mut by_root: BTreeMap<String, Vec<&dedup::MergePlan>> = BTreeMap::new();
    for p in &plans {
        by_root.entry(p.root.clone()).or_default().push(p);
    }
    let rank = |c: Class| match c {
        Class::Exact => 3,
        Class::SameReencoded => 2,
        _ => 1,
    };
    by_root
        .into_iter()
        .map(|(root, group)| {
            let mut members = vec![root];
            members.extend(group.iter().map(|p| p.duplicate.clone()));
            let weakest = group
                .iter()
                .min_by(|a, b| {
                    a.score
                        .total_cmp(&b.score)
                        .then(rank(a.kind).cmp(&rank(b.kind)))
                        .then(a.duplicate.cmp(&b.duplicate))
                })
                .expect("a group has a merge");
            RawGroup {
                members,
                class: weakest.kind,
                score: weakest.score,
            }
        })
        .collect()
}

/// Rank the members of each raw group and sort the groups: applicable first, then best score.
pub fn rank_groups(
    raw: Vec<RawGroup>,
    facts_of: &mut dyn FnMut(&[String]) -> rusqlite::Result<Vec<Facts>>,
    hash_of: &HashMap<String, String>,
    opts: &ApplyOpts,
) -> rusqlite::Result<Vec<Group>> {
    let mut out = Vec::with_capacity(raw.len());
    for g in raw {
        let (ordered, reason) = order(facts_of(&g.members)?);
        if ordered.len() < 2 {
            continue;
        }
        let keep = ordered[0].id.clone();
        let keep_hash = hash_of.get(&keep);
        let reclaimable = ordered[1..]
            .iter()
            .filter(|m| hash_of.get(&m.id) != keep_hash)
            .map(|m| m.bytes)
            .sum();
        out.push(Group {
            id: group_id(&g.members),
            class: g.class,
            score: g.score,
            applicable: g.score >= opts.min_score && opts.classes.contains(&g.class),
            keep,
            reason,
            members: ordered.iter().map(|f| f.id.clone()).collect(),
            metadata: ordered.iter().map(|f| (f.id.clone(), f.metadata)).collect(),
            reclaimable,
        });
    }
    out.sort_by(|a, b| {
        b.applicable
            .cmp(&a.applicable)
            .then(b.score.total_cmp(&a.score))
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Pairs the detector must not propose: dismissed ones, and merges the owner undid.
fn skip_pairs(c: &Connection) -> rusqlite::Result<HashSet<(String, String)>> {
    let mut out = HashSet::new();
    let mut st = c.prepare("SELECT a, b FROM dedup_dismissed")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (a, b) = row?;
        out.insert((a, b));
    }
    let mut st =
        c.prepare("SELECT item_id, duplicate_of FROM item_links WHERE undone_at IS NOT NULL")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (a, b) = row?;
        out.insert(pair_key(&a, &b));
    }
    Ok(out)
}

/// What the groups were computed over.
pub struct Computed {
    pub groups: Vec<Group>,
    pub photos: usize,
    pub hashed: usize,
}

/// The current groups. The catalog is locked only to read; the pair search runs outside it.
pub async fn compute(lib: &Library, opts: &ApplyOpts) -> Result<Computed, ApiError> {
    let (cands, skip) = lib
        .db
        .call(|c| {
            let stacked: HashSet<String> = {
                let mut st = c.prepare("SELECT item_id FROM stack_items")?;
                let rows = st
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                rows
            };
            // A stacked photo is the owner's (or a burst's) grouping, not a duplicate to review.
            let cands: Vec<Cand> = dedup::load_cands(c, c)?
                .into_iter()
                .filter(|x| !stacked.contains(&x.id))
                .collect();
            Ok((cands, skip_pairs(c)?))
        })
        .await?;
    let (photos, hashed) = (
        cands.len(),
        cands.iter().filter(|x| x.hashes.is_some()).count(),
    );
    let hash_of: HashMap<String, String> = cands
        .iter()
        .map(|x| (x.id.clone(), x.hash.clone()))
        .collect();
    let raw =
        tokio::task::spawn_blocking(move || plan_groups(&cands, &skip, &Thresholds::default()))
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let opts = opts.clone();
    let groups = lib
        .db
        .call(move |c| rank_groups(raw, &mut |ids| facts(c, ids), &hash_of, &opts))
        .await?;
    Ok(Computed {
        groups,
        photos,
        hashed,
    })
}

// ---- merging with metadata -----------------------------------------------------------------

/// What a keep copied onto the keeper, for the answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetaOutcome {
    pub captions: usize,
    pub location: bool,
    pub taken_at: bool,
}

struct MetaRow {
    caption: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    place: [Option<String>; 4],
    taken_at: Option<String>,
    sort_at: String,
}

fn meta_row(c: &Connection, id: &str) -> rusqlite::Result<Option<MetaRow>> {
    c.query_row(
        "SELECT caption, lat, lon, place_name, admin1, admin2, cc, taken_at, sort_at FROM items WHERE id = ?1",
        [id],
        |r| {
            Ok(MetaRow {
                caption: r.get(0)?,
                lat: r.get(1)?,
                lon: r.get(2)?,
                place: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
                taken_at: r.get(7)?,
                sort_at: r.get(8)?,
            })
        },
    )
    .optional()
}

fn millis(t: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(t)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Copy what the merge does not carry from the hidden `dups` onto `survivor`, in one transaction.
pub fn merge_metadata(
    c: &mut Connection,
    survivor: &str,
    dups: &[String],
) -> rusqlite::Result<MetaOutcome> {
    let tx = c.transaction()?;
    let mut out = MetaOutcome::default();
    let Some(s) = meta_row(&tx, survivor)? else {
        return Ok(out);
    };
    let rows: Vec<(String, MetaRow)> = dups
        .iter()
        .filter_map(|d| {
            meta_row(&tx, d)
                .transpose()
                .map(|r| r.map(|r| (d.clone(), r)))
        })
        .collect::<rusqlite::Result<_>>()?;

    // Captions: the copies' captions joined under the keeper's own, each once.
    let mut caption = s.caption.clone().unwrap_or_default();
    let mut added: HashMap<String, String> = HashMap::new();
    for (id, r) in &rows {
        let Some(text) = r
            .caption
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        if caption.contains(text) {
            continue;
        }
        let segment = if caption.trim().is_empty() {
            caption.clear();
            text.to_string()
        } else {
            format!("\n\n{text}")
        };
        caption.push_str(&segment);
        added.insert(id.clone(), segment);
        out.captions += 1;
    }

    // Location: only when the keeper has none and every copy that has one agrees.
    let mut place: Option<(&String, &MetaRow)> = None;
    if s.lat.is_none() || s.lon.is_none() {
        let located: Vec<(&String, &MetaRow)> = rows
            .iter()
            .filter(|(_, r)| r.lat.is_some() && r.lon.is_some())
            .map(|(id, r)| (id, r))
            .collect();
        if let Some(&(_, first)) = located.first() {
            let (la, lo) = (first.lat.unwrap_or(0.0), first.lon.unwrap_or(0.0));
            let agree = located.iter().all(|(_, r)| {
                crate::geo::haversine_km(la, lo, r.lat.unwrap_or(0.0), r.lon.unwrap_or(0.0))
                    <= SAME_PLACE_KM
            });
            if agree {
                place = located.first().copied();
            }
        }
    }

    // Capture time: only when the keeper has none and the copies that have one agree.
    let mut moment: Option<(&String, &MetaRow)> = None;
    if s.taken_at.is_none() {
        let dated: Vec<(&String, &MetaRow, i64)> = rows
            .iter()
            .filter_map(|(id, r)| r.taken_at.as_deref().and_then(millis).map(|t| (id, r, t)))
            .collect();
        if let (Some(lo), Some(hi)) = (
            dated.iter().map(|d| d.2).min(),
            dated.iter().map(|d| d.2).max(),
        ) {
            if hi - lo <= SAME_MOMENT_MS {
                moment = dated.iter().find(|d| d.2 == lo).map(|d| (d.0, d.1));
            }
        }
    }

    let now = now_str();
    if out.captions > 0 {
        tx.execute(
            "UPDATE items SET caption = ?2 WHERE id = ?1",
            params![survivor, caption],
        )?;
    }
    if let Some((_, r)) = place {
        tx.execute(
            "UPDATE items SET lat = ?2, lon = ?3, place_name = ?4, admin1 = ?5, admin2 = ?6, cc = ?7 WHERE id = ?1",
            params![survivor, r.lat, r.lon, r.place[0], r.place[1], r.place[2], r.place[3]],
        )?;
        out.location = true;
    }
    if let Some((_, r)) = moment {
        tx.execute(
            "UPDATE items SET taken_at = ?2, sort_at = ?3 WHERE id = ?1",
            params![survivor, r.taken_at, r.sort_at],
        )?;
        out.taken_at = true;
    }
    if out.captions > 0 || out.location || out.taken_at {
        tx.execute(
            "UPDATE items SET updated_at = ?2 WHERE id = ?1",
            params![survivor, now],
        )?;
        for (id, _) in &rows {
            let loc = place.filter(|(p, _)| *p == id).map(|(_, r)| r);
            let when = moment.filter(|(p, _)| *p == id).map(|(_, r)| r);
            let segment = added.get(id);
            if loc.is_none() && when.is_none() && segment.is_none() {
                continue;
            }
            tx.execute(
                "INSERT OR REPLACE INTO dedup_meta_undo
                   (item_id, survivor, caption_added, loc_lat, loc_lon, taken_at_set, sort_at_before, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    survivor,
                    segment,
                    loc.and_then(|r| r.lat),
                    loc.and_then(|r| r.lon),
                    when.and_then(|r| r.taken_at.clone()),
                    when.map(|_| s.sort_at.clone()),
                    now
                ],
            )?;
        }
        db::reindex(&tx, survivor)?;
    }
    tx.commit()?;
    Ok(out)
}

/// Undo what [`merge_metadata`] copied from the hidden copy `dup` onto `survivor`, field by field and
/// only where the survivor still holds the copied value. Called by `dedup_merge::undo`.
pub fn revert_metadata(c: &Connection, dup: &str, survivor: &str) -> rusqlite::Result<()> {
    type Saved = (
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Saved> = c
        .query_row(
            "SELECT caption_added, loc_lat, loc_lon, taken_at_set, sort_at_before
             FROM dedup_meta_undo WHERE item_id = ?1 AND survivor = ?2",
            params![dup, survivor],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((segment, lat, lon, taken, sort_before)) = row else {
        return Ok(());
    };
    if let Some(segment) = segment {
        let current: Option<String> = c
            .query_row("SELECT caption FROM items WHERE id = ?1", [survivor], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        if let Some(current) = current.filter(|t| t.contains(&segment)) {
            let rest = current.replacen(&segment, "", 1);
            let rest = rest.trim();
            c.execute(
                "UPDATE items SET caption = ?2 WHERE id = ?1",
                params![survivor, (!rest.is_empty()).then_some(rest)],
            )?;
        }
    }
    if let (Some(lat), Some(lon)) = (lat, lon) {
        c.execute(
            "UPDATE items SET lat = NULL, lon = NULL, place_name = NULL, admin1 = NULL, admin2 = NULL, cc = NULL
             WHERE id = ?1 AND lat = ?2 AND lon = ?3",
            params![survivor, lat, lon],
        )?;
    }
    if let (Some(taken), Some(sort_before)) = (taken, sort_before) {
        c.execute(
            "UPDATE items SET taken_at = NULL, sort_at = ?3 WHERE id = ?1 AND taken_at = ?2",
            params![survivor, taken, sort_before],
        )?;
    }
    c.execute("DELETE FROM dedup_meta_undo WHERE item_id = ?1", [dup])?;
    Ok(())
}

/// One copy to hide under a keeper: its id, and the class and score the link records.
pub struct DropItem {
    pub id: String,
    pub kind: String,
    pub score: f64,
}

#[derive(Debug, Default)]
pub struct Merged {
    pub merged: Vec<(String, String)>,
    pub skipped: Vec<(String, &'static str)>,
    pub meta: MetaOutcome,
}

/// Why a photo cannot be a keeper, or `None`.
fn keeper_blocker(c: &Connection, id: &str) -> rusqlite::Result<Option<&'static str>> {
    let row: Option<(String, bool, i64, bool)> = c
        .query_row(
            "SELECT kind, trashed_at IS NOT NULL, hidden,
                    EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = items.id)
             FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        None => Some("missing"),
        Some((kind, ..)) if kind != "photo" => Some("not_photo"),
        Some((_, true, ..)) => Some("trashed"),
        Some((.., true)) => Some("drive_file"),
        Some((_, _, hidden, _)) if hidden != 0 => Some("hidden"),
        _ => None,
    })
}

/// Hide each copy under `keeper` (the undoable merge) and copy their metadata onto it.
pub fn merge_into(
    c: &mut Connection,
    keeper: &str,
    drops: &[DropItem],
    decided_by: &str,
    redo: bool,
) -> rusqlite::Result<Merged> {
    let mut out = Merged::default();
    if let Some(reason) = keeper_blocker(c, keeper)? {
        let reason = match reason {
            "missing" => "keep_missing",
            "not_photo" => "keep_not_photo",
            "trashed" => "keep_trashed",
            "drive_file" => "keep_drive_file",
            _ => "keep_hidden",
        };
        out.skipped = drops.iter().map(|d| (d.id.clone(), reason)).collect();
        return Ok(out);
    }
    for d in drops {
        match dedup_merge::merge(c, keeper, &d.id, &d.kind, d.score, decided_by, redo)? {
            Ok(()) => out.merged.push((d.id.clone(), keeper.to_string())),
            Err(skip) => out.skipped.push((d.id.clone(), skip.as_str())),
        }
    }
    let ids: Vec<String> = out.merged.iter().map(|(id, _)| id.clone()).collect();
    if !ids.is_empty() {
        out.meta = merge_metadata(c, keeper, &ids)?;
    }
    Ok(out)
}

/// The class and score to record for hiding `dup` under `keeper` by hand: the detector's verdict when
/// it has one, else a manual decision.
fn manual_verdict(cands: &HashMap<String, Cand>, keeper: &str, dup: &str) -> (String, f64) {
    if let (Some(a), Some(b)) = (cands.get(keeper), cands.get(dup)) {
        if let Some(v) = dedup::classify(a, b, &Thresholds::default()) {
            if v.class.is_duplicate() {
                return (v.class.as_str().to_string(), v.score as f64);
            }
        }
    }
    ("manual".to_string(), 1.0)
}

// ---- the API -------------------------------------------------------------------------------

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/duplicates", get(list))
        .route("/v1/media/duplicates/resolve", post(resolve))
        .route("/v1/media/duplicates/resolve-all", post(resolve_all))
        .route("/v1/media/duplicates/dismiss", post(dismiss))
        .route("/v1/media/duplicates/undo", post(undo))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    status: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list(State(lib): State<Library>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    if q.status.as_deref().is_some_and(|s| s != "open") {
        return Err(bad("status must be open"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0);
    let opts = ApplyOpts::default();
    let computed = compute(&lib, &opts).await?;
    let applicable = computed.groups.iter().filter(|g| g.applicable).count();
    let reclaimable: i64 = computed
        .groups
        .iter()
        .filter(|g| g.applicable)
        .map(|g| g.reclaimable)
        .sum();
    let total = computed.groups.len();
    let page: Vec<Group> = computed
        .groups
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();
    let page_ids: Vec<Vec<String>> = page.iter().map(|g| g.members.clone()).collect();
    let (items, suggestions) = lib
        .db
        .call(move |c| {
            let mut all = Vec::new();
            for ids in &page_ids {
                let mut group = Vec::new();
                for id in ids {
                    if let Some(it) = db::get_item(c, id)? {
                        group.push(it);
                    }
                }
                all.push(group);
            }
            Ok((all, crate::stacks::suggest(c)?.items.len()))
        })
        .await?;
    let groups: Vec<Value> = page
        .into_iter()
        .zip(items)
        .map(|(g, members)| {
            let members: Vec<Value> = members
                .into_iter()
                .map(|it| {
                    let id = it.id.clone();
                    let mut v =
                        serde_json::to_value(item_out(&lib, it, None)).unwrap_or(Value::Null);
                    v["review"] = json!({
                        "metadata": g.metadata.get(&id).copied().unwrap_or(0),
                        "keep": id == g.keep,
                    });
                    v
                })
                .collect();
            json!({
                "id": g.id,
                "class": g.class.as_str(),
                "score": g.score,
                "applicable": g.applicable,
                "keep": g.keep,
                "reason": g.reason.as_str(),
                "members": members,
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "status": "open",
        "hashes": {
            "photos": computed.photos,
            "hashed": computed.hashed,
            "pending": computed.photos - computed.hashed,
        },
        "total": total,
        "applicable": applicable,
        "min_score": opts.min_score,
        "reclaimable_bytes": reclaimable,
        "stack_suggestions": suggestions,
        "groups": groups,
    })))
}

#[derive(Debug, Deserialize)]
struct ResolveReq {
    #[serde(default)]
    keep: Vec<String>,
    #[serde(default)]
    drop: Vec<String>,
}

fn check_ids(ids: impl Iterator<Item = impl AsRef<str>>) -> ApiResult<()> {
    if ids.into_iter().any(|id| !crate::batch::id_ok(id.as_ref())) {
        return Err(bad("an id is not valid"));
    }
    Ok(())
}

fn merged_json(m: &Merged) -> Value {
    json!({
        "merged": m.merged.iter().map(|(id, into)| json!({ "id": id, "into": into })).collect::<Vec<_>>(),
        "skipped": m.skipped.iter().map(|(id, reason)| json!({ "id": id, "reason": reason })).collect::<Vec<_>>(),
    })
}

async fn resolve(
    State(lib): State<Library>,
    Json(req): Json<ResolveReq>,
) -> ApiResult<Json<Value>> {
    let dedupe = |v: Vec<String>| {
        let mut seen = HashSet::new();
        v.into_iter()
            .filter(|x| seen.insert(x.clone()))
            .collect::<Vec<_>>()
    };
    let (keep, drop) = (dedupe(req.keep), dedupe(req.drop));
    if keep.is_empty() {
        return Err(bad("keep must name at least one photo"));
    }
    if keep.len() + drop.len() > MAX_IDS {
        return Err(bad(format!("at most {MAX_IDS} ids per call")));
    }
    check_ids(keep.iter().chain(drop.iter()))?;
    if drop.iter().any(|d| keep.contains(d)) {
        return Err(bad("a photo cannot be both kept and dropped"));
    }
    let keeper = keep[0].clone();
    let result = lib
        .db
        .call(move |c| {
            // Photos kept side by side are, by the owner's word, not duplicates of each other.
            for (i, a) in keep.iter().enumerate() {
                for b in &keep[i + 1..] {
                    let (x, y) = pair_key(a, b);
                    c.execute(
                        "INSERT OR IGNORE INTO dedup_dismissed (a, b, created_at) VALUES (?1, ?2, ?3)",
                        params![x, y, now_str()],
                    )?;
                }
            }
            let cands: HashMap<String, Cand> = if drop.is_empty() {
                HashMap::new()
            } else {
                dedup::load_cands(c, c)?
                    .into_iter()
                    .filter(|x| x.id == keeper || drop.contains(&x.id))
                    .map(|x| (x.id.clone(), x))
                    .collect()
            };
            let drops: Vec<DropItem> = drop
                .iter()
                .map(|d| {
                    let (kind, score) = manual_verdict(&cands, &keeper, d);
                    DropItem {
                        id: d.clone(),
                        kind,
                        score,
                    }
                })
                .collect();
            merge_into(c, &keeper, &drops, "review", true)
        })
        .await?;
    let mut v = merged_json(&result);
    v["ok"] = json!(true);
    v["metadata"] = json!({
        "captions": result.meta.captions,
        "location": result.meta.location,
        "taken_at": result.meta.taken_at,
    });
    Ok(Json(v))
}

#[derive(Debug, Deserialize)]
struct ResolveAllReq {
    min_score: Option<f32>,
}

async fn resolve_all(
    State(lib): State<Library>,
    body: Option<Json<ResolveAllReq>>,
) -> ApiResult<Json<Value>> {
    let mut opts = ApplyOpts::default();
    let floor = opts.min_score;
    if let Some(Json(ResolveAllReq { min_score: Some(m) })) = body {
        if !m.is_finite() {
            return Err(bad("min_score must be a number"));
        }
        opts.min_score = m.clamp(floor, 1.0);
    }
    let computed = compute(&lib, &opts).await?;
    let groups: Vec<Group> = computed
        .groups
        .into_iter()
        .filter(|g| g.applicable)
        .collect();
    let n = groups.len();
    let total = lib
        .db
        .call(move |c| {
            let mut total = Merged::default();
            for g in &groups {
                let drops: Vec<DropItem> = g.members[1..]
                    .iter()
                    .map(|id| DropItem {
                        id: id.clone(),
                        kind: g.class.as_str().to_string(),
                        score: g.score as f64,
                    })
                    .collect();
                let m = merge_into(c, &g.keep, &drops, "review:all", false)?;
                total.merged.extend(m.merged);
                total.skipped.extend(m.skipped);
            }
            Ok(total)
        })
        .await?;
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, reason) in &total.skipped {
        *skipped.entry(reason).or_default() += 1;
    }
    Ok(Json(json!({
        "ok": true,
        "groups": n,
        "merged": total.merged.len(),
        "merged_ids": total.merged.iter().map(|(id, into)| json!({ "id": id, "into": into })).collect::<Vec<_>>(),
        "skipped": skipped,
    })))
}

#[derive(Debug, Deserialize)]
struct DismissReq {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    undo: bool,
}

async fn dismiss(
    State(lib): State<Library>,
    Json(req): Json<DismissReq>,
) -> ApiResult<Json<Value>> {
    let mut seen = HashSet::new();
    let ids: Vec<String> = req
        .ids
        .into_iter()
        .filter(|i| seen.insert(i.clone()))
        .collect();
    if ids.len() < 2 || ids.len() > MAX_DISMISS {
        return Err(bad(format!(
            "ids must be 2 to {MAX_DISMISS} different photos"
        )));
    }
    check_ids(ids.iter())?;
    let undo = req.undo;
    let pairs = lib
        .db
        .call(move |c| {
            let mut n = 0;
            for (i, a) in ids.iter().enumerate() {
                for b in &ids[i + 1..] {
                    let (x, y) = pair_key(a, b);
                    n += if undo {
                        c.execute("DELETE FROM dedup_dismissed WHERE a = ?1 AND b = ?2", params![x, y])?
                    } else {
                        c.execute(
                            "INSERT OR IGNORE INTO dedup_dismissed (a, b, created_at) VALUES (?1, ?2, ?3)",
                            params![x, y, now_str()],
                        )?
                    };
                }
            }
            Ok(n)
        })
        .await?;
    Ok(Json(json!({ "ok": true, "pairs": pairs })))
}

#[derive(Debug, Deserialize)]
struct UndoReq {
    #[serde(default)]
    ids: Vec<String>,
}

async fn undo(State(lib): State<Library>, Json(req): Json<UndoReq>) -> ApiResult<Json<Value>> {
    if req.ids.is_empty() || req.ids.len() > MAX_IDS {
        return Err(bad(format!("ids must be 1 to {MAX_IDS} photos")));
    }
    check_ids(req.ids.iter())?;
    let ids = req.ids;
    let (undone, not_merged) = lib
        .db
        .call(move |c| {
            let (mut undone, mut not_merged) = (Vec::new(), Vec::new());
            for id in ids {
                if dedup_merge::undo(c, &id)? {
                    undone.push(id);
                } else {
                    not_merged.push(id);
                }
            }
            Ok((undone, not_merged))
        })
        .await?;
    Ok(Json(
        json!({ "ok": true, "undone": undone, "not_merged": not_merged }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(id: &str, bytes: i64, pixels: i64, metadata: u32) -> Facts {
        Facts {
            id: id.into(),
            bytes,
            pixels,
            metadata,
            device_original: false,
            added_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn the_larger_file_is_kept_when_the_pictures_are_the_same_size() {
        let (o, why) = order(vec![
            facts("small", 1_000, 12_000_000, 5),
            facts("big", 3_000, 12_000_000, 1),
        ]);
        assert_eq!(o[0].id, "big");
        assert_eq!(why, Reason::LargestFile);
    }

    #[test]
    fn a_big_file_with_fewer_pixels_does_not_win() {
        // A PNG re-save of a thumbnail is bigger than the camera JPEG it came from.
        let (o, why) = order(vec![
            facts("png", 9_000_000, 800_000, 2),
            facts("jpeg", 3_000_000, 12_000_000, 2),
        ]);
        assert_eq!(o[0].id, "jpeg");
        assert_eq!(why, Reason::HighestResolution);
    }

    #[test]
    fn equal_files_fall_to_metadata_then_device_then_arrival() {
        let (o, why) = order(vec![facts("a", 100, 100, 2), facts("b", 100, 100, 6)]);
        assert_eq!((o[0].id.as_str(), why), ("b", Reason::MostMetadata));

        let mut phone = facts("phone", 100, 100, 4);
        phone.device_original = true;
        let (o, why) = order(vec![facts("chat", 100, 100, 4), phone]);
        assert_eq!((o[0].id.as_str(), why), ("phone", Reason::DeviceOriginal));

        let mut late = facts("late", 100, 100, 4);
        late.added_at = "2026-02-01T00:00:00.000Z".into();
        let (o, why) = order(vec![late, facts("early", 100, 100, 4)]);
        assert_eq!((o[0].id.as_str(), why), ("early", Reason::FirstAdded));
    }

    #[test]
    fn the_order_is_the_keeper_then_the_rest_by_size() {
        let (o, _) = order(vec![
            facts("c", 10, 100, 0),
            facts("a", 30, 100, 0),
            facts("b", 20, 100, 0),
        ]);
        let ids: Vec<&str> = o.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_group_id_does_not_depend_on_member_order() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["y".to_string(), "x".to_string()];
        assert_eq!(group_id(&a), group_id(&b));
    }
}

//! Finding the same photo twice (docs/media-library.md, "Identity").
//!
//! The catalog already folds exact duplicates (same blake3) and versions an edited phone asset. What it
//! cannot see is the same picture that arrived through two paths in two encodings: the phone's
//! HEIC and the JPEG an old backup holds, a chat share of a library photo, a resized copy. This module
//! finds them without touching anything:
//!
//! 1. [`hash_missing`] computes a perceptual hash (`phash.rs`) of each photo's 360 px thumbnail, lazily,
//!    into an `item_hashes` table (the catalog's own, or a scratch database for a read-only report);
//! 2. [`find_pairs`] proposes candidate pairs (taken within [`Thresholds::time_window_ms`], same file name
//!    and size, or visually identical) and [`classify`] scores each one, a pure function over two
//!    [`Cand`]s: `exact`, `same_photo_reencoded`, `resized_copy` or `burst_neighbor`;
//! 3. [`build_report`] turns the pairs into the dry-run numbers and a merge plan that
//!    `dedup_merge.rs` can apply.
//!
//! Wrongly merging two different photos is much worse than missing a duplicate, so every rule errs towards
//! "not a duplicate": two assets of one device, or two differently named files of one collection, are never
//! duplicates of each other however alike they look (a burst is not a duplicate).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::phash::{self, hamming, Hashes};

/// The decision thresholds. `distance` is the mean of the two 64-bit Hamming distances, 0 to 64.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    /// Two pictures taken within this many milliseconds count as the same moment.
    pub time_window_ms: i64,
    /// Largest distance for a duplicate with no time evidence (a chat copy without EXIF), and the
    /// base for the visual-only candidate search.
    pub strict: f32,
    /// Largest distance for a duplicate that also agrees on the moment (or on name and size).
    pub close: f32,
    /// Extra distance allowed when the two sizes differ (scaling blurs the hash a little more).
    pub scale_slack: f32,
    /// Largest distance at which two pictures taken together are still "similar": a burst neighbour.
    pub similar: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            time_window_ms: 2_000,
            strict: 4.0,
            close: 10.0,
            scale_slack: 2.0,
            similar: 22.0,
        }
    }
}

/// What kind of pair two pictures make.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// The same bytes (blake3): ingest should have folded them.
    Exact,
    /// The same picture at the same size in another encoding (HEIC and JPEG, a recompressed copy).
    #[serde(rename = "same_photo_reencoded")]
    SameReencoded,
    /// The same picture at another size.
    ResizedCopy,
    /// Alike but different frames: a burst, a retake. NOT a duplicate.
    BurstNeighbor,
}

impl Class {
    pub fn is_duplicate(self) -> bool {
        !matches!(self, Class::BurstNeighbor)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Exact => "exact",
            Class::SameReencoded => "same_photo_reencoded",
            Class::ResizedCopy => "resized_copy",
            Class::BurstNeighbor => "burst_neighbor",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "exact" => Class::Exact,
            "same_photo_reencoded" => Class::SameReencoded,
            "resized_copy" => Class::ResizedCopy,
            "burst_neighbor" => Class::BurstNeighbor,
            _ => return None,
        })
    }
}

/// How two capture times relate.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeRel {
    /// The very same millisecond.
    Equal,
    /// Within the window.
    Near,
    /// Apart by a whole number of quarter hours (a time zone read differently), otherwise equal.
    ZoneShift,
    Far,
    /// One of them has no capture time.
    Unknown,
}

impl TimeRel {
    fn same_moment(self) -> bool {
        matches!(self, TimeRel::Equal | TimeRel::Near | TimeRel::ZoneShift)
    }
}

/// How two sizes relate, upright or turned a quarter.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DimsRel {
    Same,
    /// The same shape at another size.
    Scaled,
    /// Sizes unknown, only the thumbnails' shapes agree.
    AspectOnly,
    Different,
}

/// One photo as the detector sees it.
#[derive(Clone, Debug)]
pub struct Cand {
    pub id: String,
    /// blake3 of the stored bytes (`items.hash`).
    pub hash: String,
    pub bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_ms: Option<i64>,
    /// The original file name, lower case.
    pub name: Option<String>,
    pub source: String,
    pub device: Option<String>,
    pub asset_id: Option<String>,
    /// Which group of sources this is, for the numbers (`iphone`, `backup`, `archive-nespc`, `chat`…).
    pub label: String,
    pub added_at: String,
    pub hashes: Option<Hashes>,
}

impl Cand {
    fn dims(&self) -> Option<(i64, i64)> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        }
    }
    fn pixels(&self) -> i64 {
        self.dims().map_or(0, |(w, h)| w * h)
    }
    /// Came from a device and is that device's own asset (not a chat copy or a Lucina picture).
    pub fn is_device_original(&self) -> bool {
        self.device.is_some()
            && self.asset_id.is_some()
            && crate::gallery::source_group(&self.source).is_none()
    }
}

/// Where a source string belongs, for the numbers. A device item is labelled by its device kind
/// (`iphone`, `backup`…), a chat share `chat`, anything else by its own `source` tag.
pub fn source_label(source: &str, device_kind: Option<&str>) -> String {
    use crate::gallery::{source_group, SourceGroup};
    match source_group(source) {
        Some(SourceGroup::Chat) => "chat".into(),
        Some(SourceGroup::Lucina) => "lucina".into(),
        None => device_kind
            .map(str::to_string)
            .unwrap_or_else(|| source.to_string()),
    }
}

/// The mean of the two Hamming distances, or `None` when either picture has no hash.
pub fn distance(a: &Cand, b: &Cand) -> Option<f32> {
    let (x, y) = (a.hashes?, b.hashes?);
    Some((hamming(x.phash, y.phash) + hamming(x.dhash, y.dhash)) as f32 / 2.0)
}

pub fn time_rel(a: &Cand, b: &Cand, th: &Thresholds) -> TimeRel {
    let (Some(x), Some(y)) = (a.taken_ms, b.taken_ms) else {
        return TimeRel::Unknown;
    };
    let d = (x - y).abs();
    if d == 0 {
        TimeRel::Equal
    } else if d <= th.time_window_ms {
        TimeRel::Near
    } else if d <= 14 * 3_600_000 {
        let quarter = 900_000;
        let off = d % quarter;
        if off.min(quarter - off) <= th.time_window_ms {
            TimeRel::ZoneShift
        } else {
            TimeRel::Far
        }
    } else {
        TimeRel::Far
    }
}

pub fn dims_rel(a: &Cand, b: &Cand) -> DimsRel {
    fn ratio_close(r1: f64, r2: f64, short: f64) -> bool {
        (r1 - r2).abs() / r1.max(r2) <= 0.02 + 2.0 / short
    }
    match (a.dims(), b.dims()) {
        (Some((aw, ah)), Some((bw, bh))) => {
            let (al, as_) = (aw.max(ah), aw.min(ah));
            let (bl, bs) = (bw.max(bh), bw.min(bh));
            let slack = (al.max(bl) / 200).max(1);
            if (al - bl).abs() <= slack && (as_ - bs).abs() <= slack {
                DimsRel::Same
            } else if ratio_close(
                al as f64 / as_ as f64,
                bl as f64 / bs as f64,
                as_.min(bs) as f64,
            ) {
                DimsRel::Scaled
            } else {
                DimsRel::Different
            }
        }
        _ => match (a.hashes, b.hashes) {
            (Some(x), Some(y)) => {
                let r =
                    |h: Hashes| h.width.max(h.height) as f64 / h.width.min(h.height).max(1) as f64;
                if ratio_close(
                    r(x),
                    r(y),
                    x.width.min(x.height).min(y.width.min(y.height)) as f64,
                ) {
                    DimsRel::AspectOnly
                } else {
                    DimsRel::Different
                }
            }
            _ => DimsRel::Different,
        },
    }
}

/// Two assets of one device are two photos, whatever they look like.
fn distinct_assets(a: &Cand, b: &Cand) -> bool {
    a.device.is_some()
        && a.device == b.device
        && matches!((&a.asset_id, &b.asset_id), (Some(x), Some(y)) if x != y)
}

fn same_name(a: &Cand, b: &Cand) -> bool {
    matches!((&a.name, &b.name), (Some(x), Some(y)) if x == y)
}

fn same_name_and_size(a: &Cand, b: &Cand) -> bool {
    same_name(a, b) && a.bytes == b.bytes
}

/// Two files of one collection (same source and device) that are not provably the same file.
fn same_collection_other_file(a: &Cand, b: &Cand) -> bool {
    a.source == b.source && a.device == b.device && !same_name(a, b)
}

/// Whether the pair can never be merged, even through a chain of other duplicates.
pub fn known_distinct(a: &Cand, b: &Cand) -> bool {
    a.hash != b.hash && (distinct_assets(a, b) || same_collection_other_file(a, b))
}

/// The verdict on one pair.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Verdict {
    pub class: Class,
    /// Confidence that this is the same photo (a burst neighbour: how alike the frames are), 0 to 1.
    pub score: f32,
    pub distance: Option<f32>,
    pub time: TimeRel,
    pub dims: DimsRel,
    pub same_name_size: bool,
}

/// Judge two photos. `None`: not related enough to mention.
pub fn classify(a: &Cand, b: &Cand, th: &Thresholds) -> Option<Verdict> {
    if a.id == b.id {
        return None;
    }
    let time = time_rel(a, b, th);
    let dims = dims_rel(a, b);
    let name_size = same_name_and_size(a, b);
    if a.hash == b.hash {
        return Some(Verdict {
            class: Class::Exact,
            score: 1.0,
            distance: Some(0.0),
            time,
            dims,
            same_name_size: name_size,
        });
    }
    if dims == DimsRel::Different {
        return None;
    }
    let d = distance(a, b)?;
    let flat = a.hashes.is_some_and(|h| h.flat) || b.hashes.is_some_and(|h| h.flat);
    let scaled = dims != DimsRel::Same;
    let slack = if scaled { th.scale_slack } else { 0.0 };
    let verdict = |class: Class, score: f32| Verdict {
        class,
        score: score.clamp(0.0, 1.0),
        distance: Some(d),
        time,
        dims,
        same_name_size: name_size,
    };
    let similarity = (1.0 - d / th.similar).clamp(0.0, 1.0);
    if known_distinct(a, b) {
        return (time.same_moment() && d <= th.similar)
            .then(|| verdict(Class::BurstNeighbor, similarity));
    }
    // A flat picture hashes like any other flat picture: only the name, size and moment can vouch for it.
    let evidence = time.same_moment() || name_size;
    let limit = (if evidence { th.close } else { th.strict }) + slack;
    let vouched = !flat || (evidence && d <= th.strict + slack);
    if d <= limit && vouched {
        let mut score = 0.6 * similarity;
        score += match time {
            TimeRel::Equal => 0.25,
            TimeRel::Near => 0.2,
            TimeRel::ZoneShift => 0.15,
            TimeRel::Unknown => 0.0,
            TimeRel::Far => -0.05,
        };
        score += match dims {
            DimsRel::Same => 0.1,
            _ => 0.05,
        };
        if name_size {
            score += 0.1;
        }
        let class = if scaled {
            Class::ResizedCopy
        } else {
            Class::SameReencoded
        };
        return Some(verdict(class, score));
    }
    if time.same_moment() && d <= th.similar {
        return Some(verdict(Class::BurstNeighbor, similarity));
    }
    None
}

/// A scored pair of candidates (indices into the slice that was searched).
#[derive(Clone, Debug)]
pub struct Pair {
    pub a: usize,
    pub b: usize,
    pub verdict: Verdict,
}

/// A histogram of distances, one bucket per two bits.
pub type Histogram = Vec<u64>;

/// What the visual search saw, for choosing the thresholds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Calibration {
    /// Pairs of the same size taken more than a day apart, by distance bucket (bucket i = [2i, 2i+2)):
    /// two different photos, so what a distance threshold would wrongly accept.
    pub unrelated_same_size: Histogram,
    /// Pairs of the same size taken at the same moment in different collections, by bucket: mostly
    /// the same photo twice, plus the odd burst.
    pub same_moment_cross_collection: Histogram,
    /// Pairs of one device's different assets taken at the same moment: certainly different photos
    /// (bursts, Live Photos), so how alike different frames get.
    pub same_moment_distinct_assets: Histogram,
    /// How many pairs of different collections were taken within the window, whatever their size
    /// (0 means the collections do not overlap in time, so the moment search had nothing to find).
    pub same_moment_cross_collection_total: u64,
    /// How many pairs of photos each need only the look to be found (every pair is compared).
    pub pairs_compared_by_look: u64,
    /// Known copies: a sample of the library's own thumbnails, re-encoded as JPEG at quality 30, 60 and
    /// 90, by distance bucket. What a duplicate threshold must still accept.
    pub known_reencoded: Histogram,
    /// Known copies: the same sample scaled to 50% and 25% and encoded at quality 70.
    pub known_resized: Histogram,
    /// How many photos the sample had.
    pub known_sample: u64,
}

const BUCKETS: usize = 33;

fn bump(h: &mut Histogram, d: f32) {
    if h.is_empty() {
        h.resize(BUCKETS, 0);
    }
    let i = ((d / 2.0) as usize).min(BUCKETS - 1);
    h[i] += 1;
}

/// Candidate pairs by moment, by name and size, by identical bytes and by look, each judged by
/// [`classify`]. Returns the related pairs and the calibration histograms.
pub fn find_pairs(cands: &[Cand], th: &Thresholds) -> (Vec<Pair>, Calibration) {
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut pairs: Vec<Pair> = Vec::new();
    let mut cal = Calibration::default();
    let mut consider = |i: usize, j: usize, pairs: &mut Vec<Pair>| {
        let key = (i.min(j), i.max(j));
        if !seen.insert(key) {
            return;
        }
        if let Some(v) = classify(&cands[key.0], &cands[key.1], th) {
            pairs.push(Pair {
                a: key.0,
                b: key.1,
                verdict: v,
            });
        }
    };
    // Identical bytes.
    let mut by_hash: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        by_hash.entry(c.hash.as_str()).or_default().push(i);
    }
    for group in by_hash.values().filter(|g| g.len() > 1 && g.len() <= 50) {
        for (x, &i) in group.iter().enumerate() {
            for &j in &group[x + 1..] {
                consider(i, j, &mut pairs);
            }
        }
    }
    // The same name and size.
    let mut by_name: HashMap<(&str, i64), Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if let Some(n) = &c.name {
            by_name.entry((n.as_str(), c.bytes)).or_default().push(i);
        }
    }
    for group in by_name.values().filter(|g| g.len() > 1 && g.len() <= 20) {
        for (x, &i) in group.iter().enumerate() {
            for &j in &group[x + 1..] {
                consider(i, j, &mut pairs);
            }
        }
    }
    // The same moment.
    let mut timed: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].taken_ms.is_some())
        .collect();
    timed.sort_by_key(|&i| cands[i].taken_ms);
    for (x, &i) in timed.iter().enumerate() {
        let t = cands[i].taken_ms.unwrap_or(0);
        for &j in &timed[x + 1..] {
            if cands[j].taken_ms.unwrap_or(0) - t > th.time_window_ms {
                break;
            }
            consider(i, j, &mut pairs);
            if !known_distinct(&cands[i], &cands[j])
                && (cands[i].source != cands[j].source || cands[i].device != cands[j].device)
            {
                cal.same_moment_cross_collection_total += 1;
            }
            if let (Some(d), true) = (
                distance(&cands[i], &cands[j]),
                dims_rel(&cands[i], &cands[j]) == DimsRel::Same,
            ) {
                if known_distinct(&cands[i], &cands[j]) {
                    if distinct_assets(&cands[i], &cands[j]) {
                        bump(&mut cal.same_moment_distinct_assets, d);
                    }
                } else {
                    bump(&mut cal.same_moment_cross_collection, d);
                }
            }
        }
    }
    // Looks alike, whatever the dates say: every pair of hashed, detailed pictures (a few
    // hundred million popcounts for a library this size).
    let hashed: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].hashes.is_some_and(|h| !h.flat))
        .collect();
    let limit = th.strict * 2.0 + th.scale_slack * 2.0;
    for (x, &i) in hashed.iter().enumerate() {
        let hi = cands[i].hashes.expect("hashed");
        for &j in &hashed[x + 1..] {
            cal.pairs_compared_by_look += 1;
            let hj = cands[j].hashes.expect("hashed");
            let dsum = hamming(hi.phash, hj.phash) + hamming(hi.dhash, hj.dhash);
            let d = dsum as f32 / 2.0;
            if d > 64.0 {
                continue;
            }
            let (a, b) = (&cands[i], &cands[j]);
            if let (Some(x), Some(y)) = (a.taken_ms, b.taken_ms) {
                if (x - y).abs() > 86_400_000 && dims_rel(a, b) == DimsRel::Same {
                    bump(&mut cal.unrelated_same_size, d);
                }
            }
            if dsum as f32 <= limit {
                consider(i, j, &mut pairs);
            }
        }
    }
    pairs.sort_by(|p, q| {
        q.verdict
            .score
            .total_cmp(&p.verdict.score)
            .then(p.a.cmp(&q.a))
            .then(p.b.cmp(&q.b))
    });
    (pairs, cal)
}

// ---- the merge plan --------------------------------------------------------------------------

/// Whether `a` should survive instead of `b`: the device's original (highest resolution, earliest
/// arrival). A picture with clearly more pixels (5%) wins; otherwise the device's own asset; then the
/// earlier arrival; then the smaller id, so the choice is stable.
pub fn survives_over(a: &Cand, b: &Cand) -> bool {
    let (pa, pb) = (a.pixels() as f64, b.pixels() as f64);
    if pa > pb * 1.05 {
        return true;
    }
    if pb > pa * 1.05 {
        return false;
    }
    if a.is_device_original() != b.is_device_original() {
        return a.is_device_original();
    }
    if a.added_at != b.added_at {
        return a.added_at < b.added_at;
    }
    a.id < b.id
}

/// One duplicate to hide under a survivor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MergePlan {
    pub duplicate: String,
    /// The photo it hides under now. In a chain of three the intermediate may itself be hidden next;
    /// the merge flattens that.
    pub survivor: String,
    /// The photo that survives in the end.
    pub root: String,
    pub kind: Class,
    pub score: f32,
    /// How many merges come after this one in the same group (apply the deepest first).
    pub depth: u32,
    pub bytes: i64,
}

/// Group the duplicate pairs into photos (never joining two pictures [`known_distinct`] or a burst
/// neighbour apart) and decide who survives in each group.
pub fn plan_merges(cands: &[Cand], pairs: &[Pair]) -> Vec<MergePlan> {
    let n = cands.len();
    let mut group_of: Vec<usize> = (0..n).collect();
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    let burst: HashSet<(usize, usize)> = pairs
        .iter()
        .filter(|p| p.verdict.class == Class::BurstNeighbor)
        .map(|p| (p.a, p.b))
        .collect();
    let apart = |x: usize, y: usize| {
        burst.contains(&(x.min(y), x.max(y))) || known_distinct(&cands[x], &cands[y])
    };
    let mut tree_edges: Vec<(usize, usize, &Verdict)> = Vec::new();
    for p in pairs.iter().filter(|p| p.verdict.class.is_duplicate()) {
        let (ga, gb) = (group_of[p.a], group_of[p.b]);
        if ga == gb {
            continue;
        }
        let ma = members.entry(ga).or_insert_with(|| vec![ga]).clone();
        let mb = members.entry(gb).or_insert_with(|| vec![gb]).clone();
        if ma.iter().any(|&x| mb.iter().any(|&y| apart(x, y))) {
            continue;
        }
        for &m in &mb {
            group_of[m] = ga;
        }
        members.remove(&gb);
        members.get_mut(&ga).expect("group").extend(mb);
        tree_edges.push((p.a, p.b, &p.verdict));
    }
    // Per group: the survivor is the best member, the tree edges (best scores first, already in
    // that order) hang the others from it.
    let mut out = Vec::new();
    for (_, group) in members {
        let root = *group
            .iter()
            .reduce(|best, m| {
                if survives_over(&cands[*m], &cands[*best]) {
                    m
                } else {
                    best
                }
            })
            .expect("a group has members");
        let in_group: HashSet<usize> = group.iter().copied().collect();
        let mut adj: HashMap<usize, Vec<(usize, &Verdict)>> = HashMap::new();
        for (a, b, v) in &tree_edges {
            if in_group.contains(a) {
                adj.entry(*a).or_default().push((*b, v));
                adj.entry(*b).or_default().push((*a, v));
            }
        }
        let mut depth: HashMap<usize, u32> = HashMap::from([(root, 0)]);
        let mut queue = std::collections::VecDeque::from([root]);
        let mut parent: HashMap<usize, (usize, &Verdict)> = HashMap::new();
        while let Some(x) = queue.pop_front() {
            for (y, v) in adj.get(&x).into_iter().flatten() {
                if depth.contains_key(y) {
                    continue;
                }
                depth.insert(*y, depth[&x] + 1);
                parent.insert(*y, (x, v));
                queue.push_back(*y);
            }
        }
        for (child, (par, v)) in parent {
            out.push(MergePlan {
                duplicate: cands[child].id.clone(),
                survivor: cands[par].id.clone(),
                root: cands[root].id.clone(),
                kind: v.class,
                score: v.score,
                depth: depth[&child],
                bytes: cands[child].bytes,
            });
        }
    }
    // The deepest first (a leaf is hidden before its parent), then best score, then id.
    out.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then(b.score.total_cmp(&a.score))
            .then(a.duplicate.cmp(&b.duplicate))
    });
    out
}

// ---- the report ------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairOut {
    pub a: String,
    pub b: String,
    pub class: Class,
    pub score: f32,
    pub distance: Option<f32>,
    pub time: TimeRel,
    pub dims: DimsRel,
    pub same_name_size: bool,
    pub a_label: String,
    pub b_label: String,
    /// Seconds between the two capture times, when both are known.
    pub time_delta_s: Option<i64>,
    /// Stored bytes and pixels of `a` and `b`.
    pub bytes: [i64; 2],
    pub pixels: [i64; 2],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClassStats {
    pub pairs: u64,
    /// Pairs that would become a merge.
    pub merges: u64,
    /// The duplicate's bytes in those merges: what is kept as an alternate rendition, and what a later
    /// cleanup of the alternates could free. Exact duplicates share one blob, so they free nothing.
    pub duplicate_bytes: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcePairStats {
    /// The two source labels, in alphabetical order.
    pub sources: [String; 2],
    pub class: Class,
    pub pairs: u64,
    pub merges: u64,
    pub duplicate_bytes: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryStats {
    pub photos: u64,
    pub hashed: u64,
    pub flat: u64,
    pub without_hash: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub generated_at: String,
    pub thresholds: Thresholds,
    pub library: LibraryStats,
    pub classes: BTreeMap<String, ClassStats>,
    pub by_source_pair: Vec<SourcePairStats>,
    /// Duplicates that would be hidden, and photos left after.
    pub merge_total: u64,
    pub merge_duplicate_bytes: i64,
    /// Score distribution of the merges, in tenths (0 = [0, 0.1), 9 = [0.9, 1.0]).
    pub merge_scores: Vec<u64>,
    pub calibration: Calibration,
    /// 20 pairs spread over the classes, opaque ids only.
    pub samples: Vec<PairOut>,
    pub merges: Vec<MergePlan>,
    pub pairs: Vec<PairOut>,
}

pub const REPORT_VERSION: u32 = 1;

pub fn build_report(cands: &[Cand], th: &Thresholds) -> Report {
    let (pairs, calibration) = find_pairs(cands, th);
    let merges = plan_merges(cands, &pairs);
    let index: HashMap<&str, usize> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let mut classes: BTreeMap<String, ClassStats> = BTreeMap::new();
    for c in [
        Class::Exact,
        Class::SameReencoded,
        Class::ResizedCopy,
        Class::BurstNeighbor,
    ] {
        classes.insert(c.as_str().into(), ClassStats::default());
    }
    let mut sp: BTreeMap<([String; 2], Class), SourcePairStats> = BTreeMap::new();
    let mut pair_out = Vec::with_capacity(pairs.len());
    let sources_of = |i: usize, j: usize| -> [String; 2] {
        let mut s = [cands[i].label.clone(), cands[j].label.clone()];
        s.sort();
        s
    };
    for p in &pairs {
        let cs = classes.get_mut(p.verdict.class.as_str()).expect("class");
        cs.pairs += 1;
        let key = (sources_of(p.a, p.b), p.verdict.class);
        sp.entry(key.clone())
            .or_insert_with(|| SourcePairStats {
                sources: key.0.clone(),
                class: key.1,
                pairs: 0,
                merges: 0,
                duplicate_bytes: 0,
            })
            .pairs += 1;
        pair_out.push(PairOut {
            a: cands[p.a].id.clone(),
            b: cands[p.b].id.clone(),
            class: p.verdict.class,
            score: p.verdict.score,
            distance: p.verdict.distance,
            time: p.verdict.time,
            dims: p.verdict.dims,
            same_name_size: p.verdict.same_name_size,
            a_label: cands[p.a].label.clone(),
            b_label: cands[p.b].label.clone(),
            time_delta_s: cands[p.a]
                .taken_ms
                .zip(cands[p.b].taken_ms)
                .map(|(x, y)| (x - y) / 1000),
            bytes: [cands[p.a].bytes, cands[p.b].bytes],
            pixels: [cands[p.a].pixels(), cands[p.b].pixels()],
        });
    }
    let mut merge_scores = vec![0u64; 10];
    let mut merge_bytes = 0;
    for m in &merges {
        let cs = classes.get_mut(m.kind.as_str()).expect("class");
        cs.merges += 1;
        let (di, si) = (index[m.duplicate.as_str()], index[m.survivor.as_str()]);
        let dup_bytes = if cands[di].hash == cands[si].hash {
            0
        } else {
            m.bytes
        };
        cs.duplicate_bytes += dup_bytes;
        merge_bytes += dup_bytes;
        merge_scores[((m.score * 10.0) as usize).min(9)] += 1;
        let key = (sources_of(di, si), m.kind);
        if let Some(s) = sp.get_mut(&key) {
            s.merges += 1;
            s.duplicate_bytes += dup_bytes;
        }
    }
    let mut by_source_pair: Vec<SourcePairStats> = sp.into_values().collect();
    by_source_pair.sort_by(|a, b| b.pairs.cmp(&a.pairs).then(a.sources.cmp(&b.sources)));
    // Samples: the best-scoring pairs of each class in turn, so every class shows up.
    let mut samples: Vec<PairOut> = Vec::new();
    let order = [
        Class::SameReencoded,
        Class::ResizedCopy,
        Class::BurstNeighbor,
        Class::Exact,
    ];
    let by_class: Vec<Vec<&PairOut>> = order
        .iter()
        .map(|c| pair_out.iter().filter(|p| p.class == *c).collect())
        .collect();
    let mut round = 0;
    while samples.len() < 20 && by_class.iter().any(|v| round < v.len()) {
        for v in &by_class {
            if let (Some(p), true) = (v.get(round * 3), samples.len() < 20) {
                samples.push((*p).clone());
            }
        }
        round += 1;
    }
    let hashed = cands.iter().filter(|c| c.hashes.is_some()).count() as u64;
    Report {
        version: REPORT_VERSION,
        generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        thresholds: th.clone(),
        library: LibraryStats {
            photos: cands.len() as u64,
            hashed,
            flat: cands
                .iter()
                .filter(|c| c.hashes.is_some_and(|h| h.flat))
                .count() as u64,
            without_hash: cands.len() as u64 - hashed,
        },
        classes,
        by_source_pair,
        merge_total: merges.len() as u64,
        merge_duplicate_bytes: merge_bytes,
        merge_scores,
        calibration,
        samples,
        merges,
        pairs: pair_out,
    }
}

// ---- hashes, lazily --------------------------------------------------------------------------

/// The hash table, in the catalog (idempotent) or in a scratch database.
pub const HASH_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS item_hashes (
    item_id TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL,
    phash INTEGER NOT NULL,
    dhash INTEGER NOT NULL,
    thumb_w INTEGER NOT NULL,
    thumb_h INTEGER NOT NULL,
    flat INTEGER NOT NULL,
    computed_at TEXT NOT NULL
);";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HashStats {
    pub photos: usize,
    pub kept: usize,
    pub computed: usize,
    pub no_thumbnail: usize,
    pub failed: usize,
}

/// Hash every photo of `cat` that has no current hash in `store` (same connection for the catalog's
/// own table). `guard` runs every hundred pictures: it pauses while the machine is hot.
/// `limit` stops after that many new hashes.
pub fn hash_missing(
    cat: &Connection,
    store: &Connection,
    thumbs_root: &Path,
    limit: Option<usize>,
    guard: &mut dyn FnMut(),
) -> rusqlite::Result<HashStats> {
    store.execute_batch(HASH_SCHEMA)?;
    let mut stats = HashStats::default();
    let photos: Vec<(String, String, String)> = {
        let mut st = cat.prepare(
            "SELECT id, hash, thumb_state FROM items WHERE kind = 'photo' AND trashed_at IS NULL AND hidden = 0
             AND NOT EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = items.id)",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let have: HashMap<String, String> = {
        let mut st = store.prepare("SELECT item_id, content_hash FROM item_hashes")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    stats.photos = photos.len();
    let mut pending = 0;
    store.execute_batch("BEGIN")?;
    for (n, (id, hash, thumb_state)) in photos.iter().enumerate() {
        if have.get(id).is_some_and(|h| h == hash) {
            stats.kept += 1;
            continue;
        }
        if limit.is_some_and(|l| stats.computed >= l) {
            break;
        }
        if n % 100 == 0 {
            store.execute_batch("COMMIT; BEGIN")?;
            guard();
        }
        let path = crate::thumbs::dir_in(thumbs_root, id).join("s.jpg");
        if thumb_state != "ready" || !path.is_file() {
            stats.no_thumbnail += 1;
            continue;
        }
        match phash::hash_file(&path) {
            Ok(h) => {
                store.execute(
                    "INSERT OR REPLACE INTO item_hashes (item_id, content_hash, phash, dhash, thumb_w, thumb_h, flat, computed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        hash,
                        h.phash as i64,
                        h.dhash as i64,
                        h.width,
                        h.height,
                        h.flat as i64,
                        crate::db::now_str()
                    ],
                )?;
                stats.computed += 1;
                pending += 1;
            }
            Err(_) => stats.failed += 1,
        }
    }
    store.execute_batch("COMMIT")?;
    let _ = pending;
    Ok(stats)
}

/// The photos of the catalog as candidates, with their current hashes from `store`. Trashed and
/// hidden items (Live Photo parts, hidden duplicates) and Drive files (a file of a Drive folder stays
/// where the owner put it, copies included) are left out.
pub fn load_cands(cat: &Connection, store: &Connection) -> rusqlite::Result<Vec<Cand>> {
    store.execute_batch(HASH_SCHEMA)?;
    let mut hashes: HashMap<String, (String, Hashes)> = HashMap::new();
    {
        let mut st = store.prepare(
            "SELECT item_id, content_hash, phash, dhash, thumb_w, thumb_h, flat FROM item_hashes",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                Hashes {
                    phash: r.get::<_, i64>(2)? as u64,
                    dhash: r.get::<_, i64>(3)? as u64,
                    width: r.get(4)?,
                    height: r.get(5)?,
                    flat: r.get::<_, i64>(6)? != 0,
                },
            ))
        })?;
        for row in rows {
            let (id, ch, h) = row?;
            hashes.insert(id, (ch, h));
        }
    }
    let mut st = cat.prepare(
        "SELECT i.id, i.hash, i.bytes, i.width, i.height, i.taken_at, i.origin_name, i.source, i.device, i.asset_id,
                i.added_at, d.kind
         FROM items i LEFT JOIN devices d ON d.id = i.device
         WHERE i.kind = 'photo' AND i.trashed_at IS NULL AND i.hidden = 0
           AND NOT EXISTS (SELECT 1 FROM drive_files df WHERE df.item_id = i.id)
         ORDER BY i.id",
    )?;
    let rows = st.query_map([], |r| {
        let id: String = r.get(0)?;
        let hash: String = r.get(1)?;
        let taken: Option<String> = r.get(5)?;
        let name: Option<String> = r.get(6)?;
        let source: String = r.get(7)?;
        let kind: Option<String> = r.get(11)?;
        let hashes = hashes
            .get(&id)
            .filter(|(ch, _)| *ch == hash)
            .map(|(_, h)| *h);
        Ok(Cand {
            label: source_label(&source, kind.as_deref()),
            taken_ms: taken
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp_millis()),
            name: name.map(|n| n.to_lowercase()),
            id,
            hash,
            bytes: r.get(2)?,
            width: r.get(3)?,
            height: r.get(4)?,
            source,
            device: r.get(8)?,
            asset_id: r.get(9)?,
            added_at: r.get(10)?,
            hashes,
        })
    })?;
    rows.collect()
}

/// Measure what real copies look like: take every `step`-th hashed photo of `cands`, make copies of its
/// thumbnail the way a re-encode and a resize do, hash them and record the distance to the original's
/// hash (see [`Calibration::known_reencoded`]). Nothing is written.
pub fn calibrate_known_copies(
    cands: &[Cand],
    thumbs_root: &Path,
    step: usize,
    cal: &mut Calibration,
) {
    let step = step.max(1);
    for c in cands.iter().step_by(step) {
        let Some(h) = c.hashes.filter(|h| !h.flat) else {
            continue;
        };
        let path = crate::thumbs::dir_in(thumbs_root, &c.id).join("s.jpg");
        let Ok(img) = image::ImageReader::open(&path)
            .and_then(|r| r.with_guessed_format())
            .map_err(anyhow::Error::from)
            .and_then(|r| r.decode().map_err(anyhow::Error::from))
        else {
            continue;
        };
        cal.known_sample += 1;
        let d = |copy: &image::DynamicImage| {
            let x = phash::hash_image(copy);
            (hamming(h.phash, x.phash) + hamming(h.dhash, x.dhash)) as f32 / 2.0
        };
        for q in [30u8, 60, 90] {
            if let Some(copy) = jpeg_copy(&img, q) {
                bump(&mut cal.known_reencoded, d(&copy));
            }
        }
        for f in [2u32, 4] {
            let small = img.resize_exact(
                (img.width() / f).max(8),
                (img.height() / f).max(8),
                image::imageops::FilterType::Triangle,
            );
            if let Some(copy) = jpeg_copy(&small, 70) {
                bump(&mut cal.known_resized, d(&copy));
            }
        }
    }
}

fn jpeg_copy(img: &image::DynamicImage, quality: u8) -> Option<image::DynamicImage> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img.to_rgb8())
        .ok()?;
    image::load_from_memory_with_format(&out, image::ImageFormat::Jpeg).ok()
}

/// The package temperature of the CPU in degrees (AMD k10temp's Tctl), when the machine has the sensor.
pub fn cpu_temp_c() -> Option<f64> {
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("name")).ok()?.trim() == "k10temp" {
            let raw = std::fs::read_to_string(dir.join("temp1_input")).ok()?;
            return raw.trim().parse::<f64>().ok().map(|m| m / 1000.0);
        }
    }
    None
}

/// Block while the CPU is at or above `max_c` (the owner's limit: 90 °C).
pub fn wait_until_cool(max_c: f64) {
    while cpu_temp_c().is_some_and(|t| t >= max_c) {
        std::thread::sleep(std::time::Duration::from_secs(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phash::testing::*;

    fn cand(id: &str, img: &image::DynamicImage, taken_ms: Option<i64>) -> Cand {
        Cand {
            id: id.into(),
            hash: format!("hash-{id}"),
            bytes: 1000,
            width: Some(img.width() as i64),
            height: Some(img.height() as i64),
            taken_ms,
            name: None,
            source: "src-a".into(),
            device: None,
            asset_id: None,
            label: "a".into(),
            added_at: "2026-01-01T00:00:00.000Z".into(),
            hashes: Some(phash::hash_image(img)),
        }
    }

    fn other(mut c: Cand, source: &str) -> Cand {
        c.source = source.into();
        c.label = source.into();
        c
    }

    const T: i64 = 1_700_000_000_000;

    #[test]
    fn a_recompressed_copy_taken_at_the_same_moment_is_the_same_photo() {
        let img = scene(1, 640, 480);
        let a = cand("a", &img, Some(T));
        let b = other(cand("b", &recompress(&img, 45), Some(T + 400)), "src-b");
        let v = classify(&a, &b, &Thresholds::default()).unwrap();
        assert_eq!(v.class, Class::SameReencoded);
        assert!(v.score > 0.7, "{v:?}");
        assert_eq!(v.time, TimeRel::Near);
        assert_eq!(v.dims, DimsRel::Same);
    }

    #[test]
    fn a_smaller_copy_is_a_resized_copy_even_turned_a_quarter() {
        let img = scene(2, 800, 600);
        let small = img.resize_exact(400, 300, image::imageops::FilterType::Lanczos3);
        let a = cand("a", &img, Some(T));
        let b = other(cand("b", &recompress(&small, 80), Some(T)), "src-b");
        let v = classify(&a, &b, &Thresholds::default()).unwrap();
        assert_eq!(v.class, Class::ResizedCopy);
        assert_eq!(v.time, TimeRel::Equal);
        // Stored dimensions that were not turned upright (portrait file, landscape EXIF) still agree.
        let mut turned = b.clone();
        std::mem::swap(&mut turned.width, &mut turned.height);
        assert_eq!(
            classify(&a, &turned, &Thresholds::default()).unwrap().class,
            Class::ResizedCopy
        );
    }

    #[test]
    fn two_different_pictures_with_one_timestamp_are_not_duplicates() {
        let a = cand("a", &scene(3, 640, 480), Some(T));
        let b = other(cand("b", &scene(4, 640, 480), Some(T)), "src-b");
        assert_eq!(classify(&a, &b, &Thresholds::default()), None);
    }

    #[test]
    fn a_burst_neighbour_is_similar_but_not_a_duplicate() {
        // The next frame of a burst: the scene moved a few pixels.
        let a = cand("a", &shifted(5, 640, 480, 0), Some(T));
        let mut b = other(cand("b", &shifted(5, 640, 480, 14), Some(T + 120)), "src-b");
        b.hash = "other-bytes".into();
        let v = classify(&a, &b, &Thresholds::default()).unwrap();
        // Alike enough to be seen, and not a duplicate however close the hashes: the frames moved.
        assert!(v.distance.unwrap() > 0.0);
        // Whatever the class, two frames of one device are never merged.
        let mut a2 = a.clone();
        let mut b2 = b.clone();
        for (c, n) in [(&mut a2, "IMG_1"), (&mut b2, "IMG_2")] {
            c.device = Some("phone".into());
            c.asset_id = Some(n.into());
            c.source = "phone-ios".into();
        }
        let v2 = classify(&a2, &b2, &Thresholds::default()).unwrap();
        assert_eq!(v2.class, Class::BurstNeighbor, "{v:?} / {v2:?}");
        assert!(known_distinct(&a2, &b2));
    }

    #[test]
    fn two_differently_named_files_of_one_collection_are_never_duplicates() {
        let img = scene(6, 640, 480);
        let mut a = cand("a", &img, None);
        let mut b = cand("b", &recompress(&img, 90), None);
        a.name = Some("img_1.jpg".into());
        b.name = Some("img_2.jpg".into());
        assert_eq!(classify(&a, &b, &Thresholds::default()), None);
        // The same name in two collections is the same file.
        b.name = a.name.clone();
        let b = other(b, "src-b");
        assert_eq!(
            classify(&a, &b, &Thresholds::default()).unwrap().class,
            Class::SameReencoded
        );
    }

    #[test]
    fn a_copy_without_a_capture_time_needs_a_near_identical_picture() {
        let img = scene(7, 640, 480);
        let a = cand("a", &img, Some(T));
        let exact = other(cand("b", &recompress(&img, 85), None), "chat");
        let v = classify(&a, &exact, &Thresholds::default()).unwrap();
        assert_eq!(v.class, Class::SameReencoded);
        assert_eq!(v.time, TimeRel::Unknown);
        let different = other(cand("c", &scene(8, 640, 480), None), "chat");
        assert_eq!(classify(&a, &different, &Thresholds::default()), None);
        // Without time or name evidence a middling resemblance is not enough.
        let shifted_copy = other(cand("d", &shifted(7, 640, 480, 40), None), "chat");
        let sv = classify(&a, &shifted_copy, &Thresholds::default());
        assert!(
            sv.as_ref().is_none_or(|v| !v.class.is_duplicate()),
            "{sv:?}"
        );
    }

    #[test]
    fn a_time_zone_read_differently_still_counts_as_the_moment() {
        let th = Thresholds::default();
        let img = scene(9, 320, 240);
        let a = cand("a", &img, Some(T));
        let b = cand("b", &img, Some(T + 2 * 3_600_000 + 500));
        assert_eq!(time_rel(&a, &b, &th), TimeRel::ZoneShift);
        let c = cand("c", &img, Some(T + 2 * 3_600_000 + 60_000));
        assert_eq!(time_rel(&a, &c, &th), TimeRel::Far);
        assert_eq!(time_rel(&a, &cand("d", &img, None), &th), TimeRel::Unknown);
    }

    #[test]
    fn flat_pictures_need_name_or_moment_to_match() {
        let blank = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            200,
            150,
            image::Rgb([240, 240, 240]),
        ));
        let a = cand("a", &blank, None);
        let b = other(cand("b", &blank, None), "chat");
        assert_eq!(classify(&a, &b, &Thresholds::default()), None);
    }

    #[test]
    fn class_names_agree_in_json_and_in_text() {
        for c in [
            Class::Exact,
            Class::SameReencoded,
            Class::ResizedCopy,
            Class::BurstNeighbor,
        ] {
            assert_eq!(
                serde_json::to_string(&c).unwrap(),
                format!("\"{}\"", c.as_str())
            );
            assert_eq!(Class::parse(c.as_str()), Some(c));
        }
        assert_eq!(Class::parse("same_reencoded"), None);
    }

    #[test]
    fn exact_bytes_are_exact() {
        let img = scene(10, 100, 100);
        let a = cand("a", &img, None);
        let mut b = cand("b", &img, None);
        b.hash = a.hash.clone();
        assert_eq!(
            classify(&a, &b, &Thresholds::default()).unwrap().class,
            Class::Exact
        );
    }

    #[test]
    fn the_survivor_is_the_device_original_with_the_most_pixels() {
        let img = scene(11, 400, 300);
        let mut chat = other(
            cand(
                "chat",
                &img.resize_exact(200, 150, image::imageops::FilterType::Triangle),
                None,
            ),
            "ios-share",
        );
        chat.added_at = "2026-01-01T00:00:00.000Z".into();
        let mut phone = cand("phone", &img, None);
        phone.device = Some("d".into());
        phone.asset_id = Some("A".into());
        phone.source = "phone-ios".into();
        phone.added_at = "2026-02-01T00:00:00.000Z".into();
        let mut backup = other(cand("backup", &img, None), "archive");
        backup.added_at = "2025-01-01T00:00:00.000Z".into();
        // More pixels win; at equal size the device's own asset beats an earlier backup copy.
        assert!(survives_over(&phone, &chat));
        assert!(survives_over(&phone, &backup));
        assert!(!survives_over(&backup, &phone));
        let mut bigger = backup.clone();
        bigger.width = Some(800);
        bigger.height = Some(600);
        assert!(survives_over(&bigger, &phone));
        // Same size, neither a device original: the earlier arrival.
        let mut x = backup.clone();
        x.id = "x".into();
        x.added_at = "2025-06-01T00:00:00.000Z".into();
        assert!(survives_over(&backup, &x));
    }

    #[test]
    fn the_plan_groups_a_photo_without_joining_bursts() {
        let base = shifted(12, 640, 480, 0);
        let next = shifted(12, 640, 480, 30);
        let mut a = cand("a", &base, Some(T));
        a.device = Some("d".into());
        a.asset_id = Some("A1".into());
        a.source = "phone-ios".into();
        let mut b = cand("b", &next, Some(T + 100));
        b.device = Some("d".into());
        b.asset_id = Some("A2".into());
        b.source = "phone-ios".into();
        // A backup copy of frame A only.
        let c = other(cand("c", &recompress(&base, 60), Some(T)), "backup");
        let cands = vec![a, b, c];
        let (pairs, _) = find_pairs(&cands, &Thresholds::default());
        let plan = plan_merges(&cands, &pairs);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(
            (plan[0].duplicate.as_str(), plan[0].survivor.as_str()),
            ("c", "a")
        );
        assert_eq!(plan[0].kind, Class::SameReencoded);
    }

    #[test]
    fn a_chain_of_copies_hangs_from_the_best_one() {
        let img = scene(13, 800, 600);
        let mut orig = cand("orig", &img, Some(T));
        orig.device = Some("d".into());
        orig.asset_id = Some("A".into());
        orig.source = "phone-ios".into();
        let big_copy = other(cand("copy", &recompress(&img, 70), Some(T)), "backup");
        let thumb = other(
            cand(
                "thumb",
                &img.resize_exact(400, 300, image::imageops::FilterType::Lanczos3),
                None,
            ),
            "chat",
        );
        let cands = vec![thumb, big_copy, orig];
        let (pairs, _) = find_pairs(&cands, &Thresholds::default());
        let plan = plan_merges(&cands, &pairs);
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert!(plan.iter().all(|m| m.root == "orig"));
        assert!(plan.iter().any(|m| m.duplicate == "thumb"));
        assert!(plan.iter().any(|m| m.duplicate == "copy"));
    }

    #[test]
    fn the_report_counts_classes_and_source_pairs() {
        let img = scene(14, 640, 480);
        let a = cand("a", &img, Some(T));
        let b = other(cand("b", &recompress(&img, 50), Some(T)), "src-b");
        let c = other(cand("c", &scene(15, 640, 480), Some(T + 60_000)), "src-b");
        let r = build_report(&[a, b, c], &Thresholds::default());
        assert_eq!(r.library.photos, 3);
        assert_eq!(r.classes["same_photo_reencoded"].pairs, 1);
        assert_eq!(r.classes["same_photo_reencoded"].merges, 1);
        assert_eq!(r.classes["same_photo_reencoded"].duplicate_bytes, 1000);
        assert_eq!(r.merge_total, 1);
        assert_eq!(r.by_source_pair.len(), 1);
        assert_eq!(
            r.by_source_pair[0].sources,
            ["a".to_string(), "src-b".to_string()]
        );
        assert_eq!(r.samples.len(), 1);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("hash-"), "no content hashes in a report");
    }

    #[test]
    fn known_copies_calibrate_the_thresholds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let mut cands = Vec::new();
        for seed in 1..=4u64 {
            let id = format!("itm_{seed:02}aa");
            let dir = crate::thumbs::dir_in(&root, &id);
            std::fs::create_dir_all(&dir).unwrap();
            scene(seed + 40, 360, 270).save(dir.join("s.jpg")).unwrap();
            let mut c = cand(&id, &scene(0, 8, 8), None);
            c.hashes = Some(phash::hash_file(&dir.join("s.jpg")).unwrap());
            cands.push(c);
        }
        let mut cal = Calibration::default();
        calibrate_known_copies(&cands, &root, 1, &mut cal);
        assert_eq!(cal.known_sample, 4);
        assert_eq!(cal.known_reencoded.iter().sum::<u64>(), 12);
        assert_eq!(cal.known_resized.iter().sum::<u64>(), 8);
        // Copies of a picture stay well inside the distance of unrelated pictures (about 30).
        let far: u64 = cal.known_reencoded[8..]
            .iter()
            .chain(&cal.known_resized[8..])
            .sum();
        assert_eq!(far, 0, "{:?} {:?}", cal.known_reencoded, cal.known_resized);
        // Every second picture.
        let mut half = Calibration::default();
        calibrate_known_copies(&cands, &root, 2, &mut half);
        assert_eq!(half.known_sample, 2);
    }

    #[test]
    fn hashes_are_computed_lazily_and_follow_the_content() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&tmp.path().join("catalog.db")).unwrap();
        let store = Connection::open_in_memory().unwrap();
        let root = tmp.path().join("thumbs");
        let dir = crate::thumbs::dir_in(&root, "itm_aa11");
        std::fs::create_dir_all(&dir).unwrap();
        scene(16, 120, 90).save(dir.join("s.jpg")).unwrap();
        db.with(|cat| {
            cat.execute("INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h1', 1, 'jpg', 'store', 'now')", [])?;
            for (id, thumb) in [("itm_aa11", "ready"), ("itm_bb22", "ready"), ("itm_cc33", "none")] {
                cat.execute(
                    "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, thumb_state, added_at, updated_at)
                     VALUES (?1, 'photo', ?1, 'h1', 'image/jpeg', 1, 'x', 'phone-ios', ?2, 'x', 'x')",
                    params![id, thumb],
                )?;
            }
            // itm_bb22 says ready but its file is missing.
            let mut guard_calls = 0;
            let s = hash_missing(cat, &store, &root, None, &mut || guard_calls += 1)?;
            assert_eq!((s.photos, s.computed, s.no_thumbnail, s.kept), (3, 1, 2, 0));
            let s2 = hash_missing(cat, &store, &root, None, &mut || {})?;
            assert_eq!((s2.computed, s2.kept), (0, 1));
            let cands = load_cands(cat, &store)?;
            assert_eq!(cands.iter().filter(|c| c.hashes.is_some()).count(), 1);
            // New bytes under the same id: the stored hash no longer applies.
            cat.execute("INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h2', 1, 'jpg', 'store', 'now')", [])?;
            cat.execute("UPDATE items SET hash = 'h2' WHERE id = 'itm_aa11'", [])?;
            assert_eq!(load_cands(cat, &store)?.iter().filter(|c| c.hashes.is_some()).count(), 0);
            let s3 = hash_missing(cat, &store, &root, None, &mut || {})?;
            assert_eq!(s3.computed, 1);
            // A Drive file is not a candidate.
            cat.execute(
                "INSERT INTO drive_files (item_id, folder, path, dir, name, ext, doc_type, bytes, mtime, mtime_ns)
                 VALUES ('itm_cc33', 'Docs', 'a.png', '', 'a.png', 'png', 'image', 1, 'x', 0)",
                [],
            )?;
            assert!(load_cands(cat, &store)?.iter().all(|c| c.id != "itm_cc33"));
            // The same table lives in the catalog too: hashing into the catalog itself works.
            let own = hash_missing(cat, cat, &root, None, &mut || {})?;
            assert_eq!(own.computed, 1);
            Ok(())
        })
        .unwrap();
    }
}

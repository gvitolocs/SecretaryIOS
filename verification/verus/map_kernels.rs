// In-app maps kernels (protocol 15, docs/verification.md): the coordinate rule, the bounds a
// map fits to, the asset path check, the "≈" travel time and the place block's keep/drop rule.
//
// Production: crates/secretary-maps/src/kernel.rs (the reference implementation; lucina-rs
// and the web client mirror normalize_place).
//   coord_valid_spec / coord_valid  <-> kernel::coord_valid (and parse_coord after rounding to e7)
//   Bounds / fit_bounds             <-> kernel::Bounds, kernel::fit_bounds
//   safe_spec / safe_asset_path     <-> kernel::safe_asset_path on the path's UTF-8 bytes
//                                      (axum has already percent-decoded it)
//   travel_spec / travel_minutes    <-> kernel::travel_minutes (kernel::travel_estimate_min
//                                      rounds the f64 distance up to whole metres, then calls it)
//   keep_spec / coord_pair / place_keep <-> kernel::coord_pair, kernel::place_keep (the decision
//                                      inside kernel::normalize_place: title non-empty after
//                                      one_line + clip, address likewise, lat/lng as e7)
//
// Coordinates are integers in degrees × 10^7 (e7). The antimeridian is not special: longitudes
// are plain integers in [-180e7, 180e7], so fit_bounds over points on both sides of ±180° spans
// the globe (the maps cover Denmark).
//
// Refinement boundaries (covered by crates/secretary-maps/tests/kernel_model.rs, which runs the
// real functions against a plain-Rust copy of these specs on fixed-seed pseudo-random inputs):
// f64 -> e7 rounding, the haversine distance (f64), string cleaning and clipping, JSON.
//
// Proven, for all inputs:
//   MK1 coord_valid returns exactly coord_valid_spec (|lat| <= 90e7, |lng| <= 180e7, not (0,0)).
//   MK2 fit_bounds over valid points: None iff the Vec is empty; otherwise min <= max on both
//       axes, every point lies inside, each bound is attained by some point (tight), and the
//       bounds are themselves in range.
//   MK3 safe_asset_path returns exactly safe_spec; lemma_safe_rejects_traversal: an accepted
//       path has no ".." segment, no "." segment, no '\\', NUL or '%', and does not start with
//       '/'; equivalently every path with a ".." segment or a leading '/' is rejected.
//   MK4 travel_minutes returns travel_spec (ceil(78 d / speed) with d clamped to 1e8 m, at
//       least 1) without overflow; lemma_travel_monotone: non-decreasing in d for each mode;
//       lemma_travel_examples pins the contract speeds (3920 m: 64 / 21 / 9 min).
//   MK5 place_keep keeps exactly when keep_spec (title non-empty && (address non-empty || both
//       coordinates present and valid)); a kept block has both coordinates or neither, and its
//       coordinates are the input pair, valid.
//
//   MK6 simplify <-> kernel::simplify: subsequence, endpoints kept for max >= 2, len <= max.
//   MK7 leave_at <-> kernel::leave_at: saturating arrive - (duration + buffer)*1000, <= arrive.
// No assume, admit or external_body.
use vstd::arithmetic::div_mod::*;
use vstd::prelude::*;

verus! {

// ------------------------------------------------------------------ MK1 coordinates
pub open spec fn lat_max() -> int {
    900_000_000
}

pub open spec fn lng_max() -> int {
    1_800_000_000
}

pub open spec fn coord_valid_spec(lat: int, lng: int) -> bool {
    -lat_max() <= lat <= lat_max() && -lng_max() <= lng <= lng_max() && !(lat == 0 && lng == 0)
}

pub fn coord_valid(lat: i64, lng: i64) -> (r: bool)
    ensures
        r == coord_valid_spec(lat as int, lng as int),
{
    -900_000_000 <= lat && lat <= 900_000_000 && -1_800_000_000 <= lng && lng <= 1_800_000_000
        && !(lat == 0 && lng == 0)
}

// ------------------------------------------------------------------ MK2 bounds
pub struct Bounds {
    pub min_lat: i64,
    pub max_lat: i64,
    pub min_lng: i64,
    pub max_lng: i64,
}

pub open spec fn inside(b: Bounds, p: (i64, i64)) -> bool {
    b.min_lat <= p.0 <= b.max_lat && b.min_lng <= p.1 <= b.max_lng
}

pub open spec fn all_valid(points: Seq<(i64, i64)>) -> bool {
    forall|i: int| #![trigger points[i]] 0 <= i < points.len() ==> coord_valid_spec(points[i].0 as int, points[i].1 as int)
}

pub open spec fn tight(b: Bounds, points: Seq<(i64, i64)>, n: int) -> bool {
    (exists|i: int| #![trigger points[i]] 0 <= i < n && points[i].0 == b.min_lat)
    && (exists|i: int| #![trigger points[i]] 0 <= i < n && points[i].0 == b.max_lat)
    && (exists|i: int| #![trigger points[i]] 0 <= i < n && points[i].1 == b.min_lng)
    && (exists|i: int| #![trigger points[i]] 0 <= i < n && points[i].1 == b.max_lng)
}

pub open spec fn bounds_ok(b: Bounds, points: Seq<(i64, i64)>) -> bool {
    &&& b.min_lat <= b.max_lat
    &&& b.min_lng <= b.max_lng
    &&& forall|i: int| #![trigger points[i]] 0 <= i < points.len() ==> inside(b, points[i])
    &&& tight(b, points, points.len() as int)
    &&& -lat_max() <= b.min_lat && b.max_lat <= lat_max()
    &&& -lng_max() <= b.min_lng && b.max_lng <= lng_max()
}

pub fn fit_bounds(points: &Vec<(i64, i64)>) -> (r: Option<Bounds>)
    requires
        all_valid(points@),
    ensures
        r.is_none() <==> points.len() == 0,
        r matches Some(b) ==> bounds_ok(b, points@),
{
    if points.len() == 0 {
        return None;
    }
    let p0 = points[0];
    let mut b = Bounds { min_lat: p0.0, max_lat: p0.0, min_lng: p0.1, max_lng: p0.1 };
    let mut i: usize = 1;
    while i < points.len()
        invariant
            1 <= i <= points.len(),
            all_valid(points@),
            b.min_lat <= b.max_lat,
            b.min_lng <= b.max_lng,
            forall|j: int| #![trigger points@[j]] 0 <= j < i ==> inside(b, points@[j]),
            tight(b, points@, i as int),
            -lat_max() <= b.min_lat && b.max_lat <= lat_max(),
            -lng_max() <= b.min_lng && b.max_lng <= lng_max(),
        decreases points.len() - i,
    {
        let p = points[i];
        assert(coord_valid_spec(points@[i as int].0 as int, points@[i as int].1 as int));
        if p.0 < b.min_lat {
            b.min_lat = p.0;
        }
        if p.0 > b.max_lat {
            b.max_lat = p.0;
        }
        if p.1 < b.min_lng {
            b.min_lng = p.1;
        }
        if p.1 > b.max_lng {
            b.max_lng = p.1;
        }
        i += 1;
    }
    Some(b)
}

// ------------------------------------------------------------------ MK3 asset paths
// Bytes: '.' = 46, '/' = 47, '%' = 37, '\\' = 92, NUL = 0, DEL = 127.
pub open spec fn bad_byte(c: u8) -> bool {
    c < 32 || c == 127 || c == 92 || c == 37
}

/// The checks at position i (the exec loop does exactly these, left to right).
pub open spec fn ok_at(s: Seq<u8>, i: int) -> bool {
    &&& !bad_byte(s[i])
    &&& !(i > 0 && s[i - 1] == 46 && s[i] == 46)
    &&& !(i > 0 && s[i - 1] == 47 && s[i] == 47)
    &&& !(s[i] == 46 && (i == 0 || s[i - 1] == 47) && (i + 1 == s.len() || s[i + 1] == 47))
}

pub open spec fn safe_spec(s: Seq<u8>) -> bool {
    &&& 1 <= s.len() <= 512
    &&& s[0] != 47
    &&& s[s.len() - 1] != 47
    &&& forall|i: int| 0 <= i < s.len() ==> ok_at(s, i)
}

/// A ".." path segment starting at i.
pub open spec fn dotdot_segment_at(s: Seq<u8>, i: int) -> bool {
    &&& 0 <= i && i + 2 <= s.len()
    &&& s[i] == 46 && s[i + 1] == 46
    &&& (i == 0 || s[i - 1] == 47)
    &&& (i + 2 == s.len() || s[i + 2] == 47)
}

pub open spec fn dot_segment_at(s: Seq<u8>, i: int) -> bool {
    &&& 0 <= i < s.len()
    &&& s[i] == 46
    &&& (i == 0 || s[i - 1] == 47)
    &&& (i + 1 == s.len() || s[i + 1] == 47)
}

pub fn safe_asset_path(s: &[u8]) -> (r: bool)
    ensures
        r == safe_spec(s@),
{
    let n = s.len();
    if n == 0 || n > 512 || s[0] == 47 || s[n - 1] == 47 {
        return false;
    }
    let mut i: usize = 0;
    while i < n
        invariant
            n == s@.len(),
            1 <= n <= 512,
            s@[0] != 47,
            s@[n - 1] != 47,
            0 <= i <= n,
            forall|j: int| 0 <= j < i ==> ok_at(s@, j),
        decreases n - i,
    {
        let c = s[i];
        if c < 32 || c == 127 || c == 92 || c == 37 {
            assert(!ok_at(s@, i as int));
            return false;
        }
        if i > 0 && s[i - 1] == 46 && c == 46 {
            assert(!ok_at(s@, i as int));
            return false;
        }
        if i > 0 && s[i - 1] == 47 && c == 47 {
            assert(!ok_at(s@, i as int));
            return false;
        }
        if c == 46 && (i == 0 || s[i - 1] == 47) && (i + 1 == n || s[i + 1] == 47) {
            assert(!ok_at(s@, i as int));
            return false;
        }
        assert(ok_at(s@, i as int));
        i += 1;
    }
    true
}

pub proof fn lemma_safe_rejects_traversal(s: Seq<u8>)
    ensures
        safe_spec(s) ==> {
            &&& s[0] != 47
            &&& forall|i: int| !dotdot_segment_at(s, i)
            &&& forall|i: int| !dot_segment_at(s, i)
            &&& forall|i: int| 0 <= i < s.len() ==> s[i] != 92 && s[i] != 0 && s[i] != 37
        },
        (exists|i: int| dotdot_segment_at(s, i)) ==> !safe_spec(s),
        (s.len() > 0 && s[0] == 47) ==> !safe_spec(s),
{
    if safe_spec(s) {
        assert forall|i: int| !dotdot_segment_at(s, i) by {
            if dotdot_segment_at(s, i) {
                assert(!ok_at(s, i + 1));
            }
        }
        assert forall|i: int| !dot_segment_at(s, i) by {
            if dot_segment_at(s, i) {
                assert(!ok_at(s, i));
            }
        }
        assert forall|i: int| 0 <= i < s.len() implies s[i] != 92 && s[i] != 0 && s[i] != 37 by {
            assert(ok_at(s, i));
        }
    }
}

/// The examples of the unit tests: "../etc/passwd" and "/etc/passwd" are refused.
pub proof fn lemma_path_examples() {
    let up = seq![46u8, 46u8, 47u8, 101u8, 116u8, 99u8];  // "../etc"
    assert(dotdot_segment_at(up, 0));
    lemma_safe_rejects_traversal(up);
    assert(!safe_spec(up));
    let abs = seq![47u8, 101u8, 116u8, 99u8];  // "/etc"
    lemma_safe_rejects_traversal(abs);
    assert(!safe_spec(abs));
    let font = seq![102u8, 47u8, 48u8, 46u8, 112u8];  // "f/0.p"
    assert(ok_at(font, 0) && ok_at(font, 1) && ok_at(font, 2) && ok_at(font, 3) && ok_at(font, 4));
    assert(safe_spec(font));
}

// ------------------------------------------------------------------ MK4 travel estimate
pub enum Mode {
    Walk,
    Bike,
    Car,
}

/// Metres per hour: walk 4.8, bike 15, car 35 km/h.
pub open spec fn speed(m: Mode) -> int {
    match m {
        Mode::Walk => 4_800,
        Mode::Bike => 15_000,
        Mode::Car => 35_000,
    }
}

pub open spec fn max_d() -> int {
    100_000_000
}

pub open spec fn clamp(d: int) -> int {
    if d > max_d() { max_d() } else { d }
}

/// ceil(d × 1.3 × 60 / speed) = ceil(78 d / speed), at least one minute.
pub open spec fn travel_spec(d: nat, m: Mode) -> int {
    let q = (78 * clamp(d as int) + speed(m) - 1) / speed(m);
    if q < 1 { 1 } else { q }
}

pub fn travel_minutes(d: u64, m: Mode) -> (r: u64)
    ensures
        r == travel_spec(d as nat, m),
        r >= 1,
{
    let d = if d > 100_000_000 { 100_000_000 } else { d };
    let s: u64 = match m {
        Mode::Walk => 4_800,
        Mode::Bike => 15_000,
        Mode::Car => 35_000,
    };
    assert(s as int == speed(m));
    let q = (78 * d + s - 1) / s;
    if q < 1 { 1 } else { q }
}

pub proof fn lemma_travel_monotone(d1: nat, d2: nat, m: Mode)
    requires
        d1 <= d2,
    ensures
        travel_spec(d1, m) <= travel_spec(d2, m),
        travel_spec(d1, m) >= 1,
{
    let s = speed(m);
    assert(clamp(d1 as int) <= clamp(d2 as int));
    lemma_div_is_ordered(78 * clamp(d1 as int) + s - 1, 78 * clamp(d2 as int) + s - 1, s);
}

pub proof fn lemma_travel_examples() {
    assert(travel_spec(3920, Mode::Walk) == 64);
    assert(travel_spec(3920, Mode::Bike) == 21);
    assert(travel_spec(3920, Mode::Car) == 9);
    assert(travel_spec(0, Mode::Walk) == 1);
}

// ------------------------------------------------------------------ MK5 place validity
pub open spec fn both_valid(lat: Option<i64>, lng: Option<i64>) -> bool {
    match (lat, lng) {
        (Some(a), Some(b)) => coord_valid_spec(a as int, b as int),
        _ => false,
    }
}

pub open spec fn keep_spec(title: bool, address: bool, lat: Option<i64>, lng: Option<i64>) -> bool {
    title && (address || both_valid(lat, lng))
}

pub fn coord_pair(lat: Option<i64>, lng: Option<i64>) -> (r: (Option<i64>, Option<i64>))
    ensures
        r.0.is_some() == r.1.is_some(),
        r.0.is_some() <==> both_valid(lat, lng),
        r.0.is_some() ==> r.0 == lat && r.1 == lng,
{
    match (lat, lng) {
        (Some(a), Some(b)) => {
            if coord_valid(a, b) {
                (Some(a), Some(b))
            } else {
                (None, None)
            }
        },
        _ => (None, None),
    }
}

pub fn place_keep(title: bool, address: bool, lat: Option<i64>, lng: Option<i64>) -> (r: Option<
    (Option<i64>, Option<i64>),
>)
    ensures
        r.is_some() <==> keep_spec(title, address, lat, lng),
        r matches Some(p) ==> p.0.is_some() == p.1.is_some(),
        r matches Some(p) ==> (p.0.is_some() <==> both_valid(lat, lng)),
        r matches Some(p) ==> (p.0.is_some() ==> p.0 == lat && p.1 == lng),
{
    let pair = coord_pair(lat, lng);
    if title && (address || pair.0.is_some()) {
        Some(pair)
    } else {
        None
    }
}

/// The contract's cases: title only (no address, no coordinates) is dropped; one coordinate
/// with an address keeps the block without coordinates; (0, 0) never counts.
pub proof fn lemma_place_examples() {
    assert(!keep_spec(true, false, None, None));
    assert(!keep_spec(true, false, Some(561_611_000), None));
    assert(keep_spec(true, true, Some(561_611_000), None));
    assert(!both_valid(Some(561_611_000), None));
    assert(!keep_spec(true, false, Some(0), Some(0)));
    assert(keep_spec(true, false, Some(561_611_000), Some(102_006_000)));
    assert(!keep_spec(false, true, Some(561_611_000), Some(102_006_000)));
}

// ------------------------------------------------------------------ MK6 stride simplification
// Mirrors kernel::simplify: adaptive stride ceil(remaining / slots); max < 2 => empty.
// A strictly increasing index witness is the definition of subsequence (including duplicates).
pub open spec fn indexed_subsequence(p: Seq<(i64,i64)>, r: Seq<(i64,i64)>, ix: Seq<int>) -> bool {
    &&& ix.len() == r.len()
    &&& forall|j:int| 0 <= j < ix.len() ==> 0 <= ix[j] < p.len() && r[j] == p[ix[j]]
    &&& forall|a:int,b:int| 0 <= a < b < ix.len() ==> ix[a] < ix[b]
}

pub fn simplify(p: &Vec<(i64,i64)>, max: usize) -> (r: Vec<(i64,i64)>)
    ensures
        r.len() <= max,
        exists|ix: Seq<int>| indexed_subsequence(p@,r@,ix),
        max >= 2 && p.len() > 0 ==> r.len() > 0 && r@[0] == p@[0] && r@[r.len()-1] == p@[p.len()-1],
{
    let mut out = Vec::new();
    if max < 2 || p.len() == 0 {
        assert(indexed_subsequence(p@,out@,Seq::<int>::empty()));
        return out;
    }
    let last = p.len() - 1;
    out.push(p[0]);
    let ghost mut ix = seq![0int];
    let mut i: usize = 0;
    let mut slots = max - 1;
    while i < last
        invariant
            last == p.len() - 1,
            p.len() > 0, max >= 2,
            i <= last,
            out.len() > 0,
            out.len() + slots == max,
            i < last ==> slots > 0,
            indexed_subsequence(p@,out@,ix),
            ix[0] == 0, ix[ix.len()-1] == i,
            out@[0] == p@[0], out@[out.len()-1] == p@[i as int],
        decreases last - i,
    {
        let remaining = last - i;
        let step = (remaining - 1) / slots + 1;
        proof {
            if remaining > 1 && slots > 1 { lemma_div_decreases((remaining - 1) as int, slots as int); }
            assert(0 <= (remaining - 1) as int / slots as int <= remaining - 1);
            if slots == 1 { assert(step == remaining); }
        }
        i = i + step;
        out.push(p[i]);
        proof {
            let oldix = ix;
            ix = ix.push(i as int);
            assert forall|a:int,b:int| 0 <= a < b < ix.len() implies ix[a] < ix[b] by {
                if b == oldix.len() {
                    if a < oldix.len()-1 { assert(oldix[a] < oldix[oldix.len()-1]); }
                }
            }
            assert(indexed_subsequence(p@,out@,ix));
        }
        slots = slots - 1;
    }
    assert(exists|w:Seq<int>| indexed_subsequence(p@,out@,w)) by {
        assert(indexed_subsequence(p@,out@,ix));
    }
    out
}

// ------------------------------------------------------------------ MK7 leave time
// Mirrors kernel::leave_at: i128 intermediate, seconds -> milliseconds, lower saturation.
pub open spec fn leave_spec(a:int,d:int,b:int) -> int {
    let x = a - (d+b)*1000;
    if x < i64::MIN { i64::MIN as int } else { x }
}
pub fn leave_at(a:i64,d:u64,b:u64) -> (r:i64)
    ensures r as int == leave_spec(a as int,d as int,b as int), r <= a,
{
    let x = (a as i128) - ((d as i128)+(b as i128))*1000;
    if x < i64::MIN as i128 { i64::MIN } else { x as i64 }
}

fn main() {
}

} // verus!

//! Differential test: the kernels of `src/kernel.rs` against a plain-Rust copy of the specs
//! proven in `verification/verus/map_kernels.rs` (MK1–MK5), on fixed-seed pseudo-random inputs
//! biased toward the boundaries. The spec functions below keep the Verus names.
use secretary_maps::kernel::{self, Bounds, Mode};
use serde_json::{json, Map, Value};

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }
    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
    /// An e7 value around `max`: the edges, 0, or anything in [-2 max, 2 max].
    fn e7(&mut self, max: i64) -> i64 {
        match self.below(4) {
            0 => self.pick(&[0, 1, -1, max, -max, max + 1, -max - 1, i64::MAX, i64::MIN]),
            1 => max - self.below(3) as i64 + 1,
            _ => (self.below(4 * max as u64 + 1) as i64) - 2 * max,
        }
    }
}

// ---- the Verus specs, copied
const LAT_MAX: i64 = 900_000_000;
const LNG_MAX: i64 = 1_800_000_000;

fn coord_valid_spec(lat: i64, lng: i64) -> bool {
    let (lat, lng) = (lat as i128, lng as i128);
    -(LAT_MAX as i128) <= lat
        && lat <= LAT_MAX as i128
        && -(LNG_MAX as i128) <= lng
        && lng <= LNG_MAX as i128
        && !(lat == 0 && lng == 0)
}

fn bounds_ok(b: &Bounds, pts: &[(i64, i64)]) -> bool {
    b.min_lat <= b.max_lat
        && b.min_lng <= b.max_lng
        && pts
            .iter()
            .all(|&(a, o)| b.min_lat <= a && a <= b.max_lat && b.min_lng <= o && o <= b.max_lng)
        && pts.iter().any(|p| p.0 == b.min_lat)
        && pts.iter().any(|p| p.0 == b.max_lat)
        && pts.iter().any(|p| p.1 == b.min_lng)
        && pts.iter().any(|p| p.1 == b.max_lng)
        && -LAT_MAX <= b.min_lat
        && b.max_lat <= LAT_MAX
        && -LNG_MAX <= b.min_lng
        && b.max_lng <= LNG_MAX
}

fn bad_byte(c: u8) -> bool {
    c < 32 || c == 127 || c == 92 || c == 37
}

fn ok_at(s: &[u8], i: usize) -> bool {
    !bad_byte(s[i])
        && !(i > 0 && s[i - 1] == 46 && s[i] == 46)
        && !(i > 0 && s[i - 1] == 47 && s[i] == 47)
        && !(s[i] == 46 && (i == 0 || s[i - 1] == 47) && (i + 1 == s.len() || s[i + 1] == 47))
}

fn safe_spec(s: &[u8]) -> bool {
    (1..=512).contains(&s.len())
        && s[0] != 47
        && s[s.len() - 1] != 47
        && (0..s.len()).all(|i| ok_at(s, i))
}

fn dotdot_segment(s: &[u8]) -> bool {
    (0..s.len().saturating_sub(1)).any(|i| {
        s[i] == 46
            && s[i + 1] == 46
            && (i == 0 || s[i - 1] == 47)
            && (i + 2 == s.len() || s[i + 2] == 47)
    })
}

fn speed(m: Mode) -> i128 {
    match m {
        Mode::Walk => 4_800,
        Mode::Bike => 15_000,
        Mode::Car => 35_000,
    }
}

fn travel_spec(d: u64, m: Mode) -> i128 {
    let d = (d as i128).min(100_000_000);
    let q = (78 * d + speed(m) - 1) / speed(m);
    q.max(1)
}

fn both_valid(lat: Option<i64>, lng: Option<i64>) -> bool {
    matches!((lat, lng), (Some(a), Some(b)) if coord_valid_spec(a, b))
}

fn keep_spec(title: bool, address: bool, lat: Option<i64>, lng: Option<i64>) -> bool {
    title && (address || both_valid(lat, lng))
}

// ---- the comparisons
#[test]
fn mk1_coord_valid() {
    let mut r = Rng(0x6d61_7073_0001);
    for _ in 0..20_000 {
        let (lat, lng) = (r.e7(LAT_MAX), r.e7(LNG_MAX));
        assert_eq!(
            kernel::coord_valid(lat, lng),
            coord_valid_spec(lat, lng),
            "{lat},{lng}"
        );
    }
    // parse_coord = finite, round to e7, then the spec.
    for _ in 0..20_000 {
        let lat = r.e7(LAT_MAX) as f64 / 1e7 + (r.below(1000) as f64 - 500.0) * 1e-10;
        let lng = r.e7(LNG_MAX) as f64 / 1e7;
        let want = {
            let (a, b) = ((lat * 1e7).round() as i64, (lng * 1e7).round() as i64);
            coord_valid_spec(a, b).then_some((a, b))
        };
        assert_eq!(kernel::parse_coord(lat, lng), want, "{lat},{lng}");
    }
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(kernel::parse_coord(bad, 10.0), None);
        assert_eq!(kernel::parse_coord(56.0, bad), None);
    }
}

#[test]
fn mk2_fit_bounds() {
    let mut r = Rng(0x6d61_7073_0002);
    assert_eq!(kernel::fit_bounds(&[]), None);
    for _ in 0..5_000 {
        let n = r.below(12) as usize + 1;
        let pts: Vec<(i64, i64)> = (0..n)
            .map(|_| loop {
                let p = (r.e7(LAT_MAX), r.e7(LNG_MAX));
                if coord_valid_spec(p.0, p.1) {
                    break p;
                }
            })
            .collect();
        let b = kernel::fit_bounds(&pts).expect("non-empty");
        assert!(bounds_ok(&b, &pts), "{pts:?} -> {b:?}");
    }
}

#[test]
fn mk3_asset_paths() {
    let mut r = Rng(0x6d61_7073_0003);
    let alphabet = [
        '.', '/', 'a', 'z', '0', '-', '@', ' ', '%', '\\', '\0', '\u{7f}', '\n', 'é', '2',
    ];
    let mut accepted = 0;
    for _ in 0..20_000 {
        let n = r.below(10) as usize;
        let s: String = (0..n).map(|_| r.pick(&alphabet)).collect();
        let got = kernel::safe_asset_path(&s);
        assert_eq!(got, safe_spec(s.as_bytes()), "{s:?}");
        if got {
            accepted += 1;
            assert!(
                !dotdot_segment(s.as_bytes()) && !s.starts_with('/'),
                "{s:?}"
            );
        }
    }
    assert!(
        accepted > 500,
        "the generator should hit accepted paths too ({accepted})"
    );
    let long = "a/".repeat(256) + "b";
    assert_eq!(kernel::safe_asset_path(&long), safe_spec(long.as_bytes()));
}

#[test]
fn mk4_travel() {
    let mut r = Rng(0x6d61_7073_0004);
    for _ in 0..20_000 {
        let d = match r.below(3) {
            0 => r.below(200),
            1 => r.below(200_000),
            _ => r.next(),
        };
        let m = r.pick(&Mode::ALL);
        let t = kernel::travel_minutes(d, m);
        assert_eq!(t as i128, travel_spec(d, m), "{d} {m:?}");
        assert!(t >= 1);
        let d2 = d.saturating_add(r.below(10_000));
        assert!(
            kernel::travel_minutes(d2, m) >= t,
            "monotone {d} {d2} {m:?}"
        );
        // The f64 entry point rounds up to whole metres.
        let x = (d % 1_000_000) as f64 + r.below(1000) as f64 / 1000.0;
        assert_eq!(
            kernel::travel_estimate_min(x, m) as i128,
            travel_spec(x.ceil() as u64, m),
            "{x} {m:?}"
        );
    }
}

#[test]
fn mk5_place_validity() {
    let mut r = Rng(0x6d61_7073_0005);
    let opt = |r: &mut Rng, max: i64| r.coin().then(|| r.e7(max));
    for _ in 0..20_000 {
        let (t, a) = (r.coin(), r.coin());
        let (lat, lng) = (opt(&mut r, LAT_MAX), opt(&mut r, LNG_MAX));
        let pair = kernel::coord_pair(lat, lng);
        assert_eq!(pair.0.is_some(), pair.1.is_some());
        assert_eq!(pair.0.is_some(), both_valid(lat, lng));
        let kept = kernel::place_keep(t, a, lat, lng);
        assert_eq!(
            kept.is_some(),
            keep_spec(t, a, lat, lng),
            "{t} {a} {lat:?} {lng:?}"
        );
        if let Some(p) = kept {
            assert_eq!(p, pair);
        }
    }
    // The normalizer makes the same decision on JSON blocks.
    let texts = ["", "  ", "\n\t", "Folkehuset", " Aarhus\nC ", "\u{7}"];
    for _ in 0..5_000 {
        let mut o = Map::new();
        o.insert("type".into(), json!("place"));
        let title = r.coin().then(|| r.pick(&texts));
        let address = r.coin().then(|| r.pick(&texts));
        if let Some(t) = title {
            o.insert("title".into(), json!(t));
        }
        if let Some(a) = address {
            o.insert("address".into(), json!(a));
        }
        let lat = opt(&mut r, LAT_MAX);
        let lng = opt(&mut r, LNG_MAX);
        // Only finite in-range JSON numbers become e7 values (serde_json has no NaN).
        if let Some(v) = lat.filter(|v| v.unsigned_abs() <= 2 * LAT_MAX as u64) {
            o.insert("lat".into(), json!(v as f64 / 1e7));
        }
        if let Some(v) = lng.filter(|v| v.unsigned_abs() <= 2 * LNG_MAX as u64) {
            o.insert("lng".into(), json!(v as f64 / 1e7));
        }
        let e7 = |k: &str, max: i64| {
            o.get(k)
                .and_then(Value::as_f64)
                .map(|x| (x * 1e7).round() as i64)
                .filter(|v| (-max..=max).contains(v))
        };
        let (lat_e7, lng_e7) = (e7("lat", LAT_MAX), e7("lng", LNG_MAX));
        let nonempty = |s: Option<&str>| s.is_some_and(|s| !kernel::one_line(s).is_empty());
        let want = keep_spec(nonempty(title), nonempty(address), lat_e7, lng_e7);
        let out = kernel::normalize_place(&Value::Object(o.clone()));
        assert_eq!(out.is_some(), want, "{o:?}");
        if let Some(out) = out {
            assert_eq!(out.get("lat").is_some(), out.get("lng").is_some(), "{o:?}");
            assert_eq!(
                out.get("lat").is_some(),
                both_valid(lat_e7, lng_e7),
                "{o:?}"
            );
        }
    }
}

#[test]
fn mk6_mk7_routing() {
    let mut seed = 0x45abcd1234u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..5000 {
        let len = (next() % 1000) as usize;
        let max = (next() % 500) as usize;
        let points: Vec<_> = (0..len).map(|i| (i as i64, next() as i64)).collect();
        let r = secretary_maps::kernel::simplify(&points, max);
        assert!(r.len() <= max);
        if max >= 2 && len > 0 {
            assert_eq!(r.first(), points.first());
            assert_eq!(r.last(), points.last());
        }
        // The first component is a strict index witness, matching indexed_subsequence.
        assert!(r.windows(2).all(|w| w[0].0 < w[1].0));
        for p in r {
            assert_eq!(points[p.0 as usize], p);
        }
        let a = next() as i64;
        let d = next();
        let b = next();
        let exact = a as i128 - (d as i128 + b as i128) * 1000;
        assert_eq!(
            secretary_maps::kernel::leave_at(a, d, b),
            exact.max(i64::MIN as i128) as i64
        );
        assert!(secretary_maps::kernel::leave_at(a, d, b) <= a);
    }
}

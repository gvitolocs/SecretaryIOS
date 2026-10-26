//! Pure map kernels (no I/O): coordinates, bounds, distances, travel estimates, the `place`
//! block normalizer and the asset path rule.
//!
//! The verified core works on integer coordinates in degrees × 10^7 (`e7`). Its Verus model is
//! `verification/verus/map_kernels.rs` (MK1–MK5); `tests/kernel_model.rs` checks these
//! functions against that model on pseudo-random inputs.
//!
//! The antimeridian is not special: longitudes are plain numbers in [-180, 180], so a set of
//! points on both sides of ±180° gets bounds spanning the whole globe instead of the short way
//! across. The maps cover Denmark, far from it.
use serde_json::{Map, Value};

/// |lat| <= 90° in e7.
pub const LAT_MAX_E7: i64 = 900_000_000;
/// |lng| <= 180° in e7.
pub const LNG_MAX_E7: i64 = 1_800_000_000;
/// Mean Earth radius (IUGG), metres.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;
/// Distances are clamped to this many metres before estimating travel time (2.5 × the
/// equator; keeps the integer arithmetic far from overflow).
pub const MAX_TRAVEL_M: u64 = 100_000_000;
pub const TITLE_MAX_CHARS: usize = 120;
pub const ADDRESS_MAX_CHARS: usize = 200;
pub const EVENT_ID_MAX_CHARS: usize = 200;
pub const ASSET_PATH_MAX_BYTES: usize = 512;

/// MK1: the range predicate. (0, 0) ("Null Island") is what a missing value becomes in too many
/// places to be trusted as a real position.
pub fn coord_valid(lat_e7: i64, lng_e7: i64) -> bool {
    (-LAT_MAX_E7..=LAT_MAX_E7).contains(&lat_e7)
        && (-LNG_MAX_E7..=LNG_MAX_E7).contains(&lng_e7)
        && !(lat_e7 == 0 && lng_e7 == 0)
}

/// Degrees to e7 when finite, rounded to the nearest e7 and valid.
pub fn parse_coord(lat: f64, lng: f64) -> Option<(i64, i64)> {
    if !lat.is_finite() || !lng.is_finite() {
        return None;
    }
    // `as` saturates, and a saturated value is out of range.
    let (lat_e7, lng_e7) = ((lat * 1e7).round() as i64, (lng * 1e7).round() as i64);
    coord_valid(lat_e7, lng_e7).then_some((lat_e7, lng_e7))
}

pub fn e7_to_deg(v: i64) -> f64 {
    v as f64 / 1e7
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub min_lat: i64,
    pub max_lat: i64,
    pub min_lng: i64,
    pub max_lng: i64,
}

impl Bounds {
    pub fn contains(&self, (lat, lng): (i64, i64)) -> bool {
        self.min_lat <= lat && lat <= self.max_lat && self.min_lng <= lng && lng <= self.max_lng
    }
}

/// MK2: the smallest box holding every point (antimeridian not special, see the module doc).
pub fn fit_bounds(points: &[(i64, i64)]) -> Option<Bounds> {
    let (&(lat0, lng0), rest) = points.split_first()?;
    let mut b = Bounds {
        min_lat: lat0,
        max_lat: lat0,
        min_lng: lng0,
        max_lng: lng0,
    };
    for &(lat, lng) in rest {
        b.min_lat = b.min_lat.min(lat);
        b.max_lat = b.max_lat.max(lat);
        b.min_lng = b.min_lng.min(lng);
        b.max_lng = b.max_lng.max(lng);
    }
    Some(b)
}

/// Great-circle distance in metres (haversine).
pub fn distance_m(a: (i64, i64), b: (i64, i64)) -> f64 {
    let (lat1, lat2) = (e7_to_deg(a.0).to_radians(), e7_to_deg(b.0).to_radians());
    let dlat = lat2 - lat1;
    let dlng = (e7_to_deg(b.1) - e7_to_deg(a.1)).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Walk,
    Bike,
    Car,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Walk, Mode::Bike, Mode::Car];

    /// Contract speeds: walk 4.8, bike 15, car 35 km/h.
    pub fn metres_per_hour(self) -> u64 {
        match self {
            Mode::Walk => 4_800,
            Mode::Bike => 15_000,
            Mode::Car => 35_000,
        }
    }
}

/// MK4 (the integer model): minutes for `d` whole metres with the 1.3 detour factor,
/// `ceil(d × 1.3 × 60 / speed) = ceil(78 d / speed)`, at least 1.
pub fn travel_minutes(d: u64, mode: Mode) -> u64 {
    let d = d.min(MAX_TRAVEL_M);
    let s = mode.metres_per_hour();
    (d * 78).div_ceil(s).max(1)
}

/// Straight-line distance to an "≈" travel time: the distance rounded up to whole metres, then
/// [`travel_minutes`]. NaN, negative and zero distances give 1 minute.
pub fn travel_estimate_min(distance_m: f64, mode: Mode) -> u32 {
    let d = if distance_m > 0.0 {
        distance_m.min(MAX_TRAVEL_M as f64).ceil() as u64
    } else {
        0
    };
    // <= 78e8 / 4800 = 1_625_000, fits.
    travel_minutes(d, mode) as u32
}

/// MK5: an invalid or half-present pair is dropped as a whole.
pub fn coord_pair(lat: Option<i64>, lng: Option<i64>) -> (Option<i64>, Option<i64>) {
    match (lat, lng) {
        (Some(a), Some(b)) if coord_valid(a, b) => (Some(a), Some(b)),
        _ => (None, None),
    }
}

/// MK5: the keep/drop decision. Kept blocks carry both coordinates or neither.
pub fn place_keep(
    title_nonempty: bool,
    address_nonempty: bool,
    lat: Option<i64>,
    lng: Option<i64>,
) -> Option<(Option<i64>, Option<i64>)> {
    let pair = coord_pair(lat, lng);
    (title_nonempty && (address_nonempty || pair.0.is_some())).then_some(pair)
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\t'
    )
}

/// One-line text: every run of line breaks/tabs becomes one space, other control characters
/// are removed, the ends are trimmed.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_break = false;
    for c in s.chars() {
        if is_line_break(c) {
            if !in_break {
                out.push(' ');
            }
            in_break = true;
        } else if c.is_control() {
            continue;
        } else {
            out.push(c);
            in_break = false;
        }
    }
    out.trim().to_string()
}

/// At most `max` chars; a longer string keeps `max - 1` chars (trailing spaces trimmed) + "…".
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

fn text_field(obj: &Map<String, Value>, key: &str, max: usize) -> Option<String> {
    let s = clip(&one_line(obj.get(key)?.as_str()?), max);
    (!s.is_empty()).then_some(s)
}

fn rfc3339(obj: &Map<String, Value>, key: &str) -> Option<(String, time::OffsetDateTime)> {
    let s = obj.get(key)?.as_str()?.trim();
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((s.to_string(), t))
}

fn coord_e7(v: Option<&Value>, max: i64) -> Option<i64> {
    let x = v?.as_f64()?;
    if !x.is_finite() {
        return None;
    }
    let e7 = (x * 1e7).round() as i64;
    (-max..=max).contains(&e7).then_some(e7)
}

/// The contract section 1 normalizer for a `place` block (with `"type":"place"`) or a
/// proactive `meta.place` (without `type`; the output keeps `type` only when the input had it).
///
/// - `title` (required, <= 120 chars) and `address` (<= 200): one line ([`one_line`]),
///   clipped with "…" ([`clip`]); empty after that = missing.
/// - `lat`/`lng`: JSON numbers, both present and [`coord_valid`] after rounding to e7, else
///   both dropped (the address stays).
/// - `when`/`end`: RFC 3339 or dropped; `end` before `when` is dropped.
/// - `eventId`: one line, 1..=200 chars, else dropped (ids are never clipped).
/// - `kind`: "event" or "place" (default "place"). Unknown fields are dropped.
/// - Kept only when `title` is non-empty and (`address` is non-empty or both coordinates are
///   valid) ([`place_keep`]). A `type` other than "place" or a non-object is `None`.
pub fn normalize_place(v: &Value) -> Option<Value> {
    let obj = v.as_object()?;
    let typed = match obj.get("type") {
        None => false,
        Some(Value::String(t)) if t == "place" => true,
        Some(_) => return None,
    };
    let title = text_field(obj, "title", TITLE_MAX_CHARS);
    let address = text_field(obj, "address", ADDRESS_MAX_CHARS);
    let lat = coord_e7(obj.get("lat"), LAT_MAX_E7);
    let lng = coord_e7(obj.get("lng"), LNG_MAX_E7);
    let (lat, lng) = place_keep(title.is_some(), address.is_some(), lat, lng)?;

    let mut out = Map::new();
    if typed {
        out.insert("type".into(), "place".into());
    }
    out.insert("title".into(), title?.into());
    if let Some(a) = address {
        out.insert("address".into(), a.into());
    }
    if let (Some(a), Some(b)) = (lat, lng) {
        out.insert("lat".into(), e7_to_deg(a).into());
        out.insert("lng".into(), e7_to_deg(b).into());
    }
    let when = rfc3339(obj, "when");
    if let Some((s, _)) = &when {
        out.insert("when".into(), s.clone().into());
    }
    if let Some((s, end)) = rfc3339(obj, "end") {
        if when.as_ref().is_none_or(|(_, start)| end >= *start) {
            out.insert("end".into(), s.into());
        }
    }
    if let Some(id) = obj.get("eventId").and_then(Value::as_str).map(one_line) {
        if !id.is_empty() && id.chars().count() <= EVENT_ID_MAX_CHARS {
            out.insert("eventId".into(), id.into());
        }
    }
    let kind = match obj.get("kind").and_then(Value::as_str) {
        Some("event") => "event",
        _ => "place",
    };
    out.insert("kind".into(), kind.into());
    Some(Value::Object(out))
}

const DOT: u8 = b'.';
const SLASH: u8 = b'/';

fn bad_byte(c: u8) -> bool {
    c < 0x20 || c == 0x7f || c == b'\\' || c == b'%'
}

/// MK3: a relative path under the assets dir, checked on the (already percent-decoded) bytes:
/// 1..=512 bytes, no leading or trailing '/', no empty, "." or ".." segment, no ".." anywhere,
/// no backslash, NUL or other control byte, and no '%' (a decoded path still holding one was
/// encoded twice). The server still canonicalizes and checks the result stays in the dir.
pub fn safe_asset_path(path: &str) -> bool {
    let s = path.as_bytes();
    let n = s.len();
    if n == 0 || n > ASSET_PATH_MAX_BYTES || s[0] == SLASH || s[n - 1] == SLASH {
        return false;
    }
    (0..n).all(|i| {
        let c = s[i];
        let prev = if i > 0 { Some(s[i - 1]) } else { None };
        let next = s.get(i + 1).copied();
        !bad_byte(c)
            && !(prev == Some(DOT) && c == DOT)
            && !(prev == Some(SLASH) && c == SLASH)
            && !(c == DOT && prev.is_none_or(|p| p == SLASH) && next.is_none_or(|x| x == SLASH))
    })
}

/// Tile set names: `^[a-z0-9-]{1,40}$`.
pub fn valid_tiles_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coord_valid_bounds_exactly() {
        assert!(coord_valid(LAT_MAX_E7, LNG_MAX_E7));
        assert!(coord_valid(-LAT_MAX_E7, -LNG_MAX_E7));
        assert!(!coord_valid(LAT_MAX_E7 + 1, 0));
        assert!(!coord_valid(0, -LNG_MAX_E7 - 1));
        assert!(!coord_valid(0, 0));
        assert!(coord_valid(0, 1));
        assert!(coord_valid(1, 0));
        assert!(!coord_valid(i64::MIN, i64::MIN));
        assert!(!coord_valid(i64::MAX, 0));
    }

    #[test]
    fn parse_coord_cases() {
        assert_eq!(
            parse_coord(56.19633, 10.2011),
            Some((561_963_300, 102_011_000))
        );
        assert_eq!(parse_coord(90.0, 180.0), Some((LAT_MAX_E7, LNG_MAX_E7)));
        assert_eq!(parse_coord(-90.0, -180.0), Some((-LAT_MAX_E7, -LNG_MAX_E7)));
        assert_eq!(parse_coord(90.000_000_1, 0.5), None);
        assert_eq!(parse_coord(0.0, 0.0), None);
        // Rounds to (0, 0).
        assert_eq!(parse_coord(0.000_000_01, -0.000_000_04), None);
        assert_eq!(parse_coord(f64::NAN, 10.0), None);
        assert_eq!(parse_coord(56.0, f64::INFINITY), None);
        assert_eq!(parse_coord(f64::NEG_INFINITY, 10.0), None);
        assert_eq!(parse_coord(1e300, 10.0), None);
    }

    #[test]
    fn fit_bounds_cases() {
        assert_eq!(fit_bounds(&[]), None);
        let one = fit_bounds(&[(5, 7)]).unwrap();
        assert_eq!(
            one,
            Bounds {
                min_lat: 5,
                max_lat: 5,
                min_lng: 7,
                max_lng: 7
            }
        );
        let pts = [
            (561_963_300, 102_011_000),
            (561_611_000, 102_006_000),
            (-10, 1_799_999_999),
        ];
        let b = fit_bounds(&pts).unwrap();
        assert!(pts.iter().all(|&p| b.contains(p)));
        assert!(b.min_lat <= b.max_lat && b.min_lng <= b.max_lng);
        assert_eq!((b.min_lat, b.max_lng), (-10, 1_799_999_999));
    }

    #[test]
    fn antimeridian_is_not_special() {
        // 179.9°E and 179.9°W are 22 km apart, but the box spans the whole globe.
        let b = fit_bounds(&[(0, 1_799_000_000), (10, -1_799_000_000)]).unwrap();
        assert_eq!((b.min_lng, b.max_lng), (-1_799_000_000, 1_799_000_000));
        // The haversine distance is still the short way.
        let d = distance_m((0, 1_799_000_000), (0, -1_799_000_000));
        assert!((d - 22_239.0).abs() < 50.0, "{d}");
    }

    #[test]
    fn distances() {
        let home = parse_coord(56.19633, 10.2011).unwrap();
        let folkehuset = parse_coord(56.1611, 10.2006).unwrap();
        let d = distance_m(home, folkehuset);
        assert!((3_900.0..3_940.0).contains(&d), "{d}");
        assert_eq!(distance_m(home, home), 0.0);
        let pole = distance_m((LAT_MAX_E7, 0), (-LAT_MAX_E7, 0));
        assert!((pole - std::f64::consts::PI * EARTH_RADIUS_M).abs() < 1.0);
    }

    #[test]
    fn travel_estimates() {
        // 3920 m: walk 3920*78/4800 = 63.7 -> 64; bike 20.4 -> 21; car 8.7 -> 9.
        assert_eq!(travel_estimate_min(3_920.0, Mode::Walk), 64);
        assert_eq!(travel_estimate_min(3_920.0, Mode::Bike), 21);
        assert_eq!(travel_estimate_min(3_920.0, Mode::Car), 9);
        for m in Mode::ALL {
            assert_eq!(travel_estimate_min(0.0, m), 1);
            assert_eq!(travel_estimate_min(-5.0, m), 1);
            assert_eq!(travel_estimate_min(f64::NAN, m), 1);
            assert_eq!(travel_estimate_min(0.1, m), 1);
            assert_eq!(
                travel_estimate_min(f64::INFINITY, m),
                travel_minutes(MAX_TRAVEL_M, m) as u32
            );
        }
        // Exactly 1 minute of walking (4800/78 is not whole: 61.53 m -> 1, 62 m -> 2).
        assert_eq!(travel_estimate_min(61.0, Mode::Walk), 1);
        assert_eq!(travel_estimate_min(61.6, Mode::Walk), 2);
    }

    #[test]
    fn coord_pairs_and_keep() {
        assert_eq!(coord_pair(Some(1), None), (None, None));
        assert_eq!(coord_pair(None, Some(1)), (None, None));
        assert_eq!(coord_pair(Some(0), Some(0)), (None, None));
        assert_eq!(coord_pair(Some(1), Some(2)), (Some(1), Some(2)));
        assert_eq!(place_keep(false, true, Some(1), Some(2)), None);
        assert_eq!(place_keep(true, false, Some(1), None), None);
        assert_eq!(place_keep(true, true, Some(1), None), Some((None, None)));
        assert_eq!(
            place_keep(true, false, Some(1), Some(2)),
            Some((Some(1), Some(2)))
        );
    }

    #[test]
    fn normalize_full_block() {
        let v = json!({"type":"place","title":"Hack Night: Rust meets AI",
            "address":"Folkehuset Møllestien, Aarhus","lat":56.1611,"lng":10.2006,
            "when":"2026-10-27T17:00:00.000Z","end":"2026-10-27T20:00:00.000Z",
            "eventId":"iphone:abc","kind":"event","extra":1});
        assert_eq!(
            normalize_place(&v).unwrap(),
            json!({"type":"place","title":"Hack Night: Rust meets AI",
                "address":"Folkehuset Møllestien, Aarhus","lat":56.1611,"lng":10.2006,
                "when":"2026-10-27T17:00:00.000Z","end":"2026-10-27T20:00:00.000Z",
                "eventId":"iphone:abc","kind":"event"})
        );
    }

    #[test]
    fn normalize_meta_place_and_defaults() {
        let out =
            normalize_place(&json!({"title":"Gym","address":"Risskov","kind":"bogus"})).unwrap();
        assert_eq!(
            out,
            json!({"title":"Gym","address":"Risskov","kind":"place"})
        );
        assert_eq!(
            normalize_place(&json!({"type":"text","title":"x","address":"y"})),
            None
        );
        assert_eq!(normalize_place(&json!("place")), None);
    }

    #[test]
    fn normalize_title_only_is_dropped() {
        assert_eq!(
            normalize_place(&json!({"type":"place","title":"Somewhere"})),
            None
        );
        assert_eq!(
            normalize_place(&json!({"title":"Somewhere","address":" \n\t "})),
            None
        );
        assert_eq!(
            normalize_place(&json!({"title":"  ","address":"Aarhus"})),
            None
        );
        assert_eq!(
            normalize_place(&json!({"address":"Aarhus","lat":56.1,"lng":10.2})),
            None
        );
    }

    #[test]
    fn normalize_coordinates() {
        // Coordinates alone are enough.
        let out = normalize_place(&json!({"title":"Pin","lat":56.1,"lng":10.2})).unwrap();
        assert_eq!(
            (out["lat"].as_f64(), out["lng"].as_f64()),
            (Some(56.1), Some(10.2))
        );
        // One coordinate missing: the pair is dropped, the address kept.
        let out = normalize_place(&json!({"title":"T","address":"A","lat":56.1})).unwrap();
        assert!(out.get("lat").is_none() && out.get("lng").is_none());
        // ... and without an address the block goes.
        assert_eq!(normalize_place(&json!({"title":"T","lng":10.2})), None);
        // (0, 0), out of range, strings.
        for (lat, lng) in [
            (json!(0), json!(0)),
            (json!(90.1), json!(1)),
            (json!("56"), json!(10)),
        ] {
            let out =
                normalize_place(&json!({"title":"T","address":"A","lat":lat,"lng":lng})).unwrap();
            assert!(out.get("lat").is_none() && out.get("lng").is_none());
        }
        let out = normalize_place(&json!({"title":"T","lat":-90.0,"lng":180.0})).unwrap();
        assert_eq!(
            (out["lat"].as_f64(), out["lng"].as_f64()),
            (Some(-90.0), Some(180.0))
        );
    }

    #[test]
    fn normalize_clips_and_cleans() {
        let long: String = "a".repeat(201);
        let out = normalize_place(&json!({"title":"T","address":long})).unwrap();
        let a = out["address"].as_str().unwrap();
        assert_eq!(a.chars().count(), 200);
        assert!(a.ends_with('…') && a.starts_with(&"a".repeat(199)));
        let exact: String = "é".repeat(200);
        let out = normalize_place(&json!({"title":"T","address":exact.clone()})).unwrap();
        assert_eq!(out["address"], json!(exact));
        let out =
            normalize_place(&json!({"title":"Line one\r\n\n  two\u{0007}\u{001b}!","address":"x"}))
                .unwrap();
        assert_eq!(out["title"], json!("Line one   two!"));
        let out = normalize_place(&json!({"title":"t".repeat(130),"address":"x"})).unwrap();
        assert_eq!(out["title"].as_str().unwrap().chars().count(), 120);
    }

    #[test]
    fn normalize_times_and_ids() {
        let out = normalize_place(&json!({"title":"T","address":"A","when":"tomorrow",
            "end":"2026-10-27T20:00:00Z","eventId":"x".repeat(201)}))
        .unwrap();
        assert!(out.get("when").is_none() && out.get("eventId").is_none());
        assert_eq!(out["end"], json!("2026-10-27T20:00:00Z"));
        let out = normalize_place(
            &json!({"title":"T","address":"A","when":"2026-10-27T20:00:00Z",
            "end":"2026-10-27T19:00:00+00:00"}),
        )
        .unwrap();
        assert!(out.get("end").is_none());
    }

    #[test]
    fn clip_and_one_line() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcd", 3), "ab…");
        assert_eq!(clip("a   bcd", 4), "a…");
        assert_eq!(one_line("\n a\n\nb \t"), "a b");
    }

    #[test]
    fn asset_paths() {
        for ok in [
            "fonts/Noto Sans Regular/0-255.pbf",
            "sprites/v4/light@2x.png",
            "a",
            "x.y/z",
        ] {
            assert!(safe_asset_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../etc/passwd",
            "fonts/../../etc/passwd",
            "..",
            "..%2fetc%2fpasswd",
            "fonts/..%2f..%2fx",
            "fonts\\..\\x",
            "fonts/a\0.pbf",
            "fonts//a.pbf",
            "fonts/",
            "./fonts/a.pbf",
            "fonts/./a.pbf",
            "fonts/.",
            "a..b",
            "fonts/\u{7f}",
        ] {
            assert!(!safe_asset_path(bad), "{bad:?}");
        }
        assert!(safe_asset_path(&"a".repeat(512)));
        assert!(!safe_asset_path(&"a".repeat(513)));
    }

    #[test]
    fn tiles_names() {
        assert!(valid_tiles_name("region"));
        assert!(valid_tiles_name("dk-2026"));
        assert!(valid_tiles_name(&"a".repeat(40)));
        assert!(!valid_tiles_name(&"a".repeat(41)));
        assert!(!valid_tiles_name(""));
        assert!(!valid_tiles_name("Region"));
        assert!(!valid_tiles_name("../x"));
        assert!(!valid_tiles_name("a.b"));
    }
}

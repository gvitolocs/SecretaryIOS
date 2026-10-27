//! Places no geocoder knows by their calendar name: Aarhus University's room codes
//! (`5125-140 Seminarrum` is room 140 in building 5125, Edison). The buildings come from
//! `MAPS_DIR/gazetteer/*.json`, arrays of `{"ref", "name", "lat", "lng"}` (built from
//! OpenStreetMap's building `ref` tags, README.md). Answered locally, before Google and
//! without a key; a code whose building is not listed falls through to the geocoder.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::geocode::GeoResult;
use crate::kernel;

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    name: Option<String>,
    lat: f64,
    lng: f64,
}

#[derive(Debug, Default)]
pub struct Gazetteer {
    buildings: HashMap<String, (Option<String>, f64, f64)>,
}

/// The building of a room code: four digits, a dash, the room (digits, maybe a letter),
/// then the end or a space. `8000 Aarhus C` (a postcode) and `Vej 12-14` are not codes.
pub fn room_building(q: &str) -> Option<&str> {
    let q = q.trim();
    let b = q.as_bytes();
    if b.len() < 6 || !b[..4].iter().all(u8::is_ascii_digit) || b[4] != b'-' {
        return None;
    }
    let room = &b[5..];
    let digits = room.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=4).contains(&digits) {
        return None;
    }
    let rest = &room[digits..];
    let rest = match rest.first() {
        Some(c) if c.is_ascii_alphabetic() => &rest[1..],
        _ => rest,
    };
    match rest.first() {
        None | Some(b' ') => Some(&q[..4]),
        _ => None,
    }
}

impl Gazetteer {
    /// Every `*.json` in `dir`; unreadable or invalid files and entries are skipped.
    pub fn load(dir: &Path) -> Gazetteer {
        let mut buildings = HashMap::new();
        let Ok(files) = std::fs::read_dir(dir) else {
            return Gazetteer { buildings };
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(entries) = serde_json::from_str::<Vec<Entry>>(&text) else {
                tracing::warn!(file = %path.display(), "gazetteer file is not a list of buildings");
                continue;
            };
            for e in entries {
                let valid_ref =
                    e.reference.len() == 4 && e.reference.bytes().all(|c| c.is_ascii_digit());
                if valid_ref && kernel::parse_coord(e.lat, e.lng).is_some() {
                    buildings.insert(
                        e.reference,
                        (e.name.filter(|n| !n.trim().is_empty()), e.lat, e.lng),
                    );
                }
            }
        }
        Gazetteer { buildings }
    }

    pub fn len(&self) -> usize {
        self.buildings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buildings.is_empty()
    }

    /// The building of a room code, as a geocoding result.
    pub fn lookup(&self, q: &str) -> Option<GeoResult> {
        let building = room_building(q)?;
        let (name, lat, lng) = self.buildings.get(building)?;
        let name = match name {
            Some(n) => format!("Aarhus Universitet {building} {n}"),
            None => format!("Aarhus Universitet {building}"),
        };
        Some(GeoResult {
            name,
            address: q.trim().to_owned(),
            lat: *lat,
            lng: *lng,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_codes() {
        assert_eq!(room_building("5125-140 Seminarrum"), Some("5125"));
        assert_eq!(room_building(" 5789-003B Seminarrum"), Some("5789"));
        assert_eq!(room_building("5522-115"), Some("5522"));
        assert_eq!(room_building("8000 Aarhus C"), None);
        assert_eq!(room_building("1530-"), None);
        assert_eq!(room_building("5125-140Seminarrum"), None);
        assert_eq!(room_building("5125-12345 X"), None);
        assert_eq!(room_building("Møllestien 12-14"), None);
        assert_eq!(room_building("51a5-140 X"), None);
        assert_eq!(room_building("æøå"), None);
    }

    #[test]
    fn lookup_from_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("au.json"),
            r#"[{"ref":"5125","name":"Edison","lat":56.17198,"lng":10.19146},
                {"ref":"5124","lat":56.17157,"lng":10.19178},
                {"ref":"x1","lat":56.0,"lng":10.0},
                {"ref":"1111","lat":0.0,"lng":0.0}]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.json"), "{").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "[]").unwrap();
        let g = Gazetteer::load(dir.path());
        assert_eq!(g.len(), 2);
        let hit = g.lookup("5125-140 Seminarrum").unwrap();
        assert_eq!(hit.name, "Aarhus Universitet 5125 Edison");
        assert_eq!(hit.address, "5125-140 Seminarrum");
        assert_eq!((hit.lat, hit.lng), (56.17198, 10.19146));
        assert_eq!(
            g.lookup("5124-038 Seminarrum").unwrap().name,
            "Aarhus Universitet 5124"
        );
        assert!(g.lookup("5522-115 Undervisningslokale").is_none());
        assert!(g.lookup("Folkehuset Møllestien").is_none());
        assert!(Gazetteer::load(&dir.path().join("missing")).is_empty());
    }
}

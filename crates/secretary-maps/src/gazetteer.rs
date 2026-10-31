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

/// A university building and its optional room, as written in calendar locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuRoom {
    pub building: String,
    pub room: Option<String>,
}

impl AuRoom {
    pub fn floor(&self) -> Option<u8> {
        let room = self.room.as_deref()?;
        (room.bytes().take_while(u8::is_ascii_digit).count() >= 3)
            .then(|| room.as_bytes()[0] - b'0')
    }

    pub fn label(&self, name: Option<&str>) -> String {
        let mut label = format!("Building {}", self.building);
        if let Some(name) = name {
            label.push_str(&format!(" ({name})"));
        }
        if let Some(room) = &self.room {
            label.push_str(&format!(", room {room}"));
        }
        match self.floor() {
            Some(0) => label.push_str(", ground floor"),
            Some(floor) => label.push_str(&format!(", floor {floor}")),
            None => {}
        }
        label
    }
}

fn boundary(c: char) -> bool {
    !c.is_alphanumeric() && !matches!(c, '-' | '+')
}

fn parse_room(s: &str) -> Option<&str> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=4).contains(&digits) {
        return None;
    }
    let end = digits
        + usize::from(
            s.as_bytes()
                .get(digits)
                .is_some_and(u8::is_ascii_alphabetic),
        );
    if s[end..].chars().next().is_some_and(|c| !boundary(c)) {
        return None;
    }
    Some(&s[..end])
}

/// Recognize standalone four-digit room codes or explicit Building/Bygning phrases,
/// including surrounding calendar text. Postcodes, dates and phone numbers are not rooms.
pub fn au_room(q: &str) -> Option<AuRoom> {
    for (start, _) in q.char_indices() {
        let before = &q[..start];
        if before.chars().next_back().is_some_and(|c| !boundary(c)) {
            continue;
        }
        // A phone country code followed by a four-digit local number is not a building.
        if before.split_whitespace().next_back().is_some_and(|s| {
            s.strip_prefix('+')
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        }) {
            continue;
        }
        let tail = &q[start..];
        let explicit = ["building", "bygning"].into_iter().find_map(|word| {
            let prefix = tail.get(..word.len())?;
            (prefix.eq_ignore_ascii_case(word)
                && tail[word.len()..].starts_with(char::is_whitespace))
            .then(|| tail[word.len()..].trim_start())
        });
        let s = explicit.unwrap_or(tail);
        let Some(building) = s.get(..4).filter(|b| b.bytes().all(|c| c.is_ascii_digit())) else {
            continue;
        };
        let rest = s[4..].trim_start();
        let room = if let Some(rest) = rest.strip_prefix('-') {
            match parse_room(rest.trim_start()) {
                Some(room) => Some(room.to_owned()),
                None => continue,
            }
        } else if explicit.is_some() {
            if s[4..].chars().next().is_some_and(|c| !boundary(c)) {
                continue;
            }
            let rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
            let room = ["room", "lokale"].into_iter().find_map(|word| {
                let prefix = rest.get(..word.len())?;
                (prefix.eq_ignore_ascii_case(word)
                    && rest[word.len()..].starts_with(char::is_whitespace))
                .then(|| rest[word.len()..].trim_start())
            });
            match room {
                Some(room) => match parse_room(room) {
                    Some(room) => Some(room.to_owned()),
                    None => continue,
                },
                None => None,
            }
        } else {
            continue;
        };
        return Some(AuRoom {
            building: building.to_owned(),
            room,
        });
    }
    None
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
        let room = au_room(q)?;
        let building = &room.building;
        let (name, lat, lng) = self.buildings.get(building)?;
        let label = room.label(name.as_deref());
        let name = match name {
            Some(n) => format!("Aarhus Universitet {building} {n}"),
            None => format!("Aarhus Universitet {building}"),
        };
        Some(GeoResult {
            name,
            label: Some(label),
            source: Some("OpenStreetMap".into()),
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
        for (q, building, room) in [
            ("5124-038 Seminarrum", "5124", Some("038")),
            ("5522-115", "5522", Some("115")),
            (" 5789-003B Seminarrum", "5789", Some("003B")),
            ("1532-316", "1532", Some("316")),
            ("1532-316, Auditorium", "1532", Some("316")),
            ("Building 1525 - 524", "1525", Some("524")),
            ("Bygning 1525 - 524", "1525", Some("524")),
            ("Building 1525, room 524", "1525", Some("524")),
            ("Bygning 1525, lokale 524", "1525", Some("524")),
            ("Building 1525", "1525", None),
            ("Bygning 1525", "1525", None),
            (
                "Class in bUiLdInG 1525, ROOM 524 today",
                "1525",
                Some("524"),
            ),
            ("Class: 5522-115 today", "5522", Some("115")),
        ] {
            assert_eq!(
                au_room(q),
                Some(AuRoom {
                    building: building.into(),
                    room: room.map(str::to_owned)
                }),
                "{q}"
            );
        }
        for q in [
            "8000 Aarhus C",
            "Vej 12-14",
            "2026-10-29",
            "+45 1234-5678",
            "12345-678",
            "plain words",
            "1530-",
            "5125-140Seminarrum",
            "5125-12345 X",
            "51a5-140 X",
            "æøå",
        ] {
            assert_eq!(au_room(q), None, "{q}");
        }
    }

    #[test]
    fn floors_and_labels() {
        for (room, floor) in [
            ("115", Some(1)),
            ("038", Some(0)),
            ("003B", Some(0)),
            ("12", None),
            ("1B", None),
            ("1234", Some(1)),
        ] {
            assert_eq!(au_room(&format!("5522-{room}")).unwrap().floor(), floor);
        }
        assert_eq!(
            au_room("Building 5522").unwrap().label(None),
            "Building 5522"
        );
        assert_eq!(
            au_room("5522-038").unwrap().label(Some("IT")),
            "Building 5522 (IT), room 038, ground floor"
        );
        assert_eq!(
            au_room("5522-12").unwrap().label(None),
            "Building 5522, room 12"
        );
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

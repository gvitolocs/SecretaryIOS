//! `au_people_search`, `au_building`, `au_campus_map`: Aarhus University
//! people, buildings and campus maps through AU Find (secretary-server
//! `/v1/au/*` → Lucina's `/api/secretary/au/*` → aufind.au.dk).
//!
//! Read-only, scope `read`; an agent-scoped token may call them too
//! (`crate::tools::also_allowed`). Lucina applies the retries (surname, then
//! prefix) and the disambiguation (top 3 when ambiguous); this module only
//! checks the arguments, renders the answer and keeps the raw JSON beside it,
//! so the model quotes fields as returned and cites the source.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use reqwest::Method;
use rmcp::model::{ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::computer::caller_headers;
use crate::tools::{ToolError, Tools};

/// Footer of every people / building answer.
pub const SOURCE: &str =
    "Source: AU Find (aufind.au.dk) + www.au.dk profile pages. Raw fields only; nothing inferred.";
/// Caption footer of a campus map.
pub const MAP_SOURCE: &str = "Source: AU Find (aufind) map, tiles © OpenStreetMap contributors";
/// What to do when AU Find is down.
const FALLBACK: &str = "AU Find is unreachable right now. Fall back to au.dk pages (www.au.dk, mit.au.dk, brightspace.au.dk, pure.au.dk only), quoting text and URL, and stop at any login.";

/// The AU park, Aarhus: the map's centre when nothing is given.
pub const AU_PARK: (f64, f64) = (56.1697, 10.2039);
const PARK_ZOOM: u8 = 15;
const BUILDING_ZOOM: u8 = 17;

/// Language of AU Find's answer.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Da,
    En,
}

/// Arguments of `au_people_search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PeopleArgs {
    /// A name (or part of one), email or phone, 1-200 characters, e.g. "Jens Hansen".
    pub query: String,
    /// Language of positions and units: da or en (AU Find's default otherwise).
    #[serde(default)]
    pub lang: Option<Lang>,
}

/// Arguments of `au_building`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildingArgs {
    /// A building number (e.g. "1532"), name or address, 1-200 characters.
    pub query: String,
}

/// Arguments of `au_campus_map`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MapArgs {
    /// An AU building id as au_building returns it (1-6 letters or digits, e.g. "1532").
    #[serde(default)]
    pub building: Option<String>,
    /// Centre latitude (54-58), with longitude, instead of a building.
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Centre longitude (8-16), with latitude, instead of a building.
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Zoom 14-18 (default 17 for a building, 15 for the AU park).
    #[serde(default)]
    pub zoom: Option<u8>,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn query(q: &str) -> Result<String, ToolError> {
    let q = q.trim();
    if q.is_empty() || q.chars().count() > 200 || q.chars().any(char::is_control) {
        return Err(bad("`query` must be 1-200 characters"));
    }
    Ok(q.to_owned())
}

fn refusal(e: crate::backend::BackendError) -> ToolError {
    match e.code.as_str() {
        "au_unavailable" => bad(FALLBACK),
        _ => e.into(),
    }
}

/// The map request: query pairs and the caption's subject.
#[derive(Debug, PartialEq)]
pub struct MapRequest {
    pub query: Vec<(&'static str, String)>,
    pub subject: String,
    pub zoom: u8,
}

/// Checks `au_campus_map`'s arguments and applies the defaults.
pub fn map_request(a: &MapArgs) -> Result<MapRequest, ToolError> {
    if let Some(z) = a.zoom {
        if !(14..=18).contains(&z) {
            return Err(bad("`zoom` must be 14-18"));
        }
    }
    let building = a
        .building
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    match (building, a.latitude, a.longitude) {
        (Some(b), None, None) => {
            if b.len() > 6 || !b.bytes().all(|c| c.is_ascii_alphanumeric()) {
                return Err(bad(
                    "`building` is an AU building id: 1-6 letters or digits, e.g. 1532",
                ));
            }
            let zoom = a.zoom.unwrap_or(BUILDING_ZOOM);
            Ok(MapRequest {
                query: vec![("building", b.to_owned()), ("z", zoom.to_string())],
                subject: format!("building {b}"),
                zoom,
            })
        }
        (None, Some(lat), Some(lng)) => {
            if !(54.0..=58.0).contains(&lat) || !(8.0..=16.0).contains(&lng) {
                return Err(bad(
                    "`latitude` must be 54-58 and `longitude` 8-16 (Denmark)",
                ));
            }
            let zoom = a.zoom.unwrap_or(BUILDING_ZOOM);
            Ok(MapRequest {
                query: vec![
                    ("lat", lat.to_string()),
                    ("lng", lng.to_string()),
                    ("z", zoom.to_string()),
                ],
                subject: format!("centre {lat}, {lng}"),
                zoom,
            })
        }
        (None, None, None) => {
            let zoom = a.zoom.unwrap_or(PARK_ZOOM);
            let (lat, lng) = AU_PARK;
            Ok(MapRequest {
                query: vec![
                    ("lat", lat.to_string()),
                    ("lng", lng.to_string()),
                    ("z", zoom.to_string()),
                ],
                subject: format!("the AU park (centre {lat}, {lng})"),
                zoom,
            })
        }
        (Some(_), _, _) => Err(bad(
            "Give either `building` or `latitude` and `longitude`, not both",
        )),
        _ => Err(bad("`latitude` and `longitude` go together")),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Strings of an array (or the `name` of its objects), joined.
fn joined(v: &Value, sep: &str) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Array(a) => {
            let parts: Vec<&str> = a
                .iter()
                .filter_map(|x| x.as_str().or_else(|| x.get("name").and_then(Value::as_str)))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(sep))
        }
        _ => None,
    }
}

fn status_line(v: &Value, what: &str, n: usize) -> String {
    let q = str_of(v, "query")
        .map(|q| format!(" for \"{q}\""))
        .unwrap_or_default();
    match v["status"].as_str().unwrap_or("") {
        "found" => format!("AU Find: found{q}: {n} {what}."),
        "ambiguous" => {
            let total = v["total"]
                .as_u64()
                .map(|t| format!(" ({t} matches)"))
                .unwrap_or_default();
            format!(
                "AU Find: ambiguous{q}{total}; the top {n} are shown. Do not pick one: list them and ask which is meant."
            )
        }
        "not_found" => format!("Not found in AU Find{q}."),
        "unavailable" => FALLBACK.to_owned(),
        other => format!("AU Find: {other}{q}."),
    }
}

/// The attempts Lucina made (`tried`), for a not_found answer.
fn attempts(v: &Value) -> Option<String> {
    let tried: Vec<String> = v["tried"]
        .as_array()?
        .iter()
        .filter_map(|t| {
            let q = t["q"].as_str()?;
            Some(match t["total"].as_u64() {
                Some(n) => format!("\"{q}\" ({n})"),
                None => format!("\"{q}\""),
            })
        })
        .collect();
    (!tried.is_empty()).then(|| format!("Attempts: {}", tried.join(", ")))
}

/// `au_people_search`'s text: status, one numbered line per person, the
/// attempts when nothing was found, and the source footer.
pub fn format_people(v: &Value) -> String {
    let people = v["people"].as_array().cloned().unwrap_or_default();
    let mut out = vec![status_line(
        v,
        if people.len() == 1 {
            "person"
        } else {
            "people"
        },
        people.len(),
    )];
    for (i, p) in people.iter().enumerate() {
        let fields: Vec<String> = [
            str_of(p, "name").map(str::to_owned),
            str_of(p, "position").map(str::to_owned),
            joined(&p["units"], ", "),
            str_of(p, "au_room").map(|r| format!("room {r}")),
            str_of(p, "email").map(str::to_owned),
            str_of(p, "phone").map(str::to_owned),
            str_of(p, "profile_url").map(str::to_owned),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push(format!("{}. {}", i + 1, fields.join(" — ")));
    }
    if let Some(more) = v.get("more").filter(|m| !m.is_null()) {
        out.push(format!(
            "More matches: {}",
            more.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| more.to_string())
        ));
    }
    if v["status"] == "not_found" {
        out.extend(attempts(v));
    }
    out.push(SOURCE.to_owned());
    out.join("\n")
}

/// `au_building`'s text.
pub fn format_buildings(v: &Value) -> String {
    let buildings = v["buildings"].as_array().cloned().unwrap_or_default();
    let mut out = vec![status_line(
        v,
        if buildings.len() == 1 {
            "building"
        } else {
            "buildings"
        },
        buildings.len(),
    )];
    for (i, b) in buildings.iter().enumerate() {
        let id = b["id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| b["id"].as_u64().map(|n| n.to_string()));
        let coords = match (b["latitude"].as_f64(), b["longitude"].as_f64()) {
            (Some(lat), Some(lng)) => Some(format!("{lat}, {lng}")),
            _ => None,
        };
        let fields: Vec<String> = [
            id.as_ref().map(|id| format!("building {id}")),
            joined(&b["names"], " / "),
            joined(&b["address"], ", "),
            coords,
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push(format!("{}. {}", i + 1, fields.join(" — ")));
        if let Some(id) = id {
            out.push(format!("   map: au_campus_map building=\"{id}\""));
        }
    }
    if v["status"] == "not_found" {
        out.extend(attempts(v));
    }
    out.push(SOURCE.to_owned());
    out.join("\n")
}

fn with_json(text: String, v: &Value) -> Vec<ContentBlock> {
    vec![
        ContentBlock::text(text),
        ContentBlock::text(serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())),
    ]
}

impl Tools<'_> {
    async fn au_get(&self, path: &str, q: &[(&str, String)]) -> Result<Value, ToolError> {
        self.app
            .backend
            .send_device_query(
                Method::GET,
                path,
                q,
                None,
                caller_headers(&self.caller.name),
            )
            .await
            .map_err(refusal)
    }

    /// `au_people_search`, `au_building`, `au_campus_map`.
    pub async fn au(&self, name: &str, args: &JsonObject) -> Result<Vec<ContentBlock>, ToolError> {
        match name {
            "au_people_search" => {
                let a: PeopleArgs = parse(args)?;
                let mut q = vec![("q", query(&a.query)?)];
                if let Some(lang) = a.lang {
                    q.push((
                        "lang",
                        match lang {
                            Lang::Da => "da",
                            Lang::En => "en",
                        }
                        .to_owned(),
                    ));
                }
                let v = self.au_get("/v1/au/people", &q).await?;
                Ok(with_json(format_people(&v), &v))
            }
            "au_building" => {
                let a: BuildingArgs = parse(args)?;
                let v = self
                    .au_get("/v1/au/buildings", &[("q", query(&a.query)?)])
                    .await?;
                Ok(with_json(format_buildings(&v), &v))
            }
            "au_campus_map" => {
                let a: MapArgs = parse(args)?;
                let req = map_request(&a)?;
                let (content_type, png) = self
                    .app
                    .backend
                    .get_device_bytes(
                        "/v1/au/map.png",
                        &req.query,
                        caller_headers(&self.caller.name),
                    )
                    .await
                    .map_err(|e| match e.code.as_str() {
                        "au_unavailable" => bad(
                            "The AU campus map is unavailable right now (AU Find is unreachable).",
                        ),
                        _ => refusal(e),
                    })?;
                if !content_type.starts_with("image/png") || png.is_empty() {
                    return Err(bad("secretary-server sent no PNG map"));
                }
                Ok(vec![
                    ContentBlock::image(STANDARD.encode(&png), "image/png"),
                    ContentBlock::text(format!(
                        "AU campus map: {}, zoom {}.\n{MAP_SOURCE}",
                        req.subject, req.zoom
                    )),
                ])
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Result<MapRequest, ToolError> {
        map_request(&parse::<MapArgs>(v.as_object().unwrap())?)
    }

    #[test]
    fn map_defaults_and_validation() {
        let r = map(json!({})).unwrap();
        assert_eq!(
            r.query,
            [
                ("lat", "56.1697".to_owned()),
                ("lng", "10.2039".to_owned()),
                ("z", "15".to_owned())
            ]
        );
        assert!(r.subject.contains("AU park"));
        let r = map(json!({"building": "1532"})).unwrap();
        assert_eq!(
            r.query,
            [("building", "1532".to_owned()), ("z", "17".to_owned())]
        );
        let r = map(json!({"latitude": 56.17, "longitude": 10.19, "zoom": 18})).unwrap();
        assert_eq!(r.zoom, 18);
        for (args, want) in [
            (json!({"zoom": 13}), "`zoom`"),
            (json!({"zoom": 19}), "`zoom`"),
            (json!({"building": "../x"}), "`building`"),
            (json!({"building": "1234567"}), "`building`"),
            (json!({"latitude": 56.1}), "go together"),
            (json!({"latitude": 50.0, "longitude": 10.0}), "54-58"),
            (json!({"latitude": 56.0, "longitude": 20.0}), "8-16"),
            (
                json!({"building": "1532", "latitude": 56.0, "longitude": 10.0}),
                "not both",
            ),
            (json!({"floor": 2}), "unknown field"),
        ] {
            let e = map(args.clone()).err().map(|e| e.0).unwrap_or_default();
            assert!(e.contains(want), "{args}: {e}");
        }
    }

    #[test]
    fn queries_are_checked() {
        assert_eq!(query("  Jens  ").unwrap(), "Jens");
        assert!(query(&"é".repeat(200)).is_ok());
        for q in ["", "   ", "a\nb", &"x".repeat(201)] {
            assert!(query(q).is_err(), "{q:?}");
        }
        assert!(
            parse::<PeopleArgs>(json!({"query": "x", "lang": "it"}).as_object().unwrap()).is_err()
        );
        assert!(
            parse::<PeopleArgs>(json!({"query": "x", "lang": "da"}).as_object().unwrap()).is_ok()
        );
    }

    #[test]
    fn people_read_well() {
        let v = json!({"source": "aufind", "status": "ambiguous", "query": "Hansen", "total": 57,
        "tried": [{"q": "Hansen", "total": 57}],
        "people": [
            {"name": "Jens Hansen", "position": "Lektor", "units": ["Datalogi", {"name": "Nat-Tech"}],
             "email": "jh@cs.au.dk", "phone": "+45 8715 0000", "au_room": "1532-123", "room": "123",
             "profile_url": "https://www.au.dk/en/jh@cs"},
            {"name": "Mette Hansen", "email": "mh@au.dk"},
        ]});
        let t = format_people(&v);
        let lines: Vec<&str> = t.lines().collect();
        assert!(
            lines[0].contains("ambiguous")
                && lines[0].contains("57 matches")
                && lines[0].contains("Do not pick one"),
            "{t}"
        );
        assert_eq!(
            lines[1],
            "1. Jens Hansen — Lektor — Datalogi, Nat-Tech — room 1532-123 — jh@cs.au.dk — +45 8715 0000 — https://www.au.dk/en/jh@cs"
        );
        assert_eq!(lines[2], "2. Mette Hansen — mh@au.dk");
        assert_eq!(*lines.last().unwrap(), SOURCE);
        assert!(!t.contains("Attempts"));

        let nf = json!({"status": "not_found", "query": "Xyz Qwerty", "people": [],
            "tried": [{"q": "Xyz Qwerty", "total": 0}, {"q": "Qwerty", "total": 0}, {"q": "Qwer", "total": 0}]});
        let t = format_people(&nf);
        assert!(
            t.starts_with("Not found in AU Find for \"Xyz Qwerty\"."),
            "{t}"
        );
        assert!(
            t.contains("Attempts: \"Xyz Qwerty\" (0), \"Qwerty\" (0), \"Qwer\" (0)"),
            "{t}"
        );
        assert!(t.ends_with(SOURCE));

        let un = format_people(&json!({"status": "unavailable", "people": []}));
        assert!(
            un.contains("pure.au.dk only") && un.ends_with(SOURCE),
            "{un}"
        );
    }

    #[test]
    fn buildings_read_well() {
        let v = json!({"status": "found", "query": "1532", "buildings": [
            {"id": "1532", "names": ["Nygaard"], "address": ["Åbogade 34", "8200 Aarhus N"], "latitude": 56.17, "longitude": 10.19}]});
        let t = format_buildings(&v);
        assert!(
            t.contains("1. building 1532 — Nygaard — Åbogade 34, 8200 Aarhus N — 56.17, 10.19"),
            "{t}"
        );
        assert!(t.contains("au_campus_map building=\"1532\""));
        assert!(t.ends_with(SOURCE));
    }
}

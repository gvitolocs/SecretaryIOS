//! AU Find (Aarhus University) through the local af-server (`LUCINA_AU_URL`,
//! default `http://127.0.0.1:18211`): people, buildings and campus maps.
//!
//! The owner's procedure is code, not a prompt: `/health` first (cached 30 s;
//! down means `unavailable`, never a guess), then the search, then at most
//! two retries (the surname alone, then a prefix of it), each attempt kept in
//! `tried`. One hit is `found`, more are `ambiguous` (the top 3, never one
//! picked), none is `not_found` with the au.dk fallback. A person carries
//! af-server's raw fields only: nothing is filled in.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolEnv, ToolError, ToolResult};

pub const DEFAULT_URL: &str = "http://127.0.0.1:18211";
pub const SOURCE: &str = "aufind";
pub const MAP_SOURCE: &str = "aufind (© OpenStreetMap contributors)";
/// The University Park, Aarhus (the campus view's centre).
pub const PARK: (f64, f64) = (56.1697, 10.2039);
pub const BUILDING_ZOOM: u8 = 17;
pub const CAMPUS_ZOOM: u8 = 15;
pub const MIN_ZOOM: u8 = 14;
pub const MAX_ZOOM: u8 = 18;
/// The tools' names (`LUCINA_AU=off` removes them).
pub const NAMES: &[&str] = &["au_people_search", "au_building", "au_campus_map"];
/// Shown in a reply: the rest is `more`.
pub const TOP: usize = 3;

const TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_TTL: Duration = Duration::from_secs(30);
const PERSON_FIELDS: &[&str] = &[
    "position",
    "units",
    "email",
    "phone",
    "building",
    "room",
    "au_room",
    "address",
    "profile_url",
    "map_url",
];

/// A building id as af-server knows it (`1532`, `1872A`).
pub fn valid_building_id(id: &str) -> bool {
    (1..=6).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Inside Denmark (a map centre outside it is refused).
pub fn in_denmark(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (54.0..=58.0).contains(&lat)
        && (8.0..=16.0).contains(&lng)
}

pub fn clamp_zoom(z: Option<i64>, default: u8) -> u8 {
    z.map_or(default, |z| z.clamp(MIN_ZOOM as i64, MAX_ZOOM as i64) as u8)
}

/// The app path of a building's map PNG (secretary-server proxies `/v1/au/*` here).
pub fn building_map_path(id: &str, z: u8) -> String {
    format!("/v1/au/map.png?building={id}&z={z}")
}

pub fn point_map_path(lat: f64, lng: f64, z: u8) -> String {
    format!("/v1/au/map.png?lat={lat:.5}&lng={lng:.5}&z={z}")
}

/// What a call to af-server can end in.
#[derive(Debug, Clone, PartialEq)]
pub enum AuError {
    /// Down, timed out, or answered 5xx: say so, never guess.
    Unavailable(String),
    NotFound,
    BadRequest(String),
}

impl std::fmt::Display for AuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuError::Unavailable(e) => write!(f, "AU Find unavailable: {e}"),
            AuError::NotFound => f.write_str("not found in AU Find"),
            AuError::BadRequest(e) => write!(f, "bad request: {e}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Found,
    Ambiguous,
    NotFound,
    Unavailable,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Found => "found",
            Status::Ambiguous => "ambiguous",
            Status::NotFound => "not_found",
            Status::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attempt {
    pub q: String,
    pub total: u64,
}

/// The people procedure's answer (also the `/api/secretary/au/people` body).
#[derive(Debug, Clone, Serialize)]
pub struct AuPeople {
    pub source: &'static str,
    pub status: Status,
    pub query: String,
    pub tried: Vec<Attempt>,
    pub total: u64,
    pub people: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub af_server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The building lookup's answer (also the `/api/secretary/au/buildings` body).
#[derive(Debug, Clone, Serialize)]
pub struct AuBuildings {
    pub source: &'static str,
    pub status: Status,
    pub query: String,
    pub buildings: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn not_found_fallback() -> Value {
    json!({"site": "www.au.dk", "suggestion": "Search au.dk in my browser"})
}

/// Per af-server base URL: when `/health` was asked, and its answer.
type HealthCache = Mutex<HashMap<String, (Instant, Result<Value, String>)>>;

fn health_cache() -> &'static HealthCache {
    static C: OnceLock<HealthCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// af-server's HTTP API.
pub struct AuClient<'a> {
    http: &'a reqwest::Client,
    base: String,
}

impl<'a> AuClient<'a> {
    pub fn new(http: &'a reqwest::Client, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// The tools' client (`None`: `LUCINA_AU=off`).
    pub fn from_env(env: &'a ToolEnv) -> Option<Self> {
        env.au_url.as_deref().map(|u| Self::new(&env.http, u))
    }

    async fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value, AuError> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| AuError::Unavailable(e.without_url().to_string()))?;
        let status = r.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AuError::NotFound);
        }
        let body: Value = r.json().await.unwrap_or(Value::Null);
        let msg = || {
            body["error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("af-server answered {}", status.as_u16()))
        };
        if status.is_client_error() {
            return Err(AuError::BadRequest(msg()));
        }
        if !status.is_success() {
            return Err(AuError::Unavailable(msg()));
        }
        Ok(body)
    }

    /// `/health` now (no cache).
    pub async fn health_fresh(&self) -> Result<Value, String> {
        let r = self.get_json("/health", &[]).await.map_err(|e| match e {
            AuError::Unavailable(e) | AuError::BadRequest(e) => e,
            AuError::NotFound => "af-server answered 404 on /health".into(),
        });
        health_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.base.clone(), (Instant::now(), r.clone()));
        r
    }

    /// `/health`, answered from the last 30 s when possible.
    pub async fn health(&self) -> Result<Value, String> {
        if let Some((at, r)) = health_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.base)
        {
            if at.elapsed() < HEALTH_TTL {
                return r.clone();
            }
        }
        self.health_fresh().await
    }

    /// One search: `(total, hits without digit-only names, upstream)`.
    async fn search(
        &self,
        q: &str,
        lang: &str,
    ) -> Result<(u64, Vec<Value>, Option<String>), AuError> {
        let v = self
            .get_json(
                "/api/people/search",
                &[
                    ("q", q.to_owned()),
                    ("lang", lang.to_owned()),
                    ("limit", "10".into()),
                    ("details", TOP.to_string()),
                ],
            )
            .await?;
        let all: Vec<Value> = v["hits"].as_array().cloned().unwrap_or_default();
        let raw = all.len() as u64;
        let hits: Vec<Value> = all
            .into_iter()
            .filter(|h| {
                let name = h["name"].as_str().unwrap_or("").trim();
                !name.is_empty()
                    && !name
                        .chars()
                        .all(|c| c.is_ascii_digit() || c.is_whitespace())
            })
            .collect();
        let dropped = raw - hits.len() as u64;
        let total = v["total"]
            .as_u64()
            .unwrap_or(raw)
            .saturating_sub(dropped)
            .max(hits.len() as u64);
        Ok((total, hits, v["upstream"].as_str().map(str::to_owned)))
    }

    pub async fn person(&self, uuid: &str) -> Result<Value, AuError> {
        if uuid.is_empty() || uuid.contains(['/', '?', '#']) {
            return Err(AuError::NotFound);
        }
        self.get_json(&format!("/api/people/{uuid}"), &[]).await
    }

    /// The people procedure (see the module doc).
    pub async fn people_search(&self, q: &str, lang: &str) -> AuPeople {
        let query = q.split_whitespace().collect::<Vec<_>>().join(" ");
        let lang = if lang == "da" { "da" } else { "en" };
        let mut out = AuPeople {
            source: SOURCE,
            status: Status::Unavailable,
            query: query.clone(),
            tried: Vec::new(),
            total: 0,
            people: Vec::new(),
            more: None,
            fallback: None,
            upstream: None,
            af_server_version: None,
            error: None,
        };
        match self.health().await {
            Ok(h) => out.af_server_version = h["version"].as_str().map(str::to_owned),
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        }
        if query.is_empty() {
            out.status = Status::NotFound;
            out.fallback = Some(not_found_fallback());
            return out;
        }
        let mut attempts = vec![query.clone()];
        let words: Vec<&str> = query.split(' ').collect();
        let surname = words.last().copied().unwrap_or_default().to_owned();
        if words.len() >= 2 {
            attempts.push(surname.clone());
        }
        let n = surname.chars().count();
        let keep = 4.max(n.saturating_sub(2));
        if keep < n {
            attempts.push(surname.chars().take(keep).collect());
        }
        let mut found: Option<(u64, Vec<Value>)> = None;
        for a in attempts {
            match self.search(&a, lang).await {
                Ok((total, hits, upstream)) => {
                    out.tried.push(Attempt { q: a, total });
                    out.upstream = upstream.or(out.upstream.take());
                    if !hits.is_empty() {
                        found = Some((total, hits));
                        break;
                    }
                }
                Err(e) => {
                    out.error = Some(e.to_string());
                    return out;
                }
            }
        }
        let Some((total, hits)) = found else {
            out.status = Status::NotFound;
            out.fallback = Some(not_found_fallback());
            return out;
        };
        out.total = total;
        out.status = if hits.len() == 1 {
            Status::Found
        } else {
            Status::Ambiguous
        };
        for h in hits.iter().take(TOP) {
            let profile = match &h["profile"] {
                p @ Value::Object(_) => p.clone(),
                _ => match h["uuid"].as_str() {
                    Some(id) => self.person(id).await.unwrap_or(Value::Null),
                    None => Value::Null,
                },
            };
            out.people.push(person_fields(h, &profile));
        }
        if out.status == Status::Ambiguous {
            out.more = Some(total.saturating_sub(TOP as u64)).filter(|m| *m > 0);
        }
        out
    }

    /// One building by id (`Ok(None)`: not in AU Find).
    pub async fn building_by_id(&self, id: &str) -> Result<Option<Value>, AuError> {
        if !valid_building_id(id) {
            return Ok(None);
        }
        match self.get_json(&format!("/api/buildings/{id}"), &[]).await {
            Ok(v) if v.is_object() => Ok(Some(v)),
            Ok(_) | Err(AuError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The building lookup: by number ("1532", "bygning 1532") or by name / address.
    pub async fn building(&self, q: &str) -> AuBuildings {
        let query = q.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out = AuBuildings {
            source: SOURCE,
            status: Status::Unavailable,
            query: query.clone(),
            buildings: Vec::new(),
            map: None,
            error: None,
        };
        if let Err(e) = self.health().await {
            out.error = Some(e);
            return out;
        }
        let v = match self
            .get_json(
                "/api/buildings",
                &[("q", query.clone()), ("limit", "10".into())],
            )
            .await
        {
            Ok(v) => v,
            Err(AuError::NotFound) | Err(AuError::BadRequest(_)) => Value::Null,
            Err(e) => {
                out.error = Some(e.to_string());
                return out;
            }
        };
        let list = v
            .as_array()
            .or_else(|| v["buildings"].as_array())
            .cloned()
            .unwrap_or_default();
        let wanted = building_id_of(&query);
        let exact = list
            .first()
            .and_then(|b| b["id"].as_str())
            .zip(wanted.as_deref())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
        out.status = match list.len() {
            0 => Status::NotFound,
            1 => Status::Found,
            _ if exact => Status::Found,
            _ => Status::Ambiguous,
        };
        let shown = if out.status == Status::Found { 1 } else { TOP };
        out.buildings = list.into_iter().take(shown).collect();
        if out.status == Status::Found {
            if let Some(id) = out.buildings[0]["id"]
                .as_str()
                .filter(|i| valid_building_id(i))
            {
                out.map =
                    Some(json!({"z": BUILDING_ZOOM, "path": building_map_path(id, BUILDING_ZOOM)}));
            }
        }
        out
    }

    /// A map PNG: a building outlined, or a point.
    pub async fn map_png(&self, target: &MapTarget) -> Result<Vec<u8>, AuError> {
        let (path, query): (String, Vec<(&str, String)>) = match target {
            MapTarget::Building { id, z } => (
                format!("/api/buildings/{id}/map"),
                vec![("z", z.to_string())],
            ),
            MapTarget::Point { lat, lng, z } => (
                format!("/api/campus/map/{z}/{lat:.6}/{lng:.6}"),
                vec![("pin", "1".into())],
            ),
        };
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .query(&query)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| AuError::Unavailable(e.without_url().to_string()))?;
        match r.status().as_u16() {
            200 => r
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| AuError::Unavailable(e.without_url().to_string())),
            404 => Err(AuError::NotFound),
            s @ 400..=499 => Err(AuError::BadRequest(format!("af-server answered {s}"))),
            s => Err(AuError::Unavailable(format!("af-server answered {s}"))),
        }
    }
}

/// What a map shows.
#[derive(Debug, Clone, PartialEq)]
pub enum MapTarget {
    Building { id: String, z: u8 },
    Point { lat: f64, lng: f64, z: u8 },
}

/// "1532", "bygning 1532", "building 1532", "edificio 1532" -> `1532`.
pub fn building_id_of(q: &str) -> Option<String> {
    let t = q.trim().to_lowercase();
    let t = ["bygning", "building", "edificio", "bldg.", "bldg"]
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(&t)
        .trim()
        .to_owned();
    (valid_building_id(&t) && t.chars().any(|c| c.is_ascii_digit())).then(|| t.to_uppercase())
}

/// A person as shown: the hit's name and the profile's raw fields, nothing filled in.
fn person_fields(hit: &Value, profile: &Value) -> Value {
    let mut p = serde_json::Map::new();
    p.insert("uuid".into(), hit["uuid"].clone());
    p.insert(
        "name".into(),
        match &profile["name"] {
            Value::String(s) if !s.trim().is_empty() => Value::String(s.clone()),
            _ => hit["name"].clone(),
        },
    );
    for k in PERSON_FIELDS {
        p.insert((*k).into(), profile.get(*k).cloned().unwrap_or(Value::Null));
    }
    p.insert("source".into(), json!(SOURCE));
    if let Some(url) = profile["profile_url"].as_str().filter(|u| !u.is_empty()) {
        p.insert("profile_source".into(), json!(format!("www.au.dk {url}")));
    }
    Value::Object(p)
}

fn client(ctx: &Ctx) -> Result<AuClient<'_>, ToolError> {
    AuClient::from_env(&ctx.env)
        .ok_or_else(|| ToolError::new("AU Find is turned off (LUCINA_AU=off)"))
}

// ---------------------------------------------------------------- tools

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Lang {
    Da,
    En,
}

#[derive(Deserialize, JsonSchema)]
struct PeopleArgs {
    /// A name, or part of one ("Dennis Vittrup", "Vittrup").
    query: String,
    /// Language of the profile fields: "da" or "en" (default "en").
    lang: Option<Lang>,
}

pub struct AuPeopleSearchTool;

#[async_trait::async_trait]
impl Tool for AuPeopleSearchTool {
    fn name(&self) -> &'static str {
        "au_people_search"
    }
    fn description(&self) -> &'static str {
        "AU Find (Aarhus University) people lookup: position, units, building-room (AU format 1872-237), email and phone of AU staff. Retries (surname, prefix) and disambiguation are built in: status found, ambiguous (the top 3: ask which one, never pick), not_found (say so, offer an au.dk search) or unavailable. Always cite the source \"aufind\" (profile fields: the www.au.dk profile URL); never invent contact data."
    }
    fn parameters(&self) -> Value {
        schema_for::<PeopleArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: PeopleArgs = super::args(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::new("query is empty"));
        }
        let lang = match a.lang {
            Some(Lang::Da) => "da",
            _ => "en",
        };
        let r = client(ctx)?.people_search(&a.query, lang).await;
        serde_json::to_value(r).map_err(|e| ToolError::new(e.to_string()))
    }
}

#[derive(Deserialize, JsonSchema)]
struct BuildingArgs {
    /// A building number ("1532", "bygning 1532") or a name / address ("Nobel Park").
    query: String,
}

pub struct AuBuildingTool;

#[async_trait::async_trait]
impl Tool for AuBuildingTool {
    fn name(&self) -> &'static str {
        "au_building"
    }
    fn description(&self) -> &'static str {
        "AU (Aarhus University) building or department lookup by number or name, from AU Find (aufind): names, address, coordinates. Cite the source \"aufind\"."
    }
    fn parameters(&self) -> Value {
        schema_for::<BuildingArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: BuildingArgs = super::args(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::new("query is empty"));
        }
        let r = client(ctx)?.building(&a.query).await;
        serde_json::to_value(r).map_err(|e| ToolError::new(e.to_string()))
    }
}

#[derive(Deserialize, JsonSchema)]
struct MapArgs {
    /// An AU building number ("1532").
    building: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    /// 14-18 (default 17 for a building, 15 for the campus).
    zoom: Option<i64>,
}

pub struct AuCampusMapTool;

/// The map tool's answer without the network (the building's centre is filled in by the caller).
fn map_answer(building: Option<&Value>, lat: f64, lng: f64, z: u8) -> Value {
    let image = match building.and_then(|b| b["id"].as_str()) {
        Some(id) => building_map_path(id, z),
        None => point_map_path(lat, lng, z),
    };
    let mut v = json!({
        "source": MAP_SOURCE,
        "image": image,
        "center": {"lat": lat, "lng": lng},
        "zoom": z,
    });
    if let Some(b) = building {
        v["building"] = b.clone();
    }
    v
}

#[async_trait::async_trait]
impl Tool for AuCampusMapTool {
    fn name(&self) -> &'static str {
        "au_campus_map"
    }
    fn description(&self) -> &'static str {
        "A map image of the Aarhus University campus from AU Find: a building outlined (by number), a point (latitude/longitude), or the whole campus (neither). The chat shows the image. No door-by-door routes."
    }
    fn parameters(&self) -> Value {
        schema_for::<MapArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: MapArgs = super::args(args)?;
        let building = a
            .building
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty());
        let out = if let Some(b) = building {
            let id = building_id_of(b)
                .ok_or_else(|| ToolError::new(format!("\"{b}\" is not an AU building number")))?;
            let z = clamp_zoom(a.zoom, BUILDING_ZOOM);
            let found = match client(ctx)?.building_by_id(&id).await {
                Ok(f) => f,
                Err(e) => {
                    return Ok(
                        json!({"source": SOURCE, "status": "unavailable", "error": e.to_string()}),
                    )
                }
            };
            let Some(found) = found else {
                return Ok(json!({"source": SOURCE, "status": "not_found", "building": id}));
            };
            let (lat, lng) = (
                found["latitude"].as_f64().unwrap_or(PARK.0),
                found["longitude"].as_f64().unwrap_or(PARK.1),
            );
            map_answer(Some(&found), lat, lng, z)
        } else {
            match (a.latitude, a.longitude) {
                (Some(lat), Some(lng)) => {
                    if !in_denmark(lat, lng) {
                        return Err(ToolError::new("the coordinates are not in Denmark"));
                    }
                    map_answer(None, lat, lng, clamp_zoom(a.zoom, BUILDING_ZOOM))
                }
                (None, None) => map_answer(None, PARK.0, PARK.1, clamp_zoom(a.zoom, CAMPUS_ZOOM)),
                _ => return Err(ToolError::new("give both latitude and longitude")),
            }
        };
        let (alt, caption) = match out["building"]["id"].as_str() {
            Some(id) => (
                format!("Map of AU building {id}"),
                format!("AU building {id} (AU Find, © OpenStreetMap contributors)"),
            ),
            None => (
                "Map of the Aarhus University campus".to_owned(),
                "Aarhus University campus (AU Find, © OpenStreetMap contributors)".to_owned(),
            ),
        };
        ctx.push_effect(Effect::Image {
            src: out["image"].as_str().unwrap_or_default().to_owned(),
            alt,
            caption,
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn health() -> Value {
        json!({"status": "ok", "service": "af-server", "version": "0.3.0", "locations": {"buildings": 608}})
    }

    async fn server() -> MockServer {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health()))
            .mount(&s)
            .await;
        s
    }

    fn profile(name: &str, building: &str, room: &str) -> Value {
        json!({"uuid": "p", "name": name, "position": "Lecturer", "units": ["AU IT"], "phone": "+45 1",
            "email": format!("{}@au.dk", name.to_lowercase().replace(' ', ".")), "address": ["Aarhus"],
            "building": building, "room": room, "au_room": format!("{building}-{room}"),
            "map_url": null, "photo_url": null,
            "profile_url": format!("https://www.au.dk/en/show/person/{}", name.to_lowercase().replace(' ', "")),
            "source": "www.au.dk"})
    }

    fn hit(uuid: &str, name: &str, profile: Value) -> Value {
        json!({"uuid": uuid, "name": name, "first_name": "", "last_name": "", "employee_id": "1", "profile": profile})
    }

    async fn search(s: &MockServer, q: &str, total: u64, hits: Vec<Value>) {
        Mock::given(method("GET"))
            .and(path("/api/people/search"))
            .and(query_param("q", q))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "source": "aufind", "upstream": format!("https://aufind.au.dk/searchPeople.php?q={q}"),
                "query": q, "total": total, "hits": hits})))
            .mount(s)
            .await;
    }

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn found_carries_the_raw_fields() {
        let s = server().await;
        search(
            &s,
            "Dennis Vittrup",
            1,
            vec![hit(
                "u1",
                "Dennis Vittrup",
                profile("Dennis Vittrup", "1872", "237"),
            )],
        )
        .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Dennis  Vittrup", "en")
            .await;
        assert_eq!(r.status, Status::Found);
        assert_eq!(r.people.len(), 1);
        let p = &r.people[0];
        assert_eq!(p["au_room"], "1872-237");
        assert_eq!(p["source"], "aufind");
        assert_eq!(p["map_url"], Value::Null);
        assert!(p["profile_source"]
            .as_str()
            .unwrap()
            .starts_with("www.au.dk https://www.au.dk/"));
        assert_eq!(
            r.tried,
            vec![Attempt {
                q: "Dennis Vittrup".into(),
                total: 1
            }]
        );
        assert_eq!(r.af_server_version.as_deref(), Some("0.3.0"));
        assert!(r.upstream.unwrap().starts_with("https://aufind.au.dk/"));
        assert!(r.more.is_none() && r.fallback.is_none());
    }

    #[tokio::test]
    async fn ambiguous_shows_three_and_fetches_missing_profiles() {
        let s = server().await;
        let hits: Vec<Value> = (0..5)
            .map(|i| {
                let p = if i == 2 {
                    Value::Null
                } else {
                    profile(&format!("Anna {i}"), "1532", "316")
                };
                hit(&format!("u{i}"), &format!("Anna {i}"), p)
            })
            .collect();
        search(&s, "Anna", 12, hits).await;
        Mock::given(method("GET"))
            .and(path("/api/people/u2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(profile("Anna 2", "1530", "101")),
            )
            .expect(1)
            .mount(&s)
            .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Anna", "en")
            .await;
        assert_eq!(r.status, Status::Ambiguous);
        assert_eq!(r.people.len(), 3);
        assert_eq!(r.people[2]["au_room"], "1530-101");
        assert_eq!(r.total, 12);
        assert_eq!(r.more, Some(9));
        s.verify().await;
    }

    #[tokio::test]
    async fn surname_retry() {
        let s = server().await;
        search(&s, "Dennis Zzz Vittrup", 0, vec![]).await;
        search(
            &s,
            "Vittrup",
            1,
            vec![hit(
                "u1",
                "Dennis Vittrup",
                profile("Dennis Vittrup", "1872", "237"),
            )],
        )
        .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Dennis Zzz Vittrup", "da")
            .await;
        assert_eq!(r.status, Status::Found);
        assert_eq!(
            r.tried,
            vec![
                Attempt {
                    q: "Dennis Zzz Vittrup".into(),
                    total: 0
                },
                Attempt {
                    q: "Vittrup".into(),
                    total: 1
                }
            ]
        );
    }

    #[tokio::test]
    async fn prefix_retry() {
        let s = server().await;
        search(&s, "Dennis Vittrupp", 0, vec![]).await;
        search(&s, "Vittrupp", 0, vec![]).await;
        search(
            &s,
            "Vittru",
            1,
            vec![hit(
                "u1",
                "Dennis Vittrup",
                profile("Dennis Vittrup", "1872", "237"),
            )],
        )
        .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Dennis Vittrupp", "en")
            .await;
        assert_eq!(r.status, Status::Found);
        assert_eq!(r.tried.len(), 3);
        assert_eq!(r.tried[2].q, "Vittru");
    }

    #[tokio::test]
    async fn not_found_after_three_attempts() {
        let s = server().await;
        Mock::given(method("GET"))
            .and(path("/api/people/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"source": "aufind", "total": 0, "hits": []})),
            )
            .mount(&s)
            .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Mario Rossiniani", "en")
            .await;
        assert_eq!(r.status, Status::NotFound);
        let qs: Vec<&str> = r.tried.iter().map(|a| a.q.as_str()).collect();
        assert_eq!(qs, ["Mario Rossiniani", "Rossiniani", "Rossinia"]);
        assert!(r.people.is_empty());
        assert_eq!(r.fallback.unwrap()["site"], "www.au.dk");
    }

    #[tokio::test]
    async fn unavailable_on_a_closed_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let h = http();
        let r = AuClient::new(&h, &url).people_search("Anna", "en").await;
        assert_eq!(r.status, Status::Unavailable);
        assert!(r.people.is_empty() && r.tried.is_empty());
        assert!(r.error.is_some());
        let b = AuClient::new(&h, &url).building("1532").await;
        assert_eq!(b.status, Status::Unavailable);
    }

    #[tokio::test]
    async fn digit_only_names_are_dropped() {
        let s = server().await;
        search(
            &s,
            "Vitt",
            2,
            vec![
                hit("x", "123456", Value::Null),
                hit(
                    "u1",
                    "Dennis Vittrup",
                    profile("Dennis Vittrup", "1872", "237"),
                ),
            ],
        )
        .await;
        let h = http();
        let r = AuClient::new(&h, &s.uri())
            .people_search("Vitt", "en")
            .await;
        assert_eq!(r.status, Status::Found);
        assert_eq!(r.total, 1);
        assert_eq!(r.people[0]["name"], "Dennis Vittrup");
    }

    #[tokio::test]
    async fn buildings_found_and_ambiguous() {
        let s = server().await;
        let b = |id: &str| json!({"id": id, "names": [format!("Bygning {id}")], "address": "Ny Munkegade", "latitude": 56.17, "longitude": 10.2});
        Mock::given(method("GET"))
            .and(path("/api/buildings"))
            .and(query_param("q", "bygning 1532"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([b("1532"), b("15320")])))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/buildings"))
            .and(query_param("q", "Nobel"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"buildings": [b("1481"), b("1482"), b("1483"), b("1485")]}),
                ),
            )
            .mount(&s)
            .await;
        let h = http();
        let c = AuClient::new(&h, &s.uri());
        let r = c.building("bygning 1532").await;
        assert_eq!(r.status, Status::Found);
        assert_eq!(r.buildings.len(), 1);
        assert_eq!(r.map.unwrap()["path"], "/v1/au/map.png?building=1532&z=17");
        let r = c.building("Nobel").await;
        assert_eq!(r.status, Status::Ambiguous);
        assert_eq!(r.buildings.len(), 3);
        assert!(r.map.is_none());
    }

    fn ctx(url: &str) -> Ctx {
        let mut env = ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        env.au_url = Some(url.into());
        Ctx::new(Arc::new(env), "ios", "s")
    }

    #[tokio::test]
    async fn campus_map_defaults_and_clamps() {
        let s = server().await;
        Mock::given(method("GET"))
            .and(path("/api/buildings/1532"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1532", "names": ["Bygning 1532"], "latitude": 56.168, "longitude": 10.203})))
            .mount(&s)
            .await;
        let c = ctx(&s.uri());
        let t = AuCampusMapTool;
        let v = t.call(json!({}), &c).await.unwrap();
        assert_eq!(v["zoom"], 15);
        assert_eq!(v["center"]["lat"], 56.1697);
        assert_eq!(v["image"], "/v1/au/map.png?lat=56.16970&lng=10.20390&z=15");
        let v = t.call(json!({"zoom": 30}), &c).await.unwrap();
        assert_eq!(v["zoom"], 18);
        let v = t
            .call(json!({"building": "bygning 1532", "zoom": 3}), &c)
            .await
            .unwrap();
        assert_eq!(v["zoom"], 14);
        assert_eq!(v["image"], "/v1/au/map.png?building=1532&z=14");
        assert_eq!(v["center"]["lng"], 10.203);
        assert_eq!(v["source"], MAP_SOURCE);
        let v = t.call(json!({"building": "1532"}), &c).await.unwrap();
        assert_eq!(v["zoom"], 17);
        let v = t.call(json!({"building": "9999"}), &c).await.unwrap();
        assert_eq!(v["status"], "not_found");
        assert!(t
            .call(json!({"latitude": 40.0, "longitude": 10.0}), &c)
            .await
            .is_err());
        let images = c
            .effects()
            .into_iter()
            .filter(|e| matches!(e, Effect::Image { .. }))
            .count();
        assert_eq!(images, 4);
    }

    #[test]
    fn building_ids() {
        assert_eq!(building_id_of("Building 1532").as_deref(), Some("1532"));
        assert_eq!(building_id_of("edificio 1872a").as_deref(), Some("1872A"));
        assert_eq!(building_id_of("Nobel Park"), None);
        assert!(!valid_building_id("../x") && !valid_building_id("1234567"));
    }
}

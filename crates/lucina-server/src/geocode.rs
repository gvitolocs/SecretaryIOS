//! Coordinates for place blocks (`LUCINA_MAPS_GEOCODE_URL`): the map server's
//! `GET /v1/maps/geocode?q=…&limit=1` (secretary-maps, backed by Google Places
//! on the server). Blocks without `lat`/`lng` get the first result for their
//! `address` (else `title`): one request per block, all at once, within a
//! 1.5 s deadline, behind a small in-memory cache. Any failure leaves the
//! block as it was. Addresses are never logged.
//!
//! Event places also get a travel time from home ([`Geocoder::add_travel`]): the map server's
//! `GET /v1/maps/routes?…&estimate=1` (Google, else the self-hosted OSRM, else a straight-line
//! estimate), home from `GET /v1/maps/config`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde_json::{json, Value};

const DEADLINE: Duration = Duration::from_millis(1500);
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
const CACHE_MAX: usize = 256;
const TRAVEL_DEADLINE: Duration = Duration::from_millis(2500);
const HOME_TTL: Duration = Duration::from_secs(3600);
/// Up to this straight-line distance the place screen (and Lucina) default to walking.
const WALK_MAX_M: f64 = 2000.0;

/// `LUCINA_MAPS_GEOCODE_URL` and the optional `LUCINA_MAPS_UPSTREAM_KEY_FILE`.
#[derive(Clone)]
pub struct GeocodeConfig {
    pub url: String,
    /// Sent as `X-Maps-Upstream-Key`.
    pub key: Option<String>,
}

impl GeocodeConfig {
    /// `None` without `LUCINA_MAPS_GEOCODE_URL` (geocoding off).
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Option<Self> {
        let url = get("LUCINA_MAPS_GEOCODE_URL")
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())?;
        let key = get("LUCINA_MAPS_UPSTREAM_KEY_FILE").and_then(|path| {
            std::fs::read_to_string(path.trim())
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
        Some(Self { url, key })
    }
}

/// Latitude and longitude.
type Coords = (f64, f64);

pub struct Geocoder {
    cfg: GeocodeConfig,
    http: reqwest::Client,
    /// Query -> first result (`None`: the geocoder found nothing).
    cache: Mutex<HashMap<String, (Option<Coords>, Instant)>>,
    /// The map server's home (`None`: none configured), fetched at most hourly.
    home: Mutex<Option<(Option<Coords>, Instant)>>,
}

impl Geocoder {
    pub fn new(cfg: GeocodeConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            cache: Mutex::new(HashMap::new()),
            home: Mutex::new(None),
        }
    }

    /// `…/v1/maps` when the geocode URL is the map server's `…/v1/maps/geocode`.
    fn maps_base(&self) -> Option<&str> {
        self.cfg.url.trim_end_matches('/').strip_suffix("/geocode")
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(url);
        match &self.cfg.key {
            Some(key) => req.header("X-Maps-Upstream-Key", key),
            None => req,
        }
    }

    async fn home(&self, base: &str) -> Option<Coords> {
        if let Some((home, at)) = *self.home.lock().unwrap_or_else(|e| e.into_inner()) {
            if at.elapsed() < HOME_TTL {
                return home;
            }
        }
        let resp = self.get(&format!("{base}/config")).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        let home = coords(&body["home"]);
        *self.home.lock().unwrap_or_else(|e| e.into_inner()) = Some((home, Instant::now()));
        home
    }

    /// Minutes, mode and whether it is an estimate, from home to `to`.
    async fn travel(
        &self,
        base: &str,
        home: Coords,
        to: Coords,
        arrive: Option<&str>,
    ) -> Option<(u64, &'static str, bool)> {
        let mut query = vec![
            ("from", format!("{},{}", home.0, home.1)),
            ("to", format!("{},{}", to.0, to.1)),
            ("estimate", "1".to_owned()),
        ];
        if let Some(a) = arrive {
            query.push(("arrive", a.to_owned()));
        }
        let resp = self
            .get(&format!("{base}/routes"))
            .query(&query)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        pick_route(&body, distance_m(home, to))
    }

    /// `travelMin`, `travelMode` and `travelApprox` on the event places that have coordinates,
    /// all within 2.5 s; any failure leaves a block without them.
    pub async fn add_travel(&self, blocks: &mut [Value]) {
        let Some(base) = self.maps_base().map(str::to_owned) else {
            return;
        };
        let deadline = tokio::time::Instant::now() + TRAVEL_DEADLINE;
        let Ok(Some(home)) = tokio::time::timeout_at(deadline, self.home(&base)).await else {
            return;
        };
        let now = chrono::Utc::now();
        let wanted: Vec<(usize, Coords, Option<String>)> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b["type"] == "place" && b["kind"] == "event")
            .filter_map(|(i, b)| {
                let arrive = b["when"]
                    .as_str()
                    .filter(|w| chrono::DateTime::parse_from_rfc3339(w).is_ok_and(|t| t > now))
                    .map(str::to_owned);
                coords(b).map(|c| (i, c, arrive))
            })
            .collect();
        let found = join_all(wanted.iter().map(|(i, to, arrive)| {
            let base = base.as_str();
            async move {
                tokio::time::timeout_at(deadline, self.travel(base, home, *to, arrive.as_deref()))
                    .await
                    .ok()
                    .flatten()
                    .map(|t| (*i, t))
            }
        }))
        .await;
        for (i, (minutes, mode, approx)) in found.into_iter().flatten() {
            blocks[i]["travelMin"] = json!(minutes);
            blocks[i]["travelMode"] = json!(mode);
            if approx {
                blocks[i]["travelApprox"] = json!(true);
            }
        }
    }

    fn cached(&self, q: &str) -> Option<Option<Coords>> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(q)
            .filter(|(_, at)| at.elapsed() < CACHE_TTL)
            .map(|(hit, _)| *hit)
    }

    fn remember(&self, q: &str, hit: Option<Coords>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(q.to_owned(), (hit, Instant::now()));
    }

    /// The first result's coordinates; `Err` on any failure (not cached).
    async fn lookup(&self, q: &str) -> Result<Option<Coords>, String> {
        if let Some(hit) = self.cached(q) {
            return Ok(hit);
        }
        let mut req = self
            .http
            .get(&self.cfg.url)
            .query(&[("q", q), ("limit", "1")]);
        if let Some(key) = &self.cfg.key {
            req = req.header("X-Maps-Upstream-Key", key);
        }
        let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
        if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            tracing::debug!(status = 503, "maps geocode provider unavailable");
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(format!("status {}", resp.status().as_u16()));
        }
        let body: Value = resp.json().await.map_err(|e| e.without_url().to_string())?;
        let first = &body["results"][0];
        // The block normalizer checks the pair (finite, in range, not (0,0)).
        let hit = lucina_store::rich::normalize_block(&json!({
            "type": "place", "title": "x", "lat": first["lat"], "lng": first["lng"],
        }))
        .and_then(|b| Some((b["lat"].as_f64()?, b["lng"].as_f64()?)));
        self.remember(q, hit);
        Ok(hit)
    }

    /// Fill `lat`/`lng` of the place blocks that lack them.
    pub async fn fill(&self, blocks: &mut [Value]) {
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let wanted: Vec<(usize, String)> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b["type"] == "place" && b.get("lat").is_none())
            .filter_map(|(i, b)| {
                [&b["address"], &b["title"]]
                    .into_iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .find(|s| !s.is_empty())
                    .map(|q| (i, q.to_owned()))
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        let found = join_all(wanted.iter().map(|(i, q)| async move {
            match tokio::time::timeout_at(deadline, self.lookup(q)).await {
                Ok(Ok(hit)) => hit.map(|c| (*i, c)),
                Ok(Err(error)) => {
                    tracing::debug!(%error, "maps geocode failed");
                    None
                }
                Err(_) => {
                    tracing::debug!("maps geocode timed out");
                    None
                }
            }
        }))
        .await;
        for (i, (lat, lng)) in found.into_iter().flatten() {
            blocks[i]["lat"] = json!(lat);
            blocks[i]["lng"] = json!(lng);
        }
    }
}

fn coords(v: &Value) -> Option<Coords> {
    let (lat, lng) = (v["lat"].as_f64()?, v["lng"].as_f64()?);
    (lat.is_finite() && lng.is_finite() && lat.abs() <= 90.0 && lng.abs() <= 180.0)
        .then_some((lat, lng))
}

/// Haversine metres.
fn distance_m(a: Coords, b: Coords) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dla, dln) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dln / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().min(1.0).asin()
}

/// The route the place screen would pick: walk up to 2 km in a straight line, else transit;
/// then the next best mode the server answered. Minutes rounded up, at least 1.
fn pick_route(body: &Value, straight_m: f64) -> Option<(u64, &'static str, bool)> {
    let order: [&'static str; 4] = if straight_m <= WALK_MAX_M {
        ["walk", "bike", "transit", "drive"]
    } else {
        ["transit", "bike", "drive", "walk"]
    };
    let routes = body["routes"].as_array()?;
    let estimate = body["provider"] == "estimate";
    order.iter().find_map(|mode| {
        let r = routes.iter().find(|r| r["mode"] == *mode)?;
        let seconds = r["durationS"].as_u64()?;
        let minutes = seconds.div_ceil(60).max(1);
        (minutes <= 1440).then_some((minutes, *mode, estimate || r["estimate"] == true))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn geocoder(server: &MockServer, key: Option<&str>) -> Geocoder {
        Geocoder::new(GeocodeConfig {
            url: format!("{}/v1/maps/geocode", server.uri()),
            key: key.map(str::to_owned),
        })
    }

    fn found(lat: f64, lng: f64) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(
            json!({"results": [{"name": "x", "address": "y", "lat": lat, "lng": lng}]}),
        )
    }

    #[tokio::test]
    async fn fills_missing_coordinates_by_address_then_title() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/maps/geocode"))
            .and(query_param("q", "Folkehuset Møllestien"))
            .and(query_param("limit", "1"))
            .and(header("X-Maps-Upstream-Key", "k1"))
            .respond_with(found(56.161_123_4, 10.2006))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("q", "Dokk1"))
            .respond_with(found(56.1535, 10.2142))
            .mount(&server)
            .await;
        let g = geocoder(&server, Some("k1"));
        let mut blocks = vec![
            json!({"type": "place", "title": "Hack Night", "address": "Folkehuset Møllestien", "kind": "event"}),
            json!({"type": "quote", "text": "x"}),
            json!({"type": "place", "title": "Dokk1", "lat": 1.0, "lng": 2.0}),
            json!({"type": "place", "title": "Dokk1", "address": " "}),
        ];
        g.fill(&mut blocks).await;
        assert_eq!(blocks[0]["lat"], 56.161123);
        assert_eq!(blocks[0]["lng"], 10.2006);
        assert_eq!(blocks[2]["lat"], 1.0, "known coordinates stay");
        assert_eq!(blocks[3]["lat"], 56.1535);
        // The second time comes from the cache (the mock expects one call).
        let mut again =
            vec![json!({"type": "place", "title": "H", "address": "Folkehuset Møllestien"})];
        g.fill(&mut again).await;
        assert_eq!(again[0]["lat"], 56.161123);
    }

    #[tokio::test]
    async fn failures_leave_the_blocks_alone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("q", "down"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(json!({"error": "provider_unavailable"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("q", "slow"))
            .respond_with(found(56.0, 10.0).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("q", "nothing"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("q", "null island"))
            .respond_with(found(0.0, 0.0))
            .mount(&server)
            .await;
        let g = geocoder(&server, None);
        let mut blocks: Vec<Value> = ["down", "slow", "nothing", "null island"]
            .iter()
            .map(|a| json!({"type": "place", "title": "t", "address": a}))
            .collect();
        let before = blocks.clone();
        let started = Instant::now();
        g.fill(&mut blocks).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(blocks, before);
    }

    #[tokio::test]
    async fn provider_unavailable_is_no_result_and_is_not_cached() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(json!({"error": "provider_unavailable"})),
            )
            .expect(2)
            .mount(&server)
            .await;
        let g = geocoder(&server, None);
        assert_eq!(g.lookup("private address").await.unwrap(), None);
        let mut blocks = vec![json!({"type": "place", "title": "t", "address": "private address"})];
        let before = blocks.clone();
        g.fill(&mut blocks).await;
        assert_eq!(blocks, before);
    }

    #[tokio::test]
    async fn requests_run_concurrently_with_one_total_deadline() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(found(56.0, 10.0).set_delay(Duration::from_millis(800)))
            .expect(3)
            .mount(&server)
            .await;
        let g = geocoder(&server, None);
        let mut blocks: Vec<Value> = ["a", "b", "c"]
            .iter()
            .map(|a| json!({"type": "place", "title": "t", "address": a}))
            .collect();
        let started = Instant::now();
        g.fill(&mut blocks).await;
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(blocks.iter().all(|b| b["lat"] == 56.0));
    }

    #[test]
    fn unset_url_means_no_geocoder() {
        assert!(GeocodeConfig::from_lookup(&|_: &str| None).is_none());
        let c = GeocodeConfig::from_lookup(&|k: &str| {
            (k == "LUCINA_MAPS_GEOCODE_URL")
                .then(|| "http://127.0.0.1:8840/v1/maps/geocode".to_owned())
        })
        .unwrap();
        assert!(c.key.is_none());
    }

    #[test]
    fn the_route_lucina_names() {
        let body = json!({"provider": "google", "routes": [
            {"mode": "drive", "durationS": 540}, {"mode": "walk", "durationS": 2950},
            {"mode": "bike", "durationS": 900}, {"mode": "transit", "durationS": 1081}]});
        assert_eq!(pick_route(&body, 3900.0), Some((19, "transit", false)));
        assert_eq!(pick_route(&body, 1500.0), Some((50, "walk", false)));
        let estimate = json!({"provider": "estimate", "routes": [
            {"mode": "drive", "durationS": 480, "estimate": true}, {"mode": "bike", "durationS": 1200, "estimate": true}],
            "failed": ["transit"]});
        assert_eq!(pick_route(&estimate, 3900.0), Some((20, "bike", true)));
        assert_eq!(pick_route(&json!({"routes": []}), 10.0), None);
    }

    #[tokio::test]
    async fn event_places_get_a_travel_time_from_home() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/maps/config"))
            .and(header("X-Maps-Upstream-Key", "k1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"home": {"lat": 56.19633, "lng": 10.2011, "label": "Home"}}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/maps/routes"))
            .and(query_param("from", "56.19633,10.2011"))
            .and(query_param("to", "56.1611,10.2006"))
            .and(query_param("estimate", "1"))
            .and(query_param("arrive", "2099-10-27T16:00:00.000Z"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"provider": "google",
                "routes": [{"mode": "transit", "durationS": 1081}, {"mode": "walk", "durationS": 2950}], "failed": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/maps/routes"))
            .and(query_param("to", "56.0,10.0"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let g = geocoder(&server, Some("k1"));
        let mut blocks = vec![
            json!({"type": "place", "kind": "event", "title": "Hack Night", "lat": 56.1611, "lng": 10.2006,
                   "when": "2099-10-27T16:00:00.000Z"}),
            json!({"type": "place", "kind": "event", "title": "Down", "lat": 56.0, "lng": 10.0}),
            json!({"type": "place", "kind": "place", "title": "Shop", "lat": 56.1611, "lng": 10.2006}),
            json!({"type": "place", "kind": "event", "title": "No coordinates", "address": "x"}),
        ];
        g.add_travel(&mut blocks).await;
        assert_eq!(
            (
                blocks[0]["travelMin"].clone(),
                blocks[0]["travelMode"].clone()
            ),
            (json!(19), json!("transit"))
        );
        assert!(blocks[0].get("travelApprox").is_none());
        for b in &blocks[1..] {
            assert!(b.get("travelMin").is_none(), "{b}");
        }
        // Home comes from the cache the second time (the mock expects one call).
        g.add_travel(&mut blocks[..1]).await;
    }

    #[tokio::test]
    async fn no_travel_without_the_map_server() {
        let g = Geocoder::new(GeocodeConfig {
            url: "http://127.0.0.1:1/somewhere".into(),
            key: None,
        });
        let mut blocks =
            vec![json!({"type": "place", "kind": "event", "title": "x", "lat": 56.1, "lng": 10.2})];
        g.add_travel(&mut blocks).await;
        assert!(blocks[0].get("travelMin").is_none());
    }
}

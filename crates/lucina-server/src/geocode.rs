//! Coordinates for place blocks (`LUCINA_MAPS_GEOCODE_URL`): the map server's
//! `GET /v1/maps/geocode?q=…&limit=1` (secretary-maps, backed by Google Places
//! on the server). Blocks without `lat`/`lng` get the first result for their
//! `address` (else `title`): one request per block, all at once, within a
//! 1.5 s deadline, behind a small in-memory cache. Any failure leaves the
//! block as it was. Addresses are never logged.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde_json::{json, Value};

const DEADLINE: Duration = Duration::from_millis(1500);
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
const CACHE_MAX: usize = 256;

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
}

impl Geocoder {
    pub fn new(cfg: GeocodeConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            cache: Mutex::new(HashMap::new()),
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
}

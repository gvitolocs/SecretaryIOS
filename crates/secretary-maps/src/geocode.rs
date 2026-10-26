//! Geocoding through a local Photon (`GET /api?q=&limit=&lat=&lon=&lang=en`), with an
//! in-memory LRU/TTL cache. The query text is private: callers log only its length.
use crate::kernel;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const CACHE_ENTRIES: usize = 512;
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeoResult {
    pub name: String,
    pub address: String,
    pub lat: f64,
    pub lng: f64,
}

/// Lowercased, whitespace-collapsed query + the bias rounded to 0.01° (about 1 km) + limit.
pub fn cache_key(q: &str, bias: Option<(i64, i64)>, limit: u8) -> String {
    let q = q
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let round = |v: i64| (v + 50_000).div_euclid(100_000);
    match bias {
        Some((lat, lng)) => format!("{limit}|{},{}|{q}", round(lat), round(lng)),
        None => format!("{limit}|-|{q}"),
    }
}

/// Fixed-capacity cache: entries expire after `ttl`; when full, the least recently used goes.
pub struct Cache {
    cap: usize,
    ttl: Duration,
    tick: u64,
    map: HashMap<String, (Instant, u64, Vec<GeoResult>)>,
}

impl Cache {
    pub fn new(cap: usize, ttl: Duration) -> Cache {
        Cache {
            cap,
            ttl,
            tick: 0,
            map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&mut self, key: &str) -> Option<Vec<GeoResult>> {
        self.get_at(key, Instant::now())
    }

    pub fn insert(&mut self, key: String, value: Vec<GeoResult>) {
        self.insert_at(key, value, Instant::now())
    }

    fn get_at(&mut self, key: &str, now: Instant) -> Option<Vec<GeoResult>> {
        self.tick += 1;
        let tick = self.tick;
        match self.map.get_mut(key) {
            Some((at, _, _)) if now.duration_since(*at) >= self.ttl => {
                self.map.remove(key);
                None
            }
            Some((_, used, v)) => {
                *used = tick;
                Some(v.clone())
            }
            None => None,
        }
    }

    fn insert_at(&mut self, key: String, value: Vec<GeoResult>, now: Instant) {
        self.tick += 1;
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            let ttl = self.ttl;
            self.map
                .retain(|_, (at, _, _)| now.duration_since(*at) < ttl);
            if self.map.len() >= self.cap {
                if let Some(oldest) = self
                    .map
                    .iter()
                    .min_by_key(|(_, (_, used, _))| *used)
                    .map(|(k, _)| k.clone())
                {
                    self.map.remove(&oldest);
                }
            }
        }
        self.map.insert(key, (now, self.tick, value));
    }
}

fn prop(p: &Value, key: &str) -> Option<String> {
    let s = kernel::one_line(p.get(key)?.as_str()?);
    (!s.is_empty()).then_some(s)
}

fn join_some(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    }
}

/// Photon's GeoJSON `FeatureCollection` to results: address = "street housenumber, postcode
/// city, country" without the missing parts (a house number without a street is skipped); name
/// falls back to the first address part. Features with invalid coordinates or neither a name
/// nor an address are dropped.
pub fn parse_features(body: &Value, limit: usize) -> Vec<GeoResult> {
    let Some(features) = body.get("features").and_then(Value::as_array) else {
        return Vec::new();
    };
    features
        .iter()
        .filter_map(|f| {
            let c = f.get("geometry")?.get("coordinates")?.as_array()?;
            let (lng, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
            let (lat_e7, lng_e7) = kernel::parse_coord(lat, lng)?;
            let p = f.get("properties").unwrap_or(&Value::Null);
            let street = prop(p, "street").map(|s| match prop(p, "housenumber") {
                Some(n) => format!("{s} {n}"),
                None => s,
            });
            let parts: Vec<String> = [
                street,
                join_some(prop(p, "postcode"), prop(p, "city")),
                prop(p, "country"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let name = prop(p, "name").or_else(|| parts.first().cloned())?;
            Some(GeoResult {
                name: kernel::clip(&name, kernel::TITLE_MAX_CHARS),
                address: kernel::clip(&parts.join(", "), kernel::ADDRESS_MAX_CHARS),
                lat: kernel::e7_to_deg(lat_e7),
                lng: kernel::e7_to_deg(lng_e7),
            })
        })
        .take(limit)
        .collect()
}

/// One Photon query (the client carries the 3 s timeout). Any transport error, non-2xx status or
/// non-JSON body is an error (the route answers 503).
pub async fn photon(
    http: &reqwest::Client,
    base: &str,
    q: &str,
    bias: Option<(i64, i64)>,
    limit: u8,
) -> Result<Vec<GeoResult>, String> {
    let mut params = vec![
        ("q", q.to_string()),
        ("limit", limit.to_string()),
        ("lang", "en".to_string()),
    ];
    if let Some((lat, lng)) = bias {
        params.push(("lat", kernel::e7_to_deg(lat).to_string()));
        params.push(("lon", kernel::e7_to_deg(lng).to_string()));
    }
    let res = http
        .get(format!("{base}/api"))
        .query(&params)
        .send()
        .await
        .map_err(|e| {
            // reqwest puts the URL (with q) in its errors: keep only the kind.
            if e.is_timeout() {
                "timeout".to_string()
            } else if e.is_connect() {
                "connect".to_string()
            } else {
                "request".to_string()
            }
        })?;
    if !res.status().is_success() {
        return Err(format!("status {}", res.status().as_u16()));
    }
    let body: Value = res.json().await.map_err(|_| "body".to_string())?;
    Ok(parse_features(&body, limit as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_normalize() {
        assert_eq!(
            cache_key("  Aarhus   C ", None, 3),
            cache_key("aarhus c", None, 3)
        );
        assert_ne!(cache_key("aarhus", None, 3), cache_key("aarhus", None, 4));
        let home = (561_963_300, 102_011_000);
        assert_eq!(
            cache_key("x", Some(home), 3),
            cache_key("x", Some((561_990_000, 101_960_000)), 3)
        );
        assert_ne!(
            cache_key("x", Some(home), 3),
            cache_key("x", Some((562_100_000, 102_011_000)), 3)
        );
        assert_eq!(cache_key("x", Some((-50_000, -150_000)), 1), "1|0,-1|x");
    }

    #[test]
    fn cache_ttl_and_lru() {
        let r = |n: &str| {
            vec![GeoResult {
                name: n.into(),
                address: String::new(),
                lat: 1.0,
                lng: 2.0,
            }]
        };
        let t0 = Instant::now();
        let mut c = Cache::new(2, Duration::from_secs(10));
        c.insert_at("a".into(), r("a"), t0);
        c.insert_at("b".into(), r("b"), t0);
        assert!(c.get_at("a", t0).is_some()); // b is now the least recently used
        c.insert_at("c".into(), r("c"), t0);
        assert_eq!(c.len(), 2);
        assert!(c.get_at("b", t0).is_none());
        assert!(c.get_at("a", t0).is_some() && c.get_at("c", t0).is_some());
        assert!(c.get_at("a", t0 + Duration::from_secs(10)).is_none());
        assert_eq!(c.len(), 1);
        // Expired entries go before a live one is evicted.
        c.insert_at("d".into(), r("d"), t0 + Duration::from_secs(5));
        c.insert_at("e".into(), r("e"), t0 + Duration::from_secs(11));
        assert!(c.get_at("d", t0 + Duration::from_secs(11)).is_some());
    }

    #[test]
    fn features_to_results() {
        let body = json!({"type":"FeatureCollection","features":[
            {"geometry":{"type":"Point","coordinates":[10.2006,56.1611]},
             "properties":{"name":"Folkehuset Møllestien","street":"Møllestien","housenumber":"65",
                           "postcode":"8000","city":"Aarhus","country":"Denmark"}},
            {"geometry":{"coordinates":[10.2011,56.19633]},
             "properties":{"street":"Skejbygårdsvej","housenumber":"3","postcode":"8240","city":"Risskov"}},
            {"geometry":{"coordinates":[0,0]},"properties":{"name":"Null Island"}},
            {"geometry":{"coordinates":[10.0]},"properties":{"name":"half"}},
            {"geometry":{"coordinates":[10.0,95.0]},"properties":{"name":"north of the pole"}},
            {"geometry":{"coordinates":[10.0,56.0]},"properties":{}},
            {"geometry":{"coordinates":[10.1,56.1]},"properties":{"name":"Aarhus","housenumber":"9","country":"Denmark"}}
        ]});
        let res = parse_features(&body, 5);
        assert_eq!(
            res,
            vec![
                GeoResult {
                    name: "Folkehuset Møllestien".into(),
                    address: "Møllestien 65, 8000 Aarhus, Denmark".into(),
                    lat: 56.1611,
                    lng: 10.2006
                },
                GeoResult {
                    name: "Skejbygårdsvej 3".into(),
                    address: "Skejbygårdsvej 3, 8240 Risskov".into(),
                    lat: 56.19633,
                    lng: 10.2011
                },
                GeoResult {
                    name: "Aarhus".into(),
                    address: "Denmark".into(),
                    lat: 56.1,
                    lng: 10.1
                },
            ]
        );
        assert_eq!(parse_features(&body, 1).len(), 1);
        assert!(parse_features(&json!({"nope":1}), 3).is_empty());
    }
}

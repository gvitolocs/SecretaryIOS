//! Deal sources (`sources.js`): Salling food waste (clearances, Bearer key)
//! and Tjek flyers (`/v2/offers/search`, `/v2/stores`). Responses are cached
//! per engine and radius (food waste 15 min, search terms 30 min, stores
//! 24 h); a request already in flight is shared; failures are never cached.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::{join_all, BoxFuture, FutureExt, Shared};
use regex::Regex;
use reqwest::Url;
use serde_json::{json, Map, Value};

use crate::js::{self, display, number, truthy, value_text};
use crate::matcher::{chain_key, is_grocery_chain};
use crate::validity::Status;
use crate::{Clock, DealsConfig};

/// A transport failure (status, network, body). Never carries the URL.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(pub String);

/// GET a JSON document. Implementations fail on a non-2xx status.
#[async_trait]
pub trait DealsHttp: Send + Sync {
    async fn get_json(&self, url: &Url, headers: &[(String, String)]) -> Result<Value, HttpError>;
}

/// The live transport.
#[derive(Debug, Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new(timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("http client"),
        }
    }
}

#[async_trait]
impl DealsHttp for ReqwestHttp {
    async fn get_json(&self, url: &Url, headers: &[(String, String)]) -> Result<Value, HttpError> {
        let mut request = self.client.get(url.clone());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| HttpError(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(HttpError("Deal source request failed".into()));
        }
        response
            .json()
            .await
            .map_err(|e| HttpError(e.without_url().to_string()))
    }
}

/// Offline transport over recorded responses (`fixtures.js`): food waste,
/// stores and `tjek-offers-<query>.json`; an unknown term has no offers and
/// nothing touches the network.
#[derive(Debug, Clone)]
pub struct FixtureHttp {
    dir: PathBuf,
}

impl FixtureHttp {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

#[async_trait]
impl DealsHttp for FixtureHttp {
    async fn get_json(&self, url: &Url, _headers: &[(String, String)]) -> Result<Value, HttpError> {
        let path = url.path();
        let filename = if path.contains("food-waste") {
            "salling-food-waste-risskov.json".to_owned()
        } else if path.ends_with("/stores") {
            "tjek-stores-risskov.json".to_owned()
        } else {
            let query = url
                .query_pairs()
                .find(|(k, _)| k == "query")
                .map_or_else(|| "null".to_owned(), |(_, v)| v.into_owned());
            format!("tjek-offers-{query}.json")
        };
        // Only names inside the fixture directory are readable.
        if filename.contains('/') || filename.contains('\\') || filename == ".." {
            return Err(HttpError("Invalid fixture term".into()));
        }
        match tokio::fs::read_to_string(self.dir.join(&filename)).await {
            Ok(text) => serde_json::from_str(&text).map_err(|e| HttpError(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Array(Vec::new())),
            Err(e) => Err(HttpError(e.to_string())),
        }
    }
}

/// A transport from a closure (tests: counting, failing, custom rows).
pub struct FnHttp<F>(pub F);

#[async_trait]
impl<F> DealsHttp for FnHttp<F>
where
    F: Fn(&Url) -> Result<Value, HttpError> + Send + Sync,
{
    async fn get_json(&self, url: &Url, _headers: &[(String, String)]) -> Result<Value, HttpError> {
        (self.0)(url)
    }
}

/// Great-circle distance in metres; `Infinity` without coordinates.
pub fn haversine(lat: f64, lng: f64, other_lat: f64, other_lng: f64) -> f64 {
    if ![lat, lng, other_lat, other_lng]
        .iter()
        .all(|n| n.is_finite())
    {
        return f64::INFINITY;
    }
    let rad = |n: f64| n * std::f64::consts::PI / 180.0;
    let s1 = rad(other_lat - lat) / 2.0;
    let s2 = rad(other_lng - lng) / 2.0;
    let a = s1.sin() * s1.sin() + rad(lat).cos() * rad(other_lat).cos() * (s2.sin() * s2.sin());
    // V8's Math.atan2 is fdlibm's; glibc's can differ in the last bit.
    6_371_000.0 * 2.0 * libm::atan2(a.sqrt(), (1.0 - a).sqrt())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DealSource {
    FoodWaste,
    Flyer,
}

impl DealSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DealSource::FoodWaste => "food_waste",
            DealSource::Flyer => "flyer",
        }
    }
}

/// Where an offer is: the clearance branch, or the dealer's nearest branch
/// for a flyer offer (then only name, street and distance). Raw source
/// values stay raw (`None` is JS `undefined`).
#[derive(Debug, Clone, PartialEq)]
pub struct DealStore {
    pub id: Option<Value>,
    pub name: Option<Value>,
    pub street: Option<Value>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub distance_m: f64,
}

impl DealStore {
    /// The Node object (`undefined` fields left out).
    pub fn to_json(&self) -> Value {
        let mut store = Map::new();
        opt_raw(&mut store, "id", &self.id);
        opt_raw(&mut store, "name", &self.name);
        opt_raw(&mut store, "street", &self.street);
        if let Some(lat) = self.lat {
            store.insert("lat".into(), js::num_value(lat));
        }
        if let Some(lng) = self.lng {
            store.insert("lng".into(), js::num_value(lng));
        }
        store.insert("distanceM".into(), js::num_value(self.distance_m));
        Value::Object(store)
    }
}

/// One offer, as the Node engine's deal object.
#[derive(Debug, Clone, PartialEq)]
pub struct Deal {
    pub key: String,
    pub source: DealSource,
    pub name: Option<Value>,
    pub chain: Option<Value>,
    pub chain_key: String,
    /// Flyers only.
    pub dealer_id: Option<Value>,
    pub store: DealStore,
    pub price_dkk: f64,
    pub normal_price_dkk: Option<f64>,
    pub discount_pct: Option<f64>,
    pub stock: Option<f64>,
    pub stock_unit: Option<Value>,
    pub valid_from: Option<Value>,
    pub valid_to: Option<Value>,
    /// Food waste only.
    pub ean: Option<Value>,
    /// Food waste only.
    pub category: Option<String>,
    /// Flyers only (`None` is `null` there).
    pub unit_price: Option<String>,
    pub status: Status,
    pub score: i32,
    /// Set once grouped (list answers): every branch's key of this offer.
    pub related_keys: Option<Vec<String>>,
    pub more_stores: Option<u32>,
}

fn opt_raw(map: &mut Map<String, Value>, key: &str, value: &Option<Value>) {
    if let Some(v) = value {
        map.insert(key.into(), v.clone());
    }
}

impl Deal {
    /// `String(deal.name || '')`, what the matcher reads.
    pub fn name_text(&self) -> String {
        if truthy(self.name.as_ref()) {
            display(self.name.as_ref())
        } else {
            String::new()
        }
    }

    pub fn valid_from_str(&self) -> Option<&str> {
        self.valid_from.as_ref().and_then(Value::as_str)
    }

    pub fn valid_to_str(&self) -> Option<&str> {
        self.valid_to.as_ref().and_then(Value::as_str)
    }

    /// The public map shape: no source-internal store or match score.
    pub fn public_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("key".into(), Value::String(self.key.clone()));
        m.insert("source".into(), self.source.as_str().into());
        opt_raw(&mut m, "name", &self.name);
        m.insert("priceDkk".into(), js::num_value(self.price_dkk));
        m.insert(
            "normalPriceDkk".into(),
            js::opt_num_value(self.normal_price_dkk),
        );
        m.insert("discountPct".into(), js::opt_num_value(self.discount_pct));
        m.insert(
            "unitPrice".into(),
            self.unit_price.clone().map_or(Value::Null, Value::String),
        );
        m.insert("stock".into(), js::opt_num_value(self.stock));
        m.insert(
            "stockUnit".into(),
            self.stock_unit.clone().unwrap_or(Value::Null),
        );
        opt_raw(&mut m, "validFrom", &self.valid_from);
        opt_raw(&mut m, "validTo", &self.valid_to);
        m.insert("status".into(), self.status.as_str().into());
        m.insert(
            "category".into(),
            self.category.clone().map_or(Value::Null, Value::String),
        );
        Value::Object(m)
    }

    /// The whole Node deal object (`JSON.stringify` of it), for parity checks
    /// and debugging.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("key".into(), Value::String(self.key.clone()));
        m.insert("source".into(), self.source.as_str().into());
        opt_raw(&mut m, "name", &self.name);
        opt_raw(&mut m, "chain", &self.chain);
        m.insert("chainKey".into(), Value::String(self.chain_key.clone()));
        m.insert("priceDkk".into(), js::num_value(self.price_dkk));
        m.insert(
            "normalPriceDkk".into(),
            js::opt_num_value(self.normal_price_dkk),
        );
        m.insert("discountPct".into(), js::opt_num_value(self.discount_pct));
        m.insert("stock".into(), js::opt_num_value(self.stock));
        opt_raw(&mut m, "stockUnit", &self.stock_unit);
        opt_raw(&mut m, "validFrom", &self.valid_from);
        opt_raw(&mut m, "validTo", &self.valid_to);
        match self.source {
            DealSource::FoodWaste => {
                opt_raw(&mut m, "ean", &self.ean);
                if let Some(c) = &self.category {
                    m.insert("category".into(), Value::String(c.clone()));
                }
            }
            DealSource::Flyer => {
                opt_raw(&mut m, "dealerId", &self.dealer_id);
                m.insert(
                    "unitPrice".into(),
                    self.unit_price.clone().map_or(Value::Null, Value::String),
                );
            }
        }
        m.insert("store".into(), self.store.to_json());
        m.insert("status".into(), self.status.as_str().into());
        m.insert("score".into(), json!(self.score));
        if let Some(keys) = &self.related_keys {
            m.insert("relatedKeys".into(), json!(keys));
        }
        if let Some(more) = self.more_stores {
            m.insert("moreStores".into(), json!(more));
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSource {
    FoodWaste,
    Tjek,
}

/// One grocery branch in the radius (map data).
#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    pub source: BranchSource,
    /// `salling:<id>` or `tjek:<id>`.
    pub id: String,
    pub source_id: Option<Value>,
    /// Tjek only.
    pub dealer_id: Option<Value>,
    pub chain: Option<Value>,
    pub chain_key: String,
    pub name: Option<Value>,
    /// `null` when the source has none.
    pub street: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub distance_m: f64,
}

/// Flyer offers for some search terms; `failed` when a term's request failed.
#[derive(Debug, Clone, Default)]
pub struct Flyers {
    pub deals: Vec<Deal>,
    pub failed: bool,
}

/// A source failure (the engine reports it per source).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct SourceError(pub String);

fn shape_error() -> SourceError {
    SourceError("Invalid deal source response".into())
}

type Rows = Arc<Vec<Value>>;
type Pending = Shared<BoxFuture<'static, Result<Rows, String>>>;

/// A field of a JS object (`undefined` for anything else).
fn field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match value {
        Some(Value::Object(m)) => m.get(key),
        _ => None,
    }
}

/// `value.key` where `value` must not be `null`/`undefined` (else JS throws).
fn strict_field<'a>(value: Option<&'a Value>, key: &str) -> Result<Option<&'a Value>, SourceError> {
    match value {
        None | Some(Value::Null) => Err(shape_error()),
        v => Ok(field(v, key)),
    }
}

fn street_of(value: Option<&Value>) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\s*\n\s*").expect("street regex"));
    let text = if truthy(value) {
        display(value)
    } else {
        String::new()
    };
    let s = re.replace_all(&text, ", ");
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    (!s.is_empty()).then(|| s.to_owned())
}

fn coordinate(store: Option<&Value>, index: usize) -> Option<f64> {
    match field(store, "coordinates") {
        Some(Value::Array(items)) => number(items.get(index)),
        Some(Value::Object(m)) => number(m.get(&index.to_string())),
        _ => None,
    }
}

fn first_truthy<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    values.iter().copied().find(|v| truthy(*v)).flatten()
}

/// JS `a === b` on two raw values.
fn strict_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        (None, None) => true,
        _ => false,
    }
}

/// A map key for a raw id (JS `Map` keys compare by value and type).
pub(crate) fn id_key(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::Number(n)) => {
            format!("n:{}", js::num_to_string(n.as_f64().unwrap_or(f64::NAN)))
        }
        Some(v) => v.to_string(),
    }
}

fn js_num_param(value: Option<f64>) -> String {
    value.map_or_else(|| "null".into(), js::num_to_string)
}

/// The cached, request-sharing source reader of one engine.
pub struct Sources {
    cfg: Arc<DealsConfig>,
    http: Arc<dyn DealsHttp>,
    clock: Clock,
    cache: Arc<Mutex<HashMap<String, (i64, Rows)>>>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources").finish_non_exhaustive()
    }
}

impl Sources {
    pub fn new(cfg: Arc<DealsConfig>, http: Arc<dyn DealsHttp>, clock: Clock) -> Self {
        Self {
            cfg,
            http,
            clock,
            cache: Arc::default(),
            pending: Mutex::default(),
        }
    }

    pub(crate) fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    async fn get(
        &self,
        key: String,
        url: Result<Url, SourceError>,
        ttl_ms: i64,
        headers: Vec<(String, String)>,
    ) -> Result<Rows, SourceError> {
        let url = url?;
        let request = {
            let now = (self.clock)();
            if let Some((at, rows)) = self.cache.lock().expect("cache lock").get(&key) {
                if now - at < ttl_ms {
                    return Ok(rows.clone());
                }
            }
            let mut pending = self.pending.lock().expect("pending lock");
            // A map request asks for deals and store lists together: share one request.
            if let Some(request) = pending.get(&key) {
                request.clone()
            } else {
                let (http, cache, clock, k) = (
                    self.http.clone(),
                    self.cache.clone(),
                    self.clock.clone(),
                    key.clone(),
                );
                let request = async move {
                    let value = http.get_json(&url, &headers).await.map_err(|e| e.0)?;
                    let Value::Array(rows) = value else {
                        return Err("Invalid deal source response".to_owned());
                    };
                    let rows = Arc::new(rows);
                    cache
                        .lock()
                        .expect("cache lock")
                        .insert(k, (clock(), rows.clone()));
                    Ok(rows)
                }
                .boxed()
                .shared();
                pending.insert(key.clone(), request.clone());
                request
            }
        };
        let result = request.clone().await;
        let mut pending = self.pending.lock().expect("pending lock");
        if pending.get(&key).is_some_and(|p| p.ptr_eq(&request)) {
            pending.remove(&key);
        }
        result.map_err(SourceError)
    }

    fn tjek_url(
        &self,
        path: &str,
        radius: f64,
        extra: &[(&str, &str)],
    ) -> Result<Url, SourceError> {
        let base = &self.cfg.tjek_base_url;
        let base = base.strip_suffix('/').unwrap_or(base);
        let mut url = Url::parse(&format!("{base}/v2/{path}"))
            .map_err(|_| SourceError("Invalid deal source URL".into()))?;
        url.set_query(None);
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("r_lat", &js_num_param(self.cfg.home_lat));
            q.append_pair("r_lng", &js_num_param(self.cfg.home_lng));
            q.append_pair("r_radius", &js::num_to_string(radius));
            q.append_pair("limit", "100");
            for (k, v) in extra {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }

    fn food_waste_url(&self, radius: f64) -> Result<Url, SourceError> {
        let base = &self.cfg.salling_base_url;
        let base = base.strip_suffix('/').unwrap_or(base);
        let mut url = Url::parse(&format!("{base}/v1/food-waste/"))
            .map_err(|_| SourceError("Invalid deal source URL".into()))?;
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair(
                "geo",
                &format!(
                    "{},{}",
                    js_num_param(self.cfg.home_lat),
                    js_num_param(self.cfg.home_lng)
                ),
            )
            .append_pair("radius", &js::num_to_string(radius / 1000.0));
        Ok(url)
    }

    async fn food_waste_rows(&self, radius: f64) -> Result<Rows, SourceError> {
        let Some(key) = self.cfg.salling_api_key.as_ref().filter(|k| !k.is_empty()) else {
            return Err(SourceError("Food waste not configured".into()));
        };
        self.get(
            format!("fw:{}", js::num_to_string(radius)),
            self.food_waste_url(radius),
            900_000,
            vec![("authorization".into(), format!("Bearer {key}"))],
        )
        .await
    }

    fn home_distance(&self, lat: f64, lng: f64) -> f64 {
        haversine(
            self.cfg.home_lat.unwrap_or(f64::NAN),
            self.cfg.home_lng.unwrap_or(f64::NAN),
            lat,
            lng,
        )
    }

    /// Clearances with stock at the food-waste branches in the radius.
    pub async fn food_waste(&self, radius: f64) -> Result<Vec<Deal>, SourceError> {
        let rows = self.food_waste_rows(radius).await?;
        let mut deals = Vec::new();
        for row in rows.iter() {
            if !row.is_object() {
                return Err(shape_error());
            }
            let store = strict_field(Some(row), "store")?;
            if store.is_none() || store == Some(&Value::Null) {
                return Err(shape_error());
            }
            let (Some(lat), Some(lng)) = (coordinate(store, 1), coordinate(store, 0)) else {
                continue;
            };
            let distance_m = self.home_distance(lat, lng);
            if distance_m > radius {
                continue;
            }
            let clearances = match field(Some(row), "clearances") {
                None => continue,
                Some(Value::Array(items)) => items,
                Some(_) => return Err(shape_error()),
            };
            for clearance in clearances {
                let o = strict_field(Some(clearance), "offer")?;
                let p = strict_field(Some(clearance), "product")?;
                let o_ean = strict_field(o, "ean")?;
                let ean_for_key = if truthy(o_ean) {
                    o_ean
                } else {
                    strict_field(p, "ean")?
                };
                let p_ean = strict_field(p, "ean")?;
                let price = number(field(o, "newPrice"));
                let stock = number(field(o, "stock"));
                let Some(price_dkk) = price else { continue };
                if !stock.is_some_and(|s| s > 0.0) {
                    continue;
                }
                let brand = field(store, "brand");
                deals.push(Deal {
                    key: format!(
                        "fw:{}:{}",
                        display(field(store, "id")),
                        display(ean_for_key)
                    ),
                    source: DealSource::FoodWaste,
                    name: field(p, "description").cloned(),
                    chain: brand.cloned(),
                    chain_key: chain_key(&truthy_text(brand)),
                    dealer_id: None,
                    store: DealStore {
                        id: field(store, "id").cloned(),
                        name: field(store, "name").cloned(),
                        street: field(field(store, "address"), "street").cloned(),
                        lat: Some(lat),
                        lng: Some(lng),
                        distance_m,
                    },
                    price_dkk,
                    normal_price_dkk: number(field(o, "originalPrice")),
                    discount_pct: number(field(o, "percentDiscount")),
                    stock,
                    stock_unit: field(o, "stockUnit").cloned(),
                    valid_from: field(o, "startTime").cloned(),
                    valid_to: field(o, "endTime").cloned(),
                    ean: if truthy(p_ean) { p_ean } else { o_ean }.cloned(),
                    category: js::opt_string(field(field(p, "categories"), "da")),
                    unit_price: None,
                    status: Status::Active,
                    score: 0,
                    related_keys: None,
                    more_stores: None,
                });
            }
        }
        Ok(deals)
    }

    /// Every grocery branch in the radius, from the same cached Salling response.
    pub async fn food_waste_stores(&self, radius: f64) -> Result<Vec<Branch>, SourceError> {
        let rows = self.food_waste_rows(radius).await?;
        let mut out = Vec::new();
        for row in rows.iter() {
            if !row.is_object() {
                return Err(shape_error());
            }
            let store = field(Some(row), "store");
            if store.is_none() || store == Some(&Value::Null) {
                return Err(shape_error());
            }
            let brand = field(store, "brand");
            let key = chain_key(&truthy_text(brand));
            let (Some(lat), Some(lng)) = (coordinate(store, 1), coordinate(store, 0)) else {
                continue;
            };
            let distance_m = self.home_distance(lat, lng);
            if !is_grocery_chain(&key) || distance_m > radius {
                continue;
            }
            let id = field(store, "id");
            out.push(Branch {
                source: BranchSource::FoodWaste,
                id: format!("salling:{}", display(id)),
                source_id: id.cloned(),
                dealer_id: None,
                chain: brand.cloned(),
                chain_key: key,
                name: field(store, "name").cloned(),
                street: street_of(field(field(store, "address"), "street")),
                lat,
                lng,
                distance_m,
            });
        }
        Ok(out)
    }

    /// Every grocery branch in the radius, from the same cached Tjek stores response.
    pub async fn tjek_stores(&self, radius: f64) -> Result<Vec<Branch>, SourceError> {
        let rows = self
            .get(
                format!("stores:{}", js::num_to_string(radius)),
                self.tjek_url("stores", radius, &[("order_by", "distance")]),
                86_400_000,
                Vec::new(),
            )
            .await?;
        let mut out = Vec::new();
        for s in rows.iter() {
            if s.is_null() {
                return Err(shape_error());
            }
            let s = Some(s);
            let chain = first_truthy(&[
                field(field(s, "branding"), "name"),
                field(field(s, "dealer"), "name"),
            ])
            .map(value_text)
            .unwrap_or_default();
            let key = chain_key(&chain);
            let (Some(lat), Some(lng)) =
                (number(field(s, "latitude")), number(field(s, "longitude")))
            else {
                continue;
            };
            let distance_m = self.home_distance(lat, lng);
            if !is_grocery_chain(&key) || distance_m > radius {
                continue;
            }
            out.push(Branch {
                source: BranchSource::Tjek,
                id: format!("tjek:{}", display(field(s, "id"))),
                source_id: field(s, "id").cloned(),
                dealer_id: field(s, "dealer_id").cloned(),
                chain: Some(Value::String(chain.clone())),
                chain_key: key,
                name: Some(Value::String(chain)),
                street: street_of(field(s, "street")),
                lat,
                lng,
                distance_m,
            });
        }
        Ok(out)
    }

    /// Flyer offers for the terms at grocery dealers with a branch in the radius.
    pub async fn flyers(&self, terms: &[String], radius: f64) -> Result<Flyers, SourceError> {
        let branches = self.tjek_stores(radius).await?;
        let mut nearest: HashMap<String, DealStore> = HashMap::new();
        for s in &branches {
            let k = id_key(s.dealer_id.as_ref());
            if nearest
                .get(&k)
                .is_none_or(|old| old.distance_m > s.distance_m)
            {
                nearest.insert(
                    k,
                    DealStore {
                        id: None,
                        name: s.chain.clone(),
                        street: Some(s.street.clone().map_or(Value::Null, Value::String)),
                        lat: None,
                        lng: None,
                        distance_m: s.distance_m,
                    },
                );
            }
        }
        let r = js::num_to_string(radius);
        let results = join_all(terms.iter().map(|query| {
            self.get(
                format!("term:{r}:{query}"),
                self.tjek_url("offers/search", radius, &[("query", query)]),
                1_800_000,
                Vec::new(),
            )
        }))
        .await;
        let failed = results.iter().any(Result::is_err);
        let mut deals = Vec::new();
        for rows in results.iter().flatten() {
            for o in rows.iter() {
                if o.is_null() {
                    return Err(shape_error());
                }
                let o = Some(o);
                let store = nearest.get(&id_key(field(o, "dealer_id")));
                let chain = first_truthy(&[
                    field(field(o, "dealer"), "name"),
                    store.and_then(|s| s.name.as_ref()),
                ])
                .or_else(|| store.and_then(|s| s.name.as_ref()));
                let Some(store) = store else { continue };
                let pricing = field(o, "pricing");
                let Some(price_dkk) = number(field(pricing, "price")) else {
                    continue;
                };
                if !is_grocery_chain(&chain_key(&truthy_text(chain))) {
                    continue;
                }
                let normal_price_dkk = number(field(pricing, "pre_price"));
                let q = field(o, "quantity");
                let unit_field = field(q, "unit");
                let unit = match field(unit_field, "symbol") {
                    Some(Value::Null) | None => unit_field,
                    symbol => symbol,
                };
                let unit_s = unit.and_then(Value::as_str);
                let size = number(field(field(q, "size"), "from"));
                let pieces = number(field(field(q, "pieces"), "from"))
                    .filter(|p| *p != 0.0)
                    .unwrap_or(1.0);
                let divisor = match unit.map(value_text).as_deref() {
                    Some("g") | Some("ml") => 1000.0,
                    Some("cl") => 100.0,
                    _ => 1.0,
                };
                let amount = match size {
                    Some(size)
                        if size != 0.0
                            && strict_eq(
                                field(field(q, "size"), "from"),
                                field(field(q, "size"), "to"),
                            ) =>
                    {
                        Some(size * pieces / divisor)
                    }
                    _ => None,
                };
                let unit_price = match (amount, unit_s) {
                    (Some(amount), Some(u))
                        if amount != 0.0
                            && !amount.is_nan()
                            && ["g", "kg", "ml", "cl", "l"].contains(&u) =>
                    {
                        Some(format!(
                            "{} DKK/{}",
                            js::to_fixed(price_dkk / amount, 2),
                            if ["g", "kg"].contains(&u) { "kg" } else { "l" }
                        ))
                    }
                    _ => None,
                };
                let normal = normal_price_dkk.unwrap_or(0.0);
                deals.push(Deal {
                    key: format!("tjek:{}", display(field(o, "id"))),
                    source: DealSource::Flyer,
                    name: field(o, "heading").cloned(),
                    chain: chain.cloned(),
                    chain_key: chain_key(&truthy_text(chain)),
                    dealer_id: field(o, "dealer_id").cloned(),
                    store: store.clone(),
                    price_dkk,
                    normal_price_dkk,
                    discount_pct: (normal > price_dkk).then(|| 100.0 * (1.0 - price_dkk / normal)),
                    stock: None,
                    stock_unit: Some(Value::Null),
                    valid_from: field(o, "run_from").cloned(),
                    valid_to: field(o, "run_till").cloned(),
                    ean: None,
                    category: None,
                    unit_price,
                    status: Status::Active,
                    score: 0,
                    related_keys: None,
                    more_stores: None,
                });
            }
        }
        Ok(Flyers { deals, failed })
    }
}

/// `String(value || '')`.
fn truthy_text(value: Option<&Value>) -> String {
    if truthy(value) {
        display(value)
    } else {
        String::new()
    }
}

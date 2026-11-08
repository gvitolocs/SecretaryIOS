//! `secretary-maps`: self-hosted maps for the Secretary apps (docs: README.md, contract in
//! docs/verification.md protocol 15).
//!
//! Routes under `/v1/maps`: `health`, `config`, `tiles/<name>.pmtiles` (HTTP range),
//! `assets/<path>` (fonts and sprites), `geocode` and `routes` (Google server APIs when a key is
//! loaded; the self-hosted gazetteer, Photon and OSRM and straight-line estimates otherwise).
//! p1's Caddy authenticates the owner (forward_auth) and, when the service is started with
//! an upstream key, adds `X-Maps-Upstream-Key`.
pub mod aufind;
pub mod cache;
pub mod gazetteer;
pub mod geocode;
pub mod kernel;
pub mod osrm;
pub mod photon;
pub mod provider;
pub mod routes;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub const ATTRIBUTION: &str = "© OpenStreetMap contributors · Protomaps";
const TILES_CACHE: &str = "private, max-age=86400";
const ASSETS_CACHE: &str = "private, max-age=604800";

#[derive(Debug, Clone, PartialEq)]
pub struct Home {
    pub lat_e7: i64,
    pub lng_e7: i64,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dir: PathBuf,
    pub tiles: String,
    pub home: Option<Home>,
    pub google_key: Option<provider::Secret>,
    /// Client keys the apps render Google Maps with (`/v1/maps/config`): web (referrer
    /// restricted) and iOS (bundle restricted).
    pub google_web_key: Option<provider::ClientKey>,
    pub google_ios_key: Option<provider::ClientKey>,
    pub places_url: String,
    pub routes_url: String,
    /// Self-hosted Photon (`MAPS_PHOTON_URL`): geocoding when Google is missing or fails.
    pub photon_url: Option<String>,
    /// AU Find building directory (`MAPS_AUFIND_URL`), before the local gazetteer.
    pub aufind_url: Option<String>,
    /// Self-hosted OSRM (`MAPS_OSRM_URL`): drive/walk/bike routes when Google is missing or fails.
    pub osrm_url: Option<String>,
    pub state_dir: PathBuf,
    pub upstream_key: Option<String>,
}

impl Config {
    /// Reads `MAPS_DIR`, `MAPS_TILES`, `MAPS_HOME_LAT`/`_LNG`/`_LABEL`, `GOOGLE_MAPS_SERVER_KEY`
    /// or `MAPS_GOOGLE_KEY_FILE`, the client keys `GOOGLE_MAPS_WEB_KEY` / `MAPS_GOOGLE_WEB_KEY_FILE`
    /// and `GOOGLE_MAPS_IOS_KEY` / `MAPS_GOOGLE_IOS_KEY_FILE`, `MAPS_PHOTON_URL`, `MAPS_AUFIND_URL`, `MAPS_OSRM_URL`
    /// and `MAPS_UPSTREAM_KEY_FILE` through `var` (`std::env::var` in the binary). No Google key
    /// is ever required: a missing, empty, unreadable (or, for client keys, malformed) key is
    /// only not configured.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let get = |k: &str| {
            var(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let dir = match get("MAPS_DIR") {
            Some(d) => PathBuf::from(d),
            None => {
                PathBuf::from(var("HOME").ok_or("MAPS_DIR unset and no HOME")?).join("data/maps")
            }
        };
        let tiles = get("MAPS_TILES").unwrap_or_else(|| "region".into());
        if !kernel::valid_tiles_name(&tiles) {
            return Err(format!("MAPS_TILES {tiles:?}: expected [a-z0-9-]{{1,40}}"));
        }
        let home = match (get("MAPS_HOME_LAT"), get("MAPS_HOME_LNG")) {
            (Some(lat), Some(lng)) => match (lat.parse::<f64>(), lng.parse::<f64>()) {
                (Ok(lat), Ok(lng)) => kernel::parse_coord(lat, lng).map(|(lat_e7, lng_e7)| Home {
                    lat_e7,
                    lng_e7,
                    label: kernel::clip(
                        &kernel::one_line(&get("MAPS_HOME_LABEL").unwrap_or_else(|| "Home".into())),
                        kernel::TITLE_MAX_CHARS,
                    ),
                }),
                _ => None,
            },
            _ => None,
        };
        let home_dir = var("HOME").map(PathBuf::from).unwrap_or_default();
        // A key: the secret's own name (Infisical) wins over the key file; trimmed; missing,
        // empty or unreadable is `None`, never an error.
        let read_key = |secret: &str, file_var: &str, default_file: &str| {
            if let Some(k) = get(secret) {
                return Some(k);
            }
            let path = get(file_var)
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir.join(".config/secretary-maps").join(default_file));
            match std::fs::read_to_string(path) {
                Ok(k) => Some(k.trim().to_string()).filter(|k| !k.is_empty()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    tracing::warn!(key = secret, "key file unreadable: key not configured");
                    None
                }
            }
        };
        let google_key = read_key(
            "GOOGLE_MAPS_SERVER_KEY",
            "MAPS_GOOGLE_KEY_FILE",
            "google.key",
        )
        .and_then(|k| provider::Secret::new(&k));
        // Client keys for the apps' Google map; a malformed key is logged (never its value)
        // and served as null.
        let client_key = |secret: &str, file_var: &str, default_file: &str| {
            let k = read_key(secret, file_var, default_file)?;
            let key = provider::ClientKey::new(&k);
            if key.is_none() {
                tracing::warn!(key = secret, "client key malformed: not configured");
            }
            key
        };
        let google_web_key = client_key(
            "GOOGLE_MAPS_WEB_KEY",
            "MAPS_GOOGLE_WEB_KEY_FILE",
            "google-web.key",
        );
        let google_ios_key = client_key(
            "GOOGLE_MAPS_IOS_KEY",
            "MAPS_GOOGLE_IOS_KEY_FILE",
            "google-ios.key",
        );
        let base_url = |k: &str| get(k).map(|u| u.trim_end_matches('/').to_string());
        let photon_url = base_url("MAPS_PHOTON_URL");
        let aufind_url = base_url("MAPS_AUFIND_URL");
        let osrm_url = base_url("MAPS_OSRM_URL");
        let places_url = get("MAPS_GOOGLE_PLACES_URL")
            .map(|url| provider::endpoint(&url, "v1/places:searchText"))
            .unwrap_or_else(|| "https://places.googleapis.com/v1/places:searchText".into());
        let routes_url = get("MAPS_GOOGLE_ROUTES_URL")
            .map(|url| provider::endpoint(&url, "directions/v2:computeRoutes"))
            .unwrap_or_else(|| "https://routes.googleapis.com/directions/v2:computeRoutes".into());
        let state_dir = get("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".local/state"))
            .join("secretary-maps");
        let upstream_key = match get("MAPS_UPSTREAM_KEY_FILE") {
            None => None,
            Some(path) => {
                let key = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .trim()
                    .to_string();
                if key.len() < 16 {
                    return Err("upstream key shorter than 16 characters".into());
                }
                Some(key)
            }
        };
        Ok(Config {
            dir,
            tiles,
            home,
            google_key,
            google_web_key,
            google_ios_key,
            places_url,
            routes_url,
            photon_url,
            aufind_url,
            osrm_url,
            state_dir,
            upstream_key,
        })
    }

    fn tiles_path(&self, name: &str) -> PathBuf {
        self.dir.join("tiles").join(format!("{name}.pmtiles"))
    }

    fn assets_dir(&self) -> PathBuf {
        self.dir.join("assets")
    }
}

pub struct AppState {
    pub config: Config,
    http: reqwest::Client,
    cache: Mutex<cache::Cache>,
    gazetteer: gazetteer::Gazetteer,
}

impl AppState {
    pub fn new(config: Config) -> Arc<AppState> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .connect_timeout(Duration::from_secs(4))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        let cache = Mutex::new(cache::Cache::load(config.state_dir.join("cache.json")));
        let gazetteer = gazetteer::Gazetteer::load(&config.dir.join("gazetteer"));
        tracing::info!(buildings = gazetteer.len(), "gazetteer loaded");
        Arc::new(AppState {
            config,
            http,
            cache,
            gazetteer,
        })
    }

    fn tiles_present(&self) -> bool {
        self.config.tiles_path(&self.config.tiles).is_file()
    }

    /// A geocoder beyond the gazetteer: Google or Photon.
    fn geocoder(&self) -> bool {
        self.config.google_key.is_some() || self.config.photon_url.is_some()
    }

    /// A road router: Google or OSRM (estimates are not routes).
    fn router(&self) -> bool {
        self.config.google_key.is_some() || self.config.osrm_url.is_some()
    }
}

/// The whole service, with the upstream-key check when configured.
pub fn app(state: Arc<AppState>) -> Router {
    let key = state.config.upstream_key.clone();
    let router = Router::new()
        .route("/v1/maps/health", get(health))
        .route("/v1/maps/config", get(config))
        .route("/v1/maps/tiles/{file}", get(tiles))
        .route("/v1/maps/assets/{*path}", get(assets))
        .route("/v1/maps/geocode", get(geocode_route))
        .route("/v1/maps/routes", get(routes::routes_route))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state);
    match key {
        Some(key) => {
            let key = Arc::new(key);
            router.layer(middleware::from_fn(move |req, next| {
                require_upstream_key(Arc::clone(&key), req, next)
            }))
        }
        None => router,
    }
}

async fn require_upstream_key(key: Arc<String>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/v1/maps/health" {
        return next.run(req).await;
    }
    let given = req
        .headers()
        .get("x-maps-upstream-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Lengths are public; compare the bytes without an early exit.
    let ok = given.len() == key.len()
        && given
            .bytes()
            .zip(key.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if ok {
        next.run(req).await
    } else {
        error(StatusCode::UNAUTHORIZED, "unauthorized")
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

async fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
}

async fn health(State(st): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "ok": true, "tiles": st.tiles_present(), "geocoder": st.geocoder() }))
}

async fn config(State(st): State<Arc<AppState>>) -> Json<Value> {
    let c = &st.config;
    let tiles = st
        .tiles_present()
        .then(|| format!("/v1/maps/tiles/{}.pmtiles", c.tiles));
    let home = c.home.as_ref().map(|h| {
        json!({ "lat": kernel::e7_to_deg(h.lat_e7), "lng": kernel::e7_to_deg(h.lng_e7), "label": h.label })
    });
    Json(json!({
        "tiles": tiles,
        "assets": "/v1/maps/assets",
        "home": home,
        "geocoder": st.geocoder(),
        "routes": st.router(),
        "attribution": ATTRIBUTION,
        "google": {
            "webKey": c.google_web_key.as_ref().map(provider::ClientKey::as_str),
            "iosKey": c.google_ios_key.as_ref().map(provider::ClientKey::as_str),
        },
    }))
}

/// A strong validator from size and mtime (what changes when the extract is rebuilt).
fn etag_for(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!("\"{:x}-{:x}\"", meta.len(), mtime.as_nanos())
}

fn etag_matches(list: &HeaderValue, etag: &str) -> bool {
    list.to_str()
        .map(|l| {
            l.split(',')
                .map(str::trim)
                .any(|t| t == "*" || t == etag || t.strip_prefix("W/") == Some(etag))
        })
        .unwrap_or(false)
}

/// Serves one file through tower-http's `ServeFile` (Range -> 206 + Content-Range, bad range ->
/// 416, Last-Modified / If-Modified-Since, `Accept-Ranges: bytes`), adding an ETag with
/// If-None-Match (304), If-Match (412) and If-Range (a stale validator gets the full body).
async fn serve_file(
    path: &FsPath,
    mime: Option<&str>,
    cache: &'static str,
    mut req: Request,
) -> Response {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    if !meta.is_file() {
        return error(StatusCode::NOT_FOUND, "not_found");
    }
    let etag = etag_for(&meta);
    let headers = req.headers();
    if headers
        .get(header::IF_MATCH)
        .is_some_and(|v| !etag_matches(v, &etag))
    {
        return error(StatusCode::PRECONDITION_FAILED, "precondition_failed");
    }
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| etag_matches(v, &etag));
    if not_modified {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        set_file_headers(res.headers_mut(), &etag, cache);
        return res;
    }
    if let Some(v) = headers.get(header::IF_RANGE) {
        // Only our ETag keeps the range; a date or another tag gets the whole file.
        if v.to_str().map(|s| s.trim() != etag).unwrap_or(true) {
            req.headers_mut().remove(header::RANGE);
        }
        req.headers_mut().remove(header::IF_RANGE);
    }
    let svc = match mime {
        Some(m) => ServeFile::new_with_mime(path, &m.parse().expect("static mime")),
        None => ServeFile::new(path),
    };
    let res = match svc.oneshot(req).await {
        Ok(res) => res.map(Body::new),
        Err(e) => {
            tracing::warn!("serve {}: {e}", path.display());
            return error(StatusCode::INTERNAL_SERVER_ERROR, "io_error");
        }
    };
    let (mut parts, body) = res.into_parts();
    if parts.status.is_success() || parts.status == StatusCode::NOT_MODIFIED {
        set_file_headers(&mut parts.headers, &etag, cache);
    } else if parts.status == StatusCode::NOT_FOUND {
        return error(StatusCode::NOT_FOUND, "not_found");
    }
    Response::from_parts(parts, body)
}

fn set_file_headers(h: &mut HeaderMap, etag: &str, cache: &'static str) {
    h.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("ascii etag"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
}

async fn tiles(
    State(st): State<Arc<AppState>>,
    Path(file): Path<String>,
    req: Request,
) -> Response {
    let Some(name) = file
        .strip_suffix(".pmtiles")
        .filter(|n| kernel::valid_tiles_name(n))
    else {
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    let path = st.config.tiles_path(name);
    serve_file(&path, Some("application/vnd.pmtiles"), TILES_CACHE, req).await
}

fn asset_mime(path: &FsPath) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pbf") => Some("application/x-protobuf"),
        Some("json") => Some("application/json"),
        Some("png") => Some("image/png"),
        _ => None,
    }
}

/// `MAPS_DIR/assets/<path>` when the path passes [`kernel::safe_asset_path`] and its canonical
/// form stays inside the canonical assets dir (a symlink pointing out is refused).
pub fn resolve_asset(assets_dir: &FsPath, rel: &str) -> Result<PathBuf, StatusCode> {
    if !kernel::safe_asset_path(rel) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let root = assets_dir
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let path = root
        .join(rel)
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(path)
}

async fn assets(
    State(st): State<Arc<AppState>>,
    Path(rel): Path<String>,
    req: Request,
) -> Response {
    match resolve_asset(&st.config.assets_dir(), &rel) {
        Ok(path) => {
            let mime = asset_mime(&path);
            serve_file(&path, mime, ASSETS_CACHE, req).await
        }
        Err(StatusCode::BAD_REQUEST) => error(StatusCode::BAD_REQUEST, "bad_path"),
        Err(_) => error(StatusCode::NOT_FOUND, "not_found"),
    }
}

#[derive(Deserialize)]
struct GeocodeQuery {
    q: Option<String>,
    lat: Option<String>,
    lng: Option<String>,
    limit: Option<String>,
}

async fn geocode_route(State(st): State<Arc<AppState>>, Query(p): Query<GeocodeQuery>) -> Response {
    let q = p.q.as_deref().unwrap_or("").trim();
    let n = q.chars().count();
    if !(2..=200).contains(&n) {
        return error(StatusCode::BAD_REQUEST, "bad_query");
    }
    let limit = match p.limit.as_deref().map(str::trim) {
        None | Some("") => 3,
        Some(s) => match s.parse::<u8>() {
            Ok(l @ 1..=5) => l,
            _ => return error(StatusCode::BAD_REQUEST, "bad_limit"),
        },
    };
    let from_query = match (p.lat.as_deref(), p.lng.as_deref()) {
        (Some(a), Some(b)) => match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            (Ok(a), Ok(b)) => kernel::parse_coord(a, b),
            _ => None,
        },
        _ => None,
    };
    let bias = from_query.or(st.config.home.as_ref().map(|h| (h.lat_e7, h.lng_e7)));
    // University codes: AU Find first, then local OSM buildings, with or without Google.
    if let (Some(base), Some(room)) = (&st.config.aufind_url, gazetteer::au_room(q)) {
        let key = format!("aufind|{}", geocode::cache_key(q, bias, limit));
        if let Some(results) = st.cache.lock().unwrap().get(&key) {
            return Json(json!({ "results": results })).into_response();
        }
        if let Some(hit) = aufind::lookup(&st.http, base, &room).await {
            let results = json!([hit]);
            st.cache
                .lock()
                .unwrap()
                .insert(key, results.clone(), 86400, false);
            return Json(json!({ "results": results })).into_response();
        }
    }
    if let Some(hit) = st.gazetteer.lookup(q) {
        tracing::info!(q_len = n, results = 1, "geocode gazetteer");
        return Json(json!({ "results": [hit] })).into_response();
    }
    // Google first (when a key is loaded), then the self-hosted Photon (when configured).
    if st.config.google_key.is_some() {
        let key = geocode::cache_key(q, bias, limit);
        if let Some(results) = st.cache.lock().unwrap().get(&key) {
            return Json(json!({ "results": results })).into_response();
        }
        match tokio::time::timeout(Duration::from_secs(3), geocode::google(&st, q, bias, limit))
            .await
            .unwrap_or(Err(provider::Unavailable))
        {
            Ok(results) => {
                tracing::info!(q_len = n, results = results.len(), "geocode");
                st.cache
                    .lock()
                    .unwrap()
                    .insert(key, json!(results), 30 * 86400, true);
                return Json(json!({ "results": results })).into_response();
            }
            Err(provider::Unavailable) => tracing::warn!(q_len = n, "geocode google unavailable"),
        }
    }
    if let Some(base) = &st.config.photon_url {
        let key = format!("photon|{}", geocode::cache_key(q, bias, limit));
        if let Some(results) = st.cache.lock().unwrap().get(&key) {
            return Json(json!({ "results": results })).into_response();
        }
        match photon::geocode(&st.http, base, q, bias, limit).await {
            Ok(results) => {
                tracing::info!(q_len = n, results = results.len(), "geocode photon");
                // Memory only: the persistent snapshot holds Google answers.
                st.cache
                    .lock()
                    .unwrap()
                    .insert(key, json!(results), 86400, false);
                return Json(json!({ "results": results })).into_response();
            }
            Err(provider::Unavailable) => tracing::warn!(q_len = n, "geocode photon unavailable"),
        }
    }
    tracing::warn!(q_len = n, "geocode unavailable");
    error(StatusCode::SERVICE_UNAVAILABLE, "provider_unavailable")
}

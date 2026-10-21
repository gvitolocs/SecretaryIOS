//! The old shopping upstream routes (from `lucina-proxy/src/routes.rs`),
//! mounted under `/v1/upstream/` with the `lucina_` prefixes dropped.
//!
//! Mounted only when `SECRETARY_UPSTREAM_LEGACY` and/or
//! `SECRETARY_UPSTREAM_SERVICES` is set, and behind device-token auth like
//! every other `/v1` route. Because `Authorization` now carries the device
//! token, the caller's upstream credential travels in
//! `X-Upstream-Authorization` and is relayed upstream as `Authorization`.
//! The device token is never forwarded.
//!
//! Upstream paths come from the API surface inventory of the reference
//! client (`docs/api-surface.md`). Bodies are relayed as bytes.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderName, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::Router;

use crate::config::UpstreamConfig;
use crate::error::ApiError;
use crate::AppState;

pub struct Upstream {
    http: reqwest::Client,
    /// Legacy API: auth, lists, items, user settings.
    legacy: Option<String>,
    /// Current service families: providers, offers, discounts, prediction.
    services: Option<String>,
    /// Optional local catalog JSON, served at `catalog_local`.
    catalog: Option<PathBuf>,
}

impl Upstream {
    pub fn new(cfg: &UpstreamConfig, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("secretary-server/", env!("CARGO_PKG_VERSION")))
                .timeout(timeout)
                .connect_timeout(Duration::from_secs(5))
                .build()?,
            legacy: cfg.legacy.clone(),
            services: cfg.services.clone(),
            catalog: cfg.catalog.clone(),
        })
    }
}

/// Request headers relayed upstream (besides the credential, below).
const PASS_HEADERS: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::ACCEPT,
    header::ACCEPT_LANGUAGE,
];
/// Response headers relayed back. Hop-by-hop and framing headers are not
/// (reqwest has already decoded the body, so content-encoding/length lie).
const RETURN_HEADERS: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::CACHE_CONTROL,
    header::ETAG,
    header::LAST_MODIFIED,
];
const UPSTREAM_AUTH: &str = "x-upstream-authorization";

const SERVICE_FAMILIES: &[&str] = &[
    "user",
    "offers",
    "discounts",
    "prediction",
    "push",
    "partner",
    "ad",
];

fn bad(msg: &str) -> Response {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", msg).into_response()
}

/// A path parameter must stay one segment (axum percent-decodes `%2F`).
fn seg(s: &str) -> Option<&str> {
    let bad = s.is_empty() || s == "." || s == ".." || s.contains(['/', '\\', '?', '#']);
    (!bad).then_some(s)
}

async fn upstream(
    st: &AppState,
    headers: &HeaderMap,
    family: &str,
    method: Method,
    path: String,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(b) => b,
        Err(r) => return ApiError::from(r).into_response(),
    };
    let Some(up) = st.upstream.as_ref() else {
        return ApiError::not_found().into_response();
    };
    let base = match family {
        "legacy" => up.legacy.as_deref(),
        f if SERVICE_FAMILIES.contains(&f) => up.services.as_deref(),
        _ => return bad("unknown family"),
    };
    let Some(base) = base else {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream_not_configured",
            "this upstream family is not configured on the server",
        )
        .into_response();
    };
    let path = if family == "legacy" {
        path
    } else {
        format!("{family}/rest/{path}")
    };
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );

    let mut req = up.http.request(method, &url);
    for name in PASS_HEADERS {
        if let Some(v) = headers.get(name) {
            req = req.header(name, v.clone());
        }
    }
    if let Some(v) = headers.get(UPSTREAM_AUTH) {
        req = req.header(header::AUTHORIZATION, v.clone());
    }
    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();
            for name in RETURN_HEADERS {
                if let Some(v) = resp.headers().get(name) {
                    out_headers.insert(name, v.clone());
                }
            }
            match resp.bytes().await {
                Ok(bytes) => (status, out_headers, bytes).into_response(),
                Err(e) => transport_error(&e),
            }
        }
        Err(e) => transport_error(&e),
    }
}

fn transport_error(e: &reqwest::Error) -> Response {
    tracing::warn!(timeout = e.is_timeout(), "upstream request failed");
    if e.is_timeout() {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "upstream did not answer in time",
        )
    } else {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unreachable",
            "upstream is not reachable",
        )
    }
    .into_response()
}

/// Shorthand for the 1:1 route handlers.
macro_rules! fixed {
    ($name:ident, $family:literal, $method:ident, $path:literal) => {
        async fn $name(
            State(st): State<AppState>,
            h: HeaderMap,
            b: Result<Bytes, BytesRejection>,
        ) -> Response {
            upstream(&st, &h, $family, Method::$method, $path.into(), b).await
        }
    };
}

macro_rules! one {
    ($name:ident, $family:literal, $method:ident, $fmt:literal) => {
        async fn $name(
            Path(p): Path<String>,
            State(st): State<AppState>,
            h: HeaderMap,
            b: Result<Bytes, BytesRejection>,
        ) -> Response {
            let Some(p) = seg(&p) else {
                return bad("invalid path parameter");
            };
            upstream(&st, &h, $family, Method::$method, format!($fmt, p), b).await
        }
    };
}

macro_rules! two {
    ($name:ident, $family:literal, $method:ident, $fmt:literal) => {
        async fn $name(
            Path((a, c)): Path<(String, String)>,
            State(st): State<AppState>,
            h: HeaderMap,
            b: Result<Bytes, BytesRejection>,
        ) -> Response {
            let (Some(a), Some(c)) = (seg(&a), seg(&c)) else {
                return bad("invalid path parameter");
            };
            upstream(&st, &h, $family, Method::$method, format!($fmt, a, c), b).await
        }
    };
}

// --- auth ------------------------------------------------------------------
fixed!(auth, "legacy", POST, "v2/bringauth");
fixed!(auth_token, "legacy", POST, "v2/bringauth/token");
fixed!(signup, "legacy", POST, "v2/bringauth/signup");
fixed!(signup_anon, "legacy", POST, "v2/bringauth/signupanonym");
fixed!(magic_login, "legacy", POST, "v2/bringauth/magiclogin");

// --- lists -----------------------------------------------------------------
one!(lists, "legacy", GET, "bringusers/{}/lists");
one!(list, "legacy", GET, "v2/bringlists/{}");
one!(list_users, "legacy", GET, "v2/bringlists/{}/users");
one!(list_details, "legacy", GET, "bringlists/{}/details");
one!(list_create, "legacy", POST, "bringlists/{}");
one!(items_put, "legacy", PUT, "v2/bringlists/{}/items");

// --- item details ----------------------------------------------------------
one!(item_details, "legacy", GET, "v2/bringlistitemdetails/{}");
fixed!(item_details_post, "legacy", POST, "v2/bringlistitemdetails");
// field: usersection | usericon | image | assignedto
two!(item_patch, "legacy", PUT, "v2/bringlistitemdetails/{}/{}");

// --- user / settings -------------------------------------------------------
one!(user, "legacy", GET, "v2/bringusers/{}");
one!(user_put, "legacy", PUT, "v2/bringusers/{}");
one!(settings, "legacy", GET, "bringusersettings/{}");
two!(settings_set, "legacy", POST, "bringusersettings/{}/{}");

// --- services (providers / offers / discounts / prediction) ----------------
fixed!(providers, "user", GET, "v1/providers");
one!(provider_stores, "user", GET, "v1/providers/{}/stores");
fixed!(offers, "offers", GET, "v1/offers");
fixed!(discounts_check, "discounts", POST, "v1/discounts/check");
fixed!(predict, "prediction", POST, "v1/itempredictor/prediction");
fixed!(device_register, "push", POST, "bringdevices");

// --- local -----------------------------------------------------------------

/// Serves the local catalog asset (`SECRETARY_UPSTREAM_CATALOG`).
async fn catalog_local(State(st): State<AppState>) -> Response {
    let Some(path) = st.upstream.as_ref().and_then(|u| u.catalog.clone()) else {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "catalog_not_configured",
            "SECRETARY_UPSTREAM_CATALOG is not set",
        )
        .into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(_) => ApiError::new(
            StatusCode::NOT_FOUND,
            "catalog_unreadable",
            "catalog file unreadable",
        )
        .into_response(),
    }
}

/// Generic escape hatch: `any/{family}/{*path}` reaches every inventoried
/// upstream operation without a dedicated route.
async fn any_route(
    State(st): State<AppState>,
    Path((family, path)): Path<(String, String)>,
    h: HeaderMap,
    method: Method,
    b: Result<Bytes, BytesRejection>,
) -> Response {
    if path
        .split('/')
        .any(|s| s == "." || s == ".." || s.contains('\\'))
    {
        return bad("invalid path");
    }
    upstream(&st, &h, &family, method, path, b).await
}

pub fn router() -> Router<AppState> {
    const P: &str = "/v1/upstream";
    let r = |s: &str| format!("{P}/{s}");
    Router::new()
        .route(&r("auth"), post(auth))
        .route(&r("auth_token"), post(auth_token))
        .route(&r("signup"), post(signup))
        .route(&r("signup_anon"), post(signup_anon))
        .route(&r("magic_login"), post(magic_login))
        .route(&r("lists/{user}"), get(lists))
        .route(&r("list/{list}"), get(list))
        .route(&r("list_users/{list}"), get(list_users))
        .route(&r("list_details/{list}"), get(list_details))
        .route(&r("list_create/{list}"), post(list_create))
        .route(&r("items_put/{list}"), put(items_put))
        .route(&r("item_details/{item}"), get(item_details))
        .route(&r("item_details_post"), post(item_details_post))
        .route(&r("item_patch/{item}/{field}"), put(item_patch))
        .route(&r("user/{user}"), get(user))
        .route(&r("user_put/{user}"), put(user_put))
        .route(&r("settings/{user}"), get(settings))
        .route(&r("settings_set/{user}/{key}"), post(settings_set))
        .route(&r("providers"), get(providers))
        .route(&r("provider_stores/{provider}"), get(provider_stores))
        .route(&r("offers"), get(offers))
        .route(&r("discounts_check"), post(discounts_check))
        .route(&r("predict"), post(predict))
        .route(&r("device_register"), post(device_register))
        .route(&r("catalog_local"), get(catalog_local))
        .route(&r("any/{family}/{*path}"), any(any_route))
}

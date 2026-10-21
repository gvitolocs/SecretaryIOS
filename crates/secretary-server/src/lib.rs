//! secretary-server: the thin, authenticated gateway between the Secretary
//! iOS app and Lucina.
//!
//! It owns auth (device tokens), validation (via `secretary-core`),
//! forwarding with the server-held Lucina token, and error mapping. It owns
//! no product logic.

pub mod cli;
pub mod config;
pub mod devices;
pub mod error;
pub mod health;
pub mod ratelimit;
pub mod routes;
pub mod upstream;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{header, HeaderMap};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use secretary_core::LucinaClient;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;

pub use config::Config;
use devices::{AuthResult, DeviceAuth, DeviceStore};
use error::ApiError;
use health::HealthCache;
use ratelimit::RateLimiter;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Inner {
    pub lucina: Option<LucinaClient>,
    pub devices: DeviceAuth,
    pub limiter: RateLimiter,
    pub health: HealthCache,
    pub upstream: Option<upstream::Upstream>,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let lucina = match &cfg.lucina_token {
            Some(t) => Some(LucinaClient::new(&cfg.lucina_url, t.clone(), cfg.timeouts)?),
            None => None,
        };
        let upstream = match &cfg.upstream {
            Some(u) => Some(upstream::Upstream::new(u, cfg.upstream_timeout)?),
            None => None,
        };
        Ok(Self(Arc::new(Inner {
            lucina,
            devices: DeviceAuth::new(DeviceStore::new(&cfg.data_dir)),
            limiter: RateLimiter::new(cfg.rate_limit_per_minute),
            health: HealthCache::new(cfg.health_ttl, cfg.health_timeout),
            upstream,
        })))
    }

    pub fn lucina(&self) -> Result<&LucinaClient, ApiError> {
        self.lucina.as_ref().ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "lucina_not_configured",
                "SECRETARY_LUCINA_TOKEN_FILE is not set on the server",
            )
        })
    }
}

/// The authenticated device, inserted by [`require_device`].
#[derive(Debug, Clone)]
pub struct DeviceId(pub String);

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Device-token auth + per-device rate limit for every protected route.
async fn require_device(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer(req.headers())
        .filter(|t| t.starts_with(devices::TOKEN_PREFIX))
        .ok_or_else(|| ApiError::unauthorized("missing or malformed device token"))?;
    let device_id = match st.devices.authenticate(token) {
        Ok(AuthResult::Ok { device_id }) => device_id,
        Ok(AuthResult::Revoked) => return Err(ApiError::unauthorized("device token revoked")),
        Ok(AuthResult::Unknown) => return Err(ApiError::unauthorized("unknown device token")),
        Err(e) => {
            tracing::error!(error = %e, "device registry unreadable; denying");
            return Err(ApiError::internal("device registry unavailable"));
        }
    };
    if let Err(wait) = st.limiter.check(&device_id) {
        let mut e = ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests; slow down",
        );
        e.retry_after = Some(wait.as_secs().max(1));
        return Err(e);
    }
    req.extensions_mut().insert(DeviceId(device_id.clone()));
    let mut resp = next.run(req).await;
    // For the access log (the id is not a secret).
    resp.extensions_mut().insert(DeviceId(device_id));
    Ok(resp)
}

/// Access log: method, route template, status, latency, body length. Never
/// the URI (ids, query), headers or bodies.
async fn access_log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |p| p.as_str().to_owned());
    let body_len: u64 = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .unwrap_or(0);
    let start = Instant::now();
    let resp = next.run(req).await;
    let device = resp
        .extensions()
        .get::<DeviceId>()
        .map(|d| d.0.clone())
        .unwrap_or_default();
    tracing::info!(
        method = %method,
        route = %route,
        status = resp.status().as_u16(),
        latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        body_len,
        device = %device,
        "request"
    );
    resp
}

async fn not_found(req: Request) -> Response {
    tracing::info!(method = %req.method(), route = "<unmatched>", status = 404, "request");
    ApiError::not_found().into_response()
}

/// The full HTTP surface.
pub fn app(state: AppState) -> Router {
    let mut protected = routes::protected();
    if state.upstream.is_some() {
        protected = protected.merge(upstream::router());
    }
    let protected = protected.route_layer(middleware::from_fn_with_state(
        state.clone(),
        require_device,
    ));

    Router::new()
        .merge(routes::public())
        .merge(protected)
        .route_layer(middleware::from_fn(access_log))
        .fallback(not_found)
        .with_state(state)
        .layer(DefaultBodyLimit::max(config::BODY_LIMIT))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-upstream-authorization"),
        ]))
        .layer(CatchPanicLayer::custom(|_| {
            ApiError::internal("internal error").into_response()
        }))
}

/// Resolves on SIGTERM or Ctrl-C.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("shutdown signal received; draining");
}

/// Bind and serve until SIGTERM.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let state = AppState::new(&cfg)?;
    if state.lucina.is_none() {
        tracing::warn!("SECRETARY_LUCINA_TOKEN_FILE not set: /v1 data routes will answer 503");
    }
    if !DeviceStore::new(&cfg.data_dir).path().exists() {
        tracing::warn!(dir = %cfg.data_dir.display(), "no devices.json yet: every request will be 401 until `token issue`");
    }
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, version = VERSION, upstream = state.upstream.is_some(), "secretary-server listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

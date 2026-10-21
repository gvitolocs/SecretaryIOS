//! secretary-server: the thin, authenticated gateway between the Secretary
//! clients (iOS app, web app at the same origin) and Lucina.
//!
//! It owns auth (device tokens, browser cookie sessions), validation (via
//! `secretary-core`), forwarding with the server-held Lucina token, error
//! mapping and static file serving. It owns no product logic.

pub mod assets;
pub mod auth;
pub mod cli;
pub mod config;
pub mod devices;
pub mod engines;
pub mod error;
pub mod health;
pub mod insights;
pub mod owner;
pub mod ratelimit;
pub mod routes;
pub mod service;
pub mod sessions;
pub mod shopping;
pub mod timeline;
pub mod tts;
pub mod upstream;
pub mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use secretary_core::LucinaClient;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::set_header::SetResponseHeaderLayer;

pub use auth::DeviceId;
pub use config::Config;
use devices::{DeviceAuth, DeviceStore};
use error::ApiError;
use health::HealthCache;
use ratelimit::RateLimiter;
use sessions::{SessionAuth, SessionStore};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sent on every response.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

pub struct Inner {
    pub lucina: Option<LucinaClient>,
    pub devices: DeviceAuth,
    pub sessions: SessionAuth,
    pub limiter: RateLimiter,
    pub login_limiter: RateLimiter,
    pub health: HealthCache,
    pub upstream: Option<upstream::Upstream>,
    pub web_dir: Option<PathBuf>,
    pub trust_proxy: bool,
    /// Gym and body engines (files in the data dir).
    pub engines: engines::Engines,
    /// `None` when `SECRETARY_SERVICE_TOKEN_FILE` is unset.
    pub service_token: Option<service::ServiceToken>,
    /// The single owner account (`owner.json`).
    pub owner: owner::OwnerStore,
    pub private_assets_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub tts: tts::Tts,
    /// Open `/v1/timeline/stream` responses.
    pub streams: timeline::StreamSlots,
    /// Flipped to `true` on shutdown; open streams end when it is.
    pub shutdown: tokio::sync::watch::Sender<bool>,
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
            sessions: SessionAuth::new(SessionStore::new(&cfg.data_dir)),
            limiter: RateLimiter::new(cfg.rate_limit_per_minute),
            login_limiter: RateLimiter::new(cfg.login_rate_per_minute),
            health: HealthCache::new(cfg.health_ttl, cfg.health_timeout),
            upstream,
            web_dir: cfg.web_dir.clone(),
            trust_proxy: cfg.trust_proxy,
            engines: engines::Engines::open(&cfg.data_dir)?,
            service_token: cfg.service_token.clone(),
            owner: owner::OwnerStore::new(&cfg.data_dir),
            private_assets_dir: cfg.private_assets_dir.clone(),
            catalog_dir: cfg.catalog_dir.clone(),
            tts: tts::Tts::new(&cfg.tts_url, cfg.tts_voices.clone(), cfg.tts_timeout)?,
            streams: timeline::StreamSlots::new(timeline::PER_DEVICE, timeline::TOTAL),
            shutdown: tokio::sync::watch::Sender::new(false),
        })))
    }

    /// End every open timeline stream (and refuse none: new ones end at
    /// once). Called when the shutdown signal arrives, so graceful shutdown
    /// does not wait on long-lived responses.
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
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

/// Access log: method, route template, status, latency, body length and the
/// device id. Never the URI (ids, query), headers, cookies or bodies.
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

fn static_header(name: &'static str, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

/// The full HTTP surface.
pub fn app(state: AppState) -> Router {
    let mut protected = routes::protected()
        .merge(engines::routes())
        .merge(auth::account_routes())
        .merge(assets::routes())
        .merge(tts::routes())
        .merge(timeline::routes())
        .merge(shopping::routes());
    if state.upstream.is_some() {
        protected = protected.merge(upstream::router());
    }
    let protected = protected.route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_device,
    ));

    let api = Router::new()
        .merge(routes::public())
        .merge(auth::session_routes())
        .merge(protected)
        .merge(insights::routes(state.clone()))
        .route_layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .route_layer(middleware::from_fn(access_log));

    api.fallback(web::fallback)
        .with_state(state)
        .layer(DefaultBodyLimit::max(config::BODY_LIMIT))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::COOKIE,
            header::SET_COOKIE,
            HeaderName::from_static("x-upstream-authorization"),
        ]))
        .layer(CatchPanicLayer::custom(|_| {
            ApiError::internal("internal error").into_response()
        }))
        .layer(static_header(
            "content-security-policy",
            CONTENT_SECURITY_POLICY,
        ))
        .layer(static_header("x-content-type-options", "nosniff"))
        .layer(static_header("referrer-policy", "no-referrer"))
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

/// Serve `router` on `listener` with peer addresses available to handlers
/// (the login rate limit needs them).
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
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
    if let Some(dir) = &cfg.web_dir {
        if !dir.join("index.html").exists() {
            tracing::warn!(dir = %dir.display(), "SECRETARY_WEB_DIR has no index.html");
        }
    }
    // The dummy Argon2 hash for unknown emails, computed before the first login.
    tokio::task::spawn_blocking(owner::warm_up);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(
        addr = %cfg.bind,
        version = VERSION,
        upstream = state.upstream.is_some(),
        web = cfg.web_dir.is_some(),
        trust_proxy = cfg.trust_proxy,
        "secretary-server listening"
    );
    let st = state.clone();
    let shutdown = async move {
        shutdown_signal().await;
        st.begin_shutdown();
    };
    serve_on(listener, app(state), shutdown).await?;
    Ok(())
}

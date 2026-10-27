//! Lucina MCP: a remote MCP server (Streamable HTTP, MCP 2026-07-28 and the
//! earlier revisions rmcp negotiates) that lets Claude Code, ChatGPT and
//! Grok talk to Lucina and use her Secretary tools.
//!
//! ```text
//! client ──HTTPS──▶ Caddy ──▶ secretary-mcp :8810 ──▶ secretary-server :8800 ──▶ Lucina
//!                              /mcp            (device token)
//!                              /oauth/*, /.well-known/oauth-*
//! ```
//!
//! - [`auth`]: Bearer auth of `/mcp` (service tokens or OAuth access tokens).
//! - [`oauth`]: the OAuth 2.1 authorization server for ChatGPT connectors.
//! - [`mcp`] / [`tools`]: the MCP handler and its tools.
//! - [`backend`]: the secretary-server client.

pub mod auth;
pub mod backend;
pub mod cli;
pub mod computer;
pub mod config;
pub mod format;
pub mod login;
pub mod mcp;
pub mod oauth;
pub mod ratelimit;
pub mod scope;
pub mod status;
pub mod store;
pub mod tokens;
pub mod tools;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

pub use config::Config;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct App {
    pub cfg: Config,
    pub tokens: tokens::TokenStore,
    pub oauth: oauth::OAuth,
    pub backend: backend::Backend,
}

pub type AppState = Arc<App>;

impl App {
    pub fn new(cfg: Config) -> anyhow::Result<AppState> {
        store::ensure_dir(&cfg.data_dir)?;
        let backend = backend::Backend::new(
            &cfg.backend_url,
            cfg.backend_token.clone(),
            cfg.service_token.clone(),
        );
        Ok(Arc::new(App {
            tokens: tokens::TokenStore::new(&cfg.data_dir),
            oauth: oauth::OAuth::new(&cfg),
            backend,
            cfg,
        }))
    }
}

/// The whole HTTP surface.
pub fn router(app: AppState, ct: CancellationToken) -> Router {
    let mcp_config = StreamableHttpServerConfig::default()
        // Stateless: no Mcp-Session-Id, so restarts and the proxy never
        // strand a client; 2026-07-28 is stateless anyway.
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_allowed_hosts(app.cfg.allowed_hosts.clone())
        .with_allowed_origins(app.cfg.allowed_origins.clone())
        .with_max_request_body_bytes(1024 * 1024)
        .with_cancellation_token(ct);
    let factory_app = app.clone();
    let mcp_service: StreamableHttpService<mcp::LucinaMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(mcp::LucinaMcp::new(factory_app.clone())),
            Arc::new(LocalSessionManager::default()),
            mcp_config,
        );
    let mcp_routes = Router::new()
        .route_service("/mcp", mcp_service)
        .route_layer(from_fn_with_state(app.clone(), auth::require_auth));
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::authorization_server),
        )
        .route("/oauth/register", post(oauth::register))
        .route(
            "/oauth/authorize",
            get(oauth::authorize_get).post(oauth::authorize_post),
        )
        .route("/oauth/token", post(oauth::token))
        .route("/oauth/revoke", post(oauth::revoke))
        .route("/healthz", get(|| async { "ok" }))
        .route("/internal/status", get(status::internal_status))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(app)
        .merge(mcp_routes)
}

/// Bind and serve in the background; returns the bound address and a token
/// that stops the server when cancelled.
pub async fn spawn(
    cfg: Config,
) -> anyhow::Result<(SocketAddr, CancellationToken, tokio::task::JoinHandle<()>)> {
    let ct = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    let addr = listener.local_addr()?;
    let app = App::new(cfg)?;
    let router = router(app, ct.child_token());
    let stop = ct.clone();
    let handle = tokio::spawn(async move {
        let r = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .await;
        if let Err(e) = r {
            tracing::error!(error = %e, "server error");
        }
    });
    Ok((addr, ct, handle))
}

/// `secretary-mcp serve`: run until SIGTERM / Ctrl-C.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    if cfg.backend_token.is_none() {
        tracing::warn!("SECRETARY_MCP_BACKEND_TOKEN_FILE is not set: every tool will fail");
    }
    if cfg.service_token.is_none() {
        tracing::info!(
            "SECRETARY_MCP_SERVICE_TOKEN_FILE is not set: gym_summary uses the gym state"
        );
    }
    tracing::info!(bind = %cfg.bind, public = %cfg.public_url, version = VERSION, "secretary-mcp starting");
    let (addr, ct, handle) = spawn(cfg).await?;
    tracing::info!(%addr, "listening");
    shutdown_signal().await;
    tracing::info!("shutting down");
    ct.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

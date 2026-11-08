//! `secretary-maps`: see the crate README. Binds `MAPS_BIND` (default 127.0.0.1:8840); the rest
//! of the configuration comes from the environment (`secretary_maps::Config::from_env`).
//!
//! Everything except `/v1/maps/health` trusts the caller unless `MAPS_UPSTREAM_KEY_FILE` is set:
//! bind it where only p1's Caddy can reach it, and/or set the key, which Caddy then sends in
//! `X-Maps-Upstream-Key`.
use secretary_maps::{app, AppState, Config};

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env(|k| std::env::var(k).ok())?;
    let bind = std::env::var("MAPS_BIND").unwrap_or_else(|_| "127.0.0.1:8840".into());
    tracing::info!(
        dir = %config.dir.display(),
        tiles = %config.tiles,
        home = config.home.is_some(),
        upstream_key = config.upstream_key.is_some(),
        "maps config"
    );
    let state = AppState::new(config);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("{bind}: {e}"))?;
    tracing::info!("maps listening on {bind}");
    axum::serve(listener, app(state))
        .await
        .map_err(|e| e.to_string())
}

//! `lucina-updates`: the read-only update channel server (see the library docs).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "lucina-updates",
    about = "Read-only update channel server: manifest and artefact bytes, no keys, no writes"
)]
struct Cli {
    /// Listen address. Loopback only: Caddy (forward_auth) is the only client.
    #[arg(long, default_value = "127.0.0.1:8811")]
    addr: SocketAddr,
    /// The store: `<product>/<channel>/manifest` and `<product>/<channel>/artefacts/<sha256>.<ext>`.
    /// Default: `$HOME/data/lucina-updates`.
    #[arg(long)]
    store: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lucina_updates=info".into()),
        )
        .init();
    let cli = Cli::parse();
    if !cli.addr.ip().is_loopback() {
        bail!("--addr {}: lucina-updates listens on loopback only", cli.addr);
    }
    let store = match cli.store {
        Some(s) => s,
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set; pass --store")?)
            .join("data/lucina-updates"),
    };
    let app = lucina_updates::app(&store)
        .with_context(|| format!("store {}", store.display()))?;
    let listener = tokio::net::TcpListener::bind(cli.addr)
        .await
        .with_context(|| format!("bind {}", cli.addr))?;
    tracing::info!(addr = %cli.addr, store = %store.display(), "serving read-only");
    axum::serve(listener, app).await?;
    Ok(())
}

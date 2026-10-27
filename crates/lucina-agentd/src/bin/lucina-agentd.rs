//! lucina-agentd: serve the agents API on a unix socket until SIGTERM/SIGINT.
//! Shutdown closes every session and exits 0 within the unit's 10 s stop timeout.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use lucina_agentd::api::router;
use lucina_agentd::config::{default_socket, default_state_dir, dev_enabled};
use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::Manager;
use lucina_agentd::mock::{MockProvider, MockSlowProvider};
use lucina_agentd::process::Registry;
use lucina_agentd::provider::Provider;
use lucina_agentd::providers;
use lucina_agentd::store::Store;
use tokio::net::{TcpListener, UnixListener};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser, Debug)]
#[command(name = "lucina-agentd", version, about = "Lucina Agents daemon")]
struct Args {
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Also listen on this loopback address (e.g. 127.0.0.1:6768)
    #[arg(long)]
    listen: Option<std::net::SocketAddr>,
    /// Register only the mock providers (tests).
    #[arg(long)]
    no_real_providers: bool,
}

async fn shutdown_signal() {
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let state_dir = args.state_dir.unwrap_or_else(default_state_dir);
    let socket = args.socket.unwrap_or_else(default_socket);

    std::fs::create_dir_all(&state_dir)?;
    let registry = Registry::open(&state_dir)?;
    let reaped = registry.reap_leftovers();
    if !reaped.is_empty() {
        tracing::warn!(count = reaped.len(), "reaped provider processes from a previous run");
    }
    let lanes = LaneConfig::load();
    let mut providers: Vec<Arc<dyn Provider>> = if args.no_real_providers { Vec::new() } else { providers::installed(&lanes, &registry) };
    if dev_enabled() {
        providers.push(Arc::new(MockProvider));
        providers.push(Arc::new(MockSlowProvider));
    }
    let manager = Manager::open(Store::open(&state_dir)?, providers, lanes)?;

    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let unix = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %socket.display(), state = %state_dir.display(), dev = dev_enabled(), "lucina-agentd listening");

    let app = router(manager.clone());
    let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let mut servers = Vec::new();
    {
        let mut rx = stop_tx.subscribe();
        let app = app.clone();
        servers.push(tokio::spawn(async move {
            axum::serve(unix, app).with_graceful_shutdown(async move { let _ = rx.recv().await; }).await
        }));
    }
    if let Some(addr) = args.listen {
        if !addr.ip().is_loopback() {
            return Err(format!("--listen must be a loopback address, got {addr}").into());
        }
        let tcp = TcpListener::bind(addr).await?;
        let mut rx = stop_tx.subscribe();
        servers.push(tokio::spawn(async move {
            axum::serve(tcp, app).with_graceful_shutdown(async move { let _ = rx.recv().await; }).await
        }));
    }

    shutdown_signal().await;
    tracing::info!("shutting down");
    let _ = stop_tx.send(());
    // Long-lived streams (SSE) would hold the graceful drain open: cap it.
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        for s in servers {
            let _ = s.await;
        }
    })
    .await;
    // Provider processes get up to 4 s (each closes with a 3 s grace): 3 + 4 s stays
    // under the unit's 10 s stop timeout.
    let _ = tokio::time::timeout(Duration::from_secs(4), manager.shutdown()).await;
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

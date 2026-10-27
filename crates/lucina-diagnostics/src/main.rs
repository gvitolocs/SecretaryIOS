//! `lucina-diagnostics`: crash reports and log bundles on nezopt.
//!
//!   lucina-diagnostics serve           # API + retention sweep
//!   lucina-diagnostics list [--app A] [--kind K] [--since 24h] [--limit N]
//!   lucina-diagnostics purge           # apply the retention now
//!   lucina-diagnostics reindex         # rebuild index.sqlite from the report.json files
use anyhow::Context;
use clap::{Parser, Subcommand};
use lucina_diagnostics::model::{AppName, Kind};
use lucina_diagnostics::store::{ListQuery, Store};
use lucina_diagnostics::{api, App, Config};

#[derive(Parser)]
#[command(
    name = "lucina-diagnostics",
    about = "Lucina diagnostics: crash reports and logs from every client"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the API and run the retention sweep.
    Serve,
    /// Print the latest reports.
    List {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        /// RFC 3339, YYYY-MM-DD or a span (30m, 24h, 7d).
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Delete reports past the retention period now.
    Purge,
    /// Rebuild the SQLite index from the report directories.
    Reindex,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match cli.cmd {
        Cmd::Serve => {
            anyhow::ensure!(
                cfg.gateway_token_file.is_some() || cfg.service_token_file.is_some(),
                "set LUCINA_DIAGNOSTICS_GATEWAY_TOKEN_FILE and/or LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE"
            );
            let app = App::new(cfg)?;
            tokio::spawn(lucina_diagnostics::retention_loop(app.clone()));
            let router = api::router(app.clone());
            let mut servers = Vec::new();
            for bind in &app.cfg.binds {
                let listener = tokio::net::TcpListener::bind(bind)
                    .await
                    .with_context(|| format!("bind {bind}"))?;
                tracing::info!(%bind, "lucina-diagnostics listening");
                let r = router.clone();
                servers.push(tokio::spawn(async move { axum::serve(listener, r).await }));
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async {
                    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
                    term.recv().await
                } => {}
            }
            tracing::info!("lucina-diagnostics stopping");
            Ok(())
        }
        Cmd::List {
            app,
            kind,
            since,
            limit,
        } => {
            let store = Store::open(&cfg.data_dir)?;
            let q = ListQuery {
                apps: app
                    .iter()
                    .map(|a| AppName::parse(a).context("--app"))
                    .collect::<anyhow::Result<_>>()?,
                kinds: kind
                    .iter()
                    .map(|k| Kind::parse(k).context("--kind"))
                    .collect::<anyhow::Result<_>>()?,
                since: since
                    .map(|s| {
                        api::parse_time(&s, cfg.timezone, chrono::Utc::now()).context("--since")
                    })
                    .transpose()?,
                limit,
                ..Default::default()
            };
            for r in store.list(&q)? {
                println!(
                    "{}  {}  {:<14} {:<11} ×{:<4} {} {}  {}",
                    r.id,
                    r.last_seen_at
                        .with_timezone(&cfg.timezone)
                        .format("%Y-%m-%d %H:%M"),
                    r.app.as_str(),
                    r.kind.as_str(),
                    r.count,
                    r.app_version,
                    r.device,
                    r.message
                );
            }
            Ok(())
        }
        Cmd::Purge => {
            let store = Store::open(&cfg.data_dir)?;
            let n = store.purge(chrono::Utc::now() - cfg.retention)?;
            println!("deleted {n}");
            Ok(())
        }
        Cmd::Reindex => {
            let store = Store::open(&cfg.data_dir)?;
            println!("indexed {}", store.reindex()?);
            Ok(())
        }
    }
}

//! `lucina-diagnostics`: crash reports and log bundles on nezopt.
//!
//!   lucina-diagnostics serve                         # API + hourly retention
//!   lucina-diagnostics list [--app A] [--kind K] [--since 24h] [--limit N]
//!   lucina-diagnostics show <id>
//!   lucina-diagnostics prune                         # retention now
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use lucina_diagnostics::{api, retention_loop, store, App, Config};

#[derive(Parser)]
#[command(
    name = "lucina-diagnostics",
    about = "Lucina diagnostics: crash reports and log bundles"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the API and run the hourly retention.
    Serve,
    /// Print reports, newest first.
    List {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        /// RFC 3339, YYYY-MM-DD, or 30m / 24h / 7d.
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Print one report.json.
    Show { id: String },
    /// Delete reports older than the retention now.
    Prune,
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
            let app: Arc<App> = App::new(cfg)?;
            tokio::spawn(retention_loop(app.clone()));
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
            let s = store::Store::open(&cfg.data_dir)?;
            let since = since
                .map(|v| {
                    api::parse_since(&v, cfg.timezone, chrono::Utc::now())
                        .context("--since: RFC 3339, YYYY-MM-DD, or 30m / 24h / 7d")
                })
                .transpose()?;
            let (rows, _) = s.list(&store::Filter {
                app,
                kind,
                since,
                limit,
                ..Default::default()
            })?;
            for r in rows {
                println!(
                    "{}  {}  {:<14} {:<11} {:>4}x  {}  {}",
                    r.id,
                    r.last_seen_at
                        .with_timezone(&cfg.timezone)
                        .format("%Y-%m-%d %H:%M"),
                    r.app,
                    r.kind,
                    r.count,
                    r.app_version,
                    r.message.lines().next().unwrap_or("")
                );
            }
            Ok(())
        }
        Cmd::Show { id } => {
            let s = store::Store::open(&cfg.data_dir)?;
            let (report, summary) = s.get(&id)?.context("no such report")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            println!(
                "count {}  last seen {}  last device {}",
                summary.count, summary.last_seen_at, summary.last_device
            );
            Ok(())
        }
        Cmd::Prune => {
            let s = store::Store::open(&cfg.data_dir)?;
            let n = s.prune(chrono::Utc::now(), cfg.retention_days)?;
            println!(
                "deleted {n} report(s) older than {} days",
                cfg.retention_days
            );
            Ok(())
        }
    }
}

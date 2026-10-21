//! `lucina`: the Rust Lucina.
//!
//! - `lucina serve` (default): the HTTP server (127.0.0.1:8790 by default).
//! - `lucina fake-deepseek --bind 127.0.0.1:8799 --script s.json [--record r.jsonl]`.
//! - `lucina check-data --data <dir>`: byte-compat round trip of the stores.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use lucina_server::{app, AppState, Config};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "lucina", version, about = "The Rust Lucina")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve /health and /api/secretary/*.
    Serve,
    /// A scripted fake DeepSeek for parity runs.
    FakeDeepseek {
        #[arg(long, default_value = "127.0.0.1:8799")]
        bind: SocketAddr,
        #[arg(long)]
        script: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Parse and re-serialize timeline.jsonl and reminders.json under a data
    /// dir (read only) and report byte equality.
    CheckData {
        #[arg(long)]
        data: PathBuf,
    },
}

fn check_data(dir: &std::path::Path) -> anyhow::Result<bool> {
    let mut all_ok = true;
    let timeline = dir.join("timeline.jsonl");
    if let Ok(text) = std::fs::read_to_string(&timeline) {
        let (mut n, mut bad) = (0usize, 0usize);
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            n += 1;
            let same = serde_json::from_str::<lucina_store::TimelineItem>(line).ok().and_then(|i| serde_json::to_string(&i).ok()).as_deref() == Some(line);
            if !same {
                bad += 1;
            }
        }
        println!("timeline.jsonl: {n} lines, {bad} not byte-identical");
        all_ok &= bad == 0;
    }
    if let Ok(bytes) = std::fs::read(dir.join("reminders.json")) {
        let same = lucina_store::ReminderStore::reserialize(&bytes).ok().as_deref().map(str::as_bytes) == Some(&bytes[..]);
        println!("reminders.json: {}", if same { "byte-identical" } else { "DIFFERS" });
        all_ok &= same;
    }
    Ok(all_ok)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();
    match Cli::parse().cmd.unwrap_or(Cmd::Serve) {
        Cmd::Serve => {
            let cfg = Config::from_env()?;
            tracing::info!(config = ?cfg, "lucina starting (shadow mode: scheduler, Telegram and Discord off unless stated)");
            let bind = cfg.bind;
            let state = AppState::build(cfg, None)?;
            state.spawn_background();
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, "listening");
            axum::serve(listener, app(state))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Cmd::FakeDeepseek { bind, script, record } => {
            let script = match script {
                Some(p) => serde_json::from_slice(&std::fs::read(p)?)?,
                None => lucina_server::fake::Script::default(),
            };
            let fake = lucina_server::fake::spawn(bind, script, record).await?;
            println!("fake DeepSeek at {}", fake.base_url);
            tokio::signal::ctrl_c().await?;
        }
        Cmd::CheckData { data } => {
            if !check_data(&data)? {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

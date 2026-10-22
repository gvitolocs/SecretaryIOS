//! `lucina`: the Rust Lucina.
//!
//! - `lucina serve` (default): the HTTP server (127.0.0.1:8790 by default).
//! - `lucina fake-deepseek --bind 127.0.0.1:8799 --script s.json [--record r.jsonl]`.
//! - `lucina check-data --data <dir>`: byte-compat round trip of the stores.
//! - `lucina presence-watch`: the owner's Discord presence watcher (its own
//!   gateway session, intents GUILDS | GUILD_PRESENCES only), writing
//!   `<data>/presence/discord.json{,l}`. Token from `--token-file`,
//!   `LUCINA_DISCORD_TOKEN_FILE` or `DISCORD_BOT_TOKEN_LUCINA`; owner from
//!   `LUCINA_OWNER_DISCORD_USER_ID`. Exit code 78 on a fatal gateway close
//!   (bad token, disallowed intents): systemd must not restart into an
//!   identify loop.

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
    /// Compare the deterministic parsers with the live Node ones on a corpus
    /// (runs the Node side read-only in the container with `docker exec`).
    ParityParsers {
        #[arg(long, default_value = "tools/parity/parser-corpus.json")]
        cases: PathBuf,
        #[arg(long, default_value = "tools/parity/node-parsers.mjs")]
        script: PathBuf,
        #[arg(long, default_value = "lucina-nez")]
        container: String,
        /// Fixed clock for both sides (default: now).
        #[arg(long)]
        now: Option<String>,
        /// Tier-0 settings as on lucina-nez (`LUCINA_FAST_CHAT_ENABLED=true`).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        fast_chat: bool,
    },
    /// Watch the owner's Discord presence (separate gateway session).
    PresenceWatch {
        /// Data dir (default LUCINA_DATA_DIR or ./data); files go to <data>/presence.
        #[arg(long)]
        data: Option<PathBuf>,
        /// File holding the bot token (mode 0600).
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
}

/// Exit code for a fatal gateway close (`RestartPreventExitStatus=78`).
const EXIT_FATAL: i32 = 78;

fn presence_watch(data: Option<PathBuf>, token_file: Option<PathBuf>) -> anyhow::Result<()> {
    let mut cfg = Config::from_env()?;
    if let Some(d) = data {
        cfg.presence_dir = d.join("presence");
    }
    let owner: u64 = cfg
        .owner_discord_user_id
        .as_deref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("LUCINA_OWNER_DISCORD_USER_ID is not set"))?;
    let token_path = token_file.or_else(|| {
        std::env::var_os("LUCINA_DISCORD_TOKEN_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let token = match token_path {
        Some(p) => std::fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("token file {}: {}", p.display(), e.kind()))?
            .trim()
            .to_owned(),
        None => std::env::var("DISCORD_BOT_TOKEN_LUCINA")
            .unwrap_or_default()
            .trim()
            .to_owned(),
    };
    if token.is_empty() {
        anyhow::bail!("no Discord bot token (--token-file, LUCINA_DISCORD_TOKEN_FILE or DISCORD_BOT_TOKEN_LUCINA)");
    }
    let wc = lucina_discord::watcher::WatchConfig {
        token,
        owner_id: owner,
        presence_dir: cfg.presence_dir.clone(),
        retention_days: cfg.presence_retention_days,
        refresh: std::time::Duration::from_secs(60),
        first_observation_timeout: std::time::Duration::from_secs(60),
    };
    tracing::info!(config = ?wc, "presence watcher starting");
    // One thread is plenty for one shard; it also keeps RSS small.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(lucina_discord::watcher::run(wc, shutdown_signal()));
    match result {
        Ok(()) => Ok(()),
        Err(lucina_discord::watcher::WatchError::Fatal(code)) => {
            tracing::error!(?code, "presence watcher: fatal gateway close, not retrying");
            std::process::exit(EXIT_FATAL);
        }
        Err(e) => Err(e.into()),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn check_data(dir: &std::path::Path) -> anyhow::Result<bool> {
    let mut all_ok = true;
    let timeline = dir.join("timeline.jsonl");
    if let Ok(text) = std::fs::read_to_string(&timeline) {
        let (mut n, mut bad) = (0usize, 0usize);
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            n += 1;
            let same = serde_json::from_str::<lucina_store::TimelineItem>(line)
                .ok()
                .and_then(|i| serde_json::to_string(&i).ok())
                .as_deref()
                == Some(line);
            if !same {
                bad += 1;
            }
        }
        println!("timeline.jsonl: {n} lines, {bad} not byte-identical");
        all_ok &= bad == 0;
    }
    if let Ok(bytes) = std::fs::read(dir.join("reminders.json")) {
        let same = lucina_store::ReminderStore::reserialize(&bytes)
            .ok()
            .as_deref()
            .map(str::as_bytes)
            == Some(&bytes[..]);
        println!(
            "reminders.json: {}",
            if same { "byte-identical" } else { "DIFFERS" }
        );
        all_ok &= same;
    }
    Ok(all_ok)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    match Cli::parse().cmd.unwrap_or(Cmd::Serve) {
        Cmd::PresenceWatch { data, token_file } => presence_watch(data, token_file),
        cmd => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run(cmd)),
    }
}

async fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::PresenceWatch { .. } => unreachable!("handled in main"),
        Cmd::Serve => {
            let cfg = Config::from_env()?;
            tracing::info!(config = ?cfg, "lucina starting (shadow mode: scheduler, Telegram and Discord off unless stated)");
            let binds = cfg.binds.clone();
            let state = AppState::build(cfg, None)?;
            state.spawn_background();
            // One server per address (127.0.0.1 and the Tailscale IP), one shutdown.
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let mut servers = tokio::task::JoinSet::new();
            for bind in binds {
                let listener = tokio::net::TcpListener::bind(bind).await?;
                tracing::info!(%bind, "listening");
                let router = app(state.clone());
                let mut rx = stop_rx.clone();
                servers.spawn(async move {
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = rx.wait_for(|stop| *stop).await;
                        })
                        .await
                });
            }
            tokio::select! {
                _ = shutdown_signal() => tracing::info!("shutting down"),
                Some(r) = servers.join_next() => {
                    tracing::error!(result = ?r.map(|x| x.map_err(|e| e.to_string())), "a listener stopped");
                }
            }
            let _ = stop_tx.send(true);
            while servers.join_next().await.is_some() {}
        }
        Cmd::FakeDeepseek {
            bind,
            script,
            record,
        } => {
            let script = match script {
                Some(p) => serde_json::from_slice(&std::fs::read(p)?)?,
                None => lucina_server::fake::Script::default(),
            };
            let fake = lucina_server::fake::spawn(bind, script, record).await?;
            println!("fake DeepSeek at {}", fake.base_url);
            tokio::signal::ctrl_c().await?;
        }
        Cmd::ParityParsers {
            cases,
            script,
            container,
            now,
            fast_chat,
        } => {
            let mut input: serde_json::Value = serde_json::from_slice(&std::fs::read(&cases)?)?;
            let now = now.unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            input["now"] = serde_json::json!(now);
            let js = std::fs::read_to_string(&script)?;
            let mut child = std::process::Command::new("docker")
                .args([
                    "exec",
                    "-i",
                    &container,
                    "node",
                    "--input-type=module",
                    "-e",
                    &js,
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;
            {
                use std::io::Write;
                let mut stdin = child.stdin.take().expect("stdin");
                stdin.write_all(input.to_string().as_bytes())?;
            }
            let out = child.wait_with_output()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout
                .lines()
                .find_map(|l| l.strip_prefix("PARITY_JSON:"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no PARITY_JSON from the Node side: {}",
                        String::from_utf8_lossy(&out.stderr)
                            .chars()
                            .take(500)
                            .collect::<String>()
                    )
                })?;
            let node: serde_json::Value = serde_json::from_str(line)?;
            let settings = lucina_intent::heuristics::Settings {
                fast_chat,
                ..Default::default()
            };
            let (n, bad, report) =
                lucina_server::parity::parsers::compare(&input, &node, &settings);
            for l in &report {
                println!("{l}");
            }
            println!(
                "parity-parsers: {n} phrases, {} identical, {bad} different",
                n - bad
            );
            if bad > 0 {
                std::process::exit(1);
            }
        }
        Cmd::CheckData { data } => {
            if !check_data(&data)? {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

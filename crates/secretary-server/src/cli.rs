//! Command line: `serve` (default) and `token issue|list|revoke`.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::{self, Config};
use crate::devices::DeviceStore;

#[derive(Debug, Parser)]
#[command(
    name = "secretary-server",
    version,
    about = "Secretary gateway: iOS app ⇄ Lucina"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the HTTP server (default). Configured from SECRETARY_* env vars.
    Serve,
    /// Manage device tokens in $SECRETARY_DATA_DIR/devices.json.
    Token {
        /// Overrides SECRETARY_DATA_DIR.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        action: TokenAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenAction {
    /// Register a device and print its token once.
    Issue {
        #[arg(long)]
        name: String,
    },
    /// List devices (never prints tokens or hashes).
    List,
    /// Revoke a device by id and end its browser sessions; takes effect on
    /// the running server immediately.
    Revoke { id: String },
}

fn data_dir(flag: Option<PathBuf>) -> PathBuf {
    flag.or_else(|| {
        std::env::var_os("SECRETARY_DATA_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
    .unwrap_or_else(|| PathBuf::from(config::DEFAULT_DATA_DIR))
}

/// Run a `token` action, writing human output to `out`.
pub fn run_token(
    data_dir_flag: Option<PathBuf>,
    action: TokenAction,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let store = DeviceStore::new(data_dir(data_dir_flag));
    match action {
        TokenAction::Issue { name } => {
            let (dev, token) = store.issue(&name)?;
            writeln!(out, "device id: {}", dev.id)?;
            writeln!(out, "name:      {}", dev.name)?;
            writeln!(out, "token (shown once, store it in the app now):")?;
            writeln!(out, "{token}")?;
        }
        TokenAction::List => {
            let devices = store.list()?;
            if devices.is_empty() {
                writeln!(out, "no devices in {}", store.path().display())?;
            }
            for d in devices {
                writeln!(
                    out,
                    "{}\t{}\t{}\tcreated {}\tlast seen {}",
                    d.id,
                    if d.revoked { "revoked" } else { "active" },
                    d.name,
                    d.created_at,
                    d.last_seen_at.as_deref().unwrap_or("never"),
                )?;
            }
        }
        TokenAction::Revoke { id } => match crate::sessions::revoke_device(store.dir(), &id)? {
            Some((d, n)) => writeln!(
                out,
                "revoked {} ({}); {n} browser session(s) ended",
                d.id, d.name
            )?,
            None => anyhow::bail!("no device with id {id}"),
        },
    }
    Ok(())
}

pub fn init_tracing() {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        // journald/plain files get no colour codes
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "secretary_server=info".into()),
        )
        .init();
}

/// Entry point used by `main`.
pub fn main_with(cli: Cli) -> anyhow::Result<()> {
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            init_tracing();
            let cfg = Config::from_env()?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(crate::serve(cfg))
        }
        Command::Token { data_dir, action } => {
            run_token(data_dir, action, &mut std::io::stdout().lock())
        }
    }
}

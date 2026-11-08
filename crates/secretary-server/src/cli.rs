//! Command line: `serve` (default), `token issue|list|revoke` and
//! `user set|show`, `service-token generate` and
//! `notes import|import-keep|list`.

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
    /// Manage the single owner account (email + password login).
    User {
        /// Overrides SECRETARY_DATA_DIR.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        action: UserAction,
    },
    /// Manage the service token Lucina uses for /v1/insights/summary.
    ServiceToken {
        #[command(subcommand)]
        action: ServiceTokenAction,
    },
    /// Print the public key of the "Add secret" grant key (made on first use)
    /// for the broker's `[owner_grants] public_key`.
    SecretGrantKey {
        /// Overrides SECRETARY_DATA_DIR.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Notes in $SECRETARY_DATA_DIR/notes (import seed files, list).
    Notes {
        /// Overrides SECRETARY_DATA_DIR.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        action: NotesAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum NotesAction {
    /// Import seed notes: `.md` files with a front matter block (slug,
    /// title, tags, source, pinned) or `.json` files, or directories of
    /// them. A note whose slug exists is kept unless --replace.
    Import {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Replace the title, content, tags and pin of existing notes.
        #[arg(long)]
        replace: bool,
    },
    /// List notes (slug, size, source, title); never prints content.
    List,
    /// Import Google Takeout Keep zips (all parts: `takeout-…-001.zip`, …).
    /// Idempotent: a re-import skips what is there, updates notes not edited
    /// here, and never brings back what was deleted here.
    ImportKeep {
        #[arg(required = true)]
        zips: Vec<PathBuf>,
        /// Count what would happen; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Add this label to every imported note (e.g. the source account).
        #[arg(long)]
        label: Option<String>,
    },
}

fn seed_files(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p)?
                .flatten()
                .map(|e| e.path())
                .filter(|f| matches!(f.extension().and_then(|x| x.to_str()), Some("md" | "json")))
                .collect();
            v.sort();
            out.extend(v);
        } else {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// Run a `notes` action, writing human output to `out`.
pub fn run_notes(
    data_dir_flag: Option<PathBuf>,
    action: NotesAction,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let store = crate::notes::NoteStore::new(&data_dir(data_dir_flag));
    match action {
        NotesAction::Import { paths, replace } => {
            for f in seed_files(&paths)? {
                let text = std::fs::read_to_string(&f)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?;
                let md = f.extension().and_then(|x| x.to_str()) == Some("md");
                let seed = crate::notes::parse_seed(&text, md)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?;
                let slug = seed.slug.clone();
                match store
                    .import(seed, replace)
                    .map_err(|e| anyhow::anyhow!("{}: {} ({})", f.display(), e.message, e.code))?
                {
                    Some((n, true)) => {
                        writeln!(out, "created {} ({} bytes)", n.slug, n.content.len())?
                    }
                    Some((n, false)) => {
                        writeln!(out, "replaced {} ({} bytes)", n.slug, n.content.len())?
                    }
                    None => writeln!(out, "kept {slug} (exists; --replace to overwrite)")?,
                }
            }
        }
        NotesAction::ImportKeep {
            zips,
            dry_run,
            json,
            label,
        } => {
            let report = crate::keep_import::import_takeout_labelled(
                &store,
                &zips,
                dry_run,
                crate::notes::Caller("owner"),
                label.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
            } else {
                write!(out, "{}", report.render_text())?;
            }
        }
        NotesAction::List => {
            let notes = store.list(None, None, crate::notes::MAX_NOTES);
            if notes.is_empty() {
                writeln!(out, "no notes in {}", store.dir().display())?;
            }
            for n in notes {
                writeln!(
                    out,
                    "{}\t{}\t{} B\t{}\t{}{}",
                    n.id,
                    n.slug,
                    n.size,
                    n.source,
                    if n.pinned { "[pinned] " } else { "" },
                    n.title
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum UserAction {
    /// Create the owner or change its password. The password is read from
    /// stdin (one line, at least 8 characters). There is exactly one user:
    /// a different email than the stored one needs --replace.
    Set {
        #[arg(long)]
        email: String,
        /// Read the password from stdin (required; never pass it as an argument).
        #[arg(long, required = true)]
        password_stdin: bool,
        /// Replace an existing owner that has a different email.
        #[arg(long)]
        replace: bool,
    },
    /// Print the owner email and timestamps (never the hash).
    Show,
}

/// Run a `user` action reading the password from `input`.
pub fn run_user(
    data_dir_flag: Option<PathBuf>,
    action: UserAction,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let store = crate::owner::OwnerStore::new(data_dir(data_dir_flag));
    match action {
        UserAction::Set {
            email,
            password_stdin: _,
            replace,
        } => {
            let mut line = String::new();
            input.read_line(&mut line)?;
            let password = line.trim_end_matches(['\n', '\r']);
            let owner = store.set(&email, password, replace)?;
            writeln!(
                out,
                "owner set: {} (updated {})",
                owner.email, owner.updated_at
            )?;
        }
        UserAction::Show => match store.load()? {
            Some(o) => {
                writeln!(out, "email:      {}", o.email)?;
                writeln!(out, "created:    {}", o.created_at)?;
                writeln!(out, "updated:    {}", o.updated_at)?;
                writeln!(out, "failed:     {}", o.failed_count)?;
                if let Some(until) = o.locked_until {
                    writeln!(out, "locked until (unix): {until}")?;
                }
            }
            None => writeln!(out, "no owner account in {}", store.path().display())?,
        },
    }
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum ServiceTokenAction {
    /// Write a fresh `svc_…` token to PATH (mode 0600) and print only the
    /// path. Point SECRETARY_SERVICE_TOKEN_FILE at it and give the same file
    /// to Lucina.
    Generate {
        path: PathBuf,
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
}

/// Run a `service-token` action, writing human output to `out`.
pub fn run_service_token(action: ServiceTokenAction, out: &mut dyn Write) -> anyhow::Result<()> {
    match action {
        ServiceTokenAction::Generate { path, force } => {
            crate::service::write_new(&path, force)?;
            writeln!(out, "service token written to {} (0600)", path.display())?;
        }
    }
    Ok(())
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
    /// Mark a device as a test client (`on`) or not (`off`): its turns go to Lucina's
    /// self-test conversation and never notify. Takes effect on the running server at once.
    Test { id: String, state: String },
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
                    "{}\t{}{}\t{}\tcreated {}\tlast seen {}",
                    d.id,
                    if d.revoked { "revoked" } else { "active" },
                    if d.test { " test" } else { "" },
                    d.name,
                    d.created_at,
                    d.last_seen_at.as_deref().unwrap_or("never"),
                )?;
            }
        }
        TokenAction::Test { id, state } => {
            let on = match state.as_str() {
                "on" => true,
                "off" => false,
                _ => anyhow::bail!("state: on or off"),
            };
            match store.set_test(&id, on)? {
                Some(d) => writeln!(out, "{} ({}): test {}", d.id, d.name, if on { "on" } else { "off" })?,
                None => anyhow::bail!("no device {id}"),
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
        Command::User { data_dir, action } => run_user(
            data_dir,
            action,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        ),
        Command::ServiceToken { action } => {
            run_service_token(action, &mut std::io::stdout().lock())
        }
        Command::Notes { data_dir, action } => {
            run_notes(data_dir, action, &mut std::io::stdout().lock())
        }
        Command::SecretGrantKey { data_dir: d } => {
            let key = crate::owner_secrets::GrantSigner::new(&data_dir(d)).public_key()?;
            writeln!(std::io::stdout().lock(), "{key}")?;
            Ok(())
        }
    }
}

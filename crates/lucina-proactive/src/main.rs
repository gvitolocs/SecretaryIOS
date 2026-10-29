//! `lucina-proactive`: Lucina's proactive engine.
//!
//! - `run`: the loop (dry-run unless `LUCINA_PROACTIVE_DRY_RUN=false`).
//! - `once`: one tick now, report as JSON.
//! - `replay --day YYYY-MM-DD`: the 24-hour dry-run replay against real
//!   data, read only, in memory (`--json` for the full report).
//! - `migrate`: create or update the `lucina_proactive` schema.
//! - `check`: configuration and connectivity, read only.
//! - `report --hours 24`: what the engine sent or would have sent, and why.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use clap::{Parser, Subcommand};
use lucina_proactive::config::{Config, Secrets, ENV_NAMES};
use lucina_proactive::store::{PgStore, Store};
use lucina_proactive::{envfile, replay, setup};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "lucina-proactive",
    version,
    about = "Lucina's proactive engine"
)]
struct Cli {
    /// The TOML config (default: LUCINA_PROACTIVE_CONFIG or ~/.config/lucina-proactive/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The loop.
    Run,
    /// One tick now.
    Once,
    /// Dry-run a whole local day against real data (read only, in memory).
    Replay {
        /// Local day (default: today).
        #[arg(long)]
        day: Option<NaiveDate>,
        #[arg(long, default_value_t = 5)]
        step_minutes: i64,
        /// Templates instead of DeepSeek.
        #[arg(long)]
        no_llm: bool,
        /// Print the full JSON report instead of the text one.
        #[arg(long)]
        json: bool,
        /// Hypothetical calendar event, "HH:MM Title" (repeatable; labelled in the report).
        #[arg(long = "what-if")]
        what_if: Vec<String>,
        /// Hypothetical outage of a health target, "Name=HH:MM-HH:MM" (repeatable).
        #[arg(long = "what-if-down")]
        what_if_down: Vec<String>,
    },
    /// Apply the schema migrations.
    Migrate,
    /// Check configuration and connectivity (read only).
    Check,
    /// Outreach and decisions of the last hours, from the database.
    Report {
        #[arg(long, default_value_t = 24)]
        hours: i64,
    },
}

fn load(path: Option<PathBuf>) -> Result<(Config, Secrets)> {
    let cfg = Config::load(path, |k| {
        std::env::var(k).ok().filter(|v| !v.trim().is_empty())
    })?;
    let mut wanted: Vec<String> = ENV_NAMES.iter().map(|s| (*s).to_owned()).collect();
    wanted.extend(cfg.health.targets.iter().filter_map(|t| t.url_env.clone()));
    let mut files = cfg.paths.env_files.clone();
    if let Ok(extra) = std::env::var("LUCINA_PROACTIVE_ENV_FILES") {
        files.extend(
            extra
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
    }
    let lookup = envfile::lookup(envfile::load(&files, &wanted));
    let secrets = Secrets::from_lookup(&cfg, lookup);
    Ok((cfg, secrets))
}

fn pg(secrets: &Secrets) -> Result<PgStore> {
    let url = secrets
        .database_url
        .as_deref()
        .context("no database: set LUCINA_PROACTIVE_DATABASE_URL or GROCERY_DATABASE_URL")?;
    PgStore::connect_lazy(url)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("LUCINA_PROACTIVE_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let (cfg, secrets) = load(cli.config)?;
    match cli.cmd {
        Cmd::Run => run(cfg, secrets).await,
        Cmd::Once => {
            let store = Arc::new(pg(&secrets)?);
            let engine = setup::engine(
                Arc::new(cfg.clone()),
                &secrets,
                store,
                setup::live_parts(&cfg, &secrets)?,
            );
            let r = engine.tick(Utc::now(), true).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
        Cmd::Replay {
            day,
            step_minutes,
            no_llm,
            json,
            what_if,
            what_if_down,
        } => {
            let tz = cfg.tz();
            let day = day.unwrap_or_else(|| Utc::now().with_timezone(&tz).date_naive());
            let r = replay::run(
                cfg,
                &secrets,
                replay::ReplayOptions {
                    day,
                    step_minutes,
                    llm: !no_llm,
                    what_if_events: what_if
                        .iter()
                        .map(|s| replay::parse_what_if_event(s))
                        .collect::<Result<_>>()?,
                    what_if_down: what_if_down
                        .iter()
                        .map(|s| replay::parse_what_if_down(s))
                        .collect::<Result<_>>()?,
                },
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", replay::render(&r, tz));
            }
            Ok(())
        }
        Cmd::Migrate => {
            let store = pg(&secrets)?;
            let applied = store.migrate().await?;
            println!("migrations applied now: {applied:?}");
            Ok(())
        }
        Cmd::Check => check(cfg, secrets).await,
        Cmd::Report { hours } => {
            let store = pg(&secrets)?;
            let to = Utc::now();
            let from = to - chrono::Duration::hours(hours);
            let out = store.outreach_between(from, to).await?;
            let dec = store.decisions_between(from, to).await?;
            let tz = cfg.tz();
            println!("## Outreach ({})", out.len());
            for o in out {
                println!(
                    "- {} {} {} [{}] {}\n  {}\n  why: {}",
                    o.sent_at.with_timezone(&tz).format("%m-%d %H:%M"),
                    o.kind,
                    o.channel,
                    o.status,
                    o.id,
                    o.message.replace('\n', "\n  "),
                    o.why
                );
            }
            println!("\n## Decisions ({})", dec.len());
            for (c, e) in dec {
                println!(
                    "- {} {} {} {:.2} {}: {}",
                    c.created_at.with_timezone(&tz).format("%m-%d %H:%M"),
                    c.kind,
                    c.decision.as_str(),
                    c.score,
                    e.subject,
                    c.reason
                );
            }
            Ok(())
        }
    }
}

async fn run(cfg: Config, secrets: Secrets) -> Result<()> {
    let store = Arc::new(pg(&secrets)?);
    let applied = store.migrate().await.context("migrations")?;
    if !applied.is_empty() {
        tracing::info!(?applied, "migrations applied");
    }
    let tick = std::time::Duration::from_secs(cfg.tick_seconds.max(15));
    tracing::info!(
        dry_run = cfg.dry_run,
        tick_secs = tick.as_secs(),
        time_zone = %cfg.time_zone,
        secrets = ?secrets,
        "lucina-proactive starting"
    );
    if !cfg.dry_run {
        tracing::warn!("LIVE mode: outreach will be sent");
    }
    let parts = setup::live_parts(&cfg, &secrets)?;
    let engine = setup::engine(Arc::new(cfg), &secrets, store, parts);
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = term.recv() => { tracing::info!("SIGTERM: stopping"); return Ok(()); }
            _ = tokio::signal::ctrl_c() => { tracing::info!("interrupted: stopping"); return Ok(()); }
        }
        match engine.tick(Utc::now(), true).await {
            Ok(r) => {
                for e in &r.errors {
                    tracing::warn!(error = %e, "tick error");
                }
                if r.new_events > 0 || !r.outreach.is_empty() {
                    tracing::info!(new_events = r.new_events, outreach = r.outreach.len(), pulse = %r.pulse, "tick");
                }
            }
            Err(e) => tracing::error!(error = %e, "tick failed"),
        }
    }
}

async fn check(cfg: Config, secrets: Secrets) -> Result<()> {
    println!("dry_run: {}", cfg.dry_run);
    println!("time_zone: {}", cfg.time_zone);
    println!("secrets: {secrets:?}");
    let parts = setup::live_parts(&cfg, &secrets)?;
    let now = Utc::now();
    match parts.api.notify_settings().await {
        Ok(Some(_)) => println!("secretary-server settings: readable (GET /v1/notify/settings)"),
        Ok(None) => println!(
            "secretary-server settings: route missing (not deployed yet): fallback quiet hours"
        ),
        Err(e) => println!("secretary-server settings: {e}"),
    }
    match parts
        .api
        .calendar_events(now, now + chrono::Duration::days(1))
        .await
    {
        Ok(v) => println!("calendar: ok, {} event(s) in the next 24 h", v.len()),
        Err(e) => println!("calendar: {e}"),
    }
    match parts.api.notes(None).await {
        Ok(v) => println!(
            "notes: ok, {} note(s), {} pinned",
            v.len(),
            v.iter().filter(|n| n.pinned).count()
        ),
        Err(e) => println!("notes: {e}"),
    }
    match parts.reminders.reminders(now) {
        Ok(v) => println!(
            "reminders: ok, {} ({} pending)",
            v.len(),
            v.iter().filter(|r| r.status == "pending").count()
        ),
        Err(e) => println!("reminders: {e}"),
    }
    match parts.timeline.items(now - chrono::Duration::days(1), now) {
        Ok(v) => println!("timeline: ok, {} item(s) in the last 24 h", v.len()),
        Err(e) => println!("timeline: {e}"),
    }
    println!("presence: {:?}", parts.presence.at(now));
    println!(
        "deepseek: {}",
        if parts.model.is_some() {
            "key available"
        } else {
            "no key: templates"
        }
    );
    println!(
        "telegram: {}",
        if parts.telegram.is_some() && secrets.owner_chat_id.is_some() {
            "configured"
        } else {
            "not configured"
        }
    );
    println!(
        "timeline sink: {}",
        lucina_proactive::channels::sink_from(&cfg.channels).describe()
    );
    if let Some(url) = &secrets.database_url {
        let store = PgStore::connect_lazy(url)?;
        let r: Result<(bool, bool)> = async {
            let row: (bool, bool) = sqlx::query_as(
                "SELECT has_database_privilege(current_user, current_database(), 'CREATE'), \
                 EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'lucina_proactive')",
            )
            .fetch_one(store.pool())
            .await?;
            Ok(row)
        }
        .await;
        match r {
            Ok((create, exists)) => println!("database: reachable; may create schemas: {create}; lucina_proactive exists: {exists}"),
            Err(e) => println!("database: {e}"),
        }
    } else {
        println!("database: not configured");
    }
    if cfg.channels.timeline_sink == "file" {
        let p = cfg.channels.timeline_path.clone().unwrap_or_default();
        let writable = std::fs::OpenOptions::new().append(true).open(&p).is_ok();
        if !writable {
            bail!("timeline_path {} is not writable", p.display());
        }
    }
    Ok(())
}

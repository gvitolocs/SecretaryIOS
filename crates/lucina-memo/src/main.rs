//! `lucina-memo`: the voice-memo service on nezopt.
//!
//!   lucina-memo serve                       # API + pipeline worker
//!   lucina-memo import <file> [--title T] [--recorded-at RFC3339]
//!   lucina-memo process <id>                # run the remaining steps now (no server needed)
//!   lucina-memo reprocess <id> [--transcript] | resummarize <id> [--instructions ..]
//!   lucina-memo list
use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use lucina_memo::{api, store, worker, App, Config};

#[derive(Parser)]
#[command(name = "lucina-memo", about = "Lucina voice memos: upload API and pipeline")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the API and run the pipeline worker.
    Serve,
    /// Upload a recording file to the running server (same protocol as the apps).
    Import {
        file: std::path::PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        recorded_at: Option<DateTime<Utc>>,
    },
    /// Run the remaining steps of one memo in this process.
    Process { id: String },
    /// Queue a memo again from its summary (or, with --transcript, from scratch).
    Reprocess {
        id: String,
        #[arg(long)]
        transcript: bool,
    },
    /// Queue a new summary with optional instructions.
    Resummarize {
        id: String,
        #[arg(long)]
        instructions: Option<String>,
    },
    /// Print the memos.
    List,
    /// Register every finished memo in the media library again (idempotent).
    LibrarySync,
}

fn app(cfg: Config) -> anyhow::Result<Arc<App>> {
    let ds = lucina_llm::DeepSeekConfig::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    let label = ds.model.clone();
    let llm = lucina_llm::DeepSeek::new(ds).context("DeepSeek client")?;
    App::new(cfg, Arc::new(llm), label)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match cli.cmd {
        Cmd::Serve => {
            anyhow::ensure!(
                cfg.gateway_token_file.is_some() || cfg.service_token_file.is_some(),
                "set LUCINA_MEMO_GATEWAY_TOKEN_FILE and/or LUCINA_MEMO_SERVICE_TOKEN_FILE"
            );
            let app = app(cfg)?;
            tokio::spawn(worker::run(app.clone()));
            let router = api::router(app.clone());
            let mut servers = Vec::new();
            for bind in &app.cfg.binds {
                let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("bind {bind}"))?;
                tracing::info!(%bind, "lucina-memo listening");
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
            tracing::info!("lucina-memo stopping");
            Ok(())
        }
        Cmd::Import { file, title, recorded_at } => {
            let id = import(&cfg, &file, title, recorded_at).await?;
            println!("{id}");
            Ok(())
        }
        Cmd::Process { id } => {
            let app = app(cfg)?;
            worker::process(&app, &id).await.map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&app.store.get(&id))?);
            Ok(())
        }
        Cmd::Reprocess { id, transcript } => {
            let app = app(cfg)?;
            let m = worker::request_reprocess(&app, &id, transcript)?;
            println!("{} queued (revision {})", m.id, m.revision);
            Ok(())
        }
        Cmd::Resummarize { id, instructions } => {
            let app = app(cfg)?;
            let m = worker::request_resummary(&app, &id, instructions)?;
            println!("{} queued (revision {})", m.id, m.revision);
            Ok(())
        }
        Cmd::LibrarySync => {
            let app = app(cfg)?;
            for m in app.store.list().into_iter().filter(|m| m.status == lucina_memo::model::Status::Done) {
                lucina_memo::library::register(&app, &m).await;
                println!("{}  {}", m.id, m.title);
            }
            Ok(())
        }
        Cmd::List => {
            let s = store::Store::open(&cfg.data_dir)?;
            for m in s.list() {
                println!(
                    "{}  {}  {:>8}  {:<12} {}{}",
                    m.id,
                    m.recorded_at.format("%Y-%m-%d %H:%M"),
                    lucina_memo::model::clock(m.duration_sec),
                    format!("{:?}", m.status).to_lowercase(),
                    m.title,
                    m.error.map(|e| format!("  [{e}]")).unwrap_or_default()
                );
            }
            Ok(())
        }
    }
}

/// Uploads `file` through the local API in 4 MiB chunks.
async fn import(cfg: &Config, file: &std::path::Path, title: Option<String>, recorded_at: Option<DateTime<Utc>>) -> anyhow::Result<String> {
    let token = std::fs::read_to_string(cfg.service_token_file.as_ref().context("LUCINA_MEMO_SERVICE_TOKEN_FILE")?)?;
    let token = token.trim();
    let base = format!("http://{}", cfg.binds.first().context("LUCINA_MEMO_BIND")?);
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "webm" => "audio/webm",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        other => anyhow::bail!("unsupported extension .{other}"),
    };
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let recorded_at = recorded_at.unwrap_or_else(|| {
        std::fs::metadata(file).and_then(|m| m.modified()).map(DateTime::<Utc>::from).unwrap_or_else(|_| Utc::now())
    });
    let http = reqwest::Client::new();
    let created: serde_json::Value = http
        .post(format!("{base}/v1/memos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "clientId": format!("import-{}", Utc::now().timestamp_millis()),
            "title": title,
            "recordedAt": recorded_at,
            "mime": mime,
            "sizeBytes": bytes.len(),
            "source": "import",
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["memo"]["id"].as_str().context("no id")?.to_owned();
    let mut offset = 0usize;
    for chunk in bytes.chunks(4 << 20) {
        http.put(format!("{base}/v1/memos/{id}/audio"))
            .bearer_auth(token)
            .header("Upload-Offset", offset)
            .body(chunk.to_vec())
            .send()
            .await?
            .error_for_status()?;
        offset += chunk.len();
    }
    use sha2::Digest;
    let sha: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    http.post(format!("{base}/v1/memos/{id}/complete"))
        .bearer_auth(token)
        .json(&serde_json::json!({"sha256": sha}))
        .send()
        .await?
        .error_for_status()?;
    Ok(id)
}

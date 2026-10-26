use std::path::PathBuf;

use clap::{Parser, Subcommand};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::{http, import, store};

#[derive(Parser)]
#[command(name = "lucina-media", about = "Lucina's media library server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the API (config from LUCINA_MEDIA_* env).
    Serve,
    /// Create the store layout and its marker on the big disk.
    Init,
    /// Import a folder or Takeout zip (copies; sources are never changed).
    Import {
        path: PathBuf,
        #[arg(long, default_value = "import")]
        source: String,
        #[arg(long)]
        album: Option<String>,
    },
    /// Move spooled originals to the store now (respects the quiet hours).
    FlushSpool,
    /// Regenerate thumbnails (all items, or only one kind: photo, video, audio).
    Rethumb {
        #[arg(long)]
        kind: Option<String>,
    },
    /// Index a folder in place (no copy) as album LABEL.
    IndexFolder { label: String, path: PathBuf },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cfg = Config::from_env()?;
    match cli.cmd {
        Cmd::Init => {
            store::init_store(&cfg)?;
            Library::open(cfg)?;
            println!("initialised");
        }
        Cmd::Import {
            path,
            source,
            album,
        } => {
            let lib = Library::open(cfg)?;
            let stats = tokio::task::spawn_blocking(move || {
                import::import_path(&lib, &path, &import::ImportOpts { source, album })
            })
            .await??;
            println!("{stats:?}");
        }
        Cmd::FlushSpool => {
            let lib = Library::open(cfg)?;
            println!("moved {}", store::flush_spool(&lib.cfg, &lib.db)?);
        }
        Cmd::Rethumb { kind } => {
            let lib = Library::open(cfg.clone())?;
            let ids: Vec<String> = lib.db.with(|c| {
                let mut st = c.prepare("SELECT id FROM items WHERE ?1 IS NULL OR kind = ?1")?;
                let rows = st.query_map([&kind], |r| r.get(0))?;
                rows.collect()
            })?;
            for id in &ids {
                // The old poster would be reused; drop it so a new frame is picked.
                let _ = std::fs::remove_file(
                    lucina_media::thumbs::dir_for(&cfg, id).join("poster.jpg"),
                );
            }
            let n = lib.db.with(|c| {
                c.execute(
                    "UPDATE items SET thumb_state = 'pending' WHERE ?1 IS NULL OR kind = ?1",
                    [&kind],
                )
            })?;
            println!("{n} items queued");
        }
        Cmd::IndexFolder { label, path } => {
            let lib = Library::open(cfg)?;
            let r = tokio::task::spawn_blocking(move || ingest::index_folder(&lib, &label, &path))
                .await??;
            println!("added {}, unchanged {}", r.0, r.1);
        }
        Cmd::Serve => serve(cfg).await?,
    }
    Ok(())
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    if cfg.upstream_key.is_none() && !cfg.listen.ip().is_loopback() {
        anyhow::bail!(
            "refusing to listen on {} without an upstream key",
            cfg.listen
        );
    }
    if !store::store_available(&cfg) {
        tracing::warn!(store = %cfg.store_dir.display(), "store marker missing: uploads stay in the spool");
    }
    let lib = Library::open(cfg.clone())?;
    tokio::spawn(ingest::thumb_worker(lib.clone()));
    tokio::spawn(ingest::spool_mover(lib.clone()));
    {
        let l = lib.clone();
        tokio::spawn(async move {
            loop {
                let l2 = l.clone();
                let _ = tokio::task::spawn_blocking(move || ingest::sweep_uploads(&l2)).await;
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
    }
    if !cfg.folders.is_empty() {
        let l = lib.clone();
        tokio::spawn(async move {
            loop {
                for (label, path) in l.cfg.folders.clone() {
                    let l2 = l.clone();
                    match tokio::task::spawn_blocking(move || {
                        ingest::index_folder(&l2, &label, &path)
                    })
                    .await
                    {
                        Ok(Ok((a, _))) if a > 0 => tracing::info!(added = a, "folder indexed"),
                        Ok(Err(e)) => tracing::warn!(error = %e, "folder index failed"),
                        _ => {}
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
            }
        });
    }
    let stream = match &cfg.play_secret {
        Some(secret) => {
            let mut sc = secretary_media_stream::StreamConfig::new(
                secret.clone(),
                cfg.stream_cache_dir.clone(),
                cfg.progress_path.clone(),
            );
            sc.ffmpeg = cfg.ffmpeg.clone();
            sc.ffprobe = cfg.ffprobe.clone();
            let src = std::sync::Arc::new(lucina_media::source::CatalogSource(lib.clone()));
            Some(secretary_media_stream::router(src, sc).map_err(anyhow::Error::msg)?)
        }
        None => {
            tracing::warn!("no play secret: /play and /v1/media/s/* are off");
            None
        }
    };
    let public = http::router(lib.clone(), stream);
    let internal = http::internal_router(lib.clone());
    if cfg.listen == cfg.internal_listen {
        let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
        tracing::info!(addr = %cfg.listen, "serving public + internal");
        axum::serve(listener, public.merge(internal)).await?;
    } else {
        let pl = tokio::net::TcpListener::bind(cfg.listen).await?;
        let il = tokio::net::TcpListener::bind(cfg.internal_listen).await?;
        anyhow::ensure!(
            cfg.internal_listen.ip().is_loopback(),
            "internal listener must be loopback"
        );
        tracing::info!(public = %cfg.listen, internal = %cfg.internal_listen, "serving");
        let a = tokio::spawn(async move { axum::serve(pl, public).await });
        let b = tokio::spawn(async move { axum::serve(il, internal).await });
        tokio::select! {
            r = a => r??,
            r = b => r??,
        }
    }
    Ok(())
}

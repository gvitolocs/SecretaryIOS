use std::path::PathBuf;

use clap::{Parser, Subcommand};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::{archive_jxl, archive_jxl_measure, drive, extract, http, import, store};

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
    /// Index a folder in place (no copy) as album LABEL: a film folder's media, or every file of a
    /// Drive folder.
    IndexFolder { label: String, path: PathBuf },
    /// Find the same photo in two encodings and write the dry-run report (JSON, ids and scores only).
    /// Reads the catalog read-only; hashes go to a scratch database, never into the catalog.
    DedupReport {
        /// Where the full report goes (default: ~/scratch/gallery/dedup/report.json).
        #[arg(long)]
        report: Option<PathBuf>,
        /// The scratch database for perceptual hashes (default: next to the report).
        #[arg(long)]
        hashes: Option<PathBuf>,
        /// The library's data directory (default: LUCINA_MEDIA_DATA_DIR).
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Largest distance (0 to 64) for a duplicate with no time or name evidence.
        #[arg(long)]
        strict: Option<f32>,
        /// Largest distance for a duplicate that agrees on the moment or on name and size.
        #[arg(long)]
        close: Option<f32>,
        /// Largest distance at which two pictures taken together are a burst, not a duplicate.
        #[arg(long)]
        similar: Option<f32>,
        /// Pause while the CPU is at or above this many degrees.
        #[arg(long, default_value_t = lucina_media::dedup_cli::MAX_TEMP_C)]
        max_temp: f64,
    },
    /// Compute the perceptual hashes of photos that have none, into the library's own table.
    DedupHash {
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Hide the duplicates a report found under their survivors. Nothing is deleted; `dedup-undo`
    /// reverses one. Needs --i-have-a-backup (or --dry-run).
    DedupApply {
        #[arg(long)]
        report: PathBuf,
        /// Only merges scoring at least this (0 to 1).
        #[arg(long, default_value_t = 0.7)]
        min_score: f32,
        /// Comma list: exact, same_photo_reencoded, resized_copy.
        #[arg(long, default_value = "exact,same_photo_reencoded,resized_copy")]
        classes: String,
        #[arg(long)]
        limit: Option<usize>,
        /// Merge pairs the owner undid before.
        #[arg(long)]
        redo_undone: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        i_have_a_backup: bool,
    },
    /// Put a merged duplicate back as its own item.
    DedupUndo { item: String },
    /// The JPEG XL archive format for the photos in the store (docs/media-library.md).
    /// `--dry-run` measures a sample read-only; `--apply` converts (and, with `--delete-originals`,
    /// deletes originals that proved themselves); `--restore-originals` undoes the deletions.
    ArchiveJxl {
        #[arg(long)]
        dry_run: bool,
        /// Convert the store's JPEGs. Refuses to run without --i-have-a-backup.
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        restore_originals: bool,
        /// The owner has a backup of the store (required by --apply).
        #[arg(long)]
        i_have_a_backup: bool,
        /// Delete each original after its verified round trip (with --apply).
        #[arg(long)]
        delete_originals: bool,
        /// Dry run: sample size (default 600: 300 old-backup JPEGs, 150 iPhone JPEGs, up to 100
        /// lossless-source files, up to 50 HEIC/RAW).
        #[arg(long)]
        sample: Option<usize>,
        /// Dry run: write the full report (summary and per-file records) here as JSON.
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long, default_value_t = 20261028)]
        seed: u64,
        /// Dry run: worker threads (run under a CPU quota).
        #[arg(long, default_value_t = 3)]
        jobs: usize,
        /// Apply: stop after this many converted files (one batch).
        #[arg(long)]
        limit: Option<usize>,
        /// Pause while the CPU is at or above this temperature in degrees.
        #[arg(long, default_value_t = 90)]
        max_temp: u32,
        /// Dry run: JPEGs per stratum that also get the slower effort-9 transcode, for comparison.
        #[arg(long, default_value_t = 25)]
        effort9: usize,
    },
    /// Write the text of a document to OUTPUT (the Drive worker runs this in a capped child process).
    /// Exits 3 when the file has no text.
    ExtractText {
        #[arg(long = "type")]
        doc_type: String,
        #[arg(long)]
        name: String,
        input: PathBuf,
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    if let Cmd::ExtractText {
        doc_type,
        name,
        input,
        output,
    } = &cli.cmd
    {
        // No config needed: this is a child of the server, reading one file.
        let doc = lucina_media::doctype::DocType::parse(doc_type)
            .ok_or_else(|| anyhow::anyhow!("unknown type {doc_type}"))?;
        match extract::extract_in_process(input, name, doc)? {
            Some(text) => std::fs::write(output, extract::truncate_text(text))?,
            None => std::process::exit(extract::NO_TEXT_EXIT),
        }
        return Ok(());
    }
    if let Cmd::DedupReport {
        report,
        hashes,
        data_dir,
        strict,
        close,
        similar,
        max_temp,
    } = &cli.cmd
    {
        // Read-only: no config, no migration, nothing written under the data directory.
        let data_dir = data_dir
            .clone()
            .or_else(|| std::env::var_os("LUCINA_MEDIA_DATA_DIR").map(PathBuf::from))
            .ok_or_else(|| anyhow::anyhow!("pass --data-dir or set LUCINA_MEDIA_DATA_DIR"))?;
        let scratch = lucina_media::dedup_cli::scratch_dir();
        let report = report
            .clone()
            .unwrap_or_else(|| scratch.join("report.json"));
        let hashes = hashes
            .clone()
            .unwrap_or_else(|| report.with_file_name("hashes.db"));
        let mut thresholds = lucina_media::dedup::Thresholds::default();
        thresholds.strict = strict.unwrap_or(thresholds.strict);
        thresholds.close = close.unwrap_or(thresholds.close);
        thresholds.similar = similar.unwrap_or(thresholds.similar);
        let args = lucina_media::dedup_cli::ReportArgs {
            data_dir,
            report: report.clone(),
            hashes,
            thresholds,
            max_temp_c: *max_temp,
        };
        let r = tokio::task::spawn_blocking(move || lucina_media::dedup_cli::run_report(&args))
            .await??;
        print!("{}", lucina_media::dedup_cli::summary(&r));
        println!("report: {}", report.display());
        return Ok(());
    }
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
            if lib.cfg.film_folders.contains(&label) {
                let r =
                    tokio::task::spawn_blocking(move || ingest::index_folder(&lib, &label, &path))
                        .await??;
                println!("added {}, unchanged {}", r.0, r.1);
            } else {
                let r = tokio::task::spawn_blocking(move || {
                    drive::index_drive_folder(&lib, &label, &path)
                })
                .await??;
                println!("{r:?}");
            }
        }
        Cmd::DedupHash { limit } => {
            let lib = Library::open(cfg)?;
            let stats =
                tokio::task::spawn_blocking(move || lucina_media::dedup_cli::run_hash(&lib, limit))
                    .await??;
            println!("{stats:?}");
        }
        Cmd::DedupApply {
            report,
            min_score,
            classes,
            limit,
            redo_undone,
            dry_run,
            i_have_a_backup,
        } => {
            let args = lucina_media::dedup_cli::ApplyArgs {
                report,
                min_score,
                classes: lucina_media::dedup_cli::parse_classes(&classes)?,
                limit,
                redo: redo_undone,
                dry_run,
                backup_ack: i_have_a_backup,
            };
            let stats = tokio::task::spawn_blocking(move || {
                lucina_media::dedup_cli::run_apply(|| Library::open(cfg), &args)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Cmd::DedupUndo { item } => {
            let lib = Library::open(cfg)?;
            let done = lib.db.with(|c| lucina_media::dedup_merge::undo(c, &item))?;
            anyhow::ensure!(done, "{item} is not a merged duplicate");
            println!("{item} is its own item again");
        }
        Cmd::DedupReport { .. } | Cmd::ExtractText { .. } => unreachable!("handled above"),
        Cmd::ArchiveJxl {
            dry_run,
            apply,
            restore_originals,
            i_have_a_backup,
            delete_originals,
            sample,
            report,
            seed,
            jobs,
            limit,
            max_temp,
            effort9,
        } => {
            anyhow::ensure!(
                [dry_run, apply, restore_originals]
                    .iter()
                    .filter(|b| **b)
                    .count()
                    == 1,
                "pass exactly one of --dry-run, --apply, --restore-originals"
            );
            anyhow::ensure!(
                !delete_originals || apply,
                "--delete-originals only goes with --apply"
            );
            if dry_run {
                // Read-only: the catalog is opened read-only and nothing is written but the report.
                let opts = archive_jxl_measure::DryRunOpts {
                    sample: sample.unwrap_or(600),
                    seed,
                    jobs,
                    max_temp_c: max_temp,
                    effort9_per_stratum: effort9,
                    checkpoint: report.as_ref().map(|p| p.with_extension("jsonl")),
                };
                let rep =
                    tokio::task::spawn_blocking(move || archive_jxl_measure::dry_run(&cfg, &opts))
                        .await??;
                if let Some(path) = report {
                    std::fs::write(&path, serde_json::to_vec_pretty(&rep)?)?;
                }
                println!("{}", serde_json::to_string_pretty(&rep.summary)?);
            } else if apply {
                anyhow::ensure!(
                    i_have_a_backup,
                    "--apply needs --i-have-a-backup: a backup of the store must exist first"
                );
                let lib = Library::open(cfg)?;
                let opts = archive_jxl::ApplyOpts {
                    i_have_a_backup,
                    delete_originals,
                    limit,
                    max_temp_c: max_temp,
                };
                let stats =
                    tokio::task::spawn_blocking(move || archive_jxl::apply(&lib, &opts)).await??;
                // Counts only.
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                let lib = Library::open(cfg)?;
                let n = tokio::task::spawn_blocking(move || archive_jxl::restore_originals(&lib))
                    .await??;
                println!("restored {n}");
            }
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
    tokio::spawn(lucina_media::trash::retention_worker(lib.clone()));
    tokio::spawn(lucina_media::lab_run::worker(lib.clone(), lucina_media::lab_run::LabConfig::from_env()));
    tokio::spawn(lucina_media::smart::worker(lib.clone(), lucina_media::smart::SmartConfig::from_env()));
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
        tokio::spawn(drive::drive_worker(lib.clone()));
        tracing::info!(
            pdftotext = ?cfg.pdftotext, pdftoppm = ?cfg.pdftoppm, soffice = ?cfg.soffice,
            "drive: document tools"
        );
        tokio::spawn(async move {
            loop {
                for (label, path) in l.cfg.folders.clone() {
                    let l2 = l.clone();
                    // Film folders keep their media-only index; every other folder is the Drive.
                    if l.cfg.film_folders.contains(&label) {
                        match tokio::task::spawn_blocking(move || {
                            ingest::index_folder(&l2, &label, &path)
                        })
                        .await
                        {
                            Ok(Ok((a, _))) if a > 0 => tracing::info!(added = a, "folder indexed"),
                            Ok(Err(e)) => tracing::warn!(error = %e, "folder index failed"),
                            _ => {}
                        }
                    } else {
                        let lbl = label.clone();
                        match tokio::task::spawn_blocking(move || {
                            drive::index_drive_folder(&l2, &label, &path)
                        })
                        .await
                        {
                            Ok(Ok(s)) if s.added + s.pruned > 0 => {
                                tracing::info!(folder = %lbl, ?s, "drive folder indexed")
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(folder = %lbl, error = %e, "drive index failed")
                            }
                            _ => {}
                        }
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
            sc.prepare = cfg.prepare;
            sc.background_scope = cfg.prepare_scope;
            sc.background_cpu_quota = cfg.prepare_cpu_quota;
            sc.vaapi_device = cfg.vaapi_device.clone();
            sc.gpu_lease = cfg.gpu_lease.clone();
            sc.background_bind_to = cfg.prepare_bind_to.clone();
            sc.prepare_window_start = cfg.prepare_window_start;
            sc.prepare_window_end = cfg.prepare_window_end;
            sc.prepare_max_temp_c = cfg.prepare_max_temp_c;
            sc.prepare_max_load = cfg.prepare_max_load;
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

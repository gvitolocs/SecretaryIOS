//! Portable read-only Kindle scanner and authenticated Library importer.
use clap::{Args, Parser, Subcommand};
use ink_library::{
    import::{merge, ImportBatch},
    model::LibraryState,
    scan::{scan_kindle, ScanOptions},
};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Scan {
        mount: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Import {
        mount: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    ImportJson {
        batch: PathBuf,
        #[command(flatten)]
        target: Target,
    },
}
#[derive(Args)]
struct Target {
    #[arg(long)]
    server: String,
    #[arg(long)]
    token_file: PathBuf,
    #[arg(long)]
    dry_run: bool,
}
fn send(batch: ImportBatch, target: Target) -> anyhow::Result<()> {
    if target.dry_run {
        let (_, o) = merge(
            &LibraryState::default(),
            &batch,
            &chrono::Utc::now().to_rfc3339(),
        );
        println!("{}", serde_json::to_string_pretty(&o)?);
        return Ok(());
    }
    let mut url = reqwest::Url::parse(&target.server)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "server must be an HTTP(S) URL without credentials"
    );
    url.set_path("/v1/library/import");
    url.set_query(None);
    url.set_fragment(None);
    let token = std::fs::read_to_string(target.token_file)?;
    anyhow::ensure!(!token.trim().is_empty(), "empty token file");
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let response = client
        .post(url)
        .bearer_auth(token.trim())
        .json(&batch)
        .send()?;
    let status = response.status();
    let text = response.text()?;
    anyhow::ensure!(status.is_success(), "import failed ({status}): {text}");
    println!("{text}");
    Ok(())
}
fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Scan { mount, json } => {
            let scan = scan_kindle(&mount, ScanOptions::default())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&scan.batch)?);
            } else {
                let mut formats = BTreeMap::new();
                let mut drm = BTreeMap::new();
                for b in &scan.batch.books {
                    *formats.entry(format!("{:?}", b.format)).or_insert(0) += 1;
                    *drm.entry(format!("{:?}", b.metadata.drm)).or_insert(0) += 1;
                }
                println!("Books: {}\nFormats: {formats:?}\nDRM: {drm:?}\nClippings: {}\nIndicators: {:?}\nFirmware: {}\nWarnings: {:?}",scan.batch.books.len(),scan.batch.clippings.len(),scan.indicators,scan.version,scan.warnings);
            }
        }
        Command::Import { mount, target } => {
            let scan = scan_kindle(&mount, ScanOptions::default())?;
            for warning in scan.warnings {
                eprintln!("Warning: {warning}");
            }
            send(scan.batch, target)?
        }
        Command::ImportJson { batch, target } => {
            let f = std::fs::File::open(batch)?;
            anyhow::ensure!(
                f.metadata()?.len() <= 32 * 1024 * 1024,
                "batch exceeds 32 MiB"
            );
            send(serde_json::from_reader(f)?, target)?;
        }
    }
    Ok(())
}

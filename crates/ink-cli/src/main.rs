//! Portable read-only Kindle scanner, Obsidian indexer and authenticated Library importer.
use clap::{Args, Parser, Subcommand};
use ink_library::{
    import::{merge, ImportBatch},
    model::LibraryState,
    scan::{scan_kindle, ScanOptions},
};
use ink_obsidian::{
    books::{LibraryBook, LibraryIndex},
    config::VaultConfig,
    index::{update, IndexState, Report},
    NotesIndexBatch,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Parser)]
#[command(about = "Read-only Kindle USB metadata scanner, Obsidian indexer and Library importer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Scan books and clippings without writing to the Kindle.
    Scan {
        /// Kindle USB mount directory containing documents/.
        mount: PathBuf,
        /// Print the complete ImportBatch JSON for a later import.
        #[arg(long)]
        json: bool,
    },
    /// Scan a Kindle and send the batch to the Library API.
    Import {
        /// Kindle USB mount directory containing documents/.
        mount: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    /// Import a previously saved scan batch without accessing a Kindle.
    ImportJson {
        /// Path to the saved ImportBatch JSON (at most 32 MiB).
        batch: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    /// Index an Obsidian vault read-only and find notes that refer to library books.
    ObsidianIndex(Box<ObsidianIndex>),
}
#[derive(Args)]
struct Target {
    /// Secretary API base URL, for example http://localhost:8080.
    #[arg(long)]
    server: String,
    /// Read the authentication token from this file; never pass the token itself.
    #[arg(long)]
    token_file: PathBuf,
    /// Merge against an empty state and print counts without sending a request.
    #[arg(long)]
    dry_run: bool,
}
#[derive(Args)]
struct ObsidianIndex {
    /// Obsidian vault folder. It is only read, never written.
    vault: PathBuf,
    /// Library JSON: a saved ImportBatch, a library state snapshot, or a {"books":[…]} listing.
    #[arg(long)]
    library: PathBuf,
    /// Print the notes-index payload (references with excerpts) as JSON.
    #[arg(long, conflicts_with = "summary")]
    json: bool,
    /// Print aggregate counts only: no paths, titles or excerpts.
    #[arg(long)]
    summary: bool,
    /// Vault name used in citations (default: the vault folder name).
    #[arg(long)]
    vault_name: Option<String>,
    /// Index only files matching this pattern (repeatable), e.g. "Books/".
    #[arg(long)]
    include: Vec<String>,
    /// Also exclude files matching this pattern (repeatable).
    #[arg(long)]
    exclude: Vec<String>,
    /// Drop the privacy defaults (Templates/, Private/, Journal/, Diary/, .obsidian/, …).
    #[arg(long)]
    no_default_excludes: bool,
    /// Incremental state file, outside the vault: only changed notes are read again.
    #[arg(long)]
    state: Option<PathBuf>,
    /// Secretary API base URL: POST the references to /v1/library/notes-index.
    #[arg(long, requires = "token_file")]
    server: Option<String>,
    /// Read the authentication token from this file; never pass the token itself.
    #[arg(long, requires = "server")]
    token_file: Option<PathBuf>,
}

fn post(
    server: &str,
    token_file: &Path,
    path: &str,
    body: &impl serde::Serialize,
) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(server)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "server must be an HTTP(S) URL without credentials"
    );
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    let token = std::fs::read_to_string(token_file)?;
    anyhow::ensure!(!token.trim().is_empty(), "empty token file");
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let response = client
        .post(url)
        .bearer_auth(token.trim())
        .json(body)
        .send()?;
    let status = response.status();
    let text = response.text()?;
    anyhow::ensure!(status.is_success(), "request failed ({status}): {text}");
    Ok(text)
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
    println!(
        "{}",
        post(
            &target.server,
            &target.token_file,
            "/v1/library/import",
            &batch
        )?
    );
    Ok(())
}

fn read_json(path: &Path, max: u64) -> anyhow::Result<Value> {
    let f = std::fs::File::open(path)?;
    anyhow::ensure!(
        f.metadata()?.len() <= max,
        "{} exceeds {max} bytes",
        path.display()
    );
    Ok(serde_json::from_reader(std::io::BufReader::new(f))?)
}
/// Books from a saved ImportBatch (ids as an empty server would assign them),
/// a LibraryState snapshot, or a `GET /v1/library/books` listing.
fn load_library(path: &Path) -> anyhow::Result<Vec<LibraryBook>> {
    let v = read_json(path, 64 * 1024 * 1024)?;
    if v.get("scannedAt").is_some() {
        let batch: ImportBatch = serde_json::from_value(v)?;
        let (state, _) = merge(
            &LibraryState::default(),
            &batch,
            &chrono::Utc::now().to_rfc3339(),
        );
        return Ok(state.books.values().map(LibraryBook::from).collect());
    }
    match v.get("books") {
        Some(Value::Object(_)) => {
            let state: LibraryState = serde_json::from_value(v)?;
            Ok(state.books.values().map(LibraryBook::from).collect())
        }
        Some(Value::Array(books)) => Ok(serde_json::from_value(Value::Array(books.clone()))?),
        _ => anyhow::bail!(
            "library must be an ImportBatch, a library state snapshot or a {{\"books\":[…]}} listing"
        ),
    }
}
/// The same rule the server applies to vault names.
fn valid_vault_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
        && !s.ends_with(' ')
}
/// Canonical path of a file that may not exist yet.
fn canonical_target(p: &Path) -> anyhow::Result<PathBuf> {
    if p.exists() {
        return Ok(std::fs::canonicalize(p)?);
    }
    let parent = p
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = p
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("state path has no file name"))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}
fn save_state(path: &Path, state: &IndexState) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let f = options.open(&tmp)?;
    serde_json::to_writer(std::io::BufWriter::new(&f), state)?;
    f.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
fn print_counts(vault: &str, r: &Report) {
    let c = &r.counts;
    println!("Vault: {vault}");
    println!(
        "Notes: {} (added {}, changed {}, removed {}, unchanged {}, read {})",
        c.notes, c.added, c.changed, c.removed, c.unchanged, c.reparsed
    );
    println!(
        "Excluded: {} folders, {} files; skipped: {}; attachments: {}",
        c.excluded_dirs, c.excluded_files, c.skipped, c.attachments
    );
    println!(
        "Links: {} (wikilinks {}, embeds {}, markdown {}); resolved {}, unresolved {}, ambiguous {}",
        c.links, c.wikilinks, c.embeds, c.markdown_links, c.resolved, c.unresolved, c.ambiguous
    );
    println!("Tags: {}; book notes: {}", c.tags, c.book_notes);
    println!(
        "Refs: {} in {} notes for {} books; by kind {:?}",
        c.refs, c.notes_with_refs, c.books_referenced, c.refs_by_how
    );
    println!("Warnings: {}", r.warnings.len());
}
fn obsidian_index(a: ObsidianIndex) -> anyhow::Result<()> {
    let books = load_library(&a.library)?;
    let lib = LibraryIndex::new(&books);
    let root = std::fs::canonicalize(&a.vault)?;
    let vault = match a.vault_name {
        Some(v) => v,
        None => root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned(),
    };
    anyhow::ensure!(
        valid_vault_name(&vault),
        "vault name must be 1-64 letters, digits, spaces, '.', '_' or '-'; pass --vault-name"
    );
    let mut cfg = VaultConfig::new(&root);
    if a.no_default_excludes {
        cfg.exclude.clear();
    }
    cfg.exclude.extend(a.exclude);
    cfg.include = a.include;
    let state_path = a.state.as_deref().map(canonical_target).transpose()?;
    if let Some(p) = &state_path {
        anyhow::ensure!(
            !p.starts_with(&root),
            "the state file must be outside the vault"
        );
    }
    let mut state = match &state_path {
        Some(p) if p.exists() => serde_json::from_value(read_json(p, 512 * 1024 * 1024)?)?,
        _ => IndexState::default(),
    };
    let report = update(&mut state, &cfg, &lib)?;
    if let Some(p) = &state_path {
        save_state(p, &state)?;
    }
    let batch = NotesIndexBatch {
        vault: vault.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        refs: report.refs.clone(),
    };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&batch)?);
    } else {
        print_counts(&vault, &report);
        if !a.summary {
            for w in &report.warnings {
                eprintln!("Warning: {w}");
            }
            for r in &report.refs {
                println!(
                    "{} {} {}#L{}",
                    r.book_id,
                    r.how.as_str(),
                    r.note_path,
                    r.line
                );
            }
        }
    }
    if let (Some(server), Some(token)) = (a.server, a.token_file) {
        let text = post(&server, &token, "/v1/library/notes-index", &batch)?;
        if a.json {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    }
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
                let mut kinds = BTreeMap::new();
                for b in &scan.batch.books {
                    *kinds.entry(format!("{:?}", b.kind)).or_insert(0) += 1;
                    *formats.entry(format!("{:?}", b.format)).or_insert(0) += 1;
                    *drm.entry(format!("{:?}", b.metadata.drm)).or_insert(0) += 1;
                }
                println!("Books: {}\nFormats: {formats:?}\nDRM: {drm:?}\nKinds: {kinds:?}\nClippings: {}\nIndicators: {:?}\nMarkers: {:?}\nFirmware: {}\nWarnings: {:?}",scan.batch.books.len(),scan.batch.clippings.len(),scan.indicators,scan.markers,scan.version,scan.warnings);
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
        Command::ObsidianIndex(a) => obsidian_index(*a)?,
    }
    Ok(())
}

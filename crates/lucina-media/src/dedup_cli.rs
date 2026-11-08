//! The `lucina-media dedup-*` commands (`main.rs` only parses their arguments).
//!
//! - `dedup-report` reads the live catalog READ-ONLY and keeps its perceptual hashes in a scratch
//!   database, never in the catalog: it writes nothing under the data directory;
//! - `dedup-hash` computes the hashes into the library's own `item_hashes` table (lazily, resumable);
//! - `dedup-apply` hides the duplicates a report found (it never deletes a file) and needs
//!   `--i-have-a-backup`;
//! - `dedup-undo` puts one merged duplicate back.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use rusqlite::{Connection, OpenFlags};

use crate::dedup::{self, Class, Report, Thresholds};
use crate::dedup_merge::{self, ApplyOpts};
use crate::ingest::Library;

/// The owner's CPU temperature limit for background work.
pub const MAX_TEMP_C: f64 = 90.0;

pub struct ReportArgs {
    pub data_dir: PathBuf,
    pub report: PathBuf,
    pub hashes: PathBuf,
    pub thresholds: Thresholds,
    pub max_temp_c: f64,
}

/// The default scratch directory for hashes and reports.
pub fn scratch_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
    PathBuf::from(home).join("scratch/gallery/dedup")
}

fn inside(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| {
        // The file may not exist yet: canonicalise its directory.
        let parent = p.parent().unwrap_or(Path::new("."));
        std::fs::canonicalize(parent)
            .map(|d| d.join(p.file_name().unwrap_or_default()))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    canon(path).starts_with(dir)
}

fn open_scratch(path: &Path, data_dir: &Path) -> anyhow::Result<Connection> {
    if inside(path, data_dir) {
        bail!(
            "{} is inside the library's data directory: scratch files go elsewhere",
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

/// Compute the missing hashes into a scratch database, judge every candidate pair, write the full
/// report (ids and scores only) to `args.report` and return it.
pub fn run_report(args: &ReportArgs) -> anyhow::Result<Report> {
    let db_path = args.data_dir.join("catalog.db");
    let cat = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {} read-only", db_path.display()))?;
    cat.busy_timeout(std::time::Duration::from_secs(10))?;
    cat.pragma_update(None, "query_only", true)?;
    if inside(&args.report, &args.data_dir) {
        bail!("the report must not be written inside the library's data directory");
    }
    let store = open_scratch(&args.hashes, &args.data_dir)?;
    let max_temp = args.max_temp_c;
    let stats = dedup::hash_missing(
        &cat,
        &store,
        &args.data_dir.join("thumbs"),
        None,
        &mut || dedup::wait_until_cool(max_temp),
    )?;
    eprintln!(
        "hashes: {} photos, {} kept, {} computed, {} without thumbnail, {} failed",
        stats.photos, stats.kept, stats.computed, stats.no_thumbnail, stats.failed
    );
    let cands = dedup::load_cands(&cat, &store)?;
    dedup::wait_until_cool(max_temp);
    let mut report = dedup::build_report(&cands, &args.thresholds);
    dedup::calibrate_known_copies(
        &cands,
        &args.data_dir.join("thumbs"),
        (cands.len() / 600).max(1),
        &mut report.calibration,
    );
    if let Some(dir) = args.report.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = args.report.with_extension("json.part");
    serde_json::to_writer(
        std::io::BufWriter::new(std::fs::File::create(&tmp)?),
        &report,
    )?;
    std::fs::rename(&tmp, &args.report)?;
    Ok(report)
}

/// One screen of the numbers, with opaque ids only.
pub fn summary(r: &Report) -> String {
    let mut out = String::new();
    out += &format!(
        "photos {} (hashed {}, flat {}, no hash {})\n",
        r.library.photos, r.library.hashed, r.library.flat, r.library.without_hash
    );
    for (name, c) in &r.classes {
        out += &format!(
            "{name:<22} pairs {:>6}  merges {:>6}  duplicate bytes {:>14}\n",
            c.pairs, c.merges, c.duplicate_bytes
        );
    }
    out += &format!(
        "merges {} hiding {} bytes\n",
        r.merge_total, r.merge_duplicate_bytes
    );
    for s in &r.by_source_pair {
        out += &format!(
            "  {} x {}  {}  pairs {} merges {} bytes {}\n",
            s.sources[0],
            s.sources[1],
            s.class.as_str(),
            s.pairs,
            s.merges,
            s.duplicate_bytes
        );
    }
    out
}

/// Compute the missing hashes into the library's own table.
pub fn run_hash(lib: &Library, limit: Option<usize>) -> anyhow::Result<dedup::HashStats> {
    let thumbs = lib.cfg.thumbs_dir();
    let stats = lib.db.with(|c| {
        dedup::hash_missing(c, c, &thumbs, limit, &mut || {
            dedup::wait_until_cool(MAX_TEMP_C)
        })
    })?;
    Ok(stats)
}

pub struct ApplyArgs {
    pub report: PathBuf,
    pub min_score: f32,
    pub classes: Vec<Class>,
    pub limit: Option<usize>,
    pub redo: bool,
    pub dry_run: bool,
    pub backup_ack: bool,
}

pub fn read_report(path: &Path) -> anyhow::Result<Report> {
    let report: Report = serde_json::from_reader(std::io::BufReader::new(
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?,
    ))?;
    if report.version != dedup::REPORT_VERSION {
        bail!("report version {} is not supported", report.version);
    }
    Ok(report)
}

/// `Library` is only opened for a real run: a dry run reads the report alone.
pub fn run_apply(
    open: impl FnOnce() -> anyhow::Result<Library>,
    args: &ApplyArgs,
) -> anyhow::Result<dedup_merge::ApplyStats> {
    if !args.dry_run && !args.backup_ack {
        bail!(
            "refusing to merge without --i-have-a-backup (use --dry-run to see what would happen)"
        );
    }
    let report = read_report(&args.report)?;
    let opts = ApplyOpts {
        min_score: args.min_score,
        classes: args.classes.clone(),
        limit: args.limit,
        redo: args.redo,
        dry_run: args.dry_run,
    };
    if args.dry_run {
        let mut scratch = Connection::open_in_memory()?;
        return Ok(dedup_merge::apply_report(&mut scratch, &report, &opts)?);
    }
    let lib = open()?;
    Ok(lib
        .db
        .with(|c| dedup_merge::apply_report(c, &report, &opts))?)
}

pub fn parse_classes(s: &str) -> anyhow::Result<Vec<Class>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| Class::parse(p).with_context(|| format!("unknown class {p}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_files_may_not_live_in_the_data_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        assert!(inside(&data.join("hashes.db"), &data));
        assert!(inside(&data.join("sub/x.json"), &data) || !data.join("sub").exists());
        assert!(!inside(&tmp.path().join("scratch/hashes.db"), &data));
        assert!(open_scratch(&data.join("hashes.db"), &data).is_err());
        assert!(open_scratch(&tmp.path().join("scratch/h.db"), &data).is_ok());
    }

    #[test]
    fn classes_parse_and_unknown_ones_are_refused() {
        assert_eq!(
            parse_classes("exact, resized_copy").unwrap(),
            vec![Class::Exact, Class::ResizedCopy]
        );
        assert!(parse_classes("exact,nope").is_err());
        assert!(parse_classes("").unwrap().is_empty());
    }

    #[test]
    fn apply_needs_the_backup_flag_but_a_dry_run_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("r.json");
        let r = dedup::build_report(&[], &Thresholds::default());
        std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
        let args = |dry_run, backup_ack| ApplyArgs {
            report: path.clone(),
            min_score: 0.7,
            classes: vec![Class::SameReencoded],
            limit: None,
            redo: false,
            dry_run,
            backup_ack,
        };
        let never = || -> anyhow::Result<Library> { bail!("must not open the library") };
        assert!(run_apply(never, &args(false, false)).is_err());
        assert_eq!(run_apply(never, &args(true, false)).unwrap().planned, 0);
    }

    #[test]
    fn the_report_reads_the_catalog_read_only_and_writes_only_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let cfg = crate::config::Config::for_dirs(data.clone(), tmp.path().join("store"));
        let lib = Library::open(cfg).unwrap();
        lib.db
            .with(|c| {
                c.execute("INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h1', 5, 'jpg', 'store', 'now')", [])?;
                c.execute(
                    "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, thumb_state, added_at, updated_at)
                     VALUES ('itm_aa11', 'photo', 'x', 'h1', 'image/jpeg', 5, 'x', 'phone-ios', 'none', 'x', 'x')",
                    [],
                )
            })
            .unwrap();
        let args = ReportArgs {
            data_dir: data.clone(),
            report: tmp.path().join("out/report.json"),
            hashes: tmp.path().join("out/hashes.db"),
            thresholds: Thresholds::default(),
            max_temp_c: 1000.0,
        };
        let r = run_report(&args).unwrap();
        assert_eq!(r.library.photos, 1);
        assert_eq!(r.library.without_hash, 1);
        assert!(args.report.is_file());
        assert!(args.hashes.is_file());
        assert!(summary(&r).contains("photos 1"));
        let rows: i64 = lib
            .db
            .with(|c| c.query_row("SELECT COUNT(*) FROM item_hashes", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 0, "nothing was written to the catalog");
    }
}

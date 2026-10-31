//! Verified, additive memo archives. Missing disks never receive writes.
use crate::{
    model::{Memo, Status},
    store::{read_json, write_atomic, write_json},
    App,
};
use anyhow::{Context, Result};
use chrono::{Datelike, Utc};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Default, Debug)]
pub struct ArchiveReport {
    pub copied: u64,
    pub unchanged: u64,
    pub bytes: u64,
    pub verified_ok: u64,
    pub mismatches: u64,
}
pub fn check_mount(app: &App) -> Result<&Path> {
    let root = app.cfg.archive_dir.as_deref().context("archive disabled")?;
    if let Some(mount) = &app.cfg.archive_require_mount {
        let parent = mount.parent().context("mount has no parent")?;
        anyhow::ensure!(
            fs::metadata(mount)?.dev() != fs::metadata(parent)?.dev(),
            "archive mount absent: {}",
            mount.display()
        );
    }
    Ok(root)
}
fn regular(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for e in fs::read_dir(path)? {
        let e = e?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        if e.file_type()?.is_file()
            && !name.starts_with(".copy-")
            && !name.contains(".tmp")
            && !name.ends_with(".part")
            && name != "SHA256SUMS"
        {
            anyhow::ensure!(!name.contains(['\n', '\r']), "invalid archive filename");
            files.push(e.path());
        }
    }
    files.sort();
    Ok(files)
}
fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut sha = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
fn locate(app: &App, id: &str) -> Result<(Memo, PathBuf, PathBuf, bool)> {
    anyhow::ensure!(
        id.starts_with("mm_") && id[3..].chars().all(|c| c.is_ascii_hexdigit()),
        "invalid memo id"
    );
    let root = check_mount(app)?;
    if let Some(m) = app.store.get(id) {
        let dest = root.join(m.recorded_at.year().to_string()).join(id);
        return Ok((m, app.store.dir(id), dest, false));
    }
    let mut found = None;
    for entry in fs::read_dir(app.cfg.data_dir.join("trash"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(m) = read_json::<Memo>(&entry.path().join("meta.json"))? {
            if m.id == id {
                found = Some((m, entry.path()));
                break;
            }
        }
    }
    let (m, source) = found.context("memo not found")?;
    Ok((m, source, root.join("trash").join(id), true))
}
fn sums(dest: &Path) -> Result<BTreeMap<String, String>> {
    let mut sums = BTreeMap::new();
    for p in regular(dest)? {
        sums.insert(
            p.file_name()
                .unwrap()
                .to_str()
                .context("non-UTF8 filename")?
                .to_owned(),
            hash(&p)?,
        );
    }
    Ok(sums)
}
fn manifest(dest: &Path) -> Result<()> {
    let lines: String = sums(dest)?
        .into_iter()
        .map(|(name, sha)| format!("{sha}  {name}\n"))
        .collect();
    write_atomic(&dest.join("SHA256SUMS"), lines.as_bytes())?;
    Ok(())
}
/// Archives one memo and records it on the memo (`archived_rev`): the server's worker only.
pub fn archive_memo(app: &App, id: &str) -> Result<ArchiveReport> {
    archive_files(app, id, true)
}

/// `record: false` (the CLI, a separate process): copy and verify only. The memo's meta.json
/// belongs to the running server; writing it from here could undo the server's own update.
fn archive_files(app: &App, id: &str, record: bool) -> Result<ArchiveReport> {
    let _lock = lock_archive(app)?;
    archive_files_locked(app, id, record)
}

/// Serializes archive writers across processes: the server's worker and the daily CLI run
/// (`lucina-memo archive`) take the same lock file in the archive root.
fn lock_archive(app: &App) -> Result<(std::sync::MutexGuard<'_, ()>, fs::File)> {
    let guard = app.archive_lock.lock().unwrap();
    let root = check_mount(app)?;
    fs::create_dir_all(root)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".archive.lock"))?;
    fs4::fs_std::FileExt::lock_exclusive(&file)?;
    Ok((guard, file))
}

fn archive_files_locked(app: &App, id: &str, record: bool) -> Result<ArchiveReport> {
    let (m, source, dest, trash) = locate(app, id)?;
    fs::create_dir_all(&dest)?;
    let mut report = ArchiveReport::default();
    for path in regular(&source)? {
        let target = dest.join(path.file_name().unwrap());
        let expected = hash(&path)?;
        if target.is_file() && hash(&target)? == expected {
            report.unchanged += 1;
            continue;
        }
        let temp = dest.join(format!(".copy-{}", std::process::id()));
        let mut input = fs::File::open(&path)?;
        let mut output = fs::File::create(&temp)?;
        let bytes = std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        fs::rename(&temp, &target)?;
        anyhow::ensure!(
            hash(&target)? == expected,
            "archive hash mismatch: {}",
            target.display()
        );
        report.copied += 1;
        report.bytes += bytes;
    }
    manifest(&dest)?;
    // Mark only after all source files have been verified. Preserve an existing marker
    // on an unchanged revision so a second pass is entirely unchanged.
    if record && (m.archived_rev != Some(m.revision) || m.archived_at.is_none()) {
        if trash {
            let mut saved = m.clone();
            saved.archived_rev = Some(m.revision);
            saved.archived_at = Some(Utc::now());
            write_json(&source.join("meta.json"), &saved)?;
        } else {
            app.store.update(id, |saved| {
                saved.archived_rev = Some(m.revision);
                saved.archived_at = Some(Utc::now());
            })?;
        }
        let finalize = (|| -> Result<()> {
            let metadata = fs::read(source.join("meta.json"))?;
            write_atomic(&dest.join("meta.json"), &metadata)?;
            anyhow::ensure!(
                hash(&dest.join("meta.json"))? == format!("{:x}", Sha256::digest(&metadata)),
                "metadata verification failed"
            );
            report.bytes += metadata.len() as u64;
            manifest(&dest)?;
            Ok(())
        })();
        if let Err(error) = finalize {
            // A failed final metadata/manifest write must remain pending for retry.
            if trash {
                write_json(&source.join("meta.json"), &m)?;
            } else {
                app.store.update(id, |saved| {
                    saved.archived_rev = m.archived_rev;
                    saved.archived_at = m.archived_at;
                })?;
            }
            return Err(error);
        }
    }
    Ok(report)
}
/// Verify BEFORE a repairing copy, so corruption cannot be hidden by --all.
pub fn verify_memo(app: &App, id: &str) -> Result<ArchiveReport> {
    let _lock = lock_archive(app)?;
    verify_memo_locked(app, id)
}

fn verify_memo_locked(app: &App, id: &str) -> Result<ArchiveReport> {
    let (_, source, dest, _) = locate(app, id)?;
    verify_paths(&source, &dest, true)
}
fn verify_paths(source: &Path, dest: &Path, compare_source: bool) -> Result<ArchiveReport> {
    let lines = fs::read_to_string(dest.join("SHA256SUMS"))?;
    let mut report = ArchiveReport::default();
    let mut listed = BTreeMap::new();
    for line in lines.lines() {
        let (expected, name) = line.split_once("  ").context("invalid SHA256SUMS")?;
        anyhow::ensure!(
            Path::new(name).components().count() == 1
                && matches!(
                    Path::new(name).components().next(),
                    Some(std::path::Component::Normal(_))
                ),
            "invalid manifest path"
        );
        listed.insert(name.to_owned(), expected.to_owned());
        let archived = hash(&dest.join(name));
        let source_path = source.join(name);
        // meta.json changes on every update (status, archive bookkeeping, re-links): the
        // archived copy is checked against its manifest only.
        let source_ok = if !compare_source || name == "meta.json" || !source_path.exists() {
            true
        } else {
            hash(&source_path)? == expected
        };
        if archived.as_ref().is_ok_and(|s| s == expected) && source_ok {
            report.verified_ok += 1;
        } else {
            report.mismatches += 1;
        }
    }
    for path in regular(dest)? {
        if !listed.contains_key(
            path.file_name()
                .unwrap()
                .to_str()
                .context("invalid filename")?,
        ) {
            report.mismatches += 1;
        }
    }
    if compare_source {
        for path in regular(source)? {
            if !listed.contains_key(
                path.file_name()
                    .unwrap()
                    .to_str()
                    .context("invalid filename")?,
            ) {
                report.mismatches += 1;
            }
        }
    }
    Ok(report)
}
pub fn memo_ids(app: &App) -> Result<Vec<String>> {
    let mut ids: Vec<_> = app.store.list().into_iter().map(|m| m.id).collect();
    for entry in fs::read_dir(app.cfg.data_dir.join("trash"))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(m) = read_json::<Memo>(&entry.path().join("meta.json"))? {
                ids.push(m.id);
            }
        }
    }
    ids.sort();
    Ok(ids)
}
pub fn archive_one(app: &App) -> bool {
    if app.cfg.archive_dir.is_none() {
        return false;
    }
    let now = Utc::now();
    let mut memos: Vec<_> = app
        .store
        .list()
        .into_iter()
        .filter(|m| {
            matches!(m.status, Status::Done | Status::Failed) && m.archived_rev != Some(m.revision)
        })
        .collect();
    if let Ok(entries) = fs::read_dir(app.cfg.data_dir.join("trash")) {
        for entry in entries.flatten() {
            if let Ok(Some(m)) = read_json::<Memo>(&entry.path().join("meta.json")) {
                if !app
                    .cfg
                    .archive_dir
                    .as_ref()
                    .unwrap()
                    .join("trash")
                    .join(&m.id)
                    .join("SHA256SUMS")
                    .exists()
                {
                    memos.push(m);
                }
            }
        }
    }
    memos.sort_by_key(|m| m.recorded_at);
    let retries = app.archive_retries.lock().unwrap();
    let next = memos
        .into_iter()
        .find(|m| retries.get(&m.id).is_none_or(|(_, due)| *due <= now));
    drop(retries);
    let Some(m) = next else {
        return false;
    };
    match archive_memo(app, &m.id) {
        Ok(report) => {
            app.archive_retries.lock().unwrap().remove(&m.id);
            tracing::info!(id = %m.id, ?report, "memo archived");
            true
        }
        Err(e) => {
            let mut retries = app.archive_retries.lock().unwrap();
            let retry = retries.entry(m.id.clone()).or_insert((0, now));
            retry.0 += 1;
            retry.1 = now + chrono::Duration::seconds(if retry.0 >= 3 { 3600 } else { 30 });
            tracing::warn!(id = %m.id, error = %e, attempts = retry.0, "memo archive deferred");
            false
        }
    }
}

/// CLI and daily backup use the same copy/verification functions as the worker.
pub fn run_cli(app: &App, all: bool, verify: bool) -> Result<()> {
    check_mount(app)?;
    let mut total = ArchiveReport::default();
    let mut memos = 0;
    let mut errors = 0;
    for id in memo_ids(app)? {
        let (m, _, dest, trash) = locate(app, &id)?;
        if !all
            && !verify
            && !(trash && !dest.join("SHA256SUMS").exists()
                || !trash
                    && matches!(m.status, Status::Done | Status::Failed)
                    && m.archived_rev != Some(m.revision))
        {
            continue;
        }
        if verify && !all && !dest.exists() {
            continue;
        }
        memos += 1;
        let result = (|| -> Result<ArchiveReport> {
            // One lock for the whole check-copy-verify, so the server's worker cannot be
            // half-way through the same memo.
            let _lock = lock_archive(app)?;
            let mut before = ArchiveReport::default();
            if verify && dest.exists() {
                before = verify_paths(Path::new(""), &dest, false)?;
            }
            if verify && !all {
                return verify_memo_locked(app, &id);
            }
            let mut report = archive_files_locked(app, &id, false)?;
            if verify {
                let after = verify_memo_locked(app, &id)?;
                report.verified_ok = after.verified_ok;
                report.mismatches = before.mismatches + after.mismatches;
            }
            Ok(report)
        })();
        match result {
            Ok(r) => {
                println!(
                    "{id}: copied={} unchanged={} bytes={} verified_ok={} mismatches={}",
                    r.copied, r.unchanged, r.bytes, r.verified_ok, r.mismatches
                );
                total.copied += r.copied;
                total.unchanged += r.unchanged;
                total.bytes += r.bytes;
                total.verified_ok += r.verified_ok;
                total.mismatches += r.mismatches;
            }
            Err(e) => {
                errors += 1;
                println!("{id}: ERROR {e:#}");
            }
        }
    }
    println!("memos={memos} files_copied={} files_unchanged={} bytes_copied={} verified_ok={} mismatches={} errors={errors}", total.copied, total.unchanged, total.bytes, total.verified_ok, total.mismatches);
    anyhow::ensure!(
        errors == 0 && total.mismatches == 0,
        "archive verification/copy failed"
    );
    Ok(())
}

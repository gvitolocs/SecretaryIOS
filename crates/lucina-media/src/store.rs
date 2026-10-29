//! Where originals live.
//!
//! - `store`: content-addressed objects on the big disk, `objects/ab/cd/<hash>.<ext>`.
//! - `spool`: the same names under `data_dir/spool` on NVMe, for uploads that arrive in the quiet
//!   hours or while the big disk is not mounted. [`flush_spool`] moves them in the day window.
//! - `external`: files another service owns (voice memos); we only index them.
//!
//! The store is only used when its marker file exists, so an unmounted mybook (an empty
//! mountpoint on the NVMe) is never written to by mistake.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::params;

use crate::config::Config;
use crate::db::{BlobLoc, Db};

pub const STORE_MARKER: &str = ".lucina-media-store";

pub fn object_rel(hash: &str, ext: &str) -> PathBuf {
    PathBuf::from("objects")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(format!("{hash}.{ext}"))
}

pub fn store_available(cfg: &Config) -> bool {
    cfg.store_dir.join(STORE_MARKER).is_file()
}

/// Create the store layout and its marker. Run once by `lucina-media init`.
pub fn init_store(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(cfg.store_dir.join("objects"))?;
    fs::write(
        cfg.store_dir.join(STORE_MARKER),
        b"lucina-media originals\n",
    )?;
    for d in [cfg.spool_dir(), cfg.uploads_dir(), cfg.thumbs_dir()] {
        fs::create_dir_all(d)?;
    }
    Ok(())
}

pub fn path_of(cfg: &Config, b: &BlobLoc) -> PathBuf {
    match b.location.as_str() {
        "store" => cfg.store_dir.join(object_rel(&b.hash, &b.ext)),
        "spool" => cfg.spool_dir().join(object_rel(&b.hash, &b.ext)),
        _ => PathBuf::from(b.path.clone().unwrap_or_default()),
    }
}

/// Copy `src` to `dst` durably: write a temp file next to `dst`, fsync, rename, fsync the dir.
pub fn durable_copy(src: &Path, dst: &Path) -> io::Result<()> {
    let dir = dst.parent().ok_or_else(|| io::Error::other("no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.part",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("obj")
    ));
    {
        let mut from = File::open(src)?;
        let mut to = File::create(&tmp)?;
        io::copy(&mut from, &mut to)?;
        to.sync_all()?;
    }
    fs::rename(&tmp, dst)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Same as [`durable_copy`] but moves when source and destination share a filesystem.
pub fn durable_move(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(dir) = dst.parent() {
        fs::create_dir_all(dir)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => {
            if let Some(dir) = dst.parent() {
                File::open(dir)?.sync_all()?;
            }
            Ok(())
        }
        Err(_) => {
            durable_copy(src, dst)?;
            fs::remove_file(src)
        }
    }
}

/// Put a fully received, hash-verified upload into its final place. Returns the location.
/// The source file is consumed.
pub fn place_new(cfg: &Config, src: &Path, hash: &str, ext: &str) -> io::Result<&'static str> {
    let to_store = store_available(cfg) && !cfg.is_quiet(Utc::now());
    let (root, loc) = if to_store {
        (cfg.store_dir.clone(), "store")
    } else {
        (cfg.spool_dir(), "spool")
    };
    let dst = root.join(object_rel(hash, ext));
    if dst.exists() && fs::metadata(&dst)?.len() == fs::metadata(src)?.len() {
        fs::remove_file(src)?;
    } else {
        durable_move(src, &dst)?;
    }
    Ok(loc)
}

/// Move spooled originals to the big disk. Safe to interrupt: an object is first made durable in
/// the store, then the catalog row flips, then the spool copy goes.
pub fn flush_spool(cfg: &Config, db: &Db) -> anyhow::Result<usize> {
    if !store_available(cfg) || cfg.is_quiet(Utc::now()) {
        return Ok(0);
    }
    let spooled: Vec<(String, String)> = db.with(|c| {
        let mut st = c.prepare("SELECT hash, ext FROM blobs WHERE location = 'spool'")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    })?;
    let mut moved = 0;
    for (hash, ext) in spooled {
        if cfg.is_quiet(Utc::now()) {
            break;
        }
        let rel = object_rel(&hash, &ext);
        let src = cfg.spool_dir().join(&rel);
        let dst = cfg.store_dir.join(&rel);
        let have_dst = dst.is_file()
            && (!src.is_file() || fs::metadata(&dst)?.len() == fs::metadata(&src)?.len());
        if !have_dst {
            if !src.is_file() {
                tracing::warn!(%hash, "spooled object missing from spool and store");
                continue;
            }
            durable_copy(&src, &dst)?;
        }
        db.with(|c| {
            c.execute(
                "UPDATE blobs SET location = 'store' WHERE hash = ?1",
                params![hash],
            )
        })?;
        if src.is_file() {
            fs::remove_file(&src)?;
        }
        moved += 1;
    }
    Ok(moved)
}

/// Blake3 of a file, hex.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(File::open(path)?)?;
    Ok(h.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spooled_objects_move_to_store_when_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
        cfg.quiet_start = 0;
        cfg.quiet_end = 0; // never quiet
        init_store(&cfg).unwrap();
        let db = Db::open(&tmp.path().join("data/catalog.db")).unwrap();
        let src = tmp.path().join("in.jpg");
        fs::write(&src, b"hello").unwrap();
        let hash = hash_file(&src).unwrap();
        // Store unavailable → spool.
        fs::remove_file(cfg.store_dir.join(STORE_MARKER)).unwrap();
        assert_eq!(place_new(&cfg, &src, &hash, "jpg").unwrap(), "spool");
        db.with(|c| {
            c.execute(
                "INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES (?1, 5, 'jpg', 'spool', 'now')",
                [&hash],
            )
        })
        .unwrap();
        assert_eq!(flush_spool(&cfg, &db).unwrap(), 0);
        init_store(&cfg).unwrap();
        assert_eq!(flush_spool(&cfg, &db).unwrap(), 1);
        let rel = object_rel(&hash, "jpg");
        assert_eq!(fs::read(cfg.store_dir.join(&rel)).unwrap(), b"hello");
        assert!(!cfg.spool_dir().join(&rel).exists());
        let loc: String = db
            .with(|c| c.query_row("SELECT location FROM blobs", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(loc, "store");
    }
}

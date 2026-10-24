//! Bulk imports: a folder (e.g. a Nextcloud user's files, read through the data dir) or a
//! Google Takeout export (folder or `.zip`). Sources are copied, never moved or changed.
//!
//! Takeout strips EXIF dates and places from many files and puts them in JSON sidecars
//! (`IMG_1.jpg.json`, `IMG_1.jpg.supplemental-metadata.json`, or a name truncated to 51
//! characters). Those values win over the file's own.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::db;
use crate::ingest::{self, Library};
use crate::meta::ClientMeta;

#[derive(Clone, Debug)]
pub struct ImportOpts {
    /// `items.source`, e.g. `nextcloud`, `takeout`, `import`.
    pub source: String,
    /// Put everything in this album (Takeout album folders add their own).
    pub album: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ImportStats {
    pub seen: usize,
    pub added: usize,
    pub duplicates: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Sidecar {
    title: Option<String>,
    description: Option<String>,
    photo_taken_time: Option<Ts>,
    geo_data: Option<Geo>,
    geo_data_exif: Option<Geo>,
    favorited: Option<bool>,
}

#[derive(Deserialize)]
struct Ts {
    timestamp: String,
}

#[derive(Deserialize)]
struct Geo {
    latitude: f64,
    longitude: f64,
}

fn media_like(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !lower.ends_with(".json") && !lower.starts_with('.') && !lower.ends_with(".html") && lower != "thumbs.db"
}

fn sidecar_meta(sc: &Sidecar, client: &mut ClientMeta) {
    if let Some(t) = sc.photo_taken_time.as_ref().and_then(|t| t.timestamp.parse::<i64>().ok()) {
        if t > 0 {
            client.taken_at = chrono::DateTime::from_timestamp(t, 0)
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
    }
    for g in [&sc.geo_data, &sc.geo_data_exif].into_iter().flatten() {
        if g.latitude != 0.0 || g.longitude != 0.0 {
            client.lat = Some(g.latitude);
            client.lon = Some(g.longitude);
            break;
        }
    }
    client.caption = sc.description.clone().filter(|d| !d.trim().is_empty());
    if sc.favorited == Some(true) {
        client.favorite = Some(true);
    }
    if let Some(t) = &sc.title {
        client.filename = Some(t.clone());
    }
}

/// Candidate sidecar names for a media file, most specific first.
pub fn sidecar_names(file: &str) -> Vec<String> {
    let mut v = vec![format!("{file}.supplemental-metadata.json"), format!("{file}.json")];
    // Takeout truncates "<name>.<suffix>.json" to 51 characters before ".json".
    for suffix in [".supplemental-metadata", ""] {
        let full = format!("{file}{suffix}");
        if full.len() > 46 {
            let cut: String = full.chars().take(46).collect();
            v.push(format!("{cut}.json"));
        }
    }
    // "IMG_1(1).jpg" → "IMG_1.jpg(1).json"
    if let Some((stem, rest)) = file.split_once('(') {
        if let Some((n, ext)) = rest.split_once(')') {
            v.push(format!("{stem}{ext}({n}).json"));
        }
    }
    v
}

/// Album name implied by a Takeout folder ("Photos from 2019" is a date bucket, not an album).
fn takeout_album(dir_name: &str) -> Option<String> {
    let d = dir_name.trim();
    if d.is_empty() || d.starts_with("Photos from ") || d == "Google Photos" || d == "Takeout" {
        None
    } else {
        Some(d.to_string())
    }
}

pub fn import_path(lib: &Library, root: &Path, opts: &ImportOpts) -> anyhow::Result<ImportStats> {
    let mut stats = ImportStats::default();
    if root.is_file() && root.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        import_zip(lib, root, opts, &mut stats)?;
        return Ok(stats);
    }
    let takeout = opts.source == "takeout";
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.to_ascii_lowercase().ends_with(".zip") {
            import_zip(lib, path, opts, &mut stats)?;
            continue;
        }
        if !media_like(&name) {
            continue;
        }
        stats.seen += 1;
        let mut client = ClientMeta { filename: Some(name.clone()), source: Some(opts.source.clone()), ..Default::default() };
        client.album = opts.album.clone();
        if takeout {
            let dir = path.parent().unwrap_or(root);
            if let Some(sc) = sidecar_names(&name).into_iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
                if let Ok(sc) = serde_json::from_slice::<Sidecar>(&fs::read(sc)?) {
                    sidecar_meta(&sc, &mut client);
                    client.filename = Some(name.clone());
                }
            }
            if client.album.is_none() {
                client.album = dir.file_name().and_then(|d| takeout_album(&d.to_string_lossy()));
            }
        }
        match copy_in(lib, path).and_then(|tmp| ingest::ingest_file(lib, &tmp, None, &client, None)) {
            Ok(r) if r.duplicate => {
                stats.duplicates += 1;
                if let Some(a) = &client.album {
                    add_dup_to_album(lib, a, &r.item_id)?;
                }
            }
            Ok(_) => stats.added += 1,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "import failed");
                stats.failed += 1;
            }
        }
        if stats.seen % 200 == 0 {
            tracing::info!(?stats, "import progress");
        }
    }
    Ok(stats)
}

fn add_dup_to_album(lib: &Library, album: &str, item_id: &str) -> anyhow::Result<()> {
    lib.db.with(|c| {
        let aid = ingest::find_or_create_album(c, album)?;
        if db::add_to_album(c, &aid, item_id)? {
            db::reindex(c, item_id)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Copy a source file into the uploads area (same disk as the spool) for ingest to consume.
fn copy_in(lib: &Library, src: &Path) -> anyhow::Result<PathBuf> {
    let tmp = lib.cfg.uploads_dir().join(db::new_id("imp"));
    fs::copy(src, &tmp)?;
    Ok(tmp)
}

fn import_zip(lib: &Library, zip_path: &Path, opts: &ImportOpts, stats: &mut ImportStats) -> anyhow::Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file))?;
    // Pass 1: the sidecars (small) by full entry name.
    let mut sidecars: HashMap<String, Sidecar> = HashMap::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        if name.ends_with(".json") && e.size() < 1 << 20 {
            let mut buf = Vec::new();
            io::copy(&mut e, &mut buf)?;
            if let Ok(sc) = serde_json::from_slice::<Sidecar>(&buf) {
                sidecars.insert(name, sc);
            }
        }
    }
    // Pass 2: media entries.
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let Some(entry_path) = e.enclosed_name() else {
            stats.skipped += 1;
            continue;
        };
        let name = entry_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !media_like(&name) {
            continue;
        }
        stats.seen += 1;
        let dir = entry_path.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let mut client = ClientMeta { filename: Some(name.clone()), source: Some(opts.source.clone()), ..Default::default() };
        client.album = opts.album.clone();
        for cand in sidecar_names(&name) {
            let key = if dir.is_empty() { cand } else { format!("{dir}/{cand}") };
            if let Some(sc) = sidecars.get(&key) {
                sidecar_meta(sc, &mut client);
                client.filename = Some(name.clone());
                break;
            }
        }
        if client.album.is_none() {
            client.album = entry_path.parent().and_then(|p| p.file_name()).and_then(|d| takeout_album(&d.to_string_lossy()));
        }
        let tmp = lib.cfg.uploads_dir().join(db::new_id("imp"));
        let res = (|| -> anyhow::Result<ingest::IngestResult> {
            let mut out = fs::File::create(&tmp)?;
            io::copy(&mut e, &mut out)?;
            ingest::ingest_file(lib, &tmp, None, &client, None)
        })();
        match res {
            Ok(r) if r.duplicate => {
                stats.duplicates += 1;
                if let Some(a) = &client.album {
                    add_dup_to_album(lib, a, &r.item_id)?;
                }
            }
            Ok(_) => stats.added += 1,
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                tracing::warn!(entry = %entry_path.display(), error = %err, "zip import failed");
                stats.failed += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_name_variants() {
        let v = sidecar_names("IMG_1234.JPG");
        assert_eq!(v[0], "IMG_1234.JPG.supplemental-metadata.json");
        assert_eq!(v[1], "IMG_1234.JPG.json");
        let v = sidecar_names("IMG_1(1).jpg");
        assert!(v.contains(&"IMG_1.jpg(1).json".to_string()));
        let long = "PXL_20230612_183015123.NIGHT.RAW-01.MP.COVER.jpg";
        let v = sidecar_names(long);
        assert!(v.iter().any(|n| n.len() == 51));
        assert_eq!(takeout_album("Photos from 2019"), None);
        assert_eq!(takeout_album("Copenhagen trip"), Some("Copenhagen trip".into()));
    }
}

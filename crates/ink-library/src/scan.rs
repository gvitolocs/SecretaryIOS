//! Read-only bounded Kindle walk. Symlinks are never followed; sidecars are names only.
use crate::{clippings, import::*, model::*, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
pub const MAX_DEPTH: usize = 8;
pub const MAX_FILES: usize = 50_000;
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    pub device: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindleScan {
    pub batch: ImportBatch,
    pub indicators: BTreeMap<String, bool>,
    pub markers: Vec<String>,
    pub version: String,
    pub warnings: Vec<String>,
}
fn safe(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part);
        if !fs::symlink_metadata(&current).is_ok_and(|m| !m.file_type().is_symlink()) {
            return false;
        }
    }
    path.canonicalize().is_ok_and(|p| p.starts_with(root))
}
fn ignored(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy();
        name.starts_with("._")
            || [".DS_Store", ".Spotlight-V100", ".fseventsd", ".Trashes"].contains(&name.as_ref())
    })
}
/// Kindle file-name substitutions are only used when no header title is known.
/// Store ASINs separately; personal document ids are never treated as ASINs.
pub fn filename_metadata(stem: &str) -> Metadata {
    let mut title = stem;
    let mut asin = None;
    let mut personal = false;
    if let Some((prefix, suffix)) = stem.rsplit_once('_') {
        let id = suffix.to_ascii_uppercase();
        let isbn10 = id.len() == 10
            && id.as_bytes()[..9].iter().all(u8::is_ascii_digit)
            && id
                .as_bytes()
                .last()
                .is_some_and(|c| c.is_ascii_digit() || *c == b'X');
        let amazon =
            id.len() == 10 && id.starts_with("B0") && id.bytes().all(|b| b.is_ascii_alphanumeric());
        personal = id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b));
        if amazon || isbn10 {
            title = prefix;
            asin = Some(id);
        } else if personal {
            title = prefix;
        }
    }
    Metadata {
        title: Some(title.replace("_ ", ": ")),
        asin,
        cdetype: personal.then(|| "PDOC".into()),
        ..Metadata::default()
    }
}
fn homebrew(root: &Path, scan: &mut KindleScan) {
    let groups: &[(&str, &[&str])] = &[
        (
            "jailbreak",
            &[
                "winterbreak.log",
                "jb.sh",
                "patchedUks.sqsh",
                "mkk",
                "MKK",
                "developer.keystore",
                "system/developer.keystore",
                "jailbreak",
            ],
        ),
        (
            "kual",
            &["documents/KUAL.kual", "extensions/KUAL.cfg", "kual", "KUAL"],
        ),
        ("mrpi", &["extensions/MRInstaller", "mrpackages"]),
        ("otaBlocker", &["extensions/renameotabin"]),
        ("koreader", &["koreader"]),
    ];
    for (group, paths) in groups {
        let mut found = false;
        for path in *paths {
            if !ignored(Path::new(path)) && safe(&root.join(path), root) {
                scan.markers.push((*path).into());
                found = true;
            }
        }
        scan.indicators.insert((*group).into(), found);
    }
    for path in ["libkh", "system", ".active_content_sandbox", "extensions"] {
        let present = safe(&root.join(path), root);
        if present && path == "libkh" {
            scan.markers.push(path.into());
        }
        scan.indicators.insert(path.into(), present);
    }
    scan.markers.sort();
    scan.markers.dedup();
}
fn format(path: &Path) -> Option<Format> {
    Some(match path.extension()?.to_str()?.to_lowercase().as_str() {
        "azw3" => Format::Azw3,
        "azw1" => Format::Azw1,
        "tpz" => Format::Tpz,
        "azw" => Format::Azw,
        "mobi" => Format::Mobi,
        "kfx" => Format::Kfx,
        "pdf" => Format::Pdf,
        "txt" => Format::Txt,
        "md" => Format::Md,
        "prc" => Format::Prc,
        "pobi" => Format::Pobi,
        "epub" => Format::Epub,
        _ => return None,
    })
}
fn prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(path)?.take(limit as u64).read_to_end(&mut b)?;
    Ok(b)
}
fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
    count: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if !safe(dir, root) {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        warnings.push("depth limit reached; scan incomplete".into());
        return Err(Error::Invalid(
            "scan depth exceeds 8; refusing partial inventory".into(),
        ));
    }
    let mut entries = Vec::new();
    for e in fs::read_dir(dir)? {
        if entries.len() + *count >= MAX_FILES {
            return Err(Error::Invalid("scan exceeds 50k entries".into()));
        }
        entries.push(e?);
    }
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if ignored(&p) || !safe(&p, root) {
            continue;
        }
        *count += 1;
        if *count > MAX_FILES {
            return Err(Error::Invalid("scan exceeds 50k entries".into()));
        }
        let t = e.file_type()?;
        if t.is_dir() {
            if p.extension().is_some_and(|s| s == "sdr") {
                continue;
            }
            walk(root, &p, depth + 1, files, count, warnings)?;
        } else if t.is_file() {
            files.push(p);
        }
    }
    Ok(())
}
pub fn scan_kindle(root: &Path, opts: ScanOptions) -> Result<KindleScan> {
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(Error::Invalid("mount must not be a symlink".into()));
    }
    let root = root.canonicalize()?;
    let docs = root.join("documents");
    if !safe(&docs, &root) {
        return Err(Error::Invalid("no safe documents directory".into()));
    }
    let mut scan = KindleScan {
        batch: ImportBatch {
            source: "kindle-usb".into(),
            device: opts.device,
            scanned_at: chrono::Utc::now().to_rfc3339(),
            books: vec![],
            clippings: vec![],
        },
        indicators: BTreeMap::new(),
        markers: vec![],
        version: "unknown".into(),
        warnings: vec![],
    };
    homebrew(&root, &mut scan);
    let version = root.join("system/version.txt");
    if safe(&root.join("system"), &root) && safe(&version, &root) && version.is_file() {
        scan.version = String::from_utf8_lossy(&prefix(&version, 4096)?)
            .trim()
            .into();
    }
    let mut files = Vec::new();
    let mut count = 0;
    walk(&root, &docs, 0, &mut files, &mut count, &mut scan.warnings)?;
    let sdr = fs::read_dir(&docs)?.filter_map(|e| e.ok()).any(|e| {
        !ignored(&e.path())
            && safe(&e.path(), &root)
            && e.path().extension().is_some_and(|s| s == "sdr")
            && e.path().is_dir()
    });
    scan.indicators.insert("documents/*.sdr".into(), sdr);
    for p in files {
        if p.file_name().is_some_and(|n| n == "My Clippings.txt") {
            let b = prefix(&p, clippings::MAX_BYTES + 1)?;
            let parsed = clippings::parse(&b)?;
            scan.batch.clippings.extend(parsed.entries);
            scan.warnings.extend(parsed.warnings);
            continue;
        }
        let Some(fmt) = format(&p) else {
            continue;
        };
        let mut f = File::open(&p)?;
        let size = f.metadata()?.len();
        let mut hash = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        let parsed = match fmt {
            Format::Mobi | Format::Azw | Format::Azw3 | Format::Prc | Format::Pobi => {
                crate::mobi::read(File::open(&p)?)
            }
            Format::Epub => crate::epub::read(File::open(&p)?),
            Format::Pdf => Ok(crate::pdf::parse(&prefix(&p, MAX_METADATA_BYTES)?)),
            Format::Txt | Format::Md => {
                let b = prefix(&p, MAX_METADATA_BYTES)?;
                let t = String::from_utf8_lossy(&b);
                let title = t.lines().find_map(|l| {
                    l.strip_prefix('#')
                        .map(|l| l.trim_start_matches('#').trim().to_owned())
                        .filter(|l| !l.is_empty())
                });
                Ok(Metadata {
                    title,
                    drm: DrmState::None,
                    ..Metadata::default()
                })
            }
            Format::Kfx | Format::Azw1 | Format::Tpz => Ok(Metadata::default()),
        };
        let mut metadata = match parsed {
            Ok(m) => m,
            Err(e) => {
                scan.warnings.push(format!(
                    "{}: {e}",
                    p.strip_prefix(&root).unwrap_or(&p).display()
                ));
                Metadata::default()
            }
        };
        let filename = filename_metadata(&p.file_stem().unwrap_or_default().to_string_lossy());
        if metadata.title.as_ref().is_none_or(|t| t.is_empty()) {
            metadata.title = filename.title;
        }
        if metadata.asin.is_none() {
            metadata.asin = filename.asin;
        }
        if metadata.cdetype.is_none() {
            metadata.cdetype = filename.cdetype;
        }
        let dictionary_path = p.strip_prefix(&docs).is_ok_and(|relative| {
            relative.components().next().is_some_and(|c| {
                c.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("dictionaries")
            })
        });
        let kind = if metadata.dictionary || dictionary_path {
            BookKind::Dictionary
        } else if metadata.cdetype.as_deref() == Some("PDOC")
            || matches!(fmt, Format::Txt | Format::Md | Format::Pdf)
        {
            BookKind::Document
        } else {
            BookKind::Book
        };
        let mut extensions = Vec::new();
        let side = p.with_extension("sdr");
        let sidecar_present = safe(&side, &root) && side.is_dir();
        if sidecar_present {
            for e in fs::read_dir(side)? {
                count += 1;
                if count > MAX_FILES {
                    return Err(Error::Invalid("too many sidecars".into()));
                }
                let e = e?;
                if !ignored(&e.path()) && safe(&e.path(), &root) && e.file_type()?.is_file() {
                    if let Some(ext) = e.path().extension() {
                        let ext = ext.to_string_lossy().to_lowercase();
                        if !extensions.contains(&ext) {
                            extensions.push(ext);
                        }
                    }
                }
            }
        }
        extensions.sort();
        scan.batch.books.push(ScannedBook {
            kind,
            path: p
                .strip_prefix(&root)
                .map_err(|_| Error::Invalid("path outside mount".into()))?
                .to_string_lossy()
                .into(),
            size_bytes: size,
            fingerprint: format!("{:x}", hash.finalize()),
            format: fmt,
            metadata,
            sidecar_present,
            sidecar_extensions: extensions,
        });
    }
    Ok(scan)
}

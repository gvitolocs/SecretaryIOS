//! Archive listings: zip (and zip-based jar/apk/epub…), tar, tar.gz and
//! single-file gzip. Only the central directory or the entry headers are
//! read; nothing is extracted to disk or executed.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::rc::Rc;

use anyhow::{bail, Context};
use flate2::read::GzDecoder;
use serde_json::{json, Value};

use super::Extracted;

/// Entries read before the listing stops.
const MAX_ENTRIES: usize = 2000;
/// Entries kept in `structured.entries`.
const JSON_ENTRIES: usize = 500;
/// Declared uncompressed totals above this are flagged as a possible bomb.
const BOMB_BYTES: u64 = 50 * 1024 * 1024 * 1024;
/// Decompressed bytes a tar.gz listing may stream through to reach headers.
const MAX_GZ_STREAM: u64 = 4 * 1024 * 1024 * 1024;
const MAX_PATH_CHARS: usize = 300;
const TOP_LEVEL: usize = 100;
const LARGEST: usize = 50;

/// Zip-based formats listed as zip.
const ZIP_EXTS: &[&str] = &[
    "zip", "jar", "war", "ear", "aar", "apk", "aab", "ipa", "epub", "xpi", "crx", "whl", "nupkg",
    "vsix", "cbz", "kmz", "odt", "ods", "odp", "docx", "xlsx", "pptx",
];

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    size: u64,
    compressed: Option<u64>,
    dir: bool,
}

#[derive(Default)]
struct Listing {
    entries: Vec<Entry>,
    /// Total entries in the archive when the format says (zip).
    declared_count: Option<usize>,
    /// Declared uncompressed size of every entry, listed or not (zip).
    declared_total: Option<u64>,
    truncated: bool,
    warnings: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    Zip,
    Tar,
    TarGz,
    Gz,
}

/// Lists an archive's entries without extracting it.
pub fn list_archive(path: &Path, ext: &str) -> anyhow::Result<Extracted> {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let format = detect(path, &ext, &file_name)?;
    let mut listing = match format {
        Format::Zip => list_zip(path)?,
        Format::Tar => {
            let file = BufReader::new(File::open(path).context("cannot open the archive")?);
            list_tar_seek(file)
        }
        Format::TarGz => {
            let file = BufReader::new(File::open(path).context("cannot open the archive")?);
            let hit = Rc::new(Cell::new(false));
            let reader = Capped {
                inner: GzDecoder::new(file),
                left: MAX_GZ_STREAM,
                hit: hit.clone(),
            };
            let mut l = list_tar_stream(reader);
            if hit.get() {
                l.warnings.push(format!(
                    "Stopped after streaming {} GB of decompressed data; the listing is partial.",
                    MAX_GZ_STREAM / (1024 * 1024 * 1024)
                ));
                l.truncated = true;
            }
            l
        }
        Format::Gz => list_gz(path)?,
    };

    let read_total: u64 = listing
        .entries
        .iter()
        .fold(0u64, |a, e| a.saturating_add(e.size));
    let bomb_total = listing.declared_total.unwrap_or(read_total).max(read_total);
    if let Some(w) = bomb_warning(bomb_total) {
        listing.warnings.push(w);
    }
    if listing.truncated && listing.entries.len() >= MAX_ENTRIES {
        let of = listing
            .declared_count
            .map(|n| format!(" of {n}"))
            .unwrap_or_default();
        listing.warnings.insert(
            0,
            format!("Only the first {MAX_ENTRIES}{of} entries were read."),
        );
    }

    let subtype = match format {
        Format::Zip if ZIP_EXTS.contains(&ext.as_str()) => ext.clone(),
        Format::Zip => "zip".to_string(),
        Format::Tar => "tar".to_string(),
        Format::TarGz => "tar.gz".to_string(),
        Format::Gz => "gz".to_string(),
    };
    let mut out = Extracted::new("archive");
    out.title = (!file_name.is_empty()).then(|| file_name.clone());
    out.text = render_text(&subtype, &listing, read_total);
    out.structured = render_json(&subtype, &listing, read_total);
    out.subtype = Some(subtype);
    out.warnings = listing.warnings;
    Ok(out)
}

fn bomb_warning(declared_total: u64) -> Option<String> {
    (declared_total > BOMB_BYTES).then(|| {
        format!(
            "The entries declare {} uncompressed: possible zip bomb. Do not extract it.",
            human(declared_total)
        )
    })
}

/// Format by magic bytes first, extension second.
fn detect(path: &Path, ext: &str, file_name: &str) -> anyhow::Result<Format> {
    let mut file = File::open(path).context("cannot open the archive")?;
    let mut head = [0u8; 512];
    let n = read_up_to(&mut file, &mut head)?;
    let head = &head[..n];
    let lower = file_name.to_ascii_lowercase();
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return Ok(Format::Zip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        file.seek(SeekFrom::Start(0))?;
        let mut inner = [0u8; 512];
        let m = read_up_to(&mut GzDecoder::new(BufReader::new(file)), &mut inner).unwrap_or(0);
        let tar_inside = (m >= 262 && &inner[257..262] == b"ustar")
            || matches!(ext, "tgz" | "tar.gz")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tgz");
        return Ok(if tar_inside {
            Format::TarGz
        } else {
            Format::Gz
        });
    }
    if n >= 262 && &head[257..262] == b"ustar" {
        return Ok(Format::Tar);
    }
    Ok(match ext {
        "tar" => Format::Tar,
        "tgz" | "tar.gz" => Format::TarGz,
        "gz" | "gzip" => bail!("the file is not gzip data"),
        e if ZIP_EXTS.contains(&e) => Format::Zip,
        _ => bail!("unknown archive format"),
    })
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn list_zip(path: &Path) -> anyhow::Result<Listing> {
    let file = BufReader::new(File::open(path).context("cannot open the archive")?);
    let mut zip = zip::ZipArchive::new(file).context("cannot read the zip directory")?;
    let count = zip.len();
    let mut l = Listing {
        declared_count: Some(count),
        ..Listing::default()
    };
    let mut total = 0u64;
    let mut unreadable = 0usize;
    for i in 0..count {
        let f = match zip.by_index_raw(i) {
            Ok(f) => f,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        total = total.saturating_add(f.size());
        if l.entries.len() < MAX_ENTRIES {
            l.entries.push(Entry {
                name: f.name().to_string(),
                size: f.size(),
                compressed: Some(f.compressed_size()),
                dir: f.is_dir(),
            });
        } else {
            l.truncated = true;
        }
    }
    if unreadable > 0 {
        l.warnings
            .push(format!("{unreadable} entries could not be read."));
    }
    l.declared_total = Some(total);
    Ok(l)
}

fn list_tar_seek<R: Read + Seek>(reader: R) -> Listing {
    let mut archive = tar::Archive::new(reader);
    match archive.entries_with_seek() {
        Ok(entries) => collect_tar(entries),
        Err(e) => failed(e),
    }
}

fn list_tar_stream<R: Read>(reader: R) -> Listing {
    let mut archive = tar::Archive::new(reader);
    match archive.entries() {
        Ok(entries) => collect_tar(entries),
        Err(e) => failed(e),
    }
}

fn failed(e: io::Error) -> Listing {
    Listing {
        warnings: vec![format!("The tar headers could not be read: {e}")],
        ..Listing::default()
    }
}

fn collect_tar<'a, R: 'a + Read>(
    entries: impl Iterator<Item = io::Result<tar::Entry<'a, R>>>,
) -> Listing {
    let mut l = Listing::default();
    for entry in entries {
        if l.entries.len() >= MAX_ENTRIES {
            l.truncated = true;
            break;
        }
        match entry {
            Ok(e) => {
                let kind = e.header().entry_type();
                if !(kind.is_file()
                    || kind.is_dir()
                    || kind.is_symlink()
                    || kind.is_hard_link()
                    || kind.is_gnu_sparse())
                {
                    continue;
                }
                l.entries.push(Entry {
                    name: String::from_utf8_lossy(&e.path_bytes()).into_owned(),
                    size: if kind.is_dir() { 0 } else { e.size() },
                    compressed: None,
                    dir: kind.is_dir(),
                });
            }
            Err(e) => {
                if l.entries.is_empty() {
                    l.warnings
                        .push(format!("The tar headers could not be read: {e}"));
                } else {
                    l.warnings.push(format!("The listing stopped early: {e}"));
                }
                l.truncated = true;
                break;
            }
        }
    }
    l
}

/// Single-file gzip: the stored name from the header and the size from the
/// trailer (modulo 4 GiB, as gzip stores it).
fn list_gz(path: &Path) -> anyhow::Result<Listing> {
    let mut file = File::open(path).context("cannot open the file")?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut head = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut file, &mut head)?;
    head.truncate(n);
    let name = gzip_name(&head);
    let mut l = Listing::default();
    let size = if len >= 18 {
        let mut tail = [0u8; 4];
        file.seek(SeekFrom::End(-4))?;
        file.read_exact(&mut tail)?;
        Some(u64::from(u32::from_le_bytes(tail)))
    } else {
        None
    };
    let inner = name.unwrap_or_else(|| {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        l.warnings
            .push("The gzip header has no stored name; the file name was used.".into());
        stem
    });
    if size.is_none() {
        l.warnings
            .push("The gzip trailer is missing; the size is unknown.".into());
    } else if len > u64::from(u32::MAX) {
        l.warnings
            .push("The stored size wraps at 4 GB; the real size may be larger.".into());
    }
    l.entries.push(Entry {
        name: inner,
        size: size.unwrap_or(0),
        compressed: Some(len),
        dir: false,
    });
    Ok(l)
}

/// The FNAME field of a gzip header, if present.
fn gzip_name(h: &[u8]) -> Option<String> {
    if h.len() < 10 || h[0] != 0x1f || h[1] != 0x8b {
        return None;
    }
    let flags = h[3];
    let mut i = 10;
    if flags & 0x04 != 0 {
        let xlen = usize::from(u16::from_le_bytes([*h.get(i)?, *h.get(i + 1)?]));
        i += 2 + xlen;
    }
    if flags & 0x08 == 0 {
        return None;
    }
    let rest = h.get(i..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    // RFC 1952 says ISO-8859-1.
    let name: String = rest[..end].iter().map(|&b| b as char).collect();
    (!name.is_empty()).then_some(name)
}

/// Caps how many bytes a reader yields, flagging when the cap is hit.
struct Capped<R> {
    inner: R,
    left: u64,
    hit: Rc<Cell<bool>>,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            self.hit.set(true);
            return Err(io::Error::other("decompressed size limit reached"));
        }
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// A path safe to show: control characters replaced, at most 300 chars.
fn display_path(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if clean.chars().count() > MAX_PATH_CHARS {
        let mut s: String = clean.chars().take(MAX_PATH_CHARS - 1).collect();
        s.push('…');
        s
    } else {
        clean
    }
}

/// Path without leading `./` or `/`.
fn relative(name: &str) -> &str {
    let mut n = name;
    loop {
        if let Some(r) = n.strip_prefix("./") {
            n = r;
        } else if let Some(r) = n.strip_prefix('/') {
            n = r;
        } else {
            return n;
        }
    }
}

fn top_level(entries: &[Entry]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        let rel = relative(&e.name);
        if rel.is_empty() || rel == "." {
            continue;
        }
        let (first, nested) = match rel.split_once('/') {
            Some((f, rest)) => (f, !rest.is_empty() || e.dir),
            None => (rel, e.dir),
        };
        let shown = if nested {
            format!("{first}/")
        } else {
            first.to_string()
        };
        // A folder seen as a file entry first ("a" then "a/b") keeps one name.
        if seen.contains(first) {
            if nested {
                if let Some(slot) = out.iter_mut().find(|s: &&mut String| s.as_str() == first) {
                    *slot = shown;
                }
            }
            continue;
        }
        seen.insert(first.to_string());
        if out.len() < TOP_LEVEL {
            out.push(display_path(&shown));
        }
    }
    out
}

fn types(entries: &[Entry]) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for e in entries.iter().filter(|e| !e.dir) {
        let base = e.name.rsplit('/').next().unwrap_or(&e.name);
        let ext = match base.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 10 => {
                ext.to_ascii_lowercase()
            }
            _ => "(none)".to_string(),
        };
        *map.entry(ext).or_insert(0) += 1;
    }
    map
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn render_json(subtype: &str, l: &Listing, total: u64) -> Value {
    let entries: Vec<Value> = l
        .entries
        .iter()
        .take(JSON_ENTRIES)
        .map(|e| {
            let mut v = json!({ "name": display_path(&e.name), "size": e.size, "dir": e.dir });
            if let Some(c) = e.compressed {
                v["compressed"] = json!(c);
            }
            v
        })
        .collect();
    let mut v = json!({
        "format": subtype,
        "entries": entries,
        "entry_count": l.entries.len(),
        "total_size": total,
        "top_level": top_level(&l.entries),
        "types": types(&l.entries),
        "truncated": l.truncated,
    });
    if let Some(n) = l.declared_count {
        v["declared_entry_count"] = json!(n);
    }
    if let Some(t) = l.declared_total {
        v["declared_total_size"] = json!(t);
    }
    v
}

fn render_text(subtype: &str, l: &Listing, total: u64) -> String {
    let files = l.entries.iter().filter(|e| !e.dir).count();
    let dirs = l.entries.len() - files;
    let count = match (l.declared_count, l.truncated) {
        (Some(n), true) => format!("{n} entries (first {} read)", l.entries.len()),
        (None, true) => format!("at least {} entries", l.entries.len()),
        _ => format!("{} entries", l.entries.len()),
    };
    let mut out = format!(
        "{} archive: {count}, {files} files, {dirs} folders, {} uncompressed",
        subtype.to_ascii_uppercase(),
        human(total)
    );
    let packed: Option<u64> = l
        .entries
        .iter()
        .map(|e| e.compressed)
        .try_fold(0u64, |a, c| c.map(|c| a.saturating_add(c)));
    if let Some(p) = packed.filter(|_| !l.entries.is_empty()) {
        out.push_str(&format!(" ({} compressed)", human(p)));
    }
    out.push('\n');
    let top = top_level(&l.entries);
    if !top.is_empty() {
        out.push_str(&format!("Top level: {}\n", top.join(", ")));
    }
    let ty = types(&l.entries);
    if !ty.is_empty() {
        let mut pairs: Vec<(&String, &usize)> = ty.iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let shown: Vec<String> = pairs
            .iter()
            .take(15)
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        out.push_str(&format!("Types: {}\n", shown.join(", ")));
    }
    let mut largest: Vec<&Entry> = l.entries.iter().filter(|e| !e.dir).collect();
    largest.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
    if !largest.is_empty() {
        out.push_str("Largest files:\n");
        for e in largest.iter().take(LARGEST) {
            out.push_str(&format!(
                "{:>9}  {}\n",
                human(e.size),
                display_path(&e.name)
            ));
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in files {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(body).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    #[test]
    fn zip_listing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("photos.zip");
        let big = vec![b'a'; 10_000];
        make_zip(
            &p,
            &[
                ("trip/", b""),
                ("trip/a.jpg", &big),
                ("trip/b.JPG", b"xx"),
                ("trip/notes.txt", b"hello"),
                ("readme.md", b"# hi"),
            ],
        );
        let before = dir_names(dir.path());
        let out = list_archive(&p, "zip").unwrap();
        assert_eq!(
            dir_names(dir.path()),
            before,
            "nothing written next to the archive"
        );
        assert_eq!(out.kind, "archive");
        assert_eq!(out.subtype.as_deref(), Some("zip"));
        assert_eq!(out.title.as_deref(), Some("photos.zip"));
        let s = &out.structured;
        assert_eq!(s["entry_count"], 5);
        assert_eq!(s["total_size"], 10_000 + 2 + 5 + 4);
        assert_eq!(s["top_level"], json!(["trip/", "readme.md"]));
        assert_eq!(s["types"], json!({"jpg": 2, "txt": 1, "md": 1}));
        assert_eq!(
            s["entries"][0],
            json!({"name": "trip/", "size": 0, "dir": true, "compressed": 0})
        );
        assert!(s["entries"][1]["compressed"].as_u64().unwrap() < 10_000);
        assert_eq!(s["truncated"], false);
        assert!(out
            .text
            .starts_with("ZIP archive: 5 entries, 4 files, 1 folders, 9.8 KB uncompressed"));
        assert!(out.text.contains("Top level: trip/, readme.md"));
        let largest = out.text.split("Largest files:\n").nth(1).unwrap();
        assert!(largest.lines().next().unwrap().ends_with("trip/a.jpg"));
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn zip_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.zip");
        let names: Vec<String> = (0..2050).map(|i| format!("f{i:04}.txt")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        make_zip(&p, &files);
        let out = list_archive(&p, "zip").unwrap();
        let s = &out.structured;
        assert_eq!(s["entry_count"], 2000);
        assert_eq!(s["declared_entry_count"], 2050);
        assert_eq!(s["entries"].as_array().unwrap().len(), 500);
        assert_eq!(s["truncated"], true);
        assert_eq!(s["total_size"], 2000);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("first 2000 of 2050")),
            "{:?}",
            out.warnings
        );
        assert!(out.text.contains("2050 entries (first 2000 read)"));
        let largest = out.text.split("Largest files:\n").nth(1).unwrap();
        assert_eq!(largest.lines().count(), 50);
    }

    #[test]
    fn jar_is_listed_as_zip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.jar");
        make_zip(
            &p,
            &[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
                ("a/B.class", b"\xca\xfe"),
            ],
        );
        let out = list_archive(&p, ".JAR").unwrap();
        assert_eq!(out.subtype.as_deref(), Some("jar"));
        assert_eq!(out.structured["top_level"], json!(["META-INF/", "a/"]));
        assert_eq!(out.structured["types"]["class"], 1);
    }

    fn tar_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, body) in files {
            let mut h = tar::Header::new_gnu();
            if name.ends_with('/') {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_size(0);
                h.set_mode(0o755);
                h.set_cksum();
                b.append_data(&mut h, name, io::empty()).unwrap();
            } else {
                h.set_size(body.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, *body).unwrap();
            }
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn tar_gz_listing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("backup.tar.gz");
        let long_name = format!("project/{}/deep.rs", "d".repeat(400));
        let data = tar_bytes(&[
            ("project/", b""),
            ("project/src/main.rs", b"fn main() {}\n"),
            ("project/Cargo.toml", b"[package]\n"),
            (&long_name, b"x"),
        ]);
        let mut gz = flate2::write::GzEncoder::new(
            File::create(&p).unwrap(),
            flate2::Compression::default(),
        );
        gz.write_all(&data).unwrap();
        gz.finish().unwrap();
        let before = dir_names(dir.path());
        for ext in ["tgz", "gz", "tar.gz"] {
            let out = list_archive(&p, ext).unwrap();
            assert_eq!(out.subtype.as_deref(), Some("tar.gz"));
            let s = &out.structured;
            assert_eq!(s["entry_count"], 4);
            assert_eq!(s["total_size"], 13 + 10 + 1);
            assert_eq!(s["top_level"], json!(["project/"]));
            assert_eq!(s["types"], json!({"rs": 2, "toml": 1}));
            let shown = s["entries"][3]["name"].as_str().unwrap();
            assert_eq!(shown.chars().count(), 300);
            assert!(shown.ends_with('…'));
            assert!(s["entries"][1].get("compressed").is_none());
            assert!(out
                .text
                .starts_with("TAR.GZ archive: 4 entries, 3 files, 1 folders, 24 B"));
        }
        assert_eq!(
            dir_names(dir.path()),
            before,
            "nothing written next to the archive"
        );
    }

    #[test]
    fn plain_tar_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.tar");
        let names: Vec<String> = (0..2010).map(|i| format!("f{i}")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"ab"[..])).collect();
        std::fs::write(&p, tar_bytes(&files)).unwrap();
        let out = list_archive(&p, "tar").unwrap();
        assert_eq!(out.subtype.as_deref(), Some("tar"));
        assert_eq!(out.structured["entry_count"], 2000);
        assert_eq!(out.structured["truncated"], true);
        assert!(out.text.contains("at least 2000 entries"));
        assert!(out
            .warnings
            .iter()
            .any(|w| w.contains("first 2000 entries")));
    }

    #[test]
    fn single_gz_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log.gz");
        let mut gz = flate2::GzBuilder::new()
            .filename("server.log")
            .write(File::create(&p).unwrap(), flate2::Compression::default());
        gz.write_all(&vec![b'z'; 5000]).unwrap();
        gz.finish().unwrap();
        let out = list_archive(&p, "gz").unwrap();
        assert_eq!(out.subtype.as_deref(), Some("gz"));
        let e = &out.structured["entries"][0];
        assert_eq!(e["name"], "server.log");
        assert_eq!(e["size"], 5000);
        assert_eq!(e["compressed"], std::fs::metadata(&p).unwrap().len());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn gz_without_name_uses_stem() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.csv.gz");
        let mut gz = flate2::write::GzEncoder::new(
            File::create(&p).unwrap(),
            flate2::Compression::default(),
        );
        gz.write_all(b"a,b\n").unwrap();
        gz.finish().unwrap();
        let out = list_archive(&p, "gz").unwrap();
        assert_eq!(out.structured["entries"][0]["name"], "data.csv");
        assert_eq!(out.structured["entries"][0]["size"], 4);
    }

    #[test]
    fn bomb_threshold_flags() {
        assert_eq!(bomb_warning(BOMB_BYTES), None);
        let w = bomb_warning(BOMB_BYTES + 1).unwrap();
        assert!(
            w.contains("possible zip bomb") && w.contains("50.0 GB"),
            "{w}"
        );
        let entries = vec![Entry {
            name: "huge.bin".into(),
            size: BOMB_BYTES + 1,
            compressed: Some(10),
            dir: false,
        }];
        let l = Listing {
            entries,
            ..Listing::default()
        };
        assert!(render_text("zip", &l, BOMB_BYTES + 1).contains("50.0 GB uncompressed"));
    }

    #[test]
    fn garbage_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.zip");
        std::fs::write(&p, b"definitely not a zip file").unwrap();
        assert!(list_archive(&p, "zip").is_err());
        let q = dir.path().join("y.gz");
        std::fs::write(&q, b"nope").unwrap();
        assert!(list_archive(&q, "gz").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(relative("./a/b"), "a/b");
        assert_eq!(display_path("a\nb"), "a?b");
        let e = |n: &str, dir| Entry {
            name: n.into(),
            size: 1,
            compressed: None,
            dir,
        };
        assert_eq!(
            top_level(&[
                e("a", false),
                e("a/b", false),
                e("/c", false),
                e("./d/", true)
            ]),
            vec!["a/", "c", "d/"]
        );
    }
}

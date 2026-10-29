//! Archive listings for the Drive's preview: zip (and jar/apk), tar, tar.gz/tgz and 7z, read with
//! pure-Rust readers. Nothing is ever extracted: entry names are only shown, so a hostile name
//! (`../../etc/passwd`) is just text, cleaned of control characters.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::Serialize;

/// At most this many entries in a listing.
pub const MAX_ENTRIES: usize = 5000;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub dir: bool,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Listing {
    pub format: &'static str,
    pub total: usize,
    pub truncated: bool,
    pub entries: Vec<Entry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("this archive format can't be listed")]
    Unsupported,
    #[error("the archive can't be read: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn clean(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).collect()
}

/// The format of an archive called `name`, if it can be listed.
pub fn format_of(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some("tar.gz")
    } else if lower.ends_with(".tar") {
        Some("tar")
    } else if lower.ends_with(".7z") {
        Some("7z")
    } else if [".zip", ".jar", ".apk", ".war"]
        .iter()
        .any(|e| lower.ends_with(e))
    {
        Some("zip")
    } else {
        None
    }
}

/// List the archive at `path`; `name` (the file's real name) picks the format.
pub fn list(path: &Path, name: &str) -> Result<Listing, ArchiveError> {
    let format = format_of(name).ok_or(ArchiveError::Unsupported)?;
    let mut entries = Vec::new();
    let mut total = 0usize;
    let mut add = |e: Entry| {
        total += 1;
        if entries.len() < MAX_ENTRIES {
            entries.push(e);
        }
        total
    };
    match format {
        "zip" => {
            let mut z = zip::ZipArchive::new(BufReader::new(File::open(path)?))
                .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            for i in 0..z.len() {
                let f = z
                    .by_index_raw(i)
                    .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
                let modified = f.last_modified().and_then(|d| {
                    chrono::NaiveDate::from_ymd_opt(
                        d.year() as i32,
                        d.month() as u32,
                        d.day() as u32,
                    )?
                    .and_hms_opt(d.hour() as u32, d.minute() as u32, d.second() as u32)
                    .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
                });
                add(Entry {
                    path: clean(&String::from_utf8_lossy(f.name_raw())),
                    size: f.size(),
                    dir: f.is_dir(),
                    modified,
                });
            }
        }
        "tar" | "tar.gz" => {
            let file: Box<dyn Read> = if format == "tar" {
                Box::new(BufReader::new(File::open(path)?))
            } else {
                Box::new(flate2::read::GzDecoder::new(BufReader::new(File::open(
                    path,
                )?)))
            };
            let mut ar = tar::Archive::new(file);
            for e in ar
                .entries()
                .map_err(|e| ArchiveError::Corrupt(e.to_string()))?
            {
                let e = e.map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
                let h = e.header();
                let path = clean(&String::from_utf8_lossy(&e.path_bytes()));
                let modified = h
                    .mtime()
                    .ok()
                    .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                let seen = add(Entry {
                    size: h.size().unwrap_or(0),
                    dir: h.entry_type().is_dir(),
                    path,
                    modified,
                });
                // Reading a .tar.gz means decompressing it; stop past the cap instead of at the end.
                if seen > MAX_ENTRIES * 4 {
                    break;
                }
            }
        }
        "7z" => {
            let ar = sevenz_rust::Archive::read(
                &mut File::open(path)?,
                std::fs::metadata(path)?.len(),
                &[],
            )
            .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            for f in &ar.files {
                add(Entry {
                    path: clean(f.name()),
                    size: f.size(),
                    dir: f.is_directory(),
                    modified: None,
                });
            }
        }
        _ => return Err(ArchiveError::Unsupported),
    }
    Ok(Listing {
        format,
        truncated: total > entries.len(),
        total,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lists_zip_and_tgz_without_extracting() {
        let t = tempfile::tempdir().unwrap();
        let zp = t.path().join("a.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.add_directory("src/", o).unwrap();
            z.start_file("src/main.rs", o).unwrap();
            z.write_all(b"fn main() {}").unwrap();
            z.start_file("../../evil\u{7}.txt", o).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
        }
        let l = list(&zp, "a.zip").unwrap();
        assert_eq!(l.format, "zip");
        assert_eq!(l.total, 3);
        assert!(l.entries[0].dir);
        assert_eq!(l.entries[1].path, "src/main.rs");
        assert_eq!(l.entries[1].size, 12);
        assert_eq!(l.entries[2].path, "../../evil.txt");
        assert!(
            !t.path().join("src").exists() && !t.path().parent().unwrap().join("evil.txt").exists()
        );

        let tp = t.path().join("b.tgz");
        {
            let gz = flate2::write::GzEncoder::new(
                File::create(&tp).unwrap(),
                flate2::Compression::fast(),
            );
            let mut b = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            h.set_size(5);
            h.set_mtime(1_700_000_000);
            h.set_cksum();
            b.append_data(&mut h, "notes/readme.md", &b"hello"[..])
                .unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let l = list(&tp, "b.tgz").unwrap();
        assert_eq!(l.format, "tar.gz");
        assert_eq!(
            l.entries,
            vec![Entry {
                path: "notes/readme.md".into(),
                size: 5,
                dir: false,
                modified: Some("2023-11-14T22:13:20Z".into()),
            }]
        );
        assert!(matches!(list(&tp, "b.rar"), Err(ArchiveError::Unsupported)));
        assert!(matches!(list(&tp, "b.zip"), Err(ArchiveError::Corrupt(_))));
    }
}

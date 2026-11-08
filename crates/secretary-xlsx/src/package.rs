//! The zip package. Reading keeps every part's name in archive order; writing raw-copies every
//! part that did not change (the same compressed bytes, CRC and timestamp) and deflates only the
//! replaced ones.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::Error;

/// Refuse packages that unpack to more than this (a timesheet is a few MB).
pub const MAX_UNPACKED: u64 = 256 << 20;
pub const MAX_PARTS: usize = 10_000;

pub struct Package<'a> {
    bytes: &'a [u8],
    names: Vec<String>,
    index: HashMap<String, usize>,
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut z =
            ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::NotXlsx(e.to_string()))?;
        if z.len() > MAX_PARTS {
            return Err(Error::NotXlsx("too many parts".into()));
        }
        let mut names = Vec::with_capacity(z.len());
        let mut total = 0u64;
        for i in 0..z.len() {
            let f = z
                .by_index_raw(i)
                .map_err(|e| Error::NotXlsx(e.to_string()))?;
            total = total.saturating_add(f.size());
            names.push(f.name().to_string());
        }
        if total > MAX_UNPACKED {
            return Err(Error::NotXlsx(
                "the workbook unpacks to more than 256 MB".into(),
            ));
        }
        let index = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        Ok(Package {
            bytes,
            names,
            index,
        })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn has(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// A part's bytes, decompressed.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some(&i) = self.index.get(name) else {
            return Ok(None);
        };
        let mut z =
            ZipArchive::new(Cursor::new(self.bytes)).map_err(|e| Error::NotXlsx(e.to_string()))?;
        let mut f = z.by_index(i).map_err(|e| Error::NotXlsx(e.to_string()))?;
        let mut out = Vec::with_capacity(f.size().min(MAX_UNPACKED) as usize);
        f.by_ref()
            .take(MAX_UNPACKED + 1)
            .read_to_end(&mut out)
            .map_err(|e| Error::NotXlsx(e.to_string()))?;
        if out.len() as u64 > MAX_UNPACKED {
            return Err(Error::NotXlsx("part too large".into()));
        }
        Ok(Some(out))
    }

    /// A part as UTF-8 text (OOXML parts are UTF-8; a BOM is dropped).
    pub fn read_str(&self, name: &str) -> Result<Option<String>, Error> {
        match self.read(name)? {
            None => Ok(None),
            Some(b) => {
                let b = b
                    .strip_prefix(b"\xEF\xBB\xBF")
                    .map(<[u8]>::to_vec)
                    .unwrap_or(b);
                String::from_utf8(b)
                    .map(Some)
                    .map_err(|_| Error::Unsupported(format!("{name} is not UTF-8")))
            }
        }
    }

    /// A new package: the same parts in the same order, `replaced` ones with new bytes.
    pub fn rewrite(&self, replaced: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>, Error> {
        let zerr = |e: zip::result::ZipError| Error::NotXlsx(e.to_string());
        let mut z = ZipArchive::new(Cursor::new(self.bytes)).map_err(zerr)?;
        let mut w = ZipWriter::new(Cursor::new(Vec::with_capacity(self.bytes.len() + 4096)));
        for i in 0..z.len() {
            let f = z.by_index_raw(i).map_err(zerr)?;
            match replaced.get(f.name()) {
                Some(data) => {
                    let mut opts = SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .large_file(data.len() as u64 >= u32::MAX as u64);
                    if let Some(t) = f.last_modified() {
                        opts = opts.last_modified_time(t);
                    }
                    let name = f.name().to_string();
                    drop(f);
                    w.start_file(name, opts).map_err(zerr)?;
                    w.write_all(data)
                        .map_err(|e| Error::NotXlsx(e.to_string()))?;
                }
                None => w.raw_copy_file(f).map_err(zerr)?,
            }
        }
        let comment = z.comment().to_vec();
        if !comment.is_empty() {
            w.set_raw_comment(comment.into_boxed_slice());
        }
        Ok(w.finish().map_err(zerr)?.into_inner())
    }
}

/// Resolve a relationship target against the part that owns the `.rels` (`xl/workbook.xml` +
/// `worksheets/sheet1.xml` → `xl/worksheets/sheet1.xml`; `/xl/x.xml` is absolute).
pub fn resolve_target(owner: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = owner.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The `.rels` part of a part: `xl/workbook.xml` → `xl/_rels/workbook.xml.rels`.
pub fn rels_of(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(
            resolve_target("xl/workbook.xml", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve_target("xl/workbook.xml", "/xl/styles.xml"),
            "xl/styles.xml"
        );
        assert_eq!(
            resolve_target("xl/worksheets/sheet1.xml", "../drawings/d1.xml"),
            "xl/drawings/d1.xml"
        );
        assert_eq!(rels_of("xl/workbook.xml"), "xl/_rels/workbook.xml.rels");
        assert_eq!(
            rels_of("[Content_Types].xml"),
            "_rels/[Content_Types].xml.rels"
        );
    }
}

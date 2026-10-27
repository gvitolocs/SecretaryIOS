//! EPUB 2/3 container → spine → chapters, TOC from `nav.xhtml` (EPUB 3) or `toc.ncx` (EPUB 2).
use crate::doc::{Block, Book, Chapter, SourceFormat, TocEntry};
use crate::{xhtml, Error, Result, Unsupported};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

/// Bounds on ZIP expansion. Defaults suit a 512 MB e-reader.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    /// Largest single entry that is expanded (XHTML chapter, OPF, NCX).
    pub max_entry_bytes: u64,
    /// Sum of all expanded entries.
    pub max_total_bytes: u64,
    /// Entries above 1 MiB with a larger expansion ratio are treated as zip bombs.
    pub max_ratio: u64,
    pub max_spine_items: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_entry_bytes: 16 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
            max_ratio: 100,
            max_spine_items: 5_000,
        }
    }
}

/// Font obfuscation is not DRM: these algorithms only scramble embedded fonts.
const FONT_OBFUSCATION: &[&str] = &[
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("EPUB: {e}"))
}

struct Archive<R: Read + Seek> {
    zip: zip::ZipArchive<R>,
    limits: Limits,
    expanded: u64,
}

impl<R: Read + Seek> Archive<R> {
    fn has(&mut self, name: &str) -> bool {
        self.zip.by_name(name).is_ok()
    }

    fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        if name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains('\\') {
            return Err(invalid(format!("unsafe entry path {name:?}")));
        }
        let l = self.limits;
        let f = match self.zip.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::UnsupportedArchive(m)) if m.contains("Password") => {
                return Err(Error::Unsupported(Unsupported::EncryptedZip))
            }
            Err(zip::result::ZipError::FileNotFound) => {
                return Err(invalid(format!("missing entry {name:?}")))
            }
            Err(e) => return Err(invalid(e)),
        };
        let size = f.size();
        if size > l.max_entry_bytes {
            return Err(Error::Limit(format!("{name}: {size} bytes expanded")));
        }
        if size > 1024 * 1024 && size / f.compressed_size().max(1) > l.max_ratio {
            return Err(Error::Limit(format!(
                "{name}: compression ratio above {}:1 (zip bomb?)",
                l.max_ratio
            )));
        }
        // The declared size may lie: cap what is actually inflated, too.
        let budget = l
            .max_entry_bytes
            .min(l.max_total_bytes.saturating_sub(self.expanded));
        let mut b = Vec::with_capacity(size.min(budget) as usize);
        f.take(budget + 1).read_to_end(&mut b)?;
        if b.len() as u64 > budget {
            return Err(Error::Limit(format!(
                "{name}: expanded data exceeds the entry/total cap"
            )));
        }
        self.expanded += b.len() as u64;
        Ok(b)
    }
}

/// Validate the end-of-central-directory record before `ZipArchive` allocates its index.
fn preflight<R: Read + Seek>(reader: &mut R, limits: &Limits) -> Result<()> {
    let length = reader.seek(SeekFrom::End(0))?;
    if length < 22 {
        return Err(invalid("not a ZIP file"));
    }
    let start = length.saturating_sub(65_557);
    reader.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    reader.take(65_557).read_to_end(&mut tail)?;
    let pos = tail
        .windows(4)
        .rposition(|b| b == b"PK\x05\x06")
        .ok_or_else(|| invalid("missing ZIP central directory"))?;
    let e = tail
        .get(pos..pos + 22)
        .ok_or_else(|| invalid("truncated ZIP directory"))?;
    let count = u16::from_le_bytes([e[10], e[11]]) as usize;
    if count > limits.max_entries || e[4..8] != [0; 4] {
        return Err(Error::Limit("too many ZIP entries or split archive".into()));
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn local(e: &BytesStart) -> Vec<u8> {
    e.local_name().as_ref().to_ascii_lowercase()
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes().with_checks(false).flatten().find_map(|a| {
        let key = a.key.as_ref();
        let l = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        (key == name || l == name).then(|| {
            a.unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
        })
    })
}

fn reader(xml: &[u8]) -> Reader<&[u8]> {
    let mut r = Reader::from_reader(xml);
    let c = r.config_mut();
    c.check_end_names = false;
    c.allow_unmatched_ends = true;
    c.allow_dangling_amp = true;
    r
}

/// Percent-decode an href and resolve it against the directory of `base` (a ZIP path).
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or("");
    let bytes = href.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                decoded.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let href = String::from_utf8_lossy(&decoded);
    let mut parts: Vec<&str> = match base.rfind('/') {
        Some(p) => base[..p].split('/').collect(),
        None => Vec::new(),
    };
    for seg in href.split('/') {
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

fn text_of(r: &mut Reader<&[u8]>, end: &[u8]) -> String {
    let mut s = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Text(t)) => s.push_str(&t.decode().unwrap_or_default()),
            Ok(Event::CData(t)) => s.push_str(&t.decode().unwrap_or_default()),
            Ok(Event::GeneralRef(g)) => {
                if let Ok(Some(c)) = g.resolve_char_ref() {
                    s.push(c);
                } else if let Some(v) = g.decode().ok().and_then(|n| xhtml::named_entity(&n)) {
                    s.push_str(v);
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref().eq_ignore_ascii_case(end) => break,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_drm<R: Read + Seek>(a: &mut Archive<R>) -> Result<()> {
    if a.has("META-INF/rights.xml") {
        return Err(Error::Unsupported(Unsupported::Drm));
    }
    if !a.has("META-INF/encryption.xml") {
        return Ok(());
    }
    let xml = a.read("META-INF/encryption.xml")?;
    let mut r = reader(&xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(&e).as_slice() {
                b"encryptionmethod" => {
                    let alg = attr(&e, b"Algorithm").unwrap_or_default();
                    if !FONT_OBFUSCATION.contains(&alg.trim()) {
                        return Err(Error::Unsupported(Unsupported::Drm));
                    }
                }
                // Key info / retrieval methods only exist for real encryption.
                b"keyinfo" | b"retrievalmethod" | b"encryptedkey" => {
                    return Err(Error::Unsupported(Unsupported::Drm))
                }
                _ => {}
            },
            Ok(Event::Eof) => return Ok(()),
            Ok(_) => {}
            // Unparseable encryption.xml: refuse rather than guess.
            Err(_) => return Err(Error::Unsupported(Unsupported::Drm)),
        }
    }
}

struct Opf {
    path: String,
    title: Option<String>,
    authors: Vec<String>,
    language: Option<String>,
    /// id -> (path, media type, properties)
    manifest: HashMap<String, (String, String, String)>,
    spine: Vec<String>,
    ncx_id: Option<String>,
}

fn parse_opf(path: String, xml: &[u8]) -> Result<Opf> {
    let mut r = reader(xml);
    let mut o = Opf {
        path,
        title: None,
        authors: Vec::new(),
        language: None,
        manifest: HashMap::new(),
        spine: Vec::new(),
        ncx_id: None,
    };
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(&e).as_slice() {
                b"title" if o.title.is_none() => {
                    o.title = Some(text_of(&mut r, b"title")).filter(|t| !t.is_empty())
                }
                b"creator" => {
                    let t = text_of(&mut r, b"creator");
                    if !t.is_empty() {
                        o.authors.push(t);
                    }
                }
                b"language" if o.language.is_none() => {
                    o.language = Some(text_of(&mut r, b"language")).filter(|t| !t.is_empty())
                }
                b"item" => {
                    if let (Some(id), Some(href)) = (attr(&e, b"id"), attr(&e, b"href")) {
                        let full = resolve(&o.path, &href);
                        o.manifest.insert(
                            id,
                            (
                                full,
                                attr(&e, b"media-type").unwrap_or_default(),
                                attr(&e, b"properties").unwrap_or_default(),
                            ),
                        );
                    }
                }
                b"spine" => o.ncx_id = attr(&e, b"toc"),
                b"itemref" => {
                    if let Some(id) = attr(&e, b"idref") {
                        o.spine.push(id);
                    }
                }
                _ => {}
            },
            Ok(Event::DocType(_)) => return Err(invalid("DTD in OPF is unsupported")),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(invalid(format!("OPF: {e}"))),
        }
    }
    Ok(o)
}

fn parse_nav(nav_path: &str, xml: &[u8]) -> Vec<(String, u8, String)> {
    let mut r = reader(xml);
    let mut out = Vec::new();
    let mut in_toc = false;
    let mut nav_depth = 0u32;
    let mut ol_depth = 0u8;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match local(&e).as_slice() {
                b"nav" => {
                    if in_toc {
                        nav_depth += 1;
                    } else if attr(&e, b"type")
                        .is_some_and(|t| t.split_whitespace().any(|w| w == "toc"))
                    {
                        in_toc = true;
                        nav_depth = 1;
                    }
                }
                b"ol" if in_toc => ol_depth = ol_depth.saturating_add(1),
                b"a" if in_toc => {
                    let href = attr(&e, b"href").unwrap_or_default();
                    let title = text_of(&mut r, b"a");
                    if !href.is_empty() && !title.is_empty() {
                        let full = resolve(nav_path, &href);
                        out.push((title, ol_depth.saturating_sub(1), full));
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref().to_ascii_lowercase().as_slice() {
                b"nav" if in_toc => {
                    nav_depth -= 1;
                    if nav_depth == 0 {
                        break;
                    }
                }
                b"ol" if in_toc => ol_depth = ol_depth.saturating_sub(1),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    out
}

fn parse_ncx(ncx_path: &str, xml: &[u8]) -> Vec<(String, u8, String)> {
    let mut r = reader(xml);
    let mut out = Vec::new();
    let mut depth = 0u8;
    let mut label = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(&e).as_slice() {
                b"navpoint" => {
                    depth = depth.saturating_add(1);
                    label.clear();
                }
                b"text" if depth > 0 => label = text_of(&mut r, b"text"),
                b"content" if depth > 0 => {
                    if let Some(src) = attr(&e, b"src") {
                        if !label.is_empty() {
                            out.push((
                                std::mem::take(&mut label),
                                depth - 1,
                                resolve(ncx_path, &src),
                            ));
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref().eq_ignore_ascii_case(b"navpoint") => {
                depth = depth.saturating_sub(1)
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    out
}

/// Read a DRM-free EPUB into the block model.
pub fn read<R: Read + Seek>(mut input: R, limits: &Limits) -> Result<Book> {
    preflight(&mut input, limits)?;
    let zip = zip::ZipArchive::new(input).map_err(invalid)?;
    if zip.len() > limits.max_entries {
        return Err(Error::Limit("too many ZIP entries".into()));
    }
    let mut a = Archive {
        zip,
        limits: *limits,
        expanded: 0,
    };
    check_drm(&mut a)?;

    let container = a.read("META-INF/container.xml")?;
    let mut r = reader(&container);
    let mut opf_path = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(&e) == b"rootfile" => {
                opf_path = attr(&e, b"full-path");
                if opf_path.is_some() {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(invalid(format!("container.xml: {e}"))),
        }
    }
    let opf_path = opf_path.ok_or_else(|| invalid("no OPF rootfile in container.xml"))?;
    let opf_xml = a.read(&opf_path)?;
    let opf = parse_opf(opf_path, &opf_xml)?;
    if opf.spine.len() > limits.max_spine_items {
        return Err(Error::Limit(format!("{} spine items", opf.spine.len())));
    }

    let mut book = Book::new(SourceFormat::Epub);
    book.title = opf.title.clone();
    book.authors = opf.authors.clone();
    book.language = opf.language.clone();

    let mut by_path: HashMap<String, usize> = HashMap::new();
    for id in &opf.spine {
        let Some((path, media, _)) = opf.manifest.get(id) else {
            book.warnings
                .push(format!("spine idref {id:?} not in manifest"));
            continue;
        };
        let blocks = if media.contains("html") || media.is_empty() {
            match a.read(path) {
                Ok(x) => {
                    let p = xhtml::parse(&x);
                    if let Some(w) = p.warning {
                        book.warnings.push(format!("{path}: {w}"));
                    }
                    p.blocks
                }
                Err(e @ (Error::Limit(_) | Error::Unsupported(_))) => return Err(e),
                Err(e) => {
                    book.warnings.push(format!("{path}: {e}"));
                    continue;
                }
            }
        } else if media.starts_with("image/") {
            vec![Block::Image {
                alt: String::new(),
                src: path.clone(),
            }]
        } else {
            book.warnings
                .push(format!("{path}: unsupported spine media type {media:?}"));
            continue;
        };
        by_path.insert(path.clone(), book.chapters.len());
        book.chapters.push(Chapter {
            title: None,
            href: path.clone(),
            blocks,
        });
    }

    // TOC: EPUB 3 nav document first, then the EPUB 2 NCX.
    let nav = opf
        .manifest
        .values()
        .find(|(_, _, props)| props.split_whitespace().any(|p| p == "nav"))
        .map(|(p, _, _)| p.clone());
    let ncx = opf
        .ncx_id
        .as_ref()
        .and_then(|id| opf.manifest.get(id))
        .or_else(|| {
            opf.manifest
                .values()
                .find(|(_, m, _)| m == "application/x-dtbncx+xml")
        })
        .map(|(p, _, _)| p.clone());
    let mut raw = Vec::new();
    if let Some(p) = nav {
        match a.read(&p) {
            Ok(x) => raw = parse_nav(&p, &x),
            Err(e @ Error::Limit(_)) => return Err(e),
            Err(e) => book.warnings.push(format!("nav {p}: {e}")),
        }
    }
    if raw.is_empty() {
        if let Some(p) = ncx {
            match a.read(&p) {
                Ok(x) => raw = parse_ncx(&p, &x),
                Err(e @ Error::Limit(_)) => return Err(e),
                Err(e) => book.warnings.push(format!("ncx {p}: {e}")),
            }
        }
    }
    book.toc = raw
        .into_iter()
        .map(|(title, depth, href)| TocEntry {
            chapter: by_path.get(&href).copied(),
            title,
            depth,
            href,
        })
        .collect();

    for (i, ch) in book.chapters.iter_mut().enumerate() {
        ch.title = ch.first_heading().or_else(|| {
            book.toc
                .iter()
                .find(|t| t.chapter == Some(i))
                .map(|t| t.title.clone())
        });
    }
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn hrefs_resolve_against_the_referencing_file() {
        assert_eq!(
            resolve("OEBPS/content.opf", "text/ch%201.xhtml#p3"),
            "OEBPS/text/ch 1.xhtml"
        );
        assert_eq!(
            resolve("OEBPS/nav/nav.xhtml", "../text/a.xhtml"),
            "OEBPS/text/a.xhtml"
        );
        assert_eq!(resolve("content.opf", "./a.html"), "a.html");
        assert_eq!(resolve("a/b.opf", "../../../x"), "x");
        assert_eq!(resolve("a/b.opf", "bad%zz%4"), "a/bad%zz%4");
    }
}

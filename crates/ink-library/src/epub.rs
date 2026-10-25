//! EPUB container and OPF metadata with bounded ZIP expansion and XML parsing.
use crate::{model::Metadata, Error, Result};
use quick_xml::{events::Event, Reader};
use std::io::{Read, Seek, SeekFrom};
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("EPUB: {e}"))
}
fn entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    if name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains('\\') {
        return Err(invalid("unsafe entry path"));
    }
    let f = zip.by_name(name).map_err(invalid)?;
    if f.size() > MAX_ENTRY_BYTES as u64 {
        return Err(invalid("entry exceeds 1 MiB"));
    }
    let mut b = Vec::new();
    f.take(MAX_ENTRY_BYTES as u64 + 1).read_to_end(&mut b)?;
    if b.len() > MAX_ENTRY_BYTES {
        return Err(invalid("entry exceeds 1 MiB"));
    }
    Ok(b)
}
/// Preflight the ordinary ZIP directory before ZipArchive allocates its index.
/// ZIP64 (unnecessary for metadata on this device) is deliberately unsupported.
fn preflight<R: Read + Seek>(reader: &mut R) -> Result<()> {
    let length = reader.seek(SeekFrom::End(0))?;
    let start = length.saturating_sub(65_557);
    reader.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    reader.take(65_557).read_to_end(&mut tail)?;
    let pos = tail
        .windows(4)
        .rposition(|b| b == b"PK\x05\x06")
        .ok_or_else(|| invalid("missing ZIP directory"))?;
    let e = tail
        .get(pos..pos + 22)
        .ok_or_else(|| invalid("truncated ZIP directory"))?;
    let count = u16::from_le_bytes([e[10], e[11]]) as usize;
    let zip64 = pos >= 20 && tail.get(pos - 20..pos - 16) == Some(b"PK\x06\x07");
    if count > MAX_ENTRIES || e[4..8] != [0; 4] || zip64 {
        return Err(invalid("too many ZIP entries or split archive"));
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(())
}
pub fn read<R: Read + Seek>(mut reader: R) -> Result<Metadata> {
    preflight(&mut reader)?;
    let mut zip = zip::ZipArchive::new(reader).map_err(invalid)?;
    if zip.len() > MAX_ENTRIES {
        return Err(invalid("too many ZIP entries"));
    }
    let container = entry(&mut zip, "META-INF/container.xml")?;
    let mut xml = Reader::from_reader(container.as_slice());
    let mut path = None;
    loop {
        match xml.read_event().map_err(invalid)? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"rootfile" => {
                for a in e.attributes() {
                    let a = a.map_err(invalid)?;
                    if a.key.as_ref() == b"full-path" {
                        path = Some(
                            a.decode_and_unescape_value(xml.decoder())
                                .map_err(invalid)?
                                .into_owned(),
                        );
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let opf = entry(&mut zip, &path.ok_or_else(|| invalid("no OPF rootfile"))?)?;
    let mut xml = Reader::from_reader(opf.as_slice());
    let mut m = Metadata::default();
    let mut key = Vec::new();
    let mut value = String::new();
    loop {
        match xml.read_event().map_err(invalid)? {
            Event::Start(e) => {
                key = e.local_name().as_ref().to_vec();
                value.clear();
            }
            Event::Text(e) => value.push_str(&e.decode().map_err(invalid)?),
            Event::CData(e) => value.push_str(&e.decode().map_err(invalid)?),
            Event::GeneralRef(e) => {
                let encoded = format!("&{};", e.decode().map_err(invalid)?);
                value.push_str(&quick_xml::escape::unescape(&encoded).map_err(invalid)?);
            }
            Event::End(_) => {
                let t = value.trim().to_owned();
                match key.as_slice() {
                    b"title" if m.title.is_none() => m.title = Some(t),
                    b"creator" => m.authors.push(t),
                    b"language" => m.language = Some(t),
                    b"identifier" => {
                        let digits: String = t
                            .chars()
                            .filter(|c| c.is_ascii_digit() || matches!(c, 'x' | 'X'))
                            .collect();
                        if t.to_lowercase().contains("isbn")
                            || digits.len() == 10
                            || digits.len() == 13
                        {
                            m.isbn = Some(t.trim_start_matches("urn:isbn:").to_owned());
                        }
                    }
                    _ => {}
                }
                key.clear();
                value.clear();
            }
            Event::DocType(_) => return Err(invalid("DTD is unsupported")),
            Event::Eof => break,
            _ => {}
        }
    }
    // An EPUB may use encrypted resources (encryption.xml); do not claim DRM-free.
    Ok(m)
}

//! Bounds-checked PalmDB/PalmDOC, MOBI and EXTH metadata only. No text decoding.
use crate::{model::*, Error, Result};
use std::io::Read;
pub const MAX_READ_BYTES: usize = 1024 * 1024;
fn bad() -> Error {
    Error::Invalid("malformed or truncated PalmDB/MOBI header".into())
}
fn bytes(b: &[u8], o: usize, n: usize) -> Result<&[u8]> {
    b.get(o..o.checked_add(n).ok_or_else(bad)?).ok_or_else(bad)
}
fn u16be(b: &[u8], o: usize) -> Result<u16> {
    let a = bytes(b, o, 2)?;
    Ok(u16::from_be_bytes([a[0], a[1]]))
}
fn u32be(b: &[u8], o: usize) -> Result<usize> {
    let a = bytes(b, o, 4)?;
    Ok(u32::from_be_bytes([a[0], a[1], a[2], a[3]]) as usize)
}
fn text(b: &[u8], encoding: usize) -> Result<String> {
    let decoded = match encoding {
        1252 => encoding_rs::WINDOWS_1252.decode_without_bom_handling(b).0,
        65001 => std::borrow::Cow::Borrowed(std::str::from_utf8(b).map_err(|_| bad())?),
        _ => {
            return Err(crate::Error::Invalid(
                "unsupported MOBI text encoding".into(),
            ))
        }
    };
    Ok(decoded.trim_matches('\0').trim().to_owned())
}
pub fn read(reader: impl Read) -> Result<Metadata> {
    let mut b = Vec::new();
    reader.take(MAX_READ_BYTES as u64).read_to_end(&mut b)?;
    parse(&b)
}
pub fn parse(b: &[u8]) -> Result<Metadata> {
    if bytes(b, 60, 8)? != b"BOOKMOBI" {
        return Err(bad());
    }
    let count = u16be(b, 76)? as usize;
    if count == 0 {
        return Err(bad());
    }
    bytes(b, 78, count.checked_mul(8).ok_or_else(bad)?)?;
    let start = u32be(b, 78)?;
    if start < 78 + count * 8 {
        return Err(bad());
    }
    let end = if count > 1 { u32be(b, 86)? } else { b.len() };
    if end < start {
        return Err(bad());
    }
    let r = bytes(b, start, end - start)?;
    let enc = u16be(r, 12)?;
    if bytes(r, 16, 4)? != b"MOBI" {
        return Err(bad());
    }
    let len = u32be(r, 20)?;
    if len < 116 {
        return Err(bad());
    }
    bytes(r, 16, len)?;
    let mut m = Metadata {
        drm: match enc {
            0 => DrmState::None,
            1 | 2 => DrmState::Protected,
            _ => DrmState::Unknown,
        },
        ..Metadata::default()
    };
    let encoding = u32be(r, 16 + 0x0c)?;
    let off = u32be(r, 16 + 0x44)?;
    let size = u32be(r, 16 + 0x48)?;
    if size > 0 {
        m.title = Some(text(bytes(r, off, size)?, encoding)?);
    }
    let language = u32be(r, 16 + 0x4c)?;
    if language != 0 {
        m.language = Some(format!("mobi-lcid:{language}"));
    }
    // Orthographic and inflection indexes (record 0 offsets 0x28 and 0x2C in the MobileRead MOBI
    // header table) are set only in dictionaries. Older Kindle dictionaries (Duden, Zingarelli,
    // Progressive) have no EXTH 531/532, so this is how they are recognised. Index 0 would be the
    // header record itself, so 0 counts as absent like 0xFFFFFFFF. A KF8 book can set 0x28 alone.
    let index = |at| u32be(r, at).map(|v| v != 0 && v != u32::MAX as usize);
    if index(0x28)? && index(0x2C)? {
        m.dictionary = true;
    }
    let ex = 16 + len;
    if len >= 116 && u32be(r, 16 + 0x70)? & 0x40 != 0 {
        if bytes(r, ex, 4)? != b"EXTH" {
            return Err(bad());
        }
        let el = u32be(r, ex + 4)?;
        if el < 12 {
            return Err(bad());
        }
        let e = bytes(r, ex, el)?;
        let n = u32be(e, 8)?;
        if n > (el - 12) / 8 {
            return Err(bad());
        }
        let mut p = 12;
        for _ in 0..n {
            let kind = u32be(e, p)?;
            let length = u32be(e, p + 4)?;
            if length < 8 {
                return Err(bad());
            }
            let payload = bytes(e, p + 8, length - 8)?;
            if matches!(kind, 531 | 532) {
                m.dictionary = true;
            }
            let val = if matches!(kind, 100 | 101 | 104 | 113 | 504 | 501 | 503) {
                text(payload, encoding)?
            } else {
                String::new()
            };
            match kind {
                100 => m.authors.push(val),
                101 => m.publisher = Some(val),
                104 => m.isbn = Some(val),
                113 | 504 => {
                    if m.asin.is_none() {
                        m.asin = Some(val)
                    }
                }
                501 => m.cdetype = Some(val),
                503 => m.title = Some(val),
                _ => {}
            }
            p = p.checked_add(length).ok_or_else(bad)?;
        }
    }
    Ok(m)
}

//! Lazy unencrypted `warc` archive reader. No archive decryption or native FFI.
use crate::{binary::Reader, invalid, Error, Result};
use flate2::read::DeflateDecoder;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
const HEADER: [u8; 20] = [
    119, 97, 114, 99, 255, 170, 69, 241, 232, 3, 0, 0, 4, 0, 0, 0, 2, 0, 0, 0,
];
const LIMIT: usize = 256 * 1024 * 1024;
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub offset: u64,
}
pub struct Archive {
    file: File,
    length: u64,
    data_end: u64,
    pub entries: Vec<Entry>,
}
fn block_payload(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader::new(bytes);
    let flag = r.u32()?;
    let _padding = r.u32()?;
    let plain = r.u32()? as usize;
    let stored = r.u32()? as usize;
    if plain > LIMIT || stored > LIMIT {
        return Err(invalid("archive payload size limit"));
    }
    let data = r.take(stored)?;
    let out = match flag {
        0 => data.to_vec(),
        1 => {
            // KISS stores a two-byte zlib-style prefix followed by raw DEFLATE,
            // without the Adler trailer required by a complete zlib stream.
            if data.len() < 2
                || data[0] != 0x78
                || (u16::from_be_bytes([data[0], data[1]]) % 31) != 0
                || data[1] & 32 != 0
            {
                return Err(invalid("archive deflate prefix"));
            }
            let mut out = Vec::new();
            DeflateDecoder::new(&data[2..])
                .take((plain + 1) as u64)
                .read_to_end(&mut out)?;
            out
        }
        _ => return Err(Error::Unsupported(format!("archive compression {flag}"))),
    };
    if out.len() != plain {
        return Err(invalid("archive decompressed size mismatch"));
    }
    Ok(out)
}
fn names(data: &[u8]) -> Result<HashMap<u64, String>> {
    let mut r = Reader::new(data);
    let mut names = HashMap::new();
    while r.left() > 0 {
        let hash = r.u64()?;
        let n = r.count(32768)?;
        let units: Vec<u16> = r
            .take(n * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let name = String::from_utf16(&units).map_err(|_| invalid("archive name UTF-16"))?;
        if let Some(old) = names.insert(hash, name.clone()) {
            if old != name {
                return Err(invalid("archive name hash collision"));
            }
        }
    }
    Ok(names)
}
pub fn safe_relative(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    if name.starts_with('/')
        || name.contains(':')
        || name.contains('\0')
        || name.split('/').any(|p| p == "..")
    {
        return Err(invalid("unsafe archive path"));
    }
    let path = name
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>()
        .join("/");
    if path.is_empty() {
        return Err(invalid("empty archive path"));
    }
    Ok(path)
}
fn tree(
    r: &mut Reader<'_>,
    names: &HashMap<u64, String>,
    prefix: &str,
    depth: usize,
    entries: &mut Vec<Entry>,
) -> Result<u64> {
    if depth > 64 {
        return Err(invalid("archive directory depth"));
    }
    if r.u64()? != 0x0000001000000020 {
        return Err(invalid("archive directory signature"));
    }
    let id = r.u64()?;
    let nd = r.count(1_000_000)?;
    let nf = r.count(1_000_000)?;
    let wire_depth = r.count(64)?;
    let _padding = r.u32()?;
    if wire_depth != depth || nd.saturating_add(nf).saturating_mul(16) > r.left() {
        return Err(invalid("archive directory counts"));
    }
    let mut dirs = Vec::new();
    for _ in 0..nd {
        dirs.push((r.u64()?, r.i64()?));
    }
    for _ in 0..nf {
        let hash = r.u64()?;
        let offset = r.i64()?;
        if offset < 0 {
            return Err(invalid("negative archive file offset"));
        }
        let name = names
            .get(&hash)
            .ok_or_else(|| invalid("missing file name"))?;
        let path = if prefix.is_empty() {
            safe_relative(name)?
        } else {
            safe_relative(&format!("{prefix}/{name}"))?
        };
        entries.push(Entry {
            path,
            offset: 28 + offset as u64,
        });
        if entries.len() > 1_000_000 {
            return Err(invalid("too many archive entries"));
        }
    }
    r.take(wire_depth * 8)?;
    for (hash, _offset) in dirs {
        let name = names
            .get(&hash)
            .ok_or_else(|| invalid("missing directory name"))?;
        let next = if prefix.is_empty() {
            safe_relative(name)?
        } else {
            safe_relative(&format!("{prefix}/{name}"))?
        };
        if tree(r, names, &next, depth + 1, entries)? != hash {
            return Err(invalid("directory hash mismatch"));
        }
    }
    Ok(id)
}
impl Archive {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut header = [0; 28];
        file.read_exact(&mut header)?;
        if &header[..4] == b"warp" {
            return Err(Error::Unsupported(
                "encrypted archive; no decryption implemented".into(),
            ));
        }
        if header[..20] != HEADER {
            return Err(invalid("archive signature"));
        }
        let relative = i64::from_le_bytes(header[20..].try_into().unwrap());
        if relative < 0 {
            return Err(invalid("negative metadata offset"));
        }
        let data_end = 28u64
            .checked_add(relative as u64)
            .ok_or_else(|| invalid("metadata overflow"))?;
        let metadata_len = length
            .checked_sub(data_end)
            .filter(|n| *n <= LIMIT as u64)
            .ok_or_else(|| invalid("metadata bounds"))?;
        file.seek(SeekFrom::Start(data_end))?;
        let mut metadata = vec![0; metadata_len as usize];
        file.read_exact(&mut metadata)?;
        let mut r = Reader::new(&metadata);
        let mut hash_data = None;
        let mut name_data = None;
        let mut types = HashSet::new();
        while r.left() > 0 {
            let kind = r.u32()?;
            let size = r.u64()?;
            if size > LIMIT as u64 {
                return Err(invalid("archive metadata block limit"));
            }
            let data = r.take(size as usize)?;
            if !types.insert(kind) {
                return Err(invalid("duplicate archive metadata block"));
            }
            match kind {
                0 => hash_data = Some(data),
                1 => (),
                3 => name_data = Some(block_payload(data)?),
                _ => return Err(Error::Unsupported(format!("archive metadata {kind}"))),
            }
        }
        let names = names(&name_data.ok_or_else(|| invalid("archive names missing"))?)?;
        let mut entries = Vec::new();
        tree(
            &mut Reader::new(hash_data.ok_or_else(|| invalid("archive tree missing"))?),
            &names,
            "",
            0,
            &mut entries,
        )?;
        let mut seen = HashSet::new();
        for e in &entries {
            if !seen.insert(e.path.to_lowercase())
                || e.offset < 28
                || e.offset.checked_add(16).is_none_or(|v| v > data_end)
            {
                return Err(invalid("archive entry bounds or duplicate path"));
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            file,
            length,
            data_end,
            entries,
        })
    }
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let path = safe_relative(path)?;
        let e = self
            .entries
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(&path))
            .ok_or_else(|| invalid(format!("archive entry not found: {path}")))?;
        self.file.seek(SeekFrom::Start(e.offset))?;
        let mut header = [0; 16];
        self.file.read_exact(&mut header)?;
        let stored = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let end = e
            .offset
            .checked_add(16 + stored as u64)
            .ok_or_else(|| invalid("entry overflow"))?;
        if stored > LIMIT || end > self.data_end || end > self.length {
            return Err(invalid("archive file payload bounds"));
        }
        let mut bytes = Vec::from(header);
        bytes.resize(16 + stored, 0);
        self.file.read_exact(&mut bytes[16..])?;
        block_payload(&bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_cannot_escape() {
        for p in ["../x", "C:\\x", "/root", "x/../../a", "a\0b"] {
            assert!(safe_relative(p).is_err());
        }
        assert_eq!(
            safe_relative("body\\face.model").unwrap(),
            "body/face.model"
        );
    }
    #[test]
    fn decompression_is_bounded_and_exact() {
        let mut b = vec![0u8; 16];
        b[8..12].copy_from_slice(&3u32.to_le_bytes());
        b[12..].copy_from_slice(&3u32.to_le_bytes());
        b.extend(b"abc");
        assert_eq!(block_payload(&b).unwrap(), b"abc");
        b[8] = 4;
        assert!(block_payload(&b).is_err());
        for n in 0..16 {
            assert!(block_payload(&b[..n]).is_err());
        }
    }
}

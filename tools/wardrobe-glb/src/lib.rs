//! GLB container I/O for the wardrobe tools: the JSON chunk is edited, every byte after it (the
//! BIN chunk, meshopt-compressed or not) is written back untouched.

use serde_json::Value;
use std::{fs, io, path::Path};

pub struct Glb {
    pub json: Value,
    /// Everything after the JSON chunk, chunk headers included.
    pub rest: Vec<u8>,
}

impl Glb {
    pub fn read(path: &Path) -> io::Result<Self> {
        let b = fs::read(path)?;
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {m}", path.display()));
        if b.len() < 20 || &b[0..4] != b"glTF" || &b[16..20] != b"JSON" {
            return Err(bad("not a GLB with a JSON chunk"));
        }
        let n = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
        let end = 20usize.checked_add(n).filter(|&e| e <= b.len()).ok_or_else(|| bad("JSON chunk past the end"))?;
        let json = serde_json::from_slice(&b[20..end]).map_err(|e| bad(&e.to_string()))?;
        Ok(Self { json, rest: b[end..].to_vec() })
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut js = serde_json::to_vec(&self.json)?;
        while !js.len().is_multiple_of(4) {
            js.push(b' ');
        }
        let total = 20 + js.len() + self.rest.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(js.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&js);
        out.extend_from_slice(&self.rest);
        fs::write(path, out)
    }

    /// The array `key` (empty when absent).
    pub fn list(&self, key: &str) -> Vec<Value> {
        self.json[key].as_array().cloned().unwrap_or_default()
    }

    pub fn node_index(&self, name: &str) -> Option<usize> {
        self.json["nodes"].as_array()?.iter().position(|n| n["name"] == name)
    }

    pub fn parent_of(&self, i: usize) -> Option<usize> {
        self.json["nodes"].as_array()?.iter().position(|n| {
            n["children"].as_array().is_some_and(|c| c.iter().any(|x| x.as_u64() == Some(i as u64)))
        })
    }
}

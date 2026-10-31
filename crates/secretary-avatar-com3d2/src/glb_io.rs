use crate::{invalid, Result};
use serde_json::Value;
use std::{fs, path::Path};
pub fn read(path: &Path) -> Result<(Value, Vec<u8>)> {
    let bytes = fs::read(path)?;
    if bytes.len() < 28
        || &bytes[..4] != b"glTF"
        || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 2
        || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != bytes.len()
    {
        return Err(invalid("GLB header"));
    }
    let n = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let end = 20usize
        .checked_add(n)
        .ok_or_else(|| invalid("GLB JSON overflow"))?;
    if bytes.get(16..20) != Some(b"JSON") || bytes.get(end + 4..end + 8) != Some(b"BIN\0") {
        return Err(invalid("GLB chunks"));
    }
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut doc: Value = serde_json::from_slice(
        bytes
            .get(20..end)
            .ok_or_else(|| invalid("GLB JSON bounds"))?,
    )
    .map_err(|e| invalid(e.to_string()))?;
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut data = bytes
        .get(end + 8..)
        .ok_or_else(|| invalid("GLB data bounds"))?
        .to_vec();
    let declared = u32::from_le_bytes(
        bytes
            .get(end..end + 4)
            .ok_or_else(|| invalid("GLB BIN length"))?
            .try_into()
            .unwrap(),
    ) as usize;
    if declared != data.len() || !n.is_multiple_of(4) || !declared.is_multiple_of(4) {
        return Err(invalid("GLB chunk length"));
    }
    if doc["extensionsUsed"]
        .as_array()
        .is_some_and(|a| a.iter().any(|s| s == "EXT_meshopt_compression"))
    {
        #[cfg(not(target_arch = "wasm32"))]
        crate::compressed::unpack(&mut doc, &mut data)?;
        #[cfg(target_arch = "wasm32")]
        return Err(invalid("compressed imports are processed by the Rust host"));
    }
    if doc["buffers"].as_array().is_none_or(|a| a.len() != 1) || !doc["buffers"][0]["uri"].is_null()
    {
        return Err(invalid("only one embedded GLB buffer is supported"));
    }
    let length = doc["buffers"][0]["byteLength"]
        .as_u64()
        .ok_or_else(|| invalid("GLB buffer length"))? as usize;
    if length > data.len() || data.len() - length > 3 {
        return Err(invalid("GLB embedded buffer length"));
    }
    Ok((doc, data))
}
pub fn write(path: &Path, doc: &Value, data: &[u8]) -> Result<()> {
    let mut json = serde_json::to_vec(doc).map_err(|e| invalid(e.to_string()))?;
    while !json.len().is_multiple_of(4) {
        json.push(32);
    }
    let mut data = data.to_vec();
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let size = 12 + 8 + json.len() + 8 + data.len();
    if size > u32::MAX as usize {
        return Err(invalid("GLB size limit"));
    }
    let mut bytes = Vec::with_capacity(size);
    bytes.extend(b"glTF");
    bytes.extend(2u32.to_le_bytes());
    bytes.extend((size as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend(b"JSON");
    bytes.extend(json);
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(b"BIN\0");
    bytes.extend(data);
    fs::write(path, bytes)?;
    Ok(())
}

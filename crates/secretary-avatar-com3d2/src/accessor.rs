//! Bounded uncompressed glTF accessor IO, including sparse morph targets.
use crate::{invalid, Result};
use serde_json::{json, Value};
pub fn read(doc: &Value, data: &[u8], index: usize) -> Result<Vec<Vec<f32>>> {
    let a = &doc["accessors"][index];
    let count = a["count"]
        .as_u64()
        .filter(|n| *n <= 2_000_000)
        .ok_or_else(|| invalid("accessor count"))? as usize;
    let width = match a["type"].as_str() {
        Some("SCALAR") => 1,
        Some("VEC2") => 2,
        Some("VEC3") => 3,
        Some("VEC4") => 4,
        Some("MAT4") => 16,
        _ => return Err(invalid("accessor type")),
    };
    let component = a["componentType"]
        .as_u64()
        .ok_or_else(|| invalid("accessor component"))?;
    let size = match component {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return Err(invalid("accessor component")),
    };
    let normalized = a["normalized"] == true;
    let scalar =
        |b: &[u8]| -> f32 {
            match component {
                5126 => f32::from_le_bytes(b.try_into().unwrap()),
                5125 => u32::from_le_bytes(b.try_into().unwrap()) as f32,
                5123 => {
                    u16::from_le_bytes(b.try_into().unwrap()) as f32
                        / if normalized { 65535. } else { 1. }
                }
                5122 => (i16::from_le_bytes(b.try_into().unwrap()) as f32
                    / if normalized { 32767. } else { 1. })
                .max(if normalized { -1. } else { f32::NEG_INFINITY }),
                5120 => (b[0] as i8 as f32 / if normalized { 127. } else { 1. })
                    .max(if normalized { -1. } else { f32::NEG_INFINITY }),
                _ => b[0] as f32 / if normalized { 255. } else { 1. },
            }
        };
    let rows = |view: usize, offset: usize, n: usize, bytes: usize| -> Result<Vec<&[u8]>> {
        let v = &doc["bufferViews"][view];
        if !v["extensions"].is_null() {
            return Err(invalid("compressed accessor unsupported"));
        }
        let begin = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = v["byteLength"]
            .as_u64()
            .ok_or_else(|| invalid("view length"))? as usize;
        let end = begin
            .checked_add(len)
            .ok_or_else(|| invalid("view bounds"))?;
        let d = data
            .get(begin..end)
            .ok_or_else(|| invalid("view data bounds"))?;
        let stride = v["byteStride"].as_u64().unwrap_or(bytes as u64) as usize;
        if stride < bytes {
            return Err(invalid("accessor stride"));
        }
        let mut out = Vec::new();
        for i in 0..n {
            let start = offset
                .checked_add(
                    i.checked_mul(stride)
                        .ok_or_else(|| invalid("accessor overflow"))?,
                )
                .ok_or_else(|| invalid("accessor overflow"))?;
            out.push(
                d.get(
                    start
                        ..start
                            .checked_add(bytes)
                            .ok_or_else(|| invalid("accessor overflow"))?,
                )
                .ok_or_else(|| invalid("accessor bounds"))?,
            );
        }
        Ok(out)
    };
    let mut out = vec![vec![0.; width]; count];
    if let Some(v) = a["bufferView"].as_u64() {
        for (i, b) in rows(
            v as usize,
            a["byteOffset"].as_u64().unwrap_or(0) as usize,
            count,
            size * width,
        )?
        .iter()
        .enumerate()
        {
            out[i] = b.chunks_exact(size).map(&scalar).collect();
        }
    }
    if let Some(s) = a["sparse"].as_object() {
        let n = s["count"]
            .as_u64()
            .filter(|n| *n <= count as u64)
            .ok_or_else(|| invalid("sparse count"))? as usize;
        let isize = match s["indices"]["componentType"].as_u64() {
            Some(5121) => 1,
            Some(5123) => 2,
            Some(5125) => 4,
            _ => return Err(invalid("sparse indices")),
        };
        let ids = rows(
            s["indices"]["bufferView"]
                .as_u64()
                .ok_or_else(|| invalid("sparse view"))? as usize,
            s["indices"]["byteOffset"].as_u64().unwrap_or(0) as usize,
            n,
            isize,
        )?;
        let values = rows(
            s["values"]["bufferView"]
                .as_u64()
                .ok_or_else(|| invalid("sparse value view"))? as usize,
            s["values"]["byteOffset"].as_u64().unwrap_or(0) as usize,
            n,
            size * width,
        )?;
        let mut previous = None;
        for (i, b) in ids.iter().enumerate() {
            let id = match isize {
                1 => b[0] as usize,
                2 => u16::from_le_bytes((*b).try_into().unwrap()) as usize,
                _ => u32::from_le_bytes((*b).try_into().unwrap()) as usize,
            };
            if id >= count || previous.is_some_and(|p| id <= p) {
                return Err(invalid("sparse ordering or bounds"));
            }
            out[id] = values[i].chunks_exact(size).map(&scalar).collect();
            previous = Some(id);
        }
    }
    if out.iter().flatten().any(|v| !v.is_finite()) {
        return Err(invalid("accessor numeric value"));
    }
    Ok(out)
}
pub fn floats(
    doc: &mut Value,
    data: &mut Vec<u8>,
    rows: &[Vec<f32>],
    width: usize,
) -> Result<usize> {
    if rows.is_empty()
        || rows
            .iter()
            .any(|r| r.len() != width || r.iter().any(|v| !v.is_finite()))
    {
        return Err(invalid("output accessor shape"));
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    for row in rows {
        for v in row {
            data.extend(v.to_le_bytes());
        }
    }
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":start,"byteLength":data.len()-start}));
    let index = doc["accessors"].as_array().unwrap().len();
    let kind = match width {
        1 => "SCALAR",
        2 => "VEC2",
        3 => "VEC3",
        4 => "VEC4",
        16 => "MAT4",
        _ => return Err(invalid("output accessor width")),
    };
    let mut a = json!({"bufferView":view,"componentType":5126,"type":kind,"count":rows.len()});
    if width == 3 {
        let mut lo = vec![f32::INFINITY; 3];
        let mut hi = vec![f32::NEG_INFINITY; 3];
        for r in rows {
            for i in 0..3 {
                lo[i] = lo[i].min(r[i]);
                hi[i] = hi[i].max(r[i]);
            }
        }
        a["min"] = json!(lo);
        a["max"] = json!(hi);
    }
    doc["accessors"].as_array_mut().unwrap().push(a);
    Ok(index)
}
pub fn shorts(doc: &mut Value, data: &mut Vec<u8>, rows: &[[u16; 4]]) -> usize {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    for row in rows {
        for v in row {
            data.extend(v.to_le_bytes());
        }
    }
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":start,"byteLength":rows.len()*8}));
    let a = doc["accessors"].as_array().unwrap().len();
    doc["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView":view,"componentType":5123,"type":"VEC4","count":rows.len()}));
    a
}

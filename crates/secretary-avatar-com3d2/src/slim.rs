//! Smaller composed views for download (the last compose step). A composed view carried every
//! morph target as a dense float array (31 on the body and head, mostly zeros), embedded the same
//! garment textures more than once, and kept 4096-pixel source textures: 17–30 MB per look.
//! - Morph targets become sparse accessors: only the vertices a target moves are stored.
//! - Byte-identical images are stored once.
//! - Textures larger than `MAX_TEXTURE` (or `MAX_FACE_TEXTURE` for eyes and face) are downscaled.
//! - The binary chunk is repacked with only what the document references.
//!
//! Glb readers here (`glb_io::read`, `accessor::read`) do not read sparse accessors: run this only
//! on a finished view, which only the browser loads.
use crate::{glb_io, invalid, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    io::Cursor,
    path::Path,
};

const MAX_TEXTURE: u32 = 1024;
const MAX_FACE_TEXTURE: u32 = 2048;

fn le_f32(data: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// The bytes of buffer view `index`.
fn view<'a>(doc: &Value, data: &'a [u8], index: usize) -> Result<&'a [u8]> {
    let v = &doc["bufferViews"][index];
    let start = v["byteOffset"].as_u64().unwrap_or(0) as usize;
    let len = v["byteLength"].as_u64().ok_or_else(|| invalid("buffer view length"))? as usize;
    data.get(start..start + len).ok_or_else(|| invalid("buffer view bounds"))
}

/// Appends `bytes` as a new buffer view (4-byte aligned) and returns its index.
fn push_view(doc: &mut Value, data: &mut Vec<u8>, bytes: &[u8]) -> usize {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    data.extend_from_slice(bytes);
    let views = doc["bufferViews"].as_array_mut().unwrap();
    views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": bytes.len()}));
    views.len() - 1
}

/// Dense float VEC3 morph accessor -> sparse (non-zero rows only) when that is smaller.
fn sparsify(doc: &mut Value, data: &mut Vec<u8>, accessor: usize) -> Result<bool> {
    let a = doc["accessors"][accessor].clone();
    if a.get("sparse").is_some() || a["componentType"] != 5126 || a["type"] != "VEC3" {
        return Ok(false);
    }
    let Some(bv) = a["bufferView"].as_u64() else { return Ok(false) };
    let count = a["count"].as_u64().ok_or_else(|| invalid("accessor count"))? as usize;
    let stride = doc["bufferViews"][bv as usize]["byteStride"].as_u64().unwrap_or(12) as usize;
    let base = a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let bytes = view(doc, data, bv as usize)?;
    let mut indices = Vec::new();
    let mut values = Vec::new();
    for i in 0..count {
        let o = base + i * stride;
        if o + 12 > bytes.len() {
            return Err(invalid("morph accessor bounds"));
        }
        let row = [le_f32(bytes, o), le_f32(bytes, o + 4), le_f32(bytes, o + 8)];
        if row.iter().any(|v| v.abs() > 1e-7) {
            indices.push(i as u32);
            values.extend(row);
        }
    }
    // Sparse costs 16 bytes a moved vertex against 12 for every vertex dense.
    if indices.len() * 16 >= count * 12 {
        return Ok(false);
    }
    let mut out = json!({"componentType": 5126, "type": "VEC3", "count": count});
    if let Some(name) = a.get("name") {
        out["name"] = name.clone();
    }
    if !indices.is_empty() {
        let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for r in values.as_chunks::<3>().0 {
            for k in 0..3 {
                lo[k] = lo[k].min(r[k]);
                hi[k] = hi[k].max(r[k]);
            }
        }
        // Unmoved vertices are zero: the bounds include it.
        for k in 0..3 {
            lo[k] = lo[k].min(0.);
            hi[k] = hi[k].max(0.);
        }
        let iv = push_view(doc, data, &indices.iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<_>>());
        let vv = push_view(doc, data, &values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
        out["sparse"] = json!({"count": indices.len(), "indices": {"bufferView": iv, "componentType": 5125}, "values": {"bufferView": vv}});
        out["min"] = json!(lo);
        out["max"] = json!(hi);
    } else {
        // A target that moves nothing: an all-zero sparse accessor needs at least one entry.
        let iv = push_view(doc, data, &0u32.to_le_bytes());
        let vv = push_view(doc, data, &[0u8; 12]);
        out["sparse"] = json!({"count": 1, "indices": {"bufferView": iv, "componentType": 5125}, "values": {"bufferView": vv}});
        out["min"] = json!([0, 0, 0]);
        out["max"] = json!([0, 0, 0]);
    }
    doc["accessors"][accessor] = out;
    Ok(true)
}

fn decode(bytes: &[u8]) -> Result<image::RgbaImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| invalid(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    if bytes.starts_with(b"\x89PNG") {
        let mut d = png::Decoder::new(Cursor::new(bytes));
        d.set_limits(png::Limits { bytes: 256 * 1024 * 1024 });
        d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = d.read_info().map_err(|e| invalid(e.to_string()))?;
        let mut buffer = vec![0; r.output_buffer_size().ok_or_else(|| invalid("image size"))?];
        let info = r.next_frame(&mut buffer).map_err(|e| invalid(e.to_string()))?;
        let channels = info.color_type.samples();
        let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
        for p in buffer[..info.buffer_size()].chunks_exact(channels) {
            match channels {
                4 => rgba.extend(p),
                3 => rgba.extend([p[0], p[1], p[2], 255]),
                2 => rgba.extend([p[0], p[0], p[0], p[1]]),
                1 => rgba.extend([p[0], p[0], p[0], 255]),
                _ => return Err(invalid("image channels")),
            }
        }
        return image::RgbaImage::from_raw(info.width, info.height, rgba).ok_or_else(|| invalid("image buffer"));
    }
    Ok(reader.decode().map_err(|e| invalid(e.to_string()))?.to_rgba8())
}

fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
        let opaque = img.pixels().all(|p| p[3] == 255);
        encoder.set_color(if opaque { png::ColorType::Rgb } else { png::ColorType::Rgba });
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(|e| invalid(e.to_string()))?;
        if opaque {
            let rgb: Vec<u8> = img.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            writer.write_image_data(&rgb).map_err(|e| invalid(e.to_string()))?;
        } else {
            writer.write_image_data(img.as_raw()).map_err(|e| invalid(e.to_string()))?;
        }
    }
    Ok(out)
}

/// The largest side image `index` may keep: eyes and face textures keep more detail.
fn texture_limits(doc: &Value) -> HashMap<usize, u32> {
    let mut limits = HashMap::new();
    let textures = doc["textures"].as_array().cloned().unwrap_or_default();
    for m in doc["materials"].as_array().into_iter().flatten() {
        let name = m["name"].as_str().unwrap_or("");
        let limit = if ["Eye", "Face", "Lash", "Mouth"].iter().any(|k| name.starts_with(k)) { MAX_FACE_TEXTURE } else { MAX_TEXTURE };
        let mut refs = Vec::new();
        collect_texture_indices(m, &mut refs);
        for t in refs {
            if let Some(src) = textures.get(t).and_then(|t| t["source"].as_u64()) {
                let e = limits.entry(src as usize).or_insert(limit);
                *e = (*e).max(limit);
            }
        }
    }
    limits
}

fn collect_texture_indices(v: &Value, out: &mut Vec<usize>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if k.ends_with("Texture") {
                    if let Some(i) = x["index"].as_u64() {
                        out.push(i as usize);
                    }
                }
                collect_texture_indices(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_texture_indices(x, out)),
        _ => {}
    }
}

/// Smaller images: identical ones merged, oversized ones downscaled (PNG, alpha kept).
fn slim_images(doc: &mut Value, data: &mut Vec<u8>) -> Result<(usize, usize)> {
    let limits = texture_limits(doc);
    let images = doc["images"].as_array().cloned().unwrap_or_default();
    let mut first: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut alias: BTreeMap<usize, usize> = BTreeMap::new();
    let mut resized = 0;
    for (i, img) in images.iter().enumerate() {
        let Some(bv) = img["bufferView"].as_u64() else { continue };
        let bytes = view(doc, data, bv as usize)?.to_vec();
        if let Some(&j) = first.get(&bytes) {
            alias.insert(i, j);
            continue;
        }
        first.insert(bytes.clone(), i);
        let limit = limits.get(&i).copied().unwrap_or(MAX_TEXTURE);
        let Ok(decoded) = decode(&bytes) else { continue };
        if decoded.width().max(decoded.height()) <= limit {
            continue;
        }
        let scale = limit as f32 / decoded.width().max(decoded.height()) as f32;
        let (w, h) = (((decoded.width() as f32 * scale).round() as u32).max(1), ((decoded.height() as f32 * scale).round() as u32).max(1));
        let small = image::imageops::resize(&decoded, w, h, image::imageops::FilterType::Lanczos3);
        let png = encode_png(&small)?;
        if png.len() < bytes.len() {
            let nv = push_view(doc, data, &png);
            doc["images"][i]["bufferView"] = json!(nv);
            doc["images"][i]["mimeType"] = json!("image/png");
            resized += 1;
        }
    }
    for t in doc["textures"].as_array_mut().into_iter().flatten() {
        if let Some(src) = t["source"].as_u64() {
            if let Some(&j) = alias.get(&(src as usize)) {
                t["source"] = json!(j);
            }
        }
    }
    Ok((alias.len(), resized))
}

/// Only the buffer views the document references, compacted; indices remapped.
fn repack(doc: &mut Value, data: &[u8]) -> Result<Vec<u8>> {
    let mut used = std::collections::BTreeSet::new();
    for a in doc["accessors"].as_array().into_iter().flatten() {
        if let Some(v) = a["bufferView"].as_u64() {
            used.insert(v as usize);
        }
        if let Some(s) = a.get("sparse") {
            used.extend(s["indices"]["bufferView"].as_u64().map(|v| v as usize));
            used.extend(s["values"]["bufferView"].as_u64().map(|v| v as usize));
        }
    }
    let used_images: std::collections::BTreeSet<usize> =
        doc["textures"].as_array().into_iter().flatten().filter_map(|t| t["source"].as_u64()).map(|s| s as usize).collect();
    for (i, img) in doc["images"].as_array().into_iter().flatten().enumerate() {
        if used_images.contains(&i) {
            used.extend(img["bufferView"].as_u64().map(|v| v as usize));
        }
    }
    let mut map = HashMap::new();
    let mut views = Vec::new();
    let mut out = Vec::new();
    for &v in &used {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let bytes = view(doc, data, v)?;
        let mut nv = doc["bufferViews"][v].clone();
        nv["byteOffset"] = json!(out.len());
        out.extend_from_slice(bytes);
        map.insert(v, views.len());
        views.push(nv);
    }
    let remap = |x: &mut Value| {
        if let Some(v) = x.as_u64() {
            *x = json!(map[&(v as usize)]);
        }
    };
    for a in doc["accessors"].as_array_mut().into_iter().flatten() {
        if a.get("bufferView").is_some() {
            remap(&mut a["bufferView"]);
        }
        if let Some(s) = a.get_mut("sparse") {
            remap(&mut s["indices"]["bufferView"]);
            remap(&mut s["values"]["bufferView"]);
        }
    }
    // Unused images keep a 1x1 stand-in rather than shifting every texture's source index.
    let blank = encode_png(&image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])))?;
    let mut blank_view = None;
    for (i, img) in doc["images"].as_array_mut().into_iter().flatten().enumerate() {
        if used_images.contains(&i) {
            remap(&mut img["bufferView"]);
        } else if img.get("bufferView").is_some() {
            let v = *blank_view.get_or_insert_with(|| {
                while !out.len().is_multiple_of(4) {
                    out.push(0);
                }
                let start = out.len();
                out.extend_from_slice(&blank);
                views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": blank.len()}));
                views.len() - 1
            });
            img["bufferView"] = json!(v);
            img["mimeType"] = json!("image/png");
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    doc["bufferViews"] = json!(views);
    doc["buffers"][0]["byteLength"] = json!(out.len());
    Ok(out)
}

/// Slims the finished view at `path` in place. Returns `{before, after, sparseTargets,
/// mergedImages, resizedImages}` (bytes are the binary chunk's).
pub fn slim(path: &Path) -> Result<Value> {
    let (mut doc, mut data) = glb_io::read(path)?;
    let before = data.len();
    let mut targets = std::collections::BTreeSet::new();
    for m in doc["meshes"].as_array().into_iter().flatten() {
        for p in m["primitives"].as_array().into_iter().flatten() {
            for t in p["targets"].as_array().into_iter().flatten() {
                for a in t.as_object().into_iter().flat_map(|o| o.values()) {
                    if let Some(i) = a.as_u64() {
                        targets.insert(i as usize);
                    }
                }
            }
        }
    }
    let mut sparse = 0;
    for a in targets {
        sparse += sparsify(&mut doc, &mut data, a)? as usize;
    }
    let (merged, resized) = slim_images(&mut doc, &mut data)?;
    let packed = repack(&mut doc, &data)?;
    let after = packed.len();
    glb_io::write(path, &doc, &packed)?;
    Ok(json!({"before": before, "after": after, "sparseTargets": sparse, "mergedImages": merged, "resizedImages": resized}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn mostly_zero_morphs_go_sparse_and_keep_their_values() {
        // 100 vertices, a target that moves vertices 3 and 70 only.
        let mut dense = vec![0f32; 300];
        dense[9..12].copy_from_slice(&[0.1, -0.2, 0.3]);
        dense[210..213].copy_from_slice(&[0.0, 0.5, 0.0]);
        let mut data = le(&dense);
        let mut doc = json!({
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 1200}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 100}],
            "buffers": [{"byteLength": 1200}]
        });
        assert!(sparsify(&mut doc, &mut data, 0).unwrap());
        let a = &doc["accessors"][0];
        assert!(a.get("bufferView").is_none());
        assert_eq!(a["count"], 100);
        assert_eq!(a["sparse"]["count"], 2);
        let idx = view(&doc, &data, a["sparse"]["indices"]["bufferView"].as_u64().unwrap() as usize).unwrap();
        assert_eq!(idx, [3u32.to_le_bytes(), 70u32.to_le_bytes()].concat());
        let vals = view(&doc, &data, a["sparse"]["values"]["bufferView"].as_u64().unwrap() as usize).unwrap();
        assert_eq!(vals, le(&[0.1, -0.2, 0.3, 0.0, 0.5, 0.0]));
        assert_eq!(a["min"], json!([0.0, -0.20000000298023224, 0.0]));

        // Repacking drops the old dense view and keeps the sparse data reachable.
        let packed = repack(&mut doc, &data).unwrap();
        assert_eq!(doc["bufferViews"].as_array().unwrap().len(), 2);
        assert!(packed.len() < 1200);
        let vals = view(&doc, &packed, doc["accessors"][0]["sparse"]["values"]["bufferView"].as_u64().unwrap() as usize).unwrap();
        assert_eq!(vals, le(&[0.1, -0.2, 0.3, 0.0, 0.5, 0.0]));
    }

    #[test]
    fn a_morph_that_moves_most_vertices_stays_dense() {
        let dense: Vec<f32> = (0..300).map(|i| i as f32 * 0.001 + 0.01).collect();
        let mut data = le(&dense);
        let mut doc = json!({
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 1200}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 100}]
        });
        assert!(!sparsify(&mut doc, &mut data, 0).unwrap());
        assert_eq!(doc["accessors"][0]["bufferView"], 0);
    }

    #[test]
    fn identical_images_are_stored_once_and_oversized_ones_shrink() {
        let big = encode_png(&image::RgbaImage::from_fn(2048, 1024, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 7, 255]))).unwrap();
        let mut data = Vec::new();
        let mut doc = json!({"bufferViews": [], "images": [], "textures": [], "materials": []});
        for _ in 0..2 {
            let v = push_view(&mut doc, &mut data, &big);
            doc["images"].as_array_mut().unwrap().push(json!({"bufferView": v, "mimeType": "image/png"}));
        }
        doc["textures"] = json!([{"source": 0}, {"source": 1}]);
        doc["materials"] = json!([{"name": "Clothes", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
            {"name": "Clothes2", "pbrMetallicRoughness": {"baseColorTexture": {"index": 1}}}]);
        doc["buffers"] = json!([{"byteLength": data.len()}]);
        let (merged, resized) = slim_images(&mut doc, &mut data).unwrap();
        assert_eq!((merged, resized), (1, 1));
        assert_eq!(doc["textures"][1]["source"], 0);
        let packed = repack(&mut doc, &data).unwrap();
        let img = decode(view(&doc, &packed, doc["images"][0]["bufferView"].as_u64().unwrap() as usize).unwrap()).unwrap();
        assert_eq!((img.width(), img.height()), (1024, 512));
        assert!(packed.len() < big.len());
    }
}

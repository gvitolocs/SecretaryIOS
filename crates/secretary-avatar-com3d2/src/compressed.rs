//! EXT meshopt decoding with the safe scalar pure-Rust implementation. No C/C++
//! FFI, external process, browser decoder or Node runtime is used for imports.
use crate::{invalid, Result};
use meshoptimizer_rs::{
    codec::{BufferView, Filter, Mode},
    Limits, Workspace,
};
use serde_json::{json, Value};
pub fn unpack(doc: &mut Value, data: &mut Vec<u8>) -> Result<()> {
    let buffers = doc["buffers"]
        .as_array()
        .ok_or_else(|| invalid("meshopt buffers"))?;
    if buffers.is_empty()
        || !buffers[0]["uri"].is_null()
        || buffers.iter().skip(1).any(|b| {
            !b["uri"].is_null() || b["extensions"]["EXT_meshopt_compression"]["fallback"] != true
        })
    {
        return Err(invalid("unsupported external meshopt buffer"));
    }
    let original = data.len();
    let mut workspace = Workspace::new(Limits {
        max_bytes: 128 * 1024 * 1024,
        max_work: 1 << 30,
    });
    for view in doc["bufferViews"]
        .as_array_mut()
        .ok_or_else(|| invalid("meshopt views"))?
    {
        let ext = view["extensions"]["EXT_meshopt_compression"].clone();
        if ext.is_null() {
            let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let n = view["byteLength"]
                .as_u64()
                .ok_or_else(|| invalid("view length"))? as usize;
            if view["buffer"] != 0 || start.checked_add(n).is_none_or(|end| end > original) {
                return Err(invalid("uncompressed source view bounds"));
            }
            continue;
        }
        if ext["buffer"] != 0 {
            return Err(invalid("meshopt compressed source buffer"));
        }
        let integer = |key: &str| {
            ext[key]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| invalid("meshopt integer"))
        };
        let start = ext["byteOffset"].as_u64().unwrap_or(0) as usize;
        let length = integer("byteLength")?;
        let end = start
            .checked_add(length)
            .filter(|n| *n <= original)
            .ok_or_else(|| invalid("meshopt source bounds"))?;
        let mode = Mode::parse(ext["mode"].as_str().unwrap_or(""))
            .map_err(|e| invalid(format!("meshopt mode: {e:?}")))?;
        let filter = Filter::parse(ext["filter"].as_str().unwrap_or("NONE"))
            .map_err(|e| invalid(format!("meshopt filter: {e:?}")))?;
        let layout = BufferView::new(mode, filter, integer("count")?, integer("byteStride")?)
            .map_err(|e| invalid(format!("meshopt layout: {e:?}")))?;
        layout
            .validate_parent(
                view["byteLength"]
                    .as_u64()
                    .ok_or_else(|| invalid("meshopt parent length"))? as usize,
                view["byteStride"].as_u64().map(|i| i as usize),
            )
            .map_err(|e| invalid(format!("meshopt parent: {e:?}")))?;
        let decoded = layout
            .decode(&data[start..end], &mut workspace)
            .map_err(|e| invalid(format!("meshopt decode: {e:?}")))?;
        if data.len().saturating_add(decoded.len()) > 512 * 1024 * 1024 {
            return Err(invalid("decoded model exceeds 512 MiB"));
        }
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        let offset = data.len();
        data.extend(decoded);
        view["buffer"] = json!(0);
        view["byteOffset"] = json!(offset);
        if let Some(e) = view["extensions"].as_object_mut() {
            e.remove("EXT_meshopt_compression");
            if e.is_empty() {
                view.as_object_mut().unwrap().remove("extensions");
            }
        }
    }
    doc["buffers"] = json!([{"byteLength":data.len()}]);
    for key in ["extensionsUsed", "extensionsRequired"] {
        if let Some(a) = doc[key].as_array_mut() {
            a.retain(|v| v != "EXT_meshopt_compression");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessor;
    #[test]
    fn native_decoder_repacks_virtual_fallback_buffers_without_ffi() {
        let raw = [0., 1., 2., 3., 4., 5., 6., 7., 8.]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut workspace = Workspace::default();
        let bytes = meshoptimizer_rs::codec::encode_vertex_buffer(
            &raw,
            3,
            12,
            meshoptimizer_rs::codec::VertexEncoding::new(0, 2).unwrap(),
            &mut workspace,
        )
        .unwrap();
        let mut doc = json!({"buffers":[{"byteLength":bytes.len()},{"byteLength":raw.len(),"extensions":{"EXT_meshopt_compression":{"fallback":true}}}],"bufferViews":[{"buffer":1,"byteOffset":0,"byteLength":raw.len(),"byteStride":12,"extensions":{"EXT_meshopt_compression":{"buffer":0,"byteOffset":0,"byteLength":bytes.len(),"byteStride":12,"count":3,"mode":"ATTRIBUTES"}}}],"accessors":[{"bufferView":0,"componentType":5126,"type":"VEC3","count":3}],"extensionsUsed":["EXT_meshopt_compression"]});
        let mut data = bytes;
        unpack(&mut doc, &mut data).unwrap();
        assert_eq!(doc["buffers"].as_array().unwrap().len(), 1);
        assert!(doc["bufferViews"][0]["extensions"].is_null());
        assert_eq!(
            accessor::read(&doc, &data, 0).unwrap(),
            vec![vec![0., 1., 2.], vec![3., 4., 5.], vec![6., 7., 8.]]
        );
    }
}

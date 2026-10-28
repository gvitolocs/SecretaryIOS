//! Attach a native wearable while preserving its rest geometry and extending
//! the canonical skeleton with the item's own physics/attachment joints.
use crate::{animation::globals, glb_io, invalid, Result};
use glam::{Mat4, Quat, Vec3};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
fn matrix(n: &Value) -> Result<Mat4> {
    let floats = |key: &str, default: &[f32]| -> Result<Vec<f32>> {
        if n[key].is_null() {
            return Ok(default.to_vec());
        }
        let a = n[key]
            .as_array()
            .filter(|a| a.len() == default.len())
            .ok_or_else(|| invalid("node transform"))?;
        a.iter()
            .map(|v| {
                v.as_f64()
                    .filter(|v| v.is_finite())
                    .map(|v| v as f32)
                    .ok_or_else(|| invalid("transform value"))
            })
            .collect()
    };
    if n["matrix"].is_array() {
        return Ok(Mat4::from_cols_array(
            floats("matrix", &[0.; 16])?.as_slice().try_into().unwrap(),
        ));
    }
    Ok(Mat4::from_scale_rotation_translation(
        Vec3::from_slice(&floats("scale", &[1.; 3])?),
        Quat::from_slice(&floats("rotation", &[0., 0., 0., 1.])?).normalize(),
        Vec3::from_slice(&floats("translation", &[0.; 3])?),
    ))
}
pub fn scene_frames(doc: &Value) -> Result<(Vec<i32>, Vec<Mat4>, Vec<Mat4>)> {
    let nodes = doc["nodes"]
        .as_array()
        .ok_or_else(|| invalid("scene nodes"))?;
    let mut parents = vec![-1; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        if let Some(children) = n["children"].as_array() {
            for c in children {
                let c = c
                    .as_u64()
                    .filter(|c| *c < nodes.len() as u64)
                    .ok_or_else(|| invalid("scene child"))? as usize;
                if parents[c] >= 0 {
                    return Err(invalid("multiple node parents"));
                }
                parents[c] = i as i32;
            }
        }
    }
    let local = nodes.iter().map(matrix).collect::<Result<Vec<_>>>()?;
    let world = globals(&parents, &local)?;
    Ok((parents, local, world))
}
fn offset(v: &mut Value, n: usize) -> Result<()> {
    let i = v.as_u64().ok_or_else(|| invalid("resource index"))?;
    *v = json!(i + n as u64);
    Ok(())
}
fn material_textures(v: &mut Value, n: usize) -> Result<()> {
    if let Some(o) = v.as_object_mut() {
        for (k, v) in o {
            if k.ends_with("Texture") && v["index"].is_number() {
                offset(&mut v["index"], n)?;
            } else {
                material_textures(v, n)?;
            }
        }
    } else if let Some(a) = v.as_array_mut() {
        for v in a {
            material_textures(v, n)?;
        }
    }
    Ok(())
}
fn matrices(doc: &Value, data: &[u8], accessor: usize, count: usize) -> Result<Vec<Mat4>> {
    let a = &doc["accessors"][accessor];
    if a["componentType"] != 5126
        || a["type"] != "MAT4"
        || a["count"].as_u64() != Some(count as u64)
        || a["sparse"].is_object()
    {
        return Err(invalid("skin matrix accessor"));
    }
    let view = &doc["bufferViews"][a["bufferView"]
        .as_u64()
        .ok_or_else(|| invalid("matrix buffer view"))? as usize];
    let offset = view["byteOffset"]
        .as_u64()
        .unwrap_or(0)
        .checked_add(a["byteOffset"].as_u64().unwrap_or(0))
        .ok_or_else(|| invalid("matrix offset overflow"))? as usize;
    let stride = view["byteStride"].as_u64().unwrap_or(64) as usize;
    if stride < 64 {
        return Err(invalid("matrix stride too small"));
    }
    let mut out = Vec::new();
    for i in 0..count {
        let start = offset
            .checked_add(
                i.checked_mul(stride)
                    .ok_or_else(|| invalid("matrix stride"))?,
            )
            .ok_or_else(|| invalid("matrix offset"))?;
        let end = start
            .checked_add(64)
            .ok_or_else(|| invalid("matrix bounds overflow"))?;
        let raw = data
            .get(start..end)
            .ok_or_else(|| invalid("matrix data bounds"))?;
        let mut f = [0.; 16];
        for (j, b) in raw.as_chunks::<4>().0.iter().enumerate() {
            f[j] = f32::from_le_bytes(*b);
        }
        let m = Mat4::from_cols_array(&f);
        if !m.is_finite() {
            return Err(invalid("skin matrix numeric value"));
        }
        out.push(m);
    }
    Ok(out)
}
pub fn attach(base: &Path, part: &Path, out: &Path) -> Result<Value> {
    let (mut dst, mut data) = glb_io::read(base)?;
    let (src, source_data) = glb_io::read(part)?;
    let (_, _, base_world) = scene_frames(&dst)?;
    let (source_parents, _, source_world) = scene_frames(&src)?;
    let base_nodes = dst["nodes"]
        .as_array()
        .ok_or_else(|| invalid("base nodes"))?
        .len();
    let source_nodes = src["nodes"]
        .as_array()
        .ok_or_else(|| invalid("part nodes"))?;
    let joints = |doc: &Value| -> HashSet<usize> {
        doc["skins"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| s["joints"].as_array().into_iter().flatten())
            .filter_map(|j| j.as_u64().map(|j| j as usize))
            .collect()
    };
    let base_joints = joints(&dst);
    let source_joints = joints(&src);
    let names = dst["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(i, _)| base_joints.contains(i))
        .map(|(i, n)| (n["name"].as_str().unwrap_or("").to_owned(), i))
        .collect::<HashMap<_, _>>();
    let mut mapping = Vec::new();
    let mut fresh = Vec::new();
    let mut count = base_nodes;
    for (i, n) in source_nodes.iter().enumerate() {
        let same = if source_joints.contains(&i) {
            names.get(n["name"].as_str().unwrap_or("")).copied()
        } else {
            None
        };
        if let Some(j) = same {
            mapping.push(j);
        } else {
            mapping.push(count);
            fresh.push(i);
            count += 1;
        }
    }
    let mut target_world = base_world.clone();
    target_world.resize(count, Mat4::IDENTITY);
    for &i in &fresh {
        target_world[mapping[i]] = source_world[i];
    }
    let keys = [
        "bufferViews",
        "accessors",
        "images",
        "samplers",
        "textures",
        "materials",
        "meshes",
        "skins",
    ];
    let mut offsets = HashMap::new();
    for key in keys {
        offsets.insert(key, dst[key].as_array().map_or(0, Vec::len));
        if !dst[key].is_array() {
            dst[key] = json!([]);
        }
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let byte_offset = data.len();
    data.extend(&source_data);
    for key in keys {
        if let Some(items) = src[key].as_array() {
            for item in items {
                let mut item = item.clone();
                match key {
                    "bufferViews" => {
                        item["buffer"] = json!(0);
                        item["byteOffset"] =
                            json!(item["byteOffset"].as_u64().unwrap_or(0) + byte_offset as u64);
                    }
                    "accessors" => {
                        if item["bufferView"].is_number() {
                            offset(&mut item["bufferView"], offsets["bufferViews"])?;
                        }
                        if item["sparse"].is_object() {
                            offset(
                                &mut item["sparse"]["indices"]["bufferView"],
                                offsets["bufferViews"],
                            )?;
                            offset(
                                &mut item["sparse"]["values"]["bufferView"],
                                offsets["bufferViews"],
                            )?;
                        }
                    }
                    "images" => {
                        if item["bufferView"].is_number() {
                            offset(&mut item["bufferView"], offsets["bufferViews"])?;
                        } else {
                            return Err(invalid("wearable image must be embedded"));
                        }
                    }
                    "textures" => {
                        if item["source"].is_number() {
                            offset(&mut item["source"], offsets["images"])?;
                        }
                        for extension in ["EXT_texture_webp", "KHR_texture_basisu"] {
                            if item["extensions"][extension]["source"].is_number() {
                                offset(
                                    &mut item["extensions"][extension]["source"],
                                    offsets["images"],
                                )?;
                            }
                        }
                        if item["sampler"].is_number() {
                            offset(&mut item["sampler"], offsets["samplers"])?;
                        }
                    }
                    "materials" => material_textures(&mut item, offsets["textures"])?,
                    "meshes" => {
                        for p in item["primitives"]
                            .as_array_mut()
                            .ok_or_else(|| invalid("wearable primitives"))?
                        {
                            if p["extensions"].is_object() {
                                return Err(invalid("compressed wearable primitive unsupported"));
                            }
                            for a in p["attributes"]
                                .as_object_mut()
                                .ok_or_else(|| invalid("attributes"))?
                                .values_mut()
                            {
                                offset(a, offsets["accessors"])?;
                            }
                            if p["indices"].is_number() {
                                offset(&mut p["indices"], offsets["accessors"])?;
                            }
                            if p["material"].is_number() {
                                offset(&mut p["material"], offsets["materials"])?;
                            }
                            if let Some(targets) = p["targets"].as_array_mut() {
                                for target in targets {
                                    for a in target
                                        .as_object_mut()
                                        .ok_or_else(|| invalid("morph target"))?
                                        .values_mut()
                                    {
                                        offset(a, offsets["accessors"])?;
                                    }
                                }
                            }
                        }
                    }
                    "skins" => {
                        let old_joints = item["joints"]
                            .as_array()
                            .ok_or_else(|| invalid("wearable joints"))?
                            .iter()
                            .map(|j| j.as_u64().ok_or_else(|| invalid("joint index")))
                            .collect::<Result<Vec<_>>>()?;
                        let inverse = if let Some(a) = item["inverseBindMatrices"].as_u64() {
                            matrices(&src, &source_data, a as usize, old_joints.len())?
                        } else {
                            vec![Mat4::IDENTITY; old_joints.len()]
                        };
                        let mut rewritten = Vec::new();
                        let mut mapped = Vec::new();
                        for (k, &j) in old_joints.iter().enumerate() {
                            let j = j as usize;
                            if j >= mapping.len() {
                                return Err(invalid("wearable joint node"));
                            }
                            let target = mapping[j];
                            let matrix =
                                target_world[target].inverse() * source_world[j] * inverse[k];
                            rewritten.extend(
                                matrix
                                    .to_cols_array()
                                    .into_iter()
                                    .flat_map(|f| f.to_le_bytes()),
                            );
                            mapped.push(target);
                        }
                        while !data.len().is_multiple_of(4) {
                            data.push(0);
                        }
                        let start = data.len();
                        data.extend(&rewritten);
                        let view = dst["bufferViews"].as_array().unwrap().len();
                        dst["bufferViews"].as_array_mut().unwrap().push(
                            json!({"buffer":0,"byteOffset":start,"byteLength":rewritten.len()}),
                        );
                        let a = dst["accessors"].as_array().unwrap().len();
                        dst["accessors"].as_array_mut().unwrap().push(json!({"bufferView":view,"componentType":5126,"type":"MAT4","count":mapped.len()}));
                        item["joints"] = json!(mapped);
                        item["inverseBindMatrices"] = json!(a);
                        if let Some(skeleton) = item["skeleton"].as_u64() {
                            item["skeleton"] = json!(mapping[skeleton as usize]);
                        }
                    }
                    _ => (),
                }
                dst[key].as_array_mut().unwrap().push(item);
            }
        }
    }
    let mut roots = dst["scenes"][dst["scene"].as_u64().unwrap_or(0) as usize]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for &i in &fresh {
        let mut node = source_nodes[i].clone();
        node.as_object_mut().unwrap().remove("children");
        let parent = source_parents[i];
        let local = if parent >= 0 {
            target_world[mapping[parent as usize]].inverse() * source_world[i]
        } else {
            source_world[i]
        };
        node.as_object_mut().unwrap().remove("translation");
        node.as_object_mut().unwrap().remove("rotation");
        node.as_object_mut().unwrap().remove("scale");
        node["matrix"] = json!(local.to_cols_array());
        if node["mesh"].is_number() {
            offset(&mut node["mesh"], offsets["meshes"])?;
        }
        if node["skin"].is_number() {
            offset(&mut node["skin"], offsets["skins"])?;
        }
        dst["nodes"].as_array_mut().unwrap().push(node);
    }
    for &i in &fresh {
        let target = mapping[i];
        let parent = source_parents[i];
        if parent < 0 {
            roots.push(json!(target));
        } else {
            let p = &mut dst["nodes"][mapping[parent as usize]];
            if !p["children"].is_array() {
                p["children"] = json!([]);
            }
            p["children"].as_array_mut().unwrap().push(json!(target));
        }
    }
    let scene = dst["scene"].as_u64().unwrap_or(0) as usize;
    dst["scenes"][scene]["nodes"] = json!(roots);
    dst["buffers"][0]["byteLength"] = json!(data.len());
    for key in ["extensionsUsed", "extensionsRequired"] {
        let mut values = dst[key].as_array().cloned().unwrap_or_default();
        if let Some(other) = src[key].as_array() {
            for v in other {
                if !values.contains(v) {
                    values.push(v.clone());
                }
            }
        }
        if !values.is_empty() {
            dst[key] = json!(values);
        }
    }
    dst["extras"]["nativeWearableAttached"] = json!(true);
    glb_io::write(out, &dst, &data)?;
    Ok(
        json!({"output":out,"sharedJoints":source_nodes.len()-fresh.len(),"addedNodes":fresh.len(),"totalNodes":count}),
    )
}
